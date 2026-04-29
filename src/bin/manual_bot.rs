use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use clap::Parser;
use polymarket_arbitrage_bot::adaptive_chase::{
    decide_replace_action, AdaptiveChaseConfig, ReplaceAction, ReplacePolicyInput,
};
use polymarket_arbitrage_bot::api::{PolymarketApi, RedeemConditionRequest};
use polymarket_arbitrage_bot::config::Config;
use polymarket_arbitrage_bot::detector::{BuyOpportunity, TokenType};
use polymarket_arbitrage_bot::models::{Market, MarketDetails, OrderRequest, TokenPrice};
use polymarket_arbitrage_bot::polymarket_ws::{
    self, new_shared_polymarket_ws_state, PolymarketWsConfig, SharedPolymarketWsState,
};
use polymarket_arbitrage_bot::strategy::{Direction, Timeframe};
use polymarket_arbitrage_bot::trader::{
    EntryExecutionMode, Trader, STRATEGY_ID_MANUAL_CHASE_LIMIT_TAKER_V1,
    STRATEGY_ID_MANUAL_CHASE_LIMIT_V1, STRATEGY_ID_MANUAL_PREMARKET_TAKER_V1,
    STRATEGY_ID_MANUAL_PREMARKET_V1,
};
use polymarket_arbitrage_bot::ui_contracts::{
    build_ui_market, manual_mode_label, manual_run_kind_label, manual_run_status_label,
    manual_side_label, UiManualHealth, UiManualPosition, UiManualRun, UiMarket,
};
use polymarket_client_sdk_v2::clob::types::response::OpenOrderResponse;
use polymarket_client_sdk_v2::clob::types::OrderStatusType;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::RwLock;

const DEFAULT_BIND: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8791;
const DEFAULT_MAX_BODY_BYTES: usize = 64 * 1024;
const DEFAULT_PREMARKET_SIDE_BUDGET_USD: f64 = 200.0;

fn manual_auth_required_for_bind(addr: SocketAddr) -> bool {
    !addr.ip().is_loopback()
}
const MANUAL_WS_BALANCE_FALLBACK_POLL_MS: i64 = 2_000;
const MANUAL_TP_SUBMIT_ATTEMPTS: u32 = 3;
const MANUAL_TP_RETRY_DELAY_MS: u64 = 250;
const MANUAL_MIN_PRICE: f64 = 0.001;
const MANUAL_MAX_PRICE: f64 = 0.999;
const MANUAL_MARKET_SEARCH_LIMIT_DEFAULT: usize = 12;
const MANUAL_MARKET_SEARCH_LIMIT_MAX: usize = 24;
const MANUAL_MARKET_SEARCH_PAGE_SIZE: u32 = 100;
const MANUAL_MARKET_SEARCH_MAX_PAGES: u32 = 3;

#[derive(Parser, Debug)]
#[command(name = "manual_bot")]
#[command(about = "Standalone EVPOLY manual execution API")]
struct Args {
    #[arg(long, default_value = "config.json")]
    config: PathBuf,

    #[arg(long, default_value = DEFAULT_BIND)]
    bind: String,

    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,

    #[arg(long)]
    token: Option<String>,

    #[arg(long, default_value_t = false)]
    simulation: bool,

    #[arg(long, default_value_t = DEFAULT_MAX_BODY_BYTES)]
    max_body_bytes: usize,
}

#[derive(Clone)]
struct AppState {
    api: Arc<PolymarketApi>,
    trader: Arc<Trader>,
    auth_token: Option<String>,
    simulation_mode: bool,
    polymarket_ws_state: Option<SharedPolymarketWsState>,
    run_registry: Arc<RwLock<HashMap<String, Arc<ManualRun>>>>,
    scope_claims: Arc<RwLock<HashMap<String, String>>>,
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

impl From<anyhow::Error> for ApiError {
    fn from(value: anyhow::Error) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, value.to_string())
    }
}

static MANUAL_NONCE: AtomicU64 = AtomicU64::new(1);

fn next_manual_request_id_nonce() -> u64 {
    MANUAL_NONCE.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Clone, Serialize)]
struct ManualRunSnapshot {
    run_id: String,
    run_type: String,
    status: String,
    stop_reason: Option<String>,
    started_at_ms: i64,
    updated_at_ms: i64,
    finished_at_ms: Option<i64>,
    condition_id: String,
    market_slug: String,
    token_id: String,
    timeframe: String,
    period_timestamp: u64,
    post_only: bool,
    min_price: f64,
    max_price: f64,
    target_shares: f64,
    requested_size_usd: Option<f64>,
    filled_shares: f64,
    filled_notional_usd: f64,
    submit_attempts: u64,
    submit_success: u64,
    submit_errors: u64,
    canceled_orders: u64,
    last_submit_price: Option<f64>,
    tp_price: Option<f64>,
    tp_order_id: Option<String>,
    last_error: Option<String>,
}

#[derive(Debug)]
struct ManualRun {
    snapshot: RwLock<ManualRunSnapshot>,
    stop_requested: AtomicBool,
}

impl ManualRun {
    fn new(snapshot: ManualRunSnapshot) -> Self {
        Self {
            snapshot: RwLock::new(snapshot),
            stop_requested: AtomicBool::new(false),
        }
    }

    fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::Relaxed);
    }

    fn stop_requested(&self) -> bool {
        self.stop_requested.load(Ordering::Relaxed)
    }

    async fn snapshot(&self) -> ManualRunSnapshot {
        self.snapshot.read().await.clone()
    }

    async fn update<F>(&self, update: F)
    where
        F: FnOnce(&mut ManualRunSnapshot),
    {
        let mut guard = self.snapshot.write().await;
        update(&mut guard);
        guard.updated_at_ms = Utc::now().timestamp_millis();
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ManualPremarketRequest {
    condition_id: Option<String>,
    market_slug: Option<String>,
    up_token_id: Option<String>,
    down_token_id: Option<String>,
    side: Option<String>,
    period_timestamp: Option<u64>,
    target_size_usd: Option<f64>,
    post_only: Option<bool>,
    order_type: Option<String>,
    symbol: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManualOrderRequest {
    condition_id: Option<String>,
    market_slug: Option<String>,
    token_id: Option<String>,
    side: Option<String>,
    period_timestamp: Option<u64>,
    timeframe: Option<String>,
    mode: Option<String>,
    size: Option<f64>,
    size_unit: Option<String>,
    size_usd: Option<f64>,
    target_shares: Option<f64>,
    price: Option<f64>,
    min_price: Option<f64>,
    max_price: Option<f64>,
    duration_min: Option<u64>,
    max_duration_sec: Option<u64>,
    poll_ms: Option<u64>,
    post_only: Option<bool>,
    order_type: Option<String>,
    symbol: Option<String>,
    tp_price: Option<f64>,
    slippage_cents: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManualOrderAction {
    Open,
    Close,
}

impl ManualOrderAction {
    fn as_str(self) -> &'static str {
        match self {
            ManualOrderAction::Open => "open",
            ManualOrderAction::Close => "close",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManualOrderMode {
    ChaseLimit,
    Limit,
    Market,
}

impl ManualOrderMode {
    fn parse(raw: Option<&str>) -> Self {
        match raw
            .map(|v| v.trim().to_ascii_lowercase())
            .unwrap_or_else(|| "chase_limit".to_string())
            .as_str()
        {
            "limit" => Self::Limit,
            "market" => Self::Market,
            _ => Self::ChaseLimit,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            ManualOrderMode::ChaseLimit => "chase_limit",
            ManualOrderMode::Limit => "limit",
            ManualOrderMode::Market => "market",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManualOrderSizeUnit {
    Shares,
    Usd,
}

impl ManualOrderSizeUnit {
    fn parse(raw: Option<&str>) -> Result<Self> {
        let normalized = raw
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty());
        match normalized.as_deref() {
            None | Some("share") | Some("shares") => Ok(Self::Shares),
            Some("usd") | Some("dollar") | Some("dollars") => Ok(Self::Usd),
            Some(other) => anyhow::bail!(
                "invalid size_unit '{}'; supported values: shares, usd",
                other
            ),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ManualMergeRequest {
    condition_id: Option<String>,
    sweep: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ManualRedeemRequest {
    condition_id: Option<String>,
    outcome_label: Option<String>,
    sweep: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManualAutoRedeemRequest {
    enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ManualChaseStopRequest {
    run_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PositionsQuery {
    wallet: Option<String>,
    include_zero: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct BalanceQuery {
    wallet: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct MarketSearchQuery {
    q: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct MarketRecentQuery {
    limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PnlQuery {
    wallet: Option<String>,
    window: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RunListQuery {
    status: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ManualAuditQuery {
    include_runs: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ManualDoctorFixRequest {
    run_ids: Option<Vec<String>>,
    stop_running: Option<bool>,
    cancel_pending: Option<bool>,
    sync_portfolio: Option<bool>,
    prune_completed: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ManualDoctorResetRequest {
    hard: Option<bool>,
    stop_running: Option<bool>,
    cancel_pending: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManualSideMode {
    Up,
    Down,
    Both,
}

impl ManualSideMode {
    fn from_raw(raw: Option<&str>) -> Self {
        match raw.unwrap_or("both").trim().to_ascii_lowercase().as_str() {
            "up" | "yes" | "buy_yes" => Self::Up,
            "down" | "no" | "buy_no" => Self::Down,
            _ => Self::Both,
        }
    }

    fn include_up(self) -> bool {
        matches!(self, Self::Up | Self::Both)
    }

    fn include_down(self) -> bool {
        matches!(self, Self::Down | Self::Both)
    }
}

#[derive(Debug, Clone, Copy)]
struct PremarketOrderFloor {
    min_notional_usd: f64,
    min_size_shares: f64,
}

impl PremarketOrderFloor {
    fn from_snapshot(snapshot: polymarket_arbitrage_bot::api::MarketConstraintSnapshot) -> Self {
        let min_notional_usd = f64::try_from(snapshot.minimum_order_size)
            .unwrap_or(5.0)
            .max(0.0);
        let min_size_shares = f64::try_from(snapshot.min_size_shares)
            .unwrap_or(0.0)
            .max(0.0);
        Self {
            min_notional_usd,
            min_size_shares,
        }
    }

    fn fallback_default() -> Self {
        Self {
            min_notional_usd: 5.0,
            min_size_shares: 0.0,
        }
    }

    fn required_notional_usd(self, price: f64) -> f64 {
        let safe_price = price.max(0.000001);
        let share_floor = if self.min_size_shares > 0.0 {
            self.min_size_shares * safe_price
        } else {
            0.0
        };
        self.min_notional_usd.max(share_floor).max(0.0)
    }
}

#[derive(Debug, Clone)]
struct PremarketSizedRung {
    price: f64,
    size_usd: f64,
}

fn premarket_fixed_ladder_prices() -> &'static [f64] {
    const PRICES: [f64; 6] = [0.40, 0.30, 0.24, 0.21, 0.15, 0.12];
    &PRICES
}

fn premarket_fixed_ladder_weights() -> &'static [f64] {
    const WEIGHTS: [f64; 6] = [0.23, 0.23, 0.17, 0.14, 0.12, 0.11];
    &WEIGHTS
}

fn build_premarket_fixed_rungs(
    target_size_usd: f64,
    floor: PremarketOrderFloor,
) -> Vec<PremarketSizedRung> {
    if target_size_usd <= 0.0 {
        return Vec::new();
    }

    let prices = premarket_fixed_ladder_prices();
    let weights = premarket_fixed_ladder_weights();
    let rung_count = prices.len().min(weights.len());
    let mut rungs = Vec::with_capacity(rung_count);
    for idx in 0..rung_count {
        let price = prices[idx].clamp(0.01, 0.99);
        let mut size_usd = target_size_usd * weights[idx].max(0.0);
        let min_required = floor.required_notional_usd(price);
        if size_usd + 1e-9 < min_required {
            size_usd = min_required;
        }
        if !size_usd.is_finite() || size_usd <= 0.0 {
            continue;
        }
        rungs.push(PremarketSizedRung { price, size_usd });
    }
    rungs
}

fn premarket_side_budget_usd() -> f64 {
    std::env::var("EVPOLY_PREMARKET_SIDE_BUDGET_5M_USD")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(DEFAULT_PREMARKET_SIDE_BUDGET_USD)
}

fn manual_chase_fill_observe_max_ms() -> u64 {
    std::env::var("EVPOLY_MANUAL_CHASE_FILL_OBSERVE_MAX_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(8_000)
}

fn manual_close_cancel_confirm_max_ms() -> u64 {
    std::env::var("EVPOLY_MANUAL_CLOSE_CANCEL_CONFIRM_MAX_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(12_000)
}

fn manual_close_post_cancel_reconcile_ms() -> u64 {
    std::env::var("EVPOLY_MANUAL_CLOSE_POST_CANCEL_RECONCILE_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(15_000)
}

fn parse_bool_query(raw: Option<&str>, default: bool) -> bool {
    match raw.map(|value| value.trim().to_ascii_lowercase()) {
        Some(value) if matches!(value.as_str(), "1" | "true" | "yes" | "on") => true,
        Some(value) if matches!(value.as_str(), "0" | "false" | "no" | "off") => false,
        _ => default,
    }
}

fn normalize_market_symbol(symbol: &str) -> String {
    match symbol.trim().to_ascii_uppercase().as_str() {
        "BTC" => "BTC".to_string(),
        "ETH" => "ETH".to_string(),
        "SOL" | "SOLANA" => "SOL".to_string(),
        "XRP" => "XRP".to_string(),
        other => other.to_string(),
    }
}

fn infer_symbol_from_market(market: &Market) -> String {
    let slug = market.slug.to_ascii_lowercase();
    let question = market.question.to_ascii_lowercase();
    if slug.contains("btc") || question.contains("bitcoin") || question.contains(" btc ") {
        "BTC".to_string()
    } else if slug.contains("eth") || question.contains("ethereum") || question.contains(" eth ") {
        "ETH".to_string()
    } else if slug.contains("sol") || question.contains("solana") || question.contains(" sol ") {
        "SOL".to_string()
    } else if slug.contains("xrp") || question.contains("ripple") || question.contains(" xrp ") {
        "XRP".to_string()
    } else {
        "MISC".to_string()
    }
}

fn parse_timeframe_raw(raw: &str) -> Option<Timeframe> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "5m" => Some(Timeframe::M5),
        "15m" => Some(Timeframe::M15),
        "1h" | "60m" => Some(Timeframe::H1),
        "4h" | "240m" => Some(Timeframe::H4),
        "1d" | "24h" | "d1" => Some(Timeframe::D1),
        _ => None,
    }
}

fn manual_chase_use_evcurve_reprice_for_crypto() -> bool {
    std::env::var("EVPOLY_MANUAL_CHASE_USE_EVCURVE_REPRICE_CRYPTO")
        .ok()
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true)
}

fn is_crypto_price_market(
    symbol: &str,
    market: &Market,
    market_details: Option<&MarketDetails>,
) -> bool {
    let normalized = normalize_market_symbol(symbol);
    if !matches!(normalized.as_str(), "BTC" | "ETH" | "SOL" | "XRP") {
        return false;
    }
    let slug = market.slug.to_ascii_lowercase();
    let question = market.question.to_ascii_lowercase();
    let details_blob = market_details
        .and_then(|d| serde_json::to_string(d).ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let looks_price = slug.contains("what-price-will-")
        || slug.contains("price-on-")
        || slug.contains("above-on-")
        || slug.contains("below-on-")
        || slug.contains("will-bitcoin-reach-")
        || question.contains("what price")
        || question.contains("reach")
        || question.contains("dip to")
        || question.contains("between");
    let has_crypto_ref = slug.contains("bitcoin")
        || slug.contains("ethereum")
        || slug.contains("solana")
        || slug.contains("xrp")
        || details_blob.contains("binance")
        || details_blob.contains("coinbase");
    looks_price && has_crypto_ref
}

fn token_type_for_market_symbol(symbol: &str, direction: Direction) -> TokenType {
    let normalized = normalize_market_symbol(symbol);
    match (normalized.as_str(), direction) {
        ("ETH", Direction::Up) => TokenType::EthUp,
        ("ETH", Direction::Down) => TokenType::EthDown,
        ("SOL", Direction::Up) => TokenType::SolanaUp,
        ("SOL", Direction::Down) => TokenType::SolanaDown,
        ("XRP", Direction::Up) => TokenType::XrpUp,
        ("XRP", Direction::Down) => TokenType::XrpDown,
        (_, Direction::Up) => TokenType::BtcUp,
        (_, Direction::Down) => TokenType::BtcDown,
    }
}

fn nonempty_owned(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn outcome_matches_direction(outcome_raw: &str, direction: Direction) -> bool {
    let outcome = outcome_raw.trim().to_ascii_uppercase();
    match direction {
        Direction::Up => {
            outcome.contains("UP") || outcome == "YES" || outcome == "1" || outcome == "TRUE"
        }
        Direction::Down => {
            outcome.contains("DOWN") || outcome == "NO" || outcome == "0" || outcome == "FALSE"
        }
    }
}

fn select_token_id_for_direction(market: &Market, direction: Direction) -> Option<String> {
    if let Some(token_id) = market.tokens.as_ref().and_then(|tokens| {
        tokens
            .iter()
            .find(|token| outcome_matches_direction(token.outcome.as_str(), direction))
            .map(|token| token.token_id.clone())
    }) {
        return Some(token_id);
    }

    let ids = market
        .clob_token_ids
        .as_ref()
        .and_then(|v| serde_json::from_str::<Vec<String>>(v).ok())?;
    let outcomes = market
        .outcomes
        .as_ref()
        .and_then(|v| serde_json::from_str::<Vec<String>>(v).ok());

    if let Some(outcomes) = outcomes {
        if let Some(token_id) = outcomes
            .iter()
            .zip(ids.iter())
            .find_map(|(outcome, token_id)| {
                outcome_matches_direction(outcome.as_str(), direction).then(|| token_id.clone())
            })
        {
            return Some(token_id);
        }
    }

    if ids.len() >= 2 {
        return match direction {
            Direction::Up => Some(ids[0].clone()),
            Direction::Down => Some(ids[1].clone()),
        };
    }
    None
}

fn select_token_id_from_market_details(
    details: &MarketDetails,
    direction: Direction,
) -> Option<String> {
    details
        .tokens
        .iter()
        .find(|token| outcome_matches_direction(token.outcome.as_str(), direction))
        .map(|token| token.token_id.clone())
        .or_else(|| {
            if details.tokens.len() >= 2 {
                match direction {
                    Direction::Up => Some(details.tokens[0].token_id.clone()),
                    Direction::Down => Some(details.tokens[1].token_id.clone()),
                }
            } else {
                None
            }
        })
}

fn market_from_details(details: &MarketDetails) -> Market {
    let end_iso = details.end_date_iso.trim();
    Market {
        condition_id: details.condition_id.clone(),
        market_id: None,
        question: details.question.clone(),
        slug: details.market_slug.clone(),
        description: Some(details.description.clone()),
        resolution_source: None,
        end_date: (!end_iso.is_empty()).then_some(end_iso.to_string()),
        end_date_iso: (!end_iso.is_empty()).then_some(end_iso.to_string()),
        end_date_iso_alt: (!end_iso.is_empty()).then_some(end_iso.to_string()),
        game_start_time: details.game_start_time.clone(),
        start_date: None,
        sports_market_type: None,
        active: details.active,
        closed: details.closed,
        tokens: Some(
            details
                .tokens
                .iter()
                .map(|token| polymarket_arbitrage_bot::models::Token {
                    token_id: token.token_id.clone(),
                    outcome: token.outcome.clone(),
                    price: Some(token.price),
                })
                .collect(),
        ),
        clob_token_ids: None,
        outcomes: None,
        competitive: None,
        events: None,
    }
}

async fn resolve_manual_market_request(
    api: &PolymarketApi,
    condition_id: Option<&str>,
    market_slug: Option<&str>,
) -> Result<(Market, Option<MarketDetails>)> {
    let normalized_condition_id = condition_id
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string);
    let normalized_market_slug = market_slug
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string);

    if normalized_condition_id.is_none() && normalized_market_slug.is_none() {
        anyhow::bail!("manual request requires condition_id or market_slug");
    }

    if let Some(slug) = normalized_market_slug.as_deref() {
        let mut market = api
            .get_market_by_slug(slug)
            .await
            .with_context(|| format!("failed to resolve market_slug={}", slug))?;
        if let Some(expected_condition_id) = normalized_condition_id.as_deref() {
            if !market
                .condition_id
                .trim()
                .eq_ignore_ascii_case(expected_condition_id)
            {
                anyhow::bail!(
                    "condition_id mismatch: request={} resolved={}",
                    expected_condition_id,
                    market.condition_id
                );
            }
        }
        let details = api.get_market(market.condition_id.as_str()).await.ok();
        if market
            .tokens
            .as_ref()
            .map(|tokens| tokens.is_empty())
            .unwrap_or(true)
        {
            if let Some(details_ref) = details.as_ref() {
                market.tokens = Some(
                    details_ref
                        .tokens
                        .iter()
                        .map(|token| polymarket_arbitrage_bot::models::Token {
                            token_id: token.token_id.clone(),
                            outcome: token.outcome.clone(),
                            price: Some(token.price),
                        })
                        .collect(),
                );
            }
        }
        if market.slug.trim().is_empty() {
            if let Some(details_ref) = details.as_ref() {
                market.slug = details_ref.market_slug.clone();
            }
        }
        if market.question.trim().is_empty() {
            if let Some(details_ref) = details.as_ref() {
                market.question = details_ref.question.clone();
            }
        }
        if market.end_date_iso.is_none() {
            if let Some(details_ref) = details.as_ref() {
                let end_iso = details_ref.end_date_iso.trim();
                if !end_iso.is_empty() {
                    market.end_date_iso = Some(end_iso.to_string());
                    market.end_date_iso_alt = Some(end_iso.to_string());
                    market.end_date = Some(end_iso.to_string());
                }
            }
        }
        return Ok((market, details));
    }

    let Some(condition_id) = normalized_condition_id.as_deref() else {
        anyhow::bail!("condition_id missing");
    };
    let details = api
        .get_market(condition_id)
        .await
        .with_context(|| format!("failed to resolve condition_id={}", condition_id))?;
    Ok((market_from_details(&details), Some(details)))
}

fn market_query_limit(limit: Option<usize>) -> usize {
    limit
        .unwrap_or(MANUAL_MARKET_SEARCH_LIMIT_DEFAULT)
        .clamp(1, MANUAL_MARKET_SEARCH_LIMIT_MAX)
}

fn normalize_search_query(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn search_tokens(raw: &str) -> Vec<String> {
    raw.split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

fn market_match_score(query: &str, market: &UiMarket) -> Option<i32> {
    let normalized_query = query.trim().to_ascii_lowercase();
    if normalized_query.is_empty() {
        return None;
    }

    let title = market.title.to_ascii_lowercase();
    let slug = market.market_slug.to_ascii_lowercase();
    let subtitle = market.subtitle.to_ascii_lowercase();
    let description = market
        .description
        .as_deref()
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    let symbol = market
        .symbol
        .as_deref()
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    let timeframe = market
        .timeframe
        .as_deref()
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    let combined = format!("{title} {slug} {subtitle} {description} {symbol} {timeframe}");

    let tokens = search_tokens(normalized_query.as_str());
    if tokens.is_empty() {
        return None;
    }

    let query_hits = combined.contains(normalized_query.as_str());
    let all_token_hits = tokens.iter().all(|token| combined.contains(token.as_str()));
    if !query_hits && !all_token_hits {
        return None;
    }

    let mut score = 0_i32;
    if slug == normalized_query {
        score += 2_000;
    }
    if title == normalized_query {
        score += 1_900;
    }
    if symbol == normalized_query {
        score += 1_700;
    }
    if timeframe == normalized_query {
        score += 1_400;
    }
    if title.starts_with(normalized_query.as_str()) {
        score += 800;
    }
    if title.contains(normalized_query.as_str()) {
        score += 600;
    }
    if slug.contains(normalized_query.as_str()) {
        score += 500;
    }
    if subtitle.contains(normalized_query.as_str()) {
        score += 250;
    }
    if description.contains(normalized_query.as_str()) {
        score += 150;
    }
    score += tokens
        .iter()
        .filter(|token| title.contains(token.as_str()))
        .count() as i32
        * 60;
    score += tokens
        .iter()
        .filter(|token| slug.contains(token.as_str()))
        .count() as i32
        * 40;
    score += i32::from(market.tradable) * 25;
    Some(score.max(1))
}

fn push_unique_market(
    markets: &mut Vec<UiMarket>,
    seen: &mut HashSet<String>,
    market: UiMarket,
    limit: usize,
) {
    if markets.len() >= limit {
        return;
    }
    let key = market.condition_id.trim().to_ascii_lowercase();
    if key.is_empty() || !seen.insert(key) {
        return;
    }
    markets.push(market);
}

fn market_lookup_error(error: anyhow::Error) -> ApiError {
    let message = error.to_string();
    let normalized = message.to_ascii_lowercase();
    let status = if normalized.contains("not found")
        || normalized.contains("does not contain market slug")
        || normalized.contains("invalid market response format")
    {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    ApiError::new(status, message)
}

async fn search_manual_markets(
    state: &AppState,
    query: &str,
    limit: usize,
) -> Result<Vec<UiMarket>, ApiError> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    for page_index in 0..MANUAL_MARKET_SEARCH_MAX_PAGES {
        let offset = page_index.saturating_mul(MANUAL_MARKET_SEARCH_PAGE_SIZE);
        let page = state
            .api
            .get_all_active_markets_page(MANUAL_MARKET_SEARCH_PAGE_SIZE, offset)
            .await
            .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
        if page.is_empty() {
            break;
        }

        for market in page {
            let normalized = build_ui_market(&market, None);
            let Some(score) = market_match_score(query, &normalized) else {
                continue;
            };
            let key = normalized.condition_id.trim().to_ascii_lowercase();
            if key.is_empty() || !seen.insert(key) {
                continue;
            }
            candidates.push((score, normalized));
        }

        if candidates.len() >= limit.saturating_mul(2) {
            break;
        }
    }

    candidates.sort_by(|a, b| {
        b.0.cmp(&a.0).then_with(|| {
            a.1.title
                .to_ascii_lowercase()
                .cmp(&b.1.title.to_ascii_lowercase())
        })
    });

    Ok(candidates
        .into_iter()
        .take(limit)
        .map(|(_, market)| market)
        .collect())
}

async fn collect_recent_manual_markets(
    state: &AppState,
    limit: usize,
) -> Result<Vec<UiMarket>, ApiError> {
    let mut markets = Vec::new();
    let mut seen = HashSet::new();

    let mut snapshots = collect_run_snapshots(state).await;
    snapshots.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));
    for snapshot in snapshots {
        if markets.len() >= limit {
            break;
        }
        if let Ok((market, details)) = resolve_manual_market_request(
            state.api.as_ref(),
            Some(snapshot.condition_id.as_str()),
            Some(snapshot.market_slug.as_str()),
        )
        .await
        {
            push_unique_market(
                &mut markets,
                &mut seen,
                build_ui_market(&market, details.as_ref()),
                limit,
            );
        }
    }

    if markets.len() < limit {
        if let Ok(rows) = state.api.get_wallet_positions_live(None).await {
            for row in rows {
                if markets.len() >= limit {
                    break;
                }
                let Some(condition_id) =
                    json_value_as_string(&row, &["conditionId", "condition_id"])
                else {
                    continue;
                };
                let market_slug = json_value_as_string(&row, &["slug", "market_slug"]);
                if let Ok((market, details)) = resolve_manual_market_request(
                    state.api.as_ref(),
                    Some(condition_id.as_str()),
                    market_slug.as_deref(),
                )
                .await
                {
                    push_unique_market(
                        &mut markets,
                        &mut seen,
                        build_ui_market(&market, details.as_ref()),
                        limit,
                    );
                }
            }
        }
    }

    if markets.len() < limit {
        let fallback = state
            .api
            .get_all_active_markets_page(limit.saturating_sub(markets.len()) as u32, 0)
            .await
            .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
        for market in fallback {
            if markets.len() >= limit {
                break;
            }
            push_unique_market(
                &mut markets,
                &mut seen,
                build_ui_market(&market, None),
                limit,
            );
        }
    }

    Ok(markets)
}

fn manual_run_kind(snapshot: &ManualRunSnapshot) -> &'static str {
    if manual_run_matches_action(snapshot, ManualOrderAction::Close) {
        "close"
    } else {
        "open"
    }
}

fn manual_run_mode(snapshot: &ManualRunSnapshot) -> &'static str {
    if snapshot.run_type.ends_with("_market") {
        "market"
    } else if snapshot.run_type.ends_with("_limit") {
        "limit"
    } else {
        "chase_limit"
    }
}

fn manual_run_side(snapshot: &ManualRunSnapshot, market: Option<&UiMarket>) -> String {
    if let Some(ui_market) = market {
        if let Some(side) = ui_market.sides.iter().find(|side| {
            side.token_id
                .eq_ignore_ascii_case(snapshot.token_id.as_str())
        }) {
            return side.outcome.clone();
        }
    }
    "Unknown".to_string()
}

fn build_ui_manual_run(snapshot: &ManualRunSnapshot, market: Option<&UiMarket>) -> UiManualRun {
    let kind = manual_run_kind(snapshot).to_string();
    let status = snapshot.status.trim().to_ascii_lowercase();
    let side = manual_run_side(snapshot, market);
    let mode = manual_run_mode(snapshot).to_string();
    let target_shares = snapshot.target_shares.max(0.0);
    let filled_shares = snapshot.filled_shares.max(0.0);
    let remaining_shares = (target_shares - filled_shares).max(0.0);
    let progress_ratio = if target_shares > 0.0 {
        (filled_shares / target_shares).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let progress_summary = format!(
        "{:.4} / {:.4} shares",
        filled_shares.max(0.0),
        target_shares.max(0.0)
    );

    UiManualRun {
        run_id: snapshot.run_id.clone(),
        kind: kind.clone(),
        kind_label: manual_run_kind_label(kind.as_str()).to_string(),
        status: status.clone(),
        status_label: manual_run_status_label(status.as_str()).to_string(),
        condition_id: snapshot.condition_id.clone(),
        market_slug: snapshot.market_slug.clone(),
        market_title: market
            .map(|value| value.title.clone())
            .unwrap_or_else(|| snapshot.market_slug.replace('-', " ")),
        market_subtitle: market
            .map(|value| value.subtitle.clone())
            .unwrap_or_else(|| snapshot.timeframe.clone()),
        side: side.clone(),
        side_label: manual_side_label(side.as_str()),
        mode: mode.clone(),
        mode_label: manual_mode_label(mode.as_str()).to_string(),
        target_shares,
        filled_shares,
        remaining_shares,
        requested_size_usd: snapshot.requested_size_usd,
        progress_ratio,
        progress_summary,
        started_at_ms: snapshot.started_at_ms,
        updated_at_ms: snapshot.updated_at_ms,
        finished_at_ms: snapshot.finished_at_ms,
        post_only: snapshot.post_only,
        tp_price: snapshot.tp_price,
        error_message: snapshot.last_error.clone(),
    }
}

async fn resolve_ui_market_cached(
    state: &AppState,
    cache: &mut HashMap<String, UiMarket>,
    condition_id: &str,
    market_slug: &str,
) -> Option<UiMarket> {
    let key = condition_id.trim().to_ascii_lowercase();
    if key.is_empty() {
        return None;
    }
    if let Some(market) = cache.get(key.as_str()) {
        return Some(market.clone());
    }
    let resolved = if let Ok(value) =
        resolve_manual_market_request(state.api.as_ref(), Some(condition_id), Some(market_slug))
            .await
    {
        value
    } else {
        resolve_manual_market_request(state.api.as_ref(), Some(condition_id), None)
            .await
            .ok()?
    };
    let ui_market = build_ui_market(&resolved.0, resolved.1.as_ref());
    cache.insert(key, ui_market.clone());
    Some(ui_market)
}

async fn enrich_manual_runs(state: &AppState, runs: &[ManualRunSnapshot]) -> Vec<UiManualRun> {
    let mut cache = HashMap::new();
    let mut out = Vec::with_capacity(runs.len());
    for snapshot in runs {
        let market = resolve_ui_market_cached(
            state,
            &mut cache,
            snapshot.condition_id.as_str(),
            snapshot.market_slug.as_str(),
        )
        .await;
        out.push(build_ui_manual_run(snapshot, market.as_ref()));
    }
    out
}

fn ui_position_side(row: &Value, market: Option<&UiMarket>) -> String {
    if let Some(raw) = json_value_as_string(row, &["outcome", "side", "position_side"]) {
        return raw;
    }
    if let Some(token_id) = json_value_as_string(row, &["asset", "token_id", "tokenId"]) {
        if let Some(ui_market) = market {
            if let Some(side) = ui_market
                .sides
                .iter()
                .find(|side| side.token_id.eq_ignore_ascii_case(token_id.as_str()))
            {
                return side.outcome.clone();
            }
        }
    }
    "Unknown".to_string()
}

fn build_ui_manual_position(row: &Value, market: Option<&UiMarket>) -> UiManualPosition {
    let side = ui_position_side(row, market);
    UiManualPosition {
        condition_id: json_value_as_string(row, &["conditionId", "condition_id"])
            .unwrap_or_default(),
        market_slug: market
            .map(|value| value.market_slug.clone())
            .or_else(|| json_value_as_string(row, &["slug", "market_slug"]))
            .unwrap_or_default(),
        market_title: market
            .map(|value| value.title.clone())
            .or_else(|| json_value_as_string(row, &["title", "question", "market_title"]))
            .unwrap_or_else(|| "Market".to_string()),
        market_subtitle: market
            .map(|value| value.subtitle.clone())
            .unwrap_or_else(|| "".to_string()),
        side: side.clone(),
        side_label: manual_side_label(side.as_str()),
        size: json_value_as_f64(row, &["size", "position_size"]).unwrap_or(0.0),
        entry_price: json_value_as_f64(row, &["avgPrice", "average_price", "entry_price"]),
        current_price: json_value_as_f64(row, &["currentPrice", "current_price", "mark_price"]),
        realized_pnl: json_value_as_f64(row, &["realizedPnl", "realized_pnl"]),
        unrealized_pnl: json_value_as_f64(row, &["cashPnl", "cash_pnl"]),
        redeemable: json_value_as_bool(row, &["redeemable"]).unwrap_or(false),
        mergeable: json_value_as_bool(row, &["mergeable"]).unwrap_or(false),
    }
}

async fn enrich_manual_positions(state: &AppState, rows: &[Value]) -> Vec<UiManualPosition> {
    let mut cache = HashMap::new();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let condition_id =
            json_value_as_string(row, &["conditionId", "condition_id"]).unwrap_or_default();
        let market_slug = json_value_as_string(row, &["slug", "market_slug"]).unwrap_or_default();
        let market = if condition_id.is_empty() {
            None
        } else {
            resolve_ui_market_cached(
                state,
                &mut cache,
                condition_id.as_str(),
                market_slug.as_str(),
            )
            .await
        };
        out.push(build_ui_manual_position(row, market.as_ref()));
    }
    out
}

fn decimal_to_human_usdc(value: Decimal) -> f64 {
    f64::try_from(value / Decimal::from(1_000_000u64))
        .unwrap_or(0.0)
        .max(0.0)
}

fn manual_price_from_quote(
    quote: Option<&TokenPrice>,
    action: ManualOrderAction,
    mode: ManualOrderMode,
    post_only: bool,
    explicit_price: Option<f64>,
    min_price: f64,
    max_price: f64,
    slippage_cents: f64,
    tick_size: f64,
) -> Option<f64> {
    if let Some(px) = explicit_price.filter(|v| v.is_finite() && *v > 0.0) {
        return Some(px.clamp(min_price, max_price));
    }
    let best_bid = quote
        .and_then(|q| q.bid)
        .and_then(|v| f64::try_from(v).ok())
        .filter(|v| v.is_finite() && *v > 0.0);
    let best_ask = quote
        .and_then(|q| q.ask)
        .and_then(|v| f64::try_from(v).ok())
        .filter(|v| v.is_finite() && *v > 0.0);
    let slip = (slippage_cents.max(0.0) / 100.0).min(0.5);
    let raw = match (action, mode) {
        (ManualOrderAction::Open, ManualOrderMode::ChaseLimit) => {
            if post_only {
                best_bid.or_else(|| {
                    best_ask.map(|ask| (ask - tick_size.max(0.000_001)).max(MANUAL_MIN_PRICE))
                })
            } else {
                best_ask.or(best_bid)
            }
        }
        (ManualOrderAction::Close, ManualOrderMode::ChaseLimit) => {
            if post_only {
                best_ask
                    .or(best_bid.map(|bid| (bid + tick_size.max(0.000_001)).min(MANUAL_MAX_PRICE)))
            } else {
                best_bid.or(best_ask)
            }
        }
        (ManualOrderAction::Open, ManualOrderMode::Limit) => {
            if post_only {
                best_bid.or(best_ask)
            } else {
                best_ask.or(best_bid)
            }
        }
        (ManualOrderAction::Close, ManualOrderMode::Limit) => best_ask.or(best_bid),
        (ManualOrderAction::Open, ManualOrderMode::Market) => best_ask
            .or(best_bid)
            .map(|p| (p + slip).min(MANUAL_MAX_PRICE)),
        (ManualOrderAction::Close, ManualOrderMode::Market) => best_bid
            .or(best_ask)
            .map(|p| (p - slip).max(MANUAL_MIN_PRICE)),
    }?;
    Some(raw.clamp(min_price, max_price))
}

fn should_replace_for_manual(
    action: ManualOrderAction,
    working_price: Option<f64>,
    target_price: f64,
    min_price: f64,
    max_price: f64,
    now_ms: i64,
    last_replace_ms: i64,
    replaces_this_sec: u32,
    chase_cfg: AdaptiveChaseConfig,
    tick_size: f64,
) -> bool {
    let transformed = match action {
        ManualOrderAction::Open => {
            let max_live = max_price.clamp(min_price, MANUAL_MAX_PRICE);
            (working_price, target_price, max_live)
        }
        ManualOrderAction::Close => {
            let inv_working =
                working_price.map(|w| (1.0 - w).clamp(MANUAL_MIN_PRICE, MANUAL_MAX_PRICE));
            let inv_target = (1.0 - target_price).clamp(MANUAL_MIN_PRICE, MANUAL_MAX_PRICE);
            let inv_max = (1.0 - min_price).clamp(MANUAL_MIN_PRICE, MANUAL_MAX_PRICE);
            (inv_working, inv_target, inv_max)
        }
    };
    matches!(
        decide_replace_action(ReplacePolicyInput {
            working_price: transformed.0,
            target_bid: transformed.1,
            max_buy_live: transformed.2,
            now_ms,
            last_replace_ms,
            min_replace_interval_ms: chase_cfg.min_replace_interval_ms.max(50),
            min_price_improve_ticks: chase_cfg.min_price_improve_ticks.max(1),
            tick_size: tick_size.max(0.000_001),
            replaces_this_sec,
            max_replaces_per_sec: chase_cfg.max_replaces_per_sec.max(1),
        }),
        ReplaceAction::Place | ReplaceAction::Replace
    )
}

fn manual_order_lookup_error_is_terminal(error_text: &str) -> bool {
    let lower = error_text.to_ascii_lowercase();
    lower.contains("404")
        || lower.contains("not found")
        || lower.contains("unknown order")
        || lower.contains("does not exist")
}

fn manual_terminal_status_for_order(order_status: OrderStatusType) -> Option<&'static str> {
    match order_status {
        OrderStatusType::Matched => Some("FILLED"),
        OrderStatusType::Canceled | OrderStatusType::Unmatched => Some("CANCELED"),
        _ => None,
    }
}

fn manual_order_matched_units(order: &OpenOrderResponse) -> f64 {
    f64::try_from(order.size_matched)
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(0.0)
}

fn to_token_shares(balance: Decimal) -> f64 {
    f64::try_from(balance / Decimal::from(1_000_000u64))
        .unwrap_or(0.0)
        .max(0.0)
}

fn trim_decimal_string(value: f64, max_dp: usize) -> String {
    let mut s = format!("{:.*}", max_dp, value);
    if let Some(dot_idx) = s.find('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
        if s.len() == dot_idx {
            s.push('0');
        }
    }
    s
}

fn extract_order_id_from_text(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|token| token.trim_matches(|c: char| c == ',' || c == '.' || c == ';'))
        .find(|token| token.starts_with("0x") && token.len() >= 12)
        .map(ToString::to_string)
}

async fn token_shares_with_fallback(api: &PolymarketApi, token_id: &str) -> Result<f64> {
    if let Ok(balance) = api.check_balance_only(token_id).await {
        return Ok(to_token_shares(balance));
    }
    let rows = api.get_wallet_positions_live(None).await?;
    let mut shares = 0.0_f64;
    for row in rows {
        let is_target = row
            .get("asset")
            .and_then(Value::as_str)
            .map(|asset| asset == token_id)
            .unwrap_or(false);
        if !is_target {
            continue;
        }
        let size = mm_activity_value_as_f64(row.get("size"))
            .filter(|v| v.is_finite())
            .unwrap_or(0.0);
        shares += size.max(0.0);
    }
    Ok(shares.max(0.0))
}

fn check_auth(headers: &HeaderMap, state: &AppState) -> Result<(), ApiError> {
    let Some(expected) = state.auth_token.as_ref() else {
        return Ok(());
    };
    let provided = headers
        .get("x-evpoly-manual-token")
        .or_else(|| headers.get("x-evpoly-admin-token"))
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .unwrap_or_default();
    if provided != expected {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized"));
    }
    Ok(())
}

fn ensure_write_allowed(state: &AppState) -> Result<(), ApiError> {
    if state.simulation_mode {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "write endpoints disabled when manual_bot is started with --simulation",
        ));
    }
    Ok(())
}

fn strategy_id_for_manual_run(run: &ManualRunSnapshot) -> String {
    let is_close = run.run_type.starts_with("close_") || run.run_id.starts_with("manual_close:");
    if is_close {
        if run.post_only {
            "manual_close_limit_v1".to_string()
        } else {
            "manual_close_limit_taker_v1".to_string()
        }
    } else if run.post_only {
        STRATEGY_ID_MANUAL_CHASE_LIMIT_V1.to_string()
    } else {
        STRATEGY_ID_MANUAL_CHASE_LIMIT_TAKER_V1.to_string()
    }
}

fn side_for_manual_run(run: &ManualRunSnapshot) -> &'static str {
    if run.run_type.starts_with("close_") || run.run_id.starts_with("manual_close:") {
        "SELL"
    } else {
        "BUY"
    }
}

fn manual_run_is_active_for_scope(snapshot: &ManualRunSnapshot) -> bool {
    snapshot.status.eq_ignore_ascii_case("running")
        || snapshot.status.eq_ignore_ascii_case("stopping")
}

fn manual_scope_key(action: ManualOrderAction, condition_id: &str, token_id: &str) -> String {
    format!(
        "{}:{}:{}",
        action.as_str(),
        condition_id.trim().to_ascii_lowercase(),
        token_id.trim()
    )
}

async fn collect_run_snapshots(state: &AppState) -> Vec<ManualRunSnapshot> {
    let runs = {
        let registry = state.run_registry.read().await;
        registry.values().cloned().collect::<Vec<_>>()
    };
    let mut snapshots = Vec::with_capacity(runs.len());
    for run in runs {
        snapshots.push(run.snapshot().await);
    }
    snapshots.sort_by(|a, b| b.started_at_ms.cmp(&a.started_at_ms));
    snapshots
}

async fn find_run_handles(
    state: &AppState,
    requested_run_ids: Option<&Vec<String>>,
) -> (Vec<Arc<ManualRun>>, Vec<String>) {
    let registry = state.run_registry.read().await;
    let mut missing = Vec::new();
    let selected = if let Some(run_ids) = requested_run_ids {
        let mut out = Vec::new();
        for run_id in run_ids {
            let normalized = run_id.trim();
            if normalized.is_empty() {
                continue;
            }
            if let Some(run) = registry.get(normalized) {
                out.push(run.clone());
            } else {
                missing.push(normalized.to_string());
            }
        }
        out
    } else {
        registry.values().cloned().collect::<Vec<_>>()
    };
    (selected, missing)
}

async fn run_manual_premarket_submit(
    trader: Arc<Trader>,
    api: Arc<PolymarketApi>,
    request: ManualPremarketRequest,
) -> Result<Value> {
    let side_mode = ManualSideMode::from_raw(request.side.as_deref());
    let (market, market_details) = resolve_manual_market_request(
        api.as_ref(),
        request.condition_id.as_deref(),
        request.market_slug.as_deref(),
    )
    .await?;

    let mut up_token_id = nonempty_owned(request.up_token_id.clone())
        .or_else(|| select_token_id_for_direction(&market, Direction::Up))
        .or_else(|| {
            market_details
                .as_ref()
                .and_then(|details| select_token_id_from_market_details(details, Direction::Up))
        });
    let mut down_token_id = nonempty_owned(request.down_token_id.clone())
        .or_else(|| select_token_id_for_direction(&market, Direction::Down))
        .or_else(|| {
            market_details
                .as_ref()
                .and_then(|details| select_token_id_from_market_details(details, Direction::Down))
        });

    if side_mode.include_up() && up_token_id.is_none() {
        anyhow::bail!(
            "unable to resolve UP/YES token_id for condition {}",
            market.condition_id
        );
    }
    if side_mode.include_down() && down_token_id.is_none() {
        anyhow::bail!(
            "unable to resolve DOWN/NO token_id for condition {}",
            market.condition_id
        );
    }

    let symbol = nonempty_owned(request.symbol.clone())
        .map(|value| normalize_market_symbol(value.as_str()))
        .unwrap_or_else(|| infer_symbol_from_market(&market));
    let post_only = request.post_only.unwrap_or(true);
    let strategy_id = if post_only {
        STRATEGY_ID_MANUAL_PREMARKET_V1
    } else {
        STRATEGY_ID_MANUAL_PREMARKET_TAKER_V1
    };
    let force_fak = request
        .order_type
        .as_deref()
        .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "fak" | "ioc"))
        .unwrap_or(false);

    let now_ts = Utc::now().timestamp().max(0) as u64;
    let period_timestamp = request
        .period_timestamp
        .filter(|ts| *ts > now_ts)
        .unwrap_or(now_ts.saturating_add(300));
    let target_size_usd = request
        .target_size_usd
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or_else(premarket_side_budget_usd);
    if target_size_usd <= 0.0 {
        anyhow::bail!("target_size_usd must be positive");
    }

    let order_floor = match api.get_market_constraints(&market.condition_id).await {
        Ok(snapshot) => PremarketOrderFloor::from_snapshot(snapshot),
        Err(_) => PremarketOrderFloor::fallback_default(),
    };
    let sized_rungs = build_premarket_fixed_rungs(target_size_usd, order_floor);
    if sized_rungs.is_empty() {
        anyhow::bail!("manual premarket produced no valid ladder rungs");
    }

    let mut sides: Vec<(Direction, String, &'static str)> = Vec::new();
    if side_mode.include_up() {
        if let Some(token_id) = up_token_id.take() {
            sides.push((Direction::Up, token_id, "up"));
        }
    }
    if side_mode.include_down() {
        if let Some(token_id) = down_token_id.take() {
            sides.push((Direction::Down, token_id, "down"));
        }
    }
    if sides.is_empty() {
        anyhow::bail!("manual premarket has no token_ids to submit");
    }

    let mut total_attempts = 0usize;
    let mut successful_submits = 0usize;
    let mut no_fill_submits = 0usize;
    let mut submit_errors: Vec<String> = Vec::new();
    let mut token_ids: Vec<String> = Vec::new();

    for (direction, token_id, side_label) in sides.iter() {
        token_ids.push(token_id.clone());
        let token_type = token_type_for_market_symbol(symbol.as_str(), *direction);
        let mut side_balance_before = token_shares_with_fallback(api.as_ref(), token_id.as_str())
            .await
            .unwrap_or(0.0);

        for (rung_index, rung) in sized_rungs.iter().enumerate() {
            let units = rung.size_usd / rung.price.max(0.000_001);
            if !units.is_finite() || units <= 0.0 {
                continue;
            }
            total_attempts = total_attempts.saturating_add(1);
            let now_ms = Utc::now().timestamp_millis();
            let mut request_id = format!(
                "manual_premarket:{}:{}:{}:l{}:{}:{}",
                market.condition_id,
                period_timestamp,
                side_label,
                rung_index + 1,
                now_ms,
                next_manual_request_id_nonce()
            );
            if force_fak {
                request_id.push_str(":fak");
            }
            let opportunity = BuyOpportunity {
                condition_id: market.condition_id.clone(),
                token_id: token_id.clone(),
                token_type: token_type.clone(),
                bid_price: rung.price,
                expected_edge_bps: ((0.50 - rung.price) * 10_000.0).max(0.0),
                expected_fill_prob: (0.20 + rung.price.clamp(0.0, 1.0) * 0.60).clamp(0.05, 0.95),
                period_timestamp,
                time_remaining_seconds: Timeframe::M5.duration_seconds() as u64,
                time_elapsed_seconds: 0,
                use_market_order: false,
            };
            if let Err(e) = trader
                .execute_limit_buy(
                    &opportunity,
                    EntryExecutionMode::Ladder,
                    false,
                    Some(units),
                    Some("5m"),
                    Some(strategy_id),
                    Some(request_id.as_str()),
                )
                .await
            {
                submit_errors.push(e.to_string());
            } else {
                if force_fak {
                    match token_shares_with_fallback(api.as_ref(), token_id.as_str()).await {
                        Ok(side_balance_after) => {
                            let filled_delta = (side_balance_after - side_balance_before).max(0.0);
                            if filled_delta > 0.000_001 {
                                successful_submits = successful_submits.saturating_add(1);
                                side_balance_before = side_balance_after;
                            } else {
                                no_fill_submits = no_fill_submits.saturating_add(1);
                            }
                        }
                        Err(e) => {
                            submit_errors.push(format!("post_submit_balance_check_failed: {}", e));
                        }
                    }
                } else {
                    successful_submits = successful_submits.saturating_add(1);
                }
            }
        }
    }

    token_ids.sort();
    token_ids.dedup();

    Ok(json!({
        "ok": submit_errors.is_empty(),
        "strategy_id": strategy_id,
        "condition_id": market.condition_id,
        "market_slug": market.slug,
        "symbol": symbol,
        "period_timestamp": period_timestamp,
        "target_size_usd_per_side": target_size_usd,
        "rungs_per_side": sized_rungs.len(),
        "sides": token_ids,
        "attempted_orders": total_attempts,
        "successful_orders": successful_submits,
        "no_fill_orders": no_fill_submits,
        "failed_orders": submit_errors.len(),
        "errors": submit_errors,
        "notes": [
            "standalone manual bot does not schedule premarket cancel stages by default"
        ]
    }))
}

async fn place_tp_limit_sell(
    api: Arc<PolymarketApi>,
    token_id: &str,
    shares: f64,
    tp_price: f64,
    post_only: bool,
) -> Result<Value> {
    let clamped_shares = shares.max(0.0);
    if !clamped_shares.is_finite() || clamped_shares <= 0.0 {
        anyhow::bail!("tp sell skipped: no positive filled shares");
    }
    if !tp_price.is_finite() || tp_price <= 0.0 || tp_price >= 1.0 {
        anyhow::bail!("tp_price must be > 0 and < 1.0");
    }

    let tp_size_shares = (clamped_shares * 100.0).floor() / 100.0;
    if !tp_size_shares.is_finite() || tp_size_shares < 0.01 {
        anyhow::bail!(
            "tp sell skipped: rounded share size below minimum (raw={} rounded={})",
            clamped_shares,
            tp_size_shares
        );
    }

    let order = OrderRequest {
        token_id: token_id.to_string(),
        side: "SELL".to_string(),
        size: format!("{:.2}", tp_size_shares),
        price: trim_decimal_string(tp_price, 6),
        order_type: "GTC".to_string(),
        expiration_ts: None,
        post_only: Some(post_only),
    };

    let response = api.place_order(&order).await?;
    let order_id = response.order_id.clone().or_else(|| {
        response
            .message
            .as_deref()
            .and_then(extract_order_id_from_text)
    });
    Ok(json!({
        "requested_shares": clamped_shares,
        "submitted_shares": tp_size_shares,
        "price": tp_price,
        "post_only": post_only,
        "order_id": order_id,
        "status": response.status,
        "message": response.message,
    }))
}

fn manual_tp_error_is_retryable(error: &anyhow::Error) -> bool {
    let msg = format!("{:#}", error).to_ascii_lowercase();
    msg.contains("not enough balance / allowance")
        || msg.contains("insufficient allowance")
        || msg.contains("insufficient balance")
        || msg.contains("allowance")
        || msg.contains("balance")
        || msg.contains("429")
        || msg.contains("rate limit")
        || msg.contains("timeout")
}

async fn submit_manual_sell(
    api: Arc<PolymarketApi>,
    token_id: &str,
    shares: f64,
    price: f64,
    post_only: bool,
    force_fak: bool,
) -> Result<Value> {
    let clamped_shares = shares.max(0.0);
    if !clamped_shares.is_finite() || clamped_shares <= 0.0 {
        anyhow::bail!("sell skipped: no positive shares");
    }
    if !price.is_finite() || price <= 0.0 || price >= 1.0 {
        anyhow::bail!("sell price must be > 0 and < 1");
    }
    let size_shares = (clamped_shares * 100.0).floor() / 100.0;
    if !size_shares.is_finite() || size_shares < 0.01 {
        anyhow::bail!("sell skipped: rounded shares too small");
    }
    let order = OrderRequest {
        token_id: token_id.to_string(),
        side: "SELL".to_string(),
        size: format!("{:.2}", size_shares),
        price: trim_decimal_string(price, 6),
        order_type: if force_fak {
            "FAK".to_string()
        } else {
            "GTC".to_string()
        },
        expiration_ts: None,
        post_only: Some(if force_fak { false } else { post_only }),
    };
    let _ = api.update_balance_allowance_for_sell(token_id).await;
    let response = api.place_order(&order).await?;
    let order_id = response.order_id.clone().or_else(|| {
        response
            .message
            .as_deref()
            .and_then(extract_order_id_from_text)
    });
    Ok(json!({
        "order_id": order_id,
        "status": response.status,
        "message": response.message,
        "submitted_shares": size_shares,
        "price": price,
    }))
}

async fn submit_manual_buy(
    api: Arc<PolymarketApi>,
    token_id: &str,
    shares: f64,
    price: f64,
    post_only: bool,
    force_fak: bool,
) -> Result<Value> {
    let clamped_shares = shares.max(0.0);
    if !clamped_shares.is_finite() || clamped_shares <= 0.0 {
        anyhow::bail!("buy skipped: no positive shares");
    }
    if !price.is_finite() || price <= 0.0 || price >= 1.0 {
        anyhow::bail!("buy price must be > 0 and < 1");
    }
    let size_shares = (clamped_shares * 100.0).floor() / 100.0;
    if !size_shares.is_finite() || size_shares < 0.01 {
        anyhow::bail!("buy skipped: rounded shares too small");
    }
    let order = OrderRequest {
        token_id: token_id.to_string(),
        side: "BUY".to_string(),
        size: format!("{:.2}", size_shares),
        price: trim_decimal_string(price, 6),
        order_type: if force_fak {
            "FAK".to_string()
        } else {
            "GTC".to_string()
        },
        expiration_ts: None,
        post_only: Some(if force_fak { false } else { post_only }),
    };
    let response = api.place_order(&order).await?;
    let order_id = response.order_id.clone().or_else(|| {
        response
            .message
            .as_deref()
            .and_then(extract_order_id_from_text)
    });
    Ok(json!({
        "order_id": order_id,
        "status": response.status,
        "message": response.message,
        "submitted_shares": size_shares,
        "price": price,
    }))
}

async fn start_manual_order(
    state: AppState,
    request: ManualOrderRequest,
    action: ManualOrderAction,
) -> Result<Value> {
    let mode = ManualOrderMode::parse(request.mode.as_deref());
    let default_post_only = !matches!(mode, ManualOrderMode::Market);
    let mut post_only = request.post_only.unwrap_or(default_post_only);
    if matches!(mode, ManualOrderMode::Market) {
        post_only = false;
    }

    let force_fak = matches!(mode, ManualOrderMode::Market)
        || request
            .order_type
            .as_deref()
            .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "fak" | "ioc"))
            .unwrap_or(false);

    let min_price = request
        .min_price
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(MANUAL_MIN_PRICE)
        .clamp(MANUAL_MIN_PRICE, MANUAL_MAX_PRICE);
    let max_price = request
        .max_price
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(MANUAL_MAX_PRICE)
        .clamp(MANUAL_MIN_PRICE, MANUAL_MAX_PRICE);
    if min_price > max_price {
        anyhow::bail!("min_price must be <= max_price");
    }

    let duration_sec = request
        .max_duration_sec
        .or_else(|| request.duration_min.map(|m| m.saturating_mul(60)))
        .unwrap_or(3_600)
        .clamp(10, 24 * 60 * 60);

    let side_mode = ManualSideMode::from_raw(request.side.as_deref());
    let direction = match side_mode {
        ManualSideMode::Up => Direction::Up,
        ManualSideMode::Down => Direction::Down,
        ManualSideMode::Both => anyhow::bail!("manual/open and manual/close require single side"),
    };

    let (market, market_details) = resolve_manual_market_request(
        state.api.as_ref(),
        request.condition_id.as_deref(),
        request.market_slug.as_deref(),
    )
    .await?;

    let token_id = if let Some(token_id) = nonempty_owned(request.token_id.clone()) {
        token_id
    } else {
        select_token_id_for_direction(&market, direction)
            .or_else(|| {
                market_details
                    .as_ref()
                    .and_then(|details| select_token_id_from_market_details(details, direction))
            })
            .ok_or_else(|| anyhow::anyhow!("unable to resolve token_id for requested side"))?
    };

    let symbol = nonempty_owned(request.symbol.clone())
        .map(|value| normalize_market_symbol(value.as_str()))
        .unwrap_or_else(|| infer_symbol_from_market(&market));
    let evcurve_reprice_for_crypto = manual_chase_use_evcurve_reprice_for_crypto()
        && is_crypto_price_market(symbol.as_str(), &market, market_details.as_ref())
        && matches!(mode, ManualOrderMode::ChaseLimit);

    let timeframe = request
        .timeframe
        .as_deref()
        .and_then(parse_timeframe_raw)
        .unwrap_or(Timeframe::M5);
    let source_timeframe = timeframe.as_str().to_string();

    let period_timestamp = request
        .period_timestamp
        .filter(|v| *v > 0)
        .unwrap_or_else(|| {
            let now = Utc::now().timestamp().max(0) as u64;
            now.saturating_add(duration_sec)
                .saturating_add(next_manual_request_id_nonce())
        });

    let chase_cfg = AdaptiveChaseConfig::from_env();
    let poll_ms = request
        .poll_ms
        .unwrap_or(
            chase_cfg
                .fallback_tick_ms
                .max(chase_cfg.min_replace_interval_ms)
                .max(50) as u64,
        )
        .clamp(50, 10_000);

    let tick_size = state
        .api
        .get_market_constraints(&market.condition_id)
        .await
        .ok()
        .and_then(|snapshot| f64::try_from(snapshot.minimum_tick_size).ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(0.01);

    let quote_now = state
        .api
        .get_best_price(token_id.as_str())
        .await
        .ok()
        .flatten();
    let slippage_cents = request.slippage_cents.unwrap_or(20.0).clamp(0.0, 50.0);
    let price_hint = if matches!(action, ManualOrderAction::Open)
        && matches!(mode, ManualOrderMode::Limit)
        && request.price.is_none()
    {
        let best_bid = quote_now
            .as_ref()
            .and_then(|q| q.bid.and_then(|v| f64::try_from(v).ok()))
            .filter(|v| v.is_finite() && *v > 0.0);
        let Some(best_bid) = best_bid else {
            anyhow::bail!(
                "unable to derive /manual/open limit price from best bid; provide explicit price or wait for quote/orderbook"
            );
        };
        best_bid.clamp(min_price, max_price)
    } else {
        match manual_price_from_quote(
            quote_now.as_ref(),
            action,
            mode,
            post_only,
            request.price,
            min_price,
            max_price,
            slippage_cents,
            tick_size,
        ) {
            Some(px) => px,
            None if matches!(action, ManualOrderAction::Close)
                && matches!(mode, ManualOrderMode::Limit) =>
            {
                anyhow::bail!(
                    "unable to derive /manual/close limit price from best ask; provide explicit price or wait for quote/orderbook"
                )
            }
            None => request
                .price
                .filter(|v| v.is_finite() && *v > 0.0)
                .unwrap_or(max_price)
                .clamp(min_price, max_price),
        }
    };

    let (initial_balance_shares, balance_tracking_warning) =
        match token_shares_with_fallback(state.api.as_ref(), token_id.as_str()).await {
            Ok(shares) => (shares, None),
            Err(e) => (
                0.0,
                Some(format!("initial_balance_unavailable: {}; fallback=0.0", e)),
            ),
        };

    let sizing_reference_price = match action {
        ManualOrderAction::Open => quote_now
            .as_ref()
            .and_then(|q| q.ask.and_then(|v| f64::try_from(v).ok()))
            .or_else(|| {
                quote_now
                    .as_ref()
                    .and_then(|q| q.bid.and_then(|v| f64::try_from(v).ok()))
            })
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(price_hint),
        ManualOrderAction::Close => quote_now
            .as_ref()
            .and_then(|q| q.bid.and_then(|v| f64::try_from(v).ok()))
            .or_else(|| {
                quote_now
                    .as_ref()
                    .and_then(|q| q.ask.and_then(|v| f64::try_from(v).ok()))
            })
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(price_hint),
    };

    let mut requested_size_usd = request.size_usd.filter(|v| v.is_finite() && *v > 0.0);
    let requested_size = request.size.filter(|v| v.is_finite() && *v > 0.0);
    if requested_size_usd.is_some() && requested_size.is_some() {
        anyhow::bail!("provide either size or size_usd (not both)");
    }
    let size_unit = if requested_size.is_some() {
        ManualOrderSizeUnit::parse(request.size_unit.as_deref())?
    } else {
        ManualOrderSizeUnit::Shares
    };

    let requested_target_shares = match request.target_shares {
        Some(target_shares) => {
            if !target_shares.is_finite() || target_shares <= 0.0 {
                anyhow::bail!("target_shares must be positive");
            }
            Some(target_shares)
        }
        None => None,
    };

    let mut target_notional_usd = requested_size_usd;
    let mut requested_shares_input = requested_target_shares;
    let mut target_shares = if let Some(target_shares) = requested_target_shares {
        target_shares
    } else if let Some(size) = requested_size {
        match size_unit {
            ManualOrderSizeUnit::Shares => {
                requested_shares_input = Some(size);
                size
            }
            ManualOrderSizeUnit::Usd => {
                requested_size_usd = Some(size);
                target_notional_usd = Some(size);
                size / sizing_reference_price.max(0.000_001)
            }
        }
    } else if let Some(size_usd) = requested_size_usd {
        size_usd / sizing_reference_price.max(0.000_001)
    } else {
        anyhow::bail!("provide target_shares, size, or size_usd");
    };

    let mut budget_warning: Option<String> = None;
    if matches!(action, ManualOrderAction::Open) {
        let (usdc_balance, usdc_allowance) = state
            .api
            .check_usdc_balance_allowance()
            .await
            .map_err(|e| anyhow::anyhow!("usdc_balance_check_failed: {:#}", e))?;
        let balance_usd = decimal_to_human_usdc(usdc_balance);
        let allowance_usd = decimal_to_human_usdc(usdc_allowance);
        let available_usd = if allowance_usd > 0.0 {
            balance_usd.min(allowance_usd)
        } else {
            budget_warning = Some(format!(
                "allowance_non_positive_fallback_to_balance balance_usd={:.4} allowance_usd={:.4}",
                balance_usd, allowance_usd
            ));
            balance_usd
        };
        if available_usd <= 0.0 {
            anyhow::bail!("insufficient pUSD balance/allowance for open");
        }
        let requested_usd = target_notional_usd.unwrap_or(target_shares * price_hint);
        if requested_usd > available_usd {
            target_shares = available_usd / price_hint.max(0.000_001);
            if target_notional_usd.is_some() {
                target_notional_usd = Some(available_usd);
            }
            budget_warning = Some(format!(
                "size_clamped_to_available_usdc requested_usd={:.4} available_usd={:.4}",
                requested_usd, available_usd
            ));
        }
    } else {
        if initial_balance_shares <= 0.0 {
            anyhow::bail!("no live position available to close");
        }
        if target_shares > initial_balance_shares {
            target_shares = initial_balance_shares;
            budget_warning = Some(format!(
                "target_clamped_to_live_position requested_shares={:.6} live_shares={:.6}",
                requested_shares_input.unwrap_or(target_shares),
                initial_balance_shares
            ));
        }
    }
    if !target_shares.is_finite() || target_shares <= 0.0 {
        anyhow::bail!("target_shares resolved to zero");
    }

    if matches!(action, ManualOrderAction::Close) && request.tp_price.is_some() {
        anyhow::bail!(
            "tp_price is not allowed for /manual/close; close endpoint is reduce-only SELL"
        );
    }
    let tp_price = request.tp_price;
    let fill_observe_max_ms = manual_chase_fill_observe_max_ms();
    let close_cancel_confirm_max_ms = manual_close_cancel_confirm_max_ms();
    let close_post_cancel_reconcile_ms = manual_close_post_cancel_reconcile_ms();

    let scope_key = manual_scope_key(action, market.condition_id.as_str(), token_id.as_str());
    {
        let existing_runs = {
            let registry = state.run_registry.read().await;
            registry.values().cloned().collect::<Vec<_>>()
        };
        for run in existing_runs {
            let snapshot = run.snapshot().await;
            let other_scope_key = manual_scope_key(
                if manual_run_matches_action(&snapshot, ManualOrderAction::Close) {
                    ManualOrderAction::Close
                } else {
                    ManualOrderAction::Open
                },
                snapshot.condition_id.as_str(),
                snapshot.token_id.as_str(),
            );
            if scope_key == other_scope_key && manual_run_is_active_for_scope(&snapshot) {
                anyhow::bail!(
                    "duplicate_running_scope scope_key={} existing_run_id={}",
                    scope_key,
                    snapshot.run_id
                );
            }
        }
    }

    let run_id = format!(
        "manual_{}:{}:{}:{}",
        action.as_str(),
        market.condition_id,
        Utc::now().timestamp_millis(),
        next_manual_request_id_nonce()
    );
    {
        let mut claims = state.scope_claims.write().await;
        if let Some(existing_run) = claims.get(scope_key.as_str()) {
            anyhow::bail!(
                "duplicate_running_scope scope_key={} existing_run_id={}",
                scope_key,
                existing_run
            );
        }
        claims.insert(scope_key.clone(), run_id.clone());
    }
    let strategy_id = match action {
        ManualOrderAction::Open => {
            if post_only {
                STRATEGY_ID_MANUAL_CHASE_LIMIT_V1.to_string()
            } else {
                STRATEGY_ID_MANUAL_CHASE_LIMIT_TAKER_V1.to_string()
            }
        }
        ManualOrderAction::Close => {
            if post_only {
                "manual_close_limit_v1".to_string()
            } else {
                "manual_close_limit_taker_v1".to_string()
            }
        }
    };

    let now_ms = Utc::now().timestamp_millis();
    let snapshot = ManualRunSnapshot {
        run_id: run_id.clone(),
        run_type: format!("{}_{}", action.as_str(), mode.as_str()),
        status: "running".to_string(),
        stop_reason: None,
        started_at_ms: now_ms,
        updated_at_ms: now_ms,
        finished_at_ms: None,
        condition_id: market.condition_id.clone(),
        market_slug: market.slug.clone(),
        token_id: token_id.clone(),
        timeframe: source_timeframe.clone(),
        period_timestamp,
        post_only,
        min_price,
        max_price,
        target_shares,
        requested_size_usd,
        filled_shares: 0.0,
        filled_notional_usd: 0.0,
        submit_attempts: 0,
        submit_success: 0,
        submit_errors: 0,
        canceled_orders: 0,
        last_submit_price: None,
        tp_price,
        tp_order_id: None,
        last_error: None,
    };
    let response_snapshot = snapshot.clone();
    let ui_market = build_ui_market(&market, market_details.as_ref());
    let ui_run = build_ui_manual_run(&response_snapshot, Some(&ui_market));

    let manual_run = Arc::new(ManualRun::new(snapshot));
    {
        let mut registry = state.run_registry.write().await;
        registry.insert(run_id.clone(), manual_run.clone());
    }

    let task_state = state.clone();
    let token_id_for_task = token_id.clone();
    let scope_key_for_task = scope_key.clone();
    let run_id_for_task = run_id.clone();

    tokio::spawn(async move {
        let mut stop_reason = "duration_elapsed".to_string();
        let started = Instant::now();
        let mut quote_failures = 0u64;
        let mut last_known_balance_shares = initial_balance_shares;
        let mut last_balance_poll_ms = 0_i64;
        let mut last_ws_user_msg_ms = task_state
            .polymarket_ws_state
            .as_ref()
            .map(|ws_state| ws_state.last_user_msg_ms())
            .unwrap_or(0);
        let mut max_filled_seen = 0.0_f64;
        let mut filled_accounted_shares = 0.0_f64;
        let mut filled_accounted_notional_usd = 0.0_f64;
        let mut reserved_inflight_shares = 0.0_f64;
        let mut last_submit_ms = 0_i64;
        let mut working_price: Option<f64> = None;
        let mut last_replace_ms = 0_i64;
        let mut replaces_this_sec = 0_u32;
        let mut replace_sec_epoch = 0_i64;
        let mut inflight_order_id: Option<String> = None;
        let mut inflight_order_units = 0.0_f64;
        let mut inflight_order_matched = 0.0_f64;
        let mut inflight_order_price: Option<f64> = None;
        let mut pending_order_ids = HashSet::<String>::new();
        let mut one_shot_submitted = false;
        let settle_wait_ms = i64::try_from(fill_observe_max_ms).unwrap_or(8_000).max(500);
        let one_shot = matches!(mode, ManualOrderMode::Limit | ManualOrderMode::Market);

        loop {
            if manual_run.stop_requested() {
                stop_reason = "manual_stop_requested".to_string();
                break;
            }
            if started.elapsed() >= Duration::from_secs(duration_sec) {
                break;
            }

            let mut ws_order_update_seen = false;
            if let (Some(ws_state), Some(active_order_id)) = (
                task_state.polymarket_ws_state.as_ref(),
                inflight_order_id.as_ref(),
            ) {
                if ws_state.user_connected() {
                    let wait_after = ws_state
                        .get_order_status(active_order_id.as_str(), i64::MAX)
                        .await
                        .map(|snapshot| snapshot.updated_ms)
                        .unwrap_or(last_ws_user_msg_ms)
                        .max(last_ws_user_msg_ms);
                    if let Ok(next_user_msg_ms) = tokio::time::timeout(
                        Duration::from_millis(poll_ms.min(2_000).max(50)),
                        ws_state.wait_for_order_update_after(active_order_id.as_str(), wait_after),
                    )
                    .await
                    {
                        if next_user_msg_ms > last_ws_user_msg_ms {
                            last_ws_user_msg_ms = next_user_msg_ms;
                        }
                        ws_order_update_seen = true;
                    }
                }
            }

            let now_ms_for_balance = Utc::now().timestamp_millis();
            let should_poll_balance = !ws_order_update_seen
                || now_ms_for_balance.saturating_sub(last_balance_poll_ms)
                    >= MANUAL_WS_BALANCE_FALLBACK_POLL_MS
                || inflight_order_id.is_none();
            let live_balance = if should_poll_balance {
                last_balance_poll_ms = now_ms_for_balance;
                match token_shares_with_fallback(
                    task_state.api.as_ref(),
                    token_id_for_task.as_str(),
                )
                .await
                {
                    Ok(v) => {
                        last_known_balance_shares = v;
                        v
                    }
                    Err(e) => {
                        manual_run
                            .update(|snap| {
                                snap.submit_errors = snap.submit_errors.saturating_add(1);
                                snap.last_error =
                                    Some(format!("balance_check_failed_using_last_known: {e}"));
                            })
                            .await;
                        last_known_balance_shares
                    }
                }
            } else {
                last_known_balance_shares
            };

            let observed_filled = match action {
                ManualOrderAction::Open => (live_balance - initial_balance_shares).max(0.0),
                ManualOrderAction::Close => (initial_balance_shares - live_balance).max(0.0),
            };
            // For open runs, rely on per-order matched units to avoid attributing
            // fills from other runs/wallet drift to this run.
            if matches!(action, ManualOrderAction::Close) {
                if observed_filled > max_filled_seen {
                    max_filled_seen = observed_filled;
                }
                if observed_filled > filled_accounted_shares + 0.000_001 {
                    let delta = observed_filled - filled_accounted_shares;
                    filled_accounted_shares = observed_filled;
                    let notional_px = inflight_order_price
                        .or(working_price)
                        .unwrap_or(sizing_reference_price)
                        .max(0.000_001);
                    filled_accounted_notional_usd += delta * notional_px;
                    reserved_inflight_shares = (reserved_inflight_shares - delta).max(0.0);
                }
            }
            manual_run
                .update(|snap| {
                    snap.filled_shares = filled_accounted_shares.max(max_filled_seen);
                    snap.filled_notional_usd = filled_accounted_notional_usd.max(0.0);
                })
                .await;

            if one_shot_submitted
                && inflight_order_id.is_none()
                && reserved_inflight_shares <= 0.000_001
            {
                stop_reason = "one_shot_submitted".to_string();
                break;
            }

            if let Some(active_order_id) = inflight_order_id.clone() {
                let now_ms = Utc::now().timestamp_millis();
                let inflight_age_ms = now_ms.saturating_sub(last_submit_ms);

                let mut order_terminal = false;
                let mut terminal_lookup_error: Option<String> = None;
                let mut should_reprice_stale = false;
                match task_state.api.get_order(active_order_id.as_str()).await {
                    Ok(order_status) => {
                        let matched_now =
                            manual_order_matched_units(&order_status).min(inflight_order_units);
                        if matched_now > inflight_order_matched + 0.000_001 {
                            let delta = matched_now - inflight_order_matched;
                            inflight_order_matched = matched_now;
                            filled_accounted_shares += delta;
                            let notional_px = inflight_order_price
                                .or(working_price)
                                .unwrap_or(sizing_reference_price)
                                .max(0.000_001);
                            filled_accounted_notional_usd += delta * notional_px;
                            max_filled_seen = max_filled_seen.max(filled_accounted_shares);
                            reserved_inflight_shares = (reserved_inflight_shares - delta).max(0.0);
                            manual_run
                                .update(|snap| {
                                    snap.filled_shares =
                                        filled_accounted_shares.max(max_filled_seen);
                                    snap.filled_notional_usd =
                                        filled_accounted_notional_usd.max(0.0);
                                })
                                .await;
                        }
                        order_terminal =
                            manual_terminal_status_for_order(order_status.status).is_some();
                        if !order_terminal
                            && inflight_age_ms >= settle_wait_ms
                            && matches!(mode, ManualOrderMode::ChaseLimit)
                        {
                            should_reprice_stale = true;
                        }
                    }
                    Err(e) => {
                        let err_text = e.to_string();
                        if manual_order_lookup_error_is_terminal(err_text.as_str()) {
                            order_terminal = true;
                            terminal_lookup_error = Some(format!(
                                "terminal_order_lookup_miss order_id={} err={}",
                                active_order_id, err_text
                            ));
                        } else if inflight_age_ms >= settle_wait_ms
                            && matches!(mode, ManualOrderMode::ChaseLimit)
                        {
                            should_reprice_stale = true;
                        } else {
                            manual_run
                                .update(|snap| {
                                    snap.submit_errors = snap.submit_errors.saturating_add(1);
                                    snap.last_error = Some(format!(
                                        "order_lookup_failed order_id={} err={}",
                                        active_order_id, err_text
                                    ));
                                })
                                .await;
                        }
                    }
                }

                if should_reprice_stale {
                    // Reprice cadence: cancel stale working order before next quote pass.
                    last_replace_ms = now_ms;
                    match task_state.api.cancel_order(active_order_id.as_str()).await {
                        Ok(_) => {
                            manual_run
                                .update(|snap| {
                                    snap.canceled_orders = snap.canceled_orders.saturating_add(1);
                                })
                                .await;
                        }
                        Err(cancel_err) => {
                            manual_run
                                .update(|snap| {
                                    snap.submit_errors = snap.submit_errors.saturating_add(1);
                                    snap.last_error = Some(format!(
                                        "cancel_failed order_id={} err={}",
                                        active_order_id, cancel_err
                                    ));
                                })
                                .await;
                        }
                    }

                    match task_state.api.get_order(active_order_id.as_str()).await {
                        Ok(order_after_cancel) => {
                            let matched_now = manual_order_matched_units(&order_after_cancel)
                                .min(inflight_order_units);
                            if matched_now > inflight_order_matched + 0.000_001 {
                                let delta = matched_now - inflight_order_matched;
                                inflight_order_matched = matched_now;
                                filled_accounted_shares += delta;
                                let notional_px = inflight_order_price
                                    .or(working_price)
                                    .unwrap_or(sizing_reference_price)
                                    .max(0.000_001);
                                filled_accounted_notional_usd += delta * notional_px;
                                max_filled_seen = max_filled_seen.max(filled_accounted_shares);
                                reserved_inflight_shares =
                                    (reserved_inflight_shares - delta).max(0.0);
                                manual_run
                                    .update(|snap| {
                                        snap.filled_shares =
                                            filled_accounted_shares.max(max_filled_seen);
                                        snap.filled_notional_usd =
                                            filled_accounted_notional_usd.max(0.0);
                                    })
                                    .await;
                            }
                            order_terminal =
                                manual_terminal_status_for_order(order_after_cancel.status)
                                    .is_some();
                        }
                        Err(reconcile_err) => {
                            let reconcile_text = reconcile_err.to_string();
                            if manual_order_lookup_error_is_terminal(reconcile_text.as_str()) {
                                order_terminal = true;
                                terminal_lookup_error = Some(format!(
                                    "terminal_order_lookup_miss_after_cancel order_id={} err={}",
                                    active_order_id, reconcile_text
                                ));
                            } else {
                                terminal_lookup_error = Some(format!(
                                    "order_lookup_failed_after_cancel order_id={} err={}",
                                    active_order_id, reconcile_text
                                ));
                            }
                        }
                    }
                }

                if !order_terminal {
                    if inflight_age_ms < settle_wait_ms
                        || matches!(mode, ManualOrderMode::ChaseLimit)
                    {
                        tokio::time::sleep(Duration::from_millis(poll_ms)).await;
                        continue;
                    }
                    stop_reason = "fill_reconcile_timeout".to_string();
                    manual_run
                        .update(|snap| {
                            snap.last_error = Some(format!(
                                "fill_reconcile_timeout order_id={} reserved_inflight_shares={:.6} age_ms={}",
                                active_order_id, reserved_inflight_shares, inflight_age_ms
                            ));
                        })
                        .await;
                    break;
                }

                let outstanding_on_order = (inflight_order_units - inflight_order_matched).max(0.0);
                reserved_inflight_shares =
                    (reserved_inflight_shares - outstanding_on_order).max(0.0);
                pending_order_ids.remove(active_order_id.as_str());
                inflight_order_id = None;
                inflight_order_units = 0.0;
                inflight_order_matched = 0.0;
                inflight_order_price = None;
                if let Some(err_msg) = terminal_lookup_error {
                    manual_run
                        .update(|snap| {
                            snap.last_error = Some(err_msg);
                        })
                        .await;
                }
            }

            if reserved_inflight_shares > 0.0 {
                let now_ms = Utc::now().timestamp_millis();
                let inflight_age_ms = now_ms.saturating_sub(last_submit_ms);
                if inflight_age_ms < settle_wait_ms {
                    tokio::time::sleep(Duration::from_millis(poll_ms)).await;
                    continue;
                }
                stop_reason = "fill_reconcile_timeout".to_string();
                manual_run
                    .update(|snap| {
                        snap.last_error = Some(format!(
                            "fill_reconcile_timeout reserved_inflight_shares={:.6} age_ms={}",
                            reserved_inflight_shares, inflight_age_ms
                        ));
                    })
                    .await;
                break;
            }

            let remaining_shares =
                (target_shares - filled_accounted_shares - reserved_inflight_shares).max(0.0);
            if remaining_shares <= 0.01 {
                stop_reason = "target_filled".to_string();
                break;
            }

            let quote = match task_state
                .api
                .get_best_price(token_id_for_task.as_str())
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    quote_failures = quote_failures.saturating_add(1);
                    manual_run
                        .update(|snap| {
                            snap.submit_errors = snap.submit_errors.saturating_add(1);
                            snap.last_error = Some(format!("quote_failed: {e}"));
                        })
                        .await;
                    if one_shot {
                        stop_reason = "one_shot_submit_failed".to_string();
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(poll_ms)).await;
                    continue;
                }
            };
            let Some(submit_price) = manual_price_from_quote(
                quote.as_ref(),
                action,
                mode,
                post_only,
                request.price,
                min_price,
                max_price,
                slippage_cents,
                tick_size,
            ) else {
                if one_shot {
                    manual_run
                        .update(|snap| {
                            snap.submit_errors = snap.submit_errors.saturating_add(1);
                            snap.last_error = Some(
                                "quote_failed: unable to derive submit price from best quote"
                                    .to_string(),
                            );
                        })
                        .await;
                    stop_reason = "one_shot_submit_failed".to_string();
                    break;
                }
                tokio::time::sleep(Duration::from_millis(poll_ms)).await;
                continue;
            };

            let now_ms = Utc::now().timestamp_millis();
            let sec_bucket = now_ms.saturating_div(1_000);
            if sec_bucket != replace_sec_epoch {
                replace_sec_epoch = sec_bucket;
                replaces_this_sec = 0;
            }
            if evcurve_reprice_for_crypto
                && !should_replace_for_manual(
                    action,
                    working_price,
                    submit_price,
                    min_price,
                    max_price,
                    now_ms,
                    last_replace_ms,
                    replaces_this_sec,
                    chase_cfg,
                    tick_size,
                )
            {
                tokio::time::sleep(Duration::from_millis(poll_ms)).await;
                continue;
            }

            let remaining_notional =
                target_notional_usd.map(|target| (target - filled_accounted_notional_usd).max(0.0));
            if matches!(action, ManualOrderAction::Open)
                && remaining_notional.unwrap_or(f64::INFINITY) <= 0.01
            {
                stop_reason = "target_notional_filled".to_string();
                break;
            }

            let units = match action {
                ManualOrderAction::Open => {
                    let notional_limited_shares = remaining_notional
                        .map(|usd| usd / submit_price.max(0.000_001))
                        .unwrap_or(remaining_shares);
                    remaining_shares.min(notional_limited_shares)
                }
                ManualOrderAction::Close => remaining_shares.min(live_balance.max(0.0)),
            };
            if !units.is_finite() || units <= 0.0 {
                stop_reason = if matches!(action, ManualOrderAction::Close) {
                    "no_live_position_remaining".to_string()
                } else {
                    "invalid_remaining_units".to_string()
                };
                break;
            }

            manual_run
                .update(|snap| {
                    snap.submit_attempts = snap.submit_attempts.saturating_add(1);
                    snap.last_submit_price = Some(submit_price);
                })
                .await;

            let submit_result = match action {
                ManualOrderAction::Open => {
                    submit_manual_buy(
                        task_state.api.clone(),
                        token_id_for_task.as_str(),
                        units,
                        submit_price,
                        post_only,
                        force_fak,
                    )
                    .await
                }
                ManualOrderAction::Close => {
                    submit_manual_sell(
                        task_state.api.clone(),
                        token_id_for_task.as_str(),
                        units,
                        submit_price,
                        post_only,
                        force_fak,
                    )
                    .await
                }
            };

            match submit_result {
                Ok(result_payload) => {
                    let submitted_units = result_payload
                        .get("submitted_shares")
                        .and_then(Value::as_f64)
                        .unwrap_or(units)
                        .max(0.0);
                    let submitted_order_id = result_payload
                        .get("order_id")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .map(ToString::to_string);
                    if submitted_units <= 0.0 {
                        manual_run
                            .update(|snap| {
                                snap.submit_errors = snap.submit_errors.saturating_add(1);
                                snap.last_error =
                                    Some("submit_returned_zero_submitted_shares".to_string());
                            })
                            .await;
                        if one_shot {
                            stop_reason = "one_shot_submit_failed".to_string();
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(poll_ms)).await;
                        continue;
                    }
                    let Some(order_id) = submitted_order_id else {
                        manual_run
                            .update(|snap| {
                                snap.submit_errors = snap.submit_errors.saturating_add(1);
                                snap.last_error = Some(
                                    "submit_missing_order_id_hard_stop_for_safety".to_string(),
                                );
                            })
                            .await;
                        stop_reason = "missing_order_id".to_string();
                        break;
                    };
                    let resting_limit_one_shot = matches!(mode, ManualOrderMode::Limit)
                        && matches!(action, ManualOrderAction::Open | ManualOrderAction::Close);
                    if resting_limit_one_shot {
                        // For limit one-shot mode (open+limit and close+limit), leave the
                        // submitted maker order resting and exit without reconcile/cancel cleanup.
                        manual_run
                            .update(|snap| {
                                snap.submit_success = snap.submit_success.saturating_add(1);
                            })
                            .await;
                        stop_reason = "one_shot_submitted".to_string();
                        break;
                    }

                    reserved_inflight_shares += submitted_units;
                    last_submit_ms = now_ms;
                    working_price = Some(submit_price);
                    last_replace_ms = now_ms;
                    replaces_this_sec = replaces_this_sec.saturating_add(1);
                    inflight_order_id = Some(order_id.clone());
                    inflight_order_units = submitted_units;
                    inflight_order_matched = 0.0;
                    inflight_order_price = Some(submit_price);
                    pending_order_ids.insert(order_id);
                    manual_run
                        .update(|snap| {
                            snap.submit_success = snap.submit_success.saturating_add(1);
                        })
                        .await;
                    if one_shot {
                        one_shot_submitted = true;
                    }
                }
                Err(e) => {
                    reserved_inflight_shares = 0.0;
                    inflight_order_id = None;
                    inflight_order_units = 0.0;
                    inflight_order_matched = 0.0;
                    inflight_order_price = None;
                    manual_run
                        .update(|snap| {
                            snap.submit_errors = snap.submit_errors.saturating_add(1);
                            snap.last_error = Some(format!("submit_failed: {e}"));
                        })
                        .await;
                    if one_shot {
                        stop_reason = "one_shot_submit_failed".to_string();
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(poll_ms)).await;
        }

        let mut cancel_cleanup_incomplete = false;
        if !pending_order_ids.is_empty() {
            let cancel_confirm_deadline_ms =
                i64::try_from(close_cancel_confirm_max_ms).unwrap_or(12_000);
            for order_id in pending_order_ids.iter() {
                match task_state.api.cancel_order(order_id.as_str()).await {
                    Ok(_) => {
                        manual_run
                            .update(|snap| {
                                snap.canceled_orders = snap.canceled_orders.saturating_add(1);
                            })
                            .await;
                    }
                    Err(cancel_err) => {
                        manual_run
                            .update(|snap| {
                                snap.submit_errors = snap.submit_errors.saturating_add(1);
                                snap.last_error = Some(format!(
                                    "final_cancel_failed order_id={} err={}",
                                    order_id, cancel_err
                                ));
                            })
                            .await;
                    }
                }

                let started_wait_ms = Utc::now().timestamp_millis();
                let mut terminal = false;
                loop {
                    let age_ms = Utc::now()
                        .timestamp_millis()
                        .saturating_sub(started_wait_ms);
                    if age_ms >= cancel_confirm_deadline_ms {
                        break;
                    }
                    match task_state.api.get_order(order_id.as_str()).await {
                        Ok(order_status) => {
                            if manual_terminal_status_for_order(order_status.status).is_some() {
                                terminal = true;
                                break;
                            }
                        }
                        Err(e) => {
                            if manual_order_lookup_error_is_terminal(e.to_string().as_str()) {
                                terminal = true;
                                break;
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(poll_ms.min(500))).await;
                }

                if !terminal {
                    cancel_cleanup_incomplete = true;
                    manual_run
                        .update(|snap| {
                            snap.last_error = Some(format!(
                                "cancel_confirm_timeout order_id={} max_wait_ms={}",
                                order_id, close_cancel_confirm_max_ms
                            ));
                        })
                        .await;
                }
            }
        }

        let final_balance =
            token_shares_with_fallback(task_state.api.as_ref(), token_id_for_task.as_str())
                .await
                .unwrap_or(last_known_balance_shares);
        let balance_based_final = match action {
            ManualOrderAction::Open => (final_balance - initial_balance_shares).max(0.0),
            ManualOrderAction::Close => (initial_balance_shares - final_balance).max(0.0),
        };
        let mut final_filled = filled_accounted_shares.max(max_filled_seen);
        if matches!(action, ManualOrderAction::Close)
            || stop_reason == "fill_reconcile_timeout"
            || cancel_cleanup_incomplete
        {
            final_filled = final_filled.max(balance_based_final);
        }
        let reconcile_window_ms = i64::try_from(close_post_cancel_reconcile_ms)
            .unwrap_or(15_000)
            .max(0);
        if reconcile_window_ms > 0 {
            let reconcile_started_ms = Utc::now().timestamp_millis();
            let mut stable_polls = 0_u32;
            while stable_polls < 3
                && Utc::now()
                    .timestamp_millis()
                    .saturating_sub(reconcile_started_ms)
                    < reconcile_window_ms
            {
                tokio::time::sleep(Duration::from_millis(poll_ms.min(750))).await;
                if let Ok(balance_now) =
                    token_shares_with_fallback(task_state.api.as_ref(), token_id_for_task.as_str())
                        .await
                {
                    let observed = match action {
                        ManualOrderAction::Open => (balance_now - initial_balance_shares).max(0.0),
                        ManualOrderAction::Close => (initial_balance_shares - balance_now).max(0.0),
                    };
                    let allow_balance_reconcile = matches!(action, ManualOrderAction::Close)
                        || stop_reason == "fill_reconcile_timeout"
                        || cancel_cleanup_incomplete;
                    if allow_balance_reconcile && observed > final_filled + 0.000_001 {
                        final_filled = observed;
                        stable_polls = 0;
                    } else {
                        stable_polls = stable_polls.saturating_add(1);
                    }
                } else {
                    stable_polls = stable_polls.saturating_add(1);
                }
            }
        }
        let final_notional = filled_accounted_notional_usd.max(
            final_filled
                * working_price
                    .or(inflight_order_price)
                    .unwrap_or(sizing_reference_price)
                    .max(0.000_001),
        );

        let mut tp_order_id: Option<String> = None;
        let mut tp_error: Option<String> = None;
        if let Some(tp_price) = tp_price {
            if final_filled > 0.0 {
                let mut tp_errors: Vec<String> = Vec::new();
                for attempt in 1..=MANUAL_TP_SUBMIT_ATTEMPTS {
                    let live_shares = token_shares_with_fallback(
                        task_state.api.as_ref(),
                        token_id_for_task.as_str(),
                    )
                    .await
                    .unwrap_or(final_filled)
                    .max(0.0);
                    let attempt_shares = final_filled.min(live_shares).max(0.0);
                    if attempt_shares < 0.01 {
                        tp_errors.push(format!(
                            "tp attempt {} skipped: live_shares_below_min live={:.6} final_filled={:.6}",
                            attempt, live_shares, final_filled
                        ));
                        break;
                    }

                    if let Err(e) = task_state
                        .api
                        .update_balance_allowance_for_sell(token_id_for_task.as_str())
                        .await
                    {
                        let msg =
                            format!("tp attempt {} allowance refresh failed: {:#}", attempt, e);
                        tp_errors.push(msg.clone());
                        if attempt < MANUAL_TP_SUBMIT_ATTEMPTS {
                            tokio::time::sleep(Duration::from_millis(
                                MANUAL_TP_RETRY_DELAY_MS * u64::from(attempt),
                            ))
                            .await;
                            continue;
                        }
                        break;
                    }

                    match place_tp_limit_sell(
                        task_state.api.clone(),
                        token_id_for_task.as_str(),
                        attempt_shares,
                        tp_price,
                        true,
                    )
                    .await
                    {
                        Ok(v) => {
                            tp_order_id = v
                                .get("order_id")
                                .and_then(Value::as_str)
                                .map(ToString::to_string);
                            if tp_order_id.is_none() {
                                tp_errors.push(format!(
                                    "tp attempt {} missing order_id in response: {}",
                                    attempt, v
                                ));
                            } else {
                                break;
                            }
                        }
                        Err(e) => {
                            let err_msg = format!("tp attempt {} failed: {:#}", attempt, e);
                            let retryable = manual_tp_error_is_retryable(&e);
                            tp_errors.push(err_msg);
                            if retryable && attempt < MANUAL_TP_SUBMIT_ATTEMPTS {
                                tokio::time::sleep(Duration::from_millis(
                                    MANUAL_TP_RETRY_DELAY_MS * u64::from(attempt),
                                ))
                                .await;
                                continue;
                            }
                        }
                    }
                }
                if tp_order_id.is_none() && !tp_errors.is_empty() {
                    tp_error = Some(tp_errors.join(" | "));
                }
            }
        }

        manual_run
            .update(|snap| {
                if snap.status == "running" || snap.status == "stopping" {
                    snap.status = if stop_reason == "manual_stop_requested" {
                        "canceled".to_string()
                    } else if cancel_cleanup_incomplete {
                        "failed".to_string()
                    } else {
                        "completed".to_string()
                    };
                }
                snap.stop_reason = Some(stop_reason.clone());
                snap.finished_at_ms = Some(Utc::now().timestamp_millis());
                snap.filled_shares = final_filled;
                snap.filled_notional_usd = final_notional.max(0.0);
                if let Some(order_id) = tp_order_id {
                    snap.tp_order_id = Some(order_id);
                }
                if let Some(err) = tp_error {
                    snap.last_error = Some(err);
                }
                if cancel_cleanup_incomplete && snap.last_error.is_none() {
                    snap.last_error =
                        Some("manual_close_pending_cancel_cleanup_incomplete".to_string());
                }
                if quote_failures > 0 && snap.last_error.is_none() {
                    snap.last_error = Some(format!("quote_failures={quote_failures}"));
                }
            })
            .await;

        {
            let mut claims = task_state.scope_claims.write().await;
            if matches!(
                claims.get(scope_key_for_task.as_str()),
                Some(existing) if existing == run_id_for_task.as_str()
            ) {
                claims.remove(scope_key_for_task.as_str());
            }
        }
    });

    Ok(json!({
        "ok": true,
        "run_id": run_id,
        "action": action.as_str(),
        "mode": mode.as_str(),
        "strategy_id": strategy_id,
        "condition_id": market.condition_id,
        "market_slug": market.slug,
        "token_id": token_id,
        "symbol": symbol,
        "period_timestamp": period_timestamp,
        "timeframe": source_timeframe,
        "post_only": post_only,
        "force_fak": force_fak,
        "evcurve_reprice_for_crypto": evcurve_reprice_for_crypto,
        "min_price": min_price,
        "max_price": max_price,
        "price_hint": price_hint,
        "sizing_reference_price": sizing_reference_price,
        "target_shares": target_shares,
        "size_usd": requested_size_usd,
        "duration_sec": duration_sec,
        "poll_ms": poll_ms,
        "tp_price": tp_price,
        "slippage_cents": slippage_cents,
        "warning": budget_warning.or(balance_tracking_warning),
        "ui_market": ui_market,
        "ui_run": ui_run,
        "notes": [
            "manual open/close tracks remaining target and auto-stops when filled",
            "manual overfill guard enabled: inflight reserve + fill reconcile timeout"
        ]
    }))
}

fn mm_activity_value_as_f64(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(num)) => num.as_f64(),
        Some(Value::String(raw)) => raw.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn value_as_bool(value: Option<&Value>) -> Option<bool> {
    match value {
        Some(Value::Bool(v)) => Some(*v),
        Some(Value::Number(v)) => v
            .as_i64()
            .map(|raw| raw != 0)
            .or_else(|| v.as_u64().map(|raw| raw != 0)),
        Some(Value::String(raw)) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn json_value_as_f64(row: &Value, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(value) = mm_activity_value_as_f64(row.get(*key)).filter(|v| v.is_finite()) {
            return Some(value);
        }
    }
    None
}

fn json_value_as_string(row: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = row
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_string());
        }
    }
    None
}

fn json_value_as_bool(row: &Value, keys: &[&str]) -> Option<bool> {
    for key in keys {
        if let Some(value) = value_as_bool(row.get(*key)) {
            return Some(value);
        }
    }
    None
}

fn summarize_live_positions(rows: &[Value], include_zero: bool) -> (Vec<Value>, Value) {
    let mut out = Vec::new();
    let mut total_size = 0.0_f64;
    let mut total_initial_value = 0.0_f64;
    let mut total_current_value = 0.0_f64;
    let mut total_cash_pnl = 0.0_f64;
    let mut total_realized_pnl = 0.0_f64;
    let mut redeemable_count = 0usize;
    let mut mergeable_count = 0usize;

    for row in rows {
        let size = json_value_as_f64(row, &["size", "position_size"]).unwrap_or(0.0);
        if !include_zero && (!size.is_finite() || size <= 0.0) {
            continue;
        }
        if size.is_finite() {
            total_size += size;
        }

        let initial_value =
            json_value_as_f64(row, &["initialValue", "initial_value"]).unwrap_or(0.0);
        if initial_value.is_finite() {
            total_initial_value += initial_value;
        }
        let current_value =
            json_value_as_f64(row, &["currentValue", "current_value"]).unwrap_or(0.0);
        if current_value.is_finite() {
            total_current_value += current_value;
        }
        let cash_pnl = json_value_as_f64(row, &["cashPnl", "cash_pnl"]).unwrap_or(0.0);
        if cash_pnl.is_finite() {
            total_cash_pnl += cash_pnl;
        }
        let realized_pnl = json_value_as_f64(row, &["realizedPnl", "realized_pnl"]).unwrap_or(0.0);
        if realized_pnl.is_finite() {
            total_realized_pnl += realized_pnl;
        }
        if json_value_as_bool(row, &["redeemable"]).unwrap_or(false) {
            redeemable_count += 1;
        }
        if json_value_as_bool(row, &["mergeable"]).unwrap_or(false) {
            mergeable_count += 1;
        }

        out.push(row.clone());
    }

    let positions_count = out.len();
    (
        out,
        json!({
            "positions_count": positions_count,
            "total_size": total_size,
            "total_initial_value_usd": total_initial_value,
            "total_current_value_usd": total_current_value,
            "total_cash_pnl_usd": total_cash_pnl,
            "total_realized_pnl_usd": total_realized_pnl,
            "total_net_pnl_usd": total_cash_pnl + total_realized_pnl,
            "redeemable_positions": redeemable_count,
            "mergeable_positions": mergeable_count
        }),
    )
}

fn parse_live_window(raw_window: Option<&str>) -> Result<(String, Option<i64>, Option<i64>)> {
    let raw = raw_window.unwrap_or("24h").trim();
    let normalized = raw
        .to_ascii_lowercase()
        .replace([' ', '_'], "")
        .replace("hours", "h")
        .replace("hour", "h")
        .replace("days", "d")
        .replace("day", "d")
        .replace("minutes", "m")
        .replace("minute", "m")
        .replace("seconds", "s")
        .replace("second", "s");

    if normalized.is_empty() {
        anyhow::bail!("window is empty (examples: 1h, 5h, 30d, all)");
    }
    if normalized == "all" {
        return Ok(("all".to_string(), None, None));
    }

    let mut split_at = normalized.len();
    for (idx, ch) in normalized.char_indices() {
        if !(ch.is_ascii_digit() || ch == '.') {
            split_at = idx;
            break;
        }
    }
    let qty_raw = normalized[..split_at].trim();
    let unit_raw = normalized[split_at..].trim();
    if qty_raw.is_empty() || unit_raw.is_empty() {
        anyhow::bail!("invalid window '{}' (examples: 1h, 5h, 30d, all)", raw);
    }
    let qty = qty_raw
        .parse::<f64>()
        .with_context(|| format!("invalid window amount '{}'", qty_raw))?;
    if !qty.is_finite() || qty <= 0.0 {
        anyhow::bail!("window amount must be > 0");
    }
    let unit_seconds = match unit_raw {
        "s" | "sec" | "secs" => 1.0,
        "m" | "min" | "mins" => 60.0,
        "h" | "hr" | "hrs" => 60.0 * 60.0,
        "d" => 24.0 * 60.0 * 60.0,
        "w" | "wk" | "wks" => 7.0 * 24.0 * 60.0 * 60.0,
        _ => anyhow::bail!("unsupported window unit '{}' (use s/m/h/d/w)", unit_raw),
    };
    let duration_ms = (qty * unit_seconds * 1_000.0).round() as i64;
    if duration_ms <= 0 {
        anyhow::bail!("window duration is too small");
    }
    let now_ms = Utc::now().timestamp_millis();
    let since_ms = now_ms.saturating_sub(duration_ms);
    Ok((normalized, Some(since_ms), Some(duration_ms)))
}

fn activity_timestamp_ms(event: &Value) -> i64 {
    let raw_ts = event
        .get("timestamp")
        .and_then(Value::as_i64)
        .or_else(|| {
            event
                .get("timestamp")
                .and_then(Value::as_u64)
                .and_then(|v| i64::try_from(v).ok())
        })
        .or_else(|| {
            event
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(|v| v.trim().parse::<i64>().ok())
        })
        .unwrap_or_else(|| Utc::now().timestamp());
    if raw_ts > 10_000_000_000 {
        raw_ts
    } else {
        raw_ts.saturating_mul(1_000)
    }
}

fn activity_usdc_notional(event: &Value) -> Option<f64> {
    let direct = mm_activity_value_as_f64(event.get("usdcSize")).filter(|v| v.is_finite());
    if direct.is_some() {
        return direct;
    }
    let size = mm_activity_value_as_f64(event.get("size")).filter(|v| v.is_finite())?;
    let price = mm_activity_value_as_f64(event.get("price")).filter(|v| v.is_finite())?;
    Some(size * price)
}

fn activity_signed_pnl_cashflow(event: &Value) -> Option<f64> {
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .map(|v| v.trim().to_ascii_uppercase())
        .unwrap_or_default();
    let side = event
        .get("side")
        .and_then(Value::as_str)
        .map(|v| v.trim().to_ascii_uppercase())
        .unwrap_or_default();
    let usdc = activity_usdc_notional(event)?;
    if !usdc.is_finite() {
        return None;
    }

    if matches!(
        event_type.as_str(),
        "DEPOSIT" | "WITHDRAW" | "TRANSFER" | "TRANSFER_IN" | "TRANSFER_OUT"
    ) {
        return None;
    }

    if side == "BUY" {
        return Some(-usdc.abs());
    }
    if side == "SELL" {
        return Some(usdc.abs());
    }

    if matches!(
        event_type.as_str(),
        "REDEEM" | "REDEMPTION" | "MERGE" | "MAKER_REBATE"
    ) {
        return Some(usdc.abs());
    }

    Some(usdc)
}

async fn handle_manual_premarket(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ManualPremarketRequest>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    ensure_write_allowed(&state)?;
    let result = run_manual_premarket_submit(state.trader.clone(), state.api.clone(), payload)
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(result))
}

async fn handle_manual_open(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ManualOrderRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    check_auth(&headers, &state)?;
    ensure_write_allowed(&state)?;
    let result = start_manual_order(state, payload, ManualOrderAction::Open)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            let status = if msg.contains("duplicate_running_scope") {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            ApiError::new(status, msg)
        })?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}

async fn handle_manual_close(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ManualOrderRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    check_auth(&headers, &state)?;
    ensure_write_allowed(&state)?;
    let result = start_manual_order(state, payload, ManualOrderAction::Close)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            let status = if msg.contains("duplicate_running_scope") {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            ApiError::new(status, msg)
        })?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}

fn manual_run_matches_action(snapshot: &ManualRunSnapshot, action: ManualOrderAction) -> bool {
    match action {
        ManualOrderAction::Open => {
            snapshot.run_id.starts_with("manual_open:") || snapshot.run_type.starts_with("open_")
        }
        ManualOrderAction::Close => {
            snapshot.run_id.starts_with("manual_close:") || snapshot.run_type.starts_with("close_")
        }
    }
}

async fn stop_manual_run_by_id(
    state: &AppState,
    run_id: &str,
    action: ManualOrderAction,
) -> Result<(), ApiError> {
    let run = {
        let registry = state.run_registry.read().await;
        registry.get(run_id).cloned()
    }
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "run_id not found"))?;

    let snapshot = run.snapshot().await;
    if !manual_run_matches_action(&snapshot, action) {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "run_id not found for this endpoint scope",
        ));
    }

    run.request_stop();
    run.update(|snap| {
        if snap.status == "running" {
            snap.status = "stopping".to_string();
        }
        snap.stop_reason = Some("manual_stop_requested".to_string());
    })
    .await;
    Ok(())
}

async fn handle_manual_open_stop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ManualChaseStopRequest>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    let run_id = nonempty_owned(payload.run_id)
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "run_id is required"))?;
    stop_manual_run_by_id(&state, run_id.as_str(), ManualOrderAction::Open).await?;

    Ok(Json(json!({
        "ok": true,
        "run_id": run_id,
        "message": "stop requested"
    })))
}

async fn handle_manual_close_stop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ManualChaseStopRequest>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    let run_id = nonempty_owned(payload.run_id)
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "run_id is required"))?;
    stop_manual_run_by_id(&state, run_id.as_str(), ManualOrderAction::Close).await?;

    Ok(Json(json!({
        "ok": true,
        "run_id": run_id,
        "message": "stop requested"
    })))
}

async fn handle_manual_open_stop_path(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxPath(run_id): AxPath<String>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    stop_manual_run_by_id(&state, run_id.as_str(), ManualOrderAction::Open).await?;

    Ok(Json(json!({
        "ok": true,
        "run_id": run_id,
        "message": "stop requested"
    })))
}

async fn handle_manual_close_stop_path(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxPath(run_id): AxPath<String>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    stop_manual_run_by_id(&state, run_id.as_str(), ManualOrderAction::Close).await?;

    Ok(Json(json!({
        "ok": true,
        "run_id": run_id,
        "message": "stop requested"
    })))
}

async fn list_manual_runs_for_action(
    state: &AppState,
    status_filter: Option<&str>,
    action: ManualOrderAction,
) -> Vec<ManualRunSnapshot> {
    let registry = state.run_registry.read().await;
    let mut out = Vec::new();
    for run in registry.values() {
        let snapshot = run.snapshot().await;
        if !manual_run_matches_action(&snapshot, action) {
            continue;
        }
        if let Some(filter) = status_filter {
            if snapshot.status.to_ascii_lowercase() != filter {
                continue;
            }
        }
        out.push(snapshot);
    }
    out
}

async fn handle_manual_open_runs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RunListQuery>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    let status_filter = query
        .status
        .as_deref()
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty());
    let runs =
        list_manual_runs_for_action(&state, status_filter.as_deref(), ManualOrderAction::Open)
            .await;
    let ui_runs = enrich_manual_runs(&state, &runs).await;

    Ok(Json(json!({
        "ok": true,
        "count": runs.len(),
        "runs": runs,
        "ui_runs": ui_runs
    })))
}

async fn handle_manual_close_runs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RunListQuery>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    let status_filter = query
        .status
        .as_deref()
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty());
    let runs =
        list_manual_runs_for_action(&state, status_filter.as_deref(), ManualOrderAction::Close)
            .await;
    let ui_runs = enrich_manual_runs(&state, &runs).await;

    Ok(Json(json!({
        "ok": true,
        "count": runs.len(),
        "runs": runs,
        "ui_runs": ui_runs
    })))
}

async fn manual_run_for_action(
    state: &AppState,
    run_id: &str,
    action: ManualOrderAction,
) -> Result<ManualRunSnapshot, ApiError> {
    let run = {
        let registry = state.run_registry.read().await;
        registry.get(run_id).cloned()
    }
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "run_id not found"))?;
    let snapshot = run.snapshot().await;
    if !manual_run_matches_action(&snapshot, action) {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "run_id not found for this endpoint scope",
        ));
    }
    Ok(snapshot)
}

async fn handle_manual_open_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxPath(run_id): AxPath<String>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    let snapshot = manual_run_for_action(&state, run_id.as_str(), ManualOrderAction::Open).await?;
    let ui_market = resolve_ui_market_cached(
        &state,
        &mut HashMap::new(),
        snapshot.condition_id.as_str(),
        snapshot.market_slug.as_str(),
    )
    .await;
    Ok(Json(json!({
        "ok": true,
        "run": snapshot,
        "ui_run": build_ui_manual_run(&snapshot, ui_market.as_ref())
    })))
}

async fn handle_manual_close_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxPath(run_id): AxPath<String>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    let snapshot = manual_run_for_action(&state, run_id.as_str(), ManualOrderAction::Close).await?;
    let ui_market = resolve_ui_market_cached(
        &state,
        &mut HashMap::new(),
        snapshot.condition_id.as_str(),
        snapshot.market_slug.as_str(),
    )
    .await;
    Ok(Json(json!({
        "ok": true,
        "run": snapshot,
        "ui_run": build_ui_manual_run(&snapshot, ui_market.as_ref())
    })))
}

async fn handle_manual_market_search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<MarketSearchQuery>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    let limit = market_query_limit(query.limit);
    let normalized_query = normalize_search_query(query.q.as_deref());
    let markets = if let Some(query_value) = normalized_query.as_deref() {
        search_manual_markets(&state, query_value, limit).await?
    } else {
        Vec::new()
    };
    Ok(Json(json!({
        "ok": true,
        "query": normalized_query,
        "count": markets.len(),
        "markets": markets
    })))
}

async fn handle_manual_market_recent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<MarketRecentQuery>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    let limit = market_query_limit(query.limit);
    let markets = collect_recent_manual_markets(&state, limit).await?;
    Ok(Json(json!({
        "ok": true,
        "count": markets.len(),
        "markets": markets
    })))
}

async fn handle_manual_market_by_condition(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxPath(condition_id): AxPath<String>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    let (market, details) =
        resolve_manual_market_request(state.api.as_ref(), Some(condition_id.as_str()), None)
            .await
            .map_err(market_lookup_error)?;
    Ok(Json(json!({
        "ok": true,
        "market": build_ui_market(&market, details.as_ref())
    })))
}

async fn handle_manual_market_by_slug(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxPath(slug): AxPath<String>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    let (market, details) =
        resolve_manual_market_request(state.api.as_ref(), None, Some(slug.as_str()))
            .await
            .map_err(market_lookup_error)?;
    Ok(Json(json!({
        "ok": true,
        "market": build_ui_market(&market, details.as_ref())
    })))
}

async fn handle_manual_auto_redeem_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    let status = state
        .api
        .check_auto_redeem_approval()
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({
        "ok": true,
        "auto_redeem": status
    })))
}

async fn handle_manual_auto_redeem_set(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ManualAutoRedeemRequest>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    ensure_write_allowed(&state)?;
    let status = state
        .api
        .ensure_auto_redeem_approval(payload.enabled)
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({
        "ok": true,
        "auto_redeem": status
    })))
}

async fn handle_manual_redeem(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ManualRedeemRequest>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    ensure_write_allowed(&state)?;

    if let Some(condition_id) = nonempty_owned(payload.condition_id.clone()) {
        let outcome_label = nonempty_owned(payload.outcome_label.clone())
            .unwrap_or_else(|| "manual_condition_redeem".to_string());
        let result = state
            .api
            .redeem_tokens_batch(&[RedeemConditionRequest {
                condition_id: condition_id.clone(),
                outcome_label: outcome_label.clone(),
            }])
            .await
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let sync_error = state
            .trader
            .sync_trades_with_portfolio()
            .await
            .err()
            .map(|e| e.to_string());
        return Ok(Json(json!({
            "ok": result.success,
            "mode": "condition",
            "condition_id": condition_id,
            "outcome_label": outcome_label,
            "result": result,
            "sync_error": sync_error
        })));
    }

    if payload.sweep.unwrap_or(false) {
        state
            .trader
            .run_manual_redemption_sweep_now()
            .await
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        return Ok(Json(json!({
            "ok": true,
            "mode": "sweep",
            "message": "manual redemption sweep executed"
        })));
    }

    Err(ApiError::new(
        StatusCode::BAD_REQUEST,
        "provide condition_id or set sweep=true",
    ))
}

async fn handle_manual_merge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ManualMergeRequest>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    ensure_write_allowed(&state)?;

    if let Some(condition_id) = nonempty_owned(payload.condition_id.clone()) {
        let result = state
            .trader
            .run_manual_merge_condition_now(condition_id.as_str())
            .await
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        return Ok(Json(json!({
            "ok": true,
            "mode": "condition",
            "condition_id": condition_id,
            "result": result
        })));
    }

    if payload.sweep.unwrap_or(false) {
        state
            .trader
            .run_manual_merge_sweep_now()
            .await
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        return Ok(Json(json!({
            "ok": true,
            "mode": "sweep",
            "message": "manual merge sweep executed"
        })));
    }

    Err(ApiError::new(
        StatusCode::BAD_REQUEST,
        "provide condition_id or set sweep=true",
    ))
}

async fn handle_manual_positions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PositionsQuery>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;

    let wallet = query
        .wallet
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let include_zero = parse_bool_query(query.include_zero.as_deref(), false);
    let started = Instant::now();

    let rows = state
        .api
        .get_wallet_positions_live(wallet)
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let (filtered_rows, summary) = summarize_live_positions(&rows, include_zero);
    let ui_positions = enrich_manual_positions(&state, &filtered_rows).await;

    let resolved_wallet = wallet.map(|v| v.to_ascii_lowercase()).or_else(|| {
        state
            .api
            .trading_account_address()
            .ok()
            .map(|v| v.to_ascii_lowercase())
    });

    Ok(Json(json!({
        "ok": true,
        "source": "polymarket_data_api_live",
        "wallet": resolved_wallet,
        "include_zero": include_zero,
        "fetched_at_ms": Utc::now().timestamp_millis(),
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "summary": summary,
        "positions": filtered_rows,
        "ui_positions": ui_positions
    })))
}

async fn handle_manual_balance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<BalanceQuery>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;

    let wallet = query
        .wallet
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let started = Instant::now();

    let (positions_result, usdc_result) = tokio::join!(
        state.api.get_wallet_positions_live(wallet),
        state.api.check_usdc_balance_allowance()
    );

    let rows = positions_result
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let (_, summary) = summarize_live_positions(&rows, false);
    let total_current_value = summary
        .get("total_current_value_usd")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);

    let configured_wallet = state
        .api
        .trading_account_address()
        .ok()
        .map(|v| v.to_ascii_lowercase());
    let resolved_wallet = wallet.map(|v| v.to_ascii_lowercase()).or_else(|| {
        state
            .api
            .trading_account_address()
            .ok()
            .map(|v| v.to_ascii_lowercase())
    });

    let (usdc_balance, usdc_allowance, usdc_error) = match usdc_result {
        Ok((balance, allowance)) => (
            Some(
                f64::try_from(balance / Decimal::from(1_000_000u64))
                    .unwrap_or(0.0)
                    .max(0.0),
            ),
            Some(
                f64::try_from(allowance / Decimal::from(1_000_000u64))
                    .unwrap_or(0.0)
                    .max(0.0),
            ),
            None::<String>,
        ),
        Err(e) => (None, None, Some(e.to_string())),
    };

    let estimated_total_equity = usdc_balance.map(|bal| bal + total_current_value);
    let ui_summary = json!({
        "available_cash_usd": usdc_balance,
        "available_allowance_usd": usdc_allowance,
        "positions_value_usd": total_current_value,
        "estimated_total_equity_usd": estimated_total_equity,
        "message": if usdc_error.is_some() {
            "Balance loaded, but cash allowance needs attention."
        } else {
            "Cash and open positions are ready."
        }
    });

    Ok(Json(json!({
        "ok": true,
        "source": "polymarket_data_api_live",
        "wallet": resolved_wallet,
        "fetched_at_ms": Utc::now().timestamp_millis(),
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "usdc": {
            "balance": usdc_balance,
            "allowance": usdc_allowance,
            "error": usdc_error,
            "scope_wallet": configured_wallet,
            "scope_note": "pUSD balance/allowance is fetched for configured trading account; wallet override applies to positions only."
        },
        "positions_summary": summary,
        "estimated_total_equity_usd": estimated_total_equity,
        "ui_summary": ui_summary,
        "note": "estimated_total_equity_usd = pUSD balance + live positions current value"
    })))
}

async fn handle_manual_pnl(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PnlQuery>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;

    let (window_label, window_since_ms, window_duration_ms) =
        parse_live_window(query.window.as_deref())
            .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    let activity_limit = query.limit.unwrap_or(1_000).clamp(1, 1_000);

    let wallet = query
        .wallet
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_ascii_lowercase)
        .or_else(|| {
            state
                .api
                .trading_account_address()
                .ok()
                .map(|v| v.to_ascii_lowercase())
        })
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "wallet_missing"))?;

    let started = Instant::now();
    let (activity_result, positions_result) = tokio::join!(
        state.api.get_user_activity(wallet.as_str(), activity_limit),
        state.api.get_wallet_positions_live(Some(wallet.as_str()))
    );

    let mut events = activity_result
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if let Some(since_ms) = window_since_ms {
        events.retain(|event| activity_timestamp_ms(event) >= since_ms);
    }

    let net_cashflow_usd: f64 = events
        .iter()
        .filter_map(activity_signed_pnl_cashflow)
        .filter(|v| v.is_finite())
        .sum();

    let rows = positions_result
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let (_, positions_summary) = summarize_live_positions(&rows, false);
    let unrealized_live_usd = positions_summary
        .get("total_current_value_usd")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let estimated_total_pnl_usd = net_cashflow_usd + unrealized_live_usd;

    Ok(Json(json!({
        "ok": true,
        "wallet": wallet,
        "window": window_label,
        "window_since_ms": window_since_ms,
        "window_duration_ms": window_duration_ms,
        "activity_limit": activity_limit,
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "activity_count": events.len(),
        "cashflow_pnl_usd": net_cashflow_usd,
        "unrealized_live_value_usd": unrealized_live_usd,
        "estimated_total_pnl_usd": estimated_total_pnl_usd,
        "positions_summary": positions_summary,
        "events": events,
        "note": "cashflow_pnl_usd is computed from activity side/type heuristics; use for operational monitoring"
    })))
}

async fn handle_manual_audit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ManualAuditQuery>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    let started = Instant::now();
    let include_runs = parse_bool_query(query.include_runs.as_deref(), true);
    let now_ms = Utc::now().timestamp_millis();

    let snapshots = collect_run_snapshots(&state).await;
    let mut running = 0usize;
    let mut stopping = 0usize;
    let mut completed = 0usize;
    let mut canceled = 0usize;
    let mut failed = 0usize;
    let mut other = 0usize;
    let mut stale_stopping_runs: Vec<String> = Vec::new();
    let mut error_runs: Vec<String> = Vec::new();

    let mut running_scope_counts: HashMap<(String, String, String), usize> = HashMap::new();
    for run in &snapshots {
        let status = run.status.trim().to_ascii_lowercase();
        match status.as_str() {
            "running" => {
                running = running.saturating_add(1);
                let key = (
                    run.condition_id.clone(),
                    run.token_id.clone(),
                    run.timeframe.clone(),
                );
                *running_scope_counts.entry(key).or_insert(0) += 1;
            }
            "stopping" => {
                stopping = stopping.saturating_add(1);
                if now_ms.saturating_sub(run.updated_at_ms) > 30_000 {
                    stale_stopping_runs.push(run.run_id.clone());
                }
            }
            "completed" => completed = completed.saturating_add(1),
            "canceled" => canceled = canceled.saturating_add(1),
            "failed" => failed = failed.saturating_add(1),
            _ => other = other.saturating_add(1),
        }
        if run
            .last_error
            .as_ref()
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
        {
            error_runs.push(run.run_id.clone());
        }
    }
    let duplicated_running_scopes = running_scope_counts
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|((condition_id, token_id, timeframe), count)| {
            json!({
                "condition_id": condition_id,
                "token_id": token_id,
                "timeframe": timeframe,
                "count": count
            })
        })
        .collect::<Vec<_>>();

    let mut recommendations = Vec::new();
    if running > 0 || stopping > 0 {
        recommendations.push(json!({
            "action": "doctor_fix",
            "endpoint": "/manual/doctor/fix",
            "payload": {
                "stop_running": true,
                "cancel_pending": true,
                "sync_portfolio": true,
                "prune_completed": false
            }
        }));
    }
    if !stale_stopping_runs.is_empty() || !duplicated_running_scopes.is_empty() {
        recommendations.push(json!({
            "action": "doctor_reset_soft",
            "endpoint": "/manual/doctor/reset",
            "payload": {
                "hard": false,
                "stop_running": true,
                "cancel_pending": true
            }
        }));
    }

    let (positions_result, usdc_result) = tokio::join!(
        state.api.get_wallet_positions_live(None),
        state.api.check_usdc_balance_allowance()
    );
    let (positions_count, position_fetch_error) = match positions_result {
        Ok(rows) => (rows.len(), None::<String>),
        Err(e) => (0usize, Some(e.to_string())),
    };
    let (usdc_balance, usdc_allowance, usdc_error) = match usdc_result {
        Ok((balance, allowance)) => (
            Some(f64::try_from(balance / Decimal::from(1_000_000u64)).unwrap_or(0.0)),
            Some(f64::try_from(allowance / Decimal::from(1_000_000u64)).unwrap_or(0.0)),
            None::<String>,
        ),
        Err(e) => (None, None, Some(e.to_string())),
    };

    Ok(Json(json!({
        "ok": true,
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "time_utc": Utc::now().to_rfc3339(),
        "wallet": state.api.trading_account_address().ok(),
        "run_summary": {
            "total": snapshots.len(),
            "running": running,
            "stopping": stopping,
            "completed": completed,
            "canceled": canceled,
            "failed": failed,
            "other": other
        },
        "health_flags": {
            "stale_stopping_runs": stale_stopping_runs,
            "error_run_count": error_runs.len(),
            "duplicated_running_scopes": duplicated_running_scopes,
        },
        "account_snapshot": {
            "usdc_balance": usdc_balance,
            "usdc_allowance": usdc_allowance,
            "usdc_error": usdc_error,
            "positions_count": positions_count,
            "positions_error": position_fetch_error
        },
        "recommendations": recommendations,
        "runs": if include_runs { json!(snapshots) } else { Value::Null }
    })))
}

async fn handle_manual_doctor_check(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    let started = Instant::now();
    let now_ms = Utc::now().timestamp_millis();
    let snapshots = collect_run_snapshots(&state).await;

    let mut issues = Vec::<Value>::new();
    let running_runs = snapshots
        .iter()
        .filter(|run| run.status.eq_ignore_ascii_case("running"))
        .map(|run| run.run_id.clone())
        .collect::<Vec<_>>();
    if !running_runs.is_empty() {
        issues.push(json!({
            "code": "running_manual_runs",
            "severity": "warn",
            "message": format!("{} manual run(s) still active", running_runs.len()),
            "run_ids": running_runs,
        }));
    }

    let stale_stopping_runs = snapshots
        .iter()
        .filter(|run| run.status.eq_ignore_ascii_case("stopping"))
        .filter(|run| now_ms.saturating_sub(run.updated_at_ms) > 30_000)
        .map(|run| run.run_id.clone())
        .collect::<Vec<_>>();
    if !stale_stopping_runs.is_empty() {
        issues.push(json!({
            "code": "stale_stopping_runs",
            "severity": "warn",
            "message": "stopping runs have not updated for >30s",
            "run_ids": stale_stopping_runs
        }));
    }

    let errored_runs = snapshots
        .iter()
        .filter(|run| {
            run.last_error
                .as_ref()
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
        })
        .map(|run| {
            json!({
                "run_id": run.run_id,
                "status": run.status,
                "last_error": run.last_error
            })
        })
        .collect::<Vec<_>>();
    if !errored_runs.is_empty() {
        issues.push(json!({
            "code": "errored_runs",
            "severity": "info",
            "message": format!("{} run(s) recorded errors", errored_runs.len()),
            "runs": errored_runs
        }));
    }

    let mut running_scope_counts: HashMap<(String, String, String), usize> = HashMap::new();
    for run in snapshots
        .iter()
        .filter(|run| run.status.eq_ignore_ascii_case("running"))
    {
        let key = (
            run.condition_id.clone(),
            run.token_id.clone(),
            run.timeframe.clone(),
        );
        *running_scope_counts.entry(key).or_insert(0) += 1;
    }
    let duplicated_running_scopes = running_scope_counts
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|((condition_id, token_id, timeframe), count)| {
            json!({
                "condition_id": condition_id,
                "token_id": token_id,
                "timeframe": timeframe,
                "count": count
            })
        })
        .collect::<Vec<_>>();
    if !duplicated_running_scopes.is_empty() {
        issues.push(json!({
            "code": "duplicate_running_scope",
            "severity": "warn",
            "message": "multiple active runs are targeting the same condition/token/timeframe",
            "scopes": duplicated_running_scopes
        }));
    }

    Ok(Json(json!({
        "ok": issues.is_empty(),
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "issue_count": issues.len(),
        "issues": issues,
        "suggested_fix": {
            "endpoint": "/manual/doctor/fix",
            "payload": {
                "stop_running": true,
                "cancel_pending": true,
                "sync_portfolio": true,
                "prune_completed": true
            }
        }
    })))
}

async fn handle_manual_doctor_fix(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ManualDoctorFixRequest>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    let started = Instant::now();
    let stop_running = payload.stop_running.unwrap_or(true);
    let cancel_pending = payload.cancel_pending.unwrap_or(true);
    let sync_portfolio = payload.sync_portfolio.unwrap_or(true);
    let prune_completed = payload.prune_completed.unwrap_or(true);

    let (runs, missing_run_ids) = find_run_handles(&state, payload.run_ids.as_ref()).await;

    let mut stop_requested = 0usize;
    let mut canceled_orders = 0usize;
    let mut canceled_scopes = 0usize;
    let mut canceled_errors = Vec::<Value>::new();
    let mut touched_run_ids = Vec::<String>::new();
    for run in runs {
        let snapshot = run.snapshot().await;
        touched_run_ids.push(snapshot.run_id.clone());
        let is_active = snapshot.status.eq_ignore_ascii_case("running")
            || snapshot.status.eq_ignore_ascii_case("stopping");
        if stop_running && is_active {
            run.request_stop();
            run.update(|snap| {
                if snap.status.eq_ignore_ascii_case("running") {
                    snap.status = "stopping".to_string();
                }
                snap.stop_reason = Some("doctor_fix_stop_requested".to_string());
            })
            .await;
            stop_requested = stop_requested.saturating_add(1);
        }
        if cancel_pending && snapshot.run_type.ends_with("chase_limit") {
            let strategy_id = strategy_id_for_manual_run(&snapshot);
            let side = side_for_manual_run(&snapshot);
            match state
                .trader
                .cancel_pending_orders_for_scope(
                    snapshot.period_timestamp,
                    snapshot.timeframe.as_str(),
                    strategy_id.as_str(),
                    EntryExecutionMode::Ladder,
                    snapshot.token_id.as_str(),
                    side,
                )
                .await
            {
                Ok(canceled) => {
                    canceled_scopes = canceled_scopes.saturating_add(1);
                    canceled_orders = canceled_orders.saturating_add(canceled);
                }
                Err(e) => {
                    canceled_errors.push(json!({
                        "run_id": snapshot.run_id,
                        "error": e.to_string()
                    }));
                }
            }
        }
    }

    let sync_error = if sync_portfolio {
        state
            .trader
            .sync_trades_with_portfolio()
            .await
            .err()
            .map(|e| e.to_string())
    } else {
        None
    };

    let mut pruned_runs = 0usize;
    if prune_completed {
        let snapshots = collect_run_snapshots(&state).await;
        let target_ids: Option<HashSet<String>> = payload.run_ids.as_ref().map(|ids| {
            ids.iter()
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
                .collect::<HashSet<_>>()
        });
        let prune_ids = snapshots
            .iter()
            .filter(|run| {
                run.status.eq_ignore_ascii_case("completed")
                    || run.status.eq_ignore_ascii_case("canceled")
                    || run.status.eq_ignore_ascii_case("failed")
            })
            .filter(|run| {
                target_ids
                    .as_ref()
                    .map(|ids| ids.contains(run.run_id.as_str()))
                    .unwrap_or(true)
            })
            .map(|run| run.run_id.clone())
            .collect::<Vec<_>>();
        if !prune_ids.is_empty() {
            let mut registry = state.run_registry.write().await;
            let before = registry.len();
            for run_id in prune_ids {
                registry.remove(run_id.as_str());
            }
            pruned_runs = before.saturating_sub(registry.len());
        }
    }

    Ok(Json(json!({
        "ok": canceled_errors.is_empty() && sync_error.is_none(),
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "touched_run_ids": touched_run_ids,
        "missing_run_ids": missing_run_ids,
        "stop_requested": stop_requested,
        "cancel_pending_scopes_attempted": canceled_scopes,
        "cancel_pending_orders": canceled_orders,
        "cancel_errors": canceled_errors,
        "sync_portfolio_enabled": sync_portfolio,
        "sync_error": sync_error,
        "pruned_runs": pruned_runs
    })))
}

async fn handle_manual_doctor_reset(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ManualDoctorResetRequest>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    let started = Instant::now();
    let hard = payload.hard.unwrap_or(false);
    let stop_running = payload.stop_running.unwrap_or(true);
    let cancel_pending = payload.cancel_pending.unwrap_or(true);

    let snapshots = collect_run_snapshots(&state).await;
    let mut stop_requested = 0usize;
    let mut canceled_orders = 0usize;
    let mut cancel_errors = Vec::<Value>::new();
    for run_snapshot in &snapshots {
        let is_active = run_snapshot.status.eq_ignore_ascii_case("running")
            || run_snapshot.status.eq_ignore_ascii_case("stopping");
        if stop_running && is_active {
            let run_opt = {
                let registry = state.run_registry.read().await;
                registry.get(run_snapshot.run_id.as_str()).cloned()
            };
            if let Some(run) = run_opt {
                run.request_stop();
                run.update(|snap| {
                    if snap.status.eq_ignore_ascii_case("running") {
                        snap.status = "stopping".to_string();
                    }
                    snap.stop_reason = Some("doctor_reset_stop_requested".to_string());
                })
                .await;
                stop_requested = stop_requested.saturating_add(1);
            }
        }
        if cancel_pending && run_snapshot.run_type.ends_with("chase_limit") {
            let strategy_id = strategy_id_for_manual_run(run_snapshot);
            let side = side_for_manual_run(run_snapshot);
            match state
                .trader
                .cancel_pending_orders_for_scope(
                    run_snapshot.period_timestamp,
                    run_snapshot.timeframe.as_str(),
                    strategy_id.as_str(),
                    EntryExecutionMode::Ladder,
                    run_snapshot.token_id.as_str(),
                    side,
                )
                .await
            {
                Ok(canceled) => {
                    canceled_orders = canceled_orders.saturating_add(canceled);
                }
                Err(e) => {
                    cancel_errors.push(json!({
                        "run_id": run_snapshot.run_id,
                        "error": e.to_string()
                    }));
                }
            }
        }
    }

    let removed_runs;
    let remaining_runs;
    {
        let mut registry = state.run_registry.write().await;
        let before = registry.len();
        if hard {
            registry.clear();
        } else {
            registry.retain(|_, run| {
                if let Ok(snap) = run.snapshot.try_read() {
                    let status = snap.status.to_ascii_lowercase();
                    !(status == "completed" || status == "canceled" || status == "failed")
                } else {
                    true
                }
            });
        }
        remaining_runs = registry.len();
        removed_runs = before.saturating_sub(remaining_runs);
    }

    Ok(Json(json!({
        "ok": cancel_errors.is_empty(),
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "hard_reset": hard,
        "stop_requested": stop_requested,
        "canceled_orders": canceled_orders,
        "cancel_errors": cancel_errors,
        "removed_runs": removed_runs,
        "remaining_runs": remaining_runs
    })))
}

async fn handle_health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &state)?;
    let ui_health = UiManualHealth {
        status: "ok".to_string(),
        mode: if state.simulation_mode {
            "dry_run".to_string()
        } else {
            "live".to_string()
        },
        message: if state.simulation_mode {
            "Manual trading service is ready in dry run mode.".to_string()
        } else {
            "Manual trading service is ready.".to_string()
        },
        ready: true,
    };
    Ok(Json(json!({
        "ok": true,
        "service": "manual_bot",
        "time_utc": Utc::now().to_rfc3339(),
        "wallet": state.api.trading_account_address().ok(),
        "ui_health": ui_health,
    })))
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    let args = Args::parse();
    let addr = SocketAddr::from_str(format!("{}:{}", args.bind, args.port).as_str())
        .with_context(|| format!("invalid bind address {}:{}", args.bind, args.port))?;
    let auth_token = args
        .token
        .as_ref()
        .map(|v| v.trim().to_string())
        .or_else(|| std::env::var("EVPOLY_MANUAL_BOT_TOKEN").ok())
        .filter(|v| !v.is_empty());
    if auth_token.is_none() && manual_auth_required_for_bind(addr) {
        anyhow::bail!(
            "EVPOLY_MANUAL_BOT_TOKEN or --token is required when manual_bot bind is not loopback"
        );
    }

    let config = Config::load(&args.config)
        .with_context(|| format!("failed to load config: {}", args.config.display()))?;

    let api = Arc::new(PolymarketApi::new(
        config.polymarket.gamma_api_url.clone(),
        config.polymarket.clob_api_url.clone(),
        config.polymarket.private_key.clone(),
        config.polymarket.proxy_wallet_address.clone(),
        config.polymarket.signature_type,
    ));

    if !args.simulation {
        api.authenticate()
            .await
            .context("manual_bot authentication failed")?;
    }

    let polymarket_ws_state = if api.ws_enabled() && !args.simulation {
        let ws_state = new_shared_polymarket_ws_state();
        let ws_cfg = PolymarketWsConfig::default();
        api.attach_ws_state(ws_state.clone()).await;
        let _manual_ws_task =
            polymarket_ws::spawn_polymarket_ws_bridge(api.clone(), ws_cfg, ws_state.clone());
        log::info!(
            "manual_bot Polymarket WSS bridge started (market_stale={}ms order_stale={}ms)",
            api.ws_market_stale_ms(),
            api.ws_order_stale_ms()
        );
        Some(ws_state)
    } else {
        if args.simulation {
            log::info!("manual_bot Polymarket WSS bridge disabled in simulation mode");
        } else {
            log::info!("manual_bot Polymarket WSS bridge disabled by EVPOLY_PM_WS_ENABLE=false");
        }
        None
    };

    let trader = Arc::new(Trader::new(
        api.clone(),
        config.trading.clone(),
        args.simulation,
        None,
        None,
        None,
    )?);

    let state = AppState {
        api,
        trader,
        auth_token: auth_token.clone(),
        simulation_mode: args.simulation,
        polymarket_ws_state,
        run_registry: Arc::new(RwLock::new(HashMap::new())),
        scope_claims: Arc::new(RwLock::new(HashMap::new())),
    };

    let app = Router::new()
        .route("/health", get(handle_health))
        .route("/manual/health", get(handle_health))
        .route("/manual/markets/search", get(handle_manual_market_search))
        .route("/manual/markets/recent", get(handle_manual_market_recent))
        .route(
            "/manual/markets/by-slug/{slug}",
            get(handle_manual_market_by_slug),
        )
        .route(
            "/manual/markets/{condition_id}",
            get(handle_manual_market_by_condition),
        )
        .route("/manual/premarket", post(handle_manual_premarket))
        .route("/manual/open", post(handle_manual_open))
        .route("/manual/close", post(handle_manual_close))
        .route("/manual/open/stop", post(handle_manual_open_stop))
        .route("/manual/close/stop", post(handle_manual_close_stop))
        .route("/manual/open/runs", get(handle_manual_open_runs))
        .route("/manual/close/runs", get(handle_manual_close_runs))
        .route("/manual/open/runs/{run_id}", get(handle_manual_open_run))
        .route(
            "/manual/open/runs/{run_id}/stop",
            post(handle_manual_open_stop_path),
        )
        .route("/manual/close/runs/{run_id}", get(handle_manual_close_run))
        .route(
            "/manual/close/runs/{run_id}/stop",
            post(handle_manual_close_stop_path),
        )
        .route(
            "/manual/auto-redeem",
            get(handle_manual_auto_redeem_status).post(handle_manual_auto_redeem_set),
        )
        .route("/manual/redeem", post(handle_manual_redeem))
        .route("/manual/merge", post(handle_manual_merge))
        .route("/manual/positions", get(handle_manual_positions))
        .route("/manual/balance", get(handle_manual_balance))
        .route("/manual/pnl", get(handle_manual_pnl))
        .route("/manual/audit", get(handle_manual_audit))
        .route("/manual/doctor/check", get(handle_manual_doctor_check))
        .route("/manual/doctor/fix", post(handle_manual_doctor_fix))
        .route("/manual/doctor/reset", post(handle_manual_doctor_reset))
        .with_state(state)
        .layer(axum::extract::DefaultBodyLimit::max(args.max_body_bytes));

    log::info!(
        "manual_bot listening on {} (simulation={} auth_token={})",
        addr,
        args.simulation,
        if auth_token.as_ref().map(|v| !v.is_empty()).unwrap_or(false) {
            "enabled"
        } else {
            "disabled"
        }
    );

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {}", addr))?;
    axum::serve(listener, app)
        .await
        .context("manual_bot server failed")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn manual_auth_is_required_for_public_bind() {
        let public = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_PORT);
        let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT);

        assert!(manual_auth_required_for_bind(public));
        assert!(!manual_auth_required_for_bind(loopback));
    }
}
