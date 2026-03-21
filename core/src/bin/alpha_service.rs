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
use polymarket_arbitrage_bot::api::PolymarketApi;
use polymarket_arbitrage_bot::evcurve;
use polymarket_arbitrage_bot::evsnipe;
use polymarket_arbitrage_bot::mm::{self, MmMode};
use polymarket_arbitrage_bot::models::Market;
use polymarket_arbitrage_bot::plan3_tables::Plan3Tables;
use polymarket_arbitrage_bot::plan4b_tables::{Plan4bTables, SessionWatchKey, SessionWatchStart};
use polymarket_arbitrage_bot::plandaily_tables::PlanDailyTables;
use polymarket_arbitrage_bot::sessionband;
use polymarket_arbitrage_bot::strategy::Timeframe;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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
const DEFAULT_ENDGAME_BASE_OFFSETS_MS: &[u64] = &[3000, 1000, 100];
const DEFAULT_ENDGAME_OFFSET_JITTER_MS: i64 = 50;
const DEFAULT_ENDGAME_SUBMIT_PROXY_MAX_AGE_BASE_MS: i64 = 400;
const DEFAULT_ENDGAME_SUBMIT_PROXY_MAX_AGE_JITTER_MS: i64 = 100;

#[derive(Debug, Clone)]
struct Settings {
    bind: String,
    port: u16,
    token: Option<String>,
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
    endgame_offset_jitter_ms: i64,
    endgame_submit_proxy_max_age_base_ms: i64,
    endgame_submit_proxy_max_age_jitter_ms: i64,
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
struct PremarketAlphaShouldTradeRequest {
    strategy_id: String,
    decision_id: String,
    symbol: String,
    timeframe: String,
    market_open_ts: i64,
    proxy_wallet: String,
    ts_ms: i64,
    nonce: String,
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
struct MmRewardsSelectionRequest {
    enabled_modes: Vec<String>,
    selection_pool: usize,
    #[serde(default)]
    auto_force_top_reward: bool,
    #[serde(default)]
    auto_force_include_reward_min: f64,
    candidates: Vec<MmRewardsSelectionCandidateInput>,
}

#[derive(Debug, Deserialize)]
struct MmRewardsSelectionCandidateInput {
    condition_id: String,
    slug: String,
    reward_daily_rate: f64,
    #[serde(default)]
    reward_midpoint: Option<f64>,
    #[serde(default)]
    best_bid_up: Option<f64>,
    #[serde(default)]
    best_ask_up: Option<f64>,
    #[serde(default)]
    best_bid_down: Option<f64>,
    #[serde(default)]
    best_ask_down: Option<f64>,
    score: f64,
    expected_reward_day_est: f64,
    #[serde(default)]
    rv_short_bps: Option<f64>,
    #[serde(default)]
    competition_score_api: Option<f64>,
}

#[derive(Debug, Serialize)]
struct MmRewardsSelectionModeResponse {
    mode: String,
    condition_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct MmRewardsSelectionResponse {
    ok: bool,
    selected_by_mode: Vec<MmRewardsSelectionModeResponse>,
    force_include_by_mode: Vec<MmRewardsSelectionModeResponse>,
}

#[derive(Debug, Deserialize)]
struct MmRewardsPreflightRequest {
    side: String,
    #[serde(default)]
    scoring_guard_enable: bool,
    #[serde(default)]
    scoring_guard_require_qmin: bool,
    #[serde(default)]
    normal_full_ladder_required: bool,
    required_ladder_levels_market: usize,
    effective_ladder_levels: usize,
    feasible_levels_up_market: usize,
    feasible_levels_down_market: usize,
    feasible_pair_levels_market: usize,
    detail_min_size: f64,
    reward_min_size_unit: String,
    scoring_spread_extend_cents: f64,
    rungs: Vec<MmRewardsPreflightRungInput>,
}

#[derive(Debug, Deserialize)]
struct MmRewardsPreflightRungInput {
    #[serde(default)]
    scoring_required: bool,
    rung_bucket: String,
    submit_price: f64,
    counterpart_price: f64,
    counterpart_source: String,
    counterpart_size_shares: f64,
    side_size_shares: f64,
    #[serde(default)]
    scoring_midpoint: Option<f64>,
    scoring_max_spread: f64,
    up_quote_price: f64,
    down_quote_price: f64,
    up_quote_size_shares: f64,
    down_quote_size_shares: f64,
    up_quote_min_size_shares: f64,
    down_quote_min_size_shares: f64,
    #[serde(default)]
    up_anchor_price: Option<f64>,
    #[serde(default)]
    down_anchor_price: Option<f64>,
    #[serde(default)]
    opposite_blocked_by_skew: bool,
}

#[derive(Debug, Serialize)]
struct MmRewardsPreflightResponse {
    ok: bool,
    covered_levels: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    block_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    block_detail: Option<Value>,
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
        .route("/v1/discovery/timeframe", post(discovery_timeframe_handler))
        .route("/v1/discovery/evsnipe", post(discovery_evsnipe_handler))
        .route(
            "/v1/alpha/mm-rewards/selection",
            post(alpha_mm_rewards_selection_handler),
        )
        .route(
            "/v1/alpha/mm-rewards/preflight",
            post(alpha_mm_rewards_preflight_handler),
        )
        .route("/v1/alpha/evcurve", post(alpha_evcurve_handler))
        .route("/v1/alpha/sessionband", post(alpha_sessionband_handler))
        .route(
            "/v1/alpha/premarket/should-trade",
            post(alpha_premarket_should_trade_handler),
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
        "evpoly alpha service listening on {} with routes: /health, /v1/discovery/timeframe, /v1/discovery/evsnipe, /v1/alpha/mm-rewards/selection, /v1/alpha/mm-rewards/preflight, /v1/alpha/evcurve, /v1/alpha/sessionband, /v1/alpha/premarket/should-trade, /v1/alpha/endgame/policy",
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

    let (_updated_at_ms, mut specs) = state.discovery_cache.get_evsnipe_specs().await;
    if specs.is_empty() {
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

async fn alpha_mm_rewards_selection_handler(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let payload: MmRewardsSelectionRequest =
        parse_json_request(&state, remote.ip(), &headers, &body)?;

    let mut enabled_modes: Vec<MmMode> = Vec::new();
    for raw_mode in payload.enabled_modes.iter() {
        let mode = parse_mm_mode(raw_mode.as_str()).ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("invalid mm mode: {}", raw_mode),
            )
        })?;
        if !enabled_modes.contains(&mode) {
            enabled_modes.push(mode);
        }
    }
    if enabled_modes.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "enabled_modes must be non-empty",
        ));
    }
    if payload.candidates.is_empty() {
        return Ok(Json(MmRewardsSelectionResponse {
            ok: true,
            selected_by_mode: Vec::new(),
            force_include_by_mode: Vec::new(),
        }));
    }

    let ranked = payload
        .candidates
        .iter()
        .filter_map(mm_selection_candidate_to_auto_candidate)
        .collect::<Vec<_>>();
    if ranked.is_empty() {
        return Ok(Json(MmRewardsSelectionResponse {
            ok: true,
            selected_by_mode: Vec::new(),
            force_include_by_mode: Vec::new(),
        }));
    }

    let mut selected = mm::reward_scanner::select_top_candidates_by_reward(
        ranked.as_slice(),
        enabled_modes.as_slice(),
        payload.selection_pool.max(1),
    );
    if payload.auto_force_top_reward {
        let top_reward_candidate = ranked
            .iter()
            .max_by(|a, b| {
                a.reward_snapshot
                    .reward_daily_rate
                    .total_cmp(&b.reward_snapshot.reward_daily_rate)
            })
            .cloned();
        if let (Some(candidate), Some(first_mode)) = (top_reward_candidate, enabled_modes.first()) {
            let already_selected = selected.values().any(|rows| {
                rows.iter().any(|row| {
                    row.market
                        .condition_id
                        .eq_ignore_ascii_case(candidate.market.condition_id.as_str())
                })
            });
            if !already_selected {
                selected
                    .entry(*first_mode)
                    .or_default()
                    .insert(0, candidate);
            }
        }
    }

    let mut force_include: std::collections::HashMap<
        MmMode,
        Vec<mm::reward_scanner::AutoMarketCandidate>,
    > = std::collections::HashMap::new();
    if payload.auto_force_include_reward_min > 0.0 {
        let reward_p75 = mm::reward_scanner::reward_p75_estimate(ranked.as_slice());
        let mut already_selected_conditions = selected
            .values()
            .flat_map(|rows| {
                rows.iter()
                    .map(|row| row.market.condition_id.to_ascii_lowercase())
            })
            .collect::<HashSet<_>>();
        for candidate in ranked.iter().cloned() {
            if candidate.reward_snapshot.reward_daily_rate < payload.auto_force_include_reward_min {
                continue;
            }
            let condition_key = candidate.market.condition_id.to_ascii_lowercase();
            if already_selected_conditions.contains(condition_key.as_str()) {
                continue;
            }
            if let Some(mode) = mm::reward_scanner::preferred_mode_for_candidate(
                &candidate,
                enabled_modes.as_slice(),
                reward_p75,
            ) {
                force_include.entry(mode).or_default().push(candidate);
                already_selected_conditions.insert(condition_key);
            }
        }
    }

    let selected_by_mode = enabled_modes
        .iter()
        .filter_map(|mode| {
            let ids = selected
                .remove(mode)
                .unwrap_or_default()
                .into_iter()
                .map(|candidate| candidate.market.condition_id)
                .collect::<Vec<_>>();
            (!ids.is_empty()).then_some(MmRewardsSelectionModeResponse {
                mode: mode.as_str().to_string(),
                condition_ids: ids,
            })
        })
        .collect::<Vec<_>>();
    let force_include_by_mode = enabled_modes
        .iter()
        .filter_map(|mode| {
            let ids = force_include
                .remove(mode)
                .unwrap_or_default()
                .into_iter()
                .map(|candidate| candidate.market.condition_id)
                .collect::<Vec<_>>();
            (!ids.is_empty()).then_some(MmRewardsSelectionModeResponse {
                mode: mode.as_str().to_string(),
                condition_ids: ids,
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(MmRewardsSelectionResponse {
        ok: true,
        selected_by_mode,
        force_include_by_mode,
    }))
}

async fn alpha_mm_rewards_preflight_handler(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let payload: MmRewardsPreflightRequest =
        parse_json_request(&state, remote.ip(), &headers, &body)?;

    let side_up = match payload.side.trim().to_ascii_lowercase().as_str() {
        "up" | "yes" => true,
        "down" | "no" => false,
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "side must be up/down",
            ));
        }
    };

    let mut covered_levels = 0usize;
    let mut block_reason: Option<String> = None;
    let mut block_detail: Option<Value> = None;

    for rung in payload.rungs.iter() {
        let scoring_enabled = payload.scoring_guard_enable && rung.scoring_required;
        if scoring_enabled {
            let score_check = mm::scoring_guard::check_local_scoring(
                rung.scoring_midpoint,
                rung.scoring_max_spread,
                rung.up_quote_min_size_shares,
                rung.down_quote_min_size_shares,
                rung.up_quote_price,
                rung.down_quote_price,
                rung.up_quote_size_shares,
                rung.down_quote_size_shares,
                rung.up_anchor_price,
                rung.down_anchor_price,
            );
            let side_scoring_ok = if side_up {
                score_check.up_scoring
            } else {
                score_check.down_scoring
            };
            let opposite_side_scoring_ok = if side_up {
                score_check.down_scoring
            } else {
                score_check.up_scoring
            };
            let qmin_bypass_reason =
                if payload.scoring_guard_require_qmin && rung.opposite_blocked_by_skew {
                    Some("opposite_blocked_by_skew")
                } else if payload.scoring_guard_require_qmin
                    && side_scoring_ok
                    && !score_check.q_min_ready
                    && (!opposite_side_scoring_ok
                        || rung.counterpart_source.eq_ignore_ascii_case("none")
                        || rung.counterpart_size_shares <= 0.0
                        || payload.feasible_pair_levels_market == 0)
                {
                    Some("one_sided_quoteable")
                } else {
                    None
                };
            let qmin_ok = if payload.scoring_guard_require_qmin {
                qmin_bypass_reason.is_some() || score_check.q_min_ready
            } else {
                true
            };
            if !side_scoring_ok || !qmin_ok {
                let mut scoring_fail_reasons: Vec<&'static str> = Vec::new();
                if !score_check.midpoint_valid {
                    scoring_fail_reasons.push("midpoint_invalid");
                }
                if !score_check.up_spread_ok {
                    scoring_fail_reasons.push("up_spread_fail");
                }
                if !score_check.down_spread_ok {
                    scoring_fail_reasons.push("down_spread_fail");
                }
                if !score_check.up_size_ok {
                    scoring_fail_reasons.push("up_size_fail");
                }
                if !score_check.down_size_ok {
                    scoring_fail_reasons.push("down_size_fail");
                }
                block_reason = Some("non_scoring".to_string());
                block_detail = Some(json!({
                    "submit_price": rung.submit_price,
                    "counterpart_price": rung.counterpart_price,
                    "counterpart_source": rung.counterpart_source,
                    "counterpart_size_shares": rung.counterpart_size_shares,
                    "size_shares": rung.side_size_shares,
                    "rung_bucket": rung.rung_bucket,
                    "midpoint": rung.scoring_midpoint,
                    "max_spread": rung.scoring_max_spread,
                    "max_spread_effective": rung.scoring_max_spread,
                    "scoring_spread_extend_cents": payload.scoring_spread_extend_cents,
                    "min_size": payload.detail_min_size,
                    "reward_min_size_unit": payload.reward_min_size_unit,
                    "up_min_size_shares": score_check.up_min_size_shares,
                    "down_min_size_shares": score_check.down_min_size_shares,
                    "spread_cap_effective": score_check.spread_cap,
                    "midpoint_valid": score_check.midpoint_valid,
                    "up_spread_ok": score_check.up_spread_ok,
                    "down_spread_ok": score_check.down_spread_ok,
                    "up_size_ok": score_check.up_size_ok,
                    "down_size_ok": score_check.down_size_ok,
                    "up_scoring": score_check.up_scoring,
                    "down_scoring": score_check.down_scoring,
                    "q_min_ready": score_check.q_min_ready,
                    "scoring_fail_reasons": scoring_fail_reasons,
                    "scoring_guard_require_qmin": payload.scoring_guard_require_qmin,
                    "qmin_bypass_reason": qmin_bypass_reason
                }));
                break;
            }
        }
        covered_levels = covered_levels.saturating_add(1);
    }

    if block_reason.is_none()
        && payload.normal_full_ladder_required
        && covered_levels < payload.required_ladder_levels_market
    {
        block_reason = Some("full_ladder_required".to_string());
        block_detail = Some(json!({
            "covered_levels": covered_levels,
            "required_levels": payload.required_ladder_levels_market,
            "effective_ladder_levels": payload.effective_ladder_levels,
            "normal_full_ladder_required": payload.normal_full_ladder_required,
            "feasible_levels_up_market": payload.feasible_levels_up_market,
            "feasible_levels_down_market": payload.feasible_levels_down_market,
            "feasible_pair_levels_market": payload.feasible_pair_levels_market
        }));
    }

    Ok(Json(MmRewardsPreflightResponse {
        ok: true,
        covered_levels,
        block_reason,
        block_detail,
    }))
}

async fn alpha_evcurve_handler(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let payload: EvcurveRequest = parse_json_request(&state, remote.ip(), &headers, &body)?;

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

async fn alpha_endgame_policy_handler(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let payload: EndgameAlphaPolicyRequest =
        parse_json_request(&state, remote.ip(), &headers, &body)?;
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

    let mut tick_offsets_ms = state
        .settings
        .endgame_base_offsets_ms
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
            let base_i64 = i64::try_from(*base).ok()?;
            let jittered = seeded_jitter_ms(
                base_i64,
                state.settings.endgame_offset_jitter_ms,
                seed.as_str(),
            )
            .clamp(50, 120_000);
            u64::try_from(jittered).ok()
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
        reason: "randomized_policy".to_string(),
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
        if provided != expected_header {
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

fn parse_mm_mode(value: &str) -> Option<MmMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "quiet_stack" => Some(MmMode::QuietStack),
        "tail_biased" => Some(MmMode::TailBiased),
        "spike" => Some(MmMode::Spike),
        "sports_pregame" => Some(MmMode::SportsPregame),
        _ => None,
    }
}

fn mm_selection_candidate_to_auto_candidate(
    input: &MmRewardsSelectionCandidateInput,
) -> Option<mm::reward_scanner::AutoMarketCandidate> {
    let condition_id = input.condition_id.trim().to_string();
    let slug = input.slug.trim().to_string();
    if condition_id.is_empty() || slug.is_empty() {
        return None;
    }
    Some(mm::reward_scanner::AutoMarketCandidate {
        market: Market {
            condition_id,
            market_id: None,
            question: slug.clone(),
            slug,
            description: None,
            resolution_source: None,
            end_date: None,
            end_date_iso: None,
            end_date_iso_alt: None,
            game_start_time: None,
            start_date: None,
            active: true,
            closed: false,
            tokens: None,
            clob_token_ids: None,
            outcomes: None,
            competitive: None,
        },
        symbol: "MM".to_string(),
        timeframe: Timeframe::D1,
        timeframe_inferred: false,
        reward_snapshot: mm::reward_scanner::MarketRewardSnapshot {
            reward_daily_rate: input.reward_daily_rate.max(0.0),
            midpoint: input.reward_midpoint,
            max_spread: 0.0,
            min_size: 0.0,
        },
        best_bid_up: input.best_bid_up,
        best_ask_up: input.best_ask_up,
        best_bid_down: input.best_bid_down,
        best_ask_down: input.best_ask_down,
        tick_size_est: 0.01,
        feasible_levels_up_est: 0,
        feasible_levels_down_est: 0,
        feasible_pair_levels_est: 0,
        avg_spread: None,
        rv_short_bps: input.rv_short_bps,
        rv_mid_bps: None,
        rv_long_bps: None,
        inventory_risk: 0.0,
        competition_raw_api: None,
        competition_score_api: input.competition_score_api,
        competition_level_api: "unknown".to_string(),
        competition_source_api: "remote_payload".to_string(),
        competition_qmin_est: 0.0,
        our_qmin_est: 0.0,
        our_share_est: 0.0,
        expected_reward_day_est: input.expected_reward_day_est,
        reward_efficiency_est: 0.0,
        capital_locked_est: 0.0,
        score: input.score,
    })
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

    let mut endgame_base_offsets_ms = std::env::var("ALPHA_ENDGAME_BASE_OFFSETS_MS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .filter_map(|part| part.trim().parse::<u64>().ok())
                .filter(|value| *value > 0)
                .collect::<Vec<_>>()
        })
        .filter(|values| !values.is_empty())
        .unwrap_or_else(|| DEFAULT_ENDGAME_BASE_OFFSETS_MS.to_vec());
    endgame_base_offsets_ms.sort_by(|a, b| b.cmp(a));
    endgame_base_offsets_ms.dedup();

    let endgame_offset_jitter_ms = std::env::var("ALPHA_ENDGAME_OFFSET_JITTER_MS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT_ENDGAME_OFFSET_JITTER_MS)
        .clamp(0, 1_000);
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
        endgame_offset_jitter_ms,
        endgame_submit_proxy_max_age_base_ms,
        endgame_submit_proxy_max_age_jitter_ms,
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
