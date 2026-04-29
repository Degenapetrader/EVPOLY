use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::header::{HeaderName, AUTHORIZATION};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Datelike, Timelike, Utc};
use chrono_tz::America::New_York;
use futures_util::stream::{self, StreamExt};
use hmac::{Hmac, Mac};
use polymarket_arbitrage_bot::api::PolymarketApi;
use polymarket_arbitrage_bot::builder_attribution;
use polymarket_arbitrage_bot::evcurve;
use polymarket_arbitrage_bot::evsnipe;
use polymarket_arbitrage_bot::models::Market;
use polymarket_arbitrage_bot::plan3_tables::Plan3Tables;
use polymarket_arbitrage_bot::plan4b_tables::{Plan4bTables, SessionWatchKey, SessionWatchStart};
use polymarket_arbitrage_bot::plandaily_tables::PlanDailyTables;
use polymarket_arbitrage_bot::sessionband;
use polymarket_arbitrage_bot::strategy::Timeframe;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use tokio::sync::RwLock;
use tokio::time::sleep;

const DEFAULT_BIND: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8790;
const DEFAULT_MAX_BODY_BYTES: usize = 524_288;
const DEFAULT_PLAN3_PATH: &str = "/opt/evpoly-alpha-service/alpha/plan3.md";
const DEFAULT_PLAN4B_PATH: &str = "/opt/evpoly-alpha-service/alpha/plan4b.md";
const DEFAULT_PLANDAILY_PATH: &str = "/opt/evpoly-alpha-service/alpha/plandaily.md";
const DEFAULT_GAMMA_URL: &str = "https://gamma-api.polymarket.com";
const DEFAULT_CLOB_URL: &str = "https://clob.polymarket.com";
const DEFAULT_CLOB_V2_URL: &str = "https://clob.polymarket.com";
const DEFAULT_DISCOVERY_SYMBOLS: &[&str] = &["BTC", "ETH", "SOL", "XRP"];
const DEFAULT_DISCOVERY_REFRESH_SEC: u64 = 20;
const DEFAULT_EVSNIPE_REFRESH_SEC: u64 = 60;
const DEFAULT_DISCOVERY_BACK_PERIODS: u64 = 1;
const DEFAULT_DISCOVERY_HORIZON_5M: u64 = 2;
const DEFAULT_DISCOVERY_HORIZON_15M: u64 = 2;
const DEFAULT_DISCOVERY_HORIZON_1H: u64 = 1;
const DEFAULT_DISCOVERY_HORIZON_4H: u64 = 1;
const DEFAULT_DISCOVERY_HORIZON_1D: u64 = 1;
const DEFAULT_PREMARKET_YES_MIN: f64 = 0.70;
const DEFAULT_PREMARKET_YES_MAX: f64 = 0.90;
const DEFAULT_ENDGAME_BASE_OFFSETS_MS: &[u64] = &[2000, 1000, 100];
const DEFAULT_ENDGAME_NEAR_T_RANDOM_MAX_BPS: u32 = 2500;
const DEFAULT_ENDGAME_SUBMIT_PROXY_MAX_AGE_BASE_MS: i64 = 400;
const DEFAULT_ENDGAME_SUBMIT_PROXY_MAX_AGE_JITTER_MS: i64 = 100;
const DEFAULT_ENDGAME_LEGACY_SDK1_COMPAT: bool = true;
const DEFAULT_ENDGAME_LEGACY_SDK1_BASE_OFFSETS_MS: &[u64] = &[3000, 1000, 100];
const DEFAULT_ENDGAME_LEGACY_SDK1_OFFSET_JITTER_MS: i64 = 50;
const MM_SPORT_DEPTH_SKIP_MAX_MARKETS: usize = 200;
const MM_SPORT_DEPTH_SKIP_CONCURRENCY: usize = 8;
const AUTO_ALPHA_KEY_PREFIX: &str = "evp_auto";

#[derive(Debug, Clone)]
struct Settings {
    bind: String,
    port: u16,
    token: Option<String>,
    auto_onboard_enabled: bool,
    auto_key_secret: Option<String>,
    require_builder_code: bool,
    require_wallet_header: bool,
    max_body_bytes: usize,
    rate_limit_per_ip_rps: u32,
    rate_limit_per_ip_burst: u32,
    rate_limit_global_rps: u32,
    rate_limit_global_burst: u32,
    plan3_path: String,
    plan4b_path: String,
    plandaily_path: String,
    gamma_url: String,
    clob_url: String,
    clob_v2_url: String,
    h1_strict_match: bool,
    h1_allow_next_hour_fallback: bool,
    discovery_symbols: Vec<String>,
    discovery_refresh_sec: u64,
    evsnipe_refresh_sec: u64,
    discovery_back_periods: u64,
    discovery_horizon_5m: u64,
    discovery_horizon_15m: u64,
    discovery_horizon_1h: u64,
    discovery_horizon_4h: u64,
    discovery_horizon_1d: u64,
    premarket_yes_min: f64,
    premarket_yes_max: f64,
    endgame_base_offsets_ms: Vec<u64>,
    endgame_near_t_random_max_bps: u32,
    endgame_submit_proxy_max_age_base_ms: i64,
    endgame_submit_proxy_max_age_jitter_ms: i64,
    endgame_legacy_sdk1_compat: bool,
    endgame_legacy_sdk1_base_offsets_ms: Vec<u64>,
    endgame_legacy_sdk1_offset_jitter_ms: i64,
    allowed_proxy_wallets: HashSet<String>,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "ok": false,
                "error": self.message,
            })),
        )
            .into_response()
    }
}

#[derive(Debug)]
struct TokenBucket {
    rate_per_sec: f64,
    capacity: f64,
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    fn new(rate_per_sec: u32, capacity: u32, now: Instant) -> Self {
        Self {
            rate_per_sec: rate_per_sec as f64,
            capacity: capacity as f64,
            tokens: capacity as f64,
            last_refill: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        if elapsed <= 0.0 {
            return;
        }
        self.tokens = (self.tokens + elapsed * self.rate_per_sec).min(self.capacity);
        self.last_refill = now;
    }

    fn consume(&mut self, amount: f64, now: Instant) -> bool {
        self.refill(now);
        if self.tokens >= amount {
            self.tokens -= amount;
            true
        } else {
            false
        }
    }

    fn refund(&mut self, amount: f64) {
        self.tokens = (self.tokens + amount).min(self.capacity);
    }
}

#[derive(Debug)]
struct RateLimiterInner {
    per_ip: HashMap<IpAddr, (TokenBucket, Instant)>,
    global: TokenBucket,
    last_cleanup: Instant,
}

#[derive(Debug)]
struct RateLimiter {
    per_ip_rps: u32,
    per_ip_burst: u32,
    inner: Mutex<RateLimiterInner>,
}

impl RateLimiter {
    fn new(per_ip_rps: u32, per_ip_burst: u32, global_rps: u32, global_burst: u32) -> Self {
        let now = Instant::now();
        Self {
            per_ip_rps,
            per_ip_burst,
            inner: Mutex::new(RateLimiterInner {
                per_ip: HashMap::new(),
                global: TokenBucket::new(global_rps, global_burst, now),
                last_cleanup: now,
            }),
        }
    }

    fn allow(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let Ok(mut guard) = self.inner.lock() else {
            return false;
        };
        let ip_ok = {
            let ip_bucket = guard.per_ip.entry(ip).or_insert_with(|| {
                (
                    TokenBucket::new(self.per_ip_rps, self.per_ip_burst, now),
                    now,
                )
            });
            let allowed = ip_bucket.0.consume(1.0, now);
            ip_bucket.1 = now;
            allowed
        };

        if !ip_ok {
            self.cleanup_locked(&mut guard, now);
            return false;
        }

        if !guard.global.consume(1.0, now) {
            if let Some(ip_bucket) = guard.per_ip.get_mut(&ip) {
                ip_bucket.0.refund(1.0);
                ip_bucket.1 = now;
            }
            self.cleanup_locked(&mut guard, now);
            return false;
        }

        if let Some(ip_bucket) = guard.per_ip.get_mut(&ip) {
            ip_bucket.1 = now;
        }
        self.cleanup_locked(&mut guard, now);
        true
    }

    fn cleanup_locked(&self, inner: &mut RateLimiterInner, now: Instant) {
        if now.saturating_duration_since(inner.last_cleanup).as_secs() < 60 {
            return;
        }
        let stale_after = 180_u64;
        inner.per_ip.retain(|_, (_, last_seen)| {
            now.saturating_duration_since(*last_seen).as_secs() <= stale_after
        });
        inner.last_cleanup = now;
    }
}

#[derive(Clone)]
struct AppState {
    settings: Settings,
    rate_limiter: Arc<RateLimiter>,
    api: Arc<PolymarketApi>,
    api_v2: Arc<PolymarketApi>,
    evcurve_cfg: evcurve::EvcurveExecutionConfig,
    sessionband_cfg: sessionband::SessionBandExecutionConfig,
    evsnipe_cfg: evsnipe::EvsnipeConfig,
    evsnipe_spot_anchors: Arc<RwLock<HashMap<String, evsnipe::EvsnipeSpotAnchor>>>,
    plan3_tables: Arc<Plan3Tables>,
    plandaily_tables: Option<Arc<PlanDailyTables>>,
    watch_starts: Arc<HashMap<SessionWatchKey, SessionWatchStart>>,
    discovery_cache: Arc<DiscoveryCache>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TimeframeCacheKey {
    symbol: String,
    timeframe: String,
    target_open_ts: u64,
}

#[derive(Debug, Clone)]
struct TimeframeCacheState {
    generation: u64,
    updated_at_ms: i64,
    entries: HashMap<TimeframeCacheKey, TimeframeDiscoveryResponse>,
}

#[derive(Debug, Clone)]
struct EvsnipeCacheState {
    generation: u64,
    updated_at_ms: i64,
    specs: Vec<evsnipe::EvsnipeMarketSpec>,
}

#[derive(Debug)]
struct DiscoveryCache {
    timeframe: RwLock<TimeframeCacheState>,
    evsnipe: RwLock<EvsnipeCacheState>,
}

impl DiscoveryCache {
    fn new() -> Self {
        Self {
            timeframe: RwLock::new(TimeframeCacheState {
                generation: 0,
                updated_at_ms: 0,
                entries: HashMap::new(),
            }),
            evsnipe: RwLock::new(EvsnipeCacheState {
                generation: 0,
                updated_at_ms: 0,
                specs: Vec::new(),
            }),
        }
    }

    async fn replace_timeframe(
        &self,
        updated_at_ms: i64,
        next_entries: HashMap<TimeframeCacheKey, TimeframeDiscoveryResponse>,
    ) {
        let mut guard = self.timeframe.write().await;
        guard.generation = guard.generation.saturating_add(1);
        guard.updated_at_ms = updated_at_ms;
        guard.entries = next_entries;
    }

    async fn replace_evsnipe(&self, updated_at_ms: i64, specs: Vec<evsnipe::EvsnipeMarketSpec>) {
        let mut guard = self.evsnipe.write().await;
        guard.generation = guard.generation.saturating_add(1);
        guard.updated_at_ms = updated_at_ms;
        guard.specs = specs;
    }

    async fn get_timeframe(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        target_open_ts: u64,
    ) -> Option<TimeframeDiscoveryResponse> {
        let guard = self.timeframe.read().await;
        let key = TimeframeCacheKey {
            symbol: normalize_symbol(symbol),
            timeframe: timeframe.as_str().to_string(),
            target_open_ts,
        };
        guard.entries.get(&key).cloned()
    }

    async fn get_evsnipe_specs(&self) -> (i64, Vec<evsnipe::EvsnipeMarketSpec>) {
        let guard = self.evsnipe.read().await;
        (guard.updated_at_ms, guard.specs.clone())
    }
}

#[derive(Debug, Deserialize)]
struct TimeframeDiscoveryRequest {
    symbol: String,
    timeframe: String,
    target_open_ts: u64,
    #[serde(default)]
    builder_code: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct TimeframeDiscoveryResponse {
    market: Market,
    matched_open_ts: u64,
    matched_slug: String,
    source: String,
}

#[derive(Debug, Clone)]
struct DiscoveredMarket {
    market: Market,
    matched_open_ts: u64,
    matched_slug: String,
    source: &'static str,
}

#[derive(Debug, Clone)]
struct SlugCandidate {
    slug: String,
    open_ts: u64,
    source: &'static str,
}

#[derive(Debug, Deserialize)]
struct EvsnipeDiscoveryRequest {
    #[serde(default)]
    symbols: Vec<String>,
    #[serde(default)]
    discovery_limit: Option<u32>,
    #[serde(default)]
    max_days_to_expiry: Option<u64>,
    #[serde(default)]
    builder_code: Option<String>,
}

#[derive(Debug, Serialize)]
struct EvsnipeDiscoveryResponse {
    specs: Vec<evsnipe::EvsnipeMarketSpec>,
}

#[derive(Debug, Deserialize)]
struct EvcurveRequest {
    symbol: String,
    timeframe: String,
    period_open_ts: i64,
    tau_sec: i64,
    base_mid: f64,
    current_mid: f64,
    ask_up: Option<f64>,
    ask_down: Option<f64>,
    #[serde(default)]
    d1_zero_rule_already_fired: bool,
    #[serde(default)]
    d1_ev_rule_already_fired: bool,
    #[serde(default)]
    builder_code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionbandRequest {
    symbol: String,
    timeframe: String,
    period_open_ts: i64,
    tau_sec: i64,
    base_mid: f64,
    current_mid: f64,
    ask_up: Option<f64>,
    ask_down: Option<f64>,
    #[serde(default)]
    builder_code: Option<String>,
}

#[derive(Debug, Serialize)]
struct SessionbandResponse {
    symbol: String,
    timeframe: String,
    period_open_ts: i64,
    tau_sec: i64,
    lead_pct: f64,
    direction: String,
    session_index: Option<u8>,
    watch_start_sec: Option<i64>,
    tau_trigger_sec: Option<i64>,
    trigger_rate_pct: Option<f64>,
    should_buy: bool,
    skip_reason: Option<String>,
    chosen_ask: Option<f64>,
    band_price_min: Option<f64>,
    band_price_max: Option<f64>,
    score_bps: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct PremarketAlphaLadderRequest {
    strategy_id: String,
    decision_id: String,
    symbol: String,
    timeframe: String,
    market_open_ts: i64,
    proxy_wallet: String,
    ts_ms: i64,
    nonce: String,
    #[serde(default)]
    builder_code: Option<String>,
    base_prices: Vec<f64>,
}

#[derive(Debug, Deserialize)]
struct PremarketAlphaShouldTradeRequest {
    strategy_id: String,
    decision_id: String,
    symbol: String,
    timeframe: String,
    market_open_ts: i64,
    proxy_wallet: String,
    ts_ms: i64,
    nonce: String,
    #[serde(default)]
    builder_code: Option<String>,
}

#[derive(Debug, Serialize)]
struct PremarketAlphaLadderResponse {
    ok: bool,
    source: String,
    reason: String,
    shift_pct: f64,
    prices: Vec<f64>,
}

#[derive(Debug, Serialize)]
struct PremarketAlphaShouldTradeResponse {
    ok: bool,
    should_trade: bool,
    source: String,
    reason: String,
    yes_prob: f64,
}

#[derive(Debug, Deserialize)]
struct EndgameAlphaPolicyRequest {
    strategy_id: String,
    symbol: String,
    timeframe: String,
    market_open_ts: i64,
    market_close_ts: i64,
    request_ts_ms: i64,
    proxy_wallet: String,
    nonce: String,
    #[serde(default)]
    builder_code: Option<String>,
}

#[derive(Debug, Serialize)]
struct EndgameAlphaPolicyResponse {
    ok: bool,
    tick_offsets_ms: Vec<u64>,
    submit_proxy_max_age_ms: i64,
    source: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct MmSportDepthSkipRequest {
    strategy_id: String,
    request_ts_ms: i64,
    depth_floor_usd: f64,
    #[serde(default)]
    builder_code: Option<String>,
    #[serde(default)]
    clob_version: Option<String>,
    markets: Vec<MmSportDepthSkipMarketInput>,
}

#[derive(Debug, Clone, Deserialize)]
struct MmSportDepthSkipMarketInput {
    condition_id: String,
    market_slug: String,
    up_token_id: String,
    down_token_id: String,
    minimum_tick_size: f64,
}

#[derive(Debug, Serialize)]
struct MmSportDepthSkipMarketResponse {
    condition_id: String,
    market_slug: String,
    skipped: bool,
    reason: String,
    up_submit_bid_price: Option<f64>,
    down_submit_bid_price: Option<f64>,
    up_ext_top_bid_usd: Option<f64>,
    down_ext_top_bid_usd: Option<f64>,
    pair_min_top_depth_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
struct MmSportDepthSkipResponse {
    ok: bool,
    kind: String,
    request_ts_ms: i64,
    depth_floor_usd: f64,
    checked_count: usize,
    skipped_condition_ids: Vec<String>,
    markets: Vec<MmSportDepthSkipMarketResponse>,
}

#[derive(Debug, Deserialize)]
struct AlphaOnboardRequest {
    wallet: String,
    builder_code: String,
    #[serde(default)]
    client_version: Option<String>,
    #[serde(default)]
    install_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct AlphaOnboardResponse {
    ok: bool,
    alpha_key: String,
    required_builder_code: String,
    alpha_base_url: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let settings = load_settings()?;

    let api = Arc::new(PolymarketApi::new(
        settings.gamma_url.clone(),
        settings.clob_url.clone(),
        None,
        None,
        None,
    ));
    let api_v2 = Arc::new(PolymarketApi::new(
        settings.gamma_url.clone(),
        settings.clob_v2_url.clone(),
        None,
        None,
        None,
    ));

    let plan3_tables = Arc::new(
        Plan3Tables::load_from_path(Path::new(settings.plan3_path.as_str())).with_context(
            || {
                format!(
                    "failed to load plan3 tables from {}",
                    settings.plan3_path.as_str()
                )
            },
        )?,
    );

    let plandaily_tables = match PlanDailyTables::load_from_path(Path::new(
        settings.plandaily_path.as_str(),
    )) {
        Ok(tables) => Some(Arc::new(tables)),
        Err(err) => {
            eprintln!(
                "warning: failed to load plandaily tables from {}: {} (D1 endpoint will return 503)",
                settings.plandaily_path.as_str(),
                err
            );
            None
        }
    };

    let plan4b_tables =
        Plan4bTables::load_from_path(settings.plan4b_path.as_str()).with_context(|| {
            format!(
                "failed to load plan4b tables from {}",
                settings.plan4b_path.as_str()
            )
        })?;

    let evcurve_cfg = evcurve::EvcurveExecutionConfig::from_env();
    let sessionband_cfg = sessionband::SessionBandExecutionConfig::from_env();
    let evsnipe_cfg = evsnipe::EvsnipeConfig::from_env();

    let watch_starts = Arc::new(plan4b_tables.derive_watch_starts(
        sessionband_cfg.enabled_symbols().as_slice(),
        sessionband_cfg.enabled_timeframes().as_slice(),
        sessionband_cfg.flip_threshold_pct,
        sessionband_cfg.prewatch_sec,
    ));

    let rate_limiter = Arc::new(RateLimiter::new(
        settings.rate_limit_per_ip_rps,
        settings.rate_limit_per_ip_burst,
        settings.rate_limit_global_rps,
        settings.rate_limit_global_burst,
    ));
    let discovery_cache = Arc::new(DiscoveryCache::new());
    let evsnipe_spot_anchors = Arc::new(RwLock::new(HashMap::new()));

    let state = AppState {
        settings: settings.clone(),
        rate_limiter,
        api,
        api_v2,
        evcurve_cfg,
        sessionband_cfg,
        evsnipe_cfg,
        evsnipe_spot_anchors,
        plan3_tables,
        plandaily_tables,
        watch_starts,
        discovery_cache,
    };

    if let Err(err) = refresh_timeframe_discovery_cache(&state).await {
        eprintln!(
            "warning: initial timeframe discovery refresh failed: {}",
            err
        );
    }
    if let Err(err) = refresh_evsnipe_discovery_cache(&state).await {
        eprintln!("warning: initial evsnipe discovery refresh failed: {}", err);
    }
    spawn_discovery_refresh_workers(state.clone());

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/v1/onboard", post(alpha_onboard_handler))
        .route("/v1/discovery/timeframe", post(discovery_timeframe_handler))
        .route("/v1/discovery/evsnipe", post(discovery_evsnipe_handler))
        .route(
            "/v1/alpha/mm-sport/depth-skip",
            post(alpha_mm_sport_depth_skip_handler),
        )
        .route(
            "/v1/alpha/mm-rewards/selection",
            post(alpha_mm_rewards_selection_compat_handler),
        )
        .route(
            "/v1/alpha/mm-rewards/preflight",
            post(alpha_mm_rewards_preflight_compat_handler),
        )
        .route("/v1/alpha/evcurve", post(alpha_evcurve_handler))
        .route("/v1/alpha/sessionband", post(alpha_sessionband_handler))
        .route(
            "/v1/alpha/premarket/should-trade",
            post(alpha_premarket_should_trade_handler),
        )
        .route(
            "/v1/alpha/premarket/ladder",
            post(alpha_premarket_ladder_handler),
        )
        .route(
            "/v1/alpha/endgame/policy",
            post(alpha_endgame_policy_handler),
        )
        .with_state(state);

    let addr = format!("{}:{}", settings.bind, settings.port);
    let listener = tokio::net::TcpListener::bind(addr.as_str())
        .await
        .with_context(|| format!("failed to bind {}", addr.as_str()))?;

    eprintln!(
        "evpoly alpha service listening on {} with routes: /health, /v1/onboard, /v1/discovery/timeframe, /v1/discovery/evsnipe, /v1/alpha/mm-sport/depth-skip, /v1/alpha/mm-rewards/selection, /v1/alpha/mm-rewards/preflight, /v1/alpha/evcurve, /v1/alpha/sessionband, /v1/alpha/premarket/should-trade, /v1/alpha/premarket/ladder, /v1/alpha/endgame/policy",
        addr.as_str()
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("axum server error")?;

    Ok(())
}

async fn health_handler() -> Json<Value> {
    Json(json!({ "ok": true }))
}

async fn alpha_onboard_handler(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    guard_onboard_request(&state, remote.ip(), body.len())?;
    if !state.settings.auto_onboard_enabled {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "alpha auto-onboard disabled",
        ));
    }
    let payload: AlphaOnboardRequest = serde_json::from_slice(&body)
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid JSON request body"))?;
    let wallet = normalize_wallet(payload.wallet.as_str());
    if !is_valid_wallet(wallet.as_str()) {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "wallet is invalid"));
    }
    if !builder_code_matches_official(Some(payload.builder_code.as_str())) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "official builder code required",
        ));
    }
    eprintln!(
        "alpha_auto_onboard wallet={} client_version={} install_id={}",
        wallet,
        payload.client_version.as_deref().unwrap_or(""),
        payload.install_id.as_deref().unwrap_or("")
    );
    let alpha_key = auto_alpha_key_for_wallet(&state.settings, wallet.as_str())?;
    Ok(Json(AlphaOnboardResponse {
        ok: true,
        alpha_key,
        required_builder_code: builder_attribution::official_builder_code().to_string(),
        alpha_base_url: "https://alpha.evplus.ai".to_string(),
    }))
}

fn timeframe_horizon(settings: &Settings, timeframe: Timeframe) -> u64 {
    match timeframe {
        Timeframe::M5 => settings.discovery_horizon_5m,
        Timeframe::M15 => settings.discovery_horizon_15m,
        Timeframe::H1 => settings.discovery_horizon_1h,
        Timeframe::H4 => settings.discovery_horizon_4h,
        Timeframe::D1 => settings.discovery_horizon_1d,
    }
}

fn normalize_target_open_ts(timeframe: Timeframe, target_open_ts: u64) -> u64 {
    if timeframe == Timeframe::D1 {
        d1_period_bounds_for_timestamp(target_open_ts as i64)
            .map(|(open, _)| open.max(0) as u64)
            .unwrap_or(target_open_ts)
    } else {
        let period_secs = timeframe.duration_seconds().max(1) as u64;
        (target_open_ts / period_secs) * period_secs
    }
}

fn timeframe_open_ts_from_now(timeframe: Timeframe, now_ts: u64) -> u64 {
    normalize_target_open_ts(timeframe, now_ts)
}

fn build_timeframe_prewarm_requests(
    settings: &Settings,
    now_ts: u64,
) -> Vec<(String, Timeframe, u64)> {
    let timeframes = [
        Timeframe::M5,
        Timeframe::M15,
        Timeframe::H1,
        Timeframe::H4,
        Timeframe::D1,
    ];
    let mut out = Vec::new();
    let mut seen: HashSet<(String, String, u64)> = HashSet::new();

    for symbol in settings.discovery_symbols.iter() {
        let symbol_norm = normalize_symbol(symbol.as_str());
        if symbol_norm.is_empty() {
            continue;
        }
        for timeframe in timeframes {
            let base_open = timeframe_open_ts_from_now(timeframe, now_ts);
            let period_secs = if timeframe == Timeframe::D1 {
                86_400_u64
            } else {
                timeframe.duration_seconds().max(1) as u64
            };
            let back = settings.discovery_back_periods;
            let ahead = timeframe_horizon(settings, timeframe);
            for delta in -(back as i64)..=(ahead as i64) {
                let candidate_open_ts = if delta < 0 {
                    let shift = period_secs.saturating_mul(delta.unsigned_abs());
                    base_open.saturating_sub(shift)
                } else {
                    base_open.saturating_add(period_secs.saturating_mul(delta as u64))
                };
                let normalized_open_ts = normalize_target_open_ts(timeframe, candidate_open_ts);
                let seen_key = (
                    symbol_norm.clone(),
                    timeframe.as_str().to_string(),
                    normalized_open_ts,
                );
                if seen.insert(seen_key) {
                    out.push((symbol_norm.clone(), timeframe, normalized_open_ts));
                }
            }
        }
    }

    out
}

async fn refresh_timeframe_discovery_cache(state: &AppState) -> Result<usize> {
    let now_ts = Utc::now().timestamp().max(0) as u64;
    let now_ms = Utc::now().timestamp_millis();
    let requests = build_timeframe_prewarm_requests(&state.settings, now_ts);
    let mut next_entries = HashMap::new();

    for (symbol, timeframe, target_open_ts) in requests {
        let discovered = discover_market_for_timeframe_once(
            state.api.as_ref(),
            timeframe,
            target_open_ts,
            symbol.as_str(),
            &state.settings,
        )
        .await?;
        let Some(found) = discovered else {
            continue;
        };
        let key = TimeframeCacheKey {
            symbol: symbol.clone(),
            timeframe: timeframe.as_str().to_string(),
            target_open_ts,
        };
        next_entries.insert(
            key,
            TimeframeDiscoveryResponse {
                market: found.market,
                matched_open_ts: found.matched_open_ts,
                matched_slug: found.matched_slug,
                source: found.source.to_string(),
            },
        );
    }

    let entry_count = next_entries.len();
    state
        .discovery_cache
        .replace_timeframe(now_ms, next_entries)
        .await;
    Ok(entry_count)
}

async fn refresh_evsnipe_discovery_cache(state: &AppState) -> Result<usize> {
    let now_ms = Utc::now().timestamp_millis();
    let raw_specs =
        evsnipe::refresh_hit_market_specs_local_only(state.api.as_ref(), &state.evsnipe_cfg)
            .await?;
    match evsnipe::fetch_binance_spot_prices(state.evsnipe_cfg.symbols.as_slice()).await {
        Ok(spot_prices) => {
            let mut anchors = state.evsnipe_spot_anchors.write().await;
            for symbol_raw in state.evsnipe_cfg.symbols.iter() {
                let symbol = normalize_symbol(symbol_raw.as_str());
                let Some(current_spot) = spot_prices.get(symbol.as_str()).copied() else {
                    continue;
                };
                let existing = anchors.get(symbol.as_str()).copied();
                let Some((next_anchor, _reason)) = evsnipe::next_spot_anchor(
                    existing,
                    current_spot,
                    now_ms,
                    state.evsnipe_cfg.anchor_refresh_sec,
                    state.evsnipe_cfg.anchor_drift_refresh_pct,
                ) else {
                    continue;
                };
                anchors.insert(symbol, next_anchor);
            }
        }
        Err(err) => {
            eprintln!("warning: evsnipe spot anchor refresh failed: {}", err);
        }
    }

    let anchors_snapshot = state.evsnipe_spot_anchors.read().await.clone();
    let specs = if anchors_snapshot.is_empty() {
        raw_specs
    } else {
        evsnipe::filter_specs_by_spot_anchor(
            raw_specs.as_slice(),
            &anchors_snapshot,
            state.evsnipe_cfg.strike_window_pct,
        )
    };

    let count = specs.len();
    state.discovery_cache.replace_evsnipe(now_ms, specs).await;
    Ok(count)
}

fn spawn_discovery_refresh_workers(state: AppState) {
    let timeframe_state = state.clone();
    tokio::spawn(async move {
        let interval = Duration::from_secs(timeframe_state.settings.discovery_refresh_sec.max(5));
        loop {
            if let Err(err) = refresh_timeframe_discovery_cache(&timeframe_state).await {
                eprintln!("warning: timeframe discovery refresh failed: {}", err);
            }
            sleep(interval).await;
        }
    });

    tokio::spawn(async move {
        let interval = Duration::from_secs(state.settings.evsnipe_refresh_sec.max(5));
        loop {
            if let Err(err) = refresh_evsnipe_discovery_cache(&state).await {
                eprintln!("warning: evsnipe discovery refresh failed: {}", err);
            }
            sleep(interval).await;
        }
    });
}

async fn discovery_timeframe_handler(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let payload: TimeframeDiscoveryRequest =
        parse_json_request(&state, remote.ip(), &headers, &body)?;
    ensure_builder_code_authorized(&state.settings, &headers, payload.builder_code.as_deref())?;

    let timeframe = parse_timeframe(payload.timeframe.as_str()).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "timeframe must be one of 5m,15m,1h,4h,1d",
        )
    })?;
    let symbol = normalize_symbol(payload.symbol.as_str());

    let target_open_ts = normalize_target_open_ts(timeframe, payload.target_open_ts);
    let cached = state
        .discovery_cache
        .get_timeframe(symbol.as_str(), timeframe, target_open_ts)
        .await;

    cached
        .map(Json)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "market not found in cache"))
}

async fn discovery_evsnipe_handler(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let payload: EvsnipeDiscoveryRequest =
        parse_json_request(&state, remote.ip(), &headers, &body)?;
    ensure_builder_code_authorized(&state.settings, &headers, payload.builder_code.as_deref())?;

    let (updated_at_ms, mut specs) = state.discovery_cache.get_evsnipe_specs().await;
    if specs.is_empty() && updated_at_ms <= 0 {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "evsnipe discovery cache unavailable",
        ));
    }

    if !payload.symbols.is_empty() {
        let allow = payload
            .symbols
            .into_iter()
            .map(|symbol| normalize_symbol(symbol.as_str()))
            .filter(|symbol| !symbol.is_empty())
            .collect::<HashSet<_>>();
        if !allow.is_empty() {
            specs.retain(|spec| allow.contains(spec.symbol.as_str()));
        }
    }

    let max_days = payload
        .max_days_to_expiry
        .unwrap_or(state.evsnipe_cfg.max_days_to_expiry)
        .clamp(1, 365);
    let now_sec = Utc::now().timestamp();
    let max_end_ts = now_sec.saturating_add(i64::try_from(max_days).ok().unwrap_or(30) * 86_400);
    specs.retain(|spec| {
        spec.end_ts
            .map(|end_ts| end_ts > now_sec && end_ts <= max_end_ts)
            .unwrap_or(false)
    });

    let limit = payload
        .discovery_limit
        .unwrap_or(specs.len() as u32)
        .clamp(1, 20_000) as usize;
    if specs.len() > limit {
        specs.truncate(limit);
    }

    Ok(Json(EvsnipeDiscoveryResponse { specs }))
}

fn mm_sport_alpha_one_tick(tick_size: f64) -> f64 {
    tick_size.max(0.000_001)
}

fn mm_sport_alpha_passive_entry_price(best_bid: f64, tick_size: f64) -> f64 {
    let tick = mm_sport_alpha_one_tick(tick_size);
    (best_bid - tick).clamp(tick, 1.0 - tick)
}

fn mm_sport_alpha_best_bid(orderbook: &polymarket_arbitrage_bot::models::OrderBook) -> Option<f64> {
    orderbook
        .bids
        .iter()
        .filter_map(|entry| f64::try_from(entry.price).ok())
        .filter(|price| price.is_finite() && *price > 0.0)
        .max_by(|a, b| a.total_cmp(b))
}

fn mm_sport_alpha_bid_depth_at_or_above(
    orderbook: &polymarket_arbitrage_bot::models::OrderBook,
    target_price: f64,
    tick_size: f64,
) -> (f64, f64) {
    let tolerance = if tick_size.is_finite() && tick_size > 0.0 {
        tick_size * 0.5
    } else {
        1e-9
    };
    orderbook
        .bids
        .iter()
        .filter_map(|entry| {
            let price = f64::try_from(entry.price).ok()?;
            let size = f64::try_from(entry.size).ok()?;
            if !price.is_finite()
                || !size.is_finite()
                || price <= 0.0
                || size <= 0.0
                || price + tolerance < target_price
            {
                return None;
            }
            Some((size, price * size))
        })
        .fold((0.0, 0.0), |(shares_acc, usd_acc), (shares, usd)| {
            (shares_acc + shares, usd_acc + usd)
        })
}

fn mm_sport_depth_skip_wants_v2(clob_version: Option<&str>) -> bool {
    match clob_version
        .map(str::trim)
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "v2" | "clob-v2" | "clob_v2" => true,
        _ => false,
    }
}

fn mm_sport_depth_skip_api_for_request(
    state: &AppState,
    clob_version: Option<&str>,
) -> Arc<PolymarketApi> {
    if mm_sport_depth_skip_wants_v2(clob_version) {
        state.api_v2.clone()
    } else {
        state.api.clone()
    }
}

fn finite_json_number(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

async fn evaluate_mm_sport_depth_skip_market(
    api: Arc<PolymarketApi>,
    market: MmSportDepthSkipMarketInput,
    depth_floor_usd: f64,
) -> MmSportDepthSkipMarketResponse {
    let condition_id = market.condition_id.trim().to_string();
    let market_slug = market.market_slug.trim().to_string();
    let up_token_id = market.up_token_id.trim().to_string();
    let down_token_id = market.down_token_id.trim().to_string();
    let tick_size = market.minimum_tick_size.max(0.000_001);
    if condition_id.is_empty()
        || up_token_id.is_empty()
        || down_token_id.is_empty()
        || !tick_size.is_finite()
        || tick_size <= 0.0
    {
        return MmSportDepthSkipMarketResponse {
            condition_id,
            market_slug,
            skipped: true,
            reason: "invalid_market_payload".to_string(),
            up_submit_bid_price: None,
            down_submit_bid_price: None,
            up_ext_top_bid_usd: None,
            down_ext_top_bid_usd: None,
            pair_min_top_depth_usd: None,
        };
    }

    let (up_book_res, down_book_res) = tokio::join!(
        api.get_orderbook(up_token_id.as_str()),
        api.get_orderbook(down_token_id.as_str()),
    );
    let (up_book, down_book) = match (up_book_res, down_book_res) {
        (Ok(up_book), Ok(down_book)) => (up_book, down_book),
        _ => {
            return MmSportDepthSkipMarketResponse {
                condition_id,
                market_slug,
                skipped: true,
                reason: "orderbook_unavailable".to_string(),
                up_submit_bid_price: None,
                down_submit_bid_price: None,
                up_ext_top_bid_usd: None,
                down_ext_top_bid_usd: None,
                pair_min_top_depth_usd: None,
            };
        }
    };
    let Some(up_best_bid) = mm_sport_alpha_best_bid(&up_book) else {
        return MmSportDepthSkipMarketResponse {
            condition_id,
            market_slug,
            skipped: true,
            reason: "up_book_missing_bid".to_string(),
            up_submit_bid_price: None,
            down_submit_bid_price: None,
            up_ext_top_bid_usd: None,
            down_ext_top_bid_usd: None,
            pair_min_top_depth_usd: None,
        };
    };
    let Some(down_best_bid) = mm_sport_alpha_best_bid(&down_book) else {
        return MmSportDepthSkipMarketResponse {
            condition_id,
            market_slug,
            skipped: true,
            reason: "down_book_missing_bid".to_string(),
            up_submit_bid_price: None,
            down_submit_bid_price: None,
            up_ext_top_bid_usd: None,
            down_ext_top_bid_usd: None,
            pair_min_top_depth_usd: None,
        };
    };
    let up_submit_bid_price = mm_sport_alpha_passive_entry_price(up_best_bid, tick_size);
    let down_submit_bid_price = mm_sport_alpha_passive_entry_price(down_best_bid, tick_size);
    let (_, up_ext_top_bid_usd) =
        mm_sport_alpha_bid_depth_at_or_above(&up_book, up_submit_bid_price, tick_size);
    let (_, down_ext_top_bid_usd) =
        mm_sport_alpha_bid_depth_at_or_above(&down_book, down_submit_bid_price, tick_size);
    let pair_min_top_depth_usd = up_ext_top_bid_usd.min(down_ext_top_bid_usd);
    let skipped = !pair_min_top_depth_usd.is_finite() || pair_min_top_depth_usd < depth_floor_usd;
    MmSportDepthSkipMarketResponse {
        condition_id,
        market_slug,
        skipped,
        reason: if skipped {
            "pair_depth_below_floor".to_string()
        } else {
            "ok".to_string()
        },
        up_submit_bid_price: finite_json_number(up_submit_bid_price),
        down_submit_bid_price: finite_json_number(down_submit_bid_price),
        up_ext_top_bid_usd: finite_json_number(up_ext_top_bid_usd),
        down_ext_top_bid_usd: finite_json_number(down_ext_top_bid_usd),
        pair_min_top_depth_usd: finite_json_number(pair_min_top_depth_usd),
    }
}

async fn alpha_mm_sport_depth_skip_handler(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let payload: MmSportDepthSkipRequest =
        parse_json_request(&state, remote.ip(), &headers, &body)?;
    ensure_builder_code_authorized(&state.settings, &headers, payload.builder_code.as_deref())?;

    if payload.strategy_id.trim() != "mm_sport_v1" {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "strategy_id must be mm_sport_v1",
        ));
    }
    if payload.markets.len() > MM_SPORT_DEPTH_SKIP_MAX_MARKETS {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "markets exceeds maximum {}",
                MM_SPORT_DEPTH_SKIP_MAX_MARKETS
            ),
        ));
    }
    let depth_floor_usd = if payload.depth_floor_usd.is_finite() && payload.depth_floor_usd >= 0.0 {
        payload.depth_floor_usd
    } else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "depth_floor_usd must be finite and non-negative",
        ));
    };

    let api = mm_sport_depth_skip_api_for_request(&state, payload.clob_version.as_deref());
    let mut markets = stream::iter(payload.markets.into_iter())
        .map(|market| {
            let api = api.clone();
            async move { evaluate_mm_sport_depth_skip_market(api, market, depth_floor_usd).await }
        })
        .buffer_unordered(MM_SPORT_DEPTH_SKIP_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    markets.sort_by(|a, b| a.condition_id.cmp(&b.condition_id));
    let skipped_condition_ids = markets
        .iter()
        .filter(|market| market.skipped)
        .map(|market| market.condition_id.clone())
        .collect::<Vec<_>>();

    Ok(Json(MmSportDepthSkipResponse {
        ok: true,
        kind: "depth_skip".to_string(),
        request_ts_ms: payload.request_ts_ms,
        depth_floor_usd,
        checked_count: markets.len(),
        skipped_condition_ids,
        markets,
    }))
}

async fn alpha_mm_rewards_selection_compat_handler(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let payload: Value = parse_json_request(&state, remote.ip(), &headers, &body)?;
    let builder_code = payload.get("builder_code").and_then(Value::as_str);
    ensure_builder_code_authorized(&state.settings, &headers, builder_code)?;

    let enabled_modes = payload
        .get("enabled_modes")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let selection_pool = payload
        .get("selection_pool")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .clamp(1, 50) as usize;
    let mut ranked = payload
        .get("candidates")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|candidate| {
                    let condition_id = candidate.get("condition_id")?.as_str()?.trim();
                    if condition_id.is_empty() {
                        return None;
                    }
                    let reward_daily_rate = candidate
                        .get("reward_daily_rate")
                        .and_then(Value::as_f64)
                        .filter(|value| value.is_finite())
                        .unwrap_or(0.0);
                    Some((condition_id.to_string(), reward_daily_rate))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.dedup_by(|a, b| a.0 == b.0);

    let selected_ids = ranked
        .iter()
        .take(selection_pool)
        .map(|(condition_id, _)| condition_id.clone())
        .collect::<Vec<_>>();
    let selected_by_mode = enabled_modes
        .iter()
        .map(|mode| {
            json!({
                "mode": mode,
                "condition_ids": selected_ids,
            })
        })
        .collect::<Vec<_>>();

    let force_threshold = payload
        .get("auto_force_include_reward_min")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or(0.0);
    let force_include_ids = if payload
        .get("auto_force_top_reward")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        ranked
            .first()
            .map(|(condition_id, _)| vec![condition_id.clone()])
            .unwrap_or_default()
    } else if force_threshold > 0.0 {
        ranked
            .iter()
            .filter(|(_, reward_daily_rate)| *reward_daily_rate >= force_threshold)
            .map(|(condition_id, _)| condition_id.clone())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let force_include_by_mode = enabled_modes
        .iter()
        .filter(|_| !force_include_ids.is_empty())
        .map(|mode| {
            json!({
                "mode": mode,
                "condition_ids": force_include_ids,
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(json!({
        "ok": true,
        "selected_by_mode": selected_by_mode,
        "force_include_by_mode": force_include_by_mode,
        "source": "compat_mm_rewards_selection",
    })))
}

async fn alpha_mm_rewards_preflight_compat_handler(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let payload: Value = parse_json_request(&state, remote.ip(), &headers, &body)?;
    let builder_code = payload.get("builder_code").and_then(Value::as_str);
    ensure_builder_code_authorized(&state.settings, &headers, builder_code)?;

    let covered_levels = payload
        .get("rungs")
        .and_then(Value::as_array)
        .map(|rungs| rungs.len())
        .unwrap_or(0);

    Ok(Json(json!({
        "ok": true,
        "covered_levels": covered_levels,
        "block_reason": null,
        "block_detail": null,
        "source": "compat_mm_rewards_preflight",
    })))
}

async fn alpha_evcurve_handler(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let payload: EvcurveRequest = parse_json_request(&state, remote.ip(), &headers, &body)?;
    ensure_builder_code_authorized(&state.settings, &headers, payload.builder_code.as_deref())?;

    let timeframe = parse_timeframe(payload.timeframe.as_str()).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "timeframe must be one of 5m,15m,1h,4h,1d",
        )
    })?;

    validate_mids_and_asks(
        payload.base_mid,
        payload.current_mid,
        payload.ask_up,
        payload.ask_down,
    )?;

    let symbol = normalize_symbol(payload.symbol.as_str());

    if timeframe == Timeframe::D1 {
        let Some(tables) = state.plandaily_tables.as_ref() else {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "plandaily tables unavailable",
            ));
        };

        let candidates = evcurve::evaluate_d1_candidates(
            &state.evcurve_cfg,
            tables.as_ref(),
            symbol.as_str(),
            payload.period_open_ts,
            payload.tau_sec,
            payload.base_mid,
            payload.current_mid,
            payload.ask_up,
            payload.ask_down,
            payload.d1_zero_rule_already_fired,
            payload.d1_ev_rule_already_fired,
        );

        let rendered = candidates
            .iter()
            .map(evcurve_candidate_to_json)
            .collect::<Vec<_>>();

        let best = candidates
            .iter()
            .filter(|candidate| candidate.decision.should_buy)
            .max_by(|a, b| a.score.total_cmp(&b.score))
            .map(evcurve_candidate_to_json);

        return Ok(Json(json!({
            "ok": true,
            "kind": "d1_candidates",
            "symbol": symbol,
            "timeframe": timeframe.as_str(),
            "candidates": rendered,
            "best": best,
        })));
    }

    let decision = evcurve::evaluate_decision(
        &state.evcurve_cfg,
        state.plan3_tables.as_ref(),
        timeframe,
        symbol.as_str(),
        payload.period_open_ts,
        payload.tau_sec,
        payload.base_mid,
        payload.current_mid,
        payload.ask_up,
        payload.ask_down,
    );

    Ok(Json(json!({
        "ok": true,
        "kind": "decision",
        "symbol": symbol,
        "timeframe": timeframe.as_str(),
        "decision": evcurve_decision_to_json(&decision),
    })))
}

async fn alpha_sessionband_handler(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let payload: SessionbandRequest = parse_json_request(&state, remote.ip(), &headers, &body)?;
    ensure_builder_code_authorized(&state.settings, &headers, payload.builder_code.as_deref())?;

    let timeframe = parse_timeframe(payload.timeframe.as_str()).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "timeframe must be one of 5m,15m,1h,4h",
        )
    })?;

    if timeframe == Timeframe::D1 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "sessionband does not support 1d timeframe",
        ));
    }

    validate_mids_and_asks(
        payload.base_mid,
        payload.current_mid,
        payload.ask_up,
        payload.ask_down,
    )?;

    let symbol = normalize_symbol(payload.symbol.as_str());
    let direction_up = payload.current_mid >= payload.base_mid;
    let direction = if direction_up { "UP" } else { "DOWN" };
    let lead_pct = ((payload.current_mid - payload.base_mid).abs()
        / payload.base_mid.max(f64::EPSILON)
        * 100.0)
        .max(0.0);

    let session_index = Plan4bTables::session_index_for_period_open_utc(payload.period_open_ts);
    let watch = session_index.and_then(|idx| {
        let key = SessionWatchKey {
            symbol: symbol.clone(),
            timeframe,
            session_index: idx,
        };
        state
            .watch_starts
            .get(&key)
            .copied()
            .map(|watch_start| (idx, watch_start))
    });

    let chosen_ask = if direction_up {
        payload.ask_up
    } else {
        payload.ask_down
    }
    .filter(|value| value.is_finite() && *value > 0.0);

    let band = state.sessionband_cfg.band_for_lead_pct(lead_pct);

    let mut should_buy = false;
    let mut skip_reason: Option<String> = None;
    let mut score_bps: Option<f64> = None;

    if payload.tau_sec <= 0 {
        skip_reason = Some("tau_non_positive".to_string());
    } else if watch.is_none() {
        skip_reason = Some("no_watch_start".to_string());
    } else if payload.tau_sec
        > watch
            .map(|(_, watch_start)| watch_start.watch_start_sec)
            .unwrap_or(i64::MAX)
    {
        skip_reason = Some("prewatch_not_started".to_string());
    } else if !state.sessionband_cfg.tau_allowed(payload.tau_sec) {
        skip_reason = Some("tau_not_allowed".to_string());
    } else if band.is_none() {
        skip_reason = Some("lead_out_of_range".to_string());
    } else if chosen_ask.is_none() {
        skip_reason = Some("book_empty".to_string());
    } else if !band
        .map(|b| b.contains_price(chosen_ask.unwrap_or_default()))
        .unwrap_or(false)
    {
        skip_reason = Some("ask_out_of_band".to_string());
    } else {
        should_buy = true;
        if let (Some(valid_band), Some(ask)) = (band, chosen_ask) {
            score_bps = Some(((valid_band.price_max - ask) * 10_000.0).max(0.0));
        }
    }

    let response = SessionbandResponse {
        symbol,
        timeframe: timeframe.as_str().to_string(),
        period_open_ts: payload.period_open_ts,
        tau_sec: payload.tau_sec,
        lead_pct,
        direction: direction.to_string(),
        session_index: watch.map(|(idx, _)| idx),
        watch_start_sec: watch.map(|(_, watch_start)| watch_start.watch_start_sec),
        tau_trigger_sec: watch.map(|(_, watch_start)| watch_start.tau_trigger_sec),
        trigger_rate_pct: watch.map(|(_, watch_start)| watch_start.trigger_rate_pct),
        should_buy,
        skip_reason,
        chosen_ask,
        band_price_min: band.map(|item| item.price_min),
        band_price_max: band.map(|item| item.price_max),
        score_bps,
    };

    Ok(Json(json!({ "ok": true, "result": response })))
}

async fn alpha_premarket_should_trade_handler(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let payload: PremarketAlphaShouldTradeRequest =
        parse_json_request(&state, remote.ip(), &headers, &body)?;
    ensure_builder_code_authorized(&state.settings, &headers, payload.builder_code.as_deref())?;
    let timeframe = parse_timeframe(payload.timeframe.as_str()).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "timeframe must be one of 5m,15m,1h,4h",
        )
    })?;
    if timeframe == Timeframe::D1 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "premarket alpha does not support 1d timeframe",
        ));
    }
    if !payload
        .strategy_id
        .trim()
        .eq_ignore_ascii_case("premarket_v1")
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "strategy_id must be premarket_v1",
        ));
    }
    if payload.market_open_ts <= 0 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "market_open_ts must be > 0",
        ));
    }
    if payload.decision_id.trim().is_empty() || payload.nonce.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "decision_id and nonce are required",
        ));
    }
    let now_ms = Utc::now().timestamp_millis();
    if now_ms.saturating_sub(payload.ts_ms).abs() > 120_000 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "ts_ms outside freshness window",
        ));
    }
    let proxy_wallet =
        ensure_proxy_wallet_authorized(&state.settings, &headers, payload.proxy_wallet.as_str())?;
    let symbol = normalize_symbol(payload.symbol.as_str());
    let yes_min = state
        .settings
        .premarket_yes_min
        .min(state.settings.premarket_yes_max)
        .clamp(0.0, 1.0);
    let yes_max = state
        .settings
        .premarket_yes_min
        .max(state.settings.premarket_yes_max)
        .clamp(0.0, 1.0);

    let yes_prob_seed = format!(
        "premarket:prob:{}:{}:{}:{}:{}",
        payload.decision_id,
        symbol,
        timeframe.as_str(),
        payload.market_open_ts,
        payload.ts_ms
    );
    let yes_seed = format!(
        "premarket:yes:{}:{}:{}:{}:{}",
        payload.decision_id, payload.nonce, proxy_wallet, symbol, now_ms
    );
    let yes_prob = yes_min + (yes_max - yes_min) * seeded_unit(yes_prob_seed.as_str());
    let should_trade = seeded_unit(yes_seed.as_str()) < yes_prob;
    let reason = if should_trade {
        "random_gate_yes"
    } else {
        "random_gate_no"
    };

    Ok(Json(PremarketAlphaShouldTradeResponse {
        ok: true,
        should_trade,
        source: "remote".to_string(),
        reason: reason.to_string(),
        yes_prob,
    }))
}

async fn alpha_premarket_ladder_handler(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let payload: PremarketAlphaLadderRequest =
        parse_json_request(&state, remote.ip(), &headers, &body)?;
    ensure_builder_code_authorized(&state.settings, &headers, payload.builder_code.as_deref())?;
    let timeframe = parse_timeframe(payload.timeframe.as_str()).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "timeframe must be one of 5m,15m,1h,4h",
        )
    })?;
    if timeframe == Timeframe::D1 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "premarket alpha does not support 1d timeframe",
        ));
    }
    if !payload
        .strategy_id
        .trim()
        .eq_ignore_ascii_case("premarket_v1")
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "strategy_id must be premarket_v1",
        ));
    }
    if payload.market_open_ts <= 0 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "market_open_ts must be > 0",
        ));
    }
    if payload.decision_id.trim().is_empty() || payload.nonce.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "decision_id and nonce are required",
        ));
    }
    let now_ms = Utc::now().timestamp_millis();
    if now_ms.saturating_sub(payload.ts_ms).abs() > 120_000 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "ts_ms outside freshness window",
        ));
    }
    let proxy_wallet =
        ensure_proxy_wallet_authorized(&state.settings, &headers, payload.proxy_wallet.as_str())?;
    let symbol = normalize_symbol(payload.symbol.as_str());

    if payload.base_prices.is_empty() || payload.base_prices.len() > 16 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "base_prices must contain 1..16 rungs",
        ));
    }
    let mut previous = f64::INFINITY;
    for (idx, price) in payload.base_prices.iter().enumerate() {
        if !price.is_finite() || *price < 0.01 || *price > 0.99 {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("base_prices[{idx}] must be within 0.01..0.99"),
            ));
        }
        if *price > previous + 1e-9 {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "base_prices must be descending",
            ));
        }
        previous = *price;
    }

    let shift_seed = format!(
        "premarket:ladder:{}:{}:{}:{}:{}:{}",
        payload.decision_id,
        payload.nonce,
        proxy_wallet,
        symbol,
        timeframe.as_str(),
        payload.market_open_ts
    );
    let shift_pct = (seeded_unit(shift_seed.as_str()) * 0.20) - 0.10;
    let shift_factor = 1.0 + shift_pct;
    let mut shifted = Vec::with_capacity(payload.base_prices.len());
    let mut previous_shifted = f64::INFINITY;
    for base_price in payload.base_prices.iter().copied() {
        let mut price = round_price_to_cent(base_price * shift_factor).clamp(0.01, 0.99);
        if price > previous_shifted {
            price = (previous_shifted - 0.01).max(0.01);
        }
        shifted.push(price);
        previous_shifted = price;
    }

    Ok(Json(PremarketAlphaLadderResponse {
        ok: true,
        source: "remote".to_string(),
        reason: "aligned_price_shift".to_string(),
        shift_pct,
        prices: shifted,
    }))
}

async fn alpha_endgame_policy_handler(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let payload: EndgameAlphaPolicyRequest =
        parse_json_request(&state, remote.ip(), &headers, &body)?;
    let legacy_sdk1_request =
        state.settings.endgame_legacy_sdk1_compat && payload.builder_code.is_none();
    if !(legacy_sdk1_request && request_uses_service_token(&state.settings, &headers)) {
        ensure_builder_code_authorized(&state.settings, &headers, payload.builder_code.as_deref())?;
    }
    let timeframe = parse_timeframe(payload.timeframe.as_str()).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "timeframe must be one of 5m,15m,1h,4h",
        )
    })?;
    if timeframe == Timeframe::D1 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "endgame alpha does not support 1d timeframe",
        ));
    }
    if !payload
        .strategy_id
        .trim()
        .eq_ignore_ascii_case("endgame_sweep_v1")
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "strategy_id must be endgame_sweep_v1",
        ));
    }
    if payload.market_open_ts <= 0 || payload.market_close_ts <= payload.market_open_ts {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid market_open_ts/market_close_ts",
        ));
    }
    if payload.nonce.trim().is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "nonce is required"));
    }
    let now_ms = Utc::now().timestamp_millis();
    if now_ms.saturating_sub(payload.request_ts_ms).abs() > 120_000 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "request_ts_ms outside freshness window",
        ));
    }
    let proxy_wallet =
        ensure_proxy_wallet_authorized(&state.settings, &headers, payload.proxy_wallet.as_str())?;
    let symbol = normalize_symbol(payload.symbol.as_str());
    let base_offsets_ms = if legacy_sdk1_request {
        state
            .settings
            .endgame_legacy_sdk1_base_offsets_ms
            .as_slice()
    } else {
        state.settings.endgame_base_offsets_ms.as_slice()
    };

    let mut tick_offsets_ms = base_offsets_ms
        .iter()
        .enumerate()
        .filter_map(|(idx, base)| {
            let seed = format!(
                "endgame:offset:{}:{}:{}:{}:{}:{}:{}",
                symbol,
                timeframe.as_str(),
                payload.market_open_ts,
                payload.market_close_ts,
                payload.request_ts_ms,
                payload.nonce,
                idx
            );
            if legacy_sdk1_request {
                Some(
                    seeded_jitter_ms(
                        i64::try_from(*base).unwrap_or(120_000),
                        state.settings.endgame_legacy_sdk1_offset_jitter_ms,
                        seed.as_str(),
                    )
                    .clamp(50, 120_000) as u64,
                )
            } else {
                Some(seeded_near_t_offset_ms(
                    *base,
                    state.settings.endgame_near_t_random_max_bps,
                    seed.as_str(),
                ))
            }
        })
        .collect::<Vec<_>>();
    tick_offsets_ms.sort_by(|a, b| b.cmp(a));
    tick_offsets_ms.dedup();
    if tick_offsets_ms.is_empty() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "endgame policy offsets unavailable",
        ));
    }

    let stale_seed = format!(
        "endgame:stale:{}:{}:{}:{}:{}:{}",
        symbol,
        timeframe.as_str(),
        payload.market_open_ts,
        payload.market_close_ts,
        payload.request_ts_ms,
        proxy_wallet
    );
    let submit_proxy_max_age_ms = seeded_jitter_ms(
        state.settings.endgame_submit_proxy_max_age_base_ms,
        state.settings.endgame_submit_proxy_max_age_jitter_ms,
        stale_seed.as_str(),
    )
    .clamp(50, 10_000);

    Ok(Json(EndgameAlphaPolicyResponse {
        ok: true,
        tick_offsets_ms,
        submit_proxy_max_age_ms,
        source: "remote".to_string(),
        reason: if legacy_sdk1_request {
            "legacy_sdk1_randomized_policy"
        } else {
            "near_t_randomized_policy"
        }
        .to_string(),
    }))
}

fn parse_json_request<T: for<'de> Deserialize<'de>>(
    state: &AppState,
    ip: IpAddr,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<T, ApiError> {
    guard_request(state, ip, headers, body.len())?;
    serde_json::from_slice(body)
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid JSON request body"))
}

fn guard_onboard_request(state: &AppState, ip: IpAddr, body_len: usize) -> Result<(), ApiError> {
    if body_len > state.settings.max_body_bytes {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body too large",
        ));
    }
    let client_ip = ip;
    if !state.rate_limiter.allow(client_ip) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit exceeded",
        ));
    }
    Ok(())
}

fn guard_request(
    state: &AppState,
    ip: IpAddr,
    headers: &HeaderMap,
    body_len: usize,
) -> Result<(), ApiError> {
    if body_len > state.settings.max_body_bytes {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body too large",
        ));
    }

    if let Some(expected_token) = state.settings.token.as_deref() {
        let provided = headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        let expected_header = format!("Bearer {}", expected_token);
        if provided != expected_header && !auto_alpha_authorized(&state.settings, headers) {
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "missing or invalid bearer token",
            ));
        }
    }

    if state.settings.require_wallet_header {
        let wallet_header = HeaderName::from_static("x-wallet-address");
        let wallet = headers
            .get(wallet_header)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .trim();
        if !is_valid_wallet(wallet) {
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "missing or invalid x-wallet-address",
            ));
        }
    }

    if !state.rate_limiter.allow(ip) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit exceeded",
        ));
    }

    Ok(())
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

type HmacSha256 = Hmac<Sha256>;

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(format!("{:02x}", byte).as_str());
    }
    out
}

fn alpha_auto_key_secret(settings: &Settings) -> Result<&str, ApiError> {
    settings
        .auto_key_secret
        .as_deref()
        .or(settings.token.as_deref())
        .ok_or_else(|| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "alpha key secret missing"))
}

fn auto_alpha_signature(settings: &Settings, wallet: &str) -> Result<String, ApiError> {
    let mut mac =
        HmacSha256::new_from_slice(alpha_auto_key_secret(settings)?.as_bytes()).map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid alpha key secret",
            )
        })?;
    mac.update(b"evpoly:auto-alpha:v1:");
    mac.update(wallet.as_bytes());
    mac.update(b":");
    mac.update(builder_attribution::official_builder_code().as_bytes());
    let digest = mac.finalize().into_bytes();
    Ok(hex_lower(&digest[..16]))
}

fn auto_alpha_key_for_wallet(settings: &Settings, wallet: &str) -> Result<String, ApiError> {
    let normalized = normalize_wallet(wallet);
    let wallet_part = normalized.trim_start_matches("0x");
    let sig = auto_alpha_signature(settings, normalized.as_str())?;
    Ok(format!("{}_{}_{}", AUTO_ALPHA_KEY_PREFIX, wallet_part, sig))
}

fn auto_alpha_authorized(settings: &Settings, headers: &HeaderMap) -> bool {
    let Some(token) = bearer_token(headers) else {
        return false;
    };
    if !token.starts_with(AUTO_ALPHA_KEY_PREFIX) {
        return false;
    }
    let Some(wallet) = wallet_header_value(headers) else {
        return false;
    };
    auto_alpha_key_for_wallet(settings, wallet.as_str())
        .map(|expected| constant_time_eq(expected.as_bytes(), token.as_bytes()))
        .unwrap_or(false)
}

fn request_uses_auto_alpha(headers: &HeaderMap) -> bool {
    bearer_token(headers)
        .map(|token| token.starts_with(AUTO_ALPHA_KEY_PREFIX))
        .unwrap_or(false)
}

fn request_uses_service_token(settings: &Settings, headers: &HeaderMap) -> bool {
    let Some(expected) = settings.token.as_deref() else {
        return false;
    };
    bearer_token(headers)
        .map(|token| constant_time_eq(expected.as_bytes(), token.as_bytes()))
        .unwrap_or(false)
}

fn builder_code_matches_official(builder_code: Option<&str>) -> bool {
    builder_code
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.eq_ignore_ascii_case(builder_attribution::official_builder_code()))
        .unwrap_or(false)
}

fn ensure_builder_code_authorized(
    settings: &Settings,
    headers: &HeaderMap,
    builder_code: Option<&str>,
) -> Result<(), ApiError> {
    if settings.require_builder_code || request_uses_auto_alpha(headers) {
        if !builder_code_matches_official(builder_code) {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "official builder code required",
            ));
        }
    }
    Ok(())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in a.iter().zip(b.iter()) {
        diff |= left ^ right;
    }
    diff == 0
}

fn is_valid_wallet(value: &str) -> bool {
    if value.len() != 42 || !value.starts_with("0x") {
        return false;
    }
    value.chars().skip(2).all(|ch| ch.is_ascii_hexdigit())
}

fn normalize_wallet(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn wallet_header_value(headers: &HeaderMap) -> Option<String> {
    let wallet_header = HeaderName::from_static("x-wallet-address");
    headers
        .get(wallet_header)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(normalize_wallet)
}

fn ensure_proxy_wallet_authorized(
    settings: &Settings,
    headers: &HeaderMap,
    proxy_wallet: &str,
) -> Result<String, ApiError> {
    let normalized = normalize_wallet(proxy_wallet);
    if !is_valid_wallet(normalized.as_str()) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "missing or invalid proxy wallet",
        ));
    }
    if !settings.allowed_proxy_wallets.is_empty()
        && !settings.allowed_proxy_wallets.contains(normalized.as_str())
    {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "proxy wallet not allowed",
        ));
    }
    if let Some(header_wallet) = wallet_header_value(headers) {
        if !header_wallet.eq_ignore_ascii_case(normalized.as_str()) {
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "proxy wallet/header mismatch",
            ));
        }
    }
    Ok(normalized)
}

fn seeded_unit(seed: &str) -> f64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut hasher);
    let raw = hasher.finish();
    (raw as f64) / (u64::MAX as f64)
}

fn round_price_to_cent(value: f64) -> f64 {
    ((value * 100.0).round() / 100.0).clamp(0.01, 0.99)
}

fn seeded_jitter_ms(base: i64, max_abs_jitter_ms: i64, seed: &str) -> i64 {
    if max_abs_jitter_ms <= 0 {
        return base;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut hasher);
    let raw = hasher.finish();
    let span = u64::try_from(max_abs_jitter_ms.saturating_mul(2).saturating_add(1)).unwrap_or(1);
    let slot = i64::try_from(raw % span).unwrap_or(0) - max_abs_jitter_ms;
    base.saturating_add(slot)
}

fn seeded_near_t_offset_ms(base_ms: u64, max_near_t_bps: u32, seed: &str) -> u64 {
    let base_ms = base_ms.clamp(50, 120_000);
    let max_shift_ms = ((base_ms as u128) * u128::from(max_near_t_bps.min(10_000)) / 10_000)
        .min(u128::from(base_ms.saturating_sub(50))) as u64;
    if max_shift_ms == 0 {
        return base_ms;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut hasher);
    let shift_ms = hasher.finish() % max_shift_ms.saturating_add(1);
    base_ms.saturating_sub(shift_ms).clamp(50, 120_000)
}

fn normalize_endgame_offsets_ms(mut offsets: Vec<u64>) -> Vec<u64> {
    offsets.retain(|value| *value > 0);
    offsets.sort_by(|a, b| b.cmp(a));
    offsets.dedup();
    offsets
}

fn parse_endgame_offsets_ms_env(name: &str, default_values: &[u64]) -> Vec<u64> {
    let offsets = std::env::var(name)
        .ok()
        .map(|raw| {
            raw.split(',')
                .filter_map(|part| part.trim().parse::<u64>().ok())
                .collect::<Vec<_>>()
        })
        .map(normalize_endgame_offsets_ms)
        .filter(|values| !values.is_empty())
        .unwrap_or_else(|| default_values.to_vec());
    normalize_endgame_offsets_ms(offsets)
}

fn validate_mids_and_asks(
    base_mid: f64,
    current_mid: f64,
    ask_up: Option<f64>,
    ask_down: Option<f64>,
) -> Result<(), ApiError> {
    if !base_mid.is_finite() || base_mid <= 0.0 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "base_mid must be finite and > 0",
        ));
    }
    if !current_mid.is_finite() || current_mid <= 0.0 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "current_mid must be finite and > 0",
        ));
    }

    for (label, value) in [("ask_up", ask_up), ("ask_down", ask_down)] {
        if let Some(price) = value {
            if !price.is_finite() || price <= 0.0 || price > 1.0 {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("{} must be in (0,1] when provided", label),
                ));
            }
        }
    }

    Ok(())
}

fn evcurve_decision_to_json(decision: &evcurve::EvcurveDecision) -> Value {
    json!({
        "should_buy": decision.should_buy,
        "skip_reason": decision.skip_reason,
        "hold_side": decision.hold_side.as_str(),
        "group_key": decision.group_key,
        "tau_sec": decision.tau_sec,
        "base_mid": decision.base_mid,
        "current_mid": decision.current_mid,
        "lead_pct": decision.lead_pct,
        "lead_bin_idx": decision.lead_bin_idx,
        "flips": decision.flips,
        "n": decision.n,
        "p_flip": decision.p_flip,
        "p_hold": decision.p_hold,
        "max_buy_hold": decision.max_buy_hold,
        "ask_up": decision.ask_up,
        "ask_down": decision.ask_down,
        "chosen_ask": decision.chosen_ask,
        "tau_low_sec": decision.tau_low_sec,
        "tau_high_sec": decision.tau_high_sec,
    })
}

fn evcurve_candidate_to_json(candidate: &evcurve::EvcurveDecisionCandidate) -> Value {
    json!({
        "sub_strategy": candidate.sub_strategy,
        "score": candidate.score,
        "p_flip_market": candidate.p_flip_market,
        "gap_abs": candidate.gap_abs,
        "decision": evcurve_decision_to_json(&candidate.decision),
    })
}

fn parse_timeframe(value: &str) -> Option<Timeframe> {
    match value.trim().to_ascii_lowercase().as_str() {
        "5m" | "m5" => Some(Timeframe::M5),
        "15m" | "m15" => Some(Timeframe::M15),
        "1h" | "h1" | "60m" => Some(Timeframe::H1),
        "4h" | "h4" | "240m" => Some(Timeframe::H4),
        "1d" | "d1" | "24h" => Some(Timeframe::D1),
        _ => None,
    }
}

fn normalize_symbol(symbol: &str) -> String {
    match symbol.trim().to_ascii_uppercase().as_str() {
        "SOLANA" => "SOL".to_string(),
        other => other.to_string(),
    }
}

fn timeframe_slug_candidates(timeframe: Timeframe) -> &'static [&'static str] {
    match timeframe {
        Timeframe::M5 => &["5m"],
        Timeframe::M15 => &["15m"],
        Timeframe::H1 => &["1h", "60m", "hourly"],
        Timeframe::H4 => &["4h", "240m"],
        Timeframe::D1 => &["1d", "24h", "daily"],
    }
}

fn market_symbol_slug_prefixes(symbol: &str) -> &'static [&'static str] {
    match normalize_symbol(symbol).as_str() {
        "BTC" => &["btc"],
        "ETH" => &["eth"],
        "SOL" => &["sol", "solana"],
        "XRP" => &["xrp"],
        _ => &["btc"],
    }
}

fn h1_event_slug_asset_prefix(symbol: &str) -> Option<&'static str> {
    match normalize_symbol(symbol).as_str() {
        "BTC" => Some("bitcoin"),
        "ETH" => Some("ethereum"),
        "SOL" => Some("solana"),
        "XRP" => Some("xrp"),
        _ => None,
    }
}

fn h1_event_slug_from_et(symbol: &str, dt_et: chrono::DateTime<chrono_tz::Tz>) -> Option<String> {
    let asset_prefix = h1_event_slug_asset_prefix(symbol)?;
    let month = dt_et.format("%B").to_string().to_ascii_lowercase();
    let day = dt_et.day();
    let hour24 = dt_et.hour();
    let (hour12, suffix) = match hour24 {
        0 => (12, "am"),
        1..=11 => (hour24, "am"),
        12 => (12, "pm"),
        _ => (hour24 - 12, "pm"),
    };
    Some(format!(
        "{}-up-or-down-{}-{}-{}{}-et",
        asset_prefix, month, day, hour12, suffix
    ))
}

fn h1_target_slug_for_open_ts(symbol: &str, target_open_ts: u64) -> Option<String> {
    let target_utc = chrono::DateTime::<Utc>::from_timestamp(target_open_ts as i64, 0)?;
    let target_et = target_utc.with_timezone(&New_York);
    h1_event_slug_from_et(symbol, target_et)
}

fn h1_event_slug_candidates(
    symbol: &str,
    target_open_ts: u64,
    allow_next_hour_fallback: bool,
) -> Vec<SlugCandidate> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    if let Some(target_slug) = h1_target_slug_for_open_ts(symbol, target_open_ts) {
        if seen.insert(target_slug.clone()) {
            out.push(SlugCandidate {
                slug: target_slug,
                open_ts: target_open_ts,
                source: "target",
            });
        }
    }

    if allow_next_hour_fallback {
        let now_et = Utc::now().with_timezone(&New_York);
        let rounded_now_et = now_et
            .with_minute(0)
            .and_then(|dt| dt.with_second(0))
            .and_then(|dt| dt.with_nanosecond(0))
            .unwrap_or(now_et);
        let next_et_hour = rounded_now_et + chrono::Duration::hours(1);
        let fallback_open_ts = u64::try_from(next_et_hour.with_timezone(&Utc).timestamp())
            .ok()
            .unwrap_or(target_open_ts);
        if let Some(fallback_slug) = h1_event_slug_from_et(symbol, next_et_hour) {
            if seen.insert(fallback_slug.clone()) {
                out.push(SlugCandidate {
                    slug: fallback_slug,
                    open_ts: fallback_open_ts,
                    source: "next_hour_fallback",
                });
            }
        }
    }

    out
}

fn d1_period_bounds_for_timestamp(ts: i64) -> Option<(i64, i64)> {
    use chrono::TimeZone;

    let dt_utc = chrono::DateTime::<Utc>::from_timestamp(ts, 0)?;
    let dt_et = dt_utc.with_timezone(&New_York);
    let date = dt_et.date_naive();
    let noon_today = New_York
        .with_ymd_and_hms(date.year(), date.month(), date.day(), 12, 0, 0)
        .single()?;
    let open_et = if dt_et >= noon_today {
        noon_today
    } else {
        noon_today - chrono::Duration::days(1)
    };
    let close_et = open_et + chrono::Duration::days(1);
    Some((
        open_et.with_timezone(&Utc).timestamp(),
        close_et.with_timezone(&Utc).timestamp(),
    ))
}

fn d1_event_slug_candidates(symbol: &str, target_open_ts: u64) -> Vec<SlugCandidate> {
    let mut out = Vec::new();
    if let Some(asset_prefix) = h1_event_slug_asset_prefix(symbol) {
        if let Some(open_utc) = chrono::DateTime::<Utc>::from_timestamp(target_open_ts as i64, 0) {
            let close_et = (open_utc + chrono::Duration::days(1)).with_timezone(&New_York);
            let month = close_et.format("%B").to_string().to_ascii_lowercase();
            let day = close_et.day();
            out.push(SlugCandidate {
                slug: format!("{}-up-or-down-on-{}-{}", asset_prefix, month, day),
                open_ts: target_open_ts,
                source: "daily_event",
            });
        }
    }
    out
}

async fn discover_market_for_timeframe_once(
    api: &PolymarketApi,
    timeframe: Timeframe,
    target_open_ts: u64,
    symbol: &str,
    settings: &Settings,
) -> Result<Option<DiscoveredMarket>> {
    let symbol = normalize_symbol(symbol);
    let target_open_ts = if timeframe == Timeframe::D1 {
        d1_period_bounds_for_timestamp(target_open_ts as i64)
            .map(|(open, _)| open.max(0) as u64)
            .unwrap_or(target_open_ts)
    } else {
        let period_secs = timeframe.duration_seconds().max(1) as u64;
        (target_open_ts / period_secs) * period_secs
    };

    if timeframe == Timeframe::D1 {
        let candidates = d1_event_slug_candidates(symbol.as_str(), target_open_ts);
        if !candidates.is_empty() {
            for candidate in candidates {
                let Ok(market) = api.get_market_by_slug(candidate.slug.as_str()).await else {
                    continue;
                };
                if market.active
                    && !market.closed
                    && market
                        .slug
                        .trim()
                        .eq_ignore_ascii_case(candidate.slug.as_str())
                {
                    return Ok(Some(DiscoveredMarket {
                        market,
                        matched_open_ts: candidate.open_ts,
                        matched_slug: candidate.slug,
                        source: candidate.source,
                    }));
                }
            }
            return Ok(None);
        }
    }

    if timeframe == Timeframe::H1 {
        let candidates = h1_event_slug_candidates(
            symbol.as_str(),
            target_open_ts,
            settings.h1_allow_next_hour_fallback,
        );
        if !candidates.is_empty() {
            for candidate in candidates {
                if settings.h1_strict_match && candidate.open_ts != target_open_ts {
                    continue;
                }
                let Ok(market) = api.get_market_by_slug(candidate.slug.as_str()).await else {
                    continue;
                };
                if market.active
                    && !market.closed
                    && market
                        .slug
                        .trim()
                        .eq_ignore_ascii_case(candidate.slug.as_str())
                {
                    return Ok(Some(DiscoveredMarket {
                        market,
                        matched_open_ts: candidate.open_ts,
                        matched_slug: candidate.slug,
                        source: candidate.source,
                    }));
                }
            }
            return Ok(None);
        }
    }

    let mut slugs = Vec::new();
    for prefix in market_symbol_slug_prefixes(symbol.as_str()) {
        for tf_slug in timeframe_slug_candidates(timeframe) {
            slugs.push(format!("{}-updown-{}-{}", prefix, tf_slug, target_open_ts));
        }
    }

    for slug in slugs {
        let Ok(market) = api.get_market_by_slug(slug.as_str()).await else {
            continue;
        };
        if market.active && !market.closed {
            return Ok(Some(DiscoveredMarket {
                market,
                matched_open_ts: target_open_ts,
                matched_slug: slug,
                source: "target_slug",
            }));
        }
    }

    Ok(None)
}

fn load_settings() -> Result<Settings> {
    let bind = std::env::var("ALPHA_SERVICE_BIND")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_BIND.to_string());

    let port = std::env::var("ALPHA_SERVICE_PORT")
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);

    let token = std::env::var("ALPHA_SERVICE_TOKEN").ok().and_then(|v| {
        let trimmed = v.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    });
    let allow_unauth = std::env::var("ALPHA_ALLOW_UNAUTH")
        .ok()
        .and_then(|v| {
            let normalized = v.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "1" | "true" | "yes" | "on" => Some(true),
                "0" | "false" | "no" | "off" => Some(false),
                _ => None,
            }
        })
        .unwrap_or(false);
    if token.is_none() && !allow_unauth {
        anyhow::bail!(
            "ALPHA_SERVICE_TOKEN is required unless ALPHA_ALLOW_UNAUTH=true for non-production use"
        );
    }
    if token.is_none() {
        eprintln!(
            "warning: ALPHA_ALLOW_UNAUTH=true set with empty ALPHA_SERVICE_TOKEN; alpha service is unauthenticated"
        );
    }
    let auto_onboard_enabled = std::env::var("ALPHA_AUTO_ONBOARD_ENABLE")
        .ok()
        .and_then(|v| parse_bool(v.as_str()))
        .unwrap_or(true);
    let auto_key_secret = std::env::var("ALPHA_AUTO_KEY_SECRET")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let require_builder_code = std::env::var("ALPHA_REQUIRE_BUILDER_CODE")
        .ok()
        .and_then(|v| parse_bool(v.as_str()))
        .unwrap_or(false);

    let max_body_bytes = DEFAULT_MAX_BODY_BYTES.max(1024);

    let rate_limit_per_ip_rps = std::env::var("ALPHA_RATE_LIMIT_PER_IP_RPS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(20)
        .max(1);

    let rate_limit_per_ip_burst = std::env::var("ALPHA_RATE_LIMIT_PER_IP_BURST")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(40)
        .max(rate_limit_per_ip_rps);

    let rate_limit_global_rps = std::env::var("ALPHA_RATE_LIMIT_GLOBAL_RPS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(200)
        .max(1);

    let rate_limit_global_burst = std::env::var("ALPHA_RATE_LIMIT_GLOBAL_BURST")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(400)
        .max(rate_limit_global_rps);

    let require_wallet_header = std::env::var("ALPHA_REQUIRE_WALLET_HEADER")
        .ok()
        .and_then(|v| parse_bool(v.as_str()))
        .unwrap_or(false);

    let plan3_path = std::env::var("ALPHA_PLAN3_PATH")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_PLAN3_PATH.to_string());

    let plan4b_path = std::env::var("ALPHA_PLAN4B_PATH")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_PLAN4B_PATH.to_string());

    let plandaily_path = std::env::var("ALPHA_PLANDAILY_PATH")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_PLANDAILY_PATH.to_string());

    let gamma_url = std::env::var("POLY_GAMMA_API_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_GAMMA_URL.to_string());

    let clob_url = std::env::var("POLY_CLOB_API_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_CLOB_URL.to_string());
    let clob_v2_url = std::env::var("POLY_CLOB_V2_API_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_CLOB_V2_URL.to_string());

    let h1_strict_match = std::env::var("ALPHA_H1_DISCOVERY_STRICT_MATCH")
        .ok()
        .and_then(|v| parse_bool(v.as_str()))
        .unwrap_or(true);

    let h1_allow_next_hour_fallback = std::env::var("ALPHA_H1_DISCOVERY_ALLOW_NEXT_HOUR_FALLBACK")
        .ok()
        .and_then(|v| parse_bool(v.as_str()))
        .unwrap_or(false);

    let discovery_symbols = std::env::var("ALPHA_DISCOVERY_SYMBOLS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(normalize_symbol)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|symbols| !symbols.is_empty())
        .unwrap_or_else(|| {
            DEFAULT_DISCOVERY_SYMBOLS
                .iter()
                .map(|value| normalize_symbol(value))
                .collect::<Vec<_>>()
        });

    let discovery_refresh_sec = std::env::var("ALPHA_DISCOVERY_REFRESH_SEC")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_DISCOVERY_REFRESH_SEC)
        .clamp(5, 600);

    let evsnipe_refresh_sec = std::env::var("ALPHA_EVSNIPE_REFRESH_SEC")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_EVSNIPE_REFRESH_SEC)
        .clamp(5, 600);

    let discovery_back_periods = std::env::var("ALPHA_DISCOVERY_BACK_PERIODS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_DISCOVERY_BACK_PERIODS)
        .clamp(0, 5);

    let discovery_horizon_5m = std::env::var("ALPHA_DISCOVERY_HORIZON_5M")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_DISCOVERY_HORIZON_5M)
        .clamp(0, 10);

    let discovery_horizon_15m = std::env::var("ALPHA_DISCOVERY_HORIZON_15M")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_DISCOVERY_HORIZON_15M)
        .clamp(0, 10);

    let discovery_horizon_1h = std::env::var("ALPHA_DISCOVERY_HORIZON_1H")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_DISCOVERY_HORIZON_1H)
        .clamp(0, 10);

    let discovery_horizon_4h = std::env::var("ALPHA_DISCOVERY_HORIZON_4H")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_DISCOVERY_HORIZON_4H)
        .clamp(0, 10);

    let discovery_horizon_1d = std::env::var("ALPHA_DISCOVERY_HORIZON_1D")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_DISCOVERY_HORIZON_1D)
        .clamp(0, 7);

    let premarket_yes_min = std::env::var("ALPHA_PREMARKET_YES_MIN")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(DEFAULT_PREMARKET_YES_MIN)
        .clamp(0.0, 1.0);
    let premarket_yes_max = std::env::var("ALPHA_PREMARKET_YES_MAX")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(DEFAULT_PREMARKET_YES_MAX)
        .clamp(0.0, 1.0);

    let endgame_base_offsets_ms = parse_endgame_offsets_ms_env(
        "ALPHA_ENDGAME_BASE_OFFSETS_MS",
        DEFAULT_ENDGAME_BASE_OFFSETS_MS,
    );

    let endgame_legacy_sdk1_compat = std::env::var("ALPHA_ENDGAME_LEGACY_SDK1_COMPAT")
        .ok()
        .and_then(|v| parse_bool(v.as_str()))
        .unwrap_or(DEFAULT_ENDGAME_LEGACY_SDK1_COMPAT);
    let endgame_legacy_sdk1_base_offsets_ms = parse_endgame_offsets_ms_env(
        "ALPHA_ENDGAME_LEGACY_SDK1_BASE_OFFSETS_MS",
        DEFAULT_ENDGAME_LEGACY_SDK1_BASE_OFFSETS_MS,
    );
    let endgame_legacy_sdk1_offset_jitter_ms =
        std::env::var("ALPHA_ENDGAME_LEGACY_SDK1_OFFSET_JITTER_MS")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(DEFAULT_ENDGAME_LEGACY_SDK1_OFFSET_JITTER_MS)
            .clamp(0, 5_000);

    let endgame_near_t_random_max_bps = std::env::var("ALPHA_ENDGAME_NEAR_T_RANDOM_MAX_BPS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_ENDGAME_NEAR_T_RANDOM_MAX_BPS)
        .clamp(0, 10_000);
    let endgame_submit_proxy_max_age_base_ms =
        std::env::var("ALPHA_ENDGAME_SUBMIT_PROXY_MAX_AGE_BASE_MS")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(DEFAULT_ENDGAME_SUBMIT_PROXY_MAX_AGE_BASE_MS)
            .clamp(50, 10_000);
    let endgame_submit_proxy_max_age_jitter_ms =
        std::env::var("ALPHA_ENDGAME_SUBMIT_PROXY_MAX_AGE_JITTER_MS")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(DEFAULT_ENDGAME_SUBMIT_PROXY_MAX_AGE_JITTER_MS)
            .clamp(0, 5_000);

    let allowed_proxy_wallets = std::env::var("ALPHA_ALLOWED_PROXY_WALLETS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(normalize_wallet)
                .filter(|wallet| is_valid_wallet(wallet.as_str()))
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();

    Ok(Settings {
        bind,
        port,
        token,
        auto_onboard_enabled,
        auto_key_secret,
        require_builder_code,
        require_wallet_header,
        max_body_bytes,
        rate_limit_per_ip_rps,
        rate_limit_per_ip_burst,
        rate_limit_global_rps,
        rate_limit_global_burst,
        plan3_path,
        plan4b_path,
        plandaily_path,
        gamma_url,
        clob_url,
        clob_v2_url,
        h1_strict_match,
        h1_allow_next_hour_fallback,
        discovery_symbols,
        discovery_refresh_sec,
        evsnipe_refresh_sec,
        discovery_back_periods,
        discovery_horizon_5m,
        discovery_horizon_15m,
        discovery_horizon_1h,
        discovery_horizon_4h,
        discovery_horizon_1d,
        premarket_yes_min,
        premarket_yes_max,
        endgame_base_offsets_ms,
        endgame_near_t_random_max_bps,
        endgame_submit_proxy_max_age_base_ms,
        endgame_submit_proxy_max_age_jitter_ms,
        endgame_legacy_sdk1_compat,
        endgame_legacy_sdk1_base_offsets_ms,
        endgame_legacy_sdk1_offset_jitter_ms,
        allowed_proxy_wallets,
    })
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endgame_near_t_offsets_only_move_toward_close() {
        for base_ms in [2_000_u64, 1_000, 100] {
            for idx in 0..200 {
                let offset = seeded_near_t_offset_ms(
                    base_ms,
                    DEFAULT_ENDGAME_NEAR_T_RANDOM_MAX_BPS,
                    format!("seed-{base_ms}-{idx}").as_str(),
                );
                let min_offset = base_ms - ((base_ms as u128 * 2500 / 10_000) as u64);
                assert!(
                    offset >= min_offset,
                    "offset {offset} moved more than 25% from base {base_ms}"
                );
                assert!(
                    offset <= base_ms,
                    "offset {offset} moved away from T beyond base {base_ms}"
                );
            }
        }
    }

    #[test]
    fn endgame_default_offsets_are_t0_t1_t2() {
        assert_eq!(DEFAULT_ENDGAME_BASE_OFFSETS_MS, &[2_000, 1_000, 100]);
    }

    #[test]
    fn endgame_legacy_sdk1_offsets_keep_v1_t0_t1_t2() {
        assert_eq!(
            DEFAULT_ENDGAME_LEGACY_SDK1_BASE_OFFSETS_MS,
            &[3_000, 1_000, 100]
        );
        for base_ms in DEFAULT_ENDGAME_LEGACY_SDK1_BASE_OFFSETS_MS {
            for idx in 0..200 {
                let offset = seeded_jitter_ms(
                    i64::try_from(*base_ms).unwrap(),
                    DEFAULT_ENDGAME_LEGACY_SDK1_OFFSET_JITTER_MS,
                    format!("legacy-seed-{base_ms}-{idx}").as_str(),
                )
                .clamp(50, 120_000) as u64;
                let lower =
                    base_ms.saturating_sub(DEFAULT_ENDGAME_LEGACY_SDK1_OFFSET_JITTER_MS as u64);
                let upper =
                    base_ms.saturating_add(DEFAULT_ENDGAME_LEGACY_SDK1_OFFSET_JITTER_MS as u64);
                assert!(offset >= lower && offset <= upper);
            }
        }
    }

    #[test]
    fn mm_sport_depth_skip_defaults_to_legacy_clob_unless_v2_requested() {
        assert!(!mm_sport_depth_skip_wants_v2(None));
        assert!(!mm_sport_depth_skip_wants_v2(Some("")));
        assert!(!mm_sport_depth_skip_wants_v2(Some("v1")));
        assert!(!mm_sport_depth_skip_wants_v2(Some("legacy")));

        assert!(mm_sport_depth_skip_wants_v2(Some("v2")));
        assert!(mm_sport_depth_skip_wants_v2(Some(" clob-v2 ")));
        assert!(mm_sport_depth_skip_wants_v2(Some("CLOB_V2")));
    }
}
