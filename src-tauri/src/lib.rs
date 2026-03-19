pub mod auth;
pub mod bot_manager;
pub mod config_io;
pub mod crypto_vault;
pub mod log_stream;
pub mod notifications;
pub mod onboard;
pub mod profile_manager;
pub mod wallet_rpc;

use crate::auth::AppAuth;
use crate::bot_manager::BotManager;
use crate::profile_manager::{Profile, ProfileManager};

use chrono::{TimeZone, Utc};
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
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

const DESKTOP_SYMBOL_ORDER: [&str; 7] = ["BTC", "ETH", "SOL", "XRP", "DOGE", "BNB", "HYPE"];

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

fn build_runtime_paths(
    pm: &ProfileManager,
    auth: &AppAuth,
    data_dir: &Path,
) -> Result<(PathBuf, PathBuf), String> {
    let active_id = pm.get_active_profile_id().ok_or("no active profile set")?;
    let profile = pm
        .get_profile(&active_id)
        .ok_or("active profile not found")?;
    let secrets = decrypt_profile_secrets(&profile, auth)?;
    if !secrets.contains_key("POLY_PRIVATE_KEY") {
        return Err("missing POLY_PRIVATE_KEY in profile secrets".to_string());
    }
    let env_path =
        config_io::generate_env_file(&profile, &secrets, data_dir).map_err(|e| e.to_string())?;
    let config_path =
        config_io::generate_config_json(&profile, data_dir).map_err(|e| e.to_string())?;
    Ok((env_path, config_path))
}

fn desktop_config_to_profile_payload(
    config: &DesktopConfig,
) -> (Value, Value, HashMap<String, String>, String, u8) {
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

    (
        Value::Object(strategy),
        Value::Object(sizing),
        secrets,
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

    let mut symbols = vec!["BTC".to_string()];
    if bool_from_object(&strategy, "POLY_ENABLE_ETH_TRADING", true) {
        symbols.push("ETH".to_string());
    }
    if bool_from_object(&strategy, "POLY_ENABLE_SOLANA_TRADING", true) {
        symbols.push("SOL".to_string());
    }
    if bool_from_object(&strategy, "POLY_ENABLE_XRP_TRADING", true) {
        symbols.push("XRP".to_string());
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
        "proxy_wallet": profile.wallet_address,
        "sig_type": profile.signature_type,
        "symbols": symbols,
        "strategies": {
            "premarket": bool_from_object(&strategy, "EVPOLY_STRATEGY_PREMARKET_ENABLE", true),
            "endgame": bool_from_object(&strategy, "EVPOLY_STRATEGY_ENDGAME_ENABLE", true),
            "evcurve": bool_from_object(&strategy, "EVPOLY_STRATEGY_EVCURVE_ENABLE", true),
            "session_band": bool_from_object(&strategy, "EVPOLY_STRATEGY_SESSIONBAND_ENABLE", true),
            "evsnipe": bool_from_object(&strategy, "EVPOLY_STRATEGY_EVSNIPE_ENABLE", true),
            "mm_rewards": bool_from_object(&strategy, "EVPOLY_STRATEGY_MM_REWARDS_ENABLE", false),
            "mm_sport": bool_from_object(&strategy, "EVPOLY_STRATEGY_MM_SPORT_ENABLE", false)
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
            "rewards_min_share_multiple": f64_from_object(&strategy, "EVPOLY_MM_REWARD_MIN_TARGET_MULT", 2.0),
            "sport_quote_size_multiplier": f64_from_object(&strategy, "EVPOLY_MM_SPORT_QUOTE_SIZE_MULT", 1.2)
        },
        "simulation": bool_from_object(&sizing, "APP_SIMULATION", true),
        "relayer_api_key": secrets.get("RELAYER_API_KEY").cloned().unwrap_or_default(),
        "relayer_api_key_address": secrets.get("RELAYER_API_KEY_ADDRESS").cloned().unwrap_or_default(),
        "remote_signer_token": secrets.get("EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN").cloned().unwrap_or_default()
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
    wallet_address: String,
    signature_type: u8,
) -> Result<Profile, String> {
    profiles
        .lock()
        .map_err(|e| e.to_string())?
        .create_profile(name, wallet_address, signature_type)
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
        let auth = auth.lock().map_err(|e| e.to_string())?;
        build_runtime_paths(&pm, &auth, &data_dir.0)?
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
        let auth = auth.lock().map_err(|e| e.to_string())?;
        build_runtime_paths(&pm, &auth, &data_dir.0)?
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

// ── Config ───────────────────────────────────────────────────────────

#[tauri::command]
fn save_config(
    auth: State<'_, AuthState>,
    profiles: State<'_, ProfileState>,
    profile_id: String,
    config: DesktopConfig,
) -> Result<(), String> {
    let pm = profiles.lock().map_err(|e| e.to_string())?;
    let auth = auth.lock().map_err(|e| e.to_string())?;
    let mut profile = pm.get_profile(&profile_id).ok_or("profile not found")?;

    let (strategy_config, sizing_config, new_secrets, wallet_address, signature_type) =
        desktop_config_to_profile_payload(&config);

    profile.strategy_config = strategy_config;
    profile.sizing_config = sizing_config;
    if !wallet_address.is_empty() {
        profile.wallet_address = wallet_address;
    }
    profile.signature_type = signature_type;

    let mut merged_secrets = if profile.encrypted_secrets.trim().is_empty() {
        HashMap::new()
    } else {
        decrypt_profile_secrets(&profile, &auth)?
    };
    for (k, v) in new_secrets {
        merged_secrets.insert(k, v);
    }
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
    pm.update_profile(profile).map_err(|e| e.to_string())
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
) -> Result<(), String> {
    let decrypted = crypto_vault::decrypt_data(&data, &password).map_err(|e| e.to_string())?;
    let imported: Profile = serde_json::from_slice(&decrypted).map_err(|e| e.to_string())?;
    let pm = profiles.lock().map_err(|e| e.to_string())?;
    let mut created = pm
        .create_profile(
            imported.name,
            imported.wallet_address,
            imported.signature_type,
        )
        .map_err(|e| e.to_string())?;
    created.strategy_config = imported.strategy_config;
    created.sizing_config = imported.sizing_config;
    created.encrypted_secrets = imported.encrypted_secrets;
    pm.update_profile(created).map_err(|e| e.to_string())
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

    let mut stmt = match conn.prepare(
        "SELECT token_id, \
                (COALESCE(entry_units, 0.0) - COALESCE(exit_units, 0.0) - COALESCE(inventory_consumed_units, 0.0)) AS net_units, \
                CASE WHEN COALESCE(entry_units, 0.0) > 0.0 THEN COALESCE(entry_notional_usd, 0.0) / entry_units ELSE 0.0 END AS avg_entry_price, \
                COALESCE(realized_pnl_usd, 0.0) \
         FROM positions_v2 \
         WHERE status='OPEN' \
           AND (COALESCE(entry_units, 0.0) - COALESCE(exit_units, 0.0) - COALESCE(inventory_consumed_units, 0.0)) > 1e-9",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let rows = stmt.query_map([], |row| {
        let token_id: String = row.get(0)?;
        let size: f64 = row.get(1)?;
        let entry: f64 = row.get(2)?;
        let upnl: f64 = row.get(3)?;
        Ok(serde_json::json!({
            "market": token_id,
            "side": "buy",
            "token_id": token_id,
            "size": size,
            "entry_price": entry,
            "current_price": entry,
            "pnl": upnl,
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
        p.wallet_address.clone()
    };
    wallet_rpc::fetch_usdc_balance("https://1rpc.io/matic", &wallet).await
}

#[tauri::command]
fn get_data_dir_path(data_dir: State<'_, AppDataDir>) -> String {
    data_dir.0.to_string_lossy().to_string()
}

// ── Onboard ──────────────────────────────────────────────────────────

#[tauri::command]
async fn run_onboarding(
    wallet: String,
    private_key: String,
    signature_type: u8,
    proxy_wallet: String,
) -> Result<serde_json::Value, String> {
    let result =
        onboard::run_onboarding(&wallet, &private_key, signature_type, &proxy_wallet).await?;
    serde_json::to_value(result).map_err(|e| e.to_string())
}

// ── App entry ────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let data_dir = dirs::data_dir()
        .expect("cannot determine data directory")
        .join("evpoly");
    std::fs::create_dir_all(&data_dir).expect("cannot create data directory");

    let auth: AuthState = Arc::new(Mutex::new(AppAuth::new(data_dir.clone())));
    let profiles: ProfileState = Arc::new(Mutex::new(ProfileManager::new(data_dir.clone())));
    let bot: BotState = Arc::new(Mutex::new(BotManager::new(data_dir.clone())));

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppDataDir(data_dir))
        .manage(auth)
        .manage(profiles)
        .manage(bot)
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
                        let configs = || -> Option<(PathBuf, PathBuf)> {
                            let pm = ps.lock().ok()?;
                            let auth = auth.lock().ok()?;
                            build_runtime_paths(&pm, &auth, &dd.0).ok()
                        };
                        if let Some((env_path, config_path)) = configs() {
                            if let Ok(bm) = bs.lock() {
                                let _ = bm.start(app, env_path, config_path, false);
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
            save_config,
            get_saved_config,
            export_config,
            import_config,
            get_trade_stats,
            get_recent_trades,
            get_open_positions,
            get_wallet_balance,
            get_data_dir_path,
            run_onboarding,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
