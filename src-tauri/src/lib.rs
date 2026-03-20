pub mod auth;
pub mod bot_manager;
pub mod config_io;
pub mod crypto_vault;
pub mod log_stream;
pub mod manual_manager;
pub mod notifications;
pub mod onboard;
pub mod profile_manager;
pub mod wallet_rpc;

use crate::auth::AppAuth;
use crate::bot_manager::BotManager;
use crate::manual_manager::ManualServiceManager;
use crate::profile_manager::{Profile, ProfileManager};

use chrono::{TimeZone, Utc};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde_json::{Map, Value};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State};

struct AppDataDir(PathBuf);

type AuthState = Arc<Mutex<AppAuth>>;
type ProfileState = Arc<Mutex<ProfileManager>>;
type BotState = Arc<Mutex<BotManager>>;
type ManualState = Arc<Mutex<ManualServiceManager>>;

const DESKTOP_SYMBOL_ORDER: [&str; 7] = ["BTC", "ETH", "SOL", "XRP", "DOGE", "BNB", "HYPE"];
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
const MANUAL_DEBUG_LOG_NAME: &str = "evpoly-manual-debug.log.txt";

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
struct DesktopConfig {
    private_key: String,
    eoa_wallet: String,
    proxy_wallet: String,
    sig_type: u8,
    symbols: Vec<String>,
    strategies: DesktopStrategies,
    sizing: DesktopSizing,
    caps: DesktopCaps,
    mm_tuning: DesktopMmTuning,
    simulation: bool,
    relayer_api_key: String,
    relayer_api_key_address: String,
    remote_signer_token: String,
    remote_discovery_token: String,
    remote_premarket_alpha_token: String,
    remote_endgame_alpha_token: String,
    remote_mm_rewards_alpha_token: String,
    remote_evsnipe_discovery_token: String,
    admin_api_token: String,
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

fn default_desktop_config(eoa_wallet: String, proxy_wallet: String, sig_type: u8) -> DesktopConfig {
    DesktopConfig {
        private_key: String::new(),
        eoa_wallet,
        proxy_wallet,
        sig_type,
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
            premarket: 10.0,
            endgame: 10.0,
            evcurve: 10.0,
            session_band: 10.0,
            evsnipe_per_hit: 10.0,
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
        simulation: config_io::env_template_default_bool("APP_SIMULATION", true),
        relayer_api_key: String::new(),
        relayer_api_key_address: String::new(),
        remote_signer_token: String::new(),
        remote_discovery_token: String::new(),
        remote_premarket_alpha_token: String::new(),
        remote_endgame_alpha_token: String::new(),
        remote_mm_rewards_alpha_token: String::new(),
        remote_evsnipe_discovery_token: String::new(),
        admin_api_token: String::new(),
    }
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

fn decrypt_profile_secrets(
    profile: &Profile,
    auth: &AppAuth,
) -> Result<HashMap<String, String>, String> {
    if profile.encrypted_secrets.trim().is_empty() {
        return Ok(HashMap::new());
    }
    let password = auth
        .session_password()
        .ok_or("session locked: unlock app again before using secrets")?;
    let decrypted =
        crypto_vault::decrypt_data(profile.encrypted_secrets.as_str(), password.as_str())
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

fn simulation_mode_from_profile(profile: &Profile) -> bool {
    profile
        .sizing_config
        .as_object()
        .and_then(|obj| obj.get("APP_SIMULATION"))
        .and_then(Value::as_bool)
        .unwrap_or_else(|| config_io::env_template_default_bool("APP_SIMULATION", true))
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

fn desktop_config_to_profile_payload(
    config: &DesktopConfig,
) -> (Value, Value, HashMap<String, String>, String, String, u8) {
    let mut strategy = Map::new();
    let mut sizing = Map::new();
    let mut secrets = HashMap::new();

    let symbols = normalize_symbols(&config.symbols);
    let symbol_csv = symbols.join(",");
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
        Value::String(symbol_csv.clone()),
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
        secrets.insert(
            "EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN".to_string(),
            config.remote_signer_token.trim().to_string(),
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
    let default_simulation = config_io::env_template_default_bool("APP_SIMULATION", true);

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
    symbols = normalize_symbols(&symbols);

    let secrets = decrypt_profile_secrets(profile, auth)?;

    Ok(serde_json::json!({
        "private_key": secrets.get("POLY_PRIVATE_KEY").cloned().unwrap_or_default(),
        "eoa_wallet": profile.eoa_wallet_address.clone(),
        "proxy_wallet": profile.proxy_wallet_address.clone(),
        "sig_type": profile.signature_type,
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
            "endgame": f64_from_object(&sizing, "EVPOLY_ENDGAME_BASE_SIZE_USD", 10.0),
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
        "simulation": bool_from_object(&sizing, "APP_SIMULATION", default_simulation),
        "relayer_api_key": secrets.get("RELAYER_API_KEY").cloned().unwrap_or_default(),
        "relayer_api_key_address": secrets.get("RELAYER_API_KEY_ADDRESS").cloned().unwrap_or_default(),
        "remote_signer_token": secrets.get("EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN")
            .cloned()
            .or_else(|| secrets.get("EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN").cloned())
            .unwrap_or_default(),
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
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
        {
            let _ = file.write_all(content.as_bytes());
        }
    }
}

fn ensure_debug_log_files(data_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(data_dir).map_err(|e| format!("prepare logs folder: {e}"))?;
    for file_name in [
        DESKTOP_DEBUG_LOG_NAME,
        FULL_DEBUG_LOG_NAME,
        BOT_DEBUG_LOG_NAME,
        MANUAL_DEBUG_LOG_NAME,
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

// ── Auth ─────────────────────────────────────────────────────────────

#[tauri::command]
fn is_auth_initialized(auth: State<'_, AuthState>) -> bool {
    auth.lock().map(|a| a.is_initialized()).unwrap_or(false)
}

#[tauri::command]
fn set_password(auth: State<'_, AuthState>, password: String) -> Result<(), String> {
    auth.lock()
        .map_err(|e| e.to_string())?
        .set_password(&password)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn verify_password(auth: State<'_, AuthState>, password: String) -> Result<bool, String> {
    auth.lock()
        .map_err(|e| e.to_string())?
        .verify_password(&password)
        .map_err(|e| e.to_string())
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
    eoa_wallet_address: String,
    proxy_wallet_address: String,
    signature_type: u8,
) -> Result<Profile, String> {
    let default_config = default_desktop_config(
        eoa_wallet_address.trim().to_string(),
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
    simulation: bool,
) -> Result<(), String> {
    let (env_path, config_path) = {
        let pm = profiles.lock().map_err(|e| e.to_string())?;
        let profile = persist_active_profile_simulation_mode(&pm, simulation)?;
        let auth = auth.lock().map_err(|e| e.to_string())?;
        build_runtime_paths_for_profile(&profile, &auth, &data_dir.0)?
    };
    bot.lock()
        .map_err(|e| e.to_string())?
        .start(&app, env_path, config_path, simulation)
}

#[tauri::command]
fn stop_bot(bot: State<'_, BotState>) -> Result<(), String> {
    bot.lock().map_err(|e| e.to_string())?.stop()
}

#[tauri::command]
fn restart_bot(
    app: AppHandle,
    bot: State<'_, BotState>,
    profiles: State<'_, ProfileState>,
    auth: State<'_, AuthState>,
    data_dir: State<'_, AppDataDir>,
    simulation: bool,
) -> Result<(), String> {
    let (env_path, config_path) = {
        let pm = profiles.lock().map_err(|e| e.to_string())?;
        let profile = persist_active_profile_simulation_mode(&pm, simulation)?;
        let auth = auth.lock().map_err(|e| e.to_string())?;
        build_runtime_paths_for_profile(&profile, &auth, &data_dir.0)?
    };
    bot.lock()
        .map_err(|e| e.to_string())?
        .restart(&app, env_path, config_path, simulation)
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
fn get_log_lines(bot: State<'_, BotState>, count: usize) -> Vec<serde_json::Value> {
    let bm = match bot.lock() {
        Ok(bm) => bm,
        Err(_) => return vec![],
    };
    let log_buf = bm.get_log_buffer();
    let buf = match log_buf.lock() {
        Ok(buf) => buf,
        Err(_) => return vec![],
    };
    buf.get_lines(count)
        .into_iter()
        .map(|line| {
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
        })
        .collect()
}

#[tauri::command]
fn start_manual_service(
    app: AppHandle,
    manual: State<'_, ManualState>,
    profiles: State<'_, ProfileState>,
    auth: State<'_, AuthState>,
    data_dir: State<'_, AppDataDir>,
    simulation: bool,
    port: Option<u16>,
) -> Result<(), String> {
    let (env_path, config_path) = {
        let pm = profiles.lock().map_err(|e| e.to_string())?;
        let profile = persist_active_profile_simulation_mode(&pm, simulation)?;
        let auth = auth.lock().map_err(|e| e.to_string())?;
        build_runtime_paths_for_profile(&profile, &auth, &data_dir.0)?
    };
    manual
        .lock()
        .map_err(|e| e.to_string())?
        .start(&app, env_path, config_path, simulation, port)
}

#[tauri::command]
fn stop_manual_service(manual: State<'_, ManualState>) -> Result<(), String> {
    manual.lock().map_err(|e| e.to_string())?.stop()
}

#[tauri::command]
fn get_manual_service_status(manual: State<'_, ManualState>) -> String {
    match manual.lock().map(|m| m.get_status()).unwrap_or(
        manual_manager::ManualServiceStatus::Error("lock failed".to_string()),
    ) {
        manual_manager::ManualServiceStatus::Stopped => "stopped".to_string(),
        manual_manager::ManualServiceStatus::Starting => "starting".to_string(),
        manual_manager::ManualServiceStatus::Running => "running".to_string(),
        manual_manager::ManualServiceStatus::Stopping => "stopping".to_string(),
        manual_manager::ManualServiceStatus::Error(e) => format!("error:{e}"),
    }
}

#[tauri::command]
fn get_manual_log_lines(manual: State<'_, ManualState>, count: usize) -> Vec<serde_json::Value> {
    let mgr = match manual.lock() {
        Ok(mgr) => mgr,
        Err(_) => return vec![],
    };
    let log_buf = mgr.get_log_buffer();
    let buf = match log_buf.lock() {
        Ok(buf) => buf,
        Err(_) => return vec![],
    };
    buf.get_lines(count)
        .into_iter()
        .map(|line| {
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
        })
        .collect()
}

#[tauri::command]
async fn manual_api_request(
    manual: State<'_, ManualState>,
    method: String,
    path: String,
    query: Option<serde_json::Value>,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let request_ctx = {
        let mgr = manual.lock().map_err(|e| e.to_string())?;
        mgr.request_context()?
    };
    manual_manager::send_manual_request(request_ctx, method, path, query, body).await
}

// ── Config ───────────────────────────────────────────────────────────

#[tauri::command]
fn save_config(
    auth: State<'_, AuthState>,
    profiles: State<'_, ProfileState>,
    data_dir: State<'_, AppDataDir>,
    profile_id: String,
    config: DesktopConfig,
) -> Result<(), String> {
    let pm = profiles.lock().map_err(|e| e.to_string())?;
    let auth = auth.lock().map_err(|e| e.to_string())?;
    let mut profile = pm.get_profile(&profile_id).ok_or("profile not found")?;

    let (
        strategy_config,
        sizing_config,
        new_secrets,
        eoa_wallet_address,
        proxy_wallet_address,
        signature_type,
    ) = desktop_config_to_profile_payload(&config);

    profile.strategy_config = strategy_config;
    profile.sizing_config = sizing_config;
    profile.eoa_wallet_address = eoa_wallet_address;
    profile.proxy_wallet_address = proxy_wallet_address;
    profile.signature_type = signature_type;
    profile.normalize_wallet_fields();

    let merged_secrets = if profile.encrypted_secrets.trim().is_empty() {
        HashMap::new()
    } else {
        decrypt_profile_secrets(&profile, &auth)?
    };
    let merged_secrets = merge_desktop_secrets(merged_secrets, new_secrets);
    if merged_secrets.is_empty() {
        profile.encrypted_secrets.clear();
    } else {
        let password = auth
            .session_password()
            .ok_or("session locked: unlock app before saving")?;
        let blob = serde_json::to_vec(&merged_secrets).map_err(|e| e.to_string())?;
        profile.encrypted_secrets =
            crypto_vault::encrypt_data(&blob, &password).map_err(|e| e.to_string())?;
    }
    pm.update_profile(profile.clone())
        .map_err(|e| e.to_string())?;
    config_io::generate_config_json(&profile, &data_dir.0).map_err(|e| e.to_string())?;
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
fn export_config(
    profiles: State<'_, ProfileState>,
    profile_id: String,
    password: String,
) -> Result<String, String> {
    let pm = profiles.lock().map_err(|e| e.to_string())?;
    let profile = pm.get_profile(&profile_id).ok_or("profile not found")?;
    let json = serde_json::to_string(&profile).map_err(|e| e.to_string())?;
    crypto_vault::encrypt_data(json.as_bytes(), &password).map_err(|e| e.to_string())
}

#[tauri::command]
fn import_config(
    profiles: State<'_, ProfileState>,
    data: String,
    password: String,
) -> Result<String, String> {
    let decrypted = crypto_vault::decrypt_data(&data, &password).map_err(|e| e.to_string())?;
    let mut imported: Profile = serde_json::from_slice(&decrypted).map_err(|e| e.to_string())?;
    imported.normalize_wallet_fields();
    let pm = profiles.lock().map_err(|e| e.to_string())?;
    let mut created = pm
        .create_profile(
            imported.name,
            imported.eoa_wallet_address,
            imported.proxy_wallet_address,
            imported.signature_type,
            imported.strategy_config.clone(),
            imported.sizing_config.clone(),
        )
        .map_err(|e| e.to_string())?;
    created.encrypted_secrets = imported.encrypted_secrets;
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
    let (ack_sample_count, avg_ack_latency_ms): (i64, Option<f64>) = conn
        .query_row(
            "SELECT \
                COALESCE(SUM(CASE WHEN ack_ts_ms IS NOT NULL AND submit_ts_ms IS NOT NULL AND ack_ts_ms >= submit_ts_ms THEN 1 ELSE 0 END), 0), \
                AVG(CASE WHEN ack_ts_ms IS NOT NULL AND submit_ts_ms IS NOT NULL AND ack_ts_ms >= submit_ts_ms THEN CAST(ack_ts_ms - submit_ts_ms AS REAL) END) \
             FROM strategy_feature_snapshots_v1",
            [],
            |row| Ok((row.get(0)?, row.get::<_, Option<f64>>(1)?)),
        )
        .unwrap_or((0, None));

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
    wallet_rpc::fetch_usdc_balance_with_fallback(
        &[
            config_io::DEFAULT_POLYGON_RPC_URL,
            config_io::DEFAULT_POLYGON_RPC_FALLBACK_URL,
        ],
        &wallet,
    )
    .await
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
    wallet: String,
    private_key: String,
    signature_type: u8,
    proxy_wallet: String,
) -> Result<serde_json::Value, String> {
    append_desktop_debug_line(
        &data_dir.0,
        "ONBOARD",
        format!(
            "run_onboarding start wallet_provided={} signature_type={} proxy_wallet_set={}",
            !wallet.trim().is_empty(),
            signature_type,
            !proxy_wallet.trim().is_empty()
        )
        .as_str(),
    );

    let result = onboard::run_onboarding(&wallet, &private_key, signature_type, &proxy_wallet)
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
    let _ = ensure_debug_log_files(&data_dir);
    append_desktop_debug_line(&data_dir, "SYSTEM", "desktop app startup");

    let auth: AuthState = Arc::new(Mutex::new(AppAuth::new(data_dir.clone())));
    let profiles: ProfileState = Arc::new(Mutex::new(ProfileManager::new(data_dir.clone())));
    let bot: BotState = Arc::new(Mutex::new(BotManager::new(data_dir.clone())));
    let manual: ManualState = Arc::new(Mutex::new(ManualServiceManager::new(data_dir.clone())));

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppDataDir(data_dir))
        .manage(auth)
        .manage(profiles)
        .manage(bot)
        .manage(manual)
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
                        let configs = || -> Option<(PathBuf, PathBuf, bool)> {
                            let pm = ps.lock().ok()?;
                            let auth = auth.lock().ok()?;
                            let profile = active_profile(&pm).ok()?;
                            let simulation = simulation_mode_from_profile(&profile);
                            let (env_path, config_path) =
                                build_runtime_paths_for_profile(&profile, &auth, &dd.0).ok()?;
                            Some((env_path, config_path, simulation))
                        };
                        if let Some((env_path, config_path, simulation)) = configs() {
                            if let Ok(bm) = bs.lock() {
                                let _ = bm.start(app, env_path, config_path, simulation);
                            }
                        }
                    }
                    "stop" => {
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
                        if let Ok(bm) = app.state::<BotState>().lock() {
                            let _ = bm.stop();
                        }
                        if let Ok(mm) = app.state::<ManualState>().lock() {
                            let _ = mm.stop();
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
            set_password,
            verify_password,
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
            start_manual_service,
            stop_manual_service,
            get_manual_service_status,
            get_manual_log_lines,
            manual_api_request,
            save_config,
            get_saved_config,
            export_config,
            import_config,
            get_trade_stats,
            get_recent_trades,
            get_open_positions,
            get_wallet_balance,
            get_data_dir_path,
            open_logs_folder,
            run_onboarding,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{merge_desktop_secrets, simulation_mode_from_profile};
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
    fn simulation_mode_from_profile_prefers_saved_value() {
        let profile = profile_with_simulation(Some(false));
        assert!(!simulation_mode_from_profile(&profile));
    }

    #[test]
    fn simulation_mode_from_profile_falls_back_to_template_default() {
        let profile = profile_with_simulation(None);
        assert_eq!(
            simulation_mode_from_profile(&profile),
            config_io::env_template_default_bool("APP_SIMULATION", true)
        );
    }
}
