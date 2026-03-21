use crate::api::PolymarketApi;
use crate::coinbase_ws::SharedCoinbaseBookState;
use crate::config::Config;
use crate::event_log::log_event;
use crate::signal_state::SharedSignalState;
use crate::symbol_ownership;
use crate::strategy::{
    STRATEGY_ID_ENDGAME_SWEEP_V1, STRATEGY_ID_EVCURVE_V1, STRATEGY_ID_EVSNIPE_V1,
    STRATEGY_ID_MM_REWARDS_V1, STRATEGY_ID_MM_SPORT_V1, STRATEGY_ID_PREMARKET_V1,
    STRATEGY_ID_SESSIONBAND_V1,
};
use crate::ui_contracts::{UiDashboardSummary, UiStrategyState};
use crate::trader::Trader;
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

#[derive(Clone)]
pub struct BotAdminContext {
    pub config: Arc<Config>,
    pub api: Arc<PolymarketApi>,
    pub trader: Arc<Trader>,
    pub signal_state: SharedSignalState,
    pub coinbase_state: SharedCoinbaseBookState,
    pub simulation_mode: bool,
}

impl BotAdminContext {
    pub fn new(
        config: Arc<Config>,
        api: Arc<PolymarketApi>,
        trader: Arc<Trader>,
        signal_state: SharedSignalState,
        coinbase_state: SharedCoinbaseBookState,
        simulation_mode: bool,
    ) -> Self {
        Self {
            config,
            api,
            trader,
            signal_state,
            coinbase_state,
            simulation_mode,
        }
    }
}

#[derive(Debug)]
pub struct BotHttpResponse {
    pub status_code: u16,
    pub status_text: &'static str,
    pub body: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BotSettingType {
    Bool,
    Integer,
    Number,
    String,
    Enum,
}

impl BotSettingType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::String => "string",
            Self::Enum => "enum",
        }
    }
}

#[derive(Debug, Clone)]
struct BotSettingSpec {
    key: &'static str,
    group: &'static str,
    strategy: Option<&'static str>,
    value_type: BotSettingType,
    default_raw: &'static str,
    min: Option<f64>,
    max: Option<f64>,
    enum_values: &'static [&'static str],
    mutable: bool,
    restart_required: bool,
    config_fallback_key: Option<&'static str>,
    description: &'static str,
}

#[derive(Debug, Clone)]
struct ResolvedSettingValue {
    effective_value: Value,
    source: &'static str,
    default_value: Value,
    parse_error: Option<String>,
    raw_env_value: Option<String>,
}

#[derive(Debug, Clone)]
struct DoctorIssue {
    id: String,
    severity: &'static str,
    key: Option<String>,
    summary: String,
    recommended_fix: Option<Value>,
    auto_fixable: bool,
    details: Value,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct BotSetRequest {
    changes: Vec<BotSetChange>,
    persist: Option<bool>,
    env_file: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BotSetChange {
    key: String,
    value: Value,
    persist: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct BotAuditFixRequest {
    issue_ids: Option<Vec<String>>,
    persist: Option<bool>,
    env_file: Option<String>,
    run_onboard_if_needed: Option<bool>,
    onboard_api_base: Option<String>,
    onboard_skip_approvals: Option<bool>,
    onboard_skip_retention_cron: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct BotResetRequest {
    scope: Option<String>,
    key: Option<String>,
    group: Option<String>,
    strategy: Option<String>,
    confirm: Option<bool>,
    persist: Option<bool>,
    env_file: Option<String>,
}

static ENV_FILE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn env_file_lock() -> &'static Mutex<()> {
    ENV_FILE_LOCK.get_or_init(|| Mutex::new(()))
}

fn setting_specs() -> Vec<BotSettingSpec> {
    vec![
        // Speed
        BotSettingSpec {
            key: "POLY_CHECK_INTERVAL_MS",
            group: "speed",
            strategy: None,
            value_type: BotSettingType::Integer,
            default_raw: "1000",
            min: Some(100.0),
            max: Some(120_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: Some("trading.check_interval_ms"),
            description: "Core monitor polling interval in ms.",
        },
        BotSettingSpec {
            key: "EVPOLY_SIGNAL_LIVENESS_CHECK_SEC",
            group: "speed",
            strategy: None,
            value_type: BotSettingType::Integer,
            default_raw: "20",
            min: Some(5.0),
            max: Some(600.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Signal liveness watchdog interval in seconds.",
        },
        BotSettingSpec {
            key: "EVPOLY_SIGNAL_LIVENESS_STALE_MS",
            group: "speed",
            strategy: None,
            value_type: BotSettingType::Integer,
            default_raw: "180000",
            min: Some(15_000.0),
            max: Some(3_600_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Signal freshness threshold in milliseconds.",
        },
        BotSettingSpec {
            key: "EVPOLY_PENDING_ORDERS_PRUNE_INTERVAL_SEC",
            group: "speed",
            strategy: None,
            value_type: BotSettingType::Integer,
            default_raw: "300",
            min: Some(30.0),
            max: Some(86_400.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Pending-order prune interval in seconds.",
        },
        BotSettingSpec {
            key: "EVPOLY_ENTRY_WORKER_COUNT_NORMAL",
            group: "speed",
            strategy: None,
            value_type: BotSettingType::Integer,
            default_raw: "4",
            min: Some(1.0),
            max: Some(128.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Normal entry worker pool size.",
        },
        BotSettingSpec {
            key: "EVPOLY_ENTRY_WORKER_COUNT_ENDGAME",
            group: "speed",
            strategy: Some("endgame"),
            value_type: BotSettingType::Integer,
            default_raw: "2",
            min: Some(1.0),
            max: Some(128.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Endgame worker pool size.",
        },
        BotSettingSpec {
            key: "EVPOLY_ENTRY_WORKER_COUNT_EVCURVE",
            group: "speed",
            strategy: Some("evcurve"),
            value_type: BotSettingType::Integer,
            default_raw: "4",
            min: Some(1.0),
            max: Some(128.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "EVcurve worker pool size.",
        },
        BotSettingSpec {
            key: "EVPOLY_ENTRY_WORKER_COUNT_EVSNIPE",
            group: "speed",
            strategy: Some("evsnipe"),
            value_type: BotSettingType::Integer,
            default_raw: "2",
            min: Some(1.0),
            max: Some(128.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "EVSnipe worker pool size.",
        },
        BotSettingSpec {
            key: "EVPOLY_ENTRY_SUBMIT_CONCURRENCY",
            group: "speed",
            strategy: None,
            value_type: BotSettingType::Integer,
            default_raw: "4",
            min: Some(1.0),
            max: Some(256.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Global submit concurrency fallback.",
        },
        BotSettingSpec {
            key: "EVPOLY_ENDGAME_POLL_MS",
            group: "speed",
            strategy: Some("endgame"),
            value_type: BotSettingType::Integer,
            default_raw: "500",
            min: Some(20.0),
            max: Some(60_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Endgame scheduler poll cadence in ms.",
        },
        BotSettingSpec {
            key: "EVPOLY_ENDGAME_SAFETY_POLL_MS",
            group: "speed",
            strategy: Some("endgame"),
            value_type: BotSettingType::Integer,
            default_raw: "500",
            min: Some(100.0),
            max: Some(60_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Endgame safety wake-up poll in ms.",
        },
        BotSettingSpec {
            key: "EVPOLY_EVCURVE_POLL_MS",
            group: "speed",
            strategy: Some("evcurve"),
            value_type: BotSettingType::Integer,
            default_raw: "250",
            min: Some(100.0),
            max: Some(60_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "EVcurve polling cadence in ms.",
        },
        BotSettingSpec {
            key: "EVPOLY_SESSIONBAND_POLL_MS",
            group: "speed",
            strategy: Some("sessionband"),
            value_type: BotSettingType::Integer,
            default_raw: "250",
            min: Some(100.0),
            max: Some(60_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "SessionBand polling cadence in ms.",
        },
        BotSettingSpec {
            key: "EVPOLY_EVSNIPE_DISCOVERY_REFRESH_SEC",
            group: "speed",
            strategy: Some("evsnipe"),
            value_type: BotSettingType::Integer,
            default_raw: "30",
            min: Some(5.0),
            max: Some(86_400.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "EVSnipe market discovery refresh cadence in seconds.",
        },
        BotSettingSpec {
            key: "EVPOLY_MM_POLL_MS",
            group: "speed",
            strategy: Some("mm"),
            value_type: BotSettingType::Integer,
            default_raw: "1000",
            min: Some(200.0),
            max: Some(60_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "MM quote loop polling cadence in ms.",
        },
        BotSettingSpec {
            key: "EVPOLY_MM_REPRICE_MIN_INTERVAL_MS",
            group: "speed",
            strategy: Some("mm"),
            value_type: BotSettingType::Integer,
            default_raw: "300",
            min: Some(50.0),
            max: Some(30_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Minimum MM reprice interval in ms.",
        },
        BotSettingSpec {
            key: "EVPOLY_MM_EVENT_FALLBACK_POLL_MS",
            group: "speed",
            strategy: Some("mm"),
            value_type: BotSettingType::Integer,
            default_raw: "1000",
            min: Some(50.0),
            max: Some(30_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "MM event fallback polling interval in ms.",
        },
        // Strategy toggles
        BotSettingSpec {
            key: "EVPOLY_STRATEGY_PREMARKET_ENABLE",
            group: "strategy",
            strategy: Some("premarket"),
            value_type: BotSettingType::Bool,
            default_raw: "true",
            min: None,
            max: None,
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Enable deterministic premarket strategy scheduler.",
        },
        BotSettingSpec {
            key: "EVPOLY_STRATEGY_ENDGAME_ENABLE",
            group: "strategy",
            strategy: Some("endgame"),
            value_type: BotSettingType::Bool,
            default_raw: "true",
            min: None,
            max: None,
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Enable endgame sweep strategy.",
        },
        BotSettingSpec {
            key: "EVPOLY_STRATEGY_EVCURVE_ENABLE",
            group: "strategy",
            strategy: Some("evcurve"),
            value_type: BotSettingType::Bool,
            default_raw: "true",
            min: None,
            max: None,
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Enable EVcurve strategy.",
        },
        BotSettingSpec {
            key: "EVPOLY_STRATEGY_SESSIONBAND_ENABLE",
            group: "strategy",
            strategy: Some("sessionband"),
            value_type: BotSettingType::Bool,
            default_raw: "true",
            min: None,
            max: None,
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Enable SessionBand strategy.",
        },
        BotSettingSpec {
            key: "EVPOLY_STRATEGY_EVSNIPE_ENABLE",
            group: "strategy",
            strategy: Some("evsnipe"),
            value_type: BotSettingType::Bool,
            default_raw: "true",
            min: None,
            max: None,
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Enable EVSnipe strategy.",
        },
        BotSettingSpec {
            key: "EVPOLY_STRATEGY_MM_REWARDS_ENABLE",
            group: "strategy",
            strategy: Some("mm"),
            value_type: BotSettingType::Bool,
            default_raw: "false",
            min: None,
            max: None,
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Enable MM rewards strategy.",
        },
        BotSettingSpec {
            key: "EVPOLY_STRATEGY_MM_SPORT_ENABLE",
            group: "strategy",
            strategy: Some("mm_sport"),
            value_type: BotSettingType::Bool,
            default_raw: "false",
            min: None,
            max: None,
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Enable MM Sport strategy.",
        },
        // Risk
        BotSettingSpec {
            key: "EVPOLY_MAX_ENTRIES_PER_TIMEFRAME_PERIOD",
            group: "risk",
            strategy: None,
            value_type: BotSettingType::Integer,
            default_raw: "2",
            min: Some(1.0),
            max: Some(10_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Global entry cap per timeframe period.",
        },
        BotSettingSpec {
            key: "EVPOLY_MAX_SUBMITS_PER_TOKEN_PERIOD",
            group: "risk",
            strategy: None,
            value_type: BotSettingType::Integer,
            default_raw: "5",
            min: Some(1.0),
            max: Some(10_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Global submit cap per token per period.",
        },
        BotSettingSpec {
            key: "EVPOLY_MAX_ENDGAME_ENTRIES_PER_TIMEFRAME_PERIOD",
            group: "risk",
            strategy: Some("endgame"),
            value_type: BotSettingType::Integer,
            default_raw: "5",
            min: Some(1.0),
            max: Some(10_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Endgame entry cap per timeframe period.",
        },
        BotSettingSpec {
            key: "EVPOLY_MAX_ENDGAME_SUBMITS_PER_TOKEN_PERIOD",
            group: "risk",
            strategy: Some("endgame"),
            value_type: BotSettingType::Integer,
            default_raw: "10",
            min: Some(1.0),
            max: Some(10_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Endgame submit cap per token per period.",
        },
        BotSettingSpec {
            key: "EVPOLY_ENDGAME_PER_PERIOD_CAP_USD",
            group: "risk",
            strategy: Some("endgame"),
            value_type: BotSettingType::Number,
            default_raw: "250",
            min: Some(1.0),
            max: Some(10_000_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Endgame per-period notional cap.",
        },
        BotSettingSpec {
            key: "EVPOLY_EVCURVE_STRATEGY_CAP_USD",
            group: "risk",
            strategy: Some("evcurve"),
            value_type: BotSettingType::Number,
            default_raw: "10000",
            min: Some(1.0),
            max: Some(10_000_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "EVcurve total strategy cap.",
        },
        BotSettingSpec {
            key: "EVPOLY_SESSIONBAND_STRATEGY_CAP_USD",
            group: "risk",
            strategy: Some("sessionband"),
            value_type: BotSettingType::Number,
            default_raw: "2000",
            min: Some(1.0),
            max: Some(10_000_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "SessionBand total strategy cap.",
        },
        BotSettingSpec {
            key: "EVPOLY_EVSNIPE_STRATEGY_CAP_USD",
            group: "risk",
            strategy: Some("evsnipe"),
            value_type: BotSettingType::Number,
            default_raw: "2000",
            min: Some(1.0),
            max: Some(10_000_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "EVSnipe total strategy cap.",
        },
        BotSettingSpec {
            key: "EVPOLY_MM_HARD_DISABLE",
            group: "risk",
            strategy: Some("mm"),
            value_type: BotSettingType::Bool,
            default_raw: "true",
            min: None,
            max: None,
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Hard-disable gate for MM runtime.",
        },
        BotSettingSpec {
            key: "EVPOLY_MM_MIN_PRICE",
            group: "risk",
            strategy: Some("mm"),
            value_type: BotSettingType::Number,
            default_raw: "0.01",
            min: Some(0.01),
            max: Some(0.98),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "MM minimum quote price.",
        },
        BotSettingSpec {
            key: "EVPOLY_MM_MAX_PRICE",
            group: "risk",
            strategy: Some("mm"),
            value_type: BotSettingType::Number,
            default_raw: "0.99",
            min: Some(0.02),
            max: Some(0.99),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "MM maximum quote price.",
        },
        BotSettingSpec {
            key: "EVPOLY_MM_AUTO_RANK_BUDGET_USD",
            group: "risk",
            strategy: Some("mm"),
            value_type: BotSettingType::Number,
            default_raw: "2000",
            min: Some(100.0),
            max: Some(1_000_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "MM auto-mode rank budget in USD.",
        },
        BotSettingSpec {
            key: "EVPOLY_MM_MAX_SHARES_PER_ORDER",
            group: "risk",
            strategy: Some("mm"),
            value_type: BotSettingType::Number,
            default_raw: "5000",
            min: Some(1.0),
            max: Some(10_000_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "MM max shares per order.",
        },
        BotSettingSpec {
            key: "EVPOLY_PREMARKET_ACTIVE_CAP_PER_ASSET",
            group: "risk",
            strategy: Some("premarket"),
            value_type: BotSettingType::Integer,
            default_raw: "20",
            min: Some(1.0),
            max: Some(10_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Premarket active entry cap per asset.",
        },
        BotSettingSpec {
            key: "EVPOLY_PREMARKET_SUBMIT_CAP_PER_TOKEN",
            group: "risk",
            strategy: Some("premarket"),
            value_type: BotSettingType::Integer,
            default_raw: "20",
            min: Some(1.0),
            max: Some(10_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Premarket submit cap per token.",
        },
        // Strategy-detail knobs requested explicitly
        BotSettingSpec {
            key: "EVPOLY_PREMARKET_BASE_SIZE_USD",
            group: "strategy",
            strategy: Some("premarket"),
            value_type: BotSettingType::Number,
            default_raw: "10",
            min: Some(1.0),
            max: Some(1_000_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Premarket base size in USD before symbol/timeframe multipliers.",
        },
        BotSettingSpec {
            key: "EVPOLY_ENDGAME_BASE_SIZE_USD",
            group: "strategy",
            strategy: Some("endgame"),
            value_type: BotSettingType::Number,
            default_raw: "10",
            min: Some(1.0),
            max: Some(1_000_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Endgame base size in USD before symbol and hard tick multipliers.",
        },
        BotSettingSpec {
            key: "EVPOLY_EVCURVE_BASE_SIZE_USD",
            group: "strategy",
            strategy: Some("evcurve"),
            value_type: BotSettingType::Number,
            default_raw: "10",
            min: Some(1.0),
            max: Some(1_000_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "EVcurve base size in USD before symbol/timeframe multipliers.",
        },
        BotSettingSpec {
            key: "EVPOLY_SESSIONBAND_BASE_SIZE_USD",
            group: "strategy",
            strategy: Some("sessionband"),
            value_type: BotSettingType::Number,
            default_raw: "10",
            min: Some(1.0),
            max: Some(1_000_000.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "SessionBand base size in USD before symbol and tau multipliers.",
        },
        BotSettingSpec {
            key: "EVPOLY_ENDGAME_TIMEFRAMES",
            group: "strategy",
            strategy: Some("endgame"),
            value_type: BotSettingType::String,
            default_raw: "5m,15m",
            min: None,
            max: None,
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Endgame enabled timeframe CSV.",
        },
        BotSettingSpec {
            key: "EVPOLY_ENDGAME_SYMBOLS",
            group: "strategy",
            strategy: Some("endgame"),
            value_type: BotSettingType::String,
            default_raw: "BTC,ETH,SOL,XRP,DOGE,BNB,HYPE",
            min: None,
            max: None,
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "Endgame enabled symbols CSV.",
        },
        BotSettingSpec {
            key: "EVPOLY_EVCURVE_SYMBOLS",
            group: "strategy",
            strategy: Some("evcurve"),
            value_type: BotSettingType::String,
            default_raw: "BTC,ETH,SOL,XRP,DOGE,BNB,HYPE",
            min: None,
            max: None,
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "EVcurve enabled symbols CSV.",
        },
        BotSettingSpec {
            key: "EVPOLY_EVCURVE_TIMEFRAMES",
            group: "strategy",
            strategy: Some("evcurve"),
            value_type: BotSettingType::String,
            default_raw: "5m,15m,1h,4h",
            min: None,
            max: None,
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "EVcurve enabled timeframe CSV.",
        },
        BotSettingSpec {
            key: "EVPOLY_EVSNIPE_SYMBOLS",
            group: "strategy",
            strategy: Some("evsnipe"),
            value_type: BotSettingType::String,
            default_raw: "BTC,ETH,SOL,XRP,DOGE,BNB,HYPE",
            min: None,
            max: None,
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "EVSnipe enabled symbols CSV.",
        },
        BotSettingSpec {
            key: "EVPOLY_MM_MARKET_MODE",
            group: "strategy",
            strategy: Some("mm"),
            value_type: BotSettingType::Enum,
            default_raw: "target",
            min: None,
            max: None,
            enum_values: &["target", "single", "auto"],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "MM market selection mode.",
        },
    ]
}

fn strategy_catalog() -> Vec<(&'static str, &'static str, &'static [&'static str])> {
    vec![
        (
            "premarket",
            STRATEGY_ID_PREMARKET_V1,
            &["premarket", "premarket_v1"],
        ),
        (
            "endgame",
            STRATEGY_ID_ENDGAME_SWEEP_V1,
            &["endgame", "endgame_sweep_v1"],
        ),
        (
            "evcurve",
            STRATEGY_ID_EVCURVE_V1,
            &["evcurve", "evcurve_v1"],
        ),
        (
            "evsnipe",
            STRATEGY_ID_EVSNIPE_V1,
            &["evsnipe", "evsnipe_v1"],
        ),
        (
            "mm",
            STRATEGY_ID_MM_REWARDS_V1,
            &["mm", "mm_rewards", "mm_rewards_v1"],
        ),
        (
            "mm_sport",
            STRATEGY_ID_MM_SPORT_V1,
            &["mm_sport", "mm_sport_v1", "sport", "mmsport"],
        ),
        (
            "sessionband",
            STRATEGY_ID_SESSIONBAND_V1,
            &["sessionband", "sessionband_v1"],
        ),
    ]
}

fn normalize_strategy_slug(raw: &str) -> Option<&'static str> {
    let normalized = raw.trim().trim_matches('/').to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    strategy_catalog()
        .into_iter()
        .find(|(slug, _, aliases)| {
            *slug == normalized
                || aliases
                    .iter()
                    .any(|alias| alias.to_ascii_lowercase() == normalized)
        })
        .map(|(slug, _, _)| slug)
}

fn strategy_enable_default(strategy_slug: &str) -> bool {
    match strategy_slug {
        "premarket" => true,
        _ => false,
    }
}

fn strategy_enable_key(strategy_slug: &str) -> Option<&'static str> {
    match strategy_slug {
        "premarket" => Some("EVPOLY_STRATEGY_PREMARKET_ENABLE"),
        "endgame" => Some("EVPOLY_STRATEGY_ENDGAME_ENABLE"),
        "evcurve" => Some("EVPOLY_STRATEGY_EVCURVE_ENABLE"),
        "sessionband" => Some("EVPOLY_STRATEGY_SESSIONBAND_ENABLE"),
        "evsnipe" => Some("EVPOLY_STRATEGY_EVSNIPE_ENABLE"),
        "mm" => Some("EVPOLY_STRATEGY_MM_REWARDS_ENABLE"),
        "mm_sport" => Some("EVPOLY_STRATEGY_MM_SPORT_ENABLE"),
        _ => None,
    }
}

fn parse_bool_raw(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn config_value_for_key(config: &Config, config_key: &str) -> Option<Value> {
    match config_key {
        "trading.check_interval_ms" => Some(json!(config.trading.check_interval_ms)),
        _ => None,
    }
}

fn default_config_value_for_key(config_key: &str) -> Option<Value> {
    let default_cfg = Config::default();
    config_value_for_key(&default_cfg, config_key)
}

fn parse_raw_to_value(spec: &BotSettingSpec, raw: &str) -> Result<Value> {
    let trimmed = raw.trim();
    let mut parsed = match spec.value_type {
        BotSettingType::Bool => {
            let value = parse_bool_raw(trimmed)
                .ok_or_else(|| anyhow!("invalid bool value '{}'", trimmed))?;
            json!(value)
        }
        BotSettingType::Integer => {
            let value = trimmed
                .parse::<i64>()
                .with_context(|| format!("invalid integer value '{}'", trimmed))?;
            json!(value)
        }
        BotSettingType::Number => {
            let value = trimmed
                .parse::<f64>()
                .with_context(|| format!("invalid number value '{}'", trimmed))?;
            if !value.is_finite() {
                return Err(anyhow!("number must be finite"));
            }
            json!(value)
        }
        BotSettingType::String => json!(trimmed),
        BotSettingType::Enum => {
            let value = trimmed.to_ascii_lowercase();
            if !spec
                .enum_values
                .iter()
                .any(|candidate| candidate.to_ascii_lowercase() == value)
            {
                return Err(anyhow!(
                    "value '{}' must be one of [{}]",
                    trimmed,
                    spec.enum_values.join(",")
                ));
            }
            json!(trimmed)
        }
    };

    if let Some(min) = spec.min {
        let value = parsed
            .as_f64()
            .or_else(|| parsed.as_i64().map(|v| v as f64))
            .ok_or_else(|| anyhow!("min constraint applies to numeric values only"))?;
        if value < min {
            return Err(anyhow!("value {} is below minimum {}", value, min));
        }
    }
    if let Some(max) = spec.max {
        let value = parsed
            .as_f64()
            .or_else(|| parsed.as_i64().map(|v| v as f64))
            .ok_or_else(|| anyhow!("max constraint applies to numeric values only"))?;
        if value > max {
            return Err(anyhow!("value {} is above maximum {}", value, max));
        }
    }

    // Normalize enum to configured case (first matching enum value) for canonical output.
    if matches!(spec.value_type, BotSettingType::Enum) {
        let normalized = parsed
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if let Some(candidate) = spec
            .enum_values
            .iter()
            .find(|candidate| candidate.to_ascii_lowercase() == normalized)
        {
            parsed = json!(*candidate);
        }
    }

    Ok(parsed)
}

fn default_value_for_spec(spec: &BotSettingSpec) -> Value {
    parse_raw_to_value(spec, spec.default_raw).unwrap_or_else(|_| json!(spec.default_raw))
}

fn constraints_json(spec: &BotSettingSpec) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("type".to_string(), json!(spec.value_type.as_str()));
    if let Some(min) = spec.min {
        obj.insert("min".to_string(), json!(min));
    }
    if let Some(max) = spec.max {
        obj.insert("max".to_string(), json!(max));
    }
    if !spec.enum_values.is_empty() {
        obj.insert("enum".to_string(), json!(spec.enum_values));
    }
    Value::Object(obj)
}

fn resolve_setting_value(spec: &BotSettingSpec, config: &Config) -> ResolvedSettingValue {
    let default_value = default_value_for_spec(spec);
    let raw_env_value = std::env::var(spec.key).ok();

    if let Some(raw) = raw_env_value.clone() {
        return match parse_raw_to_value(spec, raw.as_str()) {
            Ok(effective_value) => ResolvedSettingValue {
                effective_value,
                source: "env",
                default_value,
                parse_error: None,
                raw_env_value,
            },
            Err(e) => {
                let fallback_value = spec
                    .config_fallback_key
                    .and_then(|key| config_value_for_key(config, key))
                    .unwrap_or_else(|| default_value.clone());
                let fallback_source = if let Some(config_key) = spec.config_fallback_key {
                    let cfg_default = default_config_value_for_key(config_key)
                        .unwrap_or_else(|| default_value.clone());
                    if fallback_value == cfg_default {
                        "default"
                    } else {
                        "config"
                    }
                } else {
                    "default"
                };
                ResolvedSettingValue {
                    effective_value: fallback_value,
                    source: fallback_source,
                    default_value,
                    parse_error: Some(e.to_string()),
                    raw_env_value,
                }
            }
        };
    }

    if let Some(config_key) = spec.config_fallback_key {
        let current =
            config_value_for_key(config, config_key).unwrap_or_else(|| default_value.clone());
        let cfg_default =
            default_config_value_for_key(config_key).unwrap_or_else(|| default_value.clone());
        let source = if current == cfg_default {
            "default"
        } else {
            "config"
        };
        return ResolvedSettingValue {
            effective_value: current,
            source,
            default_value,
            parse_error: None,
            raw_env_value: None,
        };
    }

    ResolvedSettingValue {
        effective_value: default_value.clone(),
        source: "default",
        default_value,
        parse_error: None,
        raw_env_value: None,
    }
}

fn settings_snapshot(specs: &[BotSettingSpec], config: &Config) -> Vec<Value> {
    specs
        .iter()
        .map(|spec| {
            let resolved = resolve_setting_value(spec, config);
            json!({
                "key": spec.key,
                "group": spec.group,
                "strategy": spec.strategy,
                "effective_value": resolved.effective_value,
                "source": resolved.source,
                "default_value": resolved.default_value,
                "restart_required": spec.restart_required,
                "mutable": spec.mutable,
                "constraints": constraints_json(spec),
                "description": spec.description,
                "parse_error": resolved.parse_error,
                "raw_env_value": resolved.raw_env_value
            })
        })
        .collect()
}

fn strategy_settings_snapshot(
    strategy_slug: &str,
    specs: &[BotSettingSpec],
    config: &Config,
) -> Vec<Value> {
    let filtered = specs
        .iter()
        .filter(|spec| spec.strategy == Some(strategy_slug))
        .cloned()
        .collect::<Vec<_>>();
    settings_snapshot(filtered.as_slice(), config)
}

fn to_env_string(value: &Value, spec: &BotSettingSpec) -> Result<String> {
    let out = match spec.value_type {
        BotSettingType::Bool => value
            .as_bool()
            .map(|v| if v { "true" } else { "false" }.to_string())
            .ok_or_else(|| anyhow!("bool value expected"))?,
        BotSettingType::Integer => value
            .as_i64()
            .map(|v| v.to_string())
            .ok_or_else(|| anyhow!("integer value expected"))?,
        BotSettingType::Number => value
            .as_f64()
            .map(|v| v.to_string())
            .ok_or_else(|| anyhow!("number value expected"))?,
        BotSettingType::String | BotSettingType::Enum => value
            .as_str()
            .map(ToString::to_string)
            .ok_or_else(|| anyhow!("string value expected"))?,
    };
    Ok(out)
}

fn parse_json_value_for_spec(spec: &BotSettingSpec, input: &Value) -> Result<Value> {
    let raw = match input {
        Value::String(v) => v.clone(),
        Value::Bool(v) => {
            if *v {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        Value::Number(v) => v.to_string(),
        _ => return Err(anyhow!("value must be string, bool, or number")),
    };
    parse_raw_to_value(spec, raw.as_str())
}

fn safe_env_file_name(raw: Option<&str>) -> &'static str {
    match raw.unwrap_or(".env").trim() {
        "poly.env" => "poly.env",
        _ => ".env",
    }
}

fn format_env_value_for_file(value: &str) -> String {
    if value
        .chars()
        .any(|ch| ch.is_whitespace() || matches!(ch, '#' | '"'))
    {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{}\"", escaped)
    } else {
        value.to_string()
    }
}

fn persist_env_value(env_file: &str, key: &str, value: &str) -> Result<()> {
    let _guard = env_file_lock()
        .lock()
        .map_err(|_| anyhow!("failed to lock env file writer"))?;

    let path = Path::new(env_file);
    let existing = if path.exists() {
        std::fs::read_to_string(path).with_context(|| format!("failed to read {}", env_file))?
    } else {
        String::new()
    };

    let mut lines = existing
        .lines()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    let mut updated = false;
    let formatted = format!("{}={}", key, format_env_value_for_file(value));

    for line in lines.iter_mut() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((raw_key, _)) = line.split_once('=') else {
            continue;
        };
        if raw_key.trim() == key {
            *line = formatted.clone();
            updated = true;
            break;
        }
    }

    if !updated {
        if !lines.is_empty() && lines.last().map(|line| !line.is_empty()).unwrap_or(false) {
            lines.push(String::new());
        }
        lines.push(formatted);
    }

    let mut content = lines.join("\n");
    if !content.ends_with('\n') {
        content.push('\n');
    }

    let tmp_path = path.with_extension("tmp");
    {
        let mut file = File::create(&tmp_path)
            .with_context(|| format!("failed to create temp env file {}", tmp_path.display()))?;
        file.write_all(content.as_bytes())
            .with_context(|| format!("failed to write temp env file {}", tmp_path.display()))?;
        file.flush()
            .with_context(|| format!("failed to flush temp env file {}", tmp_path.display()))?;
    }
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to replace env file {} with {}",
            path.display(),
            tmp_path.display()
        )
    })?;

    Ok(())
}

fn truncate_for_response(raw: &str, max_chars: usize) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let count = trimmed.chars().count();
    if count <= max_chars {
        return trimmed.to_string();
    }
    let prefix = trimmed.chars().take(max_chars).collect::<String>();
    format!("{}...(truncated {} chars)", prefix, count - max_chars)
}

fn is_valid_signature_type(value: u8) -> bool {
    matches!(value, 0 | 1 | 2)
}

fn resolve_heal_signature_type(ctx: &BotAdminContext) -> u8 {
    std::env::var("POLY_SIGNATURE_TYPE")
        .ok()
        .and_then(|raw| raw.trim().parse::<u8>().ok())
        .filter(|value| is_valid_signature_type(*value))
        .or_else(|| {
            ctx.config
                .polymarket
                .signature_type
                .filter(|value| is_valid_signature_type(*value))
        })
        .unwrap_or(0)
}

fn resolve_heal_private_key(ctx: &BotAdminContext) -> Option<String> {
    std::env::var("POLY_PRIVATE_KEY")
        .ok()
        .or_else(|| ctx.config.polymarket.private_key.clone())
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
}

fn resolve_heal_proxy_wallet(ctx: &BotAdminContext) -> Option<String> {
    std::env::var("POLY_PROXY_WALLET_ADDRESS")
        .ok()
        .or_else(|| ctx.config.polymarket.proxy_wallet_address.clone())
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
}

fn is_remote_alpha_missing_issue(issue: &DoctorIssue) -> bool {
    issue.id.starts_with("remote_alpha:") && issue.id.ends_with(":missing_config")
}

fn run_onboard_refill(
    ctx: &BotAdminContext,
    env_file: &str,
    onboard_api_base: Option<&str>,
    skip_approvals: bool,
    skip_retention_cron: bool,
) -> Value {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("remote_onboard.py");
    if !script.exists() {
        return json!({
            "ok": false,
            "error": format!("onboard script missing: {}", script.display())
        });
    }

    let Some(private_key) = resolve_heal_private_key(ctx) else {
        return json!({
            "ok": false,
            "error": "POLY_PRIVATE_KEY is missing (env/config), cannot run onboard refill"
        });
    };
    let signature_type = resolve_heal_signature_type(ctx);
    let proxy_wallet = resolve_heal_proxy_wallet(ctx);
    if matches!(signature_type, 1 | 2) && proxy_wallet.is_none() {
        return json!({
            "ok": false,
            "error": format!(
                "POLY_PROXY_WALLET_ADDRESS is required for signature_type={}",
                signature_type
            )
        });
    }

    let mut command = Command::new("python3");
    command
        .arg(script.as_os_str())
        .arg("--write-env-file")
        .arg(env_file)
        .arg("--signature-type")
        .arg(signature_type.to_string())
        .env("POLY_PRIVATE_KEY", private_key);
    if let Some(api_base) = onboard_api_base
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        command.arg("--api-base").arg(api_base);
    }
    if skip_approvals {
        command.arg("--skip-approvals");
    }
    if skip_retention_cron {
        command.arg("--skip-retention-cron");
    }
    if let Some(proxy) = proxy_wallet.as_deref() {
        if matches!(signature_type, 1 | 2) {
            command.arg("--proxy-wallet").arg(proxy);
        }
    }
    if let Some(repo_root) = script.parent().and_then(Path::parent) {
        command.current_dir(repo_root);
    }

    let started_at = Instant::now();
    match command.output() {
        Ok(output) => {
            let elapsed_ms = started_at.elapsed().as_millis() as u64;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let code = output.status.code().unwrap_or(-1);
            let ok = output.status.success();
            let payload = json!({
                "ok": ok,
                "exit_code": code,
                "elapsed_ms": elapsed_ms,
                "env_file": env_file,
                "signature_type": signature_type,
                "skip_approvals": skip_approvals,
                "skip_retention_cron": skip_retention_cron,
                "api_base_override": onboard_api_base
                    .map(str::trim)
                    .filter(|value| !value.is_empty()),
                "stdout": truncate_for_response(stdout.as_str(), 4_000),
                "stderr": truncate_for_response(stderr.as_str(), 2_000),
            });
            log_event("bot_audit_fix_onboard_refill", payload.clone());
            payload
        }
        Err(err) => {
            let payload = json!({
                "ok": false,
                "error": format!("failed to execute onboard refill: {}", err),
                "env_file": env_file,
                "signature_type": signature_type,
                "skip_approvals": skip_approvals,
                "skip_retention_cron": skip_retention_cron
            });
            log_event("bot_audit_fix_onboard_refill", payload.clone());
            payload
        }
    }
}

fn pick_specs_for_scope<'a>(
    specs: &'a [BotSettingSpec],
    scope: &str,
    key: Option<&str>,
    group: Option<&str>,
    strategy: Option<&str>,
) -> Result<Vec<&'a BotSettingSpec>> {
    match scope {
        "key" => {
            let target_key = key
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| anyhow!("scope=key requires 'key'"))?;
            let spec = specs
                .iter()
                .find(|spec| spec.key.eq_ignore_ascii_case(target_key))
                .ok_or_else(|| anyhow!("unknown key '{}'", target_key))?;
            Ok(vec![spec])
        }
        "group" => {
            let target_group = group
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| anyhow!("scope=group requires 'group'"))?;
            let selected = specs
                .iter()
                .filter(|spec| spec.group.eq_ignore_ascii_case(target_group))
                .collect::<Vec<_>>();
            if selected.is_empty() {
                return Err(anyhow!("unknown group '{}'", target_group));
            }
            Ok(selected)
        }
        "strategy" => {
            let target = strategy
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| anyhow!("scope=strategy requires 'strategy'"))?;
            let normalized = normalize_strategy_slug(target)
                .ok_or_else(|| anyhow!("unknown strategy '{}'", target))?;
            let selected = specs
                .iter()
                .filter(|spec| spec.strategy == Some(normalized))
                .collect::<Vec<_>>();
            if selected.is_empty() {
                return Err(anyhow!(
                    "no registered settings for strategy '{}'",
                    normalized
                ));
            }
            Ok(selected)
        }
        "all" => Ok(specs.iter().collect::<Vec<_>>()),
        other => Err(anyhow!(
            "invalid scope '{}' (expected key|group|strategy|all)",
            other
        )),
    }
}

fn parse_event_ts_ms(event: &Value) -> Option<i64> {
    event.get("ts_ms").and_then(Value::as_i64).or_else(|| {
        event
            .get("ts_ms")
            .and_then(Value::as_u64)
            .and_then(|v| i64::try_from(v).ok())
    })
}

fn parse_event_kind(event: &Value) -> Option<String> {
    event
        .get("kind")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            event
                .get("event")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
}

fn event_contains_clob_auth_failure(event: &Value) -> bool {
    let payload = event.get("payload").cloned().unwrap_or_else(|| json!({}));
    let text = payload.to_string().to_ascii_lowercase();
    text.contains("failed to authenticate with clob api")
        || text.contains("/auth/derive-api-key")
        || text.contains("error code: 1015")
        || text.contains("invalid api key")
        || text.contains("invalid signature")
}

fn load_recent_events(max_lines: usize) -> Vec<Value> {
    let file = match File::open("events.jsonl") {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let mut buf = VecDeque::<String>::new();
    for line in reader.lines().map_while(Result::ok) {
        if buf.len() >= max_lines {
            let _ = buf.pop_front();
        }
        buf.push_back(line);
    }

    buf.into_iter()
        .filter_map(|line| serde_json::from_str::<Value>(line.as_str()).ok())
        .collect::<Vec<_>>()
}

async fn health_indicators(ctx: &BotAdminContext) -> Value {
    let now_ms = Utc::now().timestamp_millis();
    let stale_age_ms = std::env::var("EVPOLY_SIGNAL_LIVENESS_STALE_MS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(180_000)
        .max(15_000);

    let (signal_newest_update_ms, signal_source_ages) = {
        let guard = ctx.signal_state.read().await;
        (
            guard.newest_source_update_ms(),
            guard.source_ages_ms(now_ms),
        )
    };
    let signal_age_ms = if signal_newest_update_ms > 0 {
        now_ms.saturating_sub(signal_newest_update_ms)
    } else {
        i64::MAX
    };
    let signal_stale = signal_newest_update_ms <= 0 || signal_age_ms > stale_age_ms;

    let (coinbase_last_update_ms, coinbase_feed_state) = {
        let guard = ctx.coinbase_state.read().await;
        (guard.last_update_ms, format!("{:?}", guard.feed_state))
    };
    let coinbase_age_ms = if coinbase_last_update_ms > 0 {
        now_ms.saturating_sub(coinbase_last_update_ms)
    } else {
        i64::MAX
    };
    let coinbase_stale = coinbase_last_update_ms <= 0 || coinbase_age_ms > stale_age_ms;

    let events = load_recent_events(2_000);
    let window_start_ms = now_ms.saturating_sub(10 * 60 * 1_000);
    let mut contention_events_10m = 0_u64;
    let mut queue_pressure_events_10m = 0_u64;
    let mut failed_events_10m = 0_u64;
    let mut clob_auth_fail_events_10m = 0_u64;
    let mut clob_auth_last_failure_ts_ms: Option<i64> = None;
    let mut latest_failures = Vec::new();

    for event in events.iter() {
        let ts_ms = parse_event_ts_ms(event).unwrap_or(0);
        let kind = parse_event_kind(event).unwrap_or_else(|| "unknown".to_string());
        let is_recent = ts_ms >= window_start_ms;
        if is_recent && kind == "runtime_contention" {
            contention_events_10m = contention_events_10m.saturating_add(1);
        }
        if is_recent
            && matches!(
                kind.as_str(),
                "entry_enqueue_backpressure_skip"
                    | "premarket_scope_lane_drop_full"
                    | "premarket_dispatch_drop_full"
            )
        {
            queue_pressure_events_10m = queue_pressure_events_10m.saturating_add(1);
        }
        if is_recent && (kind.ends_with("_failed") || kind.contains("error")) {
            failed_events_10m = failed_events_10m.saturating_add(1);
        }
        if is_recent && event_contains_clob_auth_failure(event) {
            clob_auth_fail_events_10m = clob_auth_fail_events_10m.saturating_add(1);
            clob_auth_last_failure_ts_ms = Some(match clob_auth_last_failure_ts_ms {
                Some(existing) => existing.max(ts_ms),
                None => ts_ms,
            });
        }
        if kind.ends_with("_failed") || kind.contains("error") {
            latest_failures.push(json!({
                "ts_ms": ts_ms,
                "kind": kind,
                "payload": event.get("payload").cloned().unwrap_or_else(|| json!({}))
            }));
        }
    }

    latest_failures.sort_by_key(|row| row.get("ts_ms").and_then(Value::as_i64).unwrap_or(0));
    if latest_failures.len() > 20 {
        let keep_from = latest_failures.len().saturating_sub(20);
        latest_failures = latest_failures.split_off(keep_from);
    }
    let clob_auth_fail_threshold_10m = 5_u64;
    let clob_auth_failure_degraded = clob_auth_fail_events_10m >= clob_auth_fail_threshold_10m;
    let clob_auth_last_failure_age_ms =
        clob_auth_last_failure_ts_ms.map(|ts| now_ms.saturating_sub(ts).max(0));

    let indicators = vec![
        json!({
            "key": "signals.newest_update_age_ms",
            "effective_value": if signal_age_ms == i64::MAX { Value::Null } else { json!(signal_age_ms) },
            "source": "runtime",
            "default_value": stale_age_ms,
            "restart_required": false,
            "constraints": {"type": "integer", "max": stale_age_ms}
        }),
        json!({
            "key": "coinbase.last_update_age_ms",
            "effective_value": if coinbase_age_ms == i64::MAX { Value::Null } else { json!(coinbase_age_ms) },
            "source": "runtime",
            "default_value": stale_age_ms,
            "restart_required": false,
            "constraints": {"type": "integer", "max": stale_age_ms}
        }),
        json!({
            "key": "runtime.contention_events_10m",
            "effective_value": contention_events_10m,
            "source": "runtime",
            "default_value": 0,
            "restart_required": false,
            "constraints": {"type": "integer", "max": 0}
        }),
        json!({
            "key": "runtime.queue_pressure_events_10m",
            "effective_value": queue_pressure_events_10m,
            "source": "runtime",
            "default_value": 0,
            "restart_required": false,
            "constraints": {"type": "integer", "max": 0}
        }),
        json!({
            "key": "runtime.failed_events_10m",
            "effective_value": failed_events_10m,
            "source": "runtime",
            "default_value": 0,
            "restart_required": false,
            "constraints": {"type": "integer", "max": 0}
        }),
        json!({
            "key": "runtime.clob_auth_fail_events_10m",
            "effective_value": clob_auth_fail_events_10m,
            "source": "runtime",
            "default_value": 0,
            "restart_required": false,
            "constraints": {"type": "integer", "max": clob_auth_fail_threshold_10m.saturating_sub(1)}
        }),
    ];

    json!({
        "ok": true,
        "section": "health",
        "fetched_at_ms": now_ms,
        "indicators": indicators,
        "status": {
            "signal_stale": signal_stale,
            "coinbase_stale": coinbase_stale,
            "signal_newest_update_ms": if signal_newest_update_ms > 0 { Some(signal_newest_update_ms) } else { None },
            "coinbase_last_update_ms": if coinbase_last_update_ms > 0 { Some(coinbase_last_update_ms) } else { None },
            "coinbase_feed_state": coinbase_feed_state,
            "signal_source_ages_ms": signal_source_ages,
            "contention_events_10m": contention_events_10m,
            "queue_pressure_events_10m": queue_pressure_events_10m,
            "failed_events_10m": failed_events_10m,
            "clob_auth_fail_events_10m": clob_auth_fail_events_10m,
            "clob_auth_fail_threshold_10m": clob_auth_fail_threshold_10m,
            "clob_auth_failure_degraded": clob_auth_failure_degraded,
            "clob_auth_last_failure_ts_ms": clob_auth_last_failure_ts_ms,
            "clob_auth_last_failure_age_ms": clob_auth_last_failure_age_ms,
            "stale_age_threshold_ms": stale_age_ms,
            "overall": if signal_stale || coinbase_stale || clob_auth_failure_degraded {
                "degraded"
            } else {
                "healthy"
            }
        },
        "recent_failures": latest_failures,
        "merge_status": ctx.trader.merge_sweep_status(),
        "redemption_status": ctx.trader.redemption_sweep_status()
    })
}

fn strategy_label(strategy_slug: &str) -> &'static str {
    match strategy_slug {
        "premarket" => "Premarket",
        "endgame" => "Endgame",
        "evcurve" => "EVCurve",
        "evsnipe" => "EVSnipe",
        "mm" => "MM Rewards",
        "mm_sport" => "MM Sport",
        "sessionband" => "SessionBand",
        _ => "Strategy",
    }
}

fn strategy_enabled(strategy_slug: &str) -> bool {
    strategy_enable_key(strategy_slug)
        .and_then(|key| std::env::var(key).ok())
        .and_then(|raw| parse_bool_raw(raw.as_str()))
        .unwrap_or_else(|| strategy_enable_default(strategy_slug))
}

fn strategy_slug_for_id(strategy_id: &str) -> Option<&'static str> {
    let normalized = strategy_id.trim().to_ascii_lowercase();
    strategy_catalog()
        .into_iter()
        .find(|(_, canonical_id, aliases)| {
            canonical_id.eq_ignore_ascii_case(normalized.as_str())
                || aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(normalized.as_str()))
        })
        .map(|(slug, _, _)| slug)
}

fn strategy_label_from_id(strategy_id: &str) -> String {
    strategy_slug_for_id(strategy_id)
        .map(strategy_label)
        .unwrap_or_else(|| strategy_id)
        .to_string()
}

fn strategy_symbol_env_key(strategy_slug: &str) -> Option<&'static str> {
    match strategy_slug {
        "endgame" => Some("EVPOLY_ENDGAME_SYMBOLS"),
        "evcurve" => Some("EVPOLY_EVCURVE_SYMBOLS"),
        "evsnipe" => Some("EVPOLY_EVSNIPE_SYMBOLS"),
        "sessionband" => Some("EVPOLY_SESSIONBAND_SYMBOLS"),
        _ => None,
    }
}

fn summarize_symbols(symbols: &[String]) -> String {
    if symbols.is_empty() {
        return "Selected markets".to_string();
    }
    if symbols.len() <= 4 {
        return symbols.join(" / ");
    }
    format!(
        "{} +{}",
        symbols[..4].join(" / "),
        symbols.len().saturating_sub(4)
    )
}

fn strategy_scope_summary(strategy_slug: &str, strategy_id: &str) -> String {
    match strategy_slug {
        "mm" => "Reward markets chosen automatically".to_string(),
        "mm_sport" => "Sports reward markets".to_string(),
        _ => {
            let symbols = strategy_symbol_env_key(strategy_slug)
                .and_then(|key| std::env::var(key).ok())
                .map(|raw| symbol_ownership::parse_symbols_csv_for_strategy(strategy_id, raw.as_str()))
                .unwrap_or_else(|| symbol_ownership::default_symbols_for_strategy(strategy_id));
            summarize_symbols(symbols.as_slice())
        }
    }
}

fn iso_from_ms(ts_ms: i64) -> Option<String> {
    chrono::DateTime::<Utc>::from_timestamp_millis(ts_ms).map(|dt| dt.to_rfc3339())
}

fn humanize_event_type(raw: &str) -> String {
    match raw.trim().to_ascii_uppercase().as_str() {
        "ENTRY_SUBMIT" => "Submitted an order".to_string(),
        "ENTRY_ACK" => "Order acknowledged".to_string(),
        "ENTRY_FILL" => "Opened a position".to_string(),
        "EXIT" => "Closed a trade".to_string(),
        "CANCEL" => "Canceled an order".to_string(),
        other if other.is_empty() => "Updated".to_string(),
        other => {
            let mut out = String::new();
            for (index, chunk) in other.split('_').enumerate() {
                if index > 0 {
                    out.push(' ');
                }
                let mut chars = chunk.chars();
                if let Some(first) = chars.next() {
                    out.push(first.to_ascii_uppercase());
                    for ch in chars {
                        out.push(ch.to_ascii_lowercase());
                    }
                }
            }
            if out.is_empty() {
                "Updated".to_string()
            } else {
                out
            }
        }
    }
}

fn health_status_value<'a>(health: &'a Value, key: &str) -> Option<&'a Value> {
    health.get("status").and_then(|status| status.get(key))
}

fn market_data_blocker_from_health(health: &Value) -> Option<String> {
    if health_status_value(health, "clob_auth_failure_degraded")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Some("Trading authentication needs to recover.".to_string());
    }
    if health_status_value(health, "signal_stale")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Some("Waiting for live market data to recover.".to_string());
    }
    if health_status_value(health, "coinbase_stale")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Some("Waiting for price feeds to recover.".to_string());
    }
    if health_status_value(health, "failed_events_10m")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        >= 10
    {
        return Some("The bot hit repeated runtime errors and needs attention.".to_string());
    }
    None
}

fn global_blocker_reason(health: &Value, enabled_strategy_count: usize) -> Option<String> {
    if enabled_strategy_count == 0 {
        return Some("Turn on at least one strategy before you start trading.".to_string());
    }
    market_data_blocker_from_health(health)
}

fn recent_result_text(
    summary: &crate::tracking_db::UiDashboardDbSummary,
) -> Option<String> {
    if let Some(ts_ms) = summary.last_exit_ts_ms {
        let label = summary
            .last_exit_strategy_id
            .as_deref()
            .map(strategy_label_from_id)
            .unwrap_or_else(|| "EVPOLY".to_string());
        let pnl = summary.last_exit_pnl_usd.unwrap_or(0.0);
        let result = if pnl > 0.000_001 {
            format!("Last closed trade made +${:.2} on {}", pnl, label)
        } else if pnl < -0.000_001 {
            format!("Last closed trade lost ${:.2} on {}", pnl.abs(), label)
        } else {
            format!("Last closed trade finished flat on {}", label)
        };
        if ts_ms > 0 {
            return Some(result);
        }
    }
    if let Some(event_type) = summary.last_event_type.as_deref() {
        let label = summary
            .last_event_strategy_id
            .as_deref()
            .map(strategy_label_from_id)
            .unwrap_or_else(|| "EVPOLY".to_string());
        return Some(format!("Latest action: {} on {}", humanize_event_type(event_type), label));
    }
    None
}

async fn fetch_free_balance(ctx: &BotAdminContext) -> Option<f64> {
    match tokio::time::timeout(
        std::time::Duration::from_secs(6),
        ctx.api.check_usdc_balance_allowance(),
    )
    .await
    {
        Ok(Ok((balance, _allowance))) => balance.to_string().parse::<f64>().ok(),
        _ => None,
    }
}

async fn build_ui_dashboard_summary(ctx: &BotAdminContext) -> UiDashboardSummary {
    let health = health_indicators(ctx).await;
    let enabled_strategies = strategy_catalog()
        .into_iter()
        .filter_map(|(slug, _, _)| {
            if strategy_enabled(slug) {
                Some(strategy_label(slug).to_string())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let blocker_reason = global_blocker_reason(&health, enabled_strategies.len());
    let db_summary = ctx
        .trader
        .tracking_db()
        .and_then(|db| db.summarize_ui_dashboard().ok());
    let open_positions_count = db_summary
        .as_ref()
        .map(|summary| summary.open_positions_count)
        .unwrap_or(0);
    let recent_orders_count = db_summary
        .as_ref()
        .map(|summary| summary.recent_orders_count)
        .unwrap_or(0);
    let recent_result = db_summary.as_ref().and_then(recent_result_text);
    let (headline, detail) = if let Some(blocker) = blocker_reason.as_ref() {
        (
            "Something needs attention".to_string(),
            blocker.clone(),
        )
    } else if open_positions_count > 0 {
        (
            format!(
                "Managing {} open {}",
                open_positions_count,
                if open_positions_count == 1 {
                    "position"
                } else {
                    "positions"
                }
            ),
            "EVPOLY is already in the market and is watching those trades closely.".to_string(),
        )
    } else if recent_orders_count > 0 {
        (
            "Recent orders have been placed".to_string(),
            "EVPOLY is live and waiting for the next clean setup.".to_string(),
        )
    } else if enabled_strategies.is_empty() {
        (
            "Choose a strategy to trade".to_string(),
            "Turn on a strategy first so EVPOLY knows what it should watch.".to_string(),
        )
    } else {
        (
            "Watching for a better market".to_string(),
            "Nothing is wrong. EVPOLY is on and waiting for a setup worth taking.".to_string(),
        )
    };
    let detail = if ctx.simulation_mode && blocker_reason.is_none() {
        format!("{detail} Dry run is on, so orders stay simulated.")
    } else {
        detail
    };
    UiDashboardSummary {
        bot_state: "running".to_string(),
        mode: if ctx.simulation_mode {
            "dry_run".to_string()
        } else {
            "live".to_string()
        },
        headline,
        detail,
        last_activity_at_ms: db_summary.as_ref().and_then(|summary| summary.last_event_ts_ms),
        last_activity_at: db_summary
            .as_ref()
            .and_then(|summary| summary.last_event_ts_ms)
            .and_then(iso_from_ms),
        recent_result,
        blocker_reason,
        enabled_strategies,
        open_positions_count,
        recent_orders_count,
        free_balance: fetch_free_balance(ctx).await,
        avg_ack_latency_ms: db_summary
            .as_ref()
            .and_then(|summary| summary.avg_ack_latency_ms),
        total_pnl: db_summary.as_ref().map(|summary| summary.total_pnl).unwrap_or(0.0),
        total_trades: db_summary
            .as_ref()
            .map(|summary| summary.total_trades)
            .unwrap_or(0),
        winning_trades: db_summary
            .as_ref()
            .map(|summary| summary.winning_trades)
            .unwrap_or(0),
        losing_trades: db_summary
            .as_ref()
            .map(|summary| summary.losing_trades)
            .unwrap_or(0),
    }
}

async fn build_ui_strategy_states(ctx: &BotAdminContext) -> Vec<UiStrategyState> {
    let health = health_indicators(ctx).await;
    let market_data_blocker = market_data_blocker_from_health(&health);
    let Some(db) = ctx.trader.tracking_db() else {
        return strategy_catalog()
            .into_iter()
            .map(|(slug, strategy_id, _)| {
                let enabled = strategy_enabled(slug);
                UiStrategyState {
                    strategy_id: strategy_id.to_string(),
                    slug: slug.to_string(),
                    label: strategy_label(slug).to_string(),
                    enabled,
                    state: if enabled {
                        "idle".to_string()
                    } else {
                        "disabled".to_string()
                    },
                    summary: if enabled {
                        "Waiting for the next clean setup.".to_string()
                    } else {
                        "Turn this strategy on when you want EVPOLY to use it.".to_string()
                    },
                    scope_summary: strategy_scope_summary(slug, strategy_id),
                    last_action: None,
                    last_action_at_ms: None,
                    last_action_at: None,
                    blocker_reason: None,
                    open_orders_count: 0,
                    open_positions_count: 0,
                }
            })
            .collect();
    };

    let reporting_scope = crate::tracking_db::ReportingScope::from_env();
    let activity_rows = db
        .summarize_strategy_activity_canonical(Some(&reporting_scope))
        .unwrap_or_default()
        .into_iter()
        .map(|row| (row.strategy_id.clone(), row))
        .collect::<HashMap<_, _>>();
    let now_ms = Utc::now().timestamp_millis();

    strategy_catalog()
        .into_iter()
        .map(|(slug, strategy_id, _)| {
            let enabled = strategy_enabled(slug);
            let activity = activity_rows.get(strategy_id);
            let open_orders_count = activity.map(|row| row.open_pending_orders).unwrap_or(0);
            let open_positions_count = db
                .summarize_strategy_mtm_pnl_v2(strategy_id)
                .ok()
                .map(|summary| summary.open_positions)
                .unwrap_or(0);
            let last_event = db.latest_strategy_event(strategy_id).ok().flatten();
            let last_action = last_event
                .as_ref()
                .map(|event| humanize_event_type(event.event_type.as_str()));
            let last_action_at_ms = last_event.as_ref().map(|event| event.ts_ms);
            let last_action_at = last_action_at_ms.and_then(iso_from_ms);
            let idle_blocker = match slug {
                "mm" => Some("Waiting for reward markets worth quoting.".to_string()),
                "mm_sport" => Some("Waiting for sports reward markets worth quoting.".to_string()),
                _ => None,
            };
            let blocker_reason = if !enabled {
                None
            } else if let Some(reason) = market_data_blocker.clone() {
                Some(reason)
            } else if open_orders_count == 0 && open_positions_count == 0 {
                idle_blocker
            } else {
                None
            };
            let last_action_age_ms = last_action_at_ms.map(|ts| now_ms.saturating_sub(ts).max(0));
            let state = if !enabled {
                "disabled".to_string()
            } else if blocker_reason.is_some() && open_orders_count == 0 && open_positions_count == 0 {
                "blocked".to_string()
            } else if open_positions_count > 0 || open_orders_count > 0 {
                "running".to_string()
            } else if last_action_age_ms.unwrap_or(i64::MAX) <= 6 * 3_600_000 {
                "watching".to_string()
            } else {
                "idle".to_string()
            };
            let summary = match state.as_str() {
                "disabled" => "Turn this strategy on when you want EVPOLY to use it.".to_string(),
                "blocked" => blocker_reason
                    .clone()
                    .unwrap_or_else(|| "Waiting for the blocker to clear.".to_string()),
                "running" if open_positions_count > 0 => format!(
                    "Managing {} open {}.",
                    open_positions_count,
                    if open_positions_count == 1 {
                        "position"
                    } else {
                        "positions"
                    }
                ),
                "running" if open_orders_count > 0 => format!(
                    "Working {} open {}.",
                    open_orders_count,
                    if open_orders_count == 1 { "order" } else { "orders" }
                ),
                "watching" => "Watching for the next clean setup.".to_string(),
                _ => "Ready but waiting for a better market.".to_string(),
            };

            UiStrategyState {
                strategy_id: strategy_id.to_string(),
                slug: slug.to_string(),
                label: strategy_label(slug).to_string(),
                enabled,
                state,
                summary,
                scope_summary: strategy_scope_summary(slug, strategy_id),
                last_action,
                last_action_at_ms,
                last_action_at,
                blocker_reason,
                open_orders_count,
                open_positions_count,
            }
        })
        .collect()
}

async fn collect_doctor_issues(
    ctx: &BotAdminContext,
    specs: &[BotSettingSpec],
) -> Vec<DoctorIssue> {
    let mut issues = Vec::<DoctorIssue>::new();

    for spec in specs {
        let raw_env = std::env::var(spec.key).ok();
        if let Some(raw) = raw_env {
            if let Err(e) = parse_raw_to_value(spec, raw.as_str()) {
                let default_value = default_value_for_spec(spec);
                issues.push(DoctorIssue {
                    id: format!("invalid:{}", spec.key),
                    severity: "error",
                    key: Some(spec.key.to_string()),
                    summary: format!("{} has invalid value '{}': {}", spec.key, raw, e),
                    recommended_fix: Some(json!({
                        "key": spec.key,
                        "value": default_value,
                        "action": "set_default"
                    })),
                    auto_fixable: spec.mutable,
                    details: json!({
                        "group": spec.group,
                        "strategy": spec.strategy,
                        "constraints": constraints_json(spec)
                    }),
                });
            }
        }
    }

    let health = health_indicators(ctx).await;
    let status = health.get("status").cloned().unwrap_or_else(|| json!({}));
    if status
        .get("signal_stale")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        issues.push(DoctorIssue {
            id: "health:signal_stale".to_string(),
            severity: "critical",
            key: None,
            summary: "Signal pipeline appears stale beyond configured threshold".to_string(),
            recommended_fix: None,
            auto_fixable: false,
            details: status.clone(),
        });
    }
    if status
        .get("coinbase_stale")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        issues.push(DoctorIssue {
            id: "health:coinbase_stale".to_string(),
            severity: "critical",
            key: None,
            summary: "Coinbase book feed appears stale beyond configured threshold".to_string(),
            recommended_fix: None,
            auto_fixable: false,
            details: status.clone(),
        });
    }

    let contention = status
        .get("contention_events_10m")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if contention > 0 {
        issues.push(DoctorIssue {
            id: "health:contention".to_string(),
            severity: "warn",
            key: None,
            summary: format!(
                "Observed {} runtime contention events in the last 10m",
                contention
            ),
            recommended_fix: Some(json!({
                "key": "EVPOLY_ENTRY_WORKER_COUNT_NORMAL",
                "hint": "increase workers or reduce submit pressure"
            })),
            auto_fixable: false,
            details: status.clone(),
        });
    }

    let queue_pressure = status
        .get("queue_pressure_events_10m")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if queue_pressure > 0 {
        issues.push(DoctorIssue {
            id: "health:queue_pressure".to_string(),
            severity: "warn",
            key: None,
            summary: format!(
                "Observed {} queue-pressure drops/skips in the last 10m",
                queue_pressure
            ),
            recommended_fix: Some(json!({
                "key": "EVPOLY_ENTRY_WORKER_QUEUE_CAP",
                "hint": "increase queue cap or reduce polling/submission aggressiveness"
            })),
            auto_fixable: false,
            details: status.clone(),
        });
    }
    let clob_auth_fail_events = status
        .get("clob_auth_fail_events_10m")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let clob_auth_fail_threshold = status
        .get("clob_auth_fail_threshold_10m")
        .and_then(Value::as_u64)
        .unwrap_or(5);
    if clob_auth_fail_events >= clob_auth_fail_threshold {
        issues.push(DoctorIssue {
            id: "health:clob_auth_failures".to_string(),
            severity: "error",
            key: None,
            summary: format!(
                "Observed {} CLOB auth failures in the last 10m (threshold: {})",
                clob_auth_fail_events, clob_auth_fail_threshold
            ),
            recommended_fix: Some(json!({
                "hints": [
                    "Likely /auth/derive-api-key rate limiting or invalid API credentials",
                    "Reduce submit/auth pressure (entry concurrency, cancel cadence)",
                    "Re-run onboard if credentials/signature type are mismatched"
                ]
            })),
            auto_fixable: false,
            details: status.clone(),
        });
    }

    let mm_enabled = parse_bool_raw(
        std::env::var("EVPOLY_STRATEGY_MM_REWARDS_ENABLE")
            .unwrap_or_else(|_| "false".to_string())
            .as_str(),
    )
    .unwrap_or(false);
    let mm_hard_disable = parse_bool_raw(
        std::env::var("EVPOLY_MM_HARD_DISABLE")
            .unwrap_or_else(|_| "true".to_string())
            .as_str(),
    )
    .unwrap_or(true);
    if mm_enabled && mm_hard_disable {
        issues.push(DoctorIssue {
            id: "mm:enabled_but_hard_disabled".to_string(),
            severity: "error",
            key: None,
            summary: "MM strategy is enabled while EVPOLY_MM_HARD_DISABLE=true".to_string(),
            recommended_fix: Some(json!({
                "key": "EVPOLY_MM_HARD_DISABLE",
                "value": false,
                "action": "set"
            })),
            auto_fixable: true,
            details: json!({
                "EVPOLY_STRATEGY_MM_REWARDS_ENABLE": mm_enabled,
                "EVPOLY_MM_HARD_DISABLE": mm_hard_disable
            }),
        });
    }

    let env_bool = |key: &str, default: bool| {
        std::env::var(key)
            .ok()
            .and_then(|raw| parse_bool_raw(raw.as_str()))
            .unwrap_or(default)
    };
    let env_missing = |key: &str| {
        std::env::var(key)
            .map(|raw| raw.trim().is_empty())
            .unwrap_or(true)
    };
    let remote_url_has_builtin_default = |key: &str| {
        matches!(
            key,
            "EVPOLY_REMOTE_PREMARKET_ALPHA_URL"
                | "EVPOLY_REMOTE_ENDGAME_ALPHA_URL"
                | "EVPOLY_REMOTE_EVCURVE_ALPHA_URL"
                | "EVPOLY_REMOTE_SESSIONBAND_ALPHA_URL"
                | "EVPOLY_REMOTE_MM_REWARDS_SELECTION_ALPHA_URL"
                | "EVPOLY_REMOTE_MARKET_DISCOVERY_URL"
                | "EVPOLY_REMOTE_EVSNIPE_DISCOVERY_URL"
        )
    };
    let endgame_alpha_required = env_bool("EVPOLY_ENDGAME_ALPHA_REQUIRED", true);
    let mut check_remote_alpha_issue = |strategy_id: &'static str,
                                        enable_key: &'static str,
                                        enabled_default: bool,
                                        url_key: &'static str,
                                        token_key: &'static str,
                                        required: bool| {
        if !required || !env_bool(enable_key, enabled_default) {
            return;
        }
        let mut missing_keys = Vec::<&'static str>::new();
        if env_missing(url_key) && !remote_url_has_builtin_default(url_key) {
            missing_keys.push(url_key);
        }
        if env_missing(token_key) {
            missing_keys.push(token_key);
        }
        if missing_keys.is_empty() {
            return;
        }
        issues.push(DoctorIssue {
            id: format!("remote_alpha:{}:missing_config", strategy_id),
            severity: "error",
            key: None,
            summary: format!(
                "{} is enabled but remote alpha config is missing ({})",
                strategy_id,
                missing_keys.join(", ")
            ),
            recommended_fix: None,
            auto_fixable: false,
            details: json!({
                "strategy_id": strategy_id,
                "strategy_enable_key": enable_key,
                "required": required,
                "missing_keys": missing_keys
            }),
        });
    };
    check_remote_alpha_issue(
        STRATEGY_ID_PREMARKET_V1,
        "EVPOLY_STRATEGY_PREMARKET_ENABLE",
        true,
        "EVPOLY_REMOTE_PREMARKET_ALPHA_URL",
        "EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN",
        true,
    );
    check_remote_alpha_issue(
        STRATEGY_ID_EVCURVE_V1,
        "EVPOLY_STRATEGY_EVCURVE_ENABLE",
        true,
        "EVPOLY_REMOTE_EVCURVE_ALPHA_URL",
        "EVPOLY_REMOTE_EVCURVE_ALPHA_TOKEN",
        true,
    );
    check_remote_alpha_issue(
        STRATEGY_ID_SESSIONBAND_V1,
        "EVPOLY_STRATEGY_SESSIONBAND_ENABLE",
        true,
        "EVPOLY_REMOTE_SESSIONBAND_ALPHA_URL",
        "EVPOLY_REMOTE_SESSIONBAND_ALPHA_TOKEN",
        true,
    );
    check_remote_alpha_issue(
        STRATEGY_ID_ENDGAME_SWEEP_V1,
        "EVPOLY_STRATEGY_ENDGAME_ENABLE",
        true,
        "EVPOLY_REMOTE_ENDGAME_ALPHA_URL",
        "EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN",
        endgame_alpha_required,
    );

    issues
}

fn issue_to_json(issue: &DoctorIssue) -> Value {
    json!({
        "id": issue.id,
        "severity": issue.severity,
        "key": issue.key,
        "summary": issue.summary,
        "recommended_fix": issue.recommended_fix,
        "auto_fixable": issue.auto_fixable,
        "details": issue.details
    })
}

fn find_spec_for_key<'a>(specs: &'a [BotSettingSpec], key: &str) -> Option<&'a BotSettingSpec> {
    specs
        .iter()
        .find(|spec| spec.key.eq_ignore_ascii_case(key.trim()))
}

fn apply_changes_internal(
    specs: &[BotSettingSpec],
    config: &Config,
    changes: Vec<BotSetChange>,
    default_persist: bool,
    env_file: &str,
) -> Value {
    let mut results = Vec::<Value>::new();
    let mut applied_count = 0_u64;
    let mut failed_count = 0_u64;

    for change in changes {
        let key = change.key.trim().to_string();
        let Some(spec) = find_spec_for_key(specs, key.as_str()) else {
            failed_count = failed_count.saturating_add(1);
            results.push(json!({
                "key": key,
                "ok": false,
                "error": "unknown_key"
            }));
            continue;
        };

        if !spec.mutable {
            failed_count = failed_count.saturating_add(1);
            results.push(json!({
                "key": spec.key,
                "ok": false,
                "error": "key_not_mutable"
            }));
            continue;
        }

        let before = resolve_setting_value(spec, config);
        let parsed_value = match parse_json_value_for_spec(spec, &change.value) {
            Ok(parsed) => parsed,
            Err(e) => {
                failed_count = failed_count.saturating_add(1);
                results.push(json!({
                    "key": spec.key,
                    "ok": false,
                    "error": e.to_string(),
                    "constraints": constraints_json(spec),
                    "default_value": before.default_value,
                    "restart_required": spec.restart_required
                }));
                continue;
            }
        };
        let env_value = match to_env_string(&parsed_value, spec) {
            Ok(value) => value,
            Err(e) => {
                failed_count = failed_count.saturating_add(1);
                results.push(json!({
                    "key": spec.key,
                    "ok": false,
                    "error": e.to_string()
                }));
                continue;
            }
        };

        std::env::set_var(spec.key, env_value.as_str());

        let persist = change.persist.unwrap_or(default_persist);
        let persist_error = if persist {
            persist_env_value(env_file, spec.key, env_value.as_str())
                .err()
                .map(|e| e.to_string())
        } else {
            None
        };

        let after = resolve_setting_value(spec, config);
        let ok = persist_error.is_none();
        if ok {
            applied_count = applied_count.saturating_add(1);
        } else {
            failed_count = failed_count.saturating_add(1);
        }

        log_event(
            "bot_setting_changed",
            json!({
                "key": spec.key,
                "before": before.effective_value,
                "after": after.effective_value,
                "persist": persist,
                "persist_error": persist_error,
                "restart_required": spec.restart_required
            }),
        );

        results.push(json!({
            "key": spec.key,
            "requested_value": change.value,
            "effective_value": after.effective_value,
            "effective_value_before": before.effective_value,
            "effective_value_after": after.effective_value,
            "source": after.source,
            "default_value": after.default_value,
            "restart_required": spec.restart_required,
            "constraints": constraints_json(spec),
            "applied_now": true,
            "persisted": persist && persist_error.is_none(),
            "ok": ok,
            "error": persist_error
        }));
    }

    json!({
        "ok": failed_count == 0,
        "applied": applied_count,
        "failed": failed_count,
        "results": results
    })
}

fn parse_body_or_default<T>(body: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de> + Default,
{
    if body.trim().is_empty() {
        return Ok(T::default());
    }
    serde_json::from_str::<T>(body).with_context(|| "invalid_json_body".to_string())
}

fn bad_request(error: impl Into<String>) -> BotHttpResponse {
    BotHttpResponse {
        status_code: 400,
        status_text: "Bad Request",
        body: json!({"ok": false, "error": error.into()}),
    }
}

pub async fn try_handle_bot_request(
    method: &str,
    path: &str,
    _query: &HashMap<String, String>,
    body: &str,
    ctx: &BotAdminContext,
) -> Option<BotHttpResponse> {
    if !path.starts_with("/bot") && !path.starts_with("/ui") {
        return None;
    }

    let specs = setting_specs();
    let config_ref = ctx.config.as_ref();

    let response = match (method, path) {
        ("GET", "/bot/schema") => {
            let settings = settings_snapshot(specs.as_slice(), config_ref);
            let strategies = strategy_catalog()
                .into_iter()
                .map(|(slug, strategy_id, aliases)| {
                    let enable_key = strategy_enable_key(slug);
                    let enable_source = enable_key
                        .and_then(|key| std::env::var(key).ok())
                        .map(|_| "env")
                        .unwrap_or("default");
                    let enable_value = enable_key
                        .and_then(|key| std::env::var(key).ok())
                        .and_then(|raw| parse_bool_raw(raw.as_str()))
                        .unwrap_or_else(|| strategy_enable_default(slug));
                    json!({
                        "slug": slug,
                        "strategy_id": strategy_id,
                        "aliases": aliases,
                        "enable_key": enable_key,
                        "enabled": enable_value,
                        "enable_source": enable_source
                    })
                })
                .collect::<Vec<_>>();
            BotHttpResponse {
                status_code: 200,
                status_text: "OK",
                body: json!({
                    "ok": true,
                    "section": "schema",
                    "fetched_at_ms": Utc::now().timestamp_millis(),
                    "settings": settings,
                    "strategies": strategies
                }),
            }
        }
        ("GET", "/bot/speed") => {
            let filtered = specs
                .iter()
                .filter(|spec| spec.group == "speed")
                .cloned()
                .collect::<Vec<_>>();
            BotHttpResponse {
                status_code: 200,
                status_text: "OK",
                body: json!({
                    "ok": true,
                    "section": "speed",
                    "fetched_at_ms": Utc::now().timestamp_millis(),
                    "settings": settings_snapshot(filtered.as_slice(), config_ref)
                }),
            }
        }
        ("GET", "/bot/risk") => {
            let filtered = specs
                .iter()
                .filter(|spec| spec.group == "risk")
                .cloned()
                .collect::<Vec<_>>();
            BotHttpResponse {
                status_code: 200,
                status_text: "OK",
                body: json!({
                    "ok": true,
                    "section": "risk",
                    "fetched_at_ms": Utc::now().timestamp_millis(),
                    "settings": settings_snapshot(filtered.as_slice(), config_ref)
                }),
            }
        }
        ("GET", "/bot/health") => {
            let payload = health_indicators(ctx).await;
            BotHttpResponse {
                status_code: 200,
                status_text: "OK",
                body: payload,
            }
        }
        ("GET", "/ui/summary") | ("GET", "/bot/ui/summary") => {
            let payload = build_ui_dashboard_summary(ctx).await;
            BotHttpResponse {
                status_code: 200,
                status_text: "OK",
                body: json!({
                    "ok": true,
                    "summary": payload
                }),
            }
        }
        ("GET", "/ui/strategies") | ("GET", "/bot/ui/strategies") => {
            let strategies = build_ui_strategy_states(ctx).await;
            BotHttpResponse {
                status_code: 200,
                status_text: "OK",
                body: json!({
                    "ok": true,
                    "strategies": strategies
                }),
            }
        }
        ("GET", "/bot/strategy") => {
            let strategies = strategy_catalog()
                .into_iter()
                .map(|(slug, strategy_id, aliases)| {
                    let settings = strategy_settings_snapshot(slug, specs.as_slice(), config_ref);
                    let enabled = strategy_enable_key(slug)
                        .and_then(|key| std::env::var(key).ok())
                        .and_then(|raw| parse_bool_raw(raw.as_str()))
                        .unwrap_or_else(|| strategy_enable_default(slug));
                    json!({
                        "slug": slug,
                        "strategy_id": strategy_id,
                        "aliases": aliases,
                        "enabled": enabled,
                        "settings": settings
                    })
                })
                .collect::<Vec<_>>();
            BotHttpResponse {
                status_code: 200,
                status_text: "OK",
                body: json!({
                    "ok": true,
                    "section": "strategy",
                    "fetched_at_ms": Utc::now().timestamp_millis(),
                    "strategies": strategies
                }),
            }
        }
        ("POST", "/bot/set") => {
            let payload = match parse_body_or_default::<BotSetRequest>(body) {
                Ok(parsed) => parsed,
                Err(e) => return Some(bad_request(e.to_string())),
            };
            if payload.changes.is_empty() {
                return Some(bad_request("changes must be non-empty"));
            }
            if payload.changes.len() > 200 {
                return Some(bad_request("changes exceeds max supported size (200)"));
            }
            let default_persist = payload.persist.unwrap_or(true);
            let env_file = safe_env_file_name(payload.env_file.as_deref());
            let result = apply_changes_internal(
                specs.as_slice(),
                config_ref,
                payload.changes,
                default_persist,
                env_file,
            );
            BotHttpResponse {
                status_code: 200,
                status_text: "OK",
                body: json!({
                    "ok": result.get("ok").and_then(Value::as_bool).unwrap_or(false),
                    "section": "set",
                    "fetched_at_ms": Utc::now().timestamp_millis(),
                    "env_file": env_file,
                    "result": result
                }),
            }
        }
        ("GET", "/bot/doctor") => {
            let issues = collect_doctor_issues(ctx, specs.as_slice()).await;
            let severity_counts =
                issues
                    .iter()
                    .fold(BTreeMap::<String, u64>::new(), |mut acc, issue| {
                        *acc.entry(issue.severity.to_string()).or_insert(0) += 1;
                        acc
                    });
            BotHttpResponse {
                status_code: 200,
                status_text: "OK",
                body: json!({
                    "ok": true,
                    "section": "doctor",
                    "fetched_at_ms": Utc::now().timestamp_millis(),
                    "issues": issues.iter().map(issue_to_json).collect::<Vec<_>>(),
                    "severity_counts": severity_counts,
                    "autofix_available": issues.iter().any(|issue| issue.auto_fixable)
                }),
            }
        }
        ("POST", "/bot/audit/fix") => {
            let payload = match parse_body_or_default::<BotAuditFixRequest>(body) {
                Ok(parsed) => parsed,
                Err(e) => return Some(bad_request(e.to_string())),
            };
            let issues_before = collect_doctor_issues(ctx, specs.as_slice()).await;
            let selected_issues = if let Some(ids) = payload.issue_ids.as_ref() {
                let set = ids
                    .iter()
                    .map(|id| id.trim().to_string())
                    .filter(|id| !id.is_empty())
                    .collect::<std::collections::HashSet<_>>();
                issues_before
                    .into_iter()
                    .filter(|issue| set.contains(issue.id.as_str()))
                    .collect::<Vec<_>>()
            } else {
                issues_before
            };
            let env_file = safe_env_file_name(payload.env_file.as_deref());
            let fix_changes = selected_issues
                .iter()
                .filter(|issue| issue.auto_fixable)
                .filter_map(|issue| issue.recommended_fix.as_ref())
                .filter_map(|fix| {
                    let key = fix.get("key")?.as_str()?.to_string();
                    let value = fix.get("value")?.clone();
                    Some(BotSetChange {
                        key,
                        value,
                        persist: None,
                    })
                })
                .collect::<Vec<_>>();
            let has_fix_changes = !fix_changes.is_empty();

            let result = if !has_fix_changes {
                json!({
                    "ok": true,
                    "message": "no_auto_fixable_issues",
                    "applied": 0,
                    "failed": 0,
                    "results": []
                })
            } else {
                apply_changes_internal(
                    specs.as_slice(),
                    config_ref,
                    fix_changes,
                    payload.persist.unwrap_or(true),
                    env_file,
                )
            };

            let remote_alpha_issue_ids = selected_issues
                .iter()
                .filter(|issue| is_remote_alpha_missing_issue(issue))
                .map(|issue| issue.id.clone())
                .collect::<Vec<_>>();
            let run_onboard_requested = payload.run_onboard_if_needed.unwrap_or(false);
            let should_run_onboard = run_onboard_requested && !remote_alpha_issue_ids.is_empty();
            let onboard_result = if should_run_onboard {
                Some(run_onboard_refill(
                    ctx,
                    env_file,
                    payload.onboard_api_base.as_deref(),
                    payload.onboard_skip_approvals.unwrap_or(true),
                    payload.onboard_skip_retention_cron.unwrap_or(true),
                ))
            } else {
                None
            };

            let issues_after = collect_doctor_issues(ctx, specs.as_slice()).await;
            let remaining_remote_alpha_issue_ids = issues_after
                .iter()
                .filter(|issue| is_remote_alpha_missing_issue(issue))
                .map(|issue| issue.id.clone())
                .collect::<Vec<_>>();
            let onboard_ok = onboard_result
                .as_ref()
                .and_then(|value| value.get("ok"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let overall_ok =
                result.get("ok").and_then(Value::as_bool).unwrap_or(false) && onboard_ok;
            let message = if !has_fix_changes && onboard_result.is_none() {
                "no_auto_fixable_issues"
            } else if !has_fix_changes && onboard_result.is_some() {
                "onboard_refill_attempted"
            } else if onboard_result.is_some() {
                "auto_fix_applied_and_onboard_refill_attempted"
            } else {
                "auto_fix_applied"
            };

            BotHttpResponse {
                status_code: 200,
                status_text: "OK",
                body: json!({
                    "ok": overall_ok,
                    "section": "audit_fix",
                    "fetched_at_ms": Utc::now().timestamp_millis(),
                    "env_file": env_file,
                    "message": message,
                    "result": result,
                    "onboard": onboard_result,
                    "onboard_requested": run_onboard_requested,
                    "onboard_remote_alpha_issue_ids": remote_alpha_issue_ids,
                    "remaining_remote_alpha_issue_ids": remaining_remote_alpha_issue_ids
                }),
            }
        }
        ("POST", "/bot/reset") => {
            let payload = match parse_body_or_default::<BotResetRequest>(body) {
                Ok(parsed) => parsed,
                Err(e) => return Some(bad_request(e.to_string())),
            };
            let scope = payload
                .scope
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase())
                .unwrap_or_else(|| "all".to_string());
            if scope == "all" && !payload.confirm.unwrap_or(false) {
                return Some(bad_request("scope=all requires confirm=true"));
            }
            let selected = match pick_specs_for_scope(
                specs.as_slice(),
                scope.as_str(),
                payload.key.as_deref(),
                payload.group.as_deref(),
                payload.strategy.as_deref(),
            ) {
                Ok(selected) => selected,
                Err(e) => return Some(bad_request(e.to_string())),
            };
            let changes = selected
                .into_iter()
                .map(|spec| BotSetChange {
                    key: spec.key.to_string(),
                    value: default_value_for_spec(spec),
                    persist: None,
                })
                .collect::<Vec<_>>();
            let env_file = safe_env_file_name(payload.env_file.as_deref());
            let result = apply_changes_internal(
                specs.as_slice(),
                config_ref,
                changes,
                payload.persist.unwrap_or(true),
                env_file,
            );
            BotHttpResponse {
                status_code: 200,
                status_text: "OK",
                body: json!({
                    "ok": result.get("ok").and_then(Value::as_bool).unwrap_or(false),
                    "section": "reset",
                    "scope": scope,
                    "fetched_at_ms": Utc::now().timestamp_millis(),
                    "env_file": env_file,
                    "result": result
                }),
            }
        }
        _ => {
            if method == "GET" && path.starts_with("/bot/strategy/") {
                let raw = path.trim_start_matches("/bot/strategy/");
                let normalized = match normalize_strategy_slug(raw) {
                    Some(strategy) => strategy,
                    None => {
                        return Some(BotHttpResponse {
                            status_code: 404,
                            status_text: "Not Found",
                            body: json!({"ok": false, "error": "unknown_strategy"}),
                        });
                    }
                };
                let strategy_row = strategy_catalog()
                    .into_iter()
                    .find(|(slug, _, _)| *slug == normalized);
                let Some((slug, strategy_id, aliases)) = strategy_row else {
                    return Some(BotHttpResponse {
                        status_code: 404,
                        status_text: "Not Found",
                        body: json!({"ok": false, "error": "unknown_strategy"}),
                    });
                };
                let enabled = strategy_enable_key(slug)
                    .and_then(|key| std::env::var(key).ok())
                    .and_then(|raw| parse_bool_raw(raw.as_str()))
                    .unwrap_or_else(|| strategy_enable_default(slug));
                let settings = strategy_settings_snapshot(slug, specs.as_slice(), config_ref);
                BotHttpResponse {
                    status_code: 200,
                    status_text: "OK",
                    body: json!({
                        "ok": true,
                        "section": "strategy",
                        "fetched_at_ms": Utc::now().timestamp_millis(),
                        "strategy": {
                            "slug": slug,
                            "strategy_id": strategy_id,
                            "aliases": aliases,
                            "enabled": enabled,
                            "settings": settings
                        }
                    }),
                }
            } else {
                BotHttpResponse {
                    status_code: 404,
                    status_text: "Not Found",
                    body: json!({"ok": false, "error": "not_found"}),
                }
            }
        }
    };

    Some(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_number() -> BotSettingSpec {
        BotSettingSpec {
            key: "TEST_NUM",
            group: "speed",
            strategy: None,
            value_type: BotSettingType::Number,
            default_raw: "10",
            min: Some(1.0),
            max: Some(20.0),
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "test",
        }
    }

    #[test]
    fn parse_raw_enforces_numeric_bounds() {
        let spec = spec_number();
        assert!(parse_raw_to_value(&spec, "5").is_ok());
        assert!(parse_raw_to_value(&spec, "0.1").is_err());
        assert!(parse_raw_to_value(&spec, "25").is_err());
    }

    #[test]
    fn parse_raw_bool_accepts_common_tokens() {
        let spec = BotSettingSpec {
            key: "TEST_BOOL",
            group: "strategy",
            strategy: None,
            value_type: BotSettingType::Bool,
            default_raw: "false",
            min: None,
            max: None,
            enum_values: &[],
            mutable: true,
            restart_required: true,
            config_fallback_key: None,
            description: "test",
        };
        assert_eq!(parse_raw_to_value(&spec, "true").ok(), Some(json!(true)));
        assert_eq!(parse_raw_to_value(&spec, "1").ok(), Some(json!(true)));
        assert_eq!(parse_raw_to_value(&spec, "no").ok(), Some(json!(false)));
        assert!(parse_raw_to_value(&spec, "maybe").is_err());
    }

    #[test]
    fn persist_env_value_upserts_key() {
        let tmp_dir = std::env::temp_dir();
        let file_name = format!(
            "evpoly-bot-admin-test-{}.env",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let path = tmp_dir.join(file_name);
        let path_str = path.to_string_lossy().to_string();

        persist_env_value(path_str.as_str(), "FOO", "bar").expect("initial write");
        persist_env_value(path_str.as_str(), "FOO", "baz").expect("update write");

        let content = std::fs::read_to_string(&path).expect("read env file");
        assert!(content.contains("FOO=baz"));

        let _ = std::fs::remove_file(path);
    }
}
