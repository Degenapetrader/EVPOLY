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

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ethers_signers::{LocalWallet, Signer};
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;
use serde_json::{Map, Value};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State};

struct AppDataDir(PathBuf);
#[derive(Default)]
struct MarketMetadataState(Mutex<HashMap<String, MarketMetadata>>);
#[derive(Default)]
struct LiquidityRewardsState(Mutex<Option<LiquidityRewardsCacheEntry>>);

type AuthState = Arc<Mutex<AppAuth>>;
type ProfileState = Arc<Mutex<ProfileManager>>;
type BotState = Arc<Mutex<BotManager>>;
type WalletSyncState = Arc<Mutex<WalletSyncManager>>;

const DESKTOP_SYMBOL_ORDER: [&str; 7] = ["BTC", "ETH", "SOL", "XRP", "DOGE", "BNB", "HYPE"];
const CORE_STRATEGY_SYMBOLS: [&str; 4] = ["BTC", "ETH", "SOL", "XRP"];
const DESKTOP_SECRET_KEYS: [&str; 11] = [
    "POLY_PRIVATE_KEY",
    "RELAYER_API_KEY",
    "RELAYER_API_KEY_ADDRESS",
    "EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN",
    "EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN",
    "EVPOLY_REMOTE_MARKET_DISCOVERY_TOKEN",
    "EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN",
    "EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN",
    "EVPOLY_REMOTE_MM_REWARDS_ALPHA_TOKEN",
    "EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TOKEN",
    "EVPOLY_ADMIN_API_TOKEN",
];
const DESKTOP_DEBUG_LOG_NAME: &str = "evpoly-desktop-debug.log.txt";
const FULL_DEBUG_LOG_NAME: &str = "evpoly-full-debug.log.txt";
const BOT_DEBUG_LOG_NAME: &str = "evpoly-debug.log.txt";

#[derive(Clone, Default)]
struct MarketMetadata {
    title: String,
    outcomes_by_token: HashMap<String, String>,
    thumbnail_url: Option<String>,
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
    image: Option<String>,
    icon: Option<String>,
    image_optimized: Option<GammaOptimizedImage>,
    icon_optimized: Option<GammaOptimizedImage>,
}

#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GammaMarketResponse {
    question: Option<String>,
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
    #[serde(default = "default_premarket_ladder_mode_5m")]
    entry_ladder_mode_5m: String,
    #[serde(default = "default_premarket_ladder_mode_non_m5")]
    entry_ladder_mode_non_m5: String,
    cancel_after_open_sec: DesktopPremarketCancelAfterOpen,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopEndgameSettings {
    timeframes: Vec<String>,
    per_period_cap_usd: f64,
    tick0_multiplier: f64,
    tick1_multiplier: f64,
    tick2_multiplier: f64,
}

#[derive(Clone, serde::Deserialize)]
struct DesktopEvcurveSettings {
    timeframes: Vec<String>,
    max_flip_prob: f64,
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
    quote_size_mode: String,
    min_reward_rate_per_day: f64,
    pause_after_fill_sec: f64,
    near_expiry_exit_window_sec: f64,
    inventory_exit_mode: String,
    max_share_ratio: f64,
    min_top_depth_usd: f64,
    quote_expiry_min_sec: f64,
    quote_expiry_max_sec: f64,
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
    relayer_api_key: String,
    relayer_api_key_address: String,
    remote_signer_token: String,
    #[serde(default)]
    order_signer_primary_token_internal: String,
    remote_discovery_token: String,
    remote_premarket_alpha_token: String,
    remote_endgame_alpha_token: String,
    remote_mm_rewards_alpha_token: String,
    remote_evsnipe_discovery_token: String,
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

fn bool_from_object(obj: &Map<String, Value>, key: &str, default: bool) -> bool {
    obj.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn f64_from_object(obj: &Map<String, Value>, key: &str, default: f64) -> f64 {
    obj.get(key).and_then(Value::as_f64).unwrap_or(default)
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

const PREMARKET_LADDER_MODE_NORMAL: &str = "normal";
const PREMARKET_LADDER_MODE_SAFE: &str = "safe";
const PREMARKET_LADDER_MODE_AGGRESSIVE: &str = "aggressive";
const PREMARKET_LADDER_MODE_ENV_KEY_5M: &str = "EVPOLY_PREMARKET_LADDER_MODE_5M";
const PREMARKET_LADDER_MODE_ENV_KEY_NON_M5: &str = "EVPOLY_PREMARKET_LADDER_MODE_NON_5M";
const PREMARKET_LADDER_MODE_ENV_KEY_NON_M5_LEGACY: &str = "EVPOLY_PREMARKET_LADDER_MODE_NON_M5";
const PREMARKET_LADDER_MODE_ENV_KEY_SHARED: &str = "EVPOLY_PREMARKET_LADDER_MODE";
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

fn default_premarket_ladder_mode(env_key: &str) -> String {
    normalize_premarket_ladder_safety_mode(
        config_io::env_template_default_string(env_key).as_deref(),
    )
}

fn default_premarket_ladder_mode_5m() -> String {
    default_premarket_ladder_mode(PREMARKET_LADDER_MODE_ENV_KEY_5M)
}

fn default_premarket_timeframes() -> Vec<String> {
    csv_list(
        config_io::env_template_default_string("EVPOLY_PREMARKET_TIMEFRAMES"),
        &["5m", "15m", "1h", "4h"],
    )
}

fn default_premarket_ladder_mode_non_m5() -> String {
    default_premarket_ladder_mode(PREMARKET_LADDER_MODE_ENV_KEY_NON_M5)
}

fn normalize_premarket_ladder_safety_mode(value: Option<&str>) -> String {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(raw) if raw.eq_ignore_ascii_case(PREMARKET_LADDER_MODE_SAFE) => {
            PREMARKET_LADDER_MODE_SAFE.to_string()
        }
        Some(raw) if raw.eq_ignore_ascii_case(PREMARKET_LADDER_MODE_AGGRESSIVE) => {
            PREMARKET_LADDER_MODE_AGGRESSIVE.to_string()
        }
        _ => PREMARKET_LADDER_MODE_NORMAL.to_string(),
    }
}

#[cfg(test)]
fn premarket_default_ladder_prices_m5() -> Vec<f64> {
    vec![0.31, 0.26, 0.22, 0.16, 0.09, 0.03]
}

#[cfg(test)]
fn premarket_default_ladder_prices_non_m5() -> Vec<f64> {
    vec![0.40, 0.30, 0.24, 0.18, 0.12, 0.06]
}

#[cfg(test)]
fn premarket_default_ladder_weights() -> Vec<f64> {
    vec![0.23, 0.23, 0.17, 0.14, 0.12, 0.11]
}

#[cfg(test)]
fn premarket_mode_factor(mode: &str) -> f64 {
    match mode {
        PREMARKET_LADDER_MODE_SAFE => 0.90,
        PREMARKET_LADDER_MODE_AGGRESSIVE => 1.10,
        _ => 1.0,
    }
}

#[cfg(test)]
fn round_up_to_cent(value: f64) -> f64 {
    (((value * 100.0) - 1e-9).ceil() / 100.0).clamp(0.01, 0.99)
}

#[cfg(test)]
fn premarket_ladder_prices_for_mode(mode: &str, defaults: &[f64]) -> Vec<f64> {
    let factor = premarket_mode_factor(mode);
    defaults
        .into_iter()
        .map(|price| round_up_to_cent(*price * factor))
        .collect()
}

fn infer_premarket_ladder_mode(strategy: &Map<String, Value>, env_key: &str) -> String {
    normalize_premarket_ladder_safety_mode(
        strategy
            .get(env_key)
            .and_then(Value::as_str)
            .or_else(|| {
                (env_key == PREMARKET_LADDER_MODE_ENV_KEY_NON_M5)
                    .then(|| strategy.get(PREMARKET_LADDER_MODE_ENV_KEY_NON_M5_LEGACY))
                    .flatten()
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                strategy
                    .get(PREMARKET_LADDER_MODE_ENV_KEY_SHARED)
                    .and_then(Value::as_str)
            }),
    )
}

fn infer_premarket_ladder_mode_5m(strategy: &Map<String, Value>) -> String {
    infer_premarket_ladder_mode(strategy, PREMARKET_LADDER_MODE_ENV_KEY_5M)
}

fn infer_premarket_ladder_mode_non_m5(strategy: &Map<String, Value>) -> String {
    infer_premarket_ladder_mode(strategy, PREMARKET_LADDER_MODE_ENV_KEY_NON_M5)
}

fn default_desktop_config(eoa_wallet: String, proxy_wallet: String, sig_type: u8) -> DesktopConfig {
    let sessionband_allowed_tau = csv_list(
        config_io::env_template_default_string("EVPOLY_SESSIONBAND_ALLOWED_TAU_SEC"),
        &["2", "1"],
    );
    DesktopConfig {
        private_key: String::new(),
        eoa_wallet,
        proxy_wallet,
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
            session_band: config_io::env_template_default_bool(
                "EVPOLY_STRATEGY_SESSIONBAND_ENABLE",
                false,
            ),
            evsnipe: config_io::env_template_default_bool("EVPOLY_STRATEGY_EVSNIPE_ENABLE", true),
            mm_rewards: config_io::env_template_default_bool(
                "EVPOLY_STRATEGY_MM_REWARDS_ENABLE",
                false,
            ),
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
            evsnipe_per_hit: config_io::env_template_default_f64("EVPOLY_EVSNIPE_SIZE_USD", 10.0),
        },
        caps: DesktopCaps {
            premarket: 100000.0,
            endgame: 100000.0,
            evcurve: 100000.0,
            session_band: 100000.0,
            evsnipe: 100000.0,
        },
        mm_tuning: DesktopMmTuning {
            rewards_min_share_multiple: config_io::env_template_default_f64(
                "EVPOLY_MM_REWARD_MIN_TARGET_MULT",
                1.0,
            ),
            sport_quote_size_multiplier: config_io::env_template_default_f64(
                "EVPOLY_MM_SPORT_QUOTE_SIZE_MULT",
                1.2,
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
                entry_ladder_mode_5m: default_premarket_ladder_mode_5m(),
                entry_ladder_mode_non_m5: default_premarket_ladder_mode_non_m5(),
                cancel_after_open_sec: DesktopPremarketCancelAfterOpen {
                    m5: config_io::env_template_default_f64(
                        "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_5M_SEC",
                        20.0,
                    ),
                    m15: config_io::env_template_default_f64(
                        "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_15M_SEC",
                        15.0,
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
                timeframes: csv_list(
                    config_io::env_template_default_string("EVPOLY_ENDGAME_TIMEFRAMES"),
                    &["5m", "15m", "1h", "4h"],
                ),
                per_period_cap_usd: config_io::env_template_default_f64(
                    "EVPOLY_ENDGAME_PER_PERIOD_CAP_USD",
                    10000.0,
                ),
                tick0_multiplier: config_io::env_template_default_f64(
                    "EVPOLY_ENDGAME_TICK0_MULTIPLIER",
                    0.20,
                ),
                tick1_multiplier: config_io::env_template_default_f64(
                    "EVPOLY_ENDGAME_TICK1_MULTIPLIER",
                    0.40,
                ),
                tick2_multiplier: config_io::env_template_default_f64(
                    "EVPOLY_ENDGAME_TICK2_MULTIPLIER",
                    0.40,
                ),
            },
            evcurve: DesktopEvcurveSettings {
                timeframes: csv_list(
                    config_io::env_template_default_string("EVPOLY_EVCURVE_TIMEFRAMES"),
                    &["15m", "1h", "4h", "1d"],
                ),
                max_flip_prob: config_io::env_template_default_f64(
                    "EVPOLY_EVCURVE_MAX_FLIP_PROB",
                    0.15,
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
                tau2_enabled: sessionband_allowed_tau.iter().any(|value| value == "2"),
                tau1_enabled: sessionband_allowed_tau.iter().any(|value| value == "1"),
                tau2_multiplier: config_io::env_template_default_f64(
                    "EVPOLY_SESSIONBAND_TAU2_MULTIPLIER",
                    0.30,
                ),
                tau1_multiplier: config_io::env_template_default_f64(
                    "EVPOLY_SESSIONBAND_TAU1_MULTIPLIER",
                    0.70,
                ),
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
                quote_size_mode: normalize_mm_sport_quote_size_mode(
                    config_io::env_template_default_string("EVPOLY_MM_SPORT_QUOTE_SIZE_MODE")
                        .as_deref()
                        .unwrap_or("multiple"),
                )
                .to_string(),
                min_reward_rate_per_day: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_MIN_REWARD_RATE_PER_DAY",
                    300.0,
                ),
                pause_after_fill_sec: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC",
                    7200.0,
                ),
                near_expiry_exit_window_sec: config_io::env_template_default_f64(
                    "EVPOLY_MM_NEAR_EXPIRY_EXIT_WINDOW_SEC",
                    86400.0,
                ),
                inventory_exit_mode: normalize_mm_sport_exit_mode(
                    config_io::env_template_default_string("EVPOLY_MM_SPORT_EXIT_MODE")
                        .as_deref()
                        .unwrap_or("normal"),
                )
                .to_string(),
                max_share_ratio: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_MAX_SHARE_RATIO",
                    0.05,
                ),
                min_top_depth_usd: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD",
                    10000.0,
                ),
                quote_expiry_min_sec: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC",
                    180.0,
                ),
                quote_expiry_max_sec: config_io::env_template_default_f64(
                    "EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC",
                    300.0,
                ),
            },
        },
        simulation: config_io::env_template_default_bool("APP_SIMULATION", false),
        relayer_api_key: String::new(),
        relayer_api_key_address: String::new(),
        remote_signer_token: String::new(),
        order_signer_primary_token_internal: String::new(),
        remote_discovery_token: String::new(),
        remote_premarket_alpha_token: String::new(),
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

fn has_shared_alpha_source(config: &DesktopConfig) -> bool {
    [
        config.remote_premarket_alpha_token.as_str(),
        config.remote_endgame_alpha_token.as_str(),
        config.remote_mm_rewards_alpha_token.as_str(),
        config.remote_discovery_token.as_str(),
        config.remote_evsnipe_discovery_token.as_str(),
    ]
    .iter()
    .any(|value| !value.trim().is_empty())
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

    if matches!(config.sig_type, 1 | 2) && config.relayer_api_key.trim().is_empty() {
        push_doctor_missing_user(
            &mut audit,
            "relayer_api_key",
            "Relayer API Key",
            "Get RELAYER_API_KEY from https://polymarket.com/settings?tab=api-keys, then paste it into Settings -> Setup. EVPoly can still use remote signer fallback where supported.",
            None,
            false,
        );
    }

    if matches!(config.sig_type, 1 | 2) && config.relayer_api_key_address.trim().is_empty() {
        push_doctor_missing_user(
            &mut audit,
            "relayer_api_key_address",
            "Relayer API Key Address",
            "Get RELAYER_API_KEY_ADDRESS from https://polymarket.com/settings?tab=api-keys, then paste it into Settings -> Setup. EVPoly can still use remote signer fallback where supported.",
            None,
            false,
        );
    }

    if config.remote_signer_token.trim().is_empty() {
        push_doctor_missing_generated(
            &mut audit,
            "remote_signer_token",
            "Signer Token",
            "The remote signer token is missing and will be regenerated from onboarding. EVPoly automatically reuses it for primary order signing unless onboarding provides a separate internal override token.",
            None,
        );
    }

    if config.remote_discovery_token.trim().is_empty() {
        push_doctor_missing_generated(
            &mut audit,
            "remote_discovery_token",
            "Remote Discovery Token",
            "The shared remote discovery token is missing and will be regenerated from onboarding.",
            None,
        );
    }

    if config.remote_premarket_alpha_token.trim().is_empty() {
        push_doctor_missing_generated(
            &mut audit,
            "remote_premarket_alpha_token",
            "Premarket Remote Token",
            "The Premarket remote alpha token is missing and will be regenerated from onboarding.",
            Some("Premarket"),
        );
    }

    if config.remote_endgame_alpha_token.trim().is_empty() {
        push_doctor_missing_generated(
            &mut audit,
            "remote_endgame_alpha_token",
            "Endgame Remote Token",
            "The Endgame remote alpha token is missing and will be regenerated from onboarding.",
            Some("Endgame"),
        );
    }

    if config.remote_mm_rewards_alpha_token.trim().is_empty() {
        push_doctor_missing_generated(
            &mut audit,
            "remote_mm_rewards_alpha_token",
            "MM Rewards Remote Token",
            "The MM Rewards remote alpha token is missing and will be regenerated from onboarding.",
            Some("MM Rewards"),
        );
    }

    if config.remote_evsnipe_discovery_token.trim().is_empty() {
        push_doctor_missing_generated(
            &mut audit,
            "remote_evsnipe_discovery_token",
            "EVSnipe Discovery Token",
            "The EVSnipe discovery token is missing and will be regenerated from onboarding.",
            Some("EVSnipe"),
        );
    }

    if !has_shared_alpha_source(config) {
        push_doctor_missing_generated(
            &mut audit,
            "shared_alpha_source",
            "Shared Alpha Token",
            "The shared remote alpha source is missing, so EVCurve and SessionBand cannot backfill their runtime tokens.",
            Some("EVCurve / SessionBand"),
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
    for key in DESKTOP_SECRET_KEYS {
        existing.remove(key);
    }
    existing.extend(updates);
    existing
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
    }
}

fn portable_profile_from_profile(profile: &Profile) -> PortableProfile {
    PortableProfile {
        name: profile.name.clone(),
        eoa_wallet_address: profile.eoa_wallet_address.clone(),
        proxy_wallet_address: profile.proxy_wallet_address.clone(),
        signature_type: profile.signature_type,
        strategy_config: profile.strategy_config.clone(),
        sizing_config: profile.sizing_config.clone(),
    }
}

fn desktop_config_to_profile_payload(
    config: &DesktopConfig,
) -> (Value, Value, HashMap<String, String>, String, String, u8) {
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
        bool_to_json(config.strategies.session_band),
    );
    strategy.insert(
        "EVPOLY_STRATEGY_EVSNIPE_ENABLE".to_string(),
        bool_to_json(config.strategies.evsnipe),
    );
    strategy.insert(
        "EVPOLY_STRATEGY_MM_REWARDS_ENABLE".to_string(),
        bool_to_json(config.strategies.mm_rewards),
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
        number_to_json(config.mm_tuning.rewards_min_share_multiple),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_QUOTE_SIZE_MULT".to_string(),
        number_to_json(config.mm_tuning.sport_quote_size_multiplier),
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
        Value::String(normalize_premarket_ladder_safety_mode(Some(
            config
                .strategy_settings
                .premarket
                .entry_ladder_mode_5m
                .as_str(),
        ))),
    );
    strategy.insert(
        PREMARKET_LADDER_MODE_ENV_KEY_NON_M5.to_string(),
        Value::String(normalize_premarket_ladder_safety_mode(Some(
            config
                .strategy_settings
                .premarket
                .entry_ladder_mode_non_m5
                .as_str(),
        ))),
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
        Value::String(config.strategy_settings.endgame.timeframes.join(",")),
    );
    strategy.insert(
        "EVPOLY_ENDGAME_TICK0_MULTIPLIER".to_string(),
        number_to_json(config.strategy_settings.endgame.tick0_multiplier),
    );
    strategy.insert(
        "EVPOLY_ENDGAME_TICK1_MULTIPLIER".to_string(),
        number_to_json(config.strategy_settings.endgame.tick1_multiplier),
    );
    strategy.insert(
        "EVPOLY_ENDGAME_TICK2_MULTIPLIER".to_string(),
        number_to_json(config.strategy_settings.endgame.tick2_multiplier),
    );
    strategy.insert(
        "EVPOLY_EVCURVE_TIMEFRAMES".to_string(),
        Value::String(config.strategy_settings.evcurve.timeframes.join(",")),
    );
    strategy.insert(
        "EVPOLY_EVCURVE_MAX_FLIP_PROB".to_string(),
        number_to_json(config.strategy_settings.evcurve.max_flip_prob),
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
    let allowed_tau_csv = [
        config
            .strategy_settings
            .session_band
            .tau2_enabled
            .then_some("2"),
        config
            .strategy_settings
            .session_band
            .tau1_enabled
            .then_some("1"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",");
    strategy.insert(
        "EVPOLY_SESSIONBAND_ALLOWED_TAU_SEC".to_string(),
        Value::String(if allowed_tau_csv.is_empty() {
            "2,1".to_string()
        } else {
            allowed_tau_csv
        }),
    );
    strategy.insert(
        "EVPOLY_SESSIONBAND_TAU2_MULTIPLIER".to_string(),
        number_to_json(config.strategy_settings.session_band.tau2_multiplier),
    );
    strategy.insert(
        "EVPOLY_SESSIONBAND_TAU1_MULTIPLIER".to_string(),
        number_to_json(config.strategy_settings.session_band.tau1_multiplier),
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
        "EVPOLY_MM_SPORT_MIN_REWARD_RATE_PER_DAY".to_string(),
        number_to_json(config.strategy_settings.mm_sport.min_reward_rate_per_day),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC".to_string(),
        number_to_json(config.strategy_settings.mm_sport.pause_after_fill_sec),
    );
    strategy.insert(
        "EVPOLY_MM_NEAR_EXPIRY_EXIT_WINDOW_SEC".to_string(),
        number_to_json(
            config
                .strategy_settings
                .mm_sport
                .near_expiry_exit_window_sec,
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
        "EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD".to_string(),
        number_to_json(config.strategy_settings.mm_sport.min_top_depth_usd),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC".to_string(),
        number_to_json(config.strategy_settings.mm_sport.quote_expiry_min_sec),
    );
    strategy.insert(
        "EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC".to_string(),
        number_to_json(config.strategy_settings.mm_sport.quote_expiry_max_sec),
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
        "EVPOLY_ARB_STRAT_SESSIONBAND_MAX_USD".to_string(),
        number_to_json(config.caps.session_band),
    );
    sizing.insert(
        "EVPOLY_ARB_STRAT_EVSNIPE_MAX_USD".to_string(),
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
    if !config.remote_signer_token.trim().is_empty() {
        secrets.insert(
            "EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN".to_string(),
            config.remote_signer_token.trim().to_string(),
        );
        let order_signer_primary_token = distinct_order_signer_primary_token(
            config.remote_signer_token.as_str(),
            config.order_signer_primary_token_internal.as_str(),
        );
        secrets.insert(
            "EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN".to_string(),
            if order_signer_primary_token.is_empty() {
                config.remote_signer_token.trim().to_string()
            } else {
                order_signer_primary_token
            },
        );
    }
    if !config.remote_discovery_token.trim().is_empty() {
        secrets.insert(
            "EVPOLY_REMOTE_MARKET_DISCOVERY_TOKEN".to_string(),
            config.remote_discovery_token.trim().to_string(),
        );
    }
    if !config.remote_premarket_alpha_token.trim().is_empty() {
        secrets.insert(
            "EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN".to_string(),
            config.remote_premarket_alpha_token.trim().to_string(),
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

    let remote_signer_token = secrets
        .get("EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN")
        .cloned()
        .or_else(|| secrets.get("EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN").cloned())
        .unwrap_or_default();
    let order_signer_primary_token_internal = distinct_order_signer_primary_token(
        remote_signer_token.as_str(),
        secrets
            .get("EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN")
            .map(String::as_str)
            .unwrap_or_default(),
    );

    Ok(serde_json::json!({
        "private_key": secrets.get("POLY_PRIVATE_KEY").cloned().unwrap_or_default(),
        "eoa_wallet": profile.eoa_wallet_address.clone(),
        "proxy_wallet": profile.proxy_wallet_address.clone(),
        "sig_type": profile.signature_type,
        "weekend_policy": normalize_weekend_policy(strategy.get(WEEKEND_POLICY_ENV_KEY).and_then(Value::as_str)),
        "symbols": symbols,
        "strategies": {
            "premarket": bool_from_object(&strategy, "EVPOLY_STRATEGY_PREMARKET_ENABLE", config_io::env_template_default_bool("EVPOLY_STRATEGY_PREMARKET_ENABLE", true)),
            "endgame": bool_from_object(&strategy, "EVPOLY_STRATEGY_ENDGAME_ENABLE", config_io::env_template_default_bool("EVPOLY_STRATEGY_ENDGAME_ENABLE", true)),
            "evcurve": bool_from_object(&strategy, "EVPOLY_STRATEGY_EVCURVE_ENABLE", config_io::env_template_default_bool("EVPOLY_STRATEGY_EVCURVE_ENABLE", false)),
            "session_band": bool_from_object(&strategy, "EVPOLY_STRATEGY_SESSIONBAND_ENABLE", config_io::env_template_default_bool("EVPOLY_STRATEGY_SESSIONBAND_ENABLE", false)),
            "evsnipe": bool_from_object(&strategy, "EVPOLY_STRATEGY_EVSNIPE_ENABLE", config_io::env_template_default_bool("EVPOLY_STRATEGY_EVSNIPE_ENABLE", true)),
            "mm_rewards": bool_from_object(&strategy, "EVPOLY_STRATEGY_MM_REWARDS_ENABLE", config_io::env_template_default_bool("EVPOLY_STRATEGY_MM_REWARDS_ENABLE", false)),
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
            "evsnipe_per_hit": f64_from_object(&sizing, "EVPOLY_EVSNIPE_SIZE_USD", 10.0)
        },
        "caps": {
            "premarket": f64_from_object(&sizing, "EVPOLY_ARB_STRAT_PREMARKET_MAX_USD", 100000.0),
            "endgame": f64_from_object(&sizing, "EVPOLY_ARB_STRAT_ENDGAME_MAX_USD", 100000.0),
            "evcurve": f64_from_object(&sizing, "EVPOLY_ARB_STRAT_EVCURVE_MAX_USD", 100000.0),
            "session_band": f64_from_object(&sizing, "EVPOLY_ARB_STRAT_SESSIONBAND_MAX_USD", 100000.0),
            "evsnipe": f64_from_object(&sizing, "EVPOLY_ARB_STRAT_EVSNIPE_MAX_USD", 100000.0)
        },
        "mm_tuning": {
            "rewards_min_share_multiple": f64_from_object(&strategy, "EVPOLY_MM_REWARD_MIN_TARGET_MULT", config_io::env_template_default_f64("EVPOLY_MM_REWARD_MIN_TARGET_MULT", 1.0)),
            "sport_quote_size_multiplier": f64_from_object(&strategy, "EVPOLY_MM_SPORT_QUOTE_SIZE_MULT", config_io::env_template_default_f64("EVPOLY_MM_SPORT_QUOTE_SIZE_MULT", 1.2))
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
                "entry_ladder_mode_5m": infer_premarket_ladder_mode_5m(&strategy),
                "entry_ladder_mode_non_m5": infer_premarket_ladder_mode_non_m5(&strategy),
                "cancel_after_open_sec": {
                    "m5": f64_from_object(&strategy, "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_5M_SEC", config_io::env_template_default_f64("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_5M_SEC", 20.0)),
                    "m15": f64_from_object(&strategy, "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_15M_SEC", config_io::env_template_default_f64("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_15M_SEC", 15.0)),
                    "h1": f64_from_object(&strategy, "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_1H_SEC", config_io::env_template_default_f64("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_1H_SEC", 60.0)),
                    "h4": f64_from_object(&strategy, "EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_4H_SEC", config_io::env_template_default_f64("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_4H_SEC", 180.0))
                }
            },
            "endgame": {
                "timeframes": csv_from_object(&strategy, "EVPOLY_ENDGAME_TIMEFRAMES", &["5m", "15m", "1h", "4h"]),
                "per_period_cap_usd": f64_from_object(&sizing, "EVPOLY_ENDGAME_PER_PERIOD_CAP_USD", config_io::env_template_default_f64("EVPOLY_ENDGAME_PER_PERIOD_CAP_USD", 10000.0)),
                "tick0_multiplier": f64_from_object(&strategy, "EVPOLY_ENDGAME_TICK0_MULTIPLIER", config_io::env_template_default_f64("EVPOLY_ENDGAME_TICK0_MULTIPLIER", 0.20)),
                "tick1_multiplier": f64_from_object(&strategy, "EVPOLY_ENDGAME_TICK1_MULTIPLIER", config_io::env_template_default_f64("EVPOLY_ENDGAME_TICK1_MULTIPLIER", 0.40)),
                "tick2_multiplier": f64_from_object(&strategy, "EVPOLY_ENDGAME_TICK2_MULTIPLIER", config_io::env_template_default_f64("EVPOLY_ENDGAME_TICK2_MULTIPLIER", 0.40))
            },
            "evcurve": {
                "timeframes": csv_from_object(&strategy, "EVPOLY_EVCURVE_TIMEFRAMES", &["15m", "1h", "4h", "1d"]),
                "max_flip_prob": f64_from_object(&strategy, "EVPOLY_EVCURVE_MAX_FLIP_PROB", config_io::env_template_default_f64("EVPOLY_EVCURVE_MAX_FLIP_PROB", 0.15)),
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
                "quote_size_mode": normalize_mm_sport_quote_size_mode(
                    string_from_object(&strategy, "EVPOLY_MM_SPORT_QUOTE_SIZE_MODE", "multiple").as_str()
                ),
                "min_reward_rate_per_day": f64_from_object(&strategy, "EVPOLY_MM_SPORT_MIN_REWARD_RATE_PER_DAY", config_io::env_template_default_f64("EVPOLY_MM_SPORT_MIN_REWARD_RATE_PER_DAY", 300.0)),
                "pause_after_fill_sec": f64_from_object(&strategy, "EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC", config_io::env_template_default_f64("EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC", 7200.0)),
                "near_expiry_exit_window_sec": f64_from_object(&strategy, "EVPOLY_MM_NEAR_EXPIRY_EXIT_WINDOW_SEC", config_io::env_template_default_f64("EVPOLY_MM_NEAR_EXPIRY_EXIT_WINDOW_SEC", 86400.0)),
                "inventory_exit_mode": normalize_mm_sport_exit_mode(
                    string_from_object(&strategy, "EVPOLY_MM_SPORT_EXIT_MODE", "normal").as_str()
                ),
                "max_share_ratio": f64_from_object(&strategy, "EVPOLY_MM_SPORT_MAX_SHARE_RATIO", config_io::env_template_default_f64("EVPOLY_MM_SPORT_MAX_SHARE_RATIO", 0.05)),
                "min_top_depth_usd": f64_from_object(&strategy, "EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD", config_io::env_template_default_f64("EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD", 10000.0)),
                "quote_expiry_min_sec": f64_from_object(&strategy, "EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC", config_io::env_template_default_f64("EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC", 180.0)),
                "quote_expiry_max_sec": f64_from_object(&strategy, "EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC", config_io::env_template_default_f64("EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC", 300.0))
            }
        },
        "simulation": bool_from_object(&sizing, "APP_SIMULATION", default_simulation),
        "relayer_api_key": secrets.get("RELAYER_API_KEY").cloned().unwrap_or_default(),
        "relayer_api_key_address": secrets.get("RELAYER_API_KEY_ADDRESS").cloned().unwrap_or_default(),
        "remote_signer_token": remote_signer_token,
        "order_signer_primary_token_internal": order_signer_primary_token_internal,
        "remote_discovery_token": secrets.get("EVPOLY_REMOTE_MARKET_DISCOVERY_TOKEN").cloned().unwrap_or_default(),
        "remote_premarket_alpha_token": secrets.get("EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN").cloned().unwrap_or_default(),
        "remote_endgame_alpha_token": secrets.get("EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN").cloned().unwrap_or_default(),
        "remote_mm_rewards_alpha_token": secrets.get("EVPOLY_REMOTE_MM_REWARDS_ALPHA_TOKEN").cloned().unwrap_or_default(),
        "remote_evsnipe_discovery_token": secrets.get("EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TOKEN").cloned().unwrap_or_default(),
        "admin_api_token": secrets.get("EVPOLY_ADMIN_API_TOKEN").cloned().unwrap_or_default()
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

fn query_pnl_today_utc(conn: &Connection) -> f64 {
    conn.query_row(
        "SELECT COALESCE(SUM(COALESCE(pnl_usd, 0.0)), 0.0) \
         FROM trade_events \
         WHERE event_type='EXIT' AND COALESCE(ts_ms, 0) >= ?1",
        [start_of_current_utc_day_ms()],
        |row| row.get(0),
    )
    .unwrap_or(0.0)
}

fn query_ack_latency_summary(conn: &Connection) -> (u64, Option<f64>, u64) {
    let cutoff_ms = ack_latency_window_start_ms();
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
                WHERE event_type='ENTRY_ACK' \
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
        .map(|(_, outcomes_by_token)| outcomes_by_token)
        .unwrap_or_default();

    Some(MarketMetadata {
        title,
        outcomes_by_token,
        thumbnail_url: gamma.as_ref().and_then(gamma_market_thumbnail),
    })
}

fn resolve_market_metadata(
    condition_id: &str,
    cache: &MarketMetadataState,
) -> Option<MarketMetadata> {
    if condition_id.trim().is_empty() {
        return None;
    }

    if let Ok(guard) = cache.0.lock() {
        if let Some(entry) = guard.get(condition_id).cloned() {
            return Some(entry);
        }
    }

    let fetched = fetch_market_metadata(condition_id)?;
    if let Ok(mut guard) = cache.0.lock() {
        guard.insert(condition_id.to_string(), fetched.clone());
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

        let cashflow_usd = signed_trade_cashflow(
            &record.side,
            record.notional_usd,
            record.price,
            record.quantity,
        );
        let action = trade_action_label(&record.side).to_string();

        items.push(serde_json::json!({
            "timestamp": record.timestamp,
            "severity": "info",
            "source": "trade",
            "kind": "trade",
            "message": title.clone(),
            "action": action,
            "thumbnail_url": market.as_ref().and_then(|entry| entry.thumbnail_url.clone()),
            "market_title": title.clone(),
            "title": title,
            "outcome": outcome_label,
            "detail": serde_json::Value::Null,
            "quantity": record.quantity,
            "cashflow_usd": cashflow_usd,
            "value_usd": cashflow_usd,
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
    app: AppHandle,
    bot: State<'_, BotState>,
    auth: State<'_, AuthState>,
    profiles: State<'_, ProfileState>,
    wallet_sync: State<'_, WalletSyncState>,
    liquidity_rewards: State<'_, LiquidityRewardsState>,
    data_dir: State<'_, AppDataDir>,
) -> Result<serde_json::Value, String> {
    let bot_snapshot = {
        let manager = bot.lock().map_err(|e| e.to_string())?;
        (
            match manager.get_status() {
                bot_manager::BotStatus::Stopped => "stopped".to_string(),
                bot_manager::BotStatus::Starting => "starting".to_string(),
                bot_manager::BotStatus::Running => "running".to_string(),
                bot_manager::BotStatus::Stopping => "stopping".to_string(),
                bot_manager::BotStatus::Error(err) => format!("error:{err}"),
            },
            manager.simulation_mode(),
            manager.last_activity_at(),
        )
    };
    let wallet_sync_status = wallet_sync.lock().map_err(|e| e.to_string())?.status();
    let db_path = resolve_tracking_db_path(&data_dir.0);
    let (pnl_today_utc, ack_sample_count, avg_ack_latency_ms, recent_ack_warning_count) =
        Connection::open(&db_path)
            .ok()
            .map(|conn| {
                let pnl = query_pnl_today_utc(&conn);
                let (ack_count, ack_avg, ack_warnings) = query_ack_latency_summary(&conn);
                (pnl, ack_count, ack_avg, ack_warnings)
            })
            .unwrap_or((0.0, 0, None, 0));
    let recent_unknown_ack_count = count_unknown_ack_warnings(&data_dir.0, 160) as u64;

    let maybe_profile = {
        let pm = profiles.lock().map_err(|e| e.to_string())?;
        pm.get_active_profile_id()
            .and_then(|id| pm.get_profile(&id))
    };

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
            "bot_state": bot_snapshot.0,
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
            "ack_warning_count_recent": recent_ack_warning_count,
            "avg_ack_latency_ms": avg_ack_latency_ms,
            "ack_sample_count": ack_sample_count,
            "warnings": [],
        }));
    };

    let wallet_address = profile.primary_wallet_address();
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
        tokio::time::timeout(Duration::from_secs(15), get_wallet_balance(app.clone())),
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
    let available_balance = available_balance_result.clone().ok();
    let portfolio_value = portfolio_value_result.clone().ok();
    let total_equity = match (available_balance, portfolio_value) {
        (Some(available), Some(portfolio)) => Some(available + portfolio),
        _ => None,
    };
    let (
        liquidity_rewards_today,
        liquidity_rewards_lifetime,
        liquidity_rewards_as_of_utc,
        liquidity_rewards_error,
    ) = match rewards_result {
        Ok(Ok((today, lifetime, as_of_utc))) => (today, lifetime, as_of_utc, None),
        Ok(Err(err)) => (None, None, None, Some(err)),
        Err(err) => (None, None, None, Some(err)),
    };
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
        "bot_state": bot_snapshot.0,
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
    signature_type: u8,
) -> Result<Profile, String> {
    let default_config = default_desktop_config(
        String::new(),
        proxy_wallet_address.trim().to_string(),
        signature_type,
    );
    let (
        strategy_config,
        sizing_config,
        _,
        eoa_wallet_address,
        proxy_wallet_address,
        signature_type,
    ) = desktop_config_to_profile_payload(&default_config);

    profiles
        .lock()
        .map_err(|e| e.to_string())?
        .create_profile(
            name,
            eoa_wallet_address,
            proxy_wallet_address,
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
fn start_bot(
    app: AppHandle,
    bot: State<'_, BotState>,
    profiles: State<'_, ProfileState>,
    auth: State<'_, AuthState>,
    data_dir: State<'_, AppDataDir>,
    wallet_sync: State<'_, WalletSyncState>,
    simulation: bool,
) -> Result<(), String> {
    geo_access::ensure_geo_start_allowed()?;
    let (env_path, config_path, wallet_sync_config) = {
        let pm = profiles.lock().map_err(|e| e.to_string())?;
        let profile = persist_active_profile_simulation_mode(&pm, simulation)?;
        let auth = auth.lock().map_err(|e| e.to_string())?;
        let (env_path, config_path) =
            build_runtime_paths_for_profile(&profile, &auth, &data_dir.0)?;
        (
            env_path,
            config_path,
            wallet_sync_config_for_profile(&profile),
        )
    };
    bot.lock()
        .map_err(|e| e.to_string())?
        .start(&app, env_path, config_path, simulation)?;
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
) -> Result<(), String> {
    wallet_sync.lock().map_err(|e| e.to_string())?.stop()?;
    bot.lock().map_err(|e| e.to_string())?.stop()
}

#[tauri::command]
fn restart_bot(
    app: AppHandle,
    bot: State<'_, BotState>,
    profiles: State<'_, ProfileState>,
    auth: State<'_, AuthState>,
    data_dir: State<'_, AppDataDir>,
    wallet_sync: State<'_, WalletSyncState>,
    simulation: bool,
) -> Result<(), String> {
    geo_access::ensure_geo_start_allowed()?;
    let (env_path, config_path, wallet_sync_config) = {
        let pm = profiles.lock().map_err(|e| e.to_string())?;
        let profile = persist_active_profile_simulation_mode(&pm, simulation)?;
        let auth = auth.lock().map_err(|e| e.to_string())?;
        let (env_path, config_path) =
            build_runtime_paths_for_profile(&profile, &auth, &data_dir.0)?;
        (
            env_path,
            config_path,
            wallet_sync_config_for_profile(&profile),
        )
    };
    wallet_sync.lock().map_err(|e| e.to_string())?.stop()?;
    bot.lock()
        .map_err(|e| e.to_string())?
        .restart(&app, env_path, config_path, simulation)?;
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
fn get_bot_status(bot: State<'_, BotState>) -> String {
    match bot
        .lock()
        .map(|bm| bm.get_status())
        .unwrap_or(bot_manager::BotStatus::Error("lock failed".into()))
    {
        bot_manager::BotStatus::Stopped => "stopped".to_string(),
        bot_manager::BotStatus::Starting => "starting".to_string(),
        bot_manager::BotStatus::Running => "running".to_string(),
        bot_manager::BotStatus::Stopping => "stopping".to_string(),
        bot_manager::BotStatus::Error(e) => format!("error:{e}"),
    }
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
        signature_type,
    ) = desktop_config_to_profile_payload(config);

    profile.strategy_config = merge_config_object(&profile.strategy_config, &strategy_config);
    remove_legacy_premarket_ladder_keys(&mut profile.strategy_config);
    profile.sizing_config = merge_config_object(&profile.sizing_config, &sizing_config);
    profile.eoa_wallet_address = eoa_wallet_address;
    profile.proxy_wallet_address = proxy_wallet_address;
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
    bot.lock()
        .map_err(|e| e.to_string())?
        .restart(app, env_path, config_path, simulation)?;
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

#[tauri::command]
fn save_config(
    auth: State<'_, AuthState>,
    profiles: State<'_, ProfileState>,
    data_dir: State<'_, AppDataDir>,
    profile_id: String,
    config: DesktopConfig,
) -> Result<(), String> {
    let pm = profiles.lock().map_err(|e| e.to_string())?;
    let mut profile = pm.get_profile(&profile_id).ok_or("profile not found")?;
    drop(pm);
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
        (
            manager.is_running(),
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

        let onboarding = match onboard::run_onboarding(
            config.private_key.as_str(),
            config.sig_type,
            config.proxy_wallet.as_str(),
        )
        .await
        {
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
            .remote_signer_token
            .as_ref()
            .or(onboarding.signer_token.as_ref())
            .map(|value| value.trim())
            .filter(|value| !value.is_empty());
        let onboard_order_signer_primary_token = onboarding
            .order_signer_primary_token
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty());
        let next_order_signer_primary_token_internal = distinct_order_signer_primary_token(
            onboard_remote_signer_token.unwrap_or_default(),
            onboard_order_signer_primary_token.unwrap_or_default(),
        );

        let remote_updates = [
            ("remote_signer_token", onboard_remote_signer_token),
            (
                "remote_discovery_token",
                onboarding.discovery_token.as_deref(),
            ),
            (
                "remote_premarket_alpha_token",
                onboarding.premarket_alpha_token.as_deref(),
            ),
            (
                "remote_endgame_alpha_token",
                onboarding.endgame_alpha_token.as_deref(),
            ),
            (
                "remote_mm_rewards_alpha_token",
                onboarding.mm_rewards_alpha_token.as_deref(),
            ),
            (
                "remote_evsnipe_discovery_token",
                onboarding.evsnipe_discovery_token.as_deref(),
            ),
            ("admin_api_token", onboarding.admin_api_token.as_deref()),
        ];
        for (key, value) in remote_updates {
            let value = value
                .map(|entry| entry.trim())
                .filter(|entry| !entry.is_empty());
            match key {
                "remote_signer_token" => {
                    if let Some(value) = value {
                        if config.remote_signer_token.trim() != value {
                            config.remote_signer_token = value.to_string();
                            config_changed = true;
                            fixed_keys.push(key.to_string());
                        }
                        if config.order_signer_primary_token_internal
                            != next_order_signer_primary_token_internal
                        {
                            config.order_signer_primary_token_internal =
                                next_order_signer_primary_token_internal.clone();
                            config_changed = true;
                        }
                    }
                }
                "remote_discovery_token" => {
                    if let Some(value) = value {
                        if config.remote_discovery_token.trim() != value {
                            config.remote_discovery_token = value.to_string();
                            config_changed = true;
                            fixed_keys.push(key.to_string());
                        }
                    }
                }
                "remote_premarket_alpha_token" => {
                    if let Some(value) = value {
                        if config.remote_premarket_alpha_token.trim() != value {
                            config.remote_premarket_alpha_token = value.to_string();
                            config_changed = true;
                            fixed_keys.push(key.to_string());
                        }
                    }
                }
                "remote_endgame_alpha_token" => {
                    if let Some(value) = value {
                        if config.remote_endgame_alpha_token.trim() != value {
                            config.remote_endgame_alpha_token = value.to_string();
                            config_changed = true;
                            fixed_keys.push(key.to_string());
                        }
                    }
                }
                "remote_mm_rewards_alpha_token" => {
                    if let Some(value) = value {
                        if config.remote_mm_rewards_alpha_token.trim() != value {
                            config.remote_mm_rewards_alpha_token = value.to_string();
                            config_changed = true;
                            fixed_keys.push(key.to_string());
                        }
                    }
                }
                "remote_evsnipe_discovery_token" => {
                    if let Some(value) = value {
                        if config.remote_evsnipe_discovery_token.trim() != value {
                            config.remote_evsnipe_discovery_token = value.to_string();
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
    let pm = profiles.lock().map_err(|e| e.to_string())?;
    let mut created = pm
        .create_profile(
            imported.profile.name,
            imported.profile.eoa_wallet_address,
            imported.profile.proxy_wallet_address,
            imported.profile.signature_type,
            imported.profile.strategy_config.clone(),
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

#[tauri::command]
fn get_trade_stats(data_dir: State<'_, AppDataDir>) -> serde_json::Value {
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
    let db_path = resolve_tracking_db_path(&data_dir.0);

    let conn = match Connection::open(&db_path) {
        Ok(c) => c,
        Err(_) => return empty.clone(),
    };

    let total_trades: i64 = conn
        .query_row("SELECT COUNT(*) FROM fills_v2", [], |row| row.get(0))
        .unwrap_or(0);
    let total_pnl: f64 = conn
        .query_row(
            "SELECT COALESCE(SUM(realized_pnl_usd), 0.0) FROM positions_v2",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0.0);
    let (winning_trades, losing_trades): (i64, i64) = conn
        .query_row(
            "SELECT \
                COALESCE(SUM(CASE WHEN COALESCE(pnl_usd, 0.0) > 0 THEN 1 ELSE 0 END), 0), \
                COALESCE(SUM(CASE WHEN COALESCE(pnl_usd, 0.0) < 0 THEN 1 ELSE 0 END), 0) \
             FROM trade_events WHERE event_type='EXIT'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap_or((0, 0));
    let denominator = (winning_trades + losing_trades) as f64;
    let win_rate = if denominator > 0.0 {
        (winning_trades as f64 / denominator) * 100.0
    } else {
        0.0
    };
    let (ack_sample_count, avg_ack_latency_ms, _) = query_ack_latency_summary(&conn);

    let mut pnl_history = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT ((ts_ms / 3600000) * 3600000) AS bucket_ms, \
                COALESCE(SUM(COALESCE(pnl_usd, 0.0)), 0.0) AS pnl_delta \
         FROM trade_events \
         WHERE event_type='EXIT' \
         GROUP BY bucket_ms \
         ORDER BY bucket_ms ASC",
    ) {
        if let Ok(rows) = stmt.query_map([], |row| {
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
    tokio::time::timeout(
        Duration::from_secs(15),
        wallet_rpc::fetch_usdc_balance_with_fallback(&config_io::DESKTOP_POLYGON_RPC_URLS, &wallet),
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
    data_dir: State<'_, AppDataDir>,
) -> Result<serde_json::Value, String> {
    build_home_overview_payload(
        app,
        bot,
        auth,
        profiles,
        wallet_sync,
        liquidity_rewards,
        data_dir,
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

#[tauri::command]
async fn run_onboarding(
    data_dir: State<'_, AppDataDir>,
    private_key: String,
    signature_type: u8,
    proxy_wallet: String,
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

    let result = onboard::run_onboarding(&private_key, signature_type, &proxy_wallet)
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
            "run_onboarding success signer_token_set={} discovery_token_set={} premarket_alpha_token_set={}",
            result.remote_signer_token.as_ref().map(|v| !v.is_empty()).unwrap_or(false),
            result.discovery_token.as_ref().map(|v| !v.is_empty()).unwrap_or(false),
            result.premarket_alpha_token.as_ref().map(|v| !v.is_empty()).unwrap_or(false)
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
        .setup(|app| {
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
                            || -> Option<(PathBuf, PathBuf, bool, WalletSyncRuntimeConfig)> {
                                let pm = ps.lock().ok()?;
                                let auth = auth.lock().ok()?;
                                let profile = active_profile(&pm).ok()?;
                                let simulation = simulation_mode_from_profile(&profile);
                                let (env_path, config_path) =
                                    build_runtime_paths_for_profile(&profile, &auth, &dd.0).ok()?;
                                Some((
                                    env_path,
                                    config_path,
                                    simulation,
                                    wallet_sync_config_for_profile(&profile),
                                ))
                            };
                        if let Some((env_path, config_path, simulation, wallet_sync_config)) =
                            configs()
                        {
                            if geo_access::ensure_geo_start_allowed().is_err() {
                                return;
                            }
                            if let Ok(bm) = bs.lock() {
                                if bm.start(app, env_path, config_path, simulation).is_ok() {
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
            get_recent_trades,
            get_open_positions,
            get_wallet_balance,
            get_home_overview,
            get_home_activity,
            get_wallet_sync_status,
            get_geo_access_status,
            run_wallet_sync_now,
            get_data_dir_path,
            open_logs_folder,
            run_onboarding,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{
        default_desktop_config, desktop_config_to_profile_payload, infer_premarket_ladder_mode_5m,
        infer_premarket_ladder_mode_non_m5, merge_config_object, merge_desktop_secrets,
        premarket_default_ladder_prices_m5, premarket_default_ladder_prices_non_m5,
        premarket_default_ladder_weights, premarket_ladder_prices_for_mode,
        remove_legacy_premarket_ladder_keys, simulation_mode_from_profile,
        PREMARKET_LADDER_MODE_ENV_KEY_5M, PREMARKET_LADDER_MODE_ENV_KEY_NON_M5,
        PREMARKET_LADDER_MODE_ENV_KEY_NON_M5_LEGACY, PREMARKET_LADDER_MODE_ENV_KEY_SHARED,
        WEEKEND_POLICY_ENV_KEY,
    };
    use crate::{config_io, profile_manager::Profile};
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
    fn merge_desktop_secrets_clears_blank_managed_fields() {
        let existing = HashMap::from([
            ("POLY_PRIVATE_KEY".to_string(), "old-private".to_string()),
            ("RELAYER_API_KEY".to_string(), "old-relayer".to_string()),
            (
                "EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN".to_string(),
                "old-signer".to_string(),
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
    }

    #[test]
    fn desktop_profile_payload_falls_back_primary_token_to_remote_signer() {
        let mut config = default_desktop_config(
            "0x1111111111111111111111111111111111111111".to_string(),
            "0x2222222222222222222222222222222222222222".to_string(),
            1,
        );
        config.remote_signer_token = "remote-token".to_string();

        let (_, _, secrets, _, _, _) = desktop_config_to_profile_payload(&config);

        assert_eq!(
            secrets.get("EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN"),
            Some(&"remote-token".to_string())
        );
        assert_eq!(
            secrets.get("EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN"),
            Some(&"remote-token".to_string())
        );
    }

    #[test]
    fn desktop_profile_payload_preserves_distinct_internal_primary_token() {
        let mut config = default_desktop_config(
            "0x1111111111111111111111111111111111111111".to_string(),
            "0x2222222222222222222222222222222222222222".to_string(),
            1,
        );
        config.remote_signer_token = "remote-token".to_string();
        config.order_signer_primary_token_internal = "primary-token".to_string();

        let (_, _, secrets, _, _, _) = desktop_config_to_profile_payload(&config);

        assert_eq!(
            secrets.get("EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN"),
            Some(&"remote-token".to_string())
        );
        assert_eq!(
            secrets.get("EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN"),
            Some(&"primary-token".to_string())
        );
    }

    #[test]
    fn desktop_profile_payload_writes_premarket_ladder_mode() {
        let mut config = default_desktop_config(
            "0x1111111111111111111111111111111111111111".to_string(),
            "0x2222222222222222222222222222222222222222".to_string(),
            1,
        );
        config.strategy_settings.premarket.timeframes = vec!["15m".to_string(), "1h".to_string()];
        config.strategy_settings.premarket.entry_ladder_mode_5m = "safe".to_string();
        config.strategy_settings.premarket.entry_ladder_mode_non_m5 = "aggressive".to_string();

        let (strategy, _, _, _, _, _) = desktop_config_to_profile_payload(&config);
        let strategy = strategy.as_object().expect("strategy object");

        assert_eq!(
            strategy.get(PREMARKET_LADDER_MODE_ENV_KEY_5M),
            Some(&serde_json::json!("safe"))
        );
        assert_eq!(
            strategy.get(PREMARKET_LADDER_MODE_ENV_KEY_NON_M5),
            Some(&serde_json::json!("aggressive"))
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

        let (strategy, _, _, _, _, _) = desktop_config_to_profile_payload(&config);
        let strategy = strategy.as_object().expect("strategy object");

        assert_eq!(
            strategy.get(WEEKEND_POLICY_ENV_KEY),
            Some(&serde_json::json!("pause"))
        );
    }

    #[test]
    fn remove_legacy_premarket_ladder_keys_clears_old_csv_fields() {
        let mut strategy_config = serde_json::json!({
            PREMARKET_LADDER_MODE_ENV_KEY_SHARED: "safe",
            PREMARKET_LADDER_MODE_ENV_KEY_NON_M5_LEGACY: "safe",
            "EVPOLY_PREMARKET_FIXED_LADDER_PRICES": "0.99,0.99,0.99,0.99,0.99,0.99",
            "EVPOLY_PREMARKET_FIXED_LADDER_WEIGHTS": "1,0,0,0,0,0",
            PREMARKET_LADDER_MODE_ENV_KEY_5M: "aggressive",
            PREMARKET_LADDER_MODE_ENV_KEY_NON_M5: "normal"
        });

        remove_legacy_premarket_ladder_keys(&mut strategy_config);

        let strategy = strategy_config.as_object().expect("strategy object");
        assert!(!strategy.contains_key(PREMARKET_LADDER_MODE_ENV_KEY_SHARED));
        assert!(!strategy.contains_key(PREMARKET_LADDER_MODE_ENV_KEY_NON_M5_LEGACY));
        assert!(!strategy.contains_key("EVPOLY_PREMARKET_FIXED_LADDER_PRICES"));
        assert!(!strategy.contains_key("EVPOLY_PREMARKET_FIXED_LADDER_WEIGHTS"));
        assert_eq!(
            strategy.get(PREMARKET_LADDER_MODE_ENV_KEY_5M),
            Some(&serde_json::json!("aggressive"))
        );
        assert_eq!(
            strategy.get(PREMARKET_LADDER_MODE_ENV_KEY_NON_M5),
            Some(&serde_json::json!("normal"))
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
    fn infer_premarket_ladder_modes_read_split_envs_with_shared_fallback() {
        let mut strategy = serde_json::Map::new();
        strategy.insert(
            PREMARKET_LADDER_MODE_ENV_KEY_5M.to_string(),
            serde_json::json!("safe"),
        );
        strategy.insert(
            PREMARKET_LADDER_MODE_ENV_KEY_NON_M5.to_string(),
            serde_json::json!("aggressive"),
        );
        assert_eq!(infer_premarket_ladder_mode_5m(&strategy), "safe");
        assert_eq!(infer_premarket_ladder_mode_non_m5(&strategy), "aggressive");

        let mut legacy_non_m5 = serde_json::Map::new();
        legacy_non_m5.insert(
            PREMARKET_LADDER_MODE_ENV_KEY_NON_M5_LEGACY.to_string(),
            serde_json::json!("safe"),
        );
        assert_eq!(infer_premarket_ladder_mode_non_m5(&legacy_non_m5), "safe");

        let mut shared_only = serde_json::Map::new();
        shared_only.insert(
            PREMARKET_LADDER_MODE_ENV_KEY_SHARED.to_string(),
            serde_json::json!("safe"),
        );
        assert_eq!(infer_premarket_ladder_mode_5m(&shared_only), "safe");
        assert_eq!(infer_premarket_ladder_mode_non_m5(&shared_only), "safe");

        let missing = serde_json::Map::new();
        assert_eq!(infer_premarket_ladder_mode_5m(&missing), "normal");
        assert_eq!(infer_premarket_ladder_mode_non_m5(&missing), "normal");

        let safe_m5 =
            premarket_ladder_prices_for_mode("safe", &premarket_default_ladder_prices_m5());
        assert_eq!(safe_m5, vec![0.28, 0.24, 0.20, 0.15, 0.09, 0.03]);
        let aggressive_non_m5 = premarket_ladder_prices_for_mode(
            "aggressive",
            &premarket_default_ladder_prices_non_m5(),
        );
        assert_eq!(aggressive_non_m5, vec![0.44, 0.33, 0.27, 0.20, 0.14, 0.07]);
        assert_eq!(
            premarket_default_ladder_weights(),
            vec![0.23, 0.23, 0.17, 0.14, 0.12, 0.11]
        );
    }
}
