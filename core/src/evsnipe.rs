use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use log::warn;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_tungstenite::connect_async;

use crate::api::PolymarketApi;
use crate::event_log::log_event;
use crate::models::{Market, OrderBook};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvsnipeRule {
    #[serde(rename = "hit_up_high_gte")]
    HitUpHighGte,
    #[serde(rename = "hit_down_low_lte")]
    HitDownLowLte,
    #[serde(rename = "close_above")]
    CloseAbove,
    #[serde(rename = "close_below")]
    CloseBelow,
    #[serde(rename = "close_between")]
    CloseBetween,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvsnipeMarketSpec {
    pub condition_id: String,
    pub slug: String,
    pub question: String,
    pub symbol: String,
    pub strike_price: f64,
    pub strike_price_upper: Option<f64>,
    pub rule: EvsnipeRule,
    pub yes_token_id: String,
    pub no_token_id: Option<String>,
    pub end_ts: Option<i64>,
}

impl EvsnipeMarketSpec {
    pub fn reference_price(&self) -> f64 {
        match (self.rule, self.strike_price_upper) {
            (EvsnipeRule::CloseBetween, Some(upper)) => (self.strike_price + upper) / 2.0,
            _ => self.strike_price,
        }
    }

    pub fn rule_label(&self) -> &'static str {
        match self.rule {
            EvsnipeRule::HitUpHighGte => "high_gte",
            EvsnipeRule::HitDownLowLte => "low_lte",
            EvsnipeRule::CloseAbove => "close_above",
            EvsnipeRule::CloseBelow => "close_below",
            EvsnipeRule::CloseBetween => "close_between",
        }
    }

    pub fn is_hit_rule(&self) -> bool {
        matches!(
            self.rule,
            EvsnipeRule::HitUpHighGte | EvsnipeRule::HitDownLowLte
        )
    }

    pub fn is_close_rule(&self) -> bool {
        matches!(
            self.rule,
            EvsnipeRule::CloseAbove | EvsnipeRule::CloseBelow | EvsnipeRule::CloseBetween
        )
    }

    pub fn hit_by_trade(&self, price: f64) -> bool {
        if !price.is_finite() || price <= 0.0 {
            return false;
        }
        match self.rule {
            EvsnipeRule::HitUpHighGte => price >= self.strike_price,
            EvsnipeRule::HitDownLowLte => price <= self.strike_price,
            _ => false,
        }
    }

    pub fn crossed_on_trade(&self, prev_price: f64, price: f64) -> bool {
        if !prev_price.is_finite() || prev_price <= 0.0 || !price.is_finite() || price <= 0.0 {
            return false;
        }
        match self.rule {
            EvsnipeRule::HitUpHighGte => {
                prev_price < self.strike_price && price >= self.strike_price
            }
            EvsnipeRule::HitDownLowLte => {
                prev_price > self.strike_price && price <= self.strike_price
            }
            _ => false,
        }
    }

    pub fn in_pre_hit_window(&self, price: f64, pre_trigger_bps: f64) -> bool {
        if !price.is_finite() || price <= 0.0 {
            return false;
        }
        let bps = pre_trigger_bps.max(0.0);
        let band = bps / 10_000.0;
        match self.rule {
            EvsnipeRule::HitUpHighGte => {
                let lower = self.strike_price * (1.0 - band);
                price >= lower && price < self.strike_price
            }
            EvsnipeRule::HitDownLowLte => {
                let upper = self.strike_price * (1.0 + band);
                price <= upper && price > self.strike_price
            }
            _ => false,
        }
    }

    pub fn entered_pre_hit_window(
        &self,
        prev_price: f64,
        price: f64,
        pre_trigger_bps: f64,
    ) -> bool {
        !self.in_pre_hit_window(prev_price, pre_trigger_bps)
            && self.in_pre_hit_window(price, pre_trigger_bps)
    }

    pub fn close_condition_yes(&self, close_price: f64) -> Option<bool> {
        if !close_price.is_finite() || close_price <= 0.0 {
            return None;
        }
        let eps = if self.symbol == "XRP" { 0.0001 } else { 0.01 };
        match self.rule {
            EvsnipeRule::CloseAbove => {
                if (close_price - self.strike_price).abs() <= eps {
                    None
                } else {
                    Some(close_price > self.strike_price)
                }
            }
            EvsnipeRule::CloseBelow => {
                if (close_price - self.strike_price).abs() <= eps {
                    None
                } else {
                    Some(close_price < self.strike_price)
                }
            }
            EvsnipeRule::CloseBetween => {
                let upper = self.strike_price_upper?;
                // Treat "between a and b" as [a, b) and skip boundary ambiguity.
                if (close_price - self.strike_price).abs() <= eps
                    || (close_price - upper).abs() <= eps
                {
                    None
                } else {
                    Some(close_price >= self.strike_price && close_price < upper)
                }
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvsnipeConfig {
    pub enable: bool,
    pub symbols: Vec<String>,
    pub discovery_refresh_sec: u64,
    pub discovery_limit: u32,
    pub max_days_to_expiry: u64,
    pub strike_window_pct: f64,
    pub anchor_refresh_sec: u64,
    pub anchor_drift_refresh_pct: f64,
    pub size_usd: f64,
    pub pre_trigger_bps: f64,
    pub pre_leg_ratio: f64,
    pub max_buy_price: f64,
    pub cross_ask_levels: usize,
    pub binance_stale_ms: i64,
    pub strategy_cap_usd: f64,
    pub max_inflight_tasks: usize,
}

impl EvsnipeConfig {
    pub fn from_env() -> Self {
        Self {
            enable: env_bool("EVPOLY_STRATEGY_EVSNIPE_ENABLE", true),
            symbols: parse_symbols_env(
                "EVPOLY_EVSNIPE_SYMBOLS",
                &["BTC", "ETH", "SOL", "XRP", "DOGE", "BNB", "HYPE"],
            ),
            discovery_refresh_sec: env_u64("EVPOLY_EVSNIPE_DISCOVERY_REFRESH_SEC", 30).max(5),
            discovery_limit: env_u32("EVPOLY_EVSNIPE_DISCOVERY_LIMIT", 2_500).max(500),
            max_days_to_expiry: env_u64("EVPOLY_EVSNIPE_MAX_DAYS_TO_EXPIRY", 30)
                .max(1)
                .min(365),
            strike_window_pct: env_f64("EVPOLY_EVSNIPE_STRIKE_WINDOW_PCT", 0.10).clamp(0.0, 1.0),
            anchor_refresh_sec: env_u64("EVPOLY_EVSNIPE_ANCHOR_REFRESH_SEC", 14_400).max(60),
            anchor_drift_refresh_pct: env_f64("EVPOLY_EVSNIPE_ANCHOR_DRIFT_REFRESH_PCT", 0.03)
                .clamp(0.0, 1.0),
            size_usd: env_f64("EVPOLY_EVSNIPE_SIZE_USD", 10.0)
                .max(1.0)
                .min(50_000.0),
            pre_trigger_bps: env_f64("EVPOLY_EVSNIPE_PRE_TRIGGER_BPS", 1.0).clamp(0.0, 100.0),
            pre_leg_ratio: env_f64("EVPOLY_EVSNIPE_PRE_LEG_RATIO", 0.30).clamp(0.0, 1.0),
            // Keep EVSnipe max buy fixed for all market families (hit + close/range).
            max_buy_price: 0.99,
            cross_ask_levels: env_usize("EVPOLY_EVSNIPE_CROSS_ASK_LEVELS", 3)
                .max(1)
                .min(8),
            binance_stale_ms: env_i64("EVPOLY_EVSNIPE_BINANCE_STALE_MS", 1_200).max(200),
            strategy_cap_usd: env_f64("EVPOLY_EVSNIPE_STRATEGY_CAP_USD", 10_000.0).max(1.0),
            max_inflight_tasks: env_usize("EVPOLY_EVSNIPE_MAX_INFLIGHT_TASKS", 16).max(1),
        }
    }
}

#[derive(Debug, Clone)]
struct EvsnipeRemoteDiscoveryConfig {
    url: String,
    token: Option<String>,
    timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
struct EvsnipeRemoteDiscoveryRequest {
    symbols: Vec<String>,
    discovery_limit: u32,
    max_days_to_expiry: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum EvsnipeRemoteDiscoveryResponse {
    Wrapped { specs: Vec<EvsnipeMarketSpec> },
    Flat(Vec<EvsnipeMarketSpec>),
}

const REMOTE_ALPHA_PRIMARY_HOST: &str = "alpha.evplus.ai";
const REMOTE_ALPHA_FALLBACK_HOST: &str = "alpha2.evplus.ai";
const REMOTE_EVSNIPE_DISCOVERY_URL_DEFAULT: &str = "https://alpha.evplus.ai/v1/discovery/evsnipe";
const EVSNIPE_DISCOVERY_TAG_SLUG: &str = "crypto";

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn truncate_for_log(value: &str, max_chars: usize) -> String {
    let mut iter = value.chars();
    let truncated: String = iter.by_ref().take(max_chars).collect();
    if iter.next().is_some() {
        format!("{}...", truncated)
    } else {
        truncated
    }
}

fn remote_alpha_failover_url(url: &str) -> Option<String> {
    let mut parsed = reqwest::Url::parse(url).ok()?;
    if !parsed
        .host_str()
        .map(|host| host.eq_ignore_ascii_case(REMOTE_ALPHA_PRIMARY_HOST))
        .unwrap_or(false)
    {
        return None;
    }
    parsed.set_host(Some(REMOTE_ALPHA_FALLBACK_HOST)).ok()?;
    Some(parsed.to_string())
}

fn remote_alpha_should_failover_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn should_failover_remote_alpha_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        if let Some(req_err) = cause.downcast_ref::<reqwest::Error>() {
            return req_err.is_timeout() || req_err.is_connect() || req_err.is_request();
        }
        if let Some(io_err) = cause.downcast_ref::<std::io::Error>() {
            return matches!(
                io_err.kind(),
                std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::BrokenPipe
            );
        }
        false
    })
}

async fn send_remote_json_post_once<T: Serialize + ?Sized>(
    client: &reqwest::Client,
    url: &str,
    timeout_ms: u64,
    token: Option<&str>,
    payload: &T,
) -> Result<(reqwest::StatusCode, String)> {
    let mut request = client
        .post(url)
        .timeout(Duration::from_millis(timeout_ms))
        .json(payload);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .with_context(|| format!("failed to send remote request to {}", url))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .with_context(|| format!("failed to read remote response body from {}", url))?;
    Ok((status, body))
}

async fn send_remote_json_post_with_alpha_failover<T: Serialize + ?Sized>(
    client: &reqwest::Client,
    url: &str,
    timeout_ms: u64,
    token: Option<&str>,
    payload: &T,
    route_label: &str,
) -> Result<(reqwest::StatusCode, String)> {
    let fallback_url = remote_alpha_failover_url(url);
    let primary_outcome = send_remote_json_post_once(client, url, timeout_ms, token, payload).await;
    match primary_outcome {
        Ok((status, body)) => {
            if remote_alpha_should_failover_status(status) {
                if let Some(fallback) = fallback_url.as_deref() {
                    warn!(
                        "Remote {} primary returned status={} url={} -> retrying fallback={}",
                        route_label,
                        status.as_u16(),
                        url,
                        fallback
                    );
                    return send_remote_json_post_once(
                        client, fallback, timeout_ms, token, payload,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "remote {} fallback request failed after primary status={}",
                            route_label,
                            status.as_u16()
                        )
                    });
                }
            }
            Ok((status, body))
        }
        Err(primary_err) => {
            if let Some(fallback) = fallback_url.as_deref() {
                if should_failover_remote_alpha_error(&primary_err) {
                    warn!(
                        "Remote {} primary transport failed url={} -> retrying fallback={} err={}",
                        route_label, url, fallback, primary_err
                    );
                    return send_remote_json_post_once(
                        client, fallback, timeout_ms, token, payload,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "remote {} fallback request failed after primary transport error: {}",
                            route_label, primary_err
                        )
                    });
                }
            }
            Err(primary_err)
        }
    }
}

fn evsnipe_remote_discovery_config() -> Option<&'static EvsnipeRemoteDiscoveryConfig> {
    static CONFIG: OnceLock<Option<EvsnipeRemoteDiscoveryConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let url = env_nonempty("EVPOLY_REMOTE_EVSNIPE_DISCOVERY_URL")
                .unwrap_or_else(|| REMOTE_EVSNIPE_DISCOVERY_URL_DEFAULT.to_string());
            Some(EvsnipeRemoteDiscoveryConfig {
                url,
                token: env_nonempty("EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TOKEN"),
                timeout_ms: 2_000_u64,
            })
        })
        .as_ref()
}

fn normalize_discovered_specs(
    specs: Vec<EvsnipeMarketSpec>,
    cfg: &EvsnipeConfig,
    now_sec: i64,
    max_end_ts: i64,
) -> Vec<EvsnipeMarketSpec> {
    let allowed: HashSet<String> = cfg.symbols.iter().map(|s| normalize_symbol(s)).collect();
    let mut out = Vec::new();
    let mut dedupe = HashSet::new();
    for mut spec in specs {
        spec.symbol = normalize_symbol(spec.symbol.as_str());
        if let Some(end_ts) = spec.end_ts {
            if end_ts <= now_sec || end_ts > max_end_ts {
                continue;
            }
        } else {
            continue;
        }
        if !allowed.is_empty() && !allowed.contains(spec.symbol.as_str()) {
            continue;
        }
        if spec.condition_id.trim().is_empty()
            || spec.slug.trim().is_empty()
            || spec.yes_token_id.trim().is_empty()
        {
            continue;
        }
        if dedupe.insert(spec.condition_id.clone()) {
            out.push(spec);
        }
    }
    out.sort_by(|a, b| {
        a.symbol
            .cmp(&b.symbol)
            .then_with(|| a.strike_price.total_cmp(&b.strike_price))
    });
    out
}

async fn refresh_hit_market_specs_remote(
    cfg: &EvsnipeConfig,
    now_sec: i64,
    max_end_ts: i64,
) -> Result<Option<Vec<EvsnipeMarketSpec>>> {
    let Some(remote_cfg) = evsnipe_remote_discovery_config() else {
        return Ok(None);
    };
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    let client = CLIENT.get_or_init(reqwest::Client::new);
    let payload = EvsnipeRemoteDiscoveryRequest {
        symbols: cfg.symbols.clone(),
        discovery_limit: cfg.discovery_limit,
        max_days_to_expiry: cfg.max_days_to_expiry,
    };
    let (status, body) = send_remote_json_post_with_alpha_failover(
        client,
        remote_cfg.url.as_str(),
        remote_cfg.timeout_ms,
        remote_cfg.token.as_deref(),
        &payload,
        "evsnipe_discovery",
    )
    .await
    .context("failed to call remote EVSnipe discovery service")?;
    if status == reqwest::StatusCode::NO_CONTENT || status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        anyhow::bail!(
            "remote EVSnipe discovery rejected request (status={} body={})",
            status.as_u16(),
            truncate_for_log(body.as_str(), 300)
        );
    }
    let parsed: EvsnipeRemoteDiscoveryResponse =
        serde_json::from_str(body.as_str()).with_context(|| {
            format!(
                "failed to parse remote EVSnipe discovery response: {}",
                truncate_for_log(body.as_str(), 300)
            )
        })?;
    let raw_specs = match parsed {
        EvsnipeRemoteDiscoveryResponse::Wrapped { specs } => specs,
        EvsnipeRemoteDiscoveryResponse::Flat(specs) => specs,
    };
    Ok(Some(normalize_discovered_specs(
        raw_specs, cfg, now_sec, max_end_ts,
    )))
}

#[derive(Debug, Clone)]
pub struct BinanceTradeTick {
    pub symbol: String,
    pub price: f64,
    pub trade_ts_ms: i64,
    pub recv_ts_ms: i64,
}

#[derive(Debug, Clone)]
pub struct BinanceKlineCloseTick {
    pub symbol: String,
    pub close_price: f64,
    pub close_ts_ms: i64,
    pub recv_ts_ms: i64,
}

#[derive(Debug, Clone)]
pub struct AskSelection {
    pub best_ask: f64,
    pub selected_ask: f64,
    pub selected_level: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EvsnipeSpotAnchor {
    pub price: f64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvsnipeAnchorRefreshReason {
    Bootstrap,
    Periodic,
    Drift,
}

#[derive(Debug, Deserialize)]
struct BinanceTradeMsg {
    #[serde(rename = "p")]
    price: String,
    #[serde(rename = "T")]
    trade_time_ms: i64,
}

#[derive(Debug, Deserialize)]
struct BinanceKlineMsg {
    #[serde(rename = "k")]
    kline: BinanceKlineDataMsg,
}

#[derive(Debug, Deserialize)]
struct BinanceKlineDataMsg {
    #[serde(rename = "c")]
    close_price: String,
    #[serde(rename = "T")]
    close_ts_ms: i64,
    #[serde(rename = "x")]
    is_closed: bool,
}

#[derive(Debug, Deserialize)]
struct BinanceTickerPriceMsg {
    price: String,
}

pub fn parse_symbols_env(key: &str, default: &[&str]) -> Vec<String> {
    let mut symbols = std::env::var(key)
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(normalize_symbol)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if symbols.is_empty() {
        symbols = default.iter().map(|v| normalize_symbol(v)).collect();
    }
    let mut deduped = Vec::new();
    for symbol in symbols {
        if !deduped.iter().any(|v| v == &symbol) {
            deduped.push(symbol);
        }
    }
    deduped
}

pub fn normalize_symbol(symbol: &str) -> String {
    match symbol.trim().to_ascii_uppercase().as_str() {
        "SOLANA" => "SOL".to_string(),
        "DOGECOIN" => "DOGE".to_string(),
        "HYPERLIQUID" => "HYPE".to_string(),
        other => other.to_string(),
    }
}

fn binance_ticker_price_url(symbol: &str) -> &'static str {
    match symbol {
        // HYPE is quoted on Binance USD-M futures (not spot ticker endpoint).
        "HYPE" => "https://fapi.binance.com/fapi/v1/ticker/price",
        _ => "https://api.binance.com/api/v3/ticker/price",
    }
}

fn binance_trade_stream_url(symbol: &str) -> String {
    let pair = format!("{}usdt@trade", symbol.to_ascii_lowercase());
    match symbol {
        "HYPE" => format!("wss://fstream.binance.com/ws/{}", pair),
        _ => format!("wss://stream.binance.com:9443/ws/{}", pair),
    }
}

fn binance_kline_close_stream_url(symbol: &str) -> String {
    let pair = format!("{}usdt@kline_1m", symbol.to_ascii_lowercase());
    match symbol {
        "HYPE" => format!("wss://fstream.binance.com/ws/{}", pair),
        _ => format!("wss://stream.binance.com:9443/ws/{}", pair),
    }
}

pub fn select_cross_ask_price(
    orderbook: &OrderBook,
    cross_ask_levels: usize,
) -> Option<AskSelection> {
    let mut asks = orderbook
        .asks
        .iter()
        .filter_map(|level| f64::try_from(level.price).ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .collect::<Vec<_>>();
    asks.sort_by(|a, b| a.total_cmp(b));
    asks.dedup_by(|a, b| (*a - *b).abs() <= f64::EPSILON);
    let best_ask = asks.first().copied()?;
    let max_level = cross_ask_levels.min(asks.len()).max(1);
    let idx = max_level.saturating_sub(1);
    let ask = asks[idx];
    Some(AskSelection {
        best_ask,
        selected_ask: ask,
        selected_level: idx + 1,
    })
}

pub async fn refresh_hit_market_specs(
    api: &PolymarketApi,
    cfg: &EvsnipeConfig,
) -> Result<Vec<EvsnipeMarketSpec>> {
    let now_sec = chrono::Utc::now().timestamp();
    let max_end_ts = now_sec.saturating_add(
        i64::try_from(cfg.max_days_to_expiry)
            .ok()
            .unwrap_or(30)
            .saturating_mul(86_400),
    );
    match refresh_hit_market_specs_remote(cfg, now_sec, max_end_ts).await {
        Ok(Some(specs)) if !specs.is_empty() => {
            log_event(
                "evsnipe_remote_discovery_hit",
                json!({
                    "symbols": cfg.symbols,
                    "spec_count": specs.len()
                }),
            );
            Ok(specs)
        }
        Ok(Some(_)) => {
            warn!("EVSnipe remote discovery returned empty spec list; falling back to local discovery");
            log_event(
                "evsnipe_discovery_remote_empty_fallback_local",
                json!({
                    "symbols": cfg.symbols,
                }),
            );
            refresh_hit_market_specs_local(api, cfg, now_sec, max_end_ts).await
        }
        Ok(None) => {
            warn!("EVSnipe remote discovery unavailable; falling back to local discovery");
            log_event(
                "evsnipe_discovery_remote_unavailable_fallback_local",
                json!({
                    "symbols": cfg.symbols,
                }),
            );
            refresh_hit_market_specs_local(api, cfg, now_sec, max_end_ts).await
        }
        Err(remote_err) => {
            warn!(
                "EVSnipe remote discovery failed (fallback to local): {}",
                remote_err
            );
            log_event(
                "evsnipe_discovery_remote_failed_fallback_local",
                json!({
                    "symbols": cfg.symbols,
                    "error": remote_err.to_string()
                }),
            );
            refresh_hit_market_specs_local(api, cfg, now_sec, max_end_ts).await
        }
    }
}

pub async fn refresh_hit_market_specs_local_only(
    api: &PolymarketApi,
    cfg: &EvsnipeConfig,
) -> Result<Vec<EvsnipeMarketSpec>> {
    let now_sec = chrono::Utc::now().timestamp();
    let max_end_ts = now_sec.saturating_add(
        i64::try_from(cfg.max_days_to_expiry)
            .ok()
            .unwrap_or(30)
            .saturating_mul(86_400),
    );
    refresh_hit_market_specs_local(api, cfg, now_sec, max_end_ts).await
}

pub fn by_symbol(specs: &[EvsnipeMarketSpec]) -> HashMap<String, Vec<EvsnipeMarketSpec>> {
    let mut out: HashMap<String, Vec<EvsnipeMarketSpec>> = HashMap::new();
    for spec in specs {
        out.entry(spec.symbol.clone())
            .or_default()
            .push(spec.clone());
    }
    for items in out.values_mut() {
        items.sort_by(|a, b| a.strike_price.total_cmp(&b.strike_price));
    }
    out
}

pub async fn fetch_binance_spot_prices(symbols: &[String]) -> Result<HashMap<String, f64>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("build binance spot client")?;
    let mut out = HashMap::new();
    for symbol in symbols {
        let symbol_norm = normalize_symbol(symbol);
        if symbol_norm.is_empty() {
            continue;
        }
        let pair = format!("{}USDT", symbol_norm);
        let url = binance_ticker_price_url(symbol_norm.as_str());
        let response = match client
            .get(url)
            .query(&[("symbol", pair.as_str())])
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(err) => {
                warn!(
                    "EVSnipe ticker fetch failed for {} via {}: {}",
                    pair, url, err
                );
                continue;
            }
        };
        let status = response.status();
        let body = match response.text().await {
            Ok(text) => text,
            Err(err) => {
                warn!(
                    "EVSnipe ticker read failed for {} via {}: {}",
                    pair, url, err
                );
                continue;
            }
        };
        if !status.is_success() {
            warn!(
                "EVSnipe ticker status {} for {} via {}: {}",
                status, pair, url, body
            );
            continue;
        }
        let message: BinanceTickerPriceMsg = match serde_json::from_str(&body) {
            Ok(parsed) => parsed,
            Err(err) => {
                warn!(
                    "EVSnipe ticker parse failed for {} via {}: {} (body={})",
                    pair, url, err, body
                );
                continue;
            }
        };
        let price = match message.price.trim().parse::<f64>() {
            Ok(parsed) => parsed,
            Err(err) => {
                warn!(
                    "EVSnipe ticker invalid price for {} via {}: {} (raw={})",
                    pair, url, err, message.price
                );
                continue;
            }
        };
        if price.is_finite() && price > 0.0 {
            out.insert(symbol_norm, price);
        }
    }
    Ok(out)
}

pub fn next_spot_anchor(
    existing: Option<EvsnipeSpotAnchor>,
    current_spot: f64,
    now_ms: i64,
    refresh_sec: u64,
    drift_refresh_pct: f64,
) -> Option<(EvsnipeSpotAnchor, Option<EvsnipeAnchorRefreshReason>)> {
    if !current_spot.is_finite() || current_spot <= 0.0 || now_ms <= 0 {
        return existing.map(|anchor| (anchor, None));
    }
    let refresh_ms = i64::try_from(refresh_sec)
        .ok()
        .and_then(|v| v.checked_mul(1_000))
        .unwrap_or(i64::MAX);
    match existing {
        None => Some((
            EvsnipeSpotAnchor {
                price: current_spot,
                updated_at_ms: now_ms,
            },
            Some(EvsnipeAnchorRefreshReason::Bootstrap),
        )),
        Some(anchor) => {
            let elapsed_ms = now_ms.saturating_sub(anchor.updated_at_ms);
            if elapsed_ms >= refresh_ms {
                return Some((
                    EvsnipeSpotAnchor {
                        price: current_spot,
                        updated_at_ms: now_ms,
                    },
                    Some(EvsnipeAnchorRefreshReason::Periodic),
                ));
            }
            let drift = strike_distance_pct(anchor.price, current_spot)?;
            if drift >= drift_refresh_pct.max(0.0) {
                return Some((
                    EvsnipeSpotAnchor {
                        price: current_spot,
                        updated_at_ms: now_ms,
                    },
                    Some(EvsnipeAnchorRefreshReason::Drift),
                ));
            }
            Some((anchor, None))
        }
    }
}

pub fn filter_specs_by_spot_anchor(
    specs: &[EvsnipeMarketSpec],
    anchors: &HashMap<String, EvsnipeSpotAnchor>,
    strike_window_pct: f64,
) -> Vec<EvsnipeMarketSpec> {
    let strike_window_pct = strike_window_pct.max(0.0);
    if strike_window_pct >= 1.0 {
        return specs.to_vec();
    }
    specs
        .iter()
        .filter(|spec| {
            let Some(anchor) = anchors.get(spec.symbol.as_str()) else {
                return false;
            };
            let Some(distance_pct) = strike_distance_pct(anchor.price, spec.reference_price())
            else {
                return false;
            };
            distance_pct <= strike_window_pct
        })
        .cloned()
        .collect()
}

fn strike_distance_pct(anchor_price: f64, strike_price: f64) -> Option<f64> {
    if !anchor_price.is_finite()
        || !strike_price.is_finite()
        || anchor_price <= 0.0
        || strike_price <= 0.0
    {
        return None;
    }
    Some((strike_price - anchor_price).abs() / anchor_price)
}

async fn refresh_hit_market_specs_local(
    api: &PolymarketApi,
    cfg: &EvsnipeConfig,
    now_sec: i64,
    max_end_ts: i64,
) -> Result<Vec<EvsnipeMarketSpec>> {
    let mut markets = Vec::new();
    let max_event_offset = cfg.discovery_limit.max(500);
    let page_limit = 500_u32;
    let mut page = 0_usize;
    let mut offset = 0_u32;

    while offset < max_event_offset {
        let (page_markets, used_limit) = match api
            .get_all_active_markets_page_tagged(
                page_limit,
                offset,
                Some(EVSNIPE_DISCOVERY_TAG_SLUG),
            )
            .await
        {
            Ok(rows) => (rows, page_limit),
            Err(first_err) => {
                if page == 0 {
                    match api
                        .get_all_active_markets_page_tagged(
                            200,
                            offset,
                            Some(EVSNIPE_DISCOVERY_TAG_SLUG),
                        )
                        .await
                    {
                        Ok(rows) => (rows, 200_u32),
                        Err(second_err) => {
                            return Err(second_err).context(format!(
                                "evsnipe get_all_active_markets_page failed (offset={}, limit=200 fallback after primary error: {})",
                                offset, first_err
                            ));
                        }
                    }
                } else {
                    warn!(
                        "EVSnipe local paged market discovery truncated at page={} offset={} due to error: {}",
                        page, offset, first_err
                    );
                    break;
                }
            }
        };

        let page_len = page_markets.len();
        if page_len == 0 {
            break;
        }
        markets.extend(page_markets);
        // Gamma `/events` offset is event-based (not flattened market-count based).
        // Advancing by page limit avoids skipping entire event windows.
        offset = offset.saturating_add(used_limit);
        page = page.saturating_add(1);
    }

    let allowed: HashSet<String> = cfg.symbols.iter().map(|s| normalize_symbol(s)).collect();
    let mut out = Vec::new();
    let mut dedupe = HashSet::new();
    for market in markets {
        if !market.active || market.closed {
            continue;
        }
        let Some(spec) = extract_market_spec(&market) else {
            continue;
        };
        if let Some(end_ts) = spec.end_ts {
            if end_ts <= now_sec || end_ts > max_end_ts {
                continue;
            }
        } else {
            continue;
        }
        if !allowed.is_empty() && !allowed.contains(spec.symbol.as_str()) {
            continue;
        }
        if dedupe.insert(spec.condition_id.clone()) {
            out.push(spec);
        }
    }
    out.sort_by(|a, b| {
        a.symbol
            .cmp(&b.symbol)
            .then_with(|| a.strike_price.total_cmp(&b.strike_price))
    });
    log_event(
        "evsnipe_local_discovery_hit",
        json!({
            "symbols": cfg.symbols,
            "spec_count": out.len(),
        }),
    );
    Ok(out)
}

pub fn extract_market_spec(market: &Market) -> Option<EvsnipeMarketSpec> {
    let symbol = detect_symbol(market)?;
    if !is_supported_binance_price_market(market, symbol.as_str()) {
        return None;
    }
    let description = market.description.clone().unwrap_or_default();
    let combined_rules = format!("{} {}", market.question, description);
    let (rule, strike_price, strike_price_upper) = parse_market_rule(market, &combined_rules)?;
    let (yes_token_id, no_token_id) = select_yes_no_token_ids(market)?;
    let end_ts = parse_market_end_ts(market);

    Some(EvsnipeMarketSpec {
        condition_id: market.condition_id.clone(),
        slug: market.slug.clone(),
        question: market.question.clone(),
        symbol,
        strike_price,
        strike_price_upper,
        rule,
        yes_token_id,
        no_token_id,
        end_ts,
    })
}

fn detect_symbol(market: &Market) -> Option<String> {
    let slug = market.slug.to_ascii_lowercase();
    let question = market.question.to_ascii_lowercase();
    if starts_with_any(
        question.as_str(),
        &[
            "will bitcoin ",
            "will btc ",
            "bitcoin ",
            "btc ",
            "does bitcoin ",
            "does btc ",
        ],
    ) || starts_with_any(
        slug.as_str(),
        &["will-bitcoin-", "will-btc-", "bitcoin-", "btc-"],
    ) {
        return Some("BTC".to_string());
    }
    if starts_with_any(
        question.as_str(),
        &[
            "will ethereum ",
            "will eth ",
            "ethereum ",
            "eth ",
            "does ethereum ",
            "does eth ",
        ],
    ) || starts_with_any(
        slug.as_str(),
        &["will-ethereum-", "will-eth-", "ethereum-", "eth-"],
    ) {
        return Some("ETH".to_string());
    }
    if starts_with_any(
        question.as_str(),
        &[
            "will solana ",
            "will sol ",
            "solana ",
            "sol ",
            "does solana ",
            "does sol ",
        ],
    ) || starts_with_any(
        slug.as_str(),
        &["will-solana-", "will-sol-", "solana-", "sol-"],
    ) {
        return Some("SOL".to_string());
    }
    if starts_with_any(question.as_str(), &["will xrp ", "xrp ", "does xrp "])
        || starts_with_any(slug.as_str(), &["will-xrp-", "xrp-"])
    {
        return Some("XRP".to_string());
    }
    if starts_with_any(
        question.as_str(),
        &[
            "will dogecoin ",
            "will doge ",
            "dogecoin ",
            "doge ",
            "does dogecoin ",
            "does doge ",
        ],
    ) || starts_with_any(
        slug.as_str(),
        &["will-dogecoin-", "will-doge-", "dogecoin-", "doge-"],
    ) {
        return Some("DOGE".to_string());
    }
    if starts_with_any(
        question.as_str(),
        &[
            "will bnb ",
            "bnb ",
            "does bnb ",
            "will binance coin ",
            "binance coin ",
            "does binance coin ",
        ],
    ) || starts_with_any(
        slug.as_str(),
        &["will-bnb-", "bnb-", "will-binance-coin-", "binance-coin-"],
    ) {
        return Some("BNB".to_string());
    }
    if starts_with_any(
        question.as_str(),
        &[
            "will hype ",
            "hype ",
            "does hype ",
            "will hyperliquid ",
            "hyperliquid ",
            "does hyperliquid ",
        ],
    ) || starts_with_any(
        slug.as_str(),
        &["will-hype-", "hype-", "will-hyperliquid-", "hyperliquid-"],
    ) {
        return Some("HYPE".to_string());
    }
    let description = market
        .description
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if description.contains("btcusdt") || description.contains("btc/usdt") {
        return Some("BTC".to_string());
    }
    if description.contains("ethusdt") || description.contains("eth/usdt") {
        return Some("ETH".to_string());
    }
    if description.contains("solusdt") || description.contains("sol/usdt") {
        return Some("SOL".to_string());
    }
    if description.contains("xrpusdt") || description.contains("xrp/usdt") {
        return Some("XRP".to_string());
    }
    if description.contains("dogeusdt") || description.contains("doge/usdt") {
        return Some("DOGE".to_string());
    }
    if description.contains("bnbusdt") || description.contains("bnb/usdt") {
        return Some("BNB".to_string());
    }
    if description.contains("hypeusdt") || description.contains("hype/usdt") {
        return Some("HYPE".to_string());
    }
    None
}

fn starts_with_any(text: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| text.starts_with(prefix))
}

fn is_supported_binance_price_market(market: &Market, symbol: &str) -> bool {
    let question = market.question.to_ascii_lowercase();
    let slug = market.slug.to_ascii_lowercase();
    let description = market
        .description
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();

    // Hard-negative filters to avoid non-spot markets that can still mention ETH/BTC tokens.
    let blocked_terms = [
        "floor price",
        "nftpricefloor",
        "cryptopunks",
        "pudgy",
        "penguins",
        "volatility index",
        "volmex",
    ];
    if blocked_terms
        .iter()
        .any(|term| question.contains(term) || slug.contains(term) || description.contains(term))
    {
        return false;
    }

    // Require Binance 1m candle settlement context.
    let has_binance =
        description.contains("binance") || question.contains("binance") || slug.contains("binance");
    if !has_binance {
        return false;
    }
    let has_one_minute_context = description.contains("1 minute")
        || description.contains("1-minute")
        || description.contains("1m candle")
        || question.contains("1 minute")
        || question.contains("1-minute")
        || question.contains("1m candle");
    if !has_one_minute_context {
        return false;
    }

    let pair_tokens: &[&str] = match symbol {
        "BTC" => &["btcusdt", "btc/usdt"],
        "ETH" => &["ethusdt", "eth/usdt"],
        "SOL" => &["solusdt", "sol/usdt"],
        "XRP" => &["xrpusdt", "xrp/usdt"],
        "DOGE" => &["dogeusdt", "doge/usdt"],
        "BNB" => &["bnbusdt", "bnb/usdt"],
        "HYPE" => &["hypeusdt", "hype/usdt"],
        _ => &[],
    };
    if pair_tokens.is_empty() {
        return false;
    }
    pair_tokens
        .iter()
        .any(|pair| description.contains(pair) || question.contains(pair) || slug.contains(pair))
}

fn parse_market_rule(
    market: &Market,
    combined_text: &str,
) -> Option<(EvsnipeRule, f64, Option<f64>)> {
    let question = market.question.to_ascii_lowercase();
    let slug = market.slug.to_ascii_lowercase();
    let lower = combined_text.to_ascii_lowercase();

    // Hit-style barrier markets (High/Low touch)
    let has_high = lower.contains("high");
    let has_low = lower.contains("low");
    let has_ge = lower.contains("equal to or greater")
        || lower.contains("greater than or equal")
        || lower.contains("greater than or equals")
        || lower.contains("or higher")
        || lower.contains("at least")
        || lower.contains(">=");
    let has_le = lower.contains("equal to or less")
        || lower.contains("less than or equal")
        || lower.contains("less than or equals")
        || lower.contains("or lower")
        || lower.contains("at most")
        || lower.contains("<=");
    if has_high && has_ge {
        let strike = parse_spot_strike_price(market)?;
        return Some((EvsnipeRule::HitUpHighGte, strike, None));
    }
    if has_low && has_le {
        let strike = parse_spot_strike_price(market)?;
        return Some((EvsnipeRule::HitDownLowLte, strike, None));
    }

    // Close-at-time markets ("above/below/between").
    let has_close = lower.contains("final \"close\"")
        || lower.contains("final 'close'")
        || lower.contains(" close price ")
        || lower.contains("close\" price")
        || lower.contains("close' price")
        || lower.contains("close");
    if !has_close {
        return None;
    }

    if question.contains("between") || slug.contains("between") {
        let (low, high) = parse_range_pair_from_text(market.question.as_str())
            .or_else(|| parse_range_pair_from_text(market.slug.as_str()))?;
        let (low, high) = if low <= high {
            (low, high)
        } else {
            (high, low)
        };
        return Some((EvsnipeRule::CloseBetween, low, Some(high)));
    }

    let strike = parse_spot_strike_price(market)?;
    if question.contains("less than")
        || slug.contains("less-than")
        || question.contains("below")
        || slug.contains("-below-")
    {
        return Some((EvsnipeRule::CloseBelow, strike, None));
    }
    if question.contains("greater than")
        || slug.contains("greater-than")
        || question.contains("above")
        || slug.contains("-above-")
    {
        return Some((EvsnipeRule::CloseAbove, strike, None));
    }
    None
}

fn parse_spot_strike_price(market: &Market) -> Option<f64> {
    parse_strike_from_text(market.question.as_str())
        .or_else(|| parse_strike_from_text(market.slug.as_str()))
}

fn parse_range_pair_from_text(text: &str) -> Option<(f64, f64)> {
    let lower = text.to_ascii_lowercase();
    let idx = lower.find("between")?;
    let tail = &text[idx + "between".len()..];
    let and_idx = tail.to_ascii_lowercase().find("and")?;
    let left = tail[..and_idx].trim();
    let right = tail[and_idx + "and".len()..].trim();
    let low = parse_strike_from_text(left)?;
    let high = parse_strike_from_text(right)?;
    Some((low, high))
}

fn parse_strike_from_text(text: &str) -> Option<f64> {
    let mut best: Option<f64> = None;
    for raw in
        text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '$' || c == '.' || c == ','))
    {
        let mut token = raw
            .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != ',' && c != '$')
            .to_ascii_lowercase();
        if token.is_empty() {
            continue;
        }
        let has_dollar = token.starts_with('$') || raw.contains('$');
        token = token.trim_start_matches('$').to_string();
        let mut multiplier = 1.0_f64;
        if token.ends_with('k') {
            multiplier = 1_000.0;
            token.pop();
        } else if token.ends_with('m') {
            multiplier = 1_000_000.0;
            token.pop();
        }
        // Polymarket slugs often encode decimals as `pt` (e.g. `0pt8`).
        let token_with_decimal = if token.contains("pt") {
            token.replace("pt", ".")
        } else {
            token.clone()
        };
        let numeric = token_with_decimal.replace(',', "");
        if numeric.is_empty() {
            continue;
        }
        if !numeric.chars().all(|c| c.is_ascii_digit() || c == '.')
            || !numeric.chars().any(|c| c.is_ascii_digit())
        {
            continue;
        }
        let Ok(value) = numeric.parse::<f64>() else {
            continue;
        };
        let strike = value * multiplier;
        if !strike.is_finite() || strike <= 0.0 || strike > 10_000_000.0 {
            continue;
        }
        let has_decimal_hint = raw.to_ascii_lowercase().contains("pt") || numeric.contains('.');
        if has_dollar || multiplier > 1.0 || has_decimal_hint {
            return Some(strike);
        }
        if strike >= 10_000.0 {
            best = Some(best.map(|v| v.max(strike)).unwrap_or(strike));
        }
    }
    best
}

fn select_yes_no_token_ids(market: &Market) -> Option<(String, Option<String>)> {
    if let Some(tokens) = market.tokens.as_ref() {
        let yes = tokens
            .iter()
            .find(|token| outcome_is_yes(token.outcome.as_str()))
            .map(|token| token.token_id.clone());
        let no = tokens
            .iter()
            .find(|token| outcome_is_no(token.outcome.as_str()))
            .map(|token| token.token_id.clone());
        if let Some(yes_token_id) = yes {
            return Some((yes_token_id, no));
        }
    }
    let ids = market
        .clob_token_ids
        .as_ref()
        .and_then(|v| serde_json::from_str::<Vec<String>>(v).ok())?;
    let outcomes = market
        .outcomes
        .as_ref()
        .and_then(|v| serde_json::from_str::<Vec<String>>(v).ok())?;
    let mut yes_token: Option<String> = None;
    let mut no_token: Option<String> = None;
    for (outcome, token_id) in outcomes.iter().zip(ids.iter()) {
        if outcome_is_yes(outcome.as_str()) {
            yes_token = Some(token_id.clone());
        } else if outcome_is_no(outcome.as_str()) {
            no_token = Some(token_id.clone());
        }
    }
    yes_token.map(|yes| (yes, no_token))
}

fn outcome_is_yes(outcome: &str) -> bool {
    let norm = outcome.trim().to_ascii_lowercase();
    norm == "yes" || norm == "1" || norm == "true"
}

fn outcome_is_no(outcome: &str) -> bool {
    let norm = outcome.trim().to_ascii_lowercase();
    norm == "no" || norm == "0" || norm == "false"
}

fn parse_market_end_ts(market: &Market) -> Option<i64> {
    if let Some(end_date) = market.end_date.as_deref() {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(end_date) {
            return Some(dt.timestamp());
        }
    }

    for value in [
        market.end_date_iso.as_deref(),
        market.end_date_iso_alt.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(value) {
            return Some(dt.timestamp());
        }
        if let Ok(date) = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d") {
            let naive = date.and_hms_opt(23, 59, 59)?;
            return Some(naive.and_utc().timestamp());
        }
    }

    None
}

pub fn spawn_binance_trade_streams(
    symbols: &[String],
    tx: mpsc::Sender<BinanceTradeTick>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::new();
    let mut seen = HashSet::new();
    for symbol in symbols {
        let symbol_norm = normalize_symbol(symbol);
        if symbol_norm.is_empty() || !seen.insert(symbol_norm.clone()) {
            continue;
        }
        handles.push(spawn_single_symbol_trade_stream(symbol_norm, tx.clone()));
    }
    handles
}

pub fn spawn_binance_kline_close_streams(
    symbols: &[String],
    tx: mpsc::Sender<BinanceKlineCloseTick>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::new();
    let mut seen = HashSet::new();
    for symbol in symbols {
        let symbol_norm = normalize_symbol(symbol);
        if symbol_norm.is_empty() || !seen.insert(symbol_norm.clone()) {
            continue;
        }
        handles.push(spawn_single_symbol_kline_close_stream(
            symbol_norm,
            tx.clone(),
        ));
    }
    handles
}

fn spawn_single_symbol_trade_stream(
    symbol: String,
    tx: mpsc::Sender<BinanceTradeTick>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let send_timeout_ms = env_u64("EVPOLY_EVSNIPE_TICK_SEND_TIMEOUT_MS", 60).clamp(5, 500);
        let stream = binance_trade_stream_url(symbol.as_str());
        let mut backoff_sec: u64 = 1;
        let mut dropped_events: u64 = 0;
        loop {
            match connect_async(stream.as_str()).await {
                Ok((mut ws, _)) => {
                    log_event(
                        "evsnipe_binance_connected",
                        json!({
                            "strategy_id": "evsnipe_v1",
                            "symbol": symbol,
                            "stream": stream,
                            "tick_send_timeout_ms": send_timeout_ms
                        }),
                    );
                    backoff_sec = 1;
                    loop {
                        let Some(next_msg) = ws.next().await else {
                            break;
                        };
                        let Ok(msg) = next_msg else {
                            break;
                        };
                        if !msg.is_text() {
                            continue;
                        }
                        let Ok(text) = msg.into_text() else {
                            continue;
                        };
                        let Ok(trade) = serde_json::from_str::<BinanceTradeMsg>(&text) else {
                            continue;
                        };
                        let Ok(price) = trade.price.parse::<f64>() else {
                            continue;
                        };
                        if !price.is_finite() || price <= 0.0 {
                            continue;
                        }
                        let tick = BinanceTradeTick {
                            symbol: symbol.clone(),
                            price,
                            trade_ts_ms: trade.trade_time_ms,
                            recv_ts_ms: chrono::Utc::now().timestamp_millis(),
                        };
                        match tx.try_send(tick) {
                            Ok(_) => {}
                            Err(tokio::sync::mpsc::error::TrySendError::Full(tick_full)) => {
                                // Bounded spillway: wait briefly before dropping to reduce
                                // burst losses while preserving real-time behavior.
                                let sent = tokio::time::timeout(
                                    Duration::from_millis(send_timeout_ms),
                                    tx.send(tick_full),
                                )
                                .await
                                .is_ok();
                                if !sent {
                                    dropped_events = dropped_events.saturating_add(1);
                                    if dropped_events == 1 || dropped_events % 500 == 0 {
                                        log_event(
                                            "evsnipe_binance_tick_drop",
                                            json!({
                                                "strategy_id": "evsnipe_v1",
                                                "symbol": symbol,
                                                "dropped_events": dropped_events,
                                                "reason": "channel_full_timeout"
                                            }),
                                        );
                                    }
                                }
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                        }
                    }
                }
                Err(err) => {
                    log_event(
                        "evsnipe_binance_connect_failed",
                        json!({
                            "strategy_id": "evsnipe_v1",
                            "symbol": symbol,
                            "error": err.to_string(),
                            "backoff_sec": backoff_sec
                        }),
                    );
                }
            }
            sleep(Duration::from_secs(backoff_sec)).await;
            backoff_sec = (backoff_sec.saturating_mul(2)).min(20);
        }
    })
}

fn spawn_single_symbol_kline_close_stream(
    symbol: String,
    tx: mpsc::Sender<BinanceKlineCloseTick>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let send_timeout_ms = env_u64("EVPOLY_EVSNIPE_KLINE_SEND_TIMEOUT_MS", 90).clamp(5, 500);
        let stream = binance_kline_close_stream_url(symbol.as_str());
        let mut backoff_sec: u64 = 1;
        let mut dropped_events: u64 = 0;
        loop {
            match connect_async(stream.as_str()).await {
                Ok((mut ws, _)) => {
                    log_event(
                        "evsnipe_binance_kline_connected",
                        json!({
                            "strategy_id": "evsnipe_v1",
                            "symbol": symbol,
                            "stream": stream,
                            "kline_send_timeout_ms": send_timeout_ms
                        }),
                    );
                    backoff_sec = 1;
                    loop {
                        let Some(next_msg) = ws.next().await else {
                            break;
                        };
                        let Ok(msg) = next_msg else {
                            break;
                        };
                        if !msg.is_text() {
                            continue;
                        }
                        let Ok(text) = msg.into_text() else {
                            continue;
                        };
                        let Ok(kline) = serde_json::from_str::<BinanceKlineMsg>(&text) else {
                            continue;
                        };
                        if !kline.kline.is_closed {
                            continue;
                        }
                        let Ok(close_price) = kline.kline.close_price.parse::<f64>() else {
                            continue;
                        };
                        if !close_price.is_finite() || close_price <= 0.0 {
                            continue;
                        }
                        let tick = BinanceKlineCloseTick {
                            symbol: symbol.clone(),
                            close_price,
                            close_ts_ms: kline.kline.close_ts_ms,
                            recv_ts_ms: chrono::Utc::now().timestamp_millis(),
                        };
                        match tx.try_send(tick) {
                            Ok(_) => {}
                            Err(tokio::sync::mpsc::error::TrySendError::Full(tick_full)) => {
                                let sent = tokio::time::timeout(
                                    Duration::from_millis(send_timeout_ms),
                                    tx.send(tick_full),
                                )
                                .await
                                .is_ok();
                                if !sent {
                                    dropped_events = dropped_events.saturating_add(1);
                                    if dropped_events == 1 || dropped_events % 200 == 0 {
                                        log_event(
                                            "evsnipe_binance_kline_drop",
                                            json!({
                                                "strategy_id": "evsnipe_v1",
                                                "symbol": symbol,
                                                "dropped_events": dropped_events,
                                                "reason": "channel_full_timeout"
                                            }),
                                        );
                                    }
                                }
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                        }
                    }
                }
                Err(err) => {
                    log_event(
                        "evsnipe_binance_kline_connect_failed",
                        json!({
                            "strategy_id": "evsnipe_v1",
                            "symbol": symbol,
                            "error": err.to_string(),
                            "backoff_sec": backoff_sec
                        }),
                    );
                }
            }
            sleep(Duration::from_secs(backoff_sec)).await;
            backoff_sec = (backoff_sec.saturating_mul(2)).min(20);
        }
    })
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .and_then(|v| match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Market;

    #[test]
    fn select_cross_ask_prefers_requested_deeper_level() {
        let orderbook = OrderBook {
            bids: vec![],
            asks: vec![
                crate::models::OrderBookEntry {
                    price: rust_decimal::Decimal::new(95, 2),
                    size: rust_decimal::Decimal::new(10, 0),
                },
                crate::models::OrderBookEntry {
                    price: rust_decimal::Decimal::new(97, 2),
                    size: rust_decimal::Decimal::new(10, 0),
                },
                crate::models::OrderBookEntry {
                    price: rust_decimal::Decimal::new(99, 2),
                    size: rust_decimal::Decimal::new(10, 0),
                },
            ],
        };
        let selected = select_cross_ask_price(&orderbook, 3).unwrap();
        assert_eq!(selected.selected_level, 3);
        assert!((selected.selected_ask - 0.99).abs() < 1e-9);
    }

    #[test]
    fn parse_strike_with_suffix() {
        assert_eq!(
            parse_strike_from_text("Will Bitcoin hit $150k by March?"),
            Some(150_000.0)
        );
        assert_eq!(
            parse_strike_from_text("will-bitcoin-hit-1m-before-gta"),
            Some(1_000_000.0)
        );
    }

    #[test]
    fn parse_strike_supports_small_and_pt_decimals() {
        assert_eq!(
            parse_strike_from_text("Will XRP dip to $0.8 in March?"),
            Some(0.8)
        );
        assert_eq!(
            parse_strike_from_text("will-xrp-dip-to-0pt8-march-2-8"),
            Some(0.8)
        );
        assert_eq!(
            parse_strike_from_text("Will Solana dip to $70 in March?"),
            Some(70.0)
        );
    }

    #[test]
    fn parse_market_rule_high_gte() {
        let market = Market {
            condition_id: "cond-hit".to_string(),
            market_id: None,
            question: "Will Bitcoin hit $100k?".to_string(),
            slug: "will-bitcoin-hit-100k".to_string(),
            description: Some(
                "resolves yes if final 'High' price is equal to or greater than strike on Binance BTC/USDT 1 minute candle".to_string(),
            ),
            resolution_source: None,
            end_date: Some("2026-04-01T04:00:00Z".to_string()),
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
        };
        let parsed = parse_market_rule(
            &market,
            &format!(
                "{} {}",
                market.question,
                market.description.clone().unwrap_or_default()
            ),
        )
        .unwrap();
        assert_eq!(parsed.0, EvsnipeRule::HitUpHighGte);
        assert_eq!(parsed.1, 100_000.0);
    }

    #[test]
    fn parse_market_rule_close_between() {
        let market = Market {
            condition_id: "cond-range".to_string(),
            market_id: None,
            question: "Will the price of Ethereum be between $1,500 and $1,600 on March 6?"
                .to_string(),
            slug: "will-the-price-of-ethereum-be-between-1500-1600-on-march-6".to_string(),
            description: Some(
                "This market will resolve according to the final \"Close\" price of the Binance 1 minute candle for ETH/USDT 12:00 ET".to_string(),
            ),
            resolution_source: None,
            end_date: Some("2026-03-06T17:00:00Z".to_string()),
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
        };
        let parsed = parse_market_rule(
            &market,
            &format!(
                "{} {}",
                market.question,
                market.description.clone().unwrap_or_default()
            ),
        )
        .unwrap();
        assert_eq!(parsed.0, EvsnipeRule::CloseBetween);
        assert_eq!(parsed.1, 1500.0);
        assert_eq!(parsed.2, Some(1600.0));
    }

    #[test]
    fn parse_market_end_ts_uses_end_date_timestamp() {
        let market = Market {
            condition_id: "cond-1".to_string(),
            market_id: None,
            question: "Will Bitcoin reach $100,000 in March?".to_string(),
            slug: "will-bitcoin-reach-100k-in-march-2026".to_string(),
            description: Some("Binance 1 minute candle BTC/USDT High".to_string()),
            resolution_source: None,
            end_date: Some("2026-04-01T04:00:00Z".to_string()),
            end_date_iso: Some("2026-04-01".to_string()),
            end_date_iso_alt: None,
            game_start_time: None,
            start_date: None,
            active: true,
            closed: false,
            tokens: None,
            clob_token_ids: None,
            outcomes: None,
            competitive: None,
        };
        assert_eq!(parse_market_end_ts(&market), Some(1_775_016_000));
    }

    #[test]
    fn parse_market_end_ts_accepts_date_only_iso() {
        let market = Market {
            condition_id: "cond-2".to_string(),
            market_id: None,
            question: "Will Bitcoin reach $100,000 in March?".to_string(),
            slug: "will-bitcoin-reach-100k-in-march-2026".to_string(),
            description: Some("Binance 1 minute candle BTC/USDT High".to_string()),
            resolution_source: None,
            end_date: None,
            end_date_iso: Some("2026-04-01".to_string()),
            end_date_iso_alt: None,
            game_start_time: None,
            start_date: None,
            active: true,
            closed: false,
            tokens: None,
            clob_token_ids: None,
            outcomes: None,
            competitive: None,
        };
        assert_eq!(parse_market_end_ts(&market), Some(1_775_087_999));
    }

    #[test]
    fn strike_filter_keeps_only_near_anchor() {
        let specs = vec![
            EvsnipeMarketSpec {
                condition_id: "a".to_string(),
                slug: "btc-near".to_string(),
                question: "near".to_string(),
                symbol: "BTC".to_string(),
                strike_price: 105.0,
                strike_price_upper: None,
                rule: EvsnipeRule::HitUpHighGte,
                yes_token_id: "yes-a".to_string(),
                no_token_id: None,
                end_ts: Some(2_000_000_000),
            },
            EvsnipeMarketSpec {
                condition_id: "b".to_string(),
                slug: "btc-far".to_string(),
                question: "far".to_string(),
                symbol: "BTC".to_string(),
                strike_price: 125.0,
                strike_price_upper: None,
                rule: EvsnipeRule::HitUpHighGte,
                yes_token_id: "yes-b".to_string(),
                no_token_id: None,
                end_ts: Some(2_000_000_000),
            },
        ];
        let mut anchors = HashMap::new();
        anchors.insert(
            "BTC".to_string(),
            EvsnipeSpotAnchor {
                price: 100.0,
                updated_at_ms: 1_000,
            },
        );
        let filtered = filter_specs_by_spot_anchor(specs.as_slice(), &anchors, 0.10);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].slug, "btc-near");
    }

    #[test]
    fn next_spot_anchor_refreshes_on_drift() {
        let existing = Some(EvsnipeSpotAnchor {
            price: 100.0,
            updated_at_ms: 1_000,
        });
        let (anchor, reason) = next_spot_anchor(existing, 104.0, 2_000, 14_400, 0.03).unwrap();
        assert_eq!(reason, Some(EvsnipeAnchorRefreshReason::Drift));
        assert_eq!(anchor.price, 104.0);
        assert_eq!(anchor.updated_at_ms, 2_000);
    }

    #[test]
    fn close_between_condition_yes() {
        let spec = EvsnipeMarketSpec {
            condition_id: "x".to_string(),
            slug: "range".to_string(),
            question: "range".to_string(),
            symbol: "ETH".to_string(),
            strike_price: 1500.0,
            strike_price_upper: Some(1600.0),
            rule: EvsnipeRule::CloseBetween,
            yes_token_id: "yes".to_string(),
            no_token_id: Some("no".to_string()),
            end_ts: Some(2_000_000_000),
        };
        assert_eq!(spec.close_condition_yes(1550.0), Some(true));
        assert_eq!(spec.close_condition_yes(1650.0), Some(false));
    }

    #[test]
    fn detect_symbol_supports_doge_bnb_hype() {
        let mut market = Market {
            condition_id: "cond-new".to_string(),
            market_id: None,
            question: "Will Dogecoin hit $1?".to_string(),
            slug: "will-dogecoin-hit-1".to_string(),
            description: Some("Binance 1 minute candle DOGE/USDT High".to_string()),
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
        };
        assert_eq!(detect_symbol(&market).as_deref(), Some("DOGE"));

        market.question = "Will BNB close above $700?".to_string();
        market.slug = "will-bnb-close-above-700".to_string();
        market.description = Some("Binance 1 minute candle BNB/USDT Close".to_string());
        assert_eq!(detect_symbol(&market).as_deref(), Some("BNB"));

        market.question = "Will Hyperliquid close above $50?".to_string();
        market.slug = "will-hyperliquid-close-above-50".to_string();
        market.description = Some("Binance 1 minute candle HYPE/USDT Close".to_string());
        assert_eq!(detect_symbol(&market).as_deref(), Some("HYPE"));
    }
}
