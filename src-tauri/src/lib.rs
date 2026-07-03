#![recursion_limit = "256"]

pub mod auth;
pub mod bot_manager;
pub mod config_io;
pub mod crypto_vault;
pub mod geo_access;
pub mod liquidity_rewards;
pub mod log_stream;
pub mod onboard;
pub mod portfolio_api;
pub mod profile_manager;
pub mod wallet_rpc;
pub mod wallet_sync;

use crate::auth::AppAuth;
use crate::bot_manager::BotManager;
use crate::geo_access::GeoAccessStatus;
use crate::liquidity_rewards::{LiquidityRewardsCacheEntry, LiquidityRewardsQuery};
use crate::profile_manager::{Profile, ProfileManager};
use crate::wallet_sync::{WalletSyncManager, WalletSyncRuntimeConfig};

use alloy_primitives::Address as AlloyAddress;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ethers_signers::{LocalWallet, Signer};
use polymarket_client_sdk_v2::{derive_proxy_wallet, derive_safe_wallet, POLYGON};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;
use serde_json::{Map, Value};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

struct AppDataDir(PathBuf);
#[derive(Default)]
struct MarketMetadataState(Mutex<HashMap<String, MarketMetadata>>);
#[derive(Default)]
struct LiquidityRewardsState(Mutex<Option<LiquidityRewardsCacheEntry>>);
#[derive(Default)]
struct HomeOverviewCacheState(Mutex<HomeOverviewCache>);

type AuthState = Arc<Mutex<AppAuth>>;
type ProfileState = Arc<Mutex<ProfileManager>>;
type BotState = Arc<Mutex<BotManager>>;
type WalletSyncState = Arc<Mutex<WalletSyncManager>>;

const DESKTOP_SYMBOL_ORDER: [&str; 7] = ["BTC", "ETH", "SOL", "XRP", "DOGE", "BNB", "HYPE"];
const CORE_STRATEGY_SYMBOLS: [&str; 4] = ["BTC", "ETH", "SOL", "XRP"];
const DESKTOP_SECRET_KEYS: &[&str] = &[
    "POLY_PRIVATE_KEY",
    "RELAYER_API_KEY",
    "RELAYER_API_KEY_ADDRESS",
    "EVPOLY_ALPHA_KEY",
    "EVPOLY_RELAYER_REMOTE_SIGNER_TOKEN",
    "EVPOLY_RELAYER_SUBMIT_SIGNER_URL",
    "EVPOLY_WALLET_BINDING",
    "EVPOLY_ONBOARDING_STATUS",
    "EVPOLY_APPROVAL_STATUS",
    "EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN",
    "EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN",
    "EVPOLY_REMOTE_MARKET_DISCOVERY_TOKEN",
    "EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN",
    "EVPOLY_REMOTE_EVCURVE_ALPHA_TOKEN",
    "EVPOLY_REMOTE_SESSIONBAND_ALPHA_TOKEN",
    "EVPOLY_REMOTE_MM_REWARDS_ALPHA_TOKEN",
    "EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TOKEN",
    "EVPOLY_ADMIN_API_TOKEN",
];
const OBSOLETE_PREMARKET_REMOTE_ALPHA_KEYS: &[&str] = &[
    "EVPOLY_REMOTE_PREMARKET_ALPHA_URL",
    "EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN",
];
const DESKTOP_DEBUG_LOG_NAME: &str = "evpoly-desktop-debug.log.txt";
const FULL_DEBUG_LOG_NAME: &str = "evpoly-full-debug.log.txt";
const BOT_DEBUG_LOG_NAME: &str = "evpoly-debug.log.txt";
const DEFAULT_DESKTOP_MAGIC_BRIDGE_BASE_URL: &str = "https://api-web.evplus.ai";
const DEFAULT_POLYMARKET_BRIDGE_BASE_URL: &str = "https://bridge.polymarket.com";
const HOME_DB_REFRESH_MS: i64 = 15_000;
const HOME_REMOTE_REFRESH_MS: i64 = 60_000;
const HOME_REMOTE_ERROR_REFRESH_MS: i64 = 15_000;
const TRADE_STATS_REFRESH_MS: i64 = 60_000;

#[derive(Clone, Default)]
struct MarketMetadata {
    title: String,
    outcomes_by_token: HashMap<String, String>,
    thumbnail_url: Option<String>,
    market_slug: Option<String>,
    event_slug: Option<String>,
}

#[derive(serde::Deserialize)]
struct ClobMarketToken {
    token_id: String,
    outcome: Option<String>,
}

#[derive(serde::Deserialize)]
struct ClobMarketResponse {
    question: Option<String>,
    market_slug: Option<String>,
    #[serde(default)]
    tokens: Vec<ClobMarketToken>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GammaOptimizedImage {
    image_url_optimized: Option<String>,
}

#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GammaEventResponse {
    slug: Option<String>,
    image: Option<String>,
    icon: Option<String>,
    image_optimized: Option<GammaOptimizedImage>,
    icon_optimized: Option<GammaOptimizedImage>,
}

#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GammaMarketResponse {
    #[serde(rename = "conditionId", alias = "condition_id")]
    condition_id: Option<String>,
    question: Option<String>,
    slug: Option<String>,
    #[serde(rename = "eventSlug")]
    event_slug: Option<String>,
    image: Option<String>,
    icon: Option<String>,
    image_optimized: Option<GammaOptimizedImage>,
    icon_optimized: Option<GammaOptimizedImage>,
    #[serde(default)]
    events: Vec<GammaEventResponse>,
}

struct TradeActivityRecord {
    id: u64,
    timestamp: String,
    condition_id: String,
    token_id: String,
    side: String,
    price: f64,
    quantity: f64,
    notional_usd: f64,
    timeframe: Option<String>,
    period_timestamp: Option<i64>,
    token_type: Option<String>,
    asset_symbol: Option<String>,
}

#[derive(Clone, Default)]
struct HomeDbSummary {
    profile_id: String,
    wallet_address: String,
    fetched_at_ms: i64,
    pnl_today_utc: f64,
    ack_sample_count: u64,
    avg_ack_latency_ms: Option<f64>,
    recent_ack_warning_count: u64,
}

#[derive(Clone)]
struct HomeSlowOverviewSnapshot {
    profile_id: String,
    wallet_address: String,
    fetched_at_ms: i64,
    available_balance_result: Result<f64, String>,
    portfolio_value_result: Result<f64, String>,
    liquidity_rewards_today: Option<f64>,
    liquidity_rewards_lifetime: Option<f64>,
    liquidity_rewards_as_of_utc: Option<String>,
    liquidity_rewards_error: Option<String>,
}

impl HomeSlowOverviewSnapshot {
    fn has_error(&self) -> bool {
        self.available_balance_result.is_err()
            || self.portfolio_value_result.is_err()
            || self.liquidity_rewards_error.is_some()
    }
}

#[derive(Clone)]
struct HomeTradeStatsSnapshot {
    profile_id: String,
    wallet_address: String,
    fetched_at_ms: i64,
    value: Value,
}

#[derive(Clone, Default)]
struct HomeOverviewCache {
    db_summary: Option<HomeDbSummary>,
    slow_snapshot: Option<HomeSlowOverviewSnapshot>,
    trade_stats: Option<HomeTradeStatsSnapshot>,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopStrategies {
    premarket: bool,
    endgame: bool,
    evcurve: bool,
    session_band: bool,
    evsnipe: bool,
    mm_rewards: bool,
    mm_sport: bool,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopSizing {
    premarket: f64,
    endgame: f64,
    evcurve: f64,
    session_band: f64,
    evsnipe_per_hit: f64,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopCaps {
    premarket: f64,
    endgame: f64,
    evcurve: f64,
    session_band: f64,
    evsnipe: f64,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopMmTuning {
    rewards_min_share_multiple: f64,
    sport_quote_size_multiplier: f64,
    nonsport_quote_size_multiplier: f64,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopPremarketCancelAfterOpen {
    m5: f64,
    m15: f64,
    h1: f64,
    h4: f64,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopPremarketSettings {
    tp_enabled: bool,
    active_cap_per_asset: f64,
    #[serde(default = "default_premarket_timeframes")]
    timeframes: Vec<String>,
    #[serde(default = "default_premarket_ladder_mode_m5")]
    ladder_mode_m5: String,
    #[serde(default = "default_premarket_ladder_mode_non_m5")]
    ladder_mode_non_m5: String,
    #[serde(default = "default_premarket_safe_bias_pct")]
    safe_bias_pct: f64,
    #[serde(default = "default_premarket_aggressive_bias_pct")]
    aggressive_bias_pct: f64,
    cancel_after_open_sec: DesktopPremarketCancelAfterOpen,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopEndgameSettings {
    per_period_cap_usd: f64,
    tick0_multiplier: f64,
    tick1_multiplier: f64,
    tick2_multiplier: f64,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopEvcurveSettings {
    timeframes: Vec<String>,
    min_buy_price: f64,
    d1_enabled: bool,
    d1_cap_usd: f64,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopSessionBandSettings {
    timeframes: Vec<String>,
    flip_threshold_pct: f64,
    tau2_enabled: bool,
    tau1_enabled: bool,
    tau2_multiplier: f64,
    tau1_multiplier: f64,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopEvsnipeSettings {
    pre_hit_enabled: bool,
    pre_leg_ratio: f64,
    saved_pre_leg_ratio: f64,
    pre_trigger_bps: f64,
    strike_window_pct: f64,
    max_days_to_expiry: f64,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopMmRewardsSettings {
    market_mode: String,
    single_market_slugs: String,
    auto_top_n: f64,
    auto_refresh_sec: f64,
    auto_rank_budget_usd: f64,
    blacklist_keywords: String,
    reward_min_shares_cap: f64,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopMmSportSettings {
    discovery_route: String,
    quote_size_mode: String,
    nonsport_quote_size_mode: String,
    entry_price_mode: String,
    min_reward_rate_per_day: f64,
    match_only: bool,
    allowed_sport_league_codes: String,
    blocked_sport_league_codes: String,
    blocked_competition_levels: String,
    market_allowlist_keywords: String,
    market_blacklist_keywords: String,
    reward_min_shares_cap: f64,
    polymarket_live_guard_enable: bool,
    polymarket_live_guard_ws_enable: bool,
    polymarket_live_guard_ws_stale_ms: f64,
    pause_after_fill_sec: f64,
    inventory_exit_start_hours: f64,
    nonsport_end_exit_start_hours: f64,
    sport_entry_schedule_enabled: bool,
    sport_entry_schedule_days_utc: String,
    sport_entry_schedule_start_minute_utc: f64,
    sport_entry_schedule_end_minute_utc: f64,
    nonsport_entry_schedule_enabled: bool,
    nonsport_entry_schedule_days_utc: String,
    nonsport_entry_schedule_start_minute_utc: f64,
    nonsport_entry_schedule_end_minute_utc: f64,
    inventory_exit_max_loss_cents: f64,
    inventory_exit_mode: String,
    max_share_ratio: f64,
    nonsport_max_share_ratio: f64,
    max_quote_shares: f64,
    nonsport_max_quote_shares: f64,
    min_top_depth_usd: f64,
    nonsport_min_top_depth_usd: f64,
    min_entry_top_bid_price: f64,
    allow_sponsored_rewards: bool,
    sponsored_reward_min_share: f64,
    quote_expiry_min_sec: f64,
    quote_expiry_max_sec: f64,
    quote_cooldown_min_sec: f64,
    quote_cooldown_max_sec: f64,
    fifo_max_share_ratio: f64,
    active_sport_market_cap: f64,
    active_nonsport_market_cap: f64,
}

fn normalize_mm_rewards_market_mode(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "hybrid" => "hybrid",
        _ => "auto",
    }
}

fn normalize_mm_sport_quote_size_mode(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "depth_ratio" | "depth-ratio" | "ratio" => "depth_ratio",
        _ => "multiple",
    }
}

fn normalize_mm_sport_entry_price_mode(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "passive" => "passive",
        _ => "best_bid",
    }
}

fn normalize_mm_sport_discovery_route(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "nonsports" | "non-sports" | "non_sports" => "nonsports",
        "dual" => "dual",
        _ => "sports",
    }
}

fn mm_sport_route_default_caps(route: &str) -> (f64, f64) {
    match normalize_mm_sport_discovery_route(route) {
        "nonsports" => (0.0, 100.0),
        "dual" => (50.0, 50.0),
        _ => (100.0, 0.0),
    }
}

fn normalize_nonnegative_integer_f64(value: f64, default: f64) -> f64 {
    if value.is_finite() && value >= 0.0 {
        value.floor()
    } else {
        default
    }
}

fn normalize_utc_minute_f64(value: f64, default: f64) -> f64 {
    normalize_nonnegative_integer_f64(value, default).min(1439.0)
}

fn normalize_cooldown_pair_f64(min_value: f64, max_value: f64) -> (f64, f64) {
    let min_value = normalize_nonnegative_integer_f64(min_value, 10.0);
    let max_value = normalize_nonnegative_integer_f64(max_value, 60.0).max(min_value);
    (min_value, max_value)
}

fn normalize_mm_sport_exit_mode(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "aggressive" => "aggressive",
        "no_exit" | "no-exit" | "hold" => "no_exit",
        _ => "normal",
    }
}

#[derive(Clone, serde::Deserialize)]
struct DesktopSymbolMultipliers {
    btc: f64,
    eth: f64,
    sol: f64,
    xrp: f64,
    doge: f64,
    bnb: f64,
    hype: f64,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopPremarketTimeframeMultipliers {
    m5: f64,
    m15: f64,
    h1: f64,
    h4: f64,
    d1: f64,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopEvcurveTimeframeMultipliers {
    m15: f64,
    h1: f64,
    h4: f64,
    d1: f64,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopSizePolicy {
    symbol_multipliers: DesktopSymbolMultipliers,
    premarket_timeframe_multipliers: DesktopPremarketTimeframeMultipliers,
    evcurve_timeframe_multipliers: DesktopEvcurveTimeframeMultipliers,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopStrategySettings {
    premarket: DesktopPremarketSettings,
    endgame: DesktopEndgameSettings,
    evcurve: DesktopEvcurveSettings,
    session_band: DesktopSessionBandSettings,
    evsnipe: DesktopEvsnipeSettings,
    mm_rewards: DesktopMmRewardsSettings,
    mm_sport: DesktopMmSportSettings,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopConfig {
    private_key: String,
    eoa_wallet: String,
    proxy_wallet: String,
    #[serde(default)]
    deposit_wallet: String,
    sig_type: u8,
    #[serde(default = "default_weekend_policy")]
    weekend_policy: String,
    symbols: Vec<String>,
    strategies: DesktopStrategies,
    sizing: DesktopSizing,
    caps: DesktopCaps,
    mm_tuning: DesktopMmTuning,
    size_policy: DesktopSizePolicy,
    strategy_settings: DesktopStrategySettings,
    simulation: bool,
    #[serde(default)]
    alpha_key: String,
    relayer_api_key: String,
    relayer_api_key_address: String,
    #[serde(default)]
    relayer_remote_signer_token: String,
    #[serde(default)]
    relayer_submit_signer_url: String,
    #[serde(default)]
    wallet_binding: String,
    #[serde(default)]
    onboarding_status: String,
    #[serde(default)]
    approval_status: String,
    #[serde(default)]
    remote_signer_token: String,
    #[serde(default)]
    order_signer_primary_token_internal: String,
    #[serde(default)]
    remote_discovery_token: String,
    #[serde(default)]
    remote_endgame_alpha_token: String,
    #[serde(default)]
    remote_mm_rewards_alpha_token: String,
    #[serde(default)]
    remote_evsnipe_discovery_token: String,
    #[serde(default)]
    admin_api_token: String,
}

#[derive(Clone, serde::Serialize)]
struct SetupDoctorItem {
    key: String,
    label: String,
    status: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    strategy: Option<String>,
}

#[derive(Clone, serde::Serialize)]
struct SetupDoctorPopup {
    title: String,
    body: String,
    cta_label: String,
    cta_target: String,
}

#[derive(Clone, serde::Serialize)]
struct SetupDoctorResult {
    status: String,
    items: Vec<SetupDoctorItem>,
    fixed_count: usize,
    missing_user_count: usize,
    bot_was_running: bool,
    bot_restarted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    popup: Option<SetupDoctorPopup>,
}

#[derive(Clone, serde::Serialize)]
struct DerivedPolymarketFunders {
    eoa_wallet: String,
    proxy_wallet: Option<String>,
    safe_wallet: String,
    deposit_wallet: Option<String>,
}

#[derive(Default)]
struct SetupDoctorAudit {
    items: Vec<SetupDoctorItem>,
    blocking_user_labels: Vec<String>,
    missing_user_labels: Vec<String>,
    missing_generated_labels: Vec<String>,
    missing_generated_keys: Vec<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct ExportBundleV2 {
    version: u8,
    profile: PortableProfile,
    secrets: HashMap<String, String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct PortableProfile {
    name: String,
    eoa_wallet_address: String,
    proxy_wallet_address: String,
    #[serde(default)]
    deposit_wallet_address: String,
    signature_type: u8,
    strategy_config: Value,
    sizing_config: Value,
}

fn iso_from_ms(ms: i64) -> String {
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
}

fn bool_to_json(v: bool) -> Value {
    Value::Bool(v)
}

fn number_to_json(v: f64) -> Value {
    serde_json::json!(v.max(0.0))
}

fn signed_number_to_json(v: f64) -> Value {
    serde_json::json!(if v.is_finite() { v } else { 0.0 })
}

fn bool_from_object(obj: &Map<String, Value>, key: &str, default: bool) -> bool {
    obj.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn f64_from_object(obj: &Map<String, Value>, key: &str, default: f64) -> f64 {
    obj.get(key)
        .and_then(|value| {
            value.as_f64().or_else(|| {
                value
                    .as_str()
                    .and_then(|raw| raw.trim().parse::<f64>().ok())
            })
        })
        .unwrap_or(default)
}

fn string_from_object(obj: &Map<String, Value>, key: &str, default: &str) -> String {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
        .to_string()
}

fn csv_list(value: Option<String>, fallback: &[&str]) -> Vec<String> {
    value
        .map(|entry| {
            entry
                .split(',')
                .map(|part| part.trim().to_string())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|entries| !entries.is_empty())
        .unwrap_or_else(|| fallback.iter().map(|entry| (*entry).to_string()).collect())
}

fn csv_from_object(obj: &Map<String, Value>, key: &str, fallback: &[&str]) -> Vec<String> {
    csv_list(
        obj.get(key)
            .and_then(Value::as_str)
            .map(|value| value.to_string())
            .or_else(|| config_io::env_template_default_string(key)),
        fallback,
    )
}

const PREMARKET_LADDER_MODE_ENV_KEY_5M: &str = "EVPOLY_PREMARKET_LADDER_MODE_5M";
const PREMARKET_LADDER_MODE_ENV_KEY_NON_M5: &str = "EVPOLY_PREMARKET_LADDER_MODE_NON_M5";
const PREMARKET_LADDER_MODE_ENV_KEY_NON_M5_LEGACY: &str = "EVPOLY_PREMARKET_LADDER_MODE_NON_5M";
const PREMARKET_LADDER_MODE_ENV_KEY_SHARED: &str = "EVPOLY_PREMARKET_LADDER_MODE";
const PREMARKET_SAFE_BIAS_PCT_ENV_KEY: &str = "EVPOLY_PREMARKET_SAFE_BIAS_PCT";
const PREMARKET_AGGRESSIVE_BIAS_PCT_ENV_KEY: &str = "EVPOLY_PREMARKET_AGGRESSIVE_BIAS_PCT";
const PREMARKET_LADDER_MODE_NORMAL: &str = "normal";
const PREMARKET_LADDER_MODE_SAFE: &str = "safe";
const PREMARKET_LADDER_MODE_AGGRESSIVE: &str = "aggressive";
const WEEKEND_POLICY_ENV_KEY: &str = "EVPOLY_WEEKEND_POLICY";
const PREMARKET_LEGACY_LADDER_KEYS: [&str; 4] = [
    PREMARKET_LADDER_MODE_ENV_KEY_SHARED,
    PREMARKET_LADDER_MODE_ENV_KEY_NON_M5_LEGACY,
    "EVPOLY_PREMARKET_FIXED_LADDER_PRICES",
    "EVPOLY_PREMARKET_FIXED_LADDER_WEIGHTS",
];

fn normalize_weekend_policy(value: Option<&str>) -> String {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(raw) if raw.eq_ignore_ascii_case("pause") => "pause".to_string(),
        _ => "off".to_string(),
    }
}

fn default_weekend_policy() -> String {
    normalize_weekend_policy(
        config_io::env_template_default_string(WEEKEND_POLICY_ENV_KEY).as_deref(),
    )
}

fn default_premarket_timeframes() -> Vec<String> {
    csv_list(
        config_io::env_template_default_string("EVPOLY_PREMARKET_TIMEFRAMES"),
        &["5m", "15m", "1h", "4h"],
    )
}

fn default_endgame_timeframes() -> Vec<String> {
    vec!["5m".to_string(), "15m".to_string()]
}

fn normalize_premarket_ladder_mode(value: Option<&str>) -> String {
    match value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some(PREMARKET_LADDER_MODE_SAFE) => PREMARKET_LADDER_MODE_SAFE.to_string(),
        Some(PREMARKET_LADDER_MODE_AGGRESSIVE) => PREMARKET_LADDER_MODE_AGGRESSIVE.to_string(),
        _ => PREMARKET_LADDER_MODE_NORMAL.to_string(),
    }
}

fn default_premarket_ladder_mode_m5() -> String {
    normalize_premarket_ladder_mode(
        config_io::env_template_default_string(PREMARKET_LADDER_MODE_ENV_KEY_5M)
            .or_else(|| {
                config_io::env_template_default_string(PREMARKET_LADDER_MODE_ENV_KEY_SHARED)
            })
            .as_deref(),
    )
}

fn default_premarket_ladder_mode_non_m5() -> String {
    normalize_premarket_ladder_mode(
        config_io::env_template_default_string(PREMARKET_LADDER_MODE_ENV_KEY_NON_M5)
            .or_else(|| {
                config_io::env_template_default_string(PREMARKET_LADDER_MODE_ENV_KEY_NON_M5_LEGACY)
            })
            .or_else(|| {
                config_io::env_template_default_string(PREMARKET_LADDER_MODE_ENV_KEY_SHARED)
            })
            .as_deref(),
    )
}

fn normalize_premarket_ladder_bias_pct(value: f64, default: f64) -> f64 {
    if value.is_finite() {
        value.clamp(-90.0, 200.0)
    } else {
        default
    }
}

fn default_premarket_safe_bias_pct() -> f64 {
    normalize_premarket_ladder_bias_pct(
        config_io::env_template_default_f64(PREMARKET_SAFE_BIAS_PCT_ENV_KEY, -10.0),
        -10.0,
    )
}

fn default_premarket_aggressive_bias_pct() -> f64 {
    normalize_premarket_ladder_bias_pct(
        config_io::env_template_default_f64(PREMARKET_AGGRESSIVE_BIAS_PCT_ENV_KEY, 10.0),
        10.0,
    )
}

fn default_desktop_config(eoa_wallet: String, proxy_wallet: String, sig_type: u8) -> DesktopConfig {
    let mm_sport_discovery_route = normalize_mm_sport_discovery_route(
        config_io::env_template_default_string("EVPOLY_MM_SPORT_DISCOVERY_ROUTE")
            .as_deref()
            .unwrap_or("sports"),
    )
    .to_string();
    let (default_active_sport_market_cap, default_active_nonsport_market_cap) =
        mm_sport_route_default_caps(mm_sport_discovery_route.as_str());
    let (quote_cooldown_min_sec, quote_cooldown_max_sec) = normalize_cooldown_pair_f64(
        config_io::env_template_default_f64("EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MIN_SEC", 10.0),
        config_io::env_template_default_f64("EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MAX_SEC", 60.0),
    );
    let active_sport_market_cap = normalize_nonnegative_integer_f64(
        config_io::env_template_default_f64(
            "EVPOLY_MM_SPORT_ACTIVE_SPORT_MARKET_CAP",
            default_active_sport_market_cap,
        ),
        default_active_sport_market_cap,
    );
    let active_nonsport_market_cap = normalize_nonnegative_integer_f64(
        config_io::env_template_default_f64(
            "EVPOLY_MM_SPORT_ACTIVE_NONSPORT_MARKET_CAP",
            default_active_nonsport_market_cap,
        ),
        default_active_nonsport_market_cap,
    );

    DesktopConfig {
        private_key: String::new(),
        eoa_wallet,
        proxy_wallet,
        deposit_wallet: String::new(),
        sig_type,
        weekend_policy: default_weekend_policy(),
        symbols: DESKTOP_SYMBOL_ORDER
            .iter()
            .map(|symbol| (*symbol).to_string())
            .collect(),
        strategies: DesktopStrategies {
            premarket: config_io::env_template_default_bool(
                "EVPOLY_STRATEGY_PREMARKET_ENABLE",
                true,
            ),
            endgame: config_io::env_template_default_bool("EVPOLY_STRATEGY_ENDGAME_ENABLE", true),
            evcurve: config_io::env_template_default_bool("EVPOLY_STRATEGY_EVCURVE_ENABLE", false),
            session_band: false,
            evsnipe: config_io::env_template_default_bool("EVPOLY_STRATEGY_EVSNIPE_ENABLE", true),
            mm_rewards: false,
            mm_sport: config_io::env_template_default_bool(
                "EVPOLY_STRATEGY_MM_SPORT_ENABLE",
                false,
            ),
        },
        sizing: DesktopSizing {
            premarket: config_io::env_template_default_f64("EVPOLY_PREMARKET_BASE_SIZE_USD", 10.0),
            endgame: config_io::env_template_default_f64("EVPOLY_ENDGAME_BASE_SIZE_USD", 50.0),
            evcurve: config_io::env_template_default_f64("EVPOLY_EVCURVE_BASE_SIZE_USD", 10.0),
            session_band: config_io::env_template_default_f64(
                "EVPOLY_SESSIONBAND_BASE_SIZE_USD",
                10.0,
            ),
            evsnipe_per_hit: config_io::env_template_default_f64("EVPOLY_EVSNIPE_SIZE_USD", 5.0),
        },
        caps: DesktopCaps {
            premarket: 100000.0,
            endgame: 100000.0,
            evcurve: 100000.0,
            session_band: 100000.0,
            evsnipe: 10000.0,
        },
        mm_tuning: DesktopMmTuning {
            rewards_min_share_multiple: 1.0,
            sport_quote_size_multiplier: config_io::env_template_default_f64(
                "EVPOLY_MM_SPORT_QUOTE_SIZE_MULT",
                1.2,
            ),
            nonsport_quote_size_multiplier: config_io::env_template_default_f64(
                "EVPOLY_MM_SPORT_NONSPORT_QUOTE_SIZE_MULT",
                config_io::env_template_default_f64("EVPOLY_MM_SPORT_QUOTE_SIZE_MULT", 1.2),
            ),
        },
        size_policy: DesktopSizePolicy {
            symbol_multipliers: DesktopSymbolMultipliers {
                btc: config_io::env_template_default_f64("EVPOLY_SYMBOL_SIZE_MULTIPLIER_BTC", 1.0),
                eth: config_io::env_template_default_f64("EVPOLY_SYMBOL_SIZE_MULTIPLIER_ETH", 0.8),
                sol: config_io::env_template_default_f64("EVPOLY_SYMBOL_SIZE_MULTIPLIER_SOL", 0.5),
                xrp: config_io::env_template_default_f64("EVPOLY_SYMBOL_SIZE_MULTIPLIER_XRP", 0.5),
                doge: config_io::env_template_default_f64(
                    "EVPOLY_SYMBOL_SIZE_MULTIPLIER_DOGE",
                    0.5,
                ),
                bnb: config_io::env_template_default_f64("EVPOLY_SYMBOL_SIZE_MULTIPLIER_BNB", 0.5),
                hype: config_io::env_template_default_f64(
                    "EVPOLY_SYMBOL_SIZE_MULTIPLIER_HYPE",
                    0.5,
                ),
            },
            premarket_timeframe_multipliers: DesktopPremarketTimeframeMultipliers {
                m5: config_io::env_template_default_f64(
                    "EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_5M",
                    0.75,
                ),
                m15: config_io::env_template_default_f64(
                    "EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_15M",
                    1.0,
                ),
                h1: config_io::env_template_default_f64(
                    "EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_1H",
                    1.25,
                ),
                h4: config_io::env_template_default_f64(
                    "EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_4H",
                    1.25,
                ),
                d1: config_io::env_template_default_f64(
                    "EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_1D",
                    1.25,
                ),
            },
            evcurve_timeframe_multipliers: DesktopEvcurveTimeframeMultipliers {
                m15: config_io::env_template_default_f64(
                    "EVPOLY_EVCURVE_TIMEFRAME_MULTIPLIER_15M",
                    0.75,
                ),
                h1: config_io::env_template_default_f64(
                    "EVPOLY_EVCURVE_TIMEFRAME_MULTIPLIER_1H",
                    1.0,
                ),
                h4: config_io::env_template_default_f64(
                    "EVPOLY_EVCURVE_TIMEFRAME_MULTIPLIER_4H",
                    1.25,
                ),
                d1: config_io::env_template_default_f64(
                    "EVPOLY_EVCURVE_TIMEFRAME_MULTIPLIER_1D",
                    1.25,
                ),
            },
        },
        strategy_settings: DesktopStrategySettings {
            premarket: DesktopPremarketSettings {
                tp_enabled: config_io::env_template_default_bool(
                    "EVPOLY_PREMARKET_TP_ENABLE",
                    true,
                ),
                active_cap_per_asset: config_io::env_template_default_f64(
                    "EVPOLY_PREMARKET_ACTIVE_CAP_PER_ASSET",
                    100.0,
                ),
                timeframes: default_premarket_timeframes(),
                ladder_mode_m5: default_premarket_ladder_mode_m5(),
                ladder_mode_non_m5: default_premarket_ladder_mode_non_m5(),
                safe_bias_pct: default_premarket_safe_bias_pct(),
                aggressive_bias_pct: default_premarket_aggressive_bias_pct(),
                cancel_after_open_sec: DesktopPremarketCancelAfterOpen {
                    m5: config_io::env_template_default_f64(
                        "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_5M_SEC",
                        20.0,
                    ),
                    m15: config_io::env_template_default_f64(
                        "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_15M_SEC",
                        40.0,
                    ),
                    h1: config_io::env_template_default_f64(
                        "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_1H_SEC",
                        60.0,
                    ),
                    h4: config_io::env_template_default_f64(
                        "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_4H_SEC",
                        180.0,
                    ),
                },
            },
            endgame: DesktopEndgameSettings {
                per_period_cap_usd: config_io::env_template_default_f64(
                    "EVPOLY_ENDGAME_PER_PERIOD_CAP_USD",
                    10000.0,
                ),
                tick0_multiplier: 0.20,
                tick1_multiplier: 0.40,
                tick2_multiplier: 0.40,
            },
            evcurve: DesktopEvcurveSettings {
                timeframes: csv_list(
                    config_io::env_template_default_string("EVPOLY_EVCURVE_TIMEFRAMES"),
                    &["15m", "1h", "4h", "1d"],
                ),
                min_buy_price: config_io::env_template_default_f64(
                    "EVPOLY_EVCURVE_MIN_BUY_PRICE",
                    0.60,
                ),
                d1_enabled: config_io::env_template_default_bool("EVPOLY_EVCURVE_D1_ENABLE", true),
                d1_cap_usd: config_io::env_template_default_f64(
                    "EVPOLY_EVCURVE_D1_STRATEGY_CAP_USD",
                    10000.0,
                ),
            },
            session_band: DesktopSessionBandSettings {
                timeframes: csv_list(
                    config_io::env_template_default_string("EVPOLY_SESSIONBAND_TIMEFRAMES"),
                    &["5m", "15m", "1h", "4h"],
                ),
                flip_threshold_pct: config_io::env_template_default_f64(
                    "EVPOLY_SESSIONBAND_FLIP_THRESHOLD_PCT",
                    2.0,
                ),
                tau2_enabled: true,
                tau1_enabled: true,
                tau2_multiplier: 0.30,
                tau1_multiplier: 0.70,
            },
            evsnipe: DesktopEvsnipeSettings {
                pre_hit_enabled: true,
                pre_leg_ratio: config_io::env_template_default_f64(
                    "EVPOLY_EVSNIPE_PRE_LEG_RATIO",
                    0.30,
                ),
                saved_pre_leg_ratio: config_io::env_template_default_f64(
                    "EVPOLY_EVSNIPE_PRE_LEG_RATIO",
                    0.30,
                ),
                pre_trigger_bps: config_io::env_template_default_f64(
                    "EVPOLY_EVSNIPE_PRE_TRIGGER_BPS",
                    1.0,
                ),
                strike_window_pct: config_io::env_template_default_f64(
                    "EVPOLY_EVSNIPE_STRIKE_WINDOW_PCT",
                    0.10,
                ),
                max_days_to_expiry: config_io::env_template_default_f64(
                    "EVPOLY_EVSNIPE_MAX_DAYS_TO_EXPIRY",
                    30.0,
                ),
            },
            mm_rewards: DesktopMmRewardsSettings {
                market_mode: normalize_mm_rewards_market_mode(
                    config_io::env_template_default_string("EVPOLY_MM_MARKET_MODE")
                        .as_deref()
                        .unwrap_or(config_io::DEFAULT_MM_MARKET_MODE),
                )
                .to_string(),
                single_market_slugs: config_io::env_template_default_string(
                    "EVPOLY_MM_SINGLE_MARKET_SLUGS",
                )
                .unwrap_or_default(),
                auto_top_n: config_io::env_template_default_f64("EVPOLY_MM_AUTO_TOP_N", 80.0),
                auto_refresh_sec: config_io::env_template_default_f64(
                    "EVPOLY_MM_AUTO_REFRESH_SEC",
                    900.0,
                ),
                auto_rank_budget_usd: config_io::env_template_default_f64(
                    "EVPOLY_MM_AUTO_RANK_BUDGET_USD",
                    2000.0,
                ),
                blacklist_keywords: config_io::env_template_default_string(
                    "EVPOLY_MM_MARKET_BLACKLIST_KEYWORDS",
                )
                .unwrap_or_default(),
                reward_min_shares_cap: config_io::env_template_default_f64(
                    "EVPOLY_MM_REWARD_MIN_SHARES_CAP",
                    0.0,
                ),
            },
            mm_sport: DesktopMmSportSettings {
                discovery_route: mm_sport_discovery_route,
                quote_size_mode: normalize_mm_sport_quote_size_mode(
                    config_io::env_template_default_string("EVPOLY_MM_SPORT_QUOTE_SIZE_MODE")
                        .as_deref()
                        .unwrap_or("depth_ratio"),
                )
                .to_string(),
                nonsport_quote_size_mode: normalize_mm_sport_quote_size_mode(
                    config_io::env_template_default_string(
                        "EVPOLY_MM_SPORT_NONSPORT_QUOTE_SIZE_MODE",
                    )
                    .or_else(|| {
                        config_io::env_template_default_string("EVPOLY_MM_SPORT_QUOTE_SIZE_MODE")
                    })
                    .as_deref()
                    .unwrap_or("depth_ratio"),
                )
                .to_string(),
                entry_price_mode: normalize_mm_sport_entry_price_mode(
                    config_io::env_template_default_string("EVPOLY_MM_SPORT_ENTRY_PRICE_MODE")
                        .as_deref()
                        .unwrap_or("best_bid"),
                )
                .to_string(),
                min_reward_rate_per_day: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_MIN_REWARD_RATE_PER_DAY",
                    5.0,
                ),
                match_only: config_io::env_template_default_bool(
                    "EVPOLY_MM_SPORT_MATCH_ONLY",
                    true,
                ),
                allowed_sport_league_codes: config_io::env_template_default_string(
                    "EVPOLY_MM_SPORT_ALLOWED_SPORT_LEAGUE_CODES",
                )
                .unwrap_or_default(),
                blocked_sport_league_codes: config_io::env_template_default_string(
                    "EVPOLY_MM_SPORT_BLOCKED_SPORT_LEAGUE_CODES",
                )
                .unwrap_or_default(),
                blocked_competition_levels: config_io::env_template_default_string(
                    "EVPOLY_MM_SPORT_BLOCKED_COMPETITION_LEVELS",
                )
                .unwrap_or_default(),
                market_allowlist_keywords: config_io::env_template_default_string(
                    "EVPOLY_MM_SPORT_MARKET_ALLOWLIST_KEYWORDS",
                )
                .unwrap_or_default(),
                market_blacklist_keywords: config_io::env_template_default_string(
                    "EVPOLY_MM_SPORT_MARKET_BLACKLIST_KEYWORDS",
                )
                .unwrap_or_default(),
                reward_min_shares_cap: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_REWARD_MIN_SHARES_CAP",
                    0.0,
                ),
                polymarket_live_guard_enable: config_io::env_template_default_bool(
                    "EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_ENABLE",
                    true,
                ),
                polymarket_live_guard_ws_enable: config_io::env_template_default_bool(
                    "EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_WS_ENABLE",
                    true,
                ),
                polymarket_live_guard_ws_stale_ms: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_WS_STALE_MS",
                    600000.0,
                ),
                pause_after_fill_sec: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC",
                    3600.0,
                ),
                inventory_exit_start_hours: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_INVENTORY_EXIT_START_SEC",
                    3600.0,
                ) / 3600.0,
                nonsport_end_exit_start_hours: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_NONSPORT_END_EXIT_START_SEC",
                    172800.0,
                ) / 3600.0,
                sport_entry_schedule_enabled: config_io::env_template_default_bool(
                    "EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_ENABLE",
                    false,
                ),
                sport_entry_schedule_days_utc: config_io::env_template_default_string(
                    "EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_DAYS_UTC",
                )
                .unwrap_or_else(|| "mon,tue,wed,thu,fri".to_string()),
                sport_entry_schedule_start_minute_utc: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_START_MINUTE_UTC",
                    780.0,
                ),
                sport_entry_schedule_end_minute_utc: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_END_MINUTE_UTC",
                    240.0,
                ),
                nonsport_entry_schedule_enabled: config_io::env_template_default_bool(
                    "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_ENABLE",
                    false,
                ),
                nonsport_entry_schedule_days_utc: config_io::env_template_default_string(
                    "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_DAYS_UTC",
                )
                .unwrap_or_else(|| "mon,tue,wed,thu,fri".to_string()),
                nonsport_entry_schedule_start_minute_utc: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_START_MINUTE_UTC",
                    780.0,
                ),
                nonsport_entry_schedule_end_minute_utc: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_END_MINUTE_UTC",
                    240.0,
                ),
                inventory_exit_max_loss_cents: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_INVENTORY_EXIT_MAX_LOSS_CENTS",
                    10.0,
                ),
                inventory_exit_mode: normalize_mm_sport_exit_mode(
                    config_io::env_template_default_string("EVPOLY_MM_SPORT_EXIT_MODE")
                        .as_deref()
                        .unwrap_or("normal"),
                )
                .to_string(),
                max_share_ratio: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_MAX_SHARE_RATIO",
                    0.20,
                ),
                nonsport_max_share_ratio: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_NONSPORT_MAX_SHARE_RATIO",
                    config_io::env_template_default_f64("EVPOLY_MM_SPORT_MAX_SHARE_RATIO", 0.20),
                ),
                max_quote_shares: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_MAX_QUOTE_SHARES",
                    1000.0,
                ),
                nonsport_max_quote_shares: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_NONSPORT_MAX_QUOTE_SHARES",
                    200.0,
                ),
                min_top_depth_usd: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD",
                    1100.0,
                ),
                nonsport_min_top_depth_usd: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_NONSPORT_MIN_TOP_DEPTH_USD",
                    config_io::env_template_default_f64(
                        "EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD",
                        1100.0,
                    ),
                ),
                min_entry_top_bid_price: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_MIN_ENTRY_TOP_BID_PRICE",
                    0.05,
                ),
                allow_sponsored_rewards: config_io::env_template_default_bool(
                    "EVPOLY_MM_SPORT_ALLOW_SPONSORED_REWARDS",
                    true,
                ),
                sponsored_reward_min_share: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_SPONSORED_REWARD_MIN_SHARE",
                    0.50,
                ),
                quote_expiry_min_sec: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC",
                    65.0,
                ),
                quote_expiry_max_sec: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC",
                    185.0,
                ),
                quote_cooldown_min_sec,
                quote_cooldown_max_sec,
                fifo_max_share_ratio: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_FIFO_MAX_SHARE_RATIO",
                    0.50,
                ),
                active_sport_market_cap,
                active_nonsport_market_cap,
            },
        },
        simulation: config_io::env_template_default_bool("APP_SIMULATION", false),
        alpha_key: String::new(),
        relayer_api_key: String::new(),
        relayer_api_key_address: String::new(),
        relayer_remote_signer_token: String::new(),
        relayer_submit_signer_url: String::new(),
        wallet_binding: String::new(),
        onboarding_status: String::new(),
        approval_status: String::new(),
        remote_signer_token: String::new(),
        order_signer_primary_token_internal: String::new(),
        remote_discovery_token: String::new(),
        remote_endgame_alpha_token: String::new(),
        remote_mm_rewards_alpha_token: String::new(),
        remote_evsnipe_discovery_token: String::new(),
        admin_api_token: String::new(),
    }
}

fn doctor_item(
    key: &str,
    label: &str,
    status: &str,
    message: impl Into<String>,
    strategy: Option<&str>,
) -> SetupDoctorItem {
    SetupDoctorItem {
        key: key.to_string(),
        label: label.to_string(),
        status: status.to_string(),
        message: message.into(),
        strategy: strategy.map(|value| value.to_string()),
    }
}

fn doctor_popup(title: impl Into<String>, body: impl Into<String>) -> SetupDoctorPopup {
    SetupDoctorPopup {
        title: title.into(),
        body: body.into(),
        cta_label: "Open Setup".to_string(),
        cta_target: "setup".to_string(),
    }
}

fn doctor_result(
    status: &str,
    items: Vec<SetupDoctorItem>,
    popup: Option<SetupDoctorPopup>,
    bot_was_running: bool,
    bot_restarted: bool,
) -> SetupDoctorResult {
    let fixed_count = items.iter().filter(|item| item.status == "fixed").count();
    let missing_user_count = items
        .iter()
        .filter(|item| item.status == "missing_user")
        .count();
    SetupDoctorResult {
        status: status.to_string(),
        items,
        fixed_count,
        missing_user_count,
        bot_was_running,
        bot_restarted,
        popup,
    }
}

fn missing_labels_sentence(labels: &[String]) -> String {
    match labels {
        [] => String::new(),
        [only] => only.clone(),
        [left, right] => format!("{left} and {right}"),
        _ => {
            let mut head = labels[..labels.len() - 1].join(", ");
            head.push_str(", and ");
            head.push_str(labels.last().map(String::as_str).unwrap_or_default());
            head
        }
    }
}

fn wallet_address_from_private_key(private_key: &str) -> Result<String, String> {
    let wallet: LocalWallet = private_key
        .trim()
        .parse()
        .map_err(|e| format!("invalid private key: {e}"))?;
    Ok(format!("{:#x}", wallet.address()))
}

fn polymarket_funders_from_private_key(
    private_key: &str,
) -> Result<DerivedPolymarketFunders, String> {
    let eoa_wallet = wallet_address_from_private_key(private_key)?;
    let eoa_address = AlloyAddress::from_str(eoa_wallet.as_str())
        .map_err(|e| format!("parse signer address: {e}"))?;
    let proxy_wallet = derive_proxy_wallet(eoa_address, POLYGON).map(|address| address.to_string());
    let safe_wallet = derive_safe_wallet(eoa_address, POLYGON)
        .map(|address| address.to_string())
        .ok_or_else(|| "safe wallet derivation is unsupported on this chain".to_string())?;

    Ok(DerivedPolymarketFunders {
        eoa_wallet,
        proxy_wallet,
        safe_wallet,
        deposit_wallet: None,
    })
}

fn bound_wallet_for_config(config: &DesktopConfig, eoa_wallet: &str) -> Result<String, String> {
    if matches!(config.sig_type, 1 | 2) {
        let proxy = config.proxy_wallet.trim();
        if proxy.is_empty() {
            return Err("proxy wallet is required for proxy or safe wallet mode".to_string());
        }
        Ok(proxy.to_string())
    } else if config.sig_type == 3 {
        let deposit = config.deposit_wallet.trim();
        if deposit.is_empty() {
            return Err("deposit wallet is required for deposit wallet mode".to_string());
        }
        Ok(deposit.to_string())
    } else {
        Ok(eoa_wallet.trim().to_string())
    }
}

fn wallet_binding_for_config(config: &DesktopConfig, eoa_wallet: &str) -> Result<String, String> {
    let bound_wallet = bound_wallet_for_config(config, eoa_wallet)?;
    Ok(onboard::wallet_binding_fingerprint(
        eoa_wallet,
        config.sig_type,
        bound_wallet.as_str(),
    ))
}

fn clean_relayer_remote_signer_token(config: &DesktopConfig) -> String {
    config.relayer_remote_signer_token.trim().to_string()
}

fn clean_alpha_key_or_legacy_source(config: &DesktopConfig) -> String {
    config.alpha_key.trim().to_string()
}

fn wallet_mode_needs_approval_status(signature_type: u8) -> bool {
    matches!(signature_type, 1 | 2 | 3)
}

fn clean_onboarding_ready(config: &DesktopConfig) -> bool {
    !clean_alpha_key_or_legacy_source(config).is_empty()
        && !clean_relayer_remote_signer_token(config).is_empty()
}

fn distinct_order_signer_primary_token(remote_signer_token: &str, primary_token: &str) -> String {
    let remote = remote_signer_token.trim();
    let primary = primary_token.trim();
    if primary.is_empty() || primary == remote {
        String::new()
    } else {
        primary.to_string()
    }
}

fn push_doctor_missing_user(
    audit: &mut SetupDoctorAudit,
    key: &str,
    label: &str,
    message: impl Into<String>,
    strategy: Option<&str>,
    blocking: bool,
) {
    audit.items.push(doctor_item(
        key,
        label,
        "missing_user",
        message.into(),
        strategy,
    ));
    audit.missing_user_labels.push(label.to_string());
    if blocking {
        audit.blocking_user_labels.push(label.to_string());
    }
}

fn push_doctor_missing_generated(
    audit: &mut SetupDoctorAudit,
    key: &str,
    label: &str,
    message: impl Into<String>,
    strategy: Option<&str>,
) {
    audit.items.push(doctor_item(
        key,
        label,
        "missing_generated",
        message.into(),
        strategy,
    ));
    audit.missing_generated_labels.push(label.to_string());
    audit.missing_generated_keys.push(key.to_string());
}

fn audit_setup_doctor(config: &DesktopConfig) -> SetupDoctorAudit {
    let mut audit = SetupDoctorAudit::default();
    let private_key = config.private_key.trim();
    let derived_eoa = if private_key.is_empty() {
        push_doctor_missing_user(
            &mut audit,
            "private_key",
            "Private Key",
            "Enter the Polymarket signer private key in Settings -> Setup.",
            None,
            true,
        );
        None
    } else {
        match wallet_address_from_private_key(private_key) {
            Ok(address) => Some(address),
            Err(_) => {
                push_doctor_missing_user(
                    &mut audit,
                    "private_key",
                    "Private Key",
                    "The private key is invalid. Replace it in Settings -> Setup.",
                    None,
                    true,
                );
                None
            }
        }
    };

    if let Some(derived) = derived_eoa.as_deref() {
        let current = config.eoa_wallet.trim();
        if current.is_empty() || !current.eq_ignore_ascii_case(derived) {
            push_doctor_missing_generated(
                &mut audit,
                "eoa_wallet",
                "EOA Wallet",
                "The EOA wallet will be regenerated from the private key.",
                None,
            );
        }
    }

    if matches!(config.sig_type, 1 | 2) && config.proxy_wallet.trim().is_empty() {
        push_doctor_missing_user(
            &mut audit,
            "proxy_wallet",
            "Proxy Wallet",
            "Proxy and Safe modes require a proxy wallet address in Settings -> Setup.",
            None,
            true,
        );
    }

    if config.sig_type == 3 && config.deposit_wallet.trim().is_empty() {
        push_doctor_missing_user(
            &mut audit,
            "deposit_wallet",
            "Deposit Wallet",
            "Deposit Wallet mode requires the deployed deposit wallet address in Settings -> Setup.",
            None,
            true,
        );
    }

    if wallet_mode_needs_approval_status(config.sig_type)
        && config.relayer_api_key.trim().is_empty()
    {
        push_doctor_missing_user(
            &mut audit,
            "relayer_api_key",
            "Relayer API Key",
            "Get RELAYER_API_KEY from https://polymarket.com/settings?tab=api-keys, then paste it into Settings -> Setup. EVPoly can still use remote signer fallback where supported.",
            None,
            false,
        );
    }

    if wallet_mode_needs_approval_status(config.sig_type)
        && config.relayer_api_key_address.trim().is_empty()
    {
        push_doctor_missing_user(
            &mut audit,
            "relayer_api_key_address",
            "Relayer API Key Address",
            "Get RELAYER_API_KEY_ADDRESS from https://polymarket.com/settings?tab=api-keys, then paste it into Settings -> Setup. EVPoly can still use remote signer fallback where supported.",
            None,
            false,
        );
    }

    if config.alpha_key.trim().is_empty() {
        push_doctor_missing_generated(
            &mut audit,
            "alpha_key",
            "EVPOLY Alpha Key",
            "Alpha access is missing and will be generated automatically from onboarding.",
            None,
        );
    }

    if clean_relayer_remote_signer_token(config).is_empty() {
        push_doctor_missing_generated(
            &mut audit,
            "relayer_remote_signer_token",
            "Relayer Remote Signer Token",
            "The redeem/merge fallback signer token is missing and will be generated automatically from onboarding.",
            None,
        );
    }

    if audit.items.is_empty() {
        audit.items.push(doctor_item(
            "setup_ready",
            "Setup",
            "ok",
            "All required setup fields are present.",
            None,
        ));
    }

    audit
}

fn mark_fixed_doctor_items(items: &mut [SetupDoctorItem], fixed_keys: &[String]) {
    for item in items.iter_mut() {
        if fixed_keys.iter().any(|key| key == &item.key)
            && (item.status == "ok" || item.status == "missing_generated")
        {
            item.status = "fixed".to_string();
            item.message = format!("{} regenerated automatically.", item.label);
        }
    }
}

fn mark_failed_generated_doctor_items(items: &mut [SetupDoctorItem], missing_keys: &[String]) {
    for item in items.iter_mut() {
        if missing_keys.iter().any(|key| key == &item.key) && item.status == "missing_generated" {
            item.status = "failed".to_string();
            item.message = format!(
                "{} is still missing after Setup Doctor tried onboarding.",
                item.label
            );
        }
    }
}

fn doctor_needs_you_popup(audit: &SetupDoctorAudit) -> SetupDoctorPopup {
    let has_relayer_issue = audit.items.iter().any(|item| {
        item.status == "missing_user"
            && matches!(
                item.key.as_str(),
                "relayer_api_key" | "relayer_api_key_address"
            )
    });

    if !audit.blocking_user_labels.is_empty() {
        let missing = missing_labels_sentence(&audit.blocking_user_labels);
        return doctor_popup(
            format!("Missing {missing}"),
            format!(
                "Enter {} in Settings -> Setup, then run Setup Doctor again.",
                missing
            ),
        );
    }

    if has_relayer_issue && audit.missing_generated_labels.is_empty() {
        return doctor_popup(
            "Add Relayer Credentials",
            "Get RELAYER_API_KEY and RELAYER_API_KEY_ADDRESS from https://polymarket.com/settings?tab=api-keys, then paste them into Settings -> Setup. EVPoly can still use remote signer fallback where supported.",
        );
    }

    if !audit.missing_generated_labels.is_empty() {
        let missing = missing_labels_sentence(&audit.missing_generated_labels);
        return doctor_popup(
            "Finish Setup Doctor",
            format!(
                "Setup Doctor still could not regenerate {}. Open Settings -> Setup, rerun onboarding, and review the checklist.",
                missing
            ),
        );
    }

    let missing = missing_labels_sentence(&audit.missing_user_labels);
    doctor_popup(
        "Finish Setup Doctor",
        format!(
            "Setup Doctor finished, but {} still need manual input.",
            missing
        ),
    )
}

fn normalize_symbols(symbols: &[String]) -> Vec<String> {
    let mut selected = symbols
        .iter()
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| DESKTOP_SYMBOL_ORDER.iter().any(|v| v == s))
        .collect::<Vec<_>>();
    if !selected.iter().any(|s| s == "BTC") {
        selected.push("BTC".to_string());
    }
    DESKTOP_SYMBOL_ORDER
        .iter()
        .filter(|sym| selected.iter().any(|s| s == **sym))
        .map(|s| (*s).to_string())
        .collect()
}

fn core_strategy_symbols(symbols: &[String]) -> Vec<String> {
    let normalized = normalize_symbols(symbols);
    CORE_STRATEGY_SYMBOLS
        .iter()
        .filter(|sym| normalized.iter().any(|selected| selected == **sym))
        .map(|sym| (*sym).to_string())
        .collect()
}

fn decrypt_profile_secrets(
    profile: &Profile,
    auth: &AppAuth,
) -> Result<HashMap<String, String>, String> {
    if profile.encrypted_secrets.trim().is_empty() {
        return Ok(HashMap::new());
    }
    let decrypted = auth
        .with_session_password(|password| {
            crypto_vault::decrypt_data(profile.encrypted_secrets.as_str(), password)
        })
        .ok_or("session locked: unlock app again before using secrets")?
        .map_err(|e| e.to_string())?;
    serde_json::from_slice::<HashMap<String, String>>(&decrypted).map_err(|e| e.to_string())
}

fn active_profile(pm: &ProfileManager) -> Result<Profile, String> {
    let active_id = pm.get_active_profile_id().ok_or("no active profile set")?;
    pm.get_profile(&active_id)
        .ok_or("active profile not found".to_string())
}

fn build_runtime_paths_for_profile(
    profile: &Profile,
    auth: &AppAuth,
    data_dir: &Path,
) -> Result<(PathBuf, PathBuf), String> {
    let secrets = decrypt_profile_secrets(profile, auth)?;
    if !secrets.contains_key("POLY_PRIVATE_KEY") {
        return Err("missing POLY_PRIVATE_KEY in profile secrets".to_string());
    }
    let env_path =
        config_io::generate_env_file(profile, &secrets, data_dir).map_err(|e| e.to_string())?;
    let config_path = data_dir.join("runtime.config.json");
    config_io::write_config_json(profile, &config_path).map_err(|e| e.to_string())?;
    Ok((env_path, config_path))
}

fn wallet_sync_config_for_profile(profile: &Profile) -> WalletSyncRuntimeConfig {
    WalletSyncRuntimeConfig::new(profile.primary_wallet_address())
}

fn simulation_mode_from_profile(profile: &Profile) -> bool {
    profile
        .sizing_config
        .as_object()
        .and_then(|obj| obj.get("APP_SIMULATION"))
        .and_then(Value::as_bool)
        .unwrap_or_else(|| config_io::env_template_default_bool("APP_SIMULATION", false))
}

fn bot_status_label(status: bot_manager::BotStatus) -> String {
    match status {
        bot_manager::BotStatus::Stopped => "stopped".to_string(),
        bot_manager::BotStatus::Starting => "starting".to_string(),
        bot_manager::BotStatus::Running => "running".to_string(),
        bot_manager::BotStatus::Stopping => "stopping".to_string(),
        bot_manager::BotStatus::Error(e) => format!("error:{e}"),
    }
}

fn active_profile_bot_state(
    global_state: &str,
    active_profile_id: Option<&str>,
    running_profile_id: Option<&str>,
) -> String {
    match running_profile_id {
        Some(running_id) if active_profile_id == Some(running_id) => global_state.to_string(),
        Some(_) => "stopped".to_string(),
        None if matches!(global_state, "starting" | "running" | "stopping") => {
            "stopped".to_string()
        }
        None => global_state.to_string(),
    }
}

fn persist_profile_simulation_mode(
    pm: &ProfileManager,
    profile_id: &str,
    simulation: bool,
) -> Result<Profile, String> {
    let mut profile = pm.get_profile(profile_id).ok_or("profile not found")?;
    if !profile.sizing_config.is_object() {
        profile.sizing_config = Value::Object(Map::new());
    }
    if let Some(obj) = profile.sizing_config.as_object_mut() {
        obj.insert("APP_SIMULATION".to_string(), Value::Bool(simulation));
    }
    pm.update_profile(profile.clone())
        .map_err(|e| e.to_string())?;
    Ok(profile)
}

fn persist_active_profile_simulation_mode(
    pm: &ProfileManager,
    simulation: bool,
) -> Result<Profile, String> {
    let active_id = pm.get_active_profile_id().ok_or("no active profile set")?;
    persist_profile_simulation_mode(pm, &active_id, simulation)
}

fn merge_desktop_secrets(
    mut existing: HashMap<String, String>,
    updates: HashMap<String, String>,
) -> HashMap<String, String> {
    for &key in DESKTOP_SECRET_KEYS {
        existing.remove(key);
    }
    for &key in OBSOLETE_PREMARKET_REMOTE_ALPHA_KEYS {
        existing.remove(key);
    }
    existing.extend(updates);
    existing
}

fn generate_admin_api_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn ensure_admin_api_token(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        generate_admin_api_token()
    } else {
        trimmed.to_string()
    }
}

fn desktop_install_id(data_dir: &Path) -> Result<String, String> {
    let path = data_dir.join("desktop-install-id");
    if let Ok(value) = std::fs::read_to_string(&path) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let install_id = format!("evpoly-desktop-{}", Uuid::new_v4());
    std::fs::write(&path, install_id.as_bytes()).map_err(|e| e.to_string())?;
    Ok(install_id)
}

fn desktop_magic_bridge_base_url() -> String {
    std::env::var("EVPOLY_DESKTOP_MAGIC_BRIDGE_BASE_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| value.starts_with("https://") || value.starts_with("http://"))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_DESKTOP_MAGIC_BRIDGE_BASE_URL.to_string())
}

fn polymarket_bridge_base_url() -> String {
    std::env::var("POLYMARKET_BRIDGE_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| value.starts_with("https://") || value.starts_with("http://"))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_POLYMARKET_BRIDGE_BASE_URL.to_string())
}

fn merge_config_object(existing: &Value, updates: &Value) -> Value {
    let mut merged = existing.as_object().cloned().unwrap_or_else(Map::new);
    if let Some(update_obj) = updates.as_object() {
        for (key, value) in update_obj {
            merged.insert(key.clone(), value.clone());
        }
    }
    Value::Object(merged)
}

fn remove_legacy_premarket_ladder_keys(strategy_config: &mut Value) {
    if let Some(strategy) = strategy_config.as_object_mut() {
        for key in PREMARKET_LEGACY_LADDER_KEYS {
            strategy.remove(key);
        }
        for key in OBSOLETE_PREMARKET_REMOTE_ALPHA_KEYS {
            strategy.remove(*key);
        }
    }
}

fn normalize_endgame_timeframes(strategy_config: &mut Value) {
    if let Some(strategy) = strategy_config.as_object_mut() {
        strategy.insert(
            "EVPOLY_ENDGAME_TIMEFRAMES".to_string(),
            Value::String(default_endgame_timeframes().join(",")),
        );
    }
}

fn portable_profile_from_profile(profile: &Profile) -> PortableProfile {
    let mut strategy_config = profile.strategy_config.clone();
    normalize_endgame_timeframes(&mut strategy_config);
    PortableProfile {
        name: profile.name.clone(),
        eoa_wallet_address: profile.eoa_wallet_address.clone(),
        proxy_wallet_address: profile.proxy_wallet_address.clone(),
        deposit_wallet_address: profile.deposit_wallet_address.clone(),
        signature_type: profile.signature_type,
        strategy_config,
        sizing_config: profile.sizing_config.clone(),
    }
}

fn desktop_config_to_profile_payload(
    config: &DesktopConfig,
) -> (
    Value,
    Value,
    HashMap<String, String>,
    String,
    String,
    String,
    u8,
) {
    let mut strategy = Map::new();
    let mut sizing = Map::new();
    let mut secrets = HashMap::new();

    let symbols = normalize_symbols(&config.symbols);
    let symbol_csv = symbols.join(",");
    let core_symbol_csv = core_strategy_symbols(&symbols).join(",");
    let has_eth = symbols.iter().any(|s| s == "ETH");
    let has_sol = symbols.iter().any(|s| s == "SOL");
    let has_xrp = symbols.iter().any(|s| s == "XRP");

    strategy.insert(
        "EVPOLY_STRATEGY_PREMARKET_ENABLE".to_string(),
        bool_to_json(config.strategies.premarket),
    );
    strategy.insert(
        "EVPOLY_STRATEGY_ENDGAME_ENABLE".to_string(),
        bool_to_json(config.strategies.endgame),
    );
    strategy.insert(
        "EVPOLY_STRATEGY_EVCURVE_ENABLE".to_string(),
        bool_to_json(config.strategies.evcurve),
    );
    strategy.insert(
        "EVPOLY_STRATEGY_SESSIONBAND_ENABLE".to_string(),
        bool_to_json(false),
    );
    strategy.insert(
        "EVPOLY_STRATEGY_EVSNIPE_ENABLE".to_string(),
        bool_to_json(config.strategies.evsnipe),
    );
    strategy.insert(
        "EVPOLY_STRATEGY_MM_REWARDS_ENABLE".to_string(),
        bool_to_json(false),
    );
    strategy.insert(
        "EVPOLY_STRATEGY_MM_SPORT_ENABLE".to_string(),
        bool_to_json(config.strategies.mm_sport),
    );
    strategy.insert(
        WEEKEND_POLICY_ENV_KEY.to_string(),
        Value::String(normalize_weekend_policy(Some(
            config.weekend_policy.as_str(),
        ))),
    );
    strategy.insert("POLY_ENABLE_ETH_TRADING".to_string(), bool_to_json(has_eth));
    strategy.insert(
        "POLY_ENABLE_SOLANA_TRADING".to_string(),
        bool_to_json(has_sol),
    );
    strategy.insert("POLY_ENABLE_XRP_TRADING".to_string(), bool_to_json(has_xrp));
    strategy.insert(
        "EVPOLY_ENDGAME_SYMBOLS".to_string(),
        Value::String(symbol_csv.clone()),
    );
    strategy.insert(
        "EVPOLY_EVCURVE_SYMBOLS".to_string(),
        Value::String(core_symbol_csv.clone()),
    );
    strategy.insert(
        "EVPOLY_SESSIONBAND_SYMBOLS".to_string(),
        Value::String(core_symbol_csv),
    );
    strategy.insert(
        "EVPOLY_EVSNIPE_SYMBOLS".to_string(),
        Value::String(symbol_csv),
    );
    strategy.insert(
        "EVPOLY_MM_REWARD_MIN_TARGET_MULT".to_string(),
        number_to_json(1.0),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_QUOTE_SIZE_MULT".to_string(),
        number_to_json(config.mm_tuning.sport_quote_size_multiplier),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_NONSPORT_QUOTE_SIZE_MULT".to_string(),
        number_to_json(config.mm_tuning.nonsport_quote_size_multiplier),
    );
    strategy.insert(
        "EVPOLY_PREMARKET_TP_ENABLE".to_string(),
        bool_to_json(config.strategy_settings.premarket.tp_enabled),
    );
    strategy.insert(
        "EVPOLY_PREMARKET_ACTIVE_CAP_PER_ASSET".to_string(),
        number_to_json(config.strategy_settings.premarket.active_cap_per_asset),
    );
    strategy.insert(
        "EVPOLY_PREMARKET_TIMEFRAMES".to_string(),
        Value::String(config.strategy_settings.premarket.timeframes.join(",")),
    );
    strategy.insert(
        PREMARKET_LADDER_MODE_ENV_KEY_5M.to_string(),
        Value::String(normalize_premarket_ladder_mode(Some(
            config.strategy_settings.premarket.ladder_mode_m5.as_str(),
        ))),
    );
    strategy.insert(
        PREMARKET_LADDER_MODE_ENV_KEY_NON_M5.to_string(),
        Value::String(normalize_premarket_ladder_mode(Some(
            config
                .strategy_settings
                .premarket
                .ladder_mode_non_m5
                .as_str(),
        ))),
    );
    strategy.insert(
        PREMARKET_SAFE_BIAS_PCT_ENV_KEY.to_string(),
        signed_number_to_json(normalize_premarket_ladder_bias_pct(
            config.strategy_settings.premarket.safe_bias_pct,
            -10.0,
        )),
    );
    strategy.insert(
        PREMARKET_AGGRESSIVE_BIAS_PCT_ENV_KEY.to_string(),
        signed_number_to_json(normalize_premarket_ladder_bias_pct(
            config.strategy_settings.premarket.aggressive_bias_pct,
            10.0,
        )),
    );
    strategy.insert(
        "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_5M_SEC".to_string(),
        number_to_json(config.strategy_settings.premarket.cancel_after_open_sec.m5),
    );
    strategy.insert(
        "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_15M_SEC".to_string(),
        number_to_json(config.strategy_settings.premarket.cancel_after_open_sec.m15),
    );
    strategy.insert(
        "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_1H_SEC".to_string(),
        number_to_json(config.strategy_settings.premarket.cancel_after_open_sec.h1),
    );
    strategy.insert(
        "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_4H_SEC".to_string(),
        number_to_json(config.strategy_settings.premarket.cancel_after_open_sec.h4),
    );
    strategy.insert(
        "EVPOLY_ENDGAME_TIMEFRAMES".to_string(),
        Value::String(default_endgame_timeframes().join(",")),
    );
    strategy.insert(
        "EVPOLY_ENDGAME_TICK0_MULTIPLIER".to_string(),
        number_to_json(0.20),
    );
    strategy.insert(
        "EVPOLY_ENDGAME_TICK1_MULTIPLIER".to_string(),
        number_to_json(0.40),
    );
    strategy.insert(
        "EVPOLY_ENDGAME_TICK2_MULTIPLIER".to_string(),
        number_to_json(0.40),
    );
    strategy.insert(
        "EVPOLY_EVCURVE_TIMEFRAMES".to_string(),
        Value::String(config.strategy_settings.evcurve.timeframes.join(",")),
    );
    strategy.insert(
        "EVPOLY_EVCURVE_MIN_BUY_PRICE".to_string(),
        number_to_json(config.strategy_settings.evcurve.min_buy_price),
    );
    strategy.insert(
        "EVPOLY_EVCURVE_D1_ENABLE".to_string(),
        bool_to_json(config.strategy_settings.evcurve.d1_enabled),
    );
    strategy.insert(
        "EVPOLY_SYMBOL_SIZE_MULTIPLIER_BTC".to_string(),
        number_to_json(config.size_policy.symbol_multipliers.btc),
    );
    strategy.insert(
        "EVPOLY_SYMBOL_SIZE_MULTIPLIER_ETH".to_string(),
        number_to_json(config.size_policy.symbol_multipliers.eth),
    );
    strategy.insert(
        "EVPOLY_SYMBOL_SIZE_MULTIPLIER_SOL".to_string(),
        number_to_json(config.size_policy.symbol_multipliers.sol),
    );
    strategy.insert(
        "EVPOLY_SYMBOL_SIZE_MULTIPLIER_XRP".to_string(),
        number_to_json(config.size_policy.symbol_multipliers.xrp),
    );
    strategy.insert(
        "EVPOLY_SYMBOL_SIZE_MULTIPLIER_DOGE".to_string(),
        number_to_json(config.size_policy.symbol_multipliers.doge),
    );
    strategy.insert(
        "EVPOLY_SYMBOL_SIZE_MULTIPLIER_BNB".to_string(),
        number_to_json(config.size_policy.symbol_multipliers.bnb),
    );
    strategy.insert(
        "EVPOLY_SYMBOL_SIZE_MULTIPLIER_HYPE".to_string(),
        number_to_json(config.size_policy.symbol_multipliers.hype),
    );
    strategy.insert(
        "EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_5M".to_string(),
        number_to_json(config.size_policy.premarket_timeframe_multipliers.m5),
    );
    strategy.insert(
        "EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_15M".to_string(),
        number_to_json(config.size_policy.premarket_timeframe_multipliers.m15),
    );
    strategy.insert(
        "EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_1H".to_string(),
        number_to_json(config.size_policy.premarket_timeframe_multipliers.h1),
    );
    strategy.insert(
        "EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_4H".to_string(),
        number_to_json(config.size_policy.premarket_timeframe_multipliers.h4),
    );
    strategy.insert(
        "EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_1D".to_string(),
        number_to_json(config.size_policy.premarket_timeframe_multipliers.d1),
    );
    strategy.insert(
        "EVPOLY_EVCURVE_TIMEFRAME_MULTIPLIER_15M".to_string(),
        number_to_json(config.size_policy.evcurve_timeframe_multipliers.m15),
    );
    strategy.insert(
        "EVPOLY_EVCURVE_TIMEFRAME_MULTIPLIER_1H".to_string(),
        number_to_json(config.size_policy.evcurve_timeframe_multipliers.h1),
    );
    strategy.insert(
        "EVPOLY_EVCURVE_TIMEFRAME_MULTIPLIER_4H".to_string(),
        number_to_json(config.size_policy.evcurve_timeframe_multipliers.h4),
    );
    strategy.insert(
        "EVPOLY_EVCURVE_TIMEFRAME_MULTIPLIER_1D".to_string(),
        number_to_json(config.size_policy.evcurve_timeframe_multipliers.d1),
    );
    strategy.insert(
        "EVPOLY_SESSIONBAND_TIMEFRAMES".to_string(),
        Value::String(config.strategy_settings.session_band.timeframes.join(",")),
    );
    strategy.insert(
        "EVPOLY_SESSIONBAND_FLIP_THRESHOLD_PCT".to_string(),
        number_to_json(config.strategy_settings.session_band.flip_threshold_pct),
    );
    strategy.insert(
        "EVPOLY_SESSIONBAND_ALLOWED_TAU_SEC".to_string(),
        Value::String("2,1".to_string()),
    );
    strategy.insert(
        "EVPOLY_SESSIONBAND_TAU2_MULTIPLIER".to_string(),
        number_to_json(0.30),
    );
    strategy.insert(
        "EVPOLY_SESSIONBAND_TAU1_MULTIPLIER".to_string(),
        number_to_json(0.70),
    );
    strategy.insert(
        "EVPOLY_EVSNIPE_PRE_TRIGGER_BPS".to_string(),
        number_to_json(config.strategy_settings.evsnipe.pre_trigger_bps),
    );
    strategy.insert(
        "EVPOLY_EVSNIPE_STRIKE_WINDOW_PCT".to_string(),
        number_to_json(config.strategy_settings.evsnipe.strike_window_pct),
    );
    strategy.insert(
        "EVPOLY_EVSNIPE_MAX_DAYS_TO_EXPIRY".to_string(),
        number_to_json(config.strategy_settings.evsnipe.max_days_to_expiry),
    );
    strategy.insert(
        "EVPOLY_EVSNIPE_PRE_LEG_RATIO".to_string(),
        number_to_json(if config.strategy_settings.evsnipe.pre_hit_enabled {
            config.strategy_settings.evsnipe.pre_leg_ratio
        } else {
            0.0
        }),
    );
    strategy.insert(
        "APP_DESKTOP_EVSNIPE_PRE_LEG_RATIO_SAVED".to_string(),
        number_to_json(config.strategy_settings.evsnipe.saved_pre_leg_ratio),
    );
    strategy.insert(
        "EVPOLY_MM_MARKET_MODE".to_string(),
        Value::String(
            normalize_mm_rewards_market_mode(
                config.strategy_settings.mm_rewards.market_mode.as_str(),
            )
            .to_string(),
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SINGLE_MARKET_SLUGS".to_string(),
        Value::String(
            config
                .strategy_settings
                .mm_rewards
                .single_market_slugs
                .trim()
                .to_string(),
        ),
    );
    strategy.insert(
        "EVPOLY_MM_AUTO_TOP_N".to_string(),
        number_to_json(config.strategy_settings.mm_rewards.auto_top_n),
    );
    strategy.insert(
        "EVPOLY_MM_AUTO_REFRESH_SEC".to_string(),
        number_to_json(config.strategy_settings.mm_rewards.auto_refresh_sec),
    );
    strategy.insert(
        "EVPOLY_MM_AUTO_RANK_BUDGET_USD".to_string(),
        number_to_json(config.strategy_settings.mm_rewards.auto_rank_budget_usd),
    );
    strategy.insert(
        "EVPOLY_MM_MARKET_BLACKLIST_KEYWORDS".to_string(),
        Value::String(
            config
                .strategy_settings
                .mm_rewards
                .blacklist_keywords
                .trim()
                .to_string(),
        ),
    );
    strategy.insert(
        "EVPOLY_MM_REWARD_MIN_SHARES_CAP".to_string(),
        number_to_json(config.strategy_settings.mm_rewards.reward_min_shares_cap),
    );
    let mm_sport_discovery_route = normalize_mm_sport_discovery_route(
        config.strategy_settings.mm_sport.discovery_route.as_str(),
    );
    let (_, default_active_nonsport_market_cap) =
        mm_sport_route_default_caps(mm_sport_discovery_route);
    let (quote_cooldown_min_sec, quote_cooldown_max_sec) = normalize_cooldown_pair_f64(
        config.strategy_settings.mm_sport.quote_cooldown_min_sec,
        config.strategy_settings.mm_sport.quote_cooldown_max_sec,
    );
    let active_nonsport_market_cap = normalize_nonnegative_integer_f64(
        config.strategy_settings.mm_sport.active_nonsport_market_cap,
        default_active_nonsport_market_cap,
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_DISCOVERY_ROUTE".to_string(),
        Value::String(mm_sport_discovery_route.to_string()),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_QUOTE_SIZE_MODE".to_string(),
        Value::String(
            normalize_mm_sport_quote_size_mode(
                config.strategy_settings.mm_sport.quote_size_mode.as_str(),
            )
            .to_string(),
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_NONSPORT_QUOTE_SIZE_MODE".to_string(),
        Value::String(
            normalize_mm_sport_quote_size_mode(
                config
                    .strategy_settings
                    .mm_sport
                    .nonsport_quote_size_mode
                    .as_str(),
            )
            .to_string(),
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_ENTRY_PRICE_MODE".to_string(),
        Value::String(
            normalize_mm_sport_entry_price_mode(
                config.strategy_settings.mm_sport.entry_price_mode.as_str(),
            )
            .to_string(),
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_MIN_REWARD_RATE_PER_DAY".to_string(),
        number_to_json(config.strategy_settings.mm_sport.min_reward_rate_per_day),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_MATCH_ONLY".to_string(),
        bool_to_json(config.strategy_settings.mm_sport.match_only),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_ALLOWED_SPORT_LEAGUE_CODES".to_string(),
        Value::String(
            config
                .strategy_settings
                .mm_sport
                .allowed_sport_league_codes
                .trim()
                .to_string(),
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_BLOCKED_SPORT_LEAGUE_CODES".to_string(),
        Value::String(
            config
                .strategy_settings
                .mm_sport
                .blocked_sport_league_codes
                .trim()
                .to_string(),
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_BLOCKED_COMPETITION_LEVELS".to_string(),
        Value::String(
            config
                .strategy_settings
                .mm_sport
                .blocked_competition_levels
                .trim()
                .to_string(),
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_MARKET_ALLOWLIST_KEYWORDS".to_string(),
        Value::String(
            config
                .strategy_settings
                .mm_sport
                .market_allowlist_keywords
                .trim()
                .to_string(),
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_MARKET_BLACKLIST_KEYWORDS".to_string(),
        Value::String(
            config
                .strategy_settings
                .mm_sport
                .market_blacklist_keywords
                .trim()
                .to_string(),
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_REWARD_MIN_SHARES_CAP".to_string(),
        number_to_json(config.strategy_settings.mm_sport.reward_min_shares_cap),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_ENABLE".to_string(),
        bool_to_json(
            config
                .strategy_settings
                .mm_sport
                .polymarket_live_guard_enable,
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_WS_ENABLE".to_string(),
        bool_to_json(
            config
                .strategy_settings
                .mm_sport
                .polymarket_live_guard_ws_enable,
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_WS_STALE_MS".to_string(),
        number_to_json(
            config
                .strategy_settings
                .mm_sport
                .polymarket_live_guard_ws_stale_ms,
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC".to_string(),
        number_to_json(config.strategy_settings.mm_sport.pause_after_fill_sec),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_INVENTORY_EXIT_START_SEC".to_string(),
        number_to_json(config.strategy_settings.mm_sport.inventory_exit_start_hours * 3600.0),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_NONSPORT_END_EXIT_START_SEC".to_string(),
        number_to_json(
            config
                .strategy_settings
                .mm_sport
                .nonsport_end_exit_start_hours
                * 3600.0,
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_ENABLE".to_string(),
        bool_to_json(
            config
                .strategy_settings
                .mm_sport
                .sport_entry_schedule_enabled,
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_DAYS_UTC".to_string(),
        Value::String(
            config
                .strategy_settings
                .mm_sport
                .sport_entry_schedule_days_utc
                .trim()
                .to_string(),
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_START_MINUTE_UTC".to_string(),
        number_to_json(normalize_utc_minute_f64(
            config
                .strategy_settings
                .mm_sport
                .sport_entry_schedule_start_minute_utc,
            780.0,
        )),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_END_MINUTE_UTC".to_string(),
        number_to_json(normalize_utc_minute_f64(
            config
                .strategy_settings
                .mm_sport
                .sport_entry_schedule_end_minute_utc,
            240.0,
        )),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_ENABLE".to_string(),
        bool_to_json(
            config
                .strategy_settings
                .mm_sport
                .nonsport_entry_schedule_enabled,
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_DAYS_UTC".to_string(),
        Value::String(
            config
                .strategy_settings
                .mm_sport
                .nonsport_entry_schedule_days_utc
                .trim()
                .to_string(),
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_START_MINUTE_UTC".to_string(),
        number_to_json(normalize_utc_minute_f64(
            config
                .strategy_settings
                .mm_sport
                .nonsport_entry_schedule_start_minute_utc,
            780.0,
        )),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_END_MINUTE_UTC".to_string(),
        number_to_json(normalize_utc_minute_f64(
            config
                .strategy_settings
                .mm_sport
                .nonsport_entry_schedule_end_minute_utc,
            240.0,
        )),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_INVENTORY_EXIT_MAX_LOSS_CENTS".to_string(),
        number_to_json(
            config
                .strategy_settings
                .mm_sport
                .inventory_exit_max_loss_cents,
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_EXIT_MODE".to_string(),
        Value::String(
            normalize_mm_sport_exit_mode(
                config
                    .strategy_settings
                    .mm_sport
                    .inventory_exit_mode
                    .as_str(),
            )
            .to_string(),
        ),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_MAX_SHARE_RATIO".to_string(),
        number_to_json(config.strategy_settings.mm_sport.max_share_ratio),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_NONSPORT_MAX_SHARE_RATIO".to_string(),
        number_to_json(config.strategy_settings.mm_sport.nonsport_max_share_ratio),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_MAX_QUOTE_SHARES".to_string(),
        number_to_json(config.strategy_settings.mm_sport.max_quote_shares),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_NONSPORT_MAX_QUOTE_SHARES".to_string(),
        number_to_json(config.strategy_settings.mm_sport.nonsport_max_quote_shares),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD".to_string(),
        number_to_json(config.strategy_settings.mm_sport.min_top_depth_usd),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_NONSPORT_MIN_TOP_DEPTH_USD".to_string(),
        number_to_json(config.strategy_settings.mm_sport.nonsport_min_top_depth_usd),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_MIN_ENTRY_TOP_BID_PRICE".to_string(),
        number_to_json(config.strategy_settings.mm_sport.min_entry_top_bid_price),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_ALLOW_SPONSORED_REWARDS".to_string(),
        Value::Bool(config.strategy_settings.mm_sport.allow_sponsored_rewards),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_SPONSORED_REWARD_MIN_SHARE".to_string(),
        number_to_json(config.strategy_settings.mm_sport.sponsored_reward_min_share),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC".to_string(),
        number_to_json(config.strategy_settings.mm_sport.quote_expiry_min_sec),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC".to_string(),
        number_to_json(config.strategy_settings.mm_sport.quote_expiry_max_sec),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MIN_SEC".to_string(),
        number_to_json(quote_cooldown_min_sec),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MAX_SEC".to_string(),
        number_to_json(quote_cooldown_max_sec),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_FIFO_MAX_SHARE_RATIO".to_string(),
        number_to_json(config.strategy_settings.mm_sport.fifo_max_share_ratio),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_ACTIVE_NONSPORT_MARKET_CAP".to_string(),
        number_to_json(active_nonsport_market_cap),
    );

    sizing.insert(
        "EVPOLY_PREMARKET_BASE_SIZE_USD".to_string(),
        number_to_json(config.sizing.premarket),
    );
    sizing.insert(
        "EVPOLY_ENDGAME_BASE_SIZE_USD".to_string(),
        number_to_json(config.sizing.endgame),
    );
    sizing.insert(
        "EVPOLY_EVCURVE_BASE_SIZE_USD".to_string(),
        number_to_json(config.sizing.evcurve),
    );
    sizing.insert(
        "EVPOLY_SESSIONBAND_BASE_SIZE_USD".to_string(),
        number_to_json(config.sizing.session_band),
    );
    sizing.insert(
        "EVPOLY_EVSNIPE_SIZE_USD".to_string(),
        number_to_json(config.sizing.evsnipe_per_hit),
    );
    sizing.insert(
        "EVPOLY_ARB_STRAT_PREMARKET_MAX_USD".to_string(),
        number_to_json(config.caps.premarket),
    );
    sizing.insert(
        "EVPOLY_ARB_STRAT_ENDGAME_MAX_USD".to_string(),
        number_to_json(config.caps.endgame),
    );
    sizing.insert(
        "EVPOLY_ARB_STRAT_EVCURVE_MAX_USD".to_string(),
        number_to_json(config.caps.evcurve),
    );
    sizing.insert(
        "EVPOLY_EVCURVE_STRATEGY_CAP_USD".to_string(),
        number_to_json(config.caps.evcurve),
    );
    sizing.insert(
        "EVPOLY_ARB_STRAT_SESSIONBAND_MAX_USD".to_string(),
        number_to_json(config.caps.session_band),
    );
    sizing.insert(
        "EVPOLY_ARB_STRAT_EVSNIPE_MAX_USD".to_string(),
        number_to_json(config.caps.evsnipe),
    );
    sizing.insert(
        "EVPOLY_EVSNIPE_STRATEGY_CAP_USD".to_string(),
        number_to_json(config.caps.evsnipe),
    );
    sizing.insert(
        "EVPOLY_ENDGAME_PER_PERIOD_CAP_USD".to_string(),
        number_to_json(config.strategy_settings.endgame.per_period_cap_usd),
    );
    sizing.insert(
        "EVPOLY_EVCURVE_D1_STRATEGY_CAP_USD".to_string(),
        number_to_json(config.strategy_settings.evcurve.d1_cap_usd),
    );
    sizing.insert(
        "APP_SIMULATION".to_string(),
        bool_to_json(config.simulation),
    );

    if !config.private_key.trim().is_empty() {
        secrets.insert(
            "POLY_PRIVATE_KEY".to_string(),
            config.private_key.trim().to_string(),
        );
    }
    if !config.relayer_api_key.trim().is_empty() {
        secrets.insert(
            "RELAYER_API_KEY".to_string(),
            config.relayer_api_key.trim().to_string(),
        );
    }
    if !config.relayer_api_key_address.trim().is_empty() {
        secrets.insert(
            "RELAYER_API_KEY_ADDRESS".to_string(),
            config.relayer_api_key_address.trim().to_string(),
        );
    }
    if !config.alpha_key.trim().is_empty() {
        secrets.insert(
            "EVPOLY_ALPHA_KEY".to_string(),
            config.alpha_key.trim().to_string(),
        );
    }
    let relayer_remote_signer_token = clean_relayer_remote_signer_token(config);
    if !relayer_remote_signer_token.is_empty() {
        secrets.insert(
            "EVPOLY_RELAYER_REMOTE_SIGNER_TOKEN".to_string(),
            relayer_remote_signer_token,
        );
    }
    if !config.relayer_submit_signer_url.trim().is_empty() {
        secrets.insert(
            "EVPOLY_RELAYER_SUBMIT_SIGNER_URL".to_string(),
            config.relayer_submit_signer_url.trim().to_string(),
        );
    }
    if !config.wallet_binding.trim().is_empty() {
        secrets.insert(
            "EVPOLY_WALLET_BINDING".to_string(),
            config.wallet_binding.trim().to_string(),
        );
    }
    if !config.onboarding_status.trim().is_empty() {
        secrets.insert(
            "EVPOLY_ONBOARDING_STATUS".to_string(),
            config.onboarding_status.trim().to_string(),
        );
    }
    if !config.approval_status.trim().is_empty() {
        secrets.insert(
            "EVPOLY_APPROVAL_STATUS".to_string(),
            config.approval_status.trim().to_string(),
        );
    }
    if !config.remote_discovery_token.trim().is_empty() {
        secrets.insert(
            "EVPOLY_REMOTE_MARKET_DISCOVERY_TOKEN".to_string(),
            config.remote_discovery_token.trim().to_string(),
        );
    }
    if !config.remote_endgame_alpha_token.trim().is_empty() {
        secrets.insert(
            "EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN".to_string(),
            config.remote_endgame_alpha_token.trim().to_string(),
        );
    }
    if !config.remote_mm_rewards_alpha_token.trim().is_empty() {
        secrets.insert(
            "EVPOLY_REMOTE_MM_REWARDS_ALPHA_TOKEN".to_string(),
            config.remote_mm_rewards_alpha_token.trim().to_string(),
        );
    }
    if !config.remote_evsnipe_discovery_token.trim().is_empty() {
        secrets.insert(
            "EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TOKEN".to_string(),
            config.remote_evsnipe_discovery_token.trim().to_string(),
        );
    }
    if !config.admin_api_token.trim().is_empty() {
        secrets.insert(
            "EVPOLY_ADMIN_API_TOKEN".to_string(),
            config.admin_api_token.trim().to_string(),
        );
    }

    (
        Value::Object(strategy),
        Value::Object(sizing),
        secrets,
        config.eoa_wallet.trim().to_string(),
        config.proxy_wallet.trim().to_string(),
        config.deposit_wallet.trim().to_string(),
        config.sig_type,
    )
}

fn profile_to_desktop_config(profile: &Profile, auth: &AppAuth) -> Result<Value, String> {
    let strategy = profile
        .strategy_config
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let sizing = profile
        .sizing_config
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let default_eth_enabled = config_io::env_template_default_bool("POLY_ENABLE_ETH_TRADING", true);
    let default_solana_enabled =
        config_io::env_template_default_bool("POLY_ENABLE_SOLANA_TRADING", true);
    let default_xrp_enabled = config_io::env_template_default_bool("POLY_ENABLE_XRP_TRADING", true);
    let default_simulation = config_io::env_template_default_bool("APP_SIMULATION", false);
    let default_premarket_ladder_mode_m5 = default_premarket_ladder_mode_m5();
    let default_premarket_ladder_mode_non_m5 = default_premarket_ladder_mode_non_m5();
    let premarket_ladder_mode_m5 = normalize_premarket_ladder_mode(Some(
        strategy
            .get(PREMARKET_LADDER_MODE_ENV_KEY_5M)
            .and_then(Value::as_str)
            .or_else(|| {
                strategy
                    .get(PREMARKET_LADDER_MODE_ENV_KEY_SHARED)
                    .and_then(Value::as_str)
            })
            .unwrap_or(default_premarket_ladder_mode_m5.as_str()),
    ));
    let premarket_ladder_mode_non_m5 = normalize_premarket_ladder_mode(Some(
        strategy
            .get(PREMARKET_LADDER_MODE_ENV_KEY_NON_M5)
            .and_then(Value::as_str)
            .or_else(|| {
                strategy
                    .get(PREMARKET_LADDER_MODE_ENV_KEY_NON_M5_LEGACY)
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                strategy
                    .get(PREMARKET_LADDER_MODE_ENV_KEY_SHARED)
                    .and_then(Value::as_str)
            })
            .unwrap_or(default_premarket_ladder_mode_non_m5.as_str()),
    ));
    let premarket_safe_bias_pct = normalize_premarket_ladder_bias_pct(
        f64_from_object(
            &strategy,
            PREMARKET_SAFE_BIAS_PCT_ENV_KEY,
            default_premarket_safe_bias_pct(),
        ),
        -10.0,
    );
    let premarket_aggressive_bias_pct = normalize_premarket_ladder_bias_pct(
        f64_from_object(
            &strategy,
            PREMARKET_AGGRESSIVE_BIAS_PCT_ENV_KEY,
            default_premarket_aggressive_bias_pct(),
        ),
        10.0,
    );

    let mut symbols = vec!["BTC".to_string()];
    if bool_from_object(&strategy, "POLY_ENABLE_ETH_TRADING", default_eth_enabled) {
        symbols.push("ETH".to_string());
    }
    if bool_from_object(
        &strategy,
        "POLY_ENABLE_SOLANA_TRADING",
        default_solana_enabled,
    ) {
        symbols.push("SOL".to_string());
    }
    if bool_from_object(&strategy, "POLY_ENABLE_XRP_TRADING", default_xrp_enabled) {
        symbols.push("XRP".to_string());
    }
    if !strategy.contains_key("EVPOLY_ENDGAME_SYMBOLS")
        && !strategy.contains_key("EVPOLY_EVCURVE_SYMBOLS")
        && !strategy.contains_key("EVPOLY_EVSNIPE_SYMBOLS")
    {
        for sym in DESKTOP_SYMBOL_ORDER.iter().skip(4) {
            symbols.push((*sym).to_string());
        }
    }
    if let Some(extra) = strategy
        .get("EVPOLY_ENDGAME_SYMBOLS")
        .and_then(Value::as_str)
    {
        for sym in extra.split(',').map(|s| s.trim().to_ascii_uppercase()) {
            if DESKTOP_SYMBOL_ORDER.iter().any(|v| *v == sym) && !symbols.iter().any(|s| s == &sym)
            {
                symbols.push(sym);
            }
        }
    }
    if let Some(extra) = strategy
        .get("EVPOLY_EVSNIPE_SYMBOLS")
        .and_then(Value::as_str)
    {
        for sym in extra.split(',').map(|s| s.trim().to_ascii_uppercase()) {
            if DESKTOP_SYMBOL_ORDER.iter().any(|v| *v == sym) && !symbols.iter().any(|s| s == &sym)
            {
                symbols.push(sym);
            }
        }
    }
    symbols = normalize_symbols(&symbols);

    let secrets = decrypt_profile_secrets(profile, auth)?;
    let evsnipe_ratio = f64_from_object(
        &strategy,
        "EVPOLY_EVSNIPE_PRE_LEG_RATIO",
        config_io::env_template_default_f64("EVPOLY_EVSNIPE_PRE_LEG_RATIO", 0.30),
    );
    let evsnipe_saved_ratio = f64_from_object(
        &strategy,
        "APP_DESKTOP_EVSNIPE_PRE_LEG_RATIO_SAVED",
        config_io::env_template_default_f64("EVPOLY_EVSNIPE_PRE_LEG_RATIO", 0.30),
    );
    let evsnipe_display_ratio = if evsnipe_ratio > 0.0 {
        evsnipe_ratio
    } else {
        evsnipe_saved_ratio
    };
    let sessionband_allowed_tau =
        csv_from_object(&strategy, "EVPOLY_SESSIONBAND_ALLOWED_TAU_SEC", &["2", "1"]);
    let sessionband_tau2_enabled = sessionband_allowed_tau.iter().any(|value| value == "2");
    let sessionband_tau1_enabled = sessionband_allowed_tau.iter().any(|value| value == "1");
    let mm_sport_quote_size_mode =
        string_from_object(&strategy, "EVPOLY_MM_SPORT_QUOTE_SIZE_MODE", "depth_ratio");
    let mm_sport_quote_size_multiplier = f64_from_object(
        &strategy,
        "EVPOLY_MM_SPORT_QUOTE_SIZE_MULT",
        config_io::env_template_default_f64("EVPOLY_MM_SPORT_QUOTE_SIZE_MULT", 1.2),
    );
    let mm_sport_multiple_collateral_cap_mult = 0.45;
    let mm_sport_depth_ratio_collateral_cap_mult = 0.45;
    let mm_sport_max_share_ratio = f64_from_object(
        &strategy,
        "EVPOLY_MM_SPORT_MAX_SHARE_RATIO",
        config_io::env_template_default_f64("EVPOLY_MM_SPORT_MAX_SHARE_RATIO", 0.20),
    );
    let mm_sport_max_quote_shares = f64_from_object(
        &strategy,
        "EVPOLY_MM_SPORT_MAX_QUOTE_SHARES",
        config_io::env_template_default_f64("EVPOLY_MM_SPORT_MAX_QUOTE_SHARES", 1000.0),
    );
    let mm_sport_min_top_depth_usd = f64_from_object(
        &strategy,
        "EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD",
        config_io::env_template_default_f64("EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD", 1100.0),
    );
    let mm_sport_discovery_route = normalize_mm_sport_discovery_route(
        string_from_object(&strategy, "EVPOLY_MM_SPORT_DISCOVERY_ROUTE", "sports").as_str(),
    );
    let (default_active_sport_market_cap, default_active_nonsport_market_cap) =
        mm_sport_route_default_caps(mm_sport_discovery_route);
    let (mm_sport_quote_cooldown_min_sec, mm_sport_quote_cooldown_max_sec) =
        normalize_cooldown_pair_f64(
            f64_from_object(
                &strategy,
                "EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MIN_SEC",
                config_io::env_template_default_f64("EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MIN_SEC", 10.0),
            ),
            f64_from_object(
                &strategy,
                "EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MAX_SEC",
                config_io::env_template_default_f64("EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MAX_SEC", 60.0),
            ),
        );
    let mm_sport_active_sport_market_cap = normalize_nonnegative_integer_f64(
        f64_from_object(
            &strategy,
            "EVPOLY_MM_SPORT_ACTIVE_SPORT_MARKET_CAP",
            config_io::env_template_default_f64(
                "EVPOLY_MM_SPORT_ACTIVE_SPORT_MARKET_CAP",
                default_active_sport_market_cap,
            ),
        ),
        default_active_sport_market_cap,
    );
    let mm_sport_active_nonsport_market_cap = normalize_nonnegative_integer_f64(
        f64_from_object(
            &strategy,
            "EVPOLY_MM_SPORT_ACTIVE_NONSPORT_MARKET_CAP",
            config_io::env_template_default_f64(
                "EVPOLY_MM_SPORT_ACTIVE_NONSPORT_MARKET_CAP",
                default_active_nonsport_market_cap,
            ),
        ),
        default_active_nonsport_market_cap,
    );

    let relayer_remote_signer_token = secrets
        .get("EVPOLY_RELAYER_REMOTE_SIGNER_TOKEN")
        .cloned()
        .unwrap_or_default();
    let legacy_remote_signer_token = secrets
        .get("EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN")
        .cloned()
        .or_else(|| secrets.get("EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN").cloned())
        .unwrap_or_default();
    let order_signer_primary_token_internal = distinct_order_signer_primary_token(
        relayer_remote_signer_token.as_str(),
        secrets
            .get("EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN")
            .map(String::as_str)
            .unwrap_or_default(),
    );

    Ok(serde_json::json!({
        "private_key": secrets.get("POLY_PRIVATE_KEY").cloned().unwrap_or_default(),
        "eoa_wallet": profile.eoa_wallet_address.clone(),
        "proxy_wallet": profile.proxy_wallet_address.clone(),
        "deposit_wallet": profile.deposit_wallet_address.clone(),
        "sig_type": profile.signature_type,
        "weekend_policy": normalize_weekend_policy(strategy.get(WEEKEND_POLICY_ENV_KEY).and_then(Value::as_str)),
        "symbols": symbols,
        "strategies": {
            "premarket": bool_from_object(&strategy, "EVPOLY_STRATEGY_PREMARKET_ENABLE", config_io::env_template_default_bool("EVPOLY_STRATEGY_PREMARKET_ENABLE", true)),
            "endgame": bool_from_object(&strategy, "EVPOLY_STRATEGY_ENDGAME_ENABLE", config_io::env_template_default_bool("EVPOLY_STRATEGY_ENDGAME_ENABLE", true)),
            "evcurve": bool_from_object(&strategy, "EVPOLY_STRATEGY_EVCURVE_ENABLE", config_io::env_template_default_bool("EVPOLY_STRATEGY_EVCURVE_ENABLE", false)),
            "session_band": false,
            "evsnipe": bool_from_object(&strategy, "EVPOLY_STRATEGY_EVSNIPE_ENABLE", config_io::env_template_default_bool("EVPOLY_STRATEGY_EVSNIPE_ENABLE", true)),
            "mm_rewards": false,
            "mm_sport": bool_from_object(&strategy, "EVPOLY_STRATEGY_MM_SPORT_ENABLE", config_io::env_template_default_bool("EVPOLY_STRATEGY_MM_SPORT_ENABLE", false))
        },
        "sizing": {
            "premarket": f64_from_object(&sizing, "EVPOLY_PREMARKET_BASE_SIZE_USD", 10.0),
            "endgame": f64_from_object(
                &sizing,
                "EVPOLY_ENDGAME_BASE_SIZE_USD",
                config_io::env_template_default_f64("EVPOLY_ENDGAME_BASE_SIZE_USD", 50.0),
            ),
            "evcurve": f64_from_object(&sizing, "EVPOLY_EVCURVE_BASE_SIZE_USD", 10.0),
            "session_band": f64_from_object(&sizing, "EVPOLY_SESSIONBAND_BASE_SIZE_USD", 10.0),
            "evsnipe_per_hit": f64_from_object(&sizing, "EVPOLY_EVSNIPE_SIZE_USD", 5.0)
        },
        "caps": {
            "premarket": f64_from_object(&sizing, "EVPOLY_ARB_STRAT_PREMARKET_MAX_USD", 100000.0),
            "endgame": f64_from_object(&sizing, "EVPOLY_ARB_STRAT_ENDGAME_MAX_USD", 100000.0),
            "evcurve": f64_from_object(&sizing, "EVPOLY_ARB_STRAT_EVCURVE_MAX_USD", 100000.0),
            "session_band": f64_from_object(&sizing, "EVPOLY_ARB_STRAT_SESSIONBAND_MAX_USD", 100000.0),
            "evsnipe": f64_from_object(&sizing, "EVPOLY_ARB_STRAT_EVSNIPE_MAX_USD", 10000.0)
        },
        "mm_tuning": {
            "rewards_min_share_multiple": 1.0,
            "sport_quote_size_multiplier": mm_sport_quote_size_multiplier,
            "nonsport_quote_size_multiplier": f64_from_object(&strategy, "EVPOLY_MM_SPORT_NONSPORT_QUOTE_SIZE_MULT", mm_sport_quote_size_multiplier)
        },
        "size_policy": {
            "symbol_multipliers": {
                "btc": f64_from_object(&strategy, "EVPOLY_SYMBOL_SIZE_MULTIPLIER_BTC", config_io::env_template_default_f64("EVPOLY_SYMBOL_SIZE_MULTIPLIER_BTC", 1.0)),
                "eth": f64_from_object(&strategy, "EVPOLY_SYMBOL_SIZE_MULTIPLIER_ETH", config_io::env_template_default_f64("EVPOLY_SYMBOL_SIZE_MULTIPLIER_ETH", 0.8)),
                "sol": f64_from_object(&strategy, "EVPOLY_SYMBOL_SIZE_MULTIPLIER_SOL", config_io::env_template_default_f64("EVPOLY_SYMBOL_SIZE_MULTIPLIER_SOL", 0.5)),
                "xrp": f64_from_object(&strategy, "EVPOLY_SYMBOL_SIZE_MULTIPLIER_XRP", config_io::env_template_default_f64("EVPOLY_SYMBOL_SIZE_MULTIPLIER_XRP", 0.5)),
                "doge": f64_from_object(&strategy, "EVPOLY_SYMBOL_SIZE_MULTIPLIER_DOGE", config_io::env_template_default_f64("EVPOLY_SYMBOL_SIZE_MULTIPLIER_DOGE", 0.5)),
                "bnb": f64_from_object(&strategy, "EVPOLY_SYMBOL_SIZE_MULTIPLIER_BNB", config_io::env_template_default_f64("EVPOLY_SYMBOL_SIZE_MULTIPLIER_BNB", 0.5)),
                "hype": f64_from_object(&strategy, "EVPOLY_SYMBOL_SIZE_MULTIPLIER_HYPE", config_io::env_template_default_f64("EVPOLY_SYMBOL_SIZE_MULTIPLIER_HYPE", 0.5))
            },
            "premarket_timeframe_multipliers": {
                "m5": f64_from_object(&strategy, "EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_5M", config_io::env_template_default_f64("EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_5M", 0.75)),
                "m15": f64_from_object(&strategy, "EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_15M", config_io::env_template_default_f64("EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_15M", 1.0)),
                "h1": f64_from_object(&strategy, "EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_1H", config_io::env_template_default_f64("EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_1H", 1.25)),
                "h4": f64_from_object(&strategy, "EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_4H", config_io::env_template_default_f64("EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_4H", 1.25)),
                "d1": f64_from_object(&strategy, "EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_1D", config_io::env_template_default_f64("EVPOLY_PREMARKET_TIMEFRAME_MULTIPLIER_1D", 1.25))
            },
            "evcurve_timeframe_multipliers": {
                "m15": f64_from_object(&strategy, "EVPOLY_EVCURVE_TIMEFRAME_MULTIPLIER_15M", config_io::env_template_default_f64("EVPOLY_EVCURVE_TIMEFRAME_MULTIPLIER_15M", 0.75)),
                "h1": f64_from_object(&strategy, "EVPOLY_EVCURVE_TIMEFRAME_MULTIPLIER_1H", config_io::env_template_default_f64("EVPOLY_EVCURVE_TIMEFRAME_MULTIPLIER_1H", 1.0)),
                "h4": f64_from_object(&strategy, "EVPOLY_EVCURVE_TIMEFRAME_MULTIPLIER_4H", config_io::env_template_default_f64("EVPOLY_EVCURVE_TIMEFRAME_MULTIPLIER_4H", 1.25)),
                "d1": f64_from_object(&strategy, "EVPOLY_EVCURVE_TIMEFRAME_MULTIPLIER_1D", config_io::env_template_default_f64("EVPOLY_EVCURVE_TIMEFRAME_MULTIPLIER_1D", 1.25))
            }
        },
        "strategy_settings": {
            "premarket": {
                "tp_enabled": bool_from_object(&strategy, "EVPOLY_PREMARKET_TP_ENABLE", config_io::env_template_default_bool("EVPOLY_PREMARKET_TP_ENABLE", true)),
                "active_cap_per_asset": f64_from_object(&strategy, "EVPOLY_PREMARKET_ACTIVE_CAP_PER_ASSET", config_io::env_template_default_f64("EVPOLY_PREMARKET_ACTIVE_CAP_PER_ASSET", 100.0)),
                "timeframes": csv_from_object(&strategy, "EVPOLY_PREMARKET_TIMEFRAMES", &["5m", "15m", "1h", "4h"]),
                "ladder_mode_m5": premarket_ladder_mode_m5,
                "ladder_mode_non_m5": premarket_ladder_mode_non_m5,
                "safe_bias_pct": premarket_safe_bias_pct,
                "aggressive_bias_pct": premarket_aggressive_bias_pct,
                "cancel_after_open_sec": {
                    "m5": f64_from_object(&strategy, "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_5M_SEC", config_io::env_template_default_f64("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_5M_SEC", 20.0)),
                    "m15": f64_from_object(&strategy, "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_15M_SEC", config_io::env_template_default_f64("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_15M_SEC", 40.0)),
                    "h1": f64_from_object(&strategy, "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_1H_SEC", config_io::env_template_default_f64("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_1H_SEC", 60.0)),
                    "h4": f64_from_object(&strategy, "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_4H_SEC", config_io::env_template_default_f64("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_4H_SEC", 180.0))
                }
            },
            "endgame": {
                "timeframes": default_endgame_timeframes(),
                "per_period_cap_usd": f64_from_object(&sizing, "EVPOLY_ENDGAME_PER_PERIOD_CAP_USD", config_io::env_template_default_f64("EVPOLY_ENDGAME_PER_PERIOD_CAP_USD", 10000.0)),
                "tick0_multiplier": 0.20,
                "tick1_multiplier": 0.40,
                "tick2_multiplier": 0.40
            },
            "evcurve": {
                "timeframes": csv_from_object(&strategy, "EVPOLY_EVCURVE_TIMEFRAMES", &["15m", "1h", "4h", "1d"]),
                "min_buy_price": f64_from_object(&strategy, "EVPOLY_EVCURVE_MIN_BUY_PRICE", config_io::env_template_default_f64("EVPOLY_EVCURVE_MIN_BUY_PRICE", 0.60)),
                "d1_enabled": bool_from_object(&strategy, "EVPOLY_EVCURVE_D1_ENABLE", config_io::env_template_default_bool("EVPOLY_EVCURVE_D1_ENABLE", true)),
                "d1_cap_usd": f64_from_object(&sizing, "EVPOLY_EVCURVE_D1_STRATEGY_CAP_USD", config_io::env_template_default_f64("EVPOLY_EVCURVE_D1_STRATEGY_CAP_USD", 10000.0))
            },
            "session_band": {
                "timeframes": csv_from_object(&strategy, "EVPOLY_SESSIONBAND_TIMEFRAMES", &["5m", "15m", "1h", "4h"]),
                "flip_threshold_pct": f64_from_object(&strategy, "EVPOLY_SESSIONBAND_FLIP_THRESHOLD_PCT", config_io::env_template_default_f64("EVPOLY_SESSIONBAND_FLIP_THRESHOLD_PCT", 2.0)),
                "tau2_enabled": sessionband_tau2_enabled,
                "tau1_enabled": sessionband_tau1_enabled,
                "tau2_multiplier": f64_from_object(&strategy, "EVPOLY_SESSIONBAND_TAU2_MULTIPLIER", config_io::env_template_default_f64("EVPOLY_SESSIONBAND_TAU2_MULTIPLIER", 0.30)),
                "tau1_multiplier": f64_from_object(&strategy, "EVPOLY_SESSIONBAND_TAU1_MULTIPLIER", config_io::env_template_default_f64("EVPOLY_SESSIONBAND_TAU1_MULTIPLIER", 0.70))
            },
            "evsnipe": {
                "pre_hit_enabled": evsnipe_ratio > 0.0,
                "pre_leg_ratio": evsnipe_display_ratio,
                "saved_pre_leg_ratio": evsnipe_saved_ratio,
                "pre_trigger_bps": f64_from_object(&strategy, "EVPOLY_EVSNIPE_PRE_TRIGGER_BPS", config_io::env_template_default_f64("EVPOLY_EVSNIPE_PRE_TRIGGER_BPS", 1.0)),
                "strike_window_pct": f64_from_object(&strategy, "EVPOLY_EVSNIPE_STRIKE_WINDOW_PCT", config_io::env_template_default_f64("EVPOLY_EVSNIPE_STRIKE_WINDOW_PCT", 0.10)),
                "max_days_to_expiry": f64_from_object(&strategy, "EVPOLY_EVSNIPE_MAX_DAYS_TO_EXPIRY", config_io::env_template_default_f64("EVPOLY_EVSNIPE_MAX_DAYS_TO_EXPIRY", 30.0))
            },
            "mm_rewards": {
                "market_mode": normalize_mm_rewards_market_mode(
                    string_from_object(&strategy, "EVPOLY_MM_MARKET_MODE", config_io::DEFAULT_MM_MARKET_MODE).as_str()
                ),
                "single_market_slugs": string_from_object(&strategy, "EVPOLY_MM_SINGLE_MARKET_SLUGS", ""),
                "auto_top_n": f64_from_object(&strategy, "EVPOLY_MM_AUTO_TOP_N", config_io::env_template_default_f64("EVPOLY_MM_AUTO_TOP_N", 80.0)),
                "auto_refresh_sec": f64_from_object(&strategy, "EVPOLY_MM_AUTO_REFRESH_SEC", config_io::env_template_default_f64("EVPOLY_MM_AUTO_REFRESH_SEC", 900.0)),
                "auto_rank_budget_usd": f64_from_object(&strategy, "EVPOLY_MM_AUTO_RANK_BUDGET_USD", config_io::env_template_default_f64("EVPOLY_MM_AUTO_RANK_BUDGET_USD", 2000.0)),
                "blacklist_keywords": string_from_object(&strategy, "EVPOLY_MM_MARKET_BLACKLIST_KEYWORDS", ""),
                "reward_min_shares_cap": f64_from_object(&strategy, "EVPOLY_MM_REWARD_MIN_SHARES_CAP", config_io::env_template_default_f64("EVPOLY_MM_REWARD_MIN_SHARES_CAP", 0.0))
            },
            "mm_sport": {
                "discovery_route": mm_sport_discovery_route,
                "quote_size_mode": normalize_mm_sport_quote_size_mode(
                    mm_sport_quote_size_mode.as_str()
                ),
                "nonsport_quote_size_mode": normalize_mm_sport_quote_size_mode(
                    string_from_object(&strategy, "EVPOLY_MM_SPORT_NONSPORT_QUOTE_SIZE_MODE", mm_sport_quote_size_mode.as_str()).as_str()
                ),
                "entry_price_mode": normalize_mm_sport_entry_price_mode(
                    string_from_object(&strategy, "EVPOLY_MM_SPORT_ENTRY_PRICE_MODE", "best_bid").as_str()
                ),
                "multiple_collateral_cap_mult": mm_sport_multiple_collateral_cap_mult,
                "nonsport_multiple_collateral_cap_mult": mm_sport_multiple_collateral_cap_mult,
                "depth_ratio_collateral_cap_mult": mm_sport_depth_ratio_collateral_cap_mult,
                "nonsport_depth_ratio_collateral_cap_mult": mm_sport_depth_ratio_collateral_cap_mult,
                "min_reward_rate_per_day": f64_from_object(&strategy, "EVPOLY_MM_SPORT_MIN_REWARD_RATE_PER_DAY", config_io::env_template_default_f64("EVPOLY_MM_SPORT_MIN_REWARD_RATE_PER_DAY", 5.0)),
                "match_only": bool_from_object(&strategy, "EVPOLY_MM_SPORT_MATCH_ONLY", config_io::env_template_default_bool("EVPOLY_MM_SPORT_MATCH_ONLY", true)),
                "allowed_sport_league_codes": string_from_object(&strategy, "EVPOLY_MM_SPORT_ALLOWED_SPORT_LEAGUE_CODES", ""),
                "blocked_sport_league_codes": string_from_object(&strategy, "EVPOLY_MM_SPORT_BLOCKED_SPORT_LEAGUE_CODES", ""),
                "blocked_competition_levels": string_from_object(&strategy, "EVPOLY_MM_SPORT_BLOCKED_COMPETITION_LEVELS", ""),
                "market_allowlist_keywords": string_from_object(&strategy, "EVPOLY_MM_SPORT_MARKET_ALLOWLIST_KEYWORDS", ""),
                "market_blacklist_keywords": string_from_object(&strategy, "EVPOLY_MM_SPORT_MARKET_BLACKLIST_KEYWORDS", config_io::env_template_default_string("EVPOLY_MM_SPORT_MARKET_BLACKLIST_KEYWORDS").as_deref().unwrap_or("")),
                "reward_min_shares_cap": f64_from_object(&strategy, "EVPOLY_MM_SPORT_REWARD_MIN_SHARES_CAP", config_io::env_template_default_f64("EVPOLY_MM_SPORT_REWARD_MIN_SHARES_CAP", 0.0)),
                "polymarket_live_guard_enable": bool_from_object(&strategy, "EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_ENABLE", config_io::env_template_default_bool("EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_ENABLE", true)),
                "polymarket_live_guard_ws_enable": bool_from_object(&strategy, "EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_WS_ENABLE", config_io::env_template_default_bool("EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_WS_ENABLE", true)),
                "polymarket_live_guard_ws_stale_ms": f64_from_object(&strategy, "EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_WS_STALE_MS", config_io::env_template_default_f64("EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_WS_STALE_MS", 600000.0)),
                "pause_after_fill_sec": f64_from_object(&strategy, "EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC", config_io::env_template_default_f64("EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC", 3600.0)),
                "inventory_exit_start_hours": f64_from_object(&strategy, "EVPOLY_MM_SPORT_INVENTORY_EXIT_START_SEC", config_io::env_template_default_f64("EVPOLY_MM_SPORT_INVENTORY_EXIT_START_SEC", 3600.0)) / 3600.0,
                "nonsport_end_exit_start_hours": f64_from_object(&strategy, "EVPOLY_MM_SPORT_NONSPORT_END_EXIT_START_SEC", config_io::env_template_default_f64("EVPOLY_MM_SPORT_NONSPORT_END_EXIT_START_SEC", 172800.0)) / 3600.0,
                "sport_entry_schedule_enabled": bool_from_object(&strategy, "EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_ENABLE", bool_from_object(&strategy, "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_ENABLE", config_io::env_template_default_bool("EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_ENABLE", false))),
                "sport_entry_schedule_days_utc": string_from_object(&strategy, "EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_DAYS_UTC", string_from_object(&strategy, "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_DAYS_UTC", config_io::env_template_default_string("EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_DAYS_UTC").as_deref().unwrap_or("mon,tue,wed,thu,fri")).as_str()),
                "sport_entry_schedule_start_minute_utc": normalize_utc_minute_f64(f64_from_object(&strategy, "EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_START_MINUTE_UTC", f64_from_object(&strategy, "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_START_MINUTE_UTC", config_io::env_template_default_f64("EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_START_MINUTE_UTC", 780.0))), 780.0),
                "sport_entry_schedule_end_minute_utc": normalize_utc_minute_f64(f64_from_object(&strategy, "EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_END_MINUTE_UTC", f64_from_object(&strategy, "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_END_MINUTE_UTC", config_io::env_template_default_f64("EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_END_MINUTE_UTC", 240.0))), 240.0),
                "nonsport_entry_schedule_enabled": bool_from_object(&strategy, "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_ENABLE", config_io::env_template_default_bool("EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_ENABLE", false)),
                "nonsport_entry_schedule_days_utc": string_from_object(&strategy, "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_DAYS_UTC", config_io::env_template_default_string("EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_DAYS_UTC").as_deref().unwrap_or("mon,tue,wed,thu,fri")),
                "nonsport_entry_schedule_start_minute_utc": normalize_utc_minute_f64(f64_from_object(&strategy, "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_START_MINUTE_UTC", config_io::env_template_default_f64("EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_START_MINUTE_UTC", 780.0)), 780.0),
                "nonsport_entry_schedule_end_minute_utc": normalize_utc_minute_f64(f64_from_object(&strategy, "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_END_MINUTE_UTC", config_io::env_template_default_f64("EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_END_MINUTE_UTC", 240.0)), 240.0),
                "inventory_exit_max_loss_cents": f64_from_object(&strategy, "EVPOLY_MM_SPORT_INVENTORY_EXIT_MAX_LOSS_CENTS", config_io::env_template_default_f64("EVPOLY_MM_SPORT_INVENTORY_EXIT_MAX_LOSS_CENTS", 10.0)),
                "inventory_exit_mode": normalize_mm_sport_exit_mode(
                    string_from_object(&strategy, "EVPOLY_MM_SPORT_EXIT_MODE", "normal").as_str()
                ),
                "max_share_ratio": mm_sport_max_share_ratio,
                "nonsport_max_share_ratio": f64_from_object(&strategy, "EVPOLY_MM_SPORT_NONSPORT_MAX_SHARE_RATIO", mm_sport_max_share_ratio),
                "max_quote_shares": mm_sport_max_quote_shares,
                "nonsport_max_quote_shares": f64_from_object(&strategy, "EVPOLY_MM_SPORT_NONSPORT_MAX_QUOTE_SHARES", config_io::env_template_default_f64("EVPOLY_MM_SPORT_NONSPORT_MAX_QUOTE_SHARES", 200.0)),
                "min_top_depth_usd": mm_sport_min_top_depth_usd,
                "nonsport_min_top_depth_usd": f64_from_object(&strategy, "EVPOLY_MM_SPORT_NONSPORT_MIN_TOP_DEPTH_USD", mm_sport_min_top_depth_usd),
                "min_entry_top_bid_price": f64_from_object(&strategy, "EVPOLY_MM_SPORT_MIN_ENTRY_TOP_BID_PRICE", config_io::env_template_default_f64("EVPOLY_MM_SPORT_MIN_ENTRY_TOP_BID_PRICE", 0.05)),
                "allow_sponsored_rewards": bool_from_object(&strategy, "EVPOLY_MM_SPORT_ALLOW_SPONSORED_REWARDS", config_io::env_template_default_bool("EVPOLY_MM_SPORT_ALLOW_SPONSORED_REWARDS", true)),
                "sponsored_reward_min_share": f64_from_object(&strategy, "EVPOLY_MM_SPORT_SPONSORED_REWARD_MIN_SHARE", config_io::env_template_default_f64("EVPOLY_MM_SPORT_SPONSORED_REWARD_MIN_SHARE", 0.50)),
                "quote_expiry_min_sec": f64_from_object(&strategy, "EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC", config_io::env_template_default_f64("EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC", 65.0)),
                "quote_expiry_max_sec": f64_from_object(&strategy, "EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC", config_io::env_template_default_f64("EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC", 185.0)),
                "quote_cooldown_min_sec": mm_sport_quote_cooldown_min_sec,
                "quote_cooldown_max_sec": mm_sport_quote_cooldown_max_sec,
                "fifo_max_share_ratio": f64_from_object(&strategy, "EVPOLY_MM_SPORT_FIFO_MAX_SHARE_RATIO", config_io::env_template_default_f64("EVPOLY_MM_SPORT_FIFO_MAX_SHARE_RATIO", 0.50)),
                "active_sport_market_cap": mm_sport_active_sport_market_cap,
                "active_nonsport_market_cap": mm_sport_active_nonsport_market_cap
            }
        },
        "simulation": bool_from_object(&sizing, "APP_SIMULATION", default_simulation),
        "alpha_key": secrets.get("EVPOLY_ALPHA_KEY").cloned().unwrap_or_default(),
        "relayer_api_key": secrets.get("RELAYER_API_KEY").cloned().unwrap_or_default(),
        "relayer_api_key_address": secrets.get("RELAYER_API_KEY_ADDRESS").cloned().unwrap_or_default(),
        "relayer_remote_signer_token": relayer_remote_signer_token,
        "relayer_submit_signer_url": secrets.get("EVPOLY_RELAYER_SUBMIT_SIGNER_URL").cloned().unwrap_or_default(),
        "wallet_binding": secrets.get("EVPOLY_WALLET_BINDING").cloned().unwrap_or_default(),
        "onboarding_status": secrets.get("EVPOLY_ONBOARDING_STATUS").cloned().unwrap_or_default(),
        "approval_status": secrets.get("EVPOLY_APPROVAL_STATUS").cloned().unwrap_or_default(),
        "remote_signer_token": legacy_remote_signer_token,
        "order_signer_primary_token_internal": order_signer_primary_token_internal,
        "remote_discovery_token": secrets.get("EVPOLY_REMOTE_MARKET_DISCOVERY_TOKEN").cloned().unwrap_or_default(),
        "remote_endgame_alpha_token": secrets.get("EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN").cloned().unwrap_or_default(),
        "remote_mm_rewards_alpha_token": secrets.get("EVPOLY_REMOTE_MM_REWARDS_ALPHA_TOKEN").cloned().unwrap_or_default(),
        "remote_evsnipe_discovery_token": secrets.get("EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TOKEN").cloned().unwrap_or_default(),
        "admin_api_token": ensure_admin_api_token(
            secrets
                .get("EVPOLY_ADMIN_API_TOKEN")
                .map(String::as_str)
                .unwrap_or_default()
        )
    }))
}

fn resolve_tracking_db_path(data_dir: &Path) -> PathBuf {
    let mut candidates = vec![data_dir.join("tracking.db")];

    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("tracking.db"));
        if let Some(parent) = cwd.parent() {
            candidates.push(parent.join("tracking.db"));
            if let Some(grand) = parent.parent() {
                candidates.push(grand.join("tracking.db"));
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("tracking.db"));
            if let Some(grand) = parent.parent() {
                candidates.push(grand.join("tracking.db"));
            }
        }
    }

    for path in candidates {
        if path.exists() {
            return path;
        }
    }
    data_dir.join("tracking.db")
}

fn configure_tracking_connection(conn: &Connection) {
    let _ = conn.busy_timeout(Duration::from_millis(1_500));
    let _ = conn.execute_batch("PRAGMA temp_store=MEMORY;");
}

fn open_tracking_connection(data_dir: &Path) -> Option<Connection> {
    let db_path = resolve_tracking_db_path(data_dir);
    let conn = Connection::open(&db_path).ok()?;
    configure_tracking_connection(&conn);
    Some(conn)
}

fn ensure_tracking_db_indexes(data_dir: &Path, allow_large_create: bool) {
    let db_path = resolve_tracking_db_path(data_dir);
    if !db_path.exists() {
        return;
    }
    let Ok(conn) = Connection::open(&db_path) else {
        return;
    };
    configure_tracking_connection(&conn);
    let db_bytes = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    let allow_create = allow_large_create || db_bytes <= 512 * 1024 * 1024;
    let mut skipped = 0usize;
    for (name, sql) in [
        (
            "idx_evpoly_trade_events_event_ts",
            "CREATE INDEX IF NOT EXISTS idx_evpoly_trade_events_event_ts ON trade_events(event_type, ts_ms)",
        ),
        (
            "idx_evpoly_fills_v2_ts",
            "CREATE INDEX IF NOT EXISTS idx_evpoly_fills_v2_ts ON fills_v2(ts_ms)",
        ),
        (
            "idx_evpoly_fills_v2_created_ts",
            "CREATE INDEX IF NOT EXISTS idx_evpoly_fills_v2_created_ts ON fills_v2(created_at_ms)",
        ),
        (
            "idx_evpoly_positions_v2_status",
            "CREATE INDEX IF NOT EXISTS idx_evpoly_positions_v2_status ON positions_v2(status)",
        ),
        (
            "idx_evpoly_marks_v2_position_ts",
            "CREATE INDEX IF NOT EXISTS idx_evpoly_marks_v2_position_ts ON marks_v2(position_key, ts_ms)",
        ),
    ] {
        let exists = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1 LIMIT 1",
                [name],
                |_| Ok(()),
            )
            .is_ok();
        if exists {
            continue;
        }
        if allow_create {
            let _ = conn.execute(sql, []);
        } else {
            skipped = skipped.saturating_add(1);
        }
    }
    if skipped > 0 {
        append_desktop_debug_line(
            data_dir,
            "SYSTEM",
            format!(
                "tracking index creation skipped for large db bytes={} missing_indexes={}",
                db_bytes, skipped
            )
            .as_str(),
        );
    }
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
}

fn append_desktop_debug_line(data_dir: &Path, source: &str, line: &str) {
    let ts = Utc::now().to_rfc3339();
    let content = format!("[{ts}] [{source}] {line}\n");
    for file_name in [DESKTOP_DEBUG_LOG_NAME, FULL_DEBUG_LOG_NAME] {
        let path = data_dir.join(file_name);
        let _ = log_stream::append_file_log_line(&path, &content);
    }
}

fn ensure_debug_log_files(data_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(data_dir).map_err(|e| format!("prepare logs folder: {e}"))?;
    for file_name in [
        DESKTOP_DEBUG_LOG_NAME,
        FULL_DEBUG_LOG_NAME,
        BOT_DEBUG_LOG_NAME,
    ] {
        let path = data_dir.join(file_name);
        let _ = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .map_err(|e| format!("prepare log file {file_name}: {e}"))?;
    }
    Ok(())
}

fn count_enabled_strategies(profile: &Profile) -> usize {
    let strategy = profile
        .strategy_config
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    [
        ("EVPOLY_STRATEGY_PREMARKET_ENABLE", true),
        ("EVPOLY_STRATEGY_ENDGAME_ENABLE", true),
        ("EVPOLY_STRATEGY_EVCURVE_ENABLE", true),
        ("EVPOLY_STRATEGY_SESSIONBAND_ENABLE", false),
        ("EVPOLY_STRATEGY_EVSNIPE_ENABLE", true),
        ("EVPOLY_STRATEGY_MM_REWARDS_ENABLE", false),
        ("EVPOLY_STRATEGY_MM_SPORT_ENABLE", false),
    ]
    .into_iter()
    .filter(|(key, default)| bool_from_object(&strategy, key, *default))
    .count()
}

fn start_of_current_utc_day_ms() -> i64 {
    let now = Utc::now();
    let date = now.date_naive();
    date.and_hms_opt(0, 0, 0)
        .map(|dt| dt.and_utc().timestamp_millis())
        .unwrap_or(0)
}

fn ack_latency_window_start_ms() -> i64 {
    Utc::now().timestamp_millis() - (24 * 60 * 60 * 1000)
}

fn profile_stats_start_ms(profile: &Profile) -> i64 {
    DateTime::parse_from_rfc3339(profile.created_at.as_str())
        .ok()
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(0)
}

fn query_pnl_today_utc(conn: &Connection, profile_start_ms: i64) -> f64 {
    let start_ms = start_of_current_utc_day_ms().max(profile_start_ms);
    conn.query_row(
        "SELECT COALESCE(SUM(COALESCE(pnl_usd, 0.0)), 0.0) \
         FROM trade_events \
         WHERE event_type='EXIT' AND COALESCE(ts_ms, 0) >= ?1",
        [start_ms],
        |row| row.get(0),
    )
    .unwrap_or(0.0)
}

fn query_ack_latency_summary(conn: &Connection, profile_start_ms: i64) -> (u64, Option<f64>, u64) {
    let cutoff_ms = ack_latency_window_start_ms().max(profile_start_ms);
    let ack_latency_expr = r#"
CASE
    WHEN COALESCE(json_valid(reason), 0) = 0 THEN NULL
    ELSE COALESCE(
        CAST(json_extract(reason, '$.api_post_order_ms') AS REAL),
        CAST(json_extract(reason, '$.order_ack_latency_ms') AS REAL),
        CASE
            WHEN json_extract(reason, '$.acked_at_ms') IS NOT NULL
              AND json_extract(reason, '$.submitted_at_ms') IS NOT NULL
              AND CAST(json_extract(reason, '$.acked_at_ms') AS INTEGER) >= CAST(json_extract(reason, '$.submitted_at_ms') AS INTEGER)
            THEN CAST(json_extract(reason, '$.acked_at_ms') AS REAL) - CAST(json_extract(reason, '$.submitted_at_ms') AS REAL)
            ELSE NULL
        END
    )
END
"#;
    conn.query_row(
        format!(
            "WITH ack_events AS ( \
                SELECT ts_ms, {ack_latency_expr} AS ack_latency_ms \
                FROM trade_events \
                WHERE event_type='ENTRY_ACK' AND COALESCE(ts_ms, 0) >= ?1 \
            ) \
            SELECT \
                COALESCE(SUM(CASE WHEN ack_latency_ms IS NOT NULL AND ack_latency_ms >= 0.0 THEN 1 ELSE 0 END), 0), \
                AVG(CASE WHEN ack_latency_ms IS NOT NULL AND ack_latency_ms >= 0.0 THEN ack_latency_ms END), \
                COALESCE(SUM(CASE WHEN ts_ms >= ?1 AND ack_latency_ms IS NOT NULL AND ack_latency_ms > 500.0 THEN 1 ELSE 0 END), 0) \
            FROM ack_events"
        )
        .as_str(),
        [cutoff_ms],
        |row| {
            Ok((
                row.get::<_, i64>(0)?.max(0) as u64,
                row.get::<_, Option<f64>>(1)?,
                row.get::<_, i64>(2)?.max(0) as u64,
            ))
        },
    )
    .unwrap_or((0, None, 0))
}

fn profile_created_date(profile: &Profile) -> Option<NaiveDate> {
    DateTime::parse_from_rfc3339(profile.created_at.as_str())
        .ok()
        .map(|dt| dt.date_naive())
}

fn earliest_local_activity_date(conn: &Connection) -> Option<NaiveDate> {
    let earliest_ts_ms = conn
        .query_row(
            "SELECT MIN(ts_ms) FROM trade_events WHERE ts_ms IS NOT NULL",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten()?;
    Utc.timestamp_millis_opt(earliest_ts_ms)
        .single()
        .map(|dt| dt.date_naive())
}

fn liquidity_rewards_start_date(profile: &Profile, conn: Option<&Connection>) -> NaiveDate {
    let today = Utc::now().date_naive();
    let profile_date = profile_created_date(profile).unwrap_or(today);
    let db_date = conn
        .and_then(earliest_local_activity_date)
        .unwrap_or(profile_date);
    profile_date.min(db_date).min(today)
}

fn build_liquidity_rewards_query(
    profile: &Profile,
    auth: &AppAuth,
    db_path: &Path,
) -> Result<LiquidityRewardsQuery, String> {
    let secrets = decrypt_profile_secrets(profile, auth)?;
    let private_key = secrets
        .get("POLY_PRIVATE_KEY")
        .cloned()
        .ok_or_else(|| "missing POLY_PRIVATE_KEY in profile secrets".to_string())?;

    let start_date = Connection::open(db_path)
        .ok()
        .as_ref()
        .map(|conn| liquidity_rewards_start_date(profile, Some(conn)))
        .unwrap_or_else(|| liquidity_rewards_start_date(profile, None));

    Ok(LiquidityRewardsQuery {
        private_key,
        maker_address: profile.primary_wallet_address(),
        signature_type: profile.signature_type,
        start_date,
    })
}

fn build_open_orders_query(
    profile: &Profile,
    auth: &AppAuth,
) -> Result<portfolio_api::AuthenticatedClobQuery, String> {
    let secrets = decrypt_profile_secrets(profile, auth)?;
    let private_key = secrets
        .get("POLY_PRIVATE_KEY")
        .cloned()
        .ok_or_else(|| "missing POLY_PRIVATE_KEY in profile secrets".to_string())?;

    Ok(portfolio_api::AuthenticatedClobQuery {
        private_key,
        maker_address: profile.primary_wallet_address(),
        signature_type: profile.signature_type,
    })
}

async fn load_liquidity_rewards_overview(
    query: &LiquidityRewardsQuery,
    cache: &Mutex<Option<LiquidityRewardsCacheEntry>>,
) -> Result<(Option<f64>, Option<f64>, Option<String>), String> {
    let cached = cache.lock().map_err(|e| e.to_string())?.clone();
    let summary: liquidity_rewards::LiquidityRewardsSummary =
        liquidity_rewards::fetch_summary(query, cached)
            .await
            .map_err(|e| e.to_string())?;
    *cache.lock().map_err(|e| e.to_string())? = Some(summary.cache.clone());
    Ok((
        Some(summary.today_rewards_usd),
        Some(summary.lifetime_rewards_usd),
        Some(summary.as_of_utc),
    ))
}

fn clean_liquidity_rewards_error(error: String) -> String {
    if error.contains("401") || error.to_ascii_lowercase().contains("unauthorized") {
        "Rewards unavailable for this wallet.".to_string()
    } else {
        error
    }
}

fn count_unknown_ack_warnings(data_dir: &Path, max_lines: usize) -> usize {
    let path = data_dir.join(FULL_DEBUG_LOG_NAME);
    let batch = match log_stream::read_log_tail(&path, None, max_lines.max(1)) {
        Ok(batch) => batch,
        Err(_) => return 0,
    };
    batch
        .lines
        .into_iter()
        .filter(|line| {
            let lower = line.content.to_ascii_lowercase();
            lower.contains("order id unavailable")
                || lower.contains("status: unknown")
                || lower.contains("returned empty orderid")
        })
        .count()
}

fn count_unknown_ack_warnings_since(
    data_dir: &Path,
    max_lines: usize,
    _profile_start_ms: i64,
) -> usize {
    count_unknown_ack_warnings(data_dir, max_lines)
}

fn format_api_timestamp(timestamp_secs: Option<i64>) -> Option<String> {
    timestamp_secs
        .and_then(|secs| Utc.timestamp_opt(secs, 0).single())
        .map(|dt| dt.to_rfc3339())
}

fn title_case_words(raw: &str) -> String {
    raw.split('_')
        .flat_map(str::split_whitespace)
        .filter(|segment| !segment.trim().is_empty())
        .map(|segment| {
            let lower = segment.to_ascii_lowercase();
            let mut chars = lower.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn activity_action_label(row: &portfolio_api::ActivityRow) -> String {
    let activity_type = row
        .activity_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("activity");
    match (
        activity_type.to_ascii_uppercase().as_str(),
        row.side
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_ascii_uppercase()
            .as_str(),
    ) {
        ("TRADE", "BUY") => "Bought".to_string(),
        ("TRADE", "SELL") => "Sold".to_string(),
        ("REDEEM", _) => "Redeemed".to_string(),
        ("MERGE", _) => "Merged".to_string(),
        ("SPLIT", _) => "Split".to_string(),
        (kind, _) => title_case_words(kind),
    }
}

fn activity_cashflow_usd(row: &portfolio_api::ActivityRow) -> Option<f64> {
    let amount = row.usdc_size?;
    match (
        row.activity_type
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_ascii_uppercase()
            .as_str(),
        row.side
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_ascii_uppercase()
            .as_str(),
    ) {
        ("TRADE", "BUY") => Some(-amount),
        ("TRADE", "SELL") => Some(amount),
        ("REDEEM", _) => Some(amount),
        _ => Some(amount),
    }
}

fn activity_trade_price(row: &portfolio_api::ActivityRow) -> Option<f64> {
    if let Some(price) = row.price {
        if price.is_finite() && price > 0.0 {
            return Some(price);
        }
    }
    let notional = row.usdc_size?;
    let size = row.size?;
    if notional.is_finite() && size.is_finite() && notional > 0.0 && size > 0.0 {
        let price = notional / size;
        if price.is_finite() && price > 0.0 {
            return Some(price);
        }
    }
    None
}

fn timeframe_seconds(timeframe: &str) -> Option<i64> {
    match timeframe {
        "5m" => Some(5 * 60),
        "15m" => Some(15 * 60),
        "1h" => Some(60 * 60),
        "4h" => Some(4 * 60 * 60),
        "1d" => Some(24 * 60 * 60),
        _ => None,
    }
}

fn format_market_title(
    symbol: Option<&str>,
    timeframe: Option<&str>,
    period_timestamp: Option<i64>,
) -> String {
    let symbol = symbol.unwrap_or("Market");
    match (timeframe, period_timestamp) {
        (Some(timeframe), Some(period_timestamp)) => {
            if let Some(open) = Utc.timestamp_opt(period_timestamp, 0).single() {
                if timeframe == "1d" {
                    return format!("{symbol} Up or Down | {}", open.format("%b %-d UTC"));
                }
                if let Some(duration_sec) = timeframe_seconds(timeframe) {
                    if let Some(close) = Utc
                        .timestamp_opt(period_timestamp + duration_sec, 0)
                        .single()
                    {
                        return format!(
                            "{symbol} Up or Down | {} to {} UTC",
                            open.format("%b %-d, %H:%M"),
                            close.format("%H:%M")
                        );
                    }
                }
            }
            format!("{symbol} Up or Down | {timeframe}")
        }
        (Some(timeframe), None) => format!("{symbol} Up or Down | {timeframe}"),
        _ => format!("{symbol} market"),
    }
}

fn parse_trade_outcome_label(token_type: Option<&str>) -> Option<String> {
    token_type
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.split_whitespace().last())
        .map(str::to_string)
}

fn format_trade_outcome(outcome: Option<&str>, price: f64) -> Option<String> {
    let outcome = outcome
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    if price.is_finite() && price > 0.0 {
        Some(format!("{outcome} {}c", (price * 100.0).round() as i64))
    } else {
        Some(outcome)
    }
}

fn trade_action_label(side: &str) -> &'static str {
    if side.eq_ignore_ascii_case("sell") {
        "Sold"
    } else {
        "Bought"
    }
}

fn signed_trade_cashflow(side: &str, notional_usd: f64, price: f64, quantity: f64) -> f64 {
    let base = if notional_usd.is_finite() && notional_usd > 0.0 {
        notional_usd
    } else {
        price * quantity
    };
    if side.eq_ignore_ascii_case("sell") {
        base
    } else {
        -base
    }
}

fn latest_fill_id(conn: &Connection) -> u64 {
    conn.query_row("SELECT COALESCE(MAX(id), 0) FROM fills_v2", [], |row| {
        row.get::<_, i64>(0)
    })
    .unwrap_or(0)
    .max(0) as u64
}

fn load_trade_activity_rows(
    conn: &Connection,
    cursor: Option<u64>,
    limit: usize,
    reset: bool,
) -> Vec<TradeActivityRecord> {
    let limit = limit.max(1) as i64;
    if cursor.is_some() && !reset {
        let sql = r#"
SELECT
    f.id,
    COALESCE(f.ts_ms, f.created_at_ms, 0),
    COALESCE(f.condition_id, ''),
    COALESCE(f.token_id, ''),
    LOWER(TRIM(COALESCE(f.side, 'buy'))),
    COALESCE(f.price, 0.0),
    COALESCE(f.units, 0.0),
    COALESCE(f.notional_usd, COALESCE(f.price, 0.0) * COALESCE(f.units, 0.0)),
    NULLIF(TRIM(f.timeframe), ''),
    NULLIF(f.period_timestamp, 0),
    NULLIF(TRIM(f.token_type), ''),
    NULLIF(TRIM(te.asset_symbol), '')
FROM fills_v2 f
LEFT JOIN trade_events te ON te.event_key = f.event_key
WHERE f.id > ?1
ORDER BY f.id ASC
LIMIT ?2
"#;
        let mut stmt = match conn.prepare(sql) {
            Ok(stmt) => stmt,
            Err(_) => return vec![],
        };
        return stmt
            .query_map([cursor.unwrap_or(0) as i64, limit], |row| {
                Ok(TradeActivityRecord {
                    id: row.get::<_, i64>(0)?.max(0) as u64,
                    timestamp: iso_from_ms(row.get(1)?),
                    condition_id: row.get(2)?,
                    token_id: row.get(3)?,
                    side: row.get(4)?,
                    price: row.get(5)?,
                    quantity: row.get(6)?,
                    notional_usd: row.get(7)?,
                    timeframe: row.get(8)?,
                    period_timestamp: row.get(9)?,
                    token_type: row.get(10)?,
                    asset_symbol: row.get(11)?,
                })
            })
            .map(|iter| iter.filter_map(Result::ok).collect())
            .unwrap_or_default();
    }

    let sql = r#"
SELECT
    f.id,
    COALESCE(f.ts_ms, f.created_at_ms, 0),
    COALESCE(f.condition_id, ''),
    COALESCE(f.token_id, ''),
    LOWER(TRIM(COALESCE(f.side, 'buy'))),
    COALESCE(f.price, 0.0),
    COALESCE(f.units, 0.0),
    COALESCE(f.notional_usd, COALESCE(f.price, 0.0) * COALESCE(f.units, 0.0)),
    NULLIF(TRIM(f.timeframe), ''),
    NULLIF(f.period_timestamp, 0),
    NULLIF(TRIM(f.token_type), ''),
    NULLIF(TRIM(te.asset_symbol), '')
FROM fills_v2 f
LEFT JOIN trade_events te ON te.event_key = f.event_key
ORDER BY f.id DESC
LIMIT ?1
"#;
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(_) => return vec![],
    };
    stmt.query_map([limit], |row| {
        Ok(TradeActivityRecord {
            id: row.get::<_, i64>(0)?.max(0) as u64,
            timestamp: iso_from_ms(row.get(1)?),
            condition_id: row.get(2)?,
            token_id: row.get(3)?,
            side: row.get(4)?,
            price: row.get(5)?,
            quantity: row.get(6)?,
            notional_usd: row.get(7)?,
            timeframe: row.get(8)?,
            period_timestamp: row.get(9)?,
            token_type: row.get(10)?,
            asset_symbol: row.get(11)?,
        })
    })
    .map(|iter| iter.filter_map(Result::ok).collect())
    .unwrap_or_default()
}

fn first_non_empty(values: &[Option<String>]) -> Option<String> {
    values.iter().find_map(|value| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(str::to_string)
    })
}

fn fetch_clob_market_metadata(
    client: &reqwest::blocking::Client,
    condition_id: &str,
) -> Option<(Option<String>, HashMap<String, String>)> {
    let url = format!("https://clob.polymarket.com/markets/{condition_id}");
    let response = client.get(&url).send().ok()?;
    if !response.status().is_success() {
        return None;
    }

    let payload: ClobMarketResponse = response.json().ok()?;
    let title = payload
        .question
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            payload
                .market_slug
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        });

    let outcomes_by_token = payload
        .tokens
        .into_iter()
        .filter_map(|token| {
            let outcome = token.outcome?;
            let outcome = outcome.trim();
            if token.token_id.trim().is_empty() || outcome.is_empty() {
                None
            } else {
                Some((token.token_id, outcome.to_string()))
            }
        })
        .collect::<HashMap<_, _>>();

    Some((title, outcomes_by_token))
}

fn fetch_gamma_market_metadata(
    client: &reqwest::blocking::Client,
    condition_id: &str,
) -> Option<GammaMarketResponse> {
    let url = format!("https://gamma-api.polymarket.com/markets?condition_ids={condition_id}");
    let response = client.get(&url).send().ok()?;
    if !response.status().is_success() {
        return None;
    }

    let payload: Vec<GammaMarketResponse> = response.json().ok()?;
    payload.into_iter().next()
}

fn fetch_gamma_market_metadata_batch(
    client: &reqwest::blocking::Client,
    condition_ids: &[String],
) -> HashMap<String, GammaMarketResponse> {
    let Some(url) = gamma_market_metadata_batch_url(condition_ids) else {
        return HashMap::new();
    };
    let response = match client.get(&url).send() {
        Ok(response) if response.status().is_success() => response,
        _ => return HashMap::new(),
    };
    let payload: Vec<GammaMarketResponse> = match response.json() {
        Ok(payload) => payload,
        Err(_) => return HashMap::new(),
    };

    payload
        .into_iter()
        .filter_map(|market| {
            let condition_id = market
                .condition_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_ascii_lowercase())?;
            Some((condition_id, market))
        })
        .collect()
}

fn gamma_market_metadata_batch_url(condition_ids: &[String]) -> Option<String> {
    let condition_ids = condition_ids
        .iter()
        .map(|condition_id| condition_id.trim().to_ascii_lowercase())
        .filter(|condition_id| !condition_id.is_empty())
        .collect::<BTreeSet<_>>();
    if condition_ids.is_empty() {
        return None;
    }

    let mut url = reqwest::Url::parse("https://gamma-api.polymarket.com/markets").ok()?;
    {
        let mut query = url.query_pairs_mut();
        for condition_id in &condition_ids {
            query.append_pair("condition_ids", condition_id);
        }
        query.append_pair("limit", &condition_ids.len().to_string());
    }
    Some(url.to_string())
}

fn gamma_market_thumbnail(payload: &GammaMarketResponse) -> Option<String> {
    let event = payload.events.first();
    first_non_empty(&[
        payload
            .image_optimized
            .as_ref()
            .and_then(|entry| entry.image_url_optimized.clone()),
        event
            .and_then(|entry| entry.image_optimized.as_ref())
            .and_then(|entry| entry.image_url_optimized.clone()),
        payload.image.clone(),
        event.and_then(|entry| entry.image.clone()),
        payload
            .icon_optimized
            .as_ref()
            .and_then(|entry| entry.image_url_optimized.clone()),
        event
            .and_then(|entry| entry.icon_optimized.as_ref())
            .and_then(|entry| entry.image_url_optimized.clone()),
        payload.icon.clone(),
        event.and_then(|entry| entry.icon.clone()),
    ])
}

fn gamma_market_event_slug(payload: &GammaMarketResponse) -> Option<String> {
    first_non_empty(&[
        payload.event_slug.clone(),
        payload.events.first().and_then(|entry| entry.slug.clone()),
    ])
}

fn local_updown_market_slug(
    symbol: Option<&str>,
    timeframe: Option<&str>,
    period_timestamp: Option<i64>,
) -> Option<String> {
    let symbol = symbol?.trim().to_ascii_lowercase();
    let timeframe = timeframe?.trim().to_ascii_lowercase();
    let period_timestamp = period_timestamp?;
    if symbol.is_empty() || timeframe.is_empty() || period_timestamp <= 0 {
        return None;
    }
    Some(format!("{symbol}-updown-{timeframe}-{period_timestamp}"))
}

fn fetch_market_metadata(condition_id: &str) -> Option<MarketMetadata> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;

    let clob = fetch_clob_market_metadata(&client, condition_id);
    let gamma = fetch_gamma_market_metadata(&client, condition_id);

    let title = clob
        .as_ref()
        .and_then(|(value, _)| value.clone())
        .or_else(|| gamma.as_ref().and_then(|payload| payload.question.clone()))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;

    let outcomes_by_token = clob
        .as_ref()
        .map(|(_, outcomes_by_token)| outcomes_by_token.clone())
        .unwrap_or_default();

    Some(MarketMetadata {
        title,
        outcomes_by_token,
        thumbnail_url: gamma.as_ref().and_then(gamma_market_thumbnail),
        market_slug: clob
            .as_ref()
            .and_then(|(slug, _)| slug.clone())
            .or_else(|| gamma.as_ref().and_then(|payload| payload.slug.clone())),
        event_slug: gamma.as_ref().and_then(gamma_market_event_slug),
    })
}

fn market_metadata_from_gamma(payload: &GammaMarketResponse) -> Option<MarketMetadata> {
    let title = payload
        .question
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)?;
    Some(MarketMetadata {
        title,
        outcomes_by_token: HashMap::new(),
        thumbnail_url: gamma_market_thumbnail(payload),
        market_slug: payload.slug.clone(),
        event_slug: gamma_market_event_slug(payload),
    })
}

fn resolve_gamma_market_metadata_batch(
    condition_ids: &[String],
    cache: &MarketMetadataState,
) -> HashMap<String, MarketMetadata> {
    let normalized_ids = condition_ids
        .iter()
        .map(|condition_id| condition_id.trim().to_ascii_lowercase())
        .filter(|condition_id| !condition_id.is_empty())
        .collect::<Vec<_>>();
    if normalized_ids.is_empty() {
        return HashMap::new();
    }

    let mut resolved = HashMap::new();
    let missing = if let Ok(guard) = cache.0.lock() {
        normalized_ids
            .iter()
            .filter_map(|condition_id| {
                if let Some(entry) = guard.get(condition_id).cloned() {
                    resolved.insert(condition_id.clone(), entry);
                    None
                } else {
                    Some(condition_id.clone())
                }
            })
            .collect::<Vec<_>>()
    } else {
        normalized_ids.clone()
    };

    if missing.is_empty() {
        return resolved;
    }

    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(client) => client,
        Err(_) => return resolved,
    };
    let fetched = fetch_gamma_market_metadata_batch(&client, &missing);
    if fetched.is_empty() {
        return resolved;
    }

    let mut new_entries = HashMap::new();
    for (condition_id, payload) in fetched {
        if let Some(metadata) = market_metadata_from_gamma(&payload) {
            resolved.insert(condition_id.clone(), metadata.clone());
            new_entries.insert(condition_id, metadata);
        }
    }
    if !new_entries.is_empty() {
        if let Ok(mut guard) = cache.0.lock() {
            guard.extend(new_entries);
        }
    }

    resolved
}

fn resolve_market_metadata(
    condition_id: &str,
    cache: &MarketMetadataState,
) -> Option<MarketMetadata> {
    let cache_key = condition_id.trim().to_ascii_lowercase();
    if cache_key.is_empty() {
        return None;
    }

    if let Ok(guard) = cache.0.lock() {
        if let Some(entry) = guard.get(&cache_key).cloned() {
            return Some(entry);
        }
    }

    let fetched = fetch_market_metadata(condition_id)?;
    if let Ok(mut guard) = cache.0.lock() {
        guard.insert(cache_key, fetched.clone());
    }
    Some(fetched)
}

fn build_home_activity_batch(
    data_dir: &Path,
    cache: &MarketMetadataState,
    cursor: Option<u64>,
    limit: usize,
) -> serde_json::Value {
    let db_path = resolve_tracking_db_path(data_dir);
    let conn = match Connection::open(&db_path) {
        Ok(conn) => conn,
        Err(_) => {
            return serde_json::json!({
                "next_cursor": cursor.unwrap_or(0),
                "reset": false,
                "items": [],
            });
        }
    };

    let latest_id = latest_fill_id(&conn);
    let reset = cursor.map(|value| latest_id < value).unwrap_or(false);
    let records = load_trade_activity_rows(&conn, cursor, limit, reset);
    let mut next_cursor = if reset { 0 } else { cursor.unwrap_or(0) };
    let mut resolved_markets: HashMap<String, Option<MarketMetadata>> = HashMap::new();
    let mut items = Vec::with_capacity(records.len());

    for record in records {
        let market = if record.condition_id.trim().is_empty() {
            None
        } else if let Some(entry) = resolved_markets.get(&record.condition_id) {
            entry.clone()
        } else {
            let resolved = resolve_market_metadata(&record.condition_id, cache);
            resolved_markets.insert(record.condition_id.clone(), resolved.clone());
            resolved
        };

        let title = market
            .as_ref()
            .map(|entry| entry.title.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                format_market_title(
                    record.asset_symbol.as_deref(),
                    record.timeframe.as_deref(),
                    record.period_timestamp,
                )
            });

        let fallback_outcome = parse_trade_outcome_label(record.token_type.as_deref());
        let outcome_label = market
            .as_ref()
            .and_then(|entry| entry.outcomes_by_token.get(&record.token_id).cloned())
            .or(fallback_outcome)
            .and_then(|outcome| format_trade_outcome(Some(outcome.as_str()), record.price));
        let fallback_slug = local_updown_market_slug(
            record.asset_symbol.as_deref(),
            record.timeframe.as_deref(),
            record.period_timestamp,
        );

        let cashflow_usd = signed_trade_cashflow(
            &record.side,
            record.notional_usd,
            record.price,
            record.quantity,
        );
        let action = trade_action_label(&record.side).to_string();

        items.push(serde_json::json!({
            "id": record.id.to_string(),
            "timestamp": record.timestamp,
            "severity": "info",
            "source": "trade",
            "kind": "trade",
            "is_reward": false,
            "message": title.clone(),
            "action": action,
            "thumbnail_url": market.as_ref().and_then(|entry| entry.thumbnail_url.clone()),
            "market_title": title.clone(),
            "market_slug": market.as_ref().and_then(|entry| entry.market_slug.clone()).or(fallback_slug),
            "event_slug": market.as_ref().and_then(|entry| entry.event_slug.clone()),
            "title": title,
            "outcome": outcome_label,
            "detail": serde_json::Value::Null,
            "quantity": record.quantity,
            "cashflow_usd": cashflow_usd,
            "value_usd": cashflow_usd,
            "condition_id": if record.condition_id.is_empty() { Value::Null } else { serde_json::json!(record.condition_id) },
            "token_id": if record.token_id.is_empty() { Value::Null } else { serde_json::json!(record.token_id) },
            "price": if record.price.is_finite() && record.price > 0.0 { serde_json::json!(record.price) } else { Value::Null },
            "activity_type": "TRADE",
            "side": record.side,
            "transaction_hash": Value::Null,
        }));
        next_cursor = next_cursor.max(record.id);
    }

    serde_json::json!({
        "next_cursor": next_cursor,
        "reset": reset,
        "items": items,
    })
}

async fn build_home_overview_payload(
    _app: AppHandle,
    bot: State<'_, BotState>,
    auth: State<'_, AuthState>,
    profiles: State<'_, ProfileState>,
    wallet_sync: State<'_, WalletSyncState>,
    liquidity_rewards: State<'_, LiquidityRewardsState>,
    overview_cache: State<'_, HomeOverviewCacheState>,
    data_dir: State<'_, AppDataDir>,
    force_refresh: bool,
) -> Result<serde_json::Value, String> {
    let now_ms = Utc::now().timestamp_millis();
    let bot_snapshot = {
        let manager = bot.lock().map_err(|e| e.to_string())?;
        (
            bot_status_label(manager.get_status()),
            manager.simulation_mode(),
            manager.last_activity_at(),
            manager.running_profile_id(),
        )
    };
    let wallet_sync_status = wallet_sync.lock().map_err(|e| e.to_string())?.status();
    let (active_profile_id, maybe_profile, live_profile_name) = {
        let pm = profiles.lock().map_err(|e| e.to_string())?;
        let active_id = pm.get_active_profile_id();
        let active_profile = active_id.as_ref().and_then(|id| pm.get_profile(id));
        let live_name = bot_snapshot
            .3
            .as_ref()
            .and_then(|id| pm.get_profile(id))
            .map(|profile| profile.name);
        (active_id, active_profile, live_name)
    };
    let db_path = resolve_tracking_db_path(&data_dir.0);
    let active_bot_state = active_profile_bot_state(
        bot_snapshot.0.as_str(),
        active_profile_id.as_deref(),
        bot_snapshot.3.as_deref(),
    );
    let global_bot_busy = matches!(bot_snapshot.0.as_str(), "starting" | "running" | "stopping");
    let other_profile_running = global_bot_busy && active_bot_state == "stopped";

    let Some(profile) = maybe_profile else {
        return Ok(serde_json::json!({
            "profile_ready": false,
            "portfolio_value": Value::Null,
            "available_balance": Value::Null,
            "total_equity": Value::Null,
            "pnl_today_utc": 0.0,
            "liquidity_rewards_today": Value::Null,
            "liquidity_rewards_lifetime": Value::Null,
            "liquidity_rewards_as_of_utc": Value::Null,
            "liquidity_rewards_error": Value::Null,
            "bot_state": active_bot_state,
            "global_bot_state": bot_snapshot.0,
            "live_profile_id": bot_snapshot.3,
            "live_profile_name": live_profile_name,
            "other_profile_running": other_profile_running,
            "mode": "dry_run",
            "active_strategy_count": 0,
            "wallet_sync": wallet_sync_status.clone(),
            "wallet_sync_status": wallet_sync_status.state,
            "wallet_sync_last_run_at": wallet_sync_status.last_run_at,
            "wallet_sync_last_run_at_ms": wallet_sync_status.last_run_at_ms,
            "last_heartbeat_at": bot_snapshot.2,
            "last_heartbeat_at_ms": Value::Null,
            "available_balance_error": Value::Null,
            "portfolio_value_error": Value::Null,
            "ack_warning_count_recent": 0,
            "avg_ack_latency_ms": Value::Null,
            "ack_sample_count": 0,
            "warnings": [],
        }));
    };

    let wallet_address = profile.primary_wallet_address();
    let profile_start_ms = profile_stats_start_ms(&profile);
    let db_summary = {
        let cached = overview_cache
            .0
            .lock()
            .map_err(|e| e.to_string())?
            .db_summary
            .clone();
        if let Some(summary) = cached.filter(|summary| {
            !force_refresh
                && summary.profile_id == profile.id
                && summary
                    .wallet_address
                    .eq_ignore_ascii_case(wallet_address.as_str())
                && now_ms.saturating_sub(summary.fetched_at_ms) <= HOME_DB_REFRESH_MS
        }) {
            summary
        } else {
            let (pnl_today_utc, ack_sample_count, avg_ack_latency_ms, recent_ack_warning_count) =
                Connection::open(&db_path)
                    .ok()
                    .map(|conn| {
                        configure_tracking_connection(&conn);
                        let pnl = query_pnl_today_utc(&conn, profile_start_ms);
                        let (ack_count, ack_avg, ack_warnings) =
                            query_ack_latency_summary(&conn, profile_start_ms);
                        (pnl, ack_count, ack_avg, ack_warnings)
                    })
                    .unwrap_or((0.0, 0, None, 0));
            let summary = HomeDbSummary {
                profile_id: profile.id.clone(),
                wallet_address: wallet_address.clone(),
                fetched_at_ms: now_ms,
                pnl_today_utc,
                ack_sample_count,
                avg_ack_latency_ms,
                recent_ack_warning_count,
            };
            overview_cache
                .0
                .lock()
                .map_err(|e| e.to_string())?
                .db_summary = Some(summary.clone());
            summary
        }
    };
    let pnl_today_utc = db_summary.pnl_today_utc;
    let ack_sample_count = db_summary.ack_sample_count;
    let avg_ack_latency_ms = db_summary.avg_ack_latency_ms;
    let recent_ack_warning_count = db_summary.recent_ack_warning_count;
    let recent_unknown_ack_count =
        count_unknown_ack_warnings_since(&data_dir.0, 160, profile_start_ms) as u64;
    let slow_snapshot = {
        let cached = overview_cache
            .0
            .lock()
            .map_err(|e| e.to_string())?
            .slow_snapshot
            .clone();
        if let Some(snapshot) = cached.filter(|snapshot| {
            !force_refresh
                && snapshot.profile_id == profile.id
                && snapshot
                    .wallet_address
                    .eq_ignore_ascii_case(wallet_address.as_str())
                && now_ms.saturating_sub(snapshot.fetched_at_ms)
                    <= if snapshot.has_error() {
                        HOME_REMOTE_ERROR_REFRESH_MS
                    } else {
                        HOME_REMOTE_REFRESH_MS
                    }
        }) {
            snapshot
        } else {
            let rewards_query = {
                let auth = auth.lock().map_err(|e| e.to_string())?;
                build_liquidity_rewards_query(&profile, &auth, &db_path)?
            };
            let rewards_result = tokio::time::timeout(
                Duration::from_secs(20),
                load_liquidity_rewards_overview(&rewards_query, &liquidity_rewards.0),
            )
            .await
            .map_err(|_| "liquidity rewards refresh timed out".to_string());

            let (available_balance_result, portfolio_value_result) = tokio::join!(
                tokio::time::timeout(
                    Duration::from_secs(15),
                    get_wallet_balance_for_address(&wallet_address),
                ),
                tokio::time::timeout(
                    Duration::from_secs(10),
                    portfolio_api::fetch_portfolio_value_with_fallback(&wallet_address),
                )
            );
            let available_balance_result = match available_balance_result {
                Ok(result) => result,
                Err(_) => Err("available balance refresh timed out".to_string()),
            };
            let portfolio_value_result = match portfolio_value_result {
                Ok(result) => result.map(|row| row.0),
                Err(_) => Err("portfolio value refresh timed out".to_string()),
            };
            let (
                liquidity_rewards_today,
                liquidity_rewards_lifetime,
                liquidity_rewards_as_of_utc,
                liquidity_rewards_error,
            ) = match rewards_result {
                Ok(Ok((today, lifetime, as_of_utc))) => (today, lifetime, as_of_utc, None),
                Ok(Err(err)) => (None, None, None, Some(clean_liquidity_rewards_error(err))),
                Err(err) => (None, None, None, Some(clean_liquidity_rewards_error(err))),
            };
            let snapshot = HomeSlowOverviewSnapshot {
                profile_id: profile.id.clone(),
                wallet_address: wallet_address.clone(),
                fetched_at_ms: now_ms,
                available_balance_result,
                portfolio_value_result,
                liquidity_rewards_today,
                liquidity_rewards_lifetime,
                liquidity_rewards_as_of_utc,
                liquidity_rewards_error,
            };
            overview_cache
                .0
                .lock()
                .map_err(|e| e.to_string())?
                .slow_snapshot = Some(snapshot.clone());
            snapshot
        }
    };
    let available_balance_result = slow_snapshot.available_balance_result.clone();
    let portfolio_value_result = slow_snapshot.portfolio_value_result.clone();
    let available_balance = available_balance_result.clone().ok();
    let portfolio_value = portfolio_value_result.clone().ok();
    let total_equity = match (available_balance, portfolio_value) {
        (Some(available), Some(portfolio)) => Some(available + portfolio),
        _ => None,
    };
    let liquidity_rewards_today = slow_snapshot.liquidity_rewards_today;
    let liquidity_rewards_lifetime = slow_snapshot.liquidity_rewards_lifetime;
    let liquidity_rewards_as_of_utc = slow_snapshot.liquidity_rewards_as_of_utc;
    let liquidity_rewards_error = slow_snapshot.liquidity_rewards_error;
    let mode = match bot_snapshot.1 {
        Some(true) => "dry_run",
        Some(false) => "live",
        None if simulation_mode_from_profile(&profile) => "dry_run",
        None => "live",
    };
    let mut warnings = Vec::new();
    if let Err(err) = &available_balance_result {
        warnings.push(format!("Available balance degraded: {err}"));
    }
    if let Err(err) = &portfolio_value_result {
        warnings.push(format!("Portfolio value degraded: {err}"));
    }
    if let Some(err) = &liquidity_rewards_error {
        warnings.push(format!("Liquidity rewards degraded: {err}"));
    }
    if recent_ack_warning_count > 0 {
        warnings.push(format!(
            "Recent order acknowledgements are degraded: {recent_ack_warning_count} acknowledgements exceeded 500 ms in the last 24 hours."
        ));
    }
    if recent_unknown_ack_count > 0 {
        warnings.push(format!(
            "Recent order acknowledgements are degraded: {recent_unknown_ack_count} recent orders were accepted without an order ID."
        ));
    }
    if wallet_sync_status.error.is_some() {
        warnings.push("Wallet sync is degraded.".to_string());
    }

    Ok(serde_json::json!({
        "profile_ready": true,
        "portfolio_value": portfolio_value,
        "available_balance": available_balance,
        "total_equity": total_equity,
        "pnl_today_utc": pnl_today_utc,
        "liquidity_rewards_today": liquidity_rewards_today,
        "liquidity_rewards_lifetime": liquidity_rewards_lifetime,
        "liquidity_rewards_as_of_utc": liquidity_rewards_as_of_utc,
        "liquidity_rewards_error": liquidity_rewards_error,
        "bot_state": active_bot_state,
        "global_bot_state": bot_snapshot.0,
        "live_profile_id": bot_snapshot.3,
        "live_profile_name": live_profile_name,
        "other_profile_running": other_profile_running,
        "mode": mode,
        "active_strategy_count": count_enabled_strategies(&profile),
        "wallet_sync": wallet_sync_status.clone(),
        "wallet_sync_status": wallet_sync_status.state,
        "wallet_sync_last_run_at": wallet_sync_status.last_run_at,
        "wallet_sync_last_run_at_ms": wallet_sync_status.last_run_at_ms,
        "last_heartbeat_at": bot_snapshot.2,
        "last_heartbeat_at_ms": Value::Null,
        "available_balance_error": available_balance_result.err(),
        "portfolio_value_error": portfolio_value_result.err(),
        "ack_warning_count_recent": recent_ack_warning_count,
        "avg_ack_latency_ms": avg_ack_latency_ms,
        "ack_sample_count": ack_sample_count,
        "warnings": warnings,
    }))
}

// ── Auth ─────────────────────────────────────────────────────────────

#[tauri::command]
fn is_auth_initialized(auth: State<'_, AuthState>) -> bool {
    auth.lock().map(|a| a.is_initialized()).unwrap_or(false)
}

#[tauri::command]
fn initialize_password(auth: State<'_, AuthState>, password: String) -> Result<(), String> {
    auth.lock()
        .map_err(|e| e.to_string())?
        .initialize_password(&password)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn verify_password(auth: State<'_, AuthState>, password: String) -> Result<bool, String> {
    auth.lock()
        .map_err(|e| e.to_string())?
        .verify_password(&password)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn lock_session(auth: State<'_, AuthState>) -> Result<(), String> {
    auth.lock().map_err(|e| e.to_string())?.clear_session();
    Ok(())
}

fn wipe_local_app_data_contents(data_dir: &Path) -> Result<(), String> {
    if !data_dir.exists() {
        std::fs::create_dir_all(data_dir).map_err(|e| format!("create data dir: {e}"))?;
        return Ok(());
    }

    let mut failures = Vec::new();
    let entries = std::fs::read_dir(data_dir).map_err(|e| format!("read data dir: {e}"))?;

    for entry in entries {
        let entry = match entry {
            Ok(value) => value,
            Err(err) => {
                failures.push(format!("read entry: {err}"));
                continue;
            }
        };

        let path = entry.path();
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("<unknown>")
            .to_string();

        if file_name == DESKTOP_DEBUG_LOG_NAME {
            continue;
        }

        let result = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else if file_name.starts_with(".env.generated") {
            config_io::cleanup_env_file(&path);
            Ok(())
        } else {
            std::fs::remove_file(&path)
        };

        if let Err(err) = result {
            failures.push(format!("{file_name}: {err}"));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "reset local data failed for {}",
            failures.join(", ")
        ))
    }
}

#[tauri::command]
fn reset_local_app_data(
    data_dir: State<'_, AppDataDir>,
    auth: State<'_, AuthState>,
    bot: State<'_, BotState>,
    wallet_sync: State<'_, WalletSyncState>,
) -> Result<(), String> {
    append_desktop_debug_line(&data_dir.0, "SYSTEM", "reset_local_app_data requested");

    if let Ok(manager) = wallet_sync.lock() {
        let _ = manager.stop();
    }

    if let Ok(manager) = bot.lock() {
        let _ = manager.stop();
    }

    if let Ok(mut state) = auth.lock() {
        state.clear_session();
    }

    config_io::cleanup_generated_env_files(&data_dir.0);
    wipe_local_app_data_contents(&data_dir.0)?;
    std::fs::create_dir_all(&data_dir.0).map_err(|e| format!("recreate data dir: {e}"))?;
    let _ = ensure_debug_log_files(&data_dir.0);
    append_desktop_debug_line(&data_dir.0, "SYSTEM", "reset_local_app_data completed");
    Ok(())
}

// ── Profiles ─────────────────────────────────────────────────────────

#[tauri::command]
fn list_profiles(profiles: State<'_, ProfileState>) -> Vec<Profile> {
    profiles
        .lock()
        .map(|pm| pm.list_profiles())
        .unwrap_or_default()
}

#[tauri::command]
fn create_profile(
    profiles: State<'_, ProfileState>,
    name: String,
    proxy_wallet_address: String,
    deposit_wallet_address: Option<String>,
    signature_type: u8,
) -> Result<Profile, String> {
    let mut default_config = default_desktop_config(
        String::new(),
        proxy_wallet_address.trim().to_string(),
        signature_type,
    );
    default_config.deposit_wallet = deposit_wallet_address
        .unwrap_or_default()
        .trim()
        .to_string();
    let (
        strategy_config,
        sizing_config,
        _,
        eoa_wallet_address,
        proxy_wallet_address,
        deposit_wallet_address,
        signature_type,
    ) = desktop_config_to_profile_payload(&default_config);

    profiles
        .lock()
        .map_err(|e| e.to_string())?
        .create_profile(
            name,
            eoa_wallet_address,
            proxy_wallet_address,
            deposit_wallet_address,
            signature_type,
            strategy_config,
            sizing_config,
        )
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_profile(profiles: State<'_, ProfileState>, id: String) -> Option<Profile> {
    profiles.lock().ok()?.get_profile(&id)
}

#[tauri::command]
fn update_profile(profiles: State<'_, ProfileState>, profile: Profile) -> Result<(), String> {
    profiles
        .lock()
        .map_err(|e| e.to_string())?
        .update_profile(profile)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_profile(profiles: State<'_, ProfileState>, id: String) -> Result<(), String> {
    profiles
        .lock()
        .map_err(|e| e.to_string())?
        .delete_profile(&id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_active_profile_id(profiles: State<'_, ProfileState>) -> Option<String> {
    profiles.lock().ok()?.get_active_profile_id()
}

#[tauri::command]
fn set_active_profile(profiles: State<'_, ProfileState>, id: String) -> Result<(), String> {
    profiles
        .lock()
        .map_err(|e| e.to_string())?
        .set_active_profile(&id)
        .map_err(|e| e.to_string())
}

// ── Bot ──────────────────────────────────────────────────────────────

#[tauri::command]
async fn start_bot(
    app: AppHandle,
    bot: State<'_, BotState>,
    profiles: State<'_, ProfileState>,
    auth: State<'_, AuthState>,
    data_dir: State<'_, AppDataDir>,
    wallet_sync: State<'_, WalletSyncState>,
    simulation: bool,
) -> Result<(), String> {
    geo_access::ensure_geo_start_allowed()?;
    let (profile, env_path, config_path) =
        prepare_active_profile_runtime_paths(&profiles, &auth, &data_dir.0, simulation).await?;
    let wallet_sync_config = wallet_sync_config_for_profile(&profile);
    bot.lock().map_err(|e| e.to_string())?.start(
        &app,
        profile.id.clone(),
        env_path,
        config_path,
        simulation,
    )?;
    if simulation {
        wallet_sync.lock().map_err(|e| e.to_string())?.stop()?;
    } else {
        if let Err(err) = wallet_sync
            .lock()
            .map_err(|e| e.to_string())?
            .start(wallet_sync_config)
        {
            let _ = bot.lock().map_err(|e| e.to_string())?.stop();
            return Err(format!(
                "live bot start aborted because wallet sync failed: {err}"
            ));
        }
    }
    Ok(())
}

#[tauri::command]
fn stop_bot(
    bot: State<'_, BotState>,
    wallet_sync: State<'_, WalletSyncState>,
    data_dir: State<'_, AppDataDir>,
) -> Result<(), String> {
    wallet_sync.lock().map_err(|e| e.to_string())?.stop()?;
    bot.lock().map_err(|e| e.to_string())?.stop()?;
    let tracking_index_dir = data_dir.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        ensure_tracking_db_indexes(&tracking_index_dir, true);
    });
    Ok(())
}

#[tauri::command]
async fn restart_bot(
    app: AppHandle,
    bot: State<'_, BotState>,
    profiles: State<'_, ProfileState>,
    auth: State<'_, AuthState>,
    data_dir: State<'_, AppDataDir>,
    wallet_sync: State<'_, WalletSyncState>,
    simulation: bool,
) -> Result<(), String> {
    geo_access::ensure_geo_start_allowed()?;
    let (profile, env_path, config_path) =
        prepare_active_profile_runtime_paths(&profiles, &auth, &data_dir.0, simulation).await?;
    let wallet_sync_config = wallet_sync_config_for_profile(&profile);
    wallet_sync.lock().map_err(|e| e.to_string())?.stop()?;
    bot.lock().map_err(|e| e.to_string())?.restart(
        &app,
        profile.id.clone(),
        env_path,
        config_path,
        simulation,
    )?;
    if simulation {
        wallet_sync.lock().map_err(|e| e.to_string())?.stop()?;
    } else {
        if let Err(err) = wallet_sync
            .lock()
            .map_err(|e| e.to_string())?
            .start(wallet_sync_config)
        {
            let _ = bot.lock().map_err(|e| e.to_string())?.stop();
            return Err(format!(
                "live bot restart aborted because wallet sync failed: {err}"
            ));
        }
    }
    Ok(())
}

#[tauri::command]
fn get_bot_status(bot: State<'_, BotState>, profiles: State<'_, ProfileState>) -> String {
    let (global_state, running_profile_id) = match bot.lock() {
        Ok(manager) => (
            bot_status_label(manager.get_status()),
            manager.running_profile_id(),
        ),
        Err(_) => ("error:lock failed".to_string(), None),
    };
    let active_profile_id = profiles
        .lock()
        .ok()
        .and_then(|pm| pm.get_active_profile_id());
    active_profile_bot_state(
        global_state.as_str(),
        active_profile_id.as_deref(),
        running_profile_id.as_deref(),
    )
}

#[tauri::command]
fn get_log_lines(
    data_dir: State<'_, AppDataDir>,
    cursor: Option<u64>,
    count: usize,
) -> Result<serde_json::Value, String> {
    let path = data_dir.0.join(FULL_DEBUG_LOG_NAME);
    let batch = log_stream::read_log_tail(&path, cursor, count.max(1))
        .map_err(|e| format!("read log tail: {e}"))?;
    Ok(serde_json::json!({
        "next_cursor": batch.next_cursor,
        "reset": batch.reset,
        "lines": batch.lines.into_iter().map(|line| {
            let level = match line.level {
                log_stream::LogLevel::Info => "INFO",
                log_stream::LogLevel::Warn => "WARN",
                log_stream::LogLevel::Error => "ERROR",
            };
            serde_json::json!({
                "timestamp": line.timestamp,
                "level": level,
                "content": line.content,
            })
        }).collect::<Vec<_>>(),
    }))
}

#[tauri::command]
async fn bot_api_request(
    bot: State<'_, BotState>,
    method: String,
    path: String,
    query: Option<serde_json::Value>,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let request_ctx = {
        let mgr = bot.lock().map_err(|e| e.to_string())?;
        mgr.request_context()?
    };
    bot_manager::send_bot_request(request_ctx, method, path, query, body).await
}

// ── Config ───────────────────────────────────────────────────────────

fn apply_desktop_config_to_profile(
    profile: &mut Profile,
    config: &DesktopConfig,
    auth: &AppAuth,
) -> Result<(), String> {
    let (
        strategy_config,
        sizing_config,
        new_secrets,
        eoa_wallet_address,
        proxy_wallet_address,
        deposit_wallet_address,
        signature_type,
    ) = desktop_config_to_profile_payload(config);

    profile.strategy_config = merge_config_object(&profile.strategy_config, &strategy_config);
    remove_legacy_premarket_ladder_keys(&mut profile.strategy_config);
    profile.sizing_config = merge_config_object(&profile.sizing_config, &sizing_config);
    profile.eoa_wallet_address = eoa_wallet_address;
    profile.proxy_wallet_address = proxy_wallet_address;
    profile.deposit_wallet_address = deposit_wallet_address;
    profile.signature_type = signature_type;
    profile.normalize_wallet_fields();

    let merged_secrets = if profile.encrypted_secrets.trim().is_empty() {
        HashMap::new()
    } else {
        decrypt_profile_secrets(profile, auth)?
    };
    let merged_secrets = merge_desktop_secrets(merged_secrets, new_secrets);
    if merged_secrets.is_empty() {
        profile.encrypted_secrets.clear();
    } else {
        let blob = serde_json::to_vec(&merged_secrets).map_err(|e| e.to_string())?;
        profile.encrypted_secrets = auth
            .with_session_password(|password| crypto_vault::encrypt_data(&blob, password))
            .ok_or("session locked: unlock app before saving")?
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

fn save_profile_and_build_runtime(
    profiles: &ProfileState,
    auth: &AuthState,
    data_dir: &Path,
    profile: &mut Profile,
    config: &DesktopConfig,
) -> Result<(PathBuf, PathBuf), String> {
    let auth = auth.lock().map_err(|e| e.to_string())?;
    apply_desktop_config_to_profile(profile, config, &auth)?;
    let pm = profiles.lock().map_err(|e| e.to_string())?;
    pm.update_profile(profile.clone())
        .map_err(|e| e.to_string())?;
    drop(pm);
    build_runtime_paths_for_profile(profile, &auth, data_dir)
}

fn restart_bot_with_runtime_paths(
    app: &AppHandle,
    bot: &BotState,
    wallet_sync: &WalletSyncState,
    profile: &Profile,
    env_path: PathBuf,
    config_path: PathBuf,
    simulation: bool,
) -> Result<(), String> {
    wallet_sync.lock().map_err(|e| e.to_string())?.stop()?;
    bot.lock().map_err(|e| e.to_string())?.restart(
        app,
        profile.id.clone(),
        env_path,
        config_path,
        simulation,
    )?;
    if simulation {
        wallet_sync.lock().map_err(|e| e.to_string())?.stop()?;
    } else if let Err(err) = wallet_sync
        .lock()
        .map_err(|e| e.to_string())?
        .start(wallet_sync_config_for_profile(profile))
    {
        let _ = bot.lock().map_err(|e| e.to_string())?.stop();
        return Err(format!(
            "live bot restart aborted because wallet sync failed: {err}"
        ));
    }
    Ok(())
}

async fn ensure_generated_credentials(config: &mut DesktopConfig) -> Result<Vec<String>, String> {
    let private_key = config.private_key.trim();
    if private_key.is_empty() {
        return Ok(Vec::new());
    }
    if !matches!(config.sig_type, 0..=3) {
        return Err("wallet mode must be EOA, Proxy, Safe, or Deposit Wallet".to_string());
    }

    let derived_eoa = wallet_address_from_private_key(private_key)?;
    config.eoa_wallet = derived_eoa.clone();
    let bound_wallet = bound_wallet_for_config(config, derived_eoa.as_str())?;
    let wallet_binding = wallet_binding_for_config(config, derived_eoa.as_str())?;
    let current_wallet_binding = config.wallet_binding.trim();
    let wallet_binding_missing = current_wallet_binding.is_empty();
    let wallet_changed = !wallet_binding_missing && current_wallet_binding != wallet_binding;
    let alpha_missing = config.alpha_key.trim().is_empty();
    let relayer_missing = clean_relayer_remote_signer_token(config).is_empty();
    let credentials_missing = alpha_missing || relayer_missing;

    if wallet_binding_missing {
        config.wallet_binding = wallet_binding.clone();
    }

    if wallet_changed {
        config.alpha_key.clear();
        config.relayer_remote_signer_token.clear();
        config.relayer_submit_signer_url.clear();
        config.remote_signer_token.clear();
        config.order_signer_primary_token_internal.clear();
        config.wallet_binding = wallet_binding.clone();
        config.onboarding_status = "wallet_saved".to_string();
    }

    let mut fixed_keys = Vec::new();
    if credentials_missing || wallet_changed {
        geo_access::ensure_geo_start_allowed()?;
        let onboarding = if !relayer_missing && !wallet_changed {
            let alpha_key = onboard::run_alpha_onboarding_for_wallet(bound_wallet.as_str()).await?;
            onboard::OnboardResult {
                alpha_key: Some(alpha_key),
                wallet_binding: Some(wallet_binding.clone()),
                approval_status: Some(if wallet_mode_needs_approval_status(config.sig_type) {
                    "not_checked".to_string()
                } else {
                    "not_required".to_string()
                }),
                ..Default::default()
            }
        } else {
            onboard::run_onboarding_with_existing_alpha(
                config.private_key.as_str(),
                config.sig_type,
                config.proxy_wallet.as_str(),
                config.deposit_wallet.as_str(),
                if alpha_missing || wallet_changed {
                    None
                } else {
                    Some(config.alpha_key.as_str())
                },
            )
            .await?
        };

        if let Some(value) = onboarding
            .eoa_wallet
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            config.eoa_wallet = value.to_string();
        }
        if let Some(value) = onboarding
            .deposit_wallet_address
            .as_deref()
            .or(onboarding.deposit_wallet.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            config.deposit_wallet = value.to_string();
        }
        if let Some(value) = onboarding
            .alpha_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            config.alpha_key = value.to_string();
            fixed_keys.push("alpha_key".to_string());
        }
        let relayer_token = onboarding
            .relayer_remote_signer_token
            .as_deref()
            .or(onboarding.remote_signer_token.as_deref())
            .or(onboarding.signer_token.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if let Some(value) = relayer_token {
            config.relayer_remote_signer_token = value.to_string();
            fixed_keys.push("relayer_remote_signer_token".to_string());
        }
        if let Some(value) = onboarding
            .relayer_submit_signer_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            config.relayer_submit_signer_url = value.to_string();
        }
        if let Some(value) = onboarding
            .wallet_binding
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            config.wallet_binding = value.to_string();
        } else {
            config.wallet_binding = wallet_binding;
        }
        if let Some(value) = onboarding
            .approval_status
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            config.approval_status = value.to_string();
        }
    } else {
        config.wallet_binding = wallet_binding;
    }

    if config.approval_status.trim().is_empty() {
        config.approval_status = if wallet_mode_needs_approval_status(config.sig_type) {
            "not_checked".to_string()
        } else {
            "not_required".to_string()
        };
    }
    if config.sig_type == 3 {
        if let Some(status) =
            reconcile_desktop_magic_deposit_wallet(config.deposit_wallet.as_str()).await?
        {
            config.approval_status = status;
        }
    }
    config.onboarding_status = if clean_onboarding_ready(config) {
        "credentials_ready".to_string()
    } else {
        "wallet_saved".to_string()
    };

    Ok(fixed_keys)
}

async fn prepare_active_profile_runtime_paths(
    profiles: &ProfileState,
    auth: &AuthState,
    data_dir: &Path,
    simulation: bool,
) -> Result<(Profile, PathBuf, PathBuf), String> {
    let mut profile = {
        let pm = profiles.lock().map_err(|e| e.to_string())?;
        persist_active_profile_simulation_mode(&pm, simulation)?
    };
    let mut config = {
        let auth_guard = auth.lock().map_err(|e| e.to_string())?;
        let value = profile_to_desktop_config(&profile, &auth_guard)?;
        serde_json::from_value::<DesktopConfig>(value).map_err(|e| e.to_string())?
    };
    let fixed_keys = ensure_generated_credentials(&mut config).await?;
    let (env_path, config_path) =
        save_profile_and_build_runtime(profiles, auth, data_dir, &mut profile, &config)?;
    if !fixed_keys.is_empty() {
        append_desktop_debug_line(
            data_dir,
            "ONBOARD",
            format!(
                "start prepared profile={} generated={}",
                profile.id,
                fixed_keys.join(",")
            )
            .as_str(),
        );
    }
    Ok((profile, env_path, config_path))
}

#[tauri::command]
async fn save_config(
    auth: State<'_, AuthState>,
    profiles: State<'_, ProfileState>,
    data_dir: State<'_, AppDataDir>,
    profile_id: String,
    mut config: DesktopConfig,
    generate_credentials: Option<bool>,
) -> Result<(), String> {
    let mut profile = {
        let pm = profiles.lock().map_err(|e| e.to_string())?;
        pm.get_profile(&profile_id).ok_or("profile not found")?
    };
    if generate_credentials.unwrap_or(true) {
        ensure_generated_credentials(&mut config).await?;
    }
    save_profile_and_build_runtime(&profiles, &auth, &data_dir.0, &mut profile, &config)?;
    Ok(())
}

#[tauri::command]
fn get_saved_config(
    auth: State<'_, AuthState>,
    profiles: State<'_, ProfileState>,
    profile_id: String,
) -> Result<serde_json::Value, String> {
    let pm = profiles.lock().map_err(|e| e.to_string())?;
    let auth = auth.lock().map_err(|e| e.to_string())?;
    let profile = pm.get_profile(&profile_id).ok_or("profile not found")?;
    profile_to_desktop_config(&profile, &auth)
}

#[tauri::command]
async fn run_setup_doctor(
    app: AppHandle,
    bot: State<'_, BotState>,
    profiles: State<'_, ProfileState>,
    auth: State<'_, AuthState>,
    data_dir: State<'_, AppDataDir>,
    wallet_sync: State<'_, WalletSyncState>,
) -> Result<SetupDoctorResult, String> {
    let mut profile = {
        let pm = profiles.lock().map_err(|e| e.to_string())?;
        match active_profile(&pm) {
            Ok(profile) => profile,
            Err(_) => {
                let items = vec![doctor_item(
                    "active_profile",
                    "Active Profile",
                    "missing_user",
                    "Create or activate a profile in Settings before running Setup Doctor.",
                    None,
                )];
                return Ok(doctor_result(
                    "needs_you",
                    items,
                    Some(doctor_popup(
                        "No Active Profile",
                        "Create or activate a profile in Settings, then run Setup Doctor again.",
                    )),
                    false,
                    false,
                ));
            }
        }
    };
    let mut config: DesktopConfig = {
        let auth = auth.lock().map_err(|e| e.to_string())?;
        let value = profile_to_desktop_config(&profile, &auth)?;
        serde_json::from_value(value).map_err(|e| format!("parse desktop config: {e}"))?
    };
    let mut config_changed = false;

    let (bot_was_running, bot_simulation) = {
        let manager = bot.lock().map_err(|e| e.to_string())?;
        let running_profile_id = manager.running_profile_id();
        let active_profile_running =
            manager.is_running() && running_profile_id.as_deref() == Some(profile.id.as_str());
        (
            active_profile_running,
            manager
                .simulation_mode()
                .unwrap_or_else(|| simulation_mode_from_profile(&profile)),
        )
    };

    append_desktop_debug_line(
        &data_dir.0,
        "DOCTOR",
        format!(
            "setup doctor started profile={} bot_running={}",
            profile.id, bot_was_running
        )
        .as_str(),
    );

    let initial_audit = audit_setup_doctor(&config);
    if !initial_audit.blocking_user_labels.is_empty() {
        let popup = doctor_needs_you_popup(&initial_audit);
        return Ok(doctor_result(
            "needs_you",
            initial_audit.items.clone(),
            Some(popup),
            bot_was_running,
            false,
        ));
    }

    let mut fixed_keys: Vec<String> = Vec::new();
    if let Ok(derived_eoa) = wallet_address_from_private_key(config.private_key.as_str()) {
        let current = config.eoa_wallet.trim();
        if current.is_empty() || !current.eq_ignore_ascii_case(derived_eoa.as_str()) {
            config.eoa_wallet = derived_eoa;
            fixed_keys.push("eoa_wallet".to_string());
        }
    }

    let needs_remote_regeneration = initial_audit
        .missing_generated_keys
        .iter()
        .any(|key| key != "eoa_wallet");
    if needs_remote_regeneration {
        let alpha_missing = initial_audit
            .missing_generated_keys
            .iter()
            .any(|key| key == "alpha_key");
        let relayer_missing = initial_audit
            .missing_generated_keys
            .iter()
            .any(|key| key == "relayer_remote_signer_token");
        let geo_status = geo_access::current_geo_access_status();
        if geo_status.status == "blocked" {
            return Ok(doctor_result(
                "needs_you",
                initial_audit.items,
                Some(doctor_popup("Location Blocked", geo_status.reason)),
                bot_was_running,
                false,
            ));
        }
        if geo_status.status != "allowed" {
            return Ok(doctor_result(
                "needs_you",
                initial_audit.items,
                Some(doctor_popup(
                    "Location Verification Required",
                    "Doctor could not regenerate remote credentials because location verification is unavailable right now. Open Settings -> Setup and run onboarding after confirming access.",
                )),
                bot_was_running,
                false,
            ));
        }

        let onboarding = match if !relayer_missing {
            let bound_wallet = bound_wallet_for_config(&config, config.eoa_wallet.as_str())?;
            let wallet_binding = wallet_binding_for_config(&config, config.eoa_wallet.as_str())?;
            let alpha_key = onboard::run_alpha_onboarding_for_wallet(bound_wallet.as_str()).await?;
            Ok(onboard::OnboardResult {
                alpha_key: Some(alpha_key),
                wallet_binding: Some(wallet_binding),
                approval_status: Some(if wallet_mode_needs_approval_status(config.sig_type) {
                    "not_checked".to_string()
                } else {
                    "not_required".to_string()
                }),
                ..Default::default()
            })
        } else {
            onboard::run_onboarding_with_existing_alpha(
                config.private_key.as_str(),
                config.sig_type,
                config.proxy_wallet.as_str(),
                config.deposit_wallet.as_str(),
                if alpha_missing {
                    None
                } else {
                    Some(config.alpha_key.as_str())
                },
            )
            .await
        } {
            Ok(result) => result,
            Err(err) => {
                return Ok(doctor_result(
                    "failed",
                    initial_audit.items,
                    Some(doctor_popup(
                        "Could Not Regenerate Remote Credentials",
                        format!("Setup Doctor could not regenerate remote credentials: {err}"),
                    )),
                    bot_was_running,
                    false,
                ))
            }
        };

        let onboard_remote_signer_token = onboarding
            .relayer_remote_signer_token
            .as_ref()
            .or(onboarding.remote_signer_token.as_ref())
            .or(onboarding.signer_token.as_ref())
            .map(|value| value.trim())
            .filter(|value| !value.is_empty());

        let remote_updates = [
            ("alpha_key", onboarding.alpha_key.as_deref()),
            ("relayer_remote_signer_token", onboard_remote_signer_token),
            (
                "relayer_submit_signer_url",
                onboarding.relayer_submit_signer_url.as_deref(),
            ),
            ("wallet_binding", onboarding.wallet_binding.as_deref()),
            (
                "deposit_wallet",
                onboarding
                    .deposit_wallet_address
                    .as_deref()
                    .or(onboarding.deposit_wallet.as_deref()),
            ),
            ("approval_status", onboarding.approval_status.as_deref()),
            ("admin_api_token", onboarding.admin_api_token.as_deref()),
        ];
        for (key, value) in remote_updates {
            let value = value
                .map(|entry| entry.trim())
                .filter(|entry| !entry.is_empty());
            match key {
                "alpha_key" => {
                    if let Some(value) = value {
                        if config.alpha_key.trim() != value {
                            config.alpha_key = value.to_string();
                            config_changed = true;
                            fixed_keys.push(key.to_string());
                        }
                    }
                }
                "relayer_remote_signer_token" => {
                    if let Some(value) = value {
                        if config.relayer_remote_signer_token.trim() != value {
                            config.relayer_remote_signer_token = value.to_string();
                            config.remote_signer_token.clear();
                            config.order_signer_primary_token_internal.clear();
                            config_changed = true;
                            fixed_keys.push(key.to_string());
                        }
                    }
                }
                "relayer_submit_signer_url" => {
                    if let Some(value) = value {
                        if config.relayer_submit_signer_url.trim() != value {
                            config.relayer_submit_signer_url = value.to_string();
                            config_changed = true;
                            fixed_keys.push(key.to_string());
                        }
                    }
                }
                "wallet_binding" => {
                    if let Some(value) = value {
                        if config.wallet_binding.trim() != value {
                            config.wallet_binding = value.to_string();
                            config_changed = true;
                            fixed_keys.push(key.to_string());
                        }
                    }
                }
                "deposit_wallet" => {
                    if let Some(value) = value {
                        if config.deposit_wallet.trim() != value {
                            config.deposit_wallet = value.to_string();
                            config_changed = true;
                            fixed_keys.push(key.to_string());
                        }
                    }
                }
                "approval_status" => {
                    if let Some(value) = value {
                        if config.approval_status.trim() != value {
                            config.approval_status = value.to_string();
                            config_changed = true;
                            fixed_keys.push(key.to_string());
                        }
                    }
                }
                "admin_api_token" => {
                    if let Some(value) = value {
                        if config.admin_api_token.trim() != value {
                            config.admin_api_token = value.to_string();
                            config_changed = true;
                            fixed_keys.push(key.to_string());
                        }
                    }
                }
                _ => {}
            }
        }
        if clean_onboarding_ready(&config) && config.onboarding_status != "credentials_ready" {
            config.onboarding_status = "credentials_ready".to_string();
            config_changed = true;
        }
    }

    let final_audit = audit_setup_doctor(&config);
    let mut items = final_audit.items.clone();
    mark_fixed_doctor_items(&mut items, &fixed_keys);
    mark_failed_generated_doctor_items(&mut items, &final_audit.missing_generated_keys);

    if !config_changed
        && fixed_keys.is_empty()
        && final_audit.missing_user_labels.is_empty()
        && final_audit.missing_generated_labels.is_empty()
    {
        return Ok(doctor_result("ready", items, None, bot_was_running, false));
    }

    let mut env_path: Option<PathBuf> = None;
    let mut config_path: Option<PathBuf> = None;
    if config_changed {
        match save_profile_and_build_runtime(&profiles, &auth, &data_dir.0, &mut profile, &config) {
            Ok((saved_env_path, saved_config_path)) => {
                env_path = Some(saved_env_path);
                config_path = Some(saved_config_path);
            }
            Err(err) => {
                return Ok(doctor_result(
                    "failed",
                    items,
                    Some(doctor_popup(
                        "Could Not Save Updated Setup",
                        format!(
                            "Setup Doctor fixed fields in memory but could not save them: {err}"
                        ),
                    )),
                    bot_was_running,
                    false,
                ));
            }
        }
    }

    let mut bot_restarted = false;
    if bot_was_running && config_changed {
        if let Err(err) = restart_bot_with_runtime_paths(
            &app,
            &bot,
            &wallet_sync,
            &profile,
            env_path.clone().expect("env path present when fixed"),
            config_path.clone().expect("config path present when fixed"),
            bot_simulation,
        ) {
            return Ok(doctor_result(
                "failed",
                items,
                Some(doctor_popup(
                    "Setup Fixed but Restart Failed",
                    format!(
                        "Setup Doctor saved the missing fields, but the bot restart failed: {err}"
                    ),
                )),
                bot_was_running,
                false,
            ));
        }
        bot_restarted = true;
    }

    append_desktop_debug_line(
        &data_dir.0,
        "DOCTOR",
        format!(
            "setup doctor completed profile={} fixed={} config_changed={} bot_restarted={}",
            profile.id,
            fixed_keys.len(),
            config_changed,
            bot_restarted
        )
        .as_str(),
    );

    if !final_audit.missing_user_labels.is_empty()
        || !final_audit.missing_generated_labels.is_empty()
    {
        return Ok(doctor_result(
            "needs_you",
            items,
            Some(doctor_needs_you_popup(&final_audit)),
            bot_was_running,
            bot_restarted,
        ));
    }

    Ok(doctor_result(
        "fixed",
        items,
        None,
        bot_was_running,
        bot_restarted,
    ))
}

#[tauri::command]
fn export_config(
    auth: State<'_, AuthState>,
    profiles: State<'_, ProfileState>,
    profile_id: String,
    password: String,
    current_password: String,
) -> Result<String, String> {
    let mut auth = auth.lock().map_err(|e| e.to_string())?;
    if !auth
        .verify_password(&current_password)
        .map_err(|e| e.to_string())?
    {
        return Err("current desktop password is incorrect".to_string());
    }
    let pm = profiles.lock().map_err(|e| e.to_string())?;
    let profile = pm.get_profile(&profile_id).ok_or("profile not found")?;
    let bundle = ExportBundleV2 {
        version: 2,
        profile: portable_profile_from_profile(&profile),
        secrets: decrypt_profile_secrets(&profile, &auth)?,
    };
    let json = serde_json::to_string(&bundle).map_err(|e| e.to_string())?;
    crypto_vault::encrypt_data(json.as_bytes(), &password).map_err(|e| e.to_string())
}

#[tauri::command]
fn import_config(
    auth: State<'_, AuthState>,
    profiles: State<'_, ProfileState>,
    data: String,
    password: String,
    current_password: String,
) -> Result<String, String> {
    let mut auth = auth.lock().map_err(|e| e.to_string())?;
    if !auth
        .verify_password(&current_password)
        .map_err(|e| e.to_string())?
    {
        return Err("current desktop password is incorrect".to_string());
    }
    let decrypted = crypto_vault::decrypt_data(&data, &password).map_err(|e| e.to_string())?;
    let imported: ExportBundleV2 = serde_json::from_slice(&decrypted).map_err(|e| e.to_string())?;
    if imported.version != 2 {
        return Err(format!(
            "unsupported import bundle version {}",
            imported.version
        ));
    }
    let mut strategy_config = imported.profile.strategy_config.clone();
    normalize_endgame_timeframes(&mut strategy_config);
    let pm = profiles.lock().map_err(|e| e.to_string())?;
    let mut created = pm
        .create_profile(
            imported.profile.name,
            imported.profile.eoa_wallet_address,
            imported.profile.proxy_wallet_address,
            imported.profile.deposit_wallet_address,
            imported.profile.signature_type,
            strategy_config,
            imported.profile.sizing_config.clone(),
        )
        .map_err(|e| e.to_string())?;
    if imported.secrets.is_empty() {
        created.encrypted_secrets.clear();
    } else {
        let secret_blob = serde_json::to_vec(&imported.secrets).map_err(|e| e.to_string())?;
        created.encrypted_secrets = auth
            .with_session_password(|session_password| {
                crypto_vault::encrypt_data(&secret_blob, session_password)
            })
            .ok_or("session locked: unlock app before importing")?
            .map_err(|e| e.to_string())?;
    }
    pm.update_profile(created.clone())
        .map_err(|e| e.to_string())?;
    pm.set_active_profile(&created.id)
        .map_err(|e| e.to_string())?;
    Ok(created.id)
}

// ── Data (tracking.db) ──────────────────────────────────────────────

fn build_trade_stats_value(data_dir: &Path, profile_start_ms: i64) -> serde_json::Value {
    let empty = serde_json::json!({
        "total_pnl": 0.0,
        "win_rate": 0.0,
        "total_trades": 0,
        "winning_trades": 0,
        "losing_trades": 0,
        "avg_ack_latency_ms": Value::Null,
        "ack_sample_count": 0,
        "pnl_history": []
    });

    let conn = match open_tracking_connection(data_dir) {
        Some(c) => c,
        None => return empty.clone(),
    };

    let total_trades: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM fills_v2 WHERE COALESCE(ts_ms, created_at_ms, 0) >= ?1",
            [profile_start_ms],
            |row| row.get(0),
        )
        .unwrap_or(0);
    let total_pnl: f64 = conn
        .query_row(
            "SELECT COALESCE(SUM(realized_pnl_usd), 0.0) FROM positions_v2 WHERE COALESCE(opened_at_ms, updated_at_ms, 0) >= ?1",
            [profile_start_ms],
            |row| row.get(0),
        )
        .unwrap_or(0.0);
    let history_start_ms = start_of_current_utc_day_ms().max(profile_start_ms);
    let (winning_trades, losing_trades): (i64, i64) = conn
        .query_row(
            "SELECT \
                COALESCE(SUM(CASE WHEN COALESCE(pnl_usd, 0.0) > 0 THEN 1 ELSE 0 END), 0), \
                COALESCE(SUM(CASE WHEN COALESCE(pnl_usd, 0.0) < 0 THEN 1 ELSE 0 END), 0) \
             FROM trade_events WHERE event_type='EXIT' AND COALESCE(ts_ms, 0) >= ?1",
            [history_start_ms],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap_or((0, 0));
    let denominator = (winning_trades + losing_trades) as f64;
    let win_rate = if denominator > 0.0 {
        (winning_trades as f64 / denominator) * 100.0
    } else {
        0.0
    };
    let (ack_sample_count, avg_ack_latency_ms, _) =
        query_ack_latency_summary(&conn, profile_start_ms);

    let mut pnl_history = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT ((ts_ms / 3600000) * 3600000) AS bucket_ms, \
                COALESCE(SUM(COALESCE(pnl_usd, 0.0)), 0.0) AS pnl_delta \
         FROM trade_events \
         WHERE event_type='EXIT' AND COALESCE(ts_ms, 0) >= ?1 \
         GROUP BY bucket_ms \
         ORDER BY bucket_ms ASC",
    ) {
        if let Ok(rows) = stmt.query_map([history_start_ms], |row| {
            let bucket_ms: i64 = row.get(0)?;
            let pnl_delta: f64 = row.get(1)?;
            Ok((bucket_ms, pnl_delta))
        }) {
            let mut cumulative = 0.0_f64;
            for row in rows.flatten() {
                cumulative += row.1;
                pnl_history.push(serde_json::json!({
                    "timestamp": iso_from_ms(row.0),
                    "pnl": cumulative
                }));
            }
        }
    }

    serde_json::json!({
        "total_pnl": total_pnl,
        "win_rate": win_rate,
        "total_trades": total_trades,
        "winning_trades": winning_trades,
        "losing_trades": losing_trades,
        "avg_ack_latency_ms": avg_ack_latency_ms,
        "ack_sample_count": ack_sample_count,
        "pnl_history": pnl_history
    })
}

fn normalize_performance_range(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "6h" => "6h",
        "7d" => "7d",
        "30d" => "30d",
        "all" => "all",
        _ => "1d",
    }
}

fn performance_range_label(range: &str) -> &'static str {
    match range {
        "6h" => "6H",
        "7d" => "7D",
        "30d" => "30D",
        "all" => "ALL",
        _ => "1D",
    }
}

fn performance_range_start_ms(range: &str, now_ms: i64, profile_start_ms: i64) -> i64 {
    let window_ms = match range {
        "6h" => Some(6 * 60 * 60 * 1_000),
        "1d" => Some(24 * 60 * 60 * 1_000),
        "7d" => Some(7 * 24 * 60 * 60 * 1_000),
        "30d" => Some(30 * 24 * 60 * 60 * 1_000),
        "all" => None,
        _ => Some(24 * 60 * 60 * 1_000),
    };
    window_ms
        .map(|ms| now_ms.saturating_sub(ms).max(profile_start_ms))
        .unwrap_or(profile_start_ms)
}

fn performance_window_value(conn: &Connection, range: &str, start_ms: i64) -> serde_json::Value {
    let bucket_ms = if range == "30d" || range == "all" {
        24 * 60 * 60 * 1_000_i64
    } else {
        60 * 60 * 1_000_i64
    };
    let mut series = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT ((ts_ms / ?2) * ?2) AS bucket_ms, \
                COALESCE(SUM(COALESCE(pnl_usd, 0.0)), 0.0) AS pnl_delta \
         FROM trade_events \
         WHERE event_type='EXIT' AND COALESCE(ts_ms, 0) >= ?1 \
         GROUP BY bucket_ms \
         ORDER BY bucket_ms ASC",
    ) {
        if let Ok(rows) = stmt.query_map([start_ms, bucket_ms], |row| {
            let bucket_ms: i64 = row.get(0)?;
            let pnl_delta: f64 = row.get(1)?;
            Ok((bucket_ms, pnl_delta))
        }) {
            let mut cumulative = 0.0_f64;
            for row in rows.flatten() {
                cumulative += row.1;
                series.push(serde_json::json!({
                    "ts": iso_from_ms(row.0),
                    "value": cumulative,
                    "raw_value": cumulative,
                }));
            }
        }
    }
    let profit_loss = series
        .last()
        .and_then(|value| value.get("value"))
        .and_then(Value::as_f64);
    serde_json::json!({
        "range": range,
        "label": performance_range_label(range),
        "profit_loss": profit_loss,
        "series": series,
    })
}

fn performance_daily_stats(conn: &Connection, profile_start_ms: i64) -> Vec<serde_json::Value> {
    let mut stats = Vec::new();
    let sql = "SELECT \
            strftime('%Y-%m-%d', COALESCE(ts_ms, 0) / 1000, 'unixepoch') AS day, \
            COALESCE(SUM(CASE WHEN event_type='EXIT' THEN COALESCE(pnl_usd, 0.0) ELSE 0.0 END), 0.0), \
            COALESCE(SUM(ABS(COALESCE(notional_usd, 0.0))), 0.0), \
            COUNT(*) \
        FROM trade_events \
        WHERE COALESCE(ts_ms, 0) >= ?1 \
        GROUP BY day \
        ORDER BY day ASC";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return stats;
    };
    if let Ok(rows) = stmt.query_map([profile_start_ms], |row| {
        let date: String = row.get(0)?;
        let pnl: f64 = row.get(1)?;
        let volume: f64 = row.get(2)?;
        let trades: i64 = row.get(3)?;
        Ok((date, pnl, volume, trades))
    }) {
        for row in rows.flatten() {
            stats.push(serde_json::json!({
                "date": row.0,
                "pnl": row.1,
                "volume": row.2,
                "maker_rebate": Value::Null,
                "lp_rewards": Value::Null,
                "trades": row.3,
            }));
        }
    }
    stats
}

fn open_position_performance_summary(conn: &Connection) -> (Option<f64>, Option<f64>) {
    let sql = "SELECT \
            SUM(CASE WHEN lm.price IS NOT NULL THEN (lm.price - avg_entry_price) * net_units ELSE NULL END), \
            SUM(CASE WHEN lm.price IS NOT NULL THEN lm.price * net_units ELSE avg_entry_price * net_units END) \
        FROM ( \
            SELECT p.position_key, \
                (COALESCE(p.entry_units, 0.0) - COALESCE(p.exit_units, 0.0) - COALESCE(p.inventory_consumed_units, 0.0)) AS net_units, \
                CASE WHEN COALESCE(p.entry_units, 0.0) > 0.0 THEN COALESCE(p.entry_notional_usd, 0.0) / p.entry_units ELSE 0.0 END AS avg_entry_price \
            FROM positions_v2 p \
            WHERE p.status='OPEN' \
        ) open_pos \
        LEFT JOIN ( \
            SELECT m.position_key, m.price \
            FROM marks_v2 m \
            INNER JOIN ( \
                SELECT position_key, MAX(ts_ms) AS max_ts \
                FROM marks_v2 GROUP BY position_key \
            ) latest ON latest.position_key = m.position_key AND latest.max_ts = m.ts_ms \
        ) lm ON lm.position_key = open_pos.position_key \
        WHERE net_units > 1e-9";
    conn.query_row(sql, [], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap_or((None, None))
}

fn build_home_performance_value(
    data_dir: &Path,
    profile: &Profile,
    range: &str,
) -> serde_json::Value {
    let range = normalize_performance_range(range);
    let now_ms = Utc::now().timestamp_millis();
    let as_of_utc = Utc::now().to_rfc3339();
    let empty_windows = ["6h", "1d", "7d", "30d", "all"]
        .into_iter()
        .map(|entry| {
            (
                entry.to_string(),
                serde_json::json!({
                    "range": entry,
                    "label": performance_range_label(entry),
                    "profit_loss": Value::Null,
                    "series": [],
                }),
            )
        })
        .collect::<Map<_, _>>();

    let Some(conn) = open_tracking_connection(data_dir) else {
        return serde_json::json!({
            "ok": true,
            "profile_id": profile.id,
            "profile_name": profile.name,
            "range": range,
            "profit_loss": Value::Null,
            "realized_pnl": Value::Null,
            "open_pnl": Value::Null,
            "position_value": Value::Null,
            "available_balance": Value::Null,
            "rewards": Value::Null,
            "series": [],
            "windows": Value::Object(empty_windows),
            "daily_stats": [],
            "daily_stats_partial": false,
            "all_time": {
                "profit_loss": Value::Null,
                "volume": Value::Null,
                "maker_rebate": Value::Null,
                "lp_rewards": Value::Null,
                "volume_partial": false,
                "maker_rebate_partial": false,
            },
            "as_of_utc": as_of_utc,
            "source": "local_tracking_empty",
            "error": "tracking_db_unavailable",
        });
    };

    let profile_start_ms = profile_stats_start_ms(profile);
    let mut windows = Map::new();
    for entry in ["6h", "1d", "7d", "30d", "all"] {
        let start_ms = performance_range_start_ms(entry, now_ms, profile_start_ms);
        windows.insert(
            entry.to_string(),
            performance_window_value(&conn, entry, start_ms),
        );
    }

    let selected = windows.get(range).cloned().unwrap_or_else(|| {
        serde_json::json!({
            "range": range,
            "label": performance_range_label(range),
            "profit_loss": Value::Null,
            "series": [],
        })
    });
    let profit_loss = selected.get("profit_loss").cloned().unwrap_or(Value::Null);
    let series = selected
        .get("series")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let daily_stats = performance_daily_stats(&conn, profile_start_ms);
    let (open_pnl, position_value) = open_position_performance_summary(&conn);
    let all_time_window = windows
        .get("all")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let all_time_pnl = all_time_window
        .get("profit_loss")
        .cloned()
        .unwrap_or(Value::Null);
    let all_time_volume = daily_stats
        .iter()
        .filter_map(|stat| stat.get("volume").and_then(Value::as_f64))
        .sum::<f64>();
    let realized_pnl = profit_loss.clone();

    serde_json::json!({
        "ok": true,
        "profile_id": profile.id,
        "profile_name": profile.name,
        "range": range,
        "profit_loss": profit_loss,
        "realized_pnl": realized_pnl,
        "open_pnl": open_pnl,
        "position_value": position_value,
        "available_balance": Value::Null,
        "rewards": Value::Null,
        "series": series,
        "windows": Value::Object(windows),
        "daily_stats": daily_stats,
        "daily_stats_partial": false,
        "all_time": {
            "profit_loss": all_time_pnl,
            "volume": all_time_volume,
            "maker_rebate": Value::Null,
            "lp_rewards": Value::Null,
            "volume_partial": false,
            "maker_rebate_partial": false,
        },
        "as_of_utc": as_of_utc,
        "source": "local_tracking",
        "error": Value::Null,
    })
}

#[tauri::command]
fn get_trade_stats(
    data_dir: State<'_, AppDataDir>,
    profiles: State<'_, ProfileState>,
    overview_cache: State<'_, HomeOverviewCacheState>,
) -> serde_json::Value {
    let now_ms = Utc::now().timestamp_millis();
    let profile = {
        let Ok(pm) = profiles.lock() else {
            return serde_json::json!({
                "total_pnl": 0.0,
                "win_rate": 0.0,
                "total_trades": 0,
                "winning_trades": 0,
                "losing_trades": 0,
                "avg_ack_latency_ms": Value::Null,
                "ack_sample_count": 0,
                "pnl_history": []
            });
        };
        match active_profile(&pm) {
            Ok(profile) => profile,
            Err(_) => {
                return serde_json::json!({
                    "total_pnl": 0.0,
                    "win_rate": 0.0,
                    "total_trades": 0,
                    "winning_trades": 0,
                    "losing_trades": 0,
                    "avg_ack_latency_ms": Value::Null,
                    "ack_sample_count": 0,
                    "pnl_history": []
                });
            }
        }
    };
    let wallet_address = profile.primary_wallet_address();
    if let Some(snapshot) = overview_cache
        .0
        .lock()
        .ok()
        .and_then(|cache| cache.trade_stats.clone())
        .filter(|snapshot| {
            snapshot.profile_id == profile.id
                && snapshot
                    .wallet_address
                    .eq_ignore_ascii_case(wallet_address.as_str())
                && now_ms.saturating_sub(snapshot.fetched_at_ms) <= TRADE_STATS_REFRESH_MS
        })
    {
        return snapshot.value;
    }

    let value = build_trade_stats_value(&data_dir.0, profile_stats_start_ms(&profile));
    if let Ok(mut cache) = overview_cache.0.lock() {
        cache.trade_stats = Some(HomeTradeStatsSnapshot {
            profile_id: profile.id,
            wallet_address,
            fetched_at_ms: now_ms,
            value: value.clone(),
        });
    }
    value
}

#[tauri::command]
fn get_home_performance_api(
    data_dir: State<'_, AppDataDir>,
    profiles: State<'_, ProfileState>,
    range: String,
) -> serde_json::Value {
    let profile = {
        let Ok(pm) = profiles.lock() else {
            return serde_json::json!({
                "ok": true,
                "profile_id": Value::Null,
                "profile_name": Value::Null,
                "range": normalize_performance_range(&range),
                "profit_loss": Value::Null,
                "realized_pnl": Value::Null,
                "open_pnl": Value::Null,
                "position_value": Value::Null,
                "available_balance": Value::Null,
                "rewards": Value::Null,
                "series": [],
                "windows": {},
                "daily_stats": [],
                "daily_stats_partial": false,
                "all_time": Value::Null,
                "as_of_utc": Utc::now().to_rfc3339(),
                "source": "unavailable",
                "error": "profile_lock_unavailable",
            });
        };
        match active_profile(&pm) {
            Ok(profile) => profile,
            Err(err) => {
                return serde_json::json!({
                    "ok": true,
                    "profile_id": Value::Null,
                    "profile_name": Value::Null,
                    "range": normalize_performance_range(&range),
                    "profit_loss": Value::Null,
                    "realized_pnl": Value::Null,
                    "open_pnl": Value::Null,
                    "position_value": Value::Null,
                    "available_balance": Value::Null,
                    "rewards": Value::Null,
                    "series": [],
                    "windows": {},
                    "daily_stats": [],
                    "daily_stats_partial": false,
                    "all_time": Value::Null,
                    "as_of_utc": Utc::now().to_rfc3339(),
                    "source": "unavailable",
                    "error": err,
                });
            }
        }
    };
    build_home_performance_value(&data_dir.0, &profile, &range)
}

#[tauri::command]
fn get_recent_trades(data_dir: State<'_, AppDataDir>, limit: usize) -> Vec<serde_json::Value> {
    let db_path = resolve_tracking_db_path(&data_dir.0);

    let conn = match Connection::open(&db_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let mut stmt = match conn.prepare(
        "SELECT f.id, f.strategy_id, f.token_id, f.side, \
                COALESCE(f.price, 0.0), COALESCE(f.units, 0.0), \
                COALESCE(f.ts_ms, f.created_at_ms, 0), \
                COALESCE(te.pnl_usd, 0.0), f.source_event_type \
         FROM fills_v2 f \
         LEFT JOIN trade_events te ON te.event_key = f.event_key \
         ORDER BY COALESCE(f.ts_ms, f.created_at_ms, 0) DESC \
         LIMIT ?1",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let rows = stmt.query_map([limit as i64], |row| {
        let id: i64 = row.get(0)?;
        let strategy_id: String = row.get(1)?;
        let token_id: String = row.get(2)?;
        let side: String = row.get(3)?;
        let price: f64 = row.get(4)?;
        let size: f64 = row.get(5)?;
        let ts: i64 = row.get(6)?;
        let pnl: f64 = row.get(7)?;
        let source_event_type: String = row.get(8)?;
        let outcome = if pnl > 0.0 {
            "win"
        } else if pnl < 0.0 {
            "loss"
        } else if source_event_type.eq_ignore_ascii_case("EXIT") {
            "breakeven"
        } else {
            "open"
        };
        Ok(serde_json::json!({
            "id": id.to_string(),
            "timestamp": iso_from_ms(ts),
            "market": token_id.clone(),
            "strategy_id": strategy_id,
            "token_id": token_id,
            "side": side.to_ascii_lowercase(),
            "price": price,
            "size": size,
            "outcome": outcome,
            "pnl": pnl,
        }))
    });

    match rows {
        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
        Err(_) => vec![],
    }
}

#[tauri::command]
fn get_open_positions(data_dir: State<'_, AppDataDir>) -> Vec<serde_json::Value> {
    let db_path = resolve_tracking_db_path(&data_dir.0);

    let conn = match Connection::open(&db_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let query_with_marks = "SELECT p.token_id, \
                (COALESCE(p.entry_units, 0.0) - COALESCE(p.exit_units, 0.0) - COALESCE(p.inventory_consumed_units, 0.0)) AS net_units, \
                CASE WHEN COALESCE(p.entry_units, 0.0) > 0.0 THEN COALESCE(p.entry_notional_usd, 0.0) / p.entry_units ELSE 0.0 END AS avg_entry_price, \
                COALESCE(p.realized_pnl_usd, 0.0) AS realized_pnl_usd, \
                lm.price AS mark_price \
         FROM positions_v2 p \
         LEFT JOIN ( \
           SELECT m.position_key, m.price \
           FROM marks_v2 m \
           INNER JOIN ( \
             SELECT position_key, MAX(ts_ms) AS max_ts \
             FROM marks_v2 \
             GROUP BY position_key \
           ) latest ON latest.position_key = m.position_key AND latest.max_ts = m.ts_ms \
         ) lm ON lm.position_key = p.position_key \
         WHERE p.status='OPEN' \
           AND (COALESCE(p.entry_units, 0.0) - COALESCE(p.exit_units, 0.0) - COALESCE(p.inventory_consumed_units, 0.0)) > 1e-9";
    let query_without_marks = "SELECT token_id, \
                (COALESCE(entry_units, 0.0) - COALESCE(exit_units, 0.0) - COALESCE(inventory_consumed_units, 0.0)) AS net_units, \
                CASE WHEN COALESCE(entry_units, 0.0) > 0.0 THEN COALESCE(entry_notional_usd, 0.0) / entry_units ELSE 0.0 END AS avg_entry_price, \
                COALESCE(realized_pnl_usd, 0.0) AS realized_pnl_usd \
         FROM positions_v2 \
         WHERE status='OPEN' \
           AND (COALESCE(entry_units, 0.0) - COALESCE(exit_units, 0.0) - COALESCE(inventory_consumed_units, 0.0)) > 1e-9";

    let (mut stmt, with_marks) = match conn.prepare(query_with_marks) {
        Ok(s) => (s, true),
        Err(_) => match conn.prepare(query_without_marks) {
            Ok(s) => (s, false),
            Err(_) => return vec![],
        },
    };

    let rows = stmt.query_map([], |row| {
        let token_id: String = row.get(0)?;
        let size: f64 = row.get(1)?;
        let entry: f64 = row.get(2)?;
        let realized_pnl: f64 = row.get(3)?;
        let mark_price: Option<f64> = if with_marks { row.get(4)? } else { None };
        let unrealized_pnl = mark_price.map(|mark| (mark - entry) * size);
        let total_pnl = unrealized_pnl.unwrap_or(0.0) + realized_pnl;
        Ok(serde_json::json!({
            "market": token_id,
            "side": "buy",
            "token_id": token_id,
            "size": size,
            "entry_price": entry,
            "current_price": mark_price,
            "realized_pnl": realized_pnl,
            "unrealized_pnl": unrealized_pnl,
            "pnl": total_pnl,
        }))
    });

    match rows {
        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
        Err(_) => vec![],
    }
}

#[tauri::command]
async fn get_wallet_balance(app: AppHandle) -> Result<f64, String> {
    let wallet = {
        let profiles = app.state::<ProfileState>();
        let pm = profiles.lock().map_err(|e| e.to_string())?;
        let id = pm.get_active_profile_id().ok_or("no active profile")?;
        let p = pm.get_profile(&id).ok_or("profile not found")?;
        p.primary_wallet_address()
    };
    get_wallet_balance_for_address(&wallet).await
}

async fn get_wallet_balance_for_address(wallet: &str) -> Result<f64, String> {
    tokio::time::timeout(
        Duration::from_secs(15),
        wallet_rpc::fetch_pusd_balance_with_fallback(&config_io::DESKTOP_POLYGON_RPC_URLS, wallet),
    )
    .await
    .map_err(|_| "wallet balance refresh timed out".to_string())?
}

#[tauri::command]
async fn get_home_overview(
    app: AppHandle,
    bot: State<'_, BotState>,
    auth: State<'_, AuthState>,
    profiles: State<'_, ProfileState>,
    wallet_sync: State<'_, WalletSyncState>,
    liquidity_rewards: State<'_, LiquidityRewardsState>,
    overview_cache: State<'_, HomeOverviewCacheState>,
    data_dir: State<'_, AppDataDir>,
    force_refresh: Option<bool>,
) -> Result<serde_json::Value, String> {
    build_home_overview_payload(
        app,
        bot,
        auth,
        profiles,
        wallet_sync,
        liquidity_rewards,
        overview_cache,
        data_dir,
        force_refresh.unwrap_or(false),
    )
    .await
}

#[tauri::command]
fn get_home_activity(
    data_dir: State<'_, AppDataDir>,
    market_metadata: State<'_, MarketMetadataState>,
    cursor: Option<u64>,
    limit: usize,
) -> serde_json::Value {
    build_home_activity_batch(&data_dir.0, &market_metadata, cursor, limit)
}

#[tauri::command]
async fn get_home_activity_api(
    profiles: State<'_, ProfileState>,
    limit: usize,
) -> Result<serde_json::Value, String> {
    let profile = {
        let pm = profiles.lock().map_err(|e| e.to_string())?;
        active_profile(&pm)?
    };
    let wallet_address = profile.primary_wallet_address();
    let rows = portfolio_api::fetch_activity(&wallet_address, limit.clamp(1, 50)).await?;
    let items = rows
        .into_iter()
        .map(|row| {
            let action = activity_action_label(&row);
            let title = row.title.clone();
            let slug = row.slug.clone();
            let cashflow_usd = activity_cashflow_usd(&row);
            let price = activity_trade_price(&row);
            let timestamp =
                format_api_timestamp(row.timestamp).unwrap_or_else(|| Utc::now().to_rfc3339());
            let is_reward = row
                .activity_type
                .as_deref()
                .map(|value| value.eq_ignore_ascii_case("MAKER_REBATE"))
                .unwrap_or(false);
            let id = row
                .transaction_hash
                .clone()
                .or_else(|| row.asset.clone())
                .or_else(|| row.condition_id.clone())
                .unwrap_or_else(|| row.timestamp.unwrap_or_default().to_string());
            serde_json::json!({
                "id": id,
                "timestamp": timestamp,
                "severity": "info",
                "source": "polymarket",
                "kind": if is_reward { "maker_rebate" } else { "trade" },
                "is_reward": is_reward,
                "action": action,
                "message": title.clone().unwrap_or_else(|| action.clone()),
                "market_title": title.clone(),
                "market_slug": slug.clone(),
                "event_slug": row.event_slug,
                "title": title,
                "outcome": row.outcome,
                "quantity": row.size,
                "cashflow_usd": cashflow_usd,
                "value_usd": cashflow_usd,
                "thumbnail_url": row.icon,
                "detail": slug,
                "condition_id": row.condition_id,
                "token_id": row.asset,
                "price": price,
                "activity_type": row.activity_type,
                "side": row.side,
                "transaction_hash": row.transaction_hash,
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::Value::Array(items))
}

#[tauri::command]
async fn get_home_positions_api(
    profiles: State<'_, ProfileState>,
    limit: usize,
) -> Result<serde_json::Value, String> {
    let profile = {
        let pm = profiles.lock().map_err(|e| e.to_string())?;
        active_profile(&pm)?
    };
    let wallet_address = profile.primary_wallet_address();
    let rows = portfolio_api::fetch_positions(&wallet_address, limit.clamp(1, 100)).await?;
    let items = rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "condition_id": row.condition_id,
                "token_id": row.asset,
                "market_title": row.title,
                "market_slug": row.slug,
                "thumbnail_url": row.icon,
                "event_slug": row.event_slug,
                "outcome": row.outcome,
                "opposite_outcome": row.opposite_outcome,
                "size": row.size,
                "avg_price": row.avg_price,
                "current_price": row.current_price,
                "initial_value": row.initial_value,
                "current_value": row.current_value,
                "cash_pnl": row.cash_pnl,
                "percent_pnl": row.percent_pnl,
                "realized_pnl": row.realized_pnl,
                "total_bought": row.total_bought,
                "redeemable": row.redeemable,
                "mergeable": row.mergeable,
                "end_date": row.end_date,
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::Value::Array(items))
}

#[tauri::command]
async fn get_home_open_orders_api(
    profiles: State<'_, ProfileState>,
    auth: State<'_, AuthState>,
    market_metadata: State<'_, MarketMetadataState>,
    limit: usize,
) -> Result<serde_json::Value, String> {
    let profile = {
        let pm = profiles.lock().map_err(|e| e.to_string())?;
        active_profile(&pm)?
    };
    let query = {
        let auth = auth.lock().map_err(|e| e.to_string())?;
        build_open_orders_query(&profile, &auth)?
    };
    let rows = portfolio_api::fetch_open_orders(&query, limit.clamp(1, 200)).await?;
    let condition_ids = rows
        .iter()
        .map(|order| format!("{:#x}", order.market))
        .collect::<Vec<_>>();
    let metadata_by_condition =
        resolve_gamma_market_metadata_batch(&condition_ids, &market_metadata);
    let items = rows
        .into_iter()
        .map(|order| {
            let condition_id = format!("{:#x}", order.market);
            let condition_key = condition_id.to_ascii_lowercase();
            let token_id = order.asset_id.to_string();
            let price = order.price.to_string().parse::<f64>().ok();
            let original_size = order.original_size.to_string().parse::<f64>().ok();
            let size_matched = order.size_matched.to_string().parse::<f64>().ok();
            let remaining_size = match (original_size, size_matched) {
                (Some(original), Some(matched)) => Some((original - matched).max(0.0)),
                (Some(original), None) => Some(original),
                _ => None,
            };
            let total_notional_usd = match (remaining_size, price) {
                (Some(size), Some(limit_price)) if size.is_finite() && limit_price.is_finite() => {
                    Some((size * limit_price).max(0.0))
                }
                _ => None,
            };
            let metadata = metadata_by_condition.get(&condition_key).cloned();
            let market_title = metadata
                .as_ref()
                .map(|entry| entry.title.clone())
                .unwrap_or_else(|| {
                    let short_condition = if condition_id.len() > 14 {
                        format!("{}...", &condition_id[..14])
                    } else {
                        condition_id.clone()
                    };
                    format!("Unknown market {short_condition}")
                });

            serde_json::json!({
                "id": order.id,
                "status": format!("{:?}", order.status),
                "condition_id": condition_id,
                "token_id": token_id,
                "market_title": market_title,
                "market_slug": metadata.as_ref().and_then(|entry| entry.market_slug.clone()),
                "event_slug": metadata.as_ref().and_then(|entry| entry.event_slug.clone()),
                "thumbnail_url": metadata.as_ref().and_then(|entry| entry.thumbnail_url.clone()),
                "outcome": order.outcome,
                "side": format!("{:?}", order.side),
                "price": price,
                "original_size": original_size,
                "size_matched": size_matched,
                "remaining_size": remaining_size,
                "total_notional_usd": total_notional_usd,
                "created_at": order.created_at.to_rfc3339(),
                "expiration": order.expiration.to_rfc3339(),
                "order_type": format!("{:?}", order.order_type),
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::Value::Array(items))
}

#[tauri::command]
fn get_wallet_sync_status(wallet_sync: State<'_, WalletSyncState>) -> serde_json::Value {
    serde_json::to_value(
        wallet_sync
            .lock()
            .map(|manager| manager.status())
            .unwrap_or_default(),
    )
    .unwrap_or_else(|_| serde_json::json!({}))
}

#[tauri::command]
fn get_geo_access_status() -> GeoAccessStatus {
    geo_access::current_geo_access_status()
}

#[tauri::command]
fn derive_wallet_address(private_key: String) -> Result<String, String> {
    wallet_address_from_private_key(private_key.as_str())
}

#[tauri::command]
fn derive_polymarket_funder_addresses(
    private_key: String,
) -> Result<DerivedPolymarketFunders, String> {
    polymarket_funders_from_private_key(private_key.as_str())
}

#[tauri::command]
async fn get_polymarket_deposit_addresses(address: String) -> Result<serde_json::Value, String> {
    let address = address.trim().to_string();
    if address.is_empty() {
        return Ok(serde_json::json!({
            "evm": Value::Null,
            "solana": Value::Null,
        }));
    }

    let url = format!("{}/deposit", polymarket_bridge_base_url());
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("build Polymarket bridge client: {e}"))?;
    let response = client
        .post(url.as_str())
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .json(&serde_json::json!({ "address": address }))
        .send()
        .await
        .map_err(|e| format!("Polymarket deposit address lookup failed: {e}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("Polymarket deposit address response read failed: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "Polymarket deposit address lookup returned {}: {}",
            status, body
        ));
    }

    let payload: Value = serde_json::from_str(&body)
        .map_err(|e| format!("parse Polymarket deposit address response: {e}"))?;
    let evm = payload
        .pointer("/address/evm")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let solana = payload
        .pointer("/address/svm")
        .or_else(|| payload.pointer("/address/solana"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);

    Ok(serde_json::json!({
        "evm": evm,
        "solana": solana,
    }))
}

#[tauri::command]
async fn run_wallet_sync_now(
    profiles: State<'_, ProfileState>,
    wallet_sync: State<'_, WalletSyncState>,
) -> Result<serde_json::Value, String> {
    let profile = {
        let pm = profiles.lock().map_err(|e| e.to_string())?;
        active_profile(&pm)?
    };
    let manager = wallet_sync.lock().map_err(|e| e.to_string())?.clone();
    let snapshot = manager
        .run_now(wallet_sync_config_for_profile(&profile))
        .await?;
    serde_json::to_value(snapshot).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_data_dir_path(data_dir: State<'_, AppDataDir>) -> String {
    data_dir.0.to_string_lossy().to_string()
}

#[tauri::command]
fn open_logs_folder(data_dir: State<'_, AppDataDir>) -> Result<(), String> {
    ensure_debug_log_files(&data_dir.0)?;
    append_desktop_debug_line(&data_dir.0, "SYSTEM", "open_logs_folder requested");

    #[cfg(target_os = "windows")]
    {
        Command::new("explorer")
            .arg(&data_dir.0)
            .spawn()
            .map_err(|e| format!("open logs folder: {e}"))?;
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(&data_dir.0)
            .spawn()
            .map_err(|e| format!("open logs folder: {e}"))?;
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open")
            .arg(&data_dir.0)
            .spawn()
            .map_err(|e| format!("open logs folder: {e}"))?;
    }

    Ok(())
}

// ── Onboard ──────────────────────────────────────────────────────────

async fn post_desktop_magic_bridge(
    operation: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let base_url = desktop_magic_bridge_base_url();
    let url = format!("{base_url}/v1/desktop/magic/{operation}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("build Magic bridge client: {e}"))?;
    let response = client
        .post(url.as_str())
        .header("accept", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("Magic bridge request failed: {e}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("Magic bridge response read failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("Magic bridge returned {}: {}", status, body));
    }
    serde_json::from_str(&body).map_err(|e| format!("parse Magic bridge response: {e}"))
}

async fn reconcile_desktop_magic_deposit_wallet(
    deposit_wallet: &str,
) -> Result<Option<String>, String> {
    let deposit_wallet = deposit_wallet.trim();
    if deposit_wallet.is_empty() {
        return Ok(None);
    }
    let response = post_desktop_magic_bridge(
        "reconcile",
        serde_json::json!({
            "deposit_wallet_address": deposit_wallet,
        }),
    )
    .await?;
    let status = response
        .get("approval_status")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let funding_status = response
        .get("funding_status")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let funding_balance = response
        .get("funding_balance_pusd")
        .and_then(|value| match value {
            Value::Number(number) => number.as_f64(),
            Value::String(text) => text.parse::<f64>().ok(),
            _ => None,
        })
        .unwrap_or(0.0);
    if response
        .get("profile_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
        && status.as_deref() != Some("ready")
    {
        if funding_balance <= 0.0
            || funding_status.eq_ignore_ascii_case("awaiting_deposit")
            || funding_status.eq_ignore_ascii_case("unknown")
        {
            return Err(
                "Polymarket account created. Deposit Fund to this wallet, then start the bot."
                    .to_string(),
            );
        }
        return Err(format!(
            "Deposit Wallet approval is not ready yet (status: {}). Wait a minute and start again.",
            status.as_deref().unwrap_or("unknown")
        ));
    }
    Ok(status)
}

#[tauri::command]
async fn desktop_magic_start(
    data_dir: State<'_, AppDataDir>,
    email: String,
    profile_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let email = email.trim();
    if email.is_empty() {
        return Err("email is required".to_string());
    }
    let install_id = desktop_install_id(&data_dir.0)?;
    post_desktop_magic_bridge(
        "start",
        serde_json::json!({
            "email": email,
            "desktop_install_id": install_id,
            "local_profile_id": profile_id.unwrap_or_default(),
        }),
    )
    .await
}

fn desktop_magic_finish_payload(
    desktop_onboard_session_id: &str,
    did_token: &str,
    rsa_public_key: &str,
) -> serde_json::Value {
    serde_json::json!({
        "desktop_onboard_session_id": desktop_onboard_session_id.trim(),
        "did_token": did_token.trim(),
        "rsa_public_key": rsa_public_key.trim(),
    })
}

#[tauri::command]
async fn desktop_magic_finish(
    desktop_onboard_session_id: String,
    did_token: String,
    rsa_public_key: String,
) -> Result<serde_json::Value, String> {
    if desktop_onboard_session_id.trim().is_empty() {
        return Err("desktop onboarding session is required".to_string());
    }
    if did_token.trim().is_empty() {
        return Err("Magic DID token is required".to_string());
    }
    if rsa_public_key.trim().is_empty() {
        return Err("RSA public key is required".to_string());
    }
    post_desktop_magic_bridge(
        "finish",
        desktop_magic_finish_payload(&desktop_onboard_session_id, &did_token, &rsa_public_key),
    )
    .await
}

#[tauri::command]
async fn run_onboarding(
    data_dir: State<'_, AppDataDir>,
    private_key: String,
    signature_type: u8,
    proxy_wallet: String,
    deposit_wallet: Option<String>,
) -> Result<serde_json::Value, String> {
    geo_access::ensure_geo_start_allowed()?;
    append_desktop_debug_line(
        &data_dir.0,
        "ONBOARD",
        format!(
            "run_onboarding start signature_type={} proxy_wallet_set={}",
            signature_type,
            !proxy_wallet.trim().is_empty()
        )
        .as_str(),
    );

    let result = onboard::run_onboarding(
        &private_key,
        signature_type,
        &proxy_wallet,
        deposit_wallet.as_deref().unwrap_or_default(),
    )
    .await
    .map_err(|e| {
        append_desktop_debug_line(
            &data_dir.0,
            "ONBOARD",
            format!("run_onboarding error: {e}").as_str(),
        );
        e
    })?;

    append_desktop_debug_line(
        &data_dir.0,
        "ONBOARD",
        format!(
            "run_onboarding success alpha_key_set={} relayer_remote_signer_token_set={}",
            result
                .alpha_key
                .as_ref()
                .map(|v| !v.is_empty())
                .unwrap_or(false),
            result
                .relayer_remote_signer_token
                .as_ref()
                .map(|v| !v.is_empty())
                .unwrap_or(false)
        )
        .as_str(),
    );
    serde_json::to_value(result).map_err(|e| e.to_string())
}

// ── App entry ────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let data_dir = dirs::data_dir()
        .expect("cannot determine data directory")
        .join("evpoly");
    std::fs::create_dir_all(&data_dir).expect("cannot create data directory");
    config_io::cleanup_generated_env_files(&data_dir);
    let _ = ensure_debug_log_files(&data_dir);
    append_desktop_debug_line(&data_dir, "SYSTEM", "desktop app startup");
    let data_dir_for_indexes = data_dir.clone();

    let auth: AuthState = Arc::new(Mutex::new(AppAuth::new(data_dir.clone())));
    let profiles: ProfileState = Arc::new(Mutex::new(ProfileManager::new(data_dir.clone())));
    let bot: BotState = Arc::new(Mutex::new(BotManager::new(data_dir.clone())));
    let wallet_sync: WalletSyncState =
        Arc::new(Mutex::new(WalletSyncManager::new(data_dir.clone())));

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppDataDir(data_dir))
        .manage(auth)
        .manage(profiles)
        .manage(bot)
        .manage(wallet_sync)
        .manage(MarketMetadataState::default())
        .manage(LiquidityRewardsState::default())
        .manage(HomeOverviewCacheState::default())
        .setup(move |app| {
            let tracking_index_dir = data_dir_for_indexes.clone();
            tauri::async_runtime::spawn_blocking(move || {
                ensure_tracking_db_indexes(&tracking_index_dir, false);
            });
            let status_item =
                MenuItem::with_id(app, "status", "EVPoly: Stopped", false, None::<&str>)?;
            let start_item = MenuItem::with_id(app, "start", "Start", true, None::<&str>)?;
            let stop_item = MenuItem::with_id(app, "stop", "Stop", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let show_item = MenuItem::with_id(app, "show", "Show Window", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

            let menu = Menu::with_items(
                app,
                &[
                    &status_item,
                    &start_item,
                    &stop_item,
                    &sep,
                    &show_item,
                    &quit_item,
                ],
            )?;

            let mut tray_builder = TrayIconBuilder::new();
            if let Some(icon) = app.default_window_icon().cloned() {
                tray_builder = tray_builder.icon(icon);
            }

            tray_builder
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "start" => {
                        let dd = app.state::<AppDataDir>();
                        let ps = app.state::<ProfileState>();
                        let auth = app.state::<AuthState>();
                        let bs = app.state::<BotState>();
                        let ws = app.state::<WalletSyncState>();
                        let configs =
                            || -> Option<(String, PathBuf, PathBuf, bool, WalletSyncRuntimeConfig)> {
                                let pm = ps.lock().ok()?;
                                let profile = active_profile(&pm).ok()?;
                                let simulation = simulation_mode_from_profile(&profile);
                                drop(pm);
                                let (profile, env_path, config_path) =
                                    tauri::async_runtime::block_on(
                                        prepare_active_profile_runtime_paths(
                                            &ps,
                                            &auth,
                                            &dd.0,
                                            simulation,
                                        ),
                                    )
                                    .ok()?;
                                Some((
                                    profile.id.clone(),
                                    env_path,
                                    config_path,
                                    simulation,
                                    wallet_sync_config_for_profile(&profile),
                                ))
                            };
                        if let Some((profile_id, env_path, config_path, simulation, wallet_sync_config)) =
                            configs()
                        {
                            if geo_access::ensure_geo_start_allowed().is_err() {
                                return;
                            }
                            if let Ok(bm) = bs.lock() {
                                if bm.start(app, profile_id, env_path, config_path, simulation).is_ok() {
                                    if simulation {
                                        if let Ok(manager) = ws.lock() {
                                            let _ = manager.stop();
                                        }
                                    } else if let Ok(manager) = ws.lock() {
                                        if manager.start(wallet_sync_config).is_err() {
                                            let _ = bm.stop();
                                        }
                                    }
                                }
                            }
                        }
                    }
                    "stop" => {
                        if let Ok(manager) = app.state::<WalletSyncState>().lock() {
                            let _ = manager.stop();
                        }
                        if let Ok(bm) = app.state::<BotState>().lock() {
                            let _ = bm.stop();
                        }
                    }
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "quit" => {
                        if let Ok(manager) = app.state::<WalletSyncState>().lock() {
                            let _ = manager.stop();
                        }
                        if let Ok(bm) = app.state::<BotState>().lock() {
                            let _ = bm.stop();
                        }
                        if let Ok(mut auth) = app.state::<AuthState>().lock() {
                            auth.clear_session();
                        }
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        if let Some(w) = tray.app_handle().get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                })
                .build(app)?;

            if let Some(window) = app.get_webview_window("main") {
                if let Some(icon) = app.default_window_icon().cloned() {
                    let _ = window.set_icon(icon);
                }
                let w = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = w.hide();
                    }
                });
            }

            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            is_auth_initialized,
            initialize_password,
            verify_password,
            lock_session,
            reset_local_app_data,
            list_profiles,
            create_profile,
            get_profile,
            update_profile,
            delete_profile,
            get_active_profile_id,
            set_active_profile,
            start_bot,
            stop_bot,
            restart_bot,
            get_bot_status,
            get_log_lines,
            bot_api_request,
            save_config,
            get_saved_config,
            run_setup_doctor,
            export_config,
            import_config,
            get_trade_stats,
            get_home_performance_api,
            get_recent_trades,
            get_open_positions,
            get_wallet_balance,
            get_home_overview,
            get_home_activity,
            get_home_activity_api,
            get_home_positions_api,
            get_home_open_orders_api,
            get_wallet_sync_status,
            get_geo_access_status,
            derive_wallet_address,
            derive_polymarket_funder_addresses,
            get_polymarket_deposit_addresses,
            run_wallet_sync_now,
            get_data_dir_path,
            open_logs_folder,
            run_onboarding,
            desktop_magic_start,
            desktop_magic_finish,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{
        active_profile_bot_state, activity_trade_price, default_desktop_config,
        desktop_config_to_profile_payload, desktop_magic_finish_payload,
        gamma_market_metadata_batch_url, merge_config_object, merge_desktop_secrets,
        polymarket_funders_from_private_key, profile_to_desktop_config,
        remove_legacy_premarket_ladder_keys, simulation_mode_from_profile,
        PREMARKET_AGGRESSIVE_BIAS_PCT_ENV_KEY, PREMARKET_LADDER_MODE_ENV_KEY_5M,
        PREMARKET_LADDER_MODE_ENV_KEY_NON_M5, PREMARKET_LADDER_MODE_ENV_KEY_NON_M5_LEGACY,
        PREMARKET_LADDER_MODE_ENV_KEY_SHARED, PREMARKET_SAFE_BIAS_PCT_ENV_KEY,
        WEEKEND_POLICY_ENV_KEY,
    };
    use crate::{auth::AppAuth, config_io, profile_manager::Profile};
    use std::collections::HashMap;

    fn profile_with_simulation(simulation: Option<bool>) -> Profile {
        let sizing_config = match simulation {
            Some(value) => serde_json::json!({ "APP_SIMULATION": value }),
            None => serde_json::json!({}),
        };

        Profile {
            id: "p1".to_string(),
            name: "desktop".to_string(),
            eoa_wallet_address: "0x1111111111111111111111111111111111111111".to_string(),
            proxy_wallet_address: "0x2222222222222222222222222222222222222222".to_string(),
            deposit_wallet_address: String::new(),
            wallet_address: "0x2222222222222222222222222222222222222222".to_string(),
            signature_type: 1,
            encrypted_secrets: String::new(),
            strategy_config: serde_json::json!({}),
            sizing_config,
            created_at: "now".to_string(),
            last_used: "now".to_string(),
        }
    }

    #[test]
    fn activity_trade_price_uses_api_price_when_available() {
        let row = crate::portfolio_api::ActivityRow {
            price: Some(0.42),
            size: Some(10.0),
            usdc_size: Some(8.0),
            ..Default::default()
        };

        assert_eq!(activity_trade_price(&row), Some(0.42));
    }

    #[test]
    fn activity_trade_price_derives_missing_api_price() {
        let row = crate::portfolio_api::ActivityRow {
            price: None,
            size: Some(29.2),
            usdc_size: Some(14.6),
            ..Default::default()
        };

        assert_eq!(activity_trade_price(&row), Some(0.5));
    }

    #[test]
    fn gamma_market_metadata_batch_url_uses_repeated_condition_ids() {
        let url = gamma_market_metadata_batch_url(&[
            " 0xBBB ".to_string(),
            "0xaaa".to_string(),
            "0xaaa".to_string(),
        ])
        .expect("gamma url");

        assert_eq!(url.matches("condition_ids=").count(), 2);
        assert!(url.contains("condition_ids=0xaaa"));
        assert!(url.contains("condition_ids=0xbbb"));
        assert!(url.contains("limit=2"));
        assert!(!url.contains("0xaaa%2C0xbbb"));
    }

    #[test]
    fn desktop_magic_finish_payload_matches_bridge_schema() {
        let payload = desktop_magic_finish_payload(" session ", " token ", " public-key ");
        let object = payload.as_object().expect("payload object");

        assert_eq!(
            payload["desktop_onboard_session_id"],
            serde_json::json!("session")
        );
        assert_eq!(payload["did_token"], serde_json::json!("token"));
        assert_eq!(payload["rsa_public_key"], serde_json::json!("public-key"));
        assert!(!object.contains_key("rsa_algorithm"));
    }

    #[test]
    fn unknown_running_bot_is_not_assigned_to_active_profile() {
        assert_eq!(
            active_profile_bot_state("running", Some("active"), None),
            "stopped"
        );
    }

    #[test]
    fn matching_running_bot_is_assigned_to_active_profile() {
        assert_eq!(
            active_profile_bot_state("running", Some("active"), Some("active")),
            "running"
        );
    }

    #[test]
    fn merge_desktop_secrets_clears_blank_managed_fields() {
        let existing = HashMap::from([
            ("POLY_PRIVATE_KEY".to_string(), "old-private".to_string()),
            ("RELAYER_API_KEY".to_string(), "old-relayer".to_string()),
            (
                "EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN".to_string(),
                "old-signer".to_string(),
            ),
            (
                "EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN".to_string(),
                "stale-token".to_string(),
            ),
            ("CUSTOM_KEEP".to_string(), "keep-me".to_string()),
        ]);
        let updates = HashMap::from([("RELAYER_API_KEY".to_string(), "new-relayer".to_string())]);

        let merged = merge_desktop_secrets(existing, updates);

        assert_eq!(
            merged.get("RELAYER_API_KEY"),
            Some(&"new-relayer".to_string())
        );
        assert_eq!(merged.get("CUSTOM_KEEP"), Some(&"keep-me".to_string()));
        assert!(!merged.contains_key("POLY_PRIVATE_KEY"));
        assert!(!merged.contains_key("EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN"));
        assert!(!merged.contains_key("EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN"));
    }

    #[test]
    fn desktop_profile_payload_writes_clean_relayer_remote_signer_token() {
        let mut config = default_desktop_config(
            "0x1111111111111111111111111111111111111111".to_string(),
            "0x2222222222222222222222222222222222222222".to_string(),
            1,
        );
        config.relayer_remote_signer_token = "remote-token".to_string();

        let (_, _, secrets, _, _, _, _) = desktop_config_to_profile_payload(&config);

        assert_eq!(
            secrets.get("EVPOLY_RELAYER_REMOTE_SIGNER_TOKEN"),
            Some(&"remote-token".to_string())
        );
        assert!(!secrets.contains_key("EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN"));
        assert!(!secrets.contains_key("EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN"));
    }

    #[test]
    fn desktop_profile_payload_does_not_promote_legacy_signer_tokens() {
        let mut config = default_desktop_config(
            "0x1111111111111111111111111111111111111111".to_string(),
            "0x2222222222222222222222222222222222222222".to_string(),
            1,
        );
        config.remote_signer_token = "remote-token".to_string();
        config.order_signer_primary_token_internal = "primary-token".to_string();

        let (_, _, secrets, _, _, _, _) = desktop_config_to_profile_payload(&config);

        assert!(!secrets.contains_key("EVPOLY_RELAYER_REMOTE_SIGNER_TOKEN"));
        assert!(!secrets.contains_key("EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN"));
        assert!(!secrets.contains_key("EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN"));
    }

    #[test]
    fn desktop_profile_payload_writes_premarket_ladder_mode() {
        let mut config = default_desktop_config(
            "0x1111111111111111111111111111111111111111".to_string(),
            "0x2222222222222222222222222222222222222222".to_string(),
            1,
        );
        config.strategy_settings.premarket.timeframes = vec!["15m".to_string(), "1h".to_string()];
        config.strategy_settings.premarket.ladder_mode_m5 = "aggressive".to_string();
        config.strategy_settings.premarket.ladder_mode_non_m5 = "safe".to_string();
        config.strategy_settings.premarket.safe_bias_pct = -15.0;
        config.strategy_settings.premarket.aggressive_bias_pct = 30.0;

        let (strategy, _, _, _, _, _, _) = desktop_config_to_profile_payload(&config);
        let strategy = strategy.as_object().expect("strategy object");

        assert_eq!(
            strategy.get(PREMARKET_LADDER_MODE_ENV_KEY_5M),
            Some(&serde_json::json!("aggressive"))
        );
        assert_eq!(
            strategy.get(PREMARKET_LADDER_MODE_ENV_KEY_NON_M5),
            Some(&serde_json::json!("safe"))
        );
        assert_eq!(
            strategy.get(PREMARKET_SAFE_BIAS_PCT_ENV_KEY),
            Some(&serde_json::json!(-15.0))
        );
        assert_eq!(
            strategy.get(PREMARKET_AGGRESSIVE_BIAS_PCT_ENV_KEY),
            Some(&serde_json::json!(30.0))
        );
        assert_eq!(
            strategy.get("EVPOLY_PREMARKET_TIMEFRAMES"),
            Some(&serde_json::json!("15m,1h"))
        );
    }

    #[test]
    fn desktop_profile_payload_writes_weekend_policy() {
        let mut config = default_desktop_config(
            "0x1111111111111111111111111111111111111111".to_string(),
            "0x2222222222222222222222222222222222222222".to_string(),
            1,
        );
        config.weekend_policy = "pause".to_string();

        let (strategy, _, _, _, _, _, _) = desktop_config_to_profile_payload(&config);
        let strategy = strategy.as_object().expect("strategy object");

        assert_eq!(
            strategy.get(WEEKEND_POLICY_ENV_KEY),
            Some(&serde_json::json!("pause"))
        );
    }

    #[test]
    fn desktop_profile_payload_forces_hidden_public_controls_to_defaults() {
        let mut config = default_desktop_config(
            "0x1111111111111111111111111111111111111111".to_string(),
            "0x2222222222222222222222222222222222222222".to_string(),
            1,
        );
        config.strategies.mm_rewards = true;
        config.mm_tuning.rewards_min_share_multiple = 9.0;
        config.strategy_settings.endgame.tick0_multiplier = 9.0;
        config.strategy_settings.endgame.tick1_multiplier = 9.0;
        config.strategy_settings.endgame.tick2_multiplier = 9.0;
        config.strategy_settings.session_band.tau2_enabled = false;
        config.strategy_settings.session_band.tau1_enabled = false;
        config.strategy_settings.session_band.tau2_multiplier = 9.0;
        config.strategy_settings.session_band.tau1_multiplier = 9.0;

        let (strategy, _, _, _, _, _, _) = desktop_config_to_profile_payload(&config);
        let strategy = strategy.as_object().expect("strategy object");

        assert_eq!(
            strategy.get("EVPOLY_STRATEGY_MM_REWARDS_ENABLE"),
            Some(&serde_json::json!(false))
        );
        assert_eq!(
            strategy.get("EVPOLY_MM_REWARD_MIN_TARGET_MULT"),
            Some(&serde_json::json!(1.0))
        );
        assert_eq!(
            strategy.get("EVPOLY_ENDGAME_TICK0_MULTIPLIER"),
            Some(&serde_json::json!(0.20))
        );
        assert_eq!(
            strategy.get("EVPOLY_ENDGAME_TICK1_MULTIPLIER"),
            Some(&serde_json::json!(0.40))
        );
        assert_eq!(
            strategy.get("EVPOLY_ENDGAME_TICK2_MULTIPLIER"),
            Some(&serde_json::json!(0.40))
        );
        assert_eq!(
            strategy.get("EVPOLY_SESSIONBAND_ALLOWED_TAU_SEC"),
            Some(&serde_json::json!("2,1"))
        );
        assert_eq!(
            strategy.get("EVPOLY_SESSIONBAND_TAU2_MULTIPLIER"),
            Some(&serde_json::json!(0.30))
        );
        assert_eq!(
            strategy.get("EVPOLY_SESSIONBAND_TAU1_MULTIPLIER"),
            Some(&serde_json::json!(0.70))
        );
    }

    #[test]
    fn mm_sport_desktop_caps_hide_sport_cap_and_normalize_nonsport_cap() {
        let mut config = default_desktop_config(
            "0x1111111111111111111111111111111111111111".to_string(),
            "0x2222222222222222222222222222222222222222".to_string(),
            1,
        );
        assert_eq!(
            config.strategy_settings.mm_sport.active_sport_market_cap,
            100.0
        );
        assert_eq!(
            config.strategy_settings.mm_sport.active_nonsport_market_cap,
            0.0
        );

        config.strategy_settings.mm_sport.active_sport_market_cap = 77.9;
        config.strategy_settings.mm_sport.active_nonsport_market_cap = -1.0;
        config.strategy_settings.mm_sport.quote_cooldown_min_sec = 61.8;
        config.strategy_settings.mm_sport.quote_cooldown_max_sec = 10.0;

        let (strategy, _, _, _, _, _, _) = desktop_config_to_profile_payload(&config);
        let strategy = strategy.as_object().expect("strategy object");
        assert_eq!(
            strategy.get("EVPOLY_MM_SPORT_ACTIVE_SPORT_MARKET_CAP"),
            None
        );
        assert_eq!(
            strategy.get("EVPOLY_MM_SPORT_ACTIVE_NONSPORT_MARKET_CAP"),
            Some(&serde_json::json!(0.0))
        );
        assert_eq!(
            strategy.get("EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MIN_SEC"),
            Some(&serde_json::json!(61.0))
        );
        assert_eq!(
            strategy.get("EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MAX_SEC"),
            Some(&serde_json::json!(61.0))
        );
    }

    #[test]
    fn mm_sport_profile_load_defaults_missing_caps_by_route() {
        let auth = AppAuth::new(std::env::temp_dir().join("evpoly-test-auth-mm-route-caps"));
        let profile = Profile {
            id: "p-route".to_string(),
            name: "desktop".to_string(),
            eoa_wallet_address: "0x1111111111111111111111111111111111111111".to_string(),
            proxy_wallet_address: "0x2222222222222222222222222222222222222222".to_string(),
            deposit_wallet_address: String::new(),
            wallet_address: "0x2222222222222222222222222222222222222222".to_string(),
            signature_type: 1,
            encrypted_secrets: String::new(),
            strategy_config: serde_json::json!({
                "EVPOLY_MM_SPORT_DISCOVERY_ROUTE": "dual"
            }),
            sizing_config: serde_json::json!({}),
            created_at: "now".to_string(),
            last_used: "now".to_string(),
        };

        let value = profile_to_desktop_config(&profile, &auth).expect("profile to desktop config");
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["active_sport_market_cap"],
            serde_json::json!(50.0)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["active_nonsport_market_cap"],
            serde_json::json!(50.0)
        );
    }

    #[test]
    fn mm_sport_depth_ratio_round_trip_preserves_max_share_ratio() {
        let mut config = default_desktop_config(
            "0x1111111111111111111111111111111111111111".to_string(),
            "0x2222222222222222222222222222222222222222".to_string(),
            1,
        );
        config.strategy_settings.mm_sport.quote_size_mode = "depth_ratio".to_string();
        config.strategy_settings.mm_sport.entry_price_mode = "best_bid".to_string();
        config.strategy_settings.mm_sport.max_share_ratio = 0.4;
        config.strategy_settings.mm_sport.fifo_max_share_ratio = 0.5;
        config.strategy_settings.mm_sport.max_quote_shares = 250.0;
        config.strategy_settings.mm_sport.nonsport_max_quote_shares = 125.0;
        config
            .strategy_settings
            .mm_sport
            .sport_entry_schedule_enabled = true;
        config
            .strategy_settings
            .mm_sport
            .sport_entry_schedule_days_utc = "tue,thu".to_string();
        config
            .strategy_settings
            .mm_sport
            .sport_entry_schedule_start_minute_utc = 800.0;
        config
            .strategy_settings
            .mm_sport
            .sport_entry_schedule_end_minute_utc = 300.0;
        config
            .strategy_settings
            .mm_sport
            .nonsport_entry_schedule_enabled = true;
        config
            .strategy_settings
            .mm_sport
            .nonsport_entry_schedule_days_utc = "mon,wed,sun".to_string();
        config
            .strategy_settings
            .mm_sport
            .nonsport_entry_schedule_start_minute_utc = 780.0;
        config
            .strategy_settings
            .mm_sport
            .nonsport_entry_schedule_end_minute_utc = 240.0;
        config.strategy_settings.mm_sport.active_sport_market_cap = 77.0;
        config.strategy_settings.mm_sport.active_nonsport_market_cap = 33.0;
        config.strategy_settings.mm_sport.quote_cooldown_min_sec = 12.0;
        config.strategy_settings.mm_sport.quote_cooldown_max_sec = 45.0;

        let (strategy, sizing, _, _, _, _, _) = desktop_config_to_profile_payload(&config);
        let strategy_object = strategy.as_object().expect("strategy object");
        assert_eq!(
            strategy_object.get("EVPOLY_MM_SPORT_ENTRY_PRICE_MODE"),
            Some(&serde_json::json!("best_bid"))
        );
        assert_eq!(
            strategy_object.get("EVPOLY_MM_SPORT_MAX_QUOTE_SHARES"),
            Some(&serde_json::json!(250.0))
        );
        assert_eq!(
            strategy_object.get("EVPOLY_MM_SPORT_NONSPORT_MAX_QUOTE_SHARES"),
            Some(&serde_json::json!(125.0))
        );
        assert_eq!(
            strategy_object.get("EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_ENABLE"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            strategy_object.get("EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_DAYS_UTC"),
            Some(&serde_json::json!("tue,thu"))
        );
        assert_eq!(
            strategy_object.get("EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_START_MINUTE_UTC"),
            Some(&serde_json::json!(800.0))
        );
        assert_eq!(
            strategy_object.get("EVPOLY_MM_SPORT_SPORT_ENTRY_SCHEDULE_END_MINUTE_UTC"),
            Some(&serde_json::json!(300.0))
        );
        assert_eq!(
            strategy_object.get("EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_ENABLE"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            strategy_object.get("EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_DAYS_UTC"),
            Some(&serde_json::json!("mon,wed,sun"))
        );
        assert_eq!(
            strategy_object.get("EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_START_MINUTE_UTC"),
            Some(&serde_json::json!(780.0))
        );
        assert_eq!(
            strategy_object.get("EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_END_MINUTE_UTC"),
            Some(&serde_json::json!(240.0))
        );
        let profile = Profile {
            id: "p2".to_string(),
            name: "desktop".to_string(),
            eoa_wallet_address: "0x1111111111111111111111111111111111111111".to_string(),
            proxy_wallet_address: "0x2222222222222222222222222222222222222222".to_string(),
            deposit_wallet_address: String::new(),
            wallet_address: "0x2222222222222222222222222222222222222222".to_string(),
            signature_type: 1,
            encrypted_secrets: String::new(),
            strategy_config: strategy,
            sizing_config: sizing,
            created_at: "now".to_string(),
            last_used: "now".to_string(),
        };
        let auth = AppAuth::new(std::env::temp_dir().join("evpoly-test-auth"));

        let value = profile_to_desktop_config(&profile, &auth).expect("profile to desktop config");

        assert_eq!(
            value["strategy_settings"]["mm_sport"]["quote_size_mode"],
            serde_json::json!("depth_ratio")
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["entry_price_mode"],
            serde_json::json!("best_bid")
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["max_share_ratio"],
            serde_json::json!(0.4)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["fifo_max_share_ratio"],
            serde_json::json!(0.5)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["max_quote_shares"],
            serde_json::json!(250.0)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["nonsport_max_quote_shares"],
            serde_json::json!(125.0)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["sport_entry_schedule_enabled"],
            serde_json::json!(true)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["sport_entry_schedule_days_utc"],
            serde_json::json!("tue,thu")
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["sport_entry_schedule_start_minute_utc"],
            serde_json::json!(800.0)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["sport_entry_schedule_end_minute_utc"],
            serde_json::json!(300.0)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["nonsport_entry_schedule_enabled"],
            serde_json::json!(true)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["nonsport_entry_schedule_days_utc"],
            serde_json::json!("mon,wed,sun")
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["nonsport_entry_schedule_start_minute_utc"],
            serde_json::json!(780.0)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["nonsport_entry_schedule_end_minute_utc"],
            serde_json::json!(240.0)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["active_sport_market_cap"],
            serde_json::json!(100.0)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["active_nonsport_market_cap"],
            serde_json::json!(33.0)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["quote_cooldown_min_sec"],
            serde_json::json!(12.0)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["quote_cooldown_max_sec"],
            serde_json::json!(45.0)
        );
    }

    #[test]
    fn mm_sport_nonsport_sizing_round_trip_preserves_overrides() {
        let mut config = default_desktop_config(
            "0x1111111111111111111111111111111111111111".to_string(),
            "0x2222222222222222222222222222222222222222".to_string(),
            1,
        );
        config.mm_tuning.sport_quote_size_multiplier = 2.0;
        config.mm_tuning.nonsport_quote_size_multiplier = 0.7;
        config.strategy_settings.mm_sport.quote_size_mode = "multiple".to_string();
        config.strategy_settings.mm_sport.nonsport_quote_size_mode = "depth_ratio".to_string();
        config.strategy_settings.mm_sport.nonsport_max_share_ratio = 0.05;
        config.strategy_settings.mm_sport.nonsport_min_top_depth_usd = 900.0;

        let (strategy, sizing, _, _, _, _, _) = desktop_config_to_profile_payload(&config);
        let profile = Profile {
            id: "p3".to_string(),
            name: "desktop".to_string(),
            eoa_wallet_address: "0x1111111111111111111111111111111111111111".to_string(),
            proxy_wallet_address: "0x2222222222222222222222222222222222222222".to_string(),
            deposit_wallet_address: String::new(),
            wallet_address: "0x2222222222222222222222222222222222222222".to_string(),
            signature_type: 1,
            encrypted_secrets: String::new(),
            strategy_config: strategy,
            sizing_config: sizing,
            created_at: "now".to_string(),
            last_used: "now".to_string(),
        };
        let auth = AppAuth::new(std::env::temp_dir().join("evpoly-test-auth-nonsport-sizing"));

        let value = profile_to_desktop_config(&profile, &auth).expect("profile to desktop config");

        assert_eq!(
            value["mm_tuning"]["nonsport_quote_size_multiplier"],
            serde_json::json!(0.7)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["nonsport_quote_size_mode"],
            serde_json::json!("depth_ratio")
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["nonsport_multiple_collateral_cap_mult"],
            serde_json::json!(0.45)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["nonsport_depth_ratio_collateral_cap_mult"],
            serde_json::json!(0.45)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["nonsport_max_share_ratio"],
            serde_json::json!(0.05)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["nonsport_min_top_depth_usd"],
            serde_json::json!(900.0)
        );
    }

    #[test]
    fn mm_sport_nonsport_sizing_falls_back_to_sport_profile() {
        let mut strategy = serde_json::json!({
            "EVPOLY_MM_SPORT_QUOTE_SIZE_MODE": "multiple",
            "EVPOLY_MM_SPORT_QUOTE_SIZE_MULT": 2.0,
            "EVPOLY_MM_SPORT_MAX_SHARE_RATIO": 0.12,
            "EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD": 1500.0
        });
        remove_legacy_premarket_ladder_keys(&mut strategy);
        let profile = Profile {
            id: "p4".to_string(),
            name: "desktop".to_string(),
            eoa_wallet_address: "0x1111111111111111111111111111111111111111".to_string(),
            proxy_wallet_address: "0x2222222222222222222222222222222222222222".to_string(),
            deposit_wallet_address: String::new(),
            wallet_address: "0x2222222222222222222222222222222222222222".to_string(),
            signature_type: 1,
            encrypted_secrets: String::new(),
            strategy_config: strategy,
            sizing_config: serde_json::json!({}),
            created_at: "now".to_string(),
            last_used: "now".to_string(),
        };
        let auth = AppAuth::new(std::env::temp_dir().join("evpoly-test-auth-nonsport-fallback"));

        let value = profile_to_desktop_config(&profile, &auth).expect("profile to desktop config");

        assert_eq!(
            value["mm_tuning"]["nonsport_quote_size_multiplier"],
            serde_json::json!(2.0)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["nonsport_quote_size_mode"],
            serde_json::json!("multiple")
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["nonsport_multiple_collateral_cap_mult"],
            serde_json::json!(0.45)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["nonsport_depth_ratio_collateral_cap_mult"],
            serde_json::json!(0.45)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["nonsport_max_share_ratio"],
            serde_json::json!(0.12)
        );
        assert_eq!(
            value["strategy_settings"]["mm_sport"]["nonsport_min_top_depth_usd"],
            serde_json::json!(1500.0)
        );
    }

    #[test]
    fn remove_legacy_premarket_ladder_keys_keeps_restored_mode_fields() {
        let mut strategy_config = serde_json::json!({
            PREMARKET_LADDER_MODE_ENV_KEY_SHARED: "safe",
            PREMARKET_LADDER_MODE_ENV_KEY_NON_M5_LEGACY: "safe",
            "EVPOLY_REMOTE_PREMARKET_ALPHA_URL": "https://alpha.evplus.ai/v1/alpha/premarket/ladder",
            "EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN": "stale-token",
            "EVPOLY_PREMARKET_FIXED_LADDER_PRICES": "0.99,0.99,0.99,0.99,0.99,0.99",
            "EVPOLY_PREMARKET_FIXED_LADDER_WEIGHTS": "1,0,0,0,0,0",
            PREMARKET_LADDER_MODE_ENV_KEY_5M: "aggressive",
            PREMARKET_LADDER_MODE_ENV_KEY_NON_M5: "normal",
            PREMARKET_SAFE_BIAS_PCT_ENV_KEY: -15.0,
            PREMARKET_AGGRESSIVE_BIAS_PCT_ENV_KEY: 30.0
        });

        remove_legacy_premarket_ladder_keys(&mut strategy_config);

        let strategy = strategy_config.as_object().expect("strategy object");
        assert!(!strategy.contains_key(PREMARKET_LADDER_MODE_ENV_KEY_SHARED));
        assert!(!strategy.contains_key(PREMARKET_LADDER_MODE_ENV_KEY_NON_M5_LEGACY));
        assert!(!strategy.contains_key("EVPOLY_PREMARKET_FIXED_LADDER_PRICES"));
        assert!(!strategy.contains_key("EVPOLY_PREMARKET_FIXED_LADDER_WEIGHTS"));
        assert!(!strategy.contains_key("EVPOLY_REMOTE_PREMARKET_ALPHA_URL"));
        assert!(!strategy.contains_key("EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN"));
        assert_eq!(
            strategy.get(PREMARKET_LADDER_MODE_ENV_KEY_5M),
            Some(&serde_json::json!("aggressive"))
        );
        assert_eq!(
            strategy.get(PREMARKET_LADDER_MODE_ENV_KEY_NON_M5),
            Some(&serde_json::json!("normal"))
        );
        assert_eq!(
            strategy.get(PREMARKET_SAFE_BIAS_PCT_ENV_KEY),
            Some(&serde_json::json!(-15.0))
        );
        assert_eq!(
            strategy.get(PREMARKET_AGGRESSIVE_BIAS_PCT_ENV_KEY),
            Some(&serde_json::json!(30.0))
        );
    }

    #[test]
    fn simulation_mode_from_profile_prefers_saved_value() {
        let profile = profile_with_simulation(Some(false));
        assert!(!simulation_mode_from_profile(&profile));
    }

    #[test]
    fn simulation_mode_from_profile_falls_back_to_template_default() {
        let profile = profile_with_simulation(None);
        assert_eq!(
            simulation_mode_from_profile(&profile),
            config_io::env_template_default_bool("APP_SIMULATION", false)
        );
    }

    #[test]
    fn merge_config_object_preserves_unknown_keys() {
        let existing = serde_json::json!({
            "EVPOLY_MM_REWARDS_GAMMA_FALLBACK_ENABLE": true,
            "CUSTOM_KEEP": "keep"
        });
        let updates = serde_json::json!({
            "EVPOLY_STRATEGY_MM_REWARDS_ENABLE": true,
            "CUSTOM_KEEP": "override"
        });

        let merged = merge_config_object(&existing, &updates);

        assert_eq!(
            merged["EVPOLY_MM_REWARDS_GAMMA_FALLBACK_ENABLE"],
            serde_json::json!(true)
        );
        assert_eq!(merged["CUSTOM_KEEP"], serde_json::json!("override"));
        assert_eq!(
            merged["EVPOLY_STRATEGY_MM_REWARDS_ENABLE"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn polymarket_funder_derivation_matches_sdk_vectors() {
        let derived = polymarket_funders_from_private_key(
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        )
        .expect("derive funders");

        assert_eq!(
            derived.eoa_wallet.to_lowercase(),
            "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
        );
        assert_eq!(
            derived.proxy_wallet.as_deref().map(str::to_lowercase),
            Some("0x365f0ca36ae1f641e02fe3b7743673da42a13a70".to_string())
        );
        assert_eq!(
            derived.safe_wallet.to_lowercase(),
            "0xd93b25cb943d14d0d34fbaf01fc93a0f8b5f6e47"
        );
    }
}
