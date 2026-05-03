#![allow(dead_code)]

use crate::profile_manager::Profile;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(crate) const DEFAULT_POLYGON_RPC_URL: &str = "https://1rpc.io/matic";
pub(crate) const DEFAULT_POLYGON_RPC_FALLBACK_URL: &str = "https://polygon-rpc.com";
pub(crate) const DESKTOP_POLYGON_RPC_URLS: [&str; 6] = [
    DEFAULT_POLYGON_RPC_URL,
    DEFAULT_POLYGON_RPC_FALLBACK_URL,
    "https://polygon.publicnode.com",
    "https://polygon.drpc.org",
    "https://tenderly.rpc.polygon.community",
    "https://polygon.api.onfinality.io/public",
];
pub(crate) const DEFAULT_MM_MARKET_MODE: &str = "auto";

const CORE_ENV_TEMPLATE: &str = include_str!("../core-contract/.env.example");
const PREMARKET_ALPHA_URL_KEY: &str = "EVPOLY_REMOTE_PREMARKET_ALPHA_URL";
const LEGACY_PREMARKET_SHOULD_TRADE_PATH: &str = "/v1/alpha/premarket/should-trade";
const CURRENT_PREMARKET_LADDER_PATH: &str = "/v1/alpha/premarket/ladder";

fn parse_env_template(template: &str) -> HashMap<String, String> {
    let mut env_map = HashMap::new();
    for line in template.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            env_map.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    env_map
}

fn core_env_defaults() -> &'static HashMap<String, String> {
    static CORE_ENV_DEFAULTS: OnceLock<HashMap<String, String>> = OnceLock::new();
    CORE_ENV_DEFAULTS.get_or_init(|| parse_env_template(CORE_ENV_TEMPLATE))
}

fn parse_env_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "y" | "on" => Some(true),
        "0" | "false" | "no" | "n" | "off" => Some(false),
        _ => None,
    }
}

pub(crate) fn env_template_default_bool(key: &str, default: bool) -> bool {
    core_env_defaults()
        .get(key)
        .and_then(|value| parse_env_bool(value))
        .unwrap_or(default)
}

pub(crate) fn env_template_default_string(key: &str) -> Option<String> {
    core_env_defaults()
        .get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn env_template_default_f64(key: &str, default: f64) -> f64 {
    core_env_defaults()
        .get(key)
        .and_then(|value| value.trim().parse::<f64>().ok())
        .unwrap_or(default)
}

fn value_to_env_string(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    if let Some(number) = v.as_f64() {
        if number.is_finite() && number.fract().abs() <= f64::EPSILON {
            return format!("{number:.0}");
        }
    }
    v.to_string()
}

fn bool_from_config(config: &Value, key: &str, default: bool) -> bool {
    config
        .as_object()
        .and_then(|obj| obj.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

fn nonempty_map_value(map: &HashMap<String, String>, key: &str) -> Option<String> {
    map.get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalized_mm_market_mode(env_map: &HashMap<String, String>) -> String {
    match env_map
        .get("EVPOLY_MM_MARKET_MODE")
        .map(|value| value.trim().to_ascii_lowercase())
    {
        Some(value) if value == "hybrid" => "hybrid".to_string(),
        Some(value) if value == "auto" || value == "target" => DEFAULT_MM_MARKET_MODE.to_string(),
        Some(value) if !value.is_empty() => DEFAULT_MM_MARKET_MODE.to_string(),
        _ => DEFAULT_MM_MARKET_MODE.to_string(),
    }
}

fn normalize_usize_min(env_map: &mut HashMap<String, String>, key: &str, minimum: usize) {
    let needs_update = env_map
        .get(key)
        .and_then(|value| value.trim().parse::<usize>().ok())
        .map(|value| value < minimum)
        .unwrap_or(true);
    if needs_update {
        env_map.insert(key.to_string(), minimum.to_string());
    }
}

fn normalize_usize_max(env_map: &mut HashMap<String, String>, key: &str, maximum: usize) {
    let needs_update = env_map
        .get(key)
        .and_then(|value| value.trim().parse::<usize>().ok())
        .map(|value| value > maximum)
        .unwrap_or(true);
    if needs_update {
        env_map.insert(key.to_string(), maximum.to_string());
    }
}

fn normalize_redemption_defaults(env_map: &mut HashMap<String, String>) {
    normalize_usize_min(env_map, "EVPOLY_REDEMPTION_MAX_CONDITIONS_PER_SWEEP", 10);
    normalize_usize_max(
        env_map,
        "EVPOLY_REDEMPTION_AUTO_TRIGGER_PENDING_THRESHOLD",
        3,
    );
}

fn normalize_premarket_alpha_url(value: &str) -> String {
    let trimmed = value.trim();
    let path = trimmed.trim_end_matches('/');
    if let Some(prefix) = path.strip_suffix(LEGACY_PREMARKET_SHOULD_TRADE_PATH) {
        return format!("{prefix}{CURRENT_PREMARKET_LADDER_PATH}");
    }
    trimmed.to_string()
}

fn normalize_remote_alpha_urls(env_map: &mut HashMap<String, String>) {
    if let Some(value) = env_map.get_mut(PREMARKET_ALPHA_URL_KEY) {
        *value = normalize_premarket_alpha_url(value);
    }
}

pub fn generate_env_file(
    profile: &Profile,
    secrets: &HashMap<String, String>,
    data_dir: &Path,
) -> Result<PathBuf> {
    cleanup_generated_env_files(data_dir);
    let mut env_map: HashMap<String, String> = HashMap::new();

    for line in CORE_ENV_TEMPLATE.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            env_map.insert(key.to_string(), value.to_string());
        }
    }

    env_map.insert(
        "POLY_SIGNATURE_TYPE".into(),
        profile.signature_type.to_string(),
    );
    env_map.insert(
        "POLY_PROXY_WALLET_ADDRESS".into(),
        profile.proxy_wallet_address.trim().to_string(),
    );
    let deposit_wallet = profile.deposit_wallet_address.trim().to_string();
    let funder_wallet = profile.primary_wallet_address();
    env_map.insert("POLY_DEPOSIT_WALLET_ADDRESS".into(), deposit_wallet.clone());
    env_map.insert("POLY_FUNDER_WALLET_ADDRESS".into(), funder_wallet.clone());
    if profile.signature_type == 3 {
        // Keep legacy runtime code paths pointed at the active funder until the
        // sidecar fully switches to POLY_FUNDER_WALLET_ADDRESS.
        env_map.insert("POLY_PROXY_WALLET_ADDRESS".into(), funder_wallet);
    }

    if let Some(obj) = profile.strategy_config.as_object() {
        for (k, v) in obj {
            env_map.insert(k.clone(), value_to_env_string(v));
        }
    }

    if let Some(obj) = profile.sizing_config.as_object() {
        for (k, v) in obj {
            env_map.insert(k.clone(), value_to_env_string(v));
        }
    }

    for (k, v) in secrets {
        env_map.insert(k.clone(), v.clone());
    }

    normalize_remote_alpha_urls(&mut env_map);

    let shared_alpha_token = [
        "EVPOLY_REMOTE_EVCURVE_ALPHA_TOKEN",
        "EVPOLY_REMOTE_SESSIONBAND_ALPHA_TOKEN",
        "EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN",
        "EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN",
        "EVPOLY_REMOTE_MARKET_DISCOVERY_TOKEN",
        "EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TOKEN",
    ]
    .iter()
    .find_map(|key| nonempty_map_value(&env_map, key));

    if env_map
        .get("POLY_POLYGON_RPC_HTTP_URL")
        .map(|value| value.trim().is_empty() || value.trim() == "https://1rpc.io/matic")
        .unwrap_or(true)
    {
        env_map.insert(
            "POLY_POLYGON_RPC_HTTP_URL".into(),
            DEFAULT_POLYGON_RPC_URL.to_string(),
        );
    }

    if env_map
        .get("POLY_POLYGON_RPC_HTTP_FALLBACK_URL")
        .map(|value| value.trim().is_empty() || value.trim() == "https://polygon-rpc.com")
        .unwrap_or(true)
    {
        env_map.insert(
            "POLY_POLYGON_RPC_HTTP_FALLBACK_URL".into(),
            DEFAULT_POLYGON_RPC_FALLBACK_URL.to_string(),
        );
    }

    if env_map
        .get("EVPOLY_REMOTE_EVCURVE_ALPHA_TOKEN")
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
    {
        if let Some(shared_token) = shared_alpha_token.clone() {
            env_map.insert("EVPOLY_REMOTE_EVCURVE_ALPHA_TOKEN".into(), shared_token);
        }
    }

    if env_map
        .get("EVPOLY_REMOTE_SESSIONBAND_ALPHA_TOKEN")
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
    {
        if let Some(shared_token) = shared_alpha_token {
            env_map.insert("EVPOLY_REMOTE_SESSIONBAND_ALPHA_TOKEN".into(), shared_token);
        }
    }

    // Keep MM rewards selection mode explicit so all core codepaths and telemetry
    // agree on AUTO behavior instead of falling back to older implicit defaults.
    env_map.insert(
        "EVPOLY_MM_MARKET_MODE".into(),
        normalized_mm_market_mode(&env_map),
    );
    normalize_redemption_defaults(&mut env_map);

    if env_map
        .get("EVPOLY_ADMIN_API_TOKEN")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    {
        env_map.insert("EVPOLY_ADMIN_API_ENABLE".into(), "true".to_string());
        env_map
            .entry("EVPOLY_ADMIN_API_BIND".into())
            .or_insert_with(|| "127.0.0.1:8787".to_string());
    }

    let mut output = String::new();
    let mut written_keys: HashSet<String> = HashSet::new();

    for line in CORE_ENV_TEMPLATE.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            output.push_str(line);
            output.push('\n');
            continue;
        }
        if let Some((key, _)) = trimmed.split_once('=') {
            if let Some(val) = env_map.get(key) {
                output.push_str(&format!("{key}={val}\n"));
            } else {
                output.push_str(line);
                output.push('\n');
            }
            written_keys.insert(key.to_string());
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }

    for (k, v) in &env_map {
        if !written_keys.contains(k) {
            output.push_str(&format!("{k}={v}\n"));
        }
    }

    let env_path = unique_env_path(data_dir);
    std::fs::write(&env_path, &output)?;
    Ok(env_path)
}

fn build_config_json(profile: &Profile) -> serde_json::Value {
    let enable_eth = bool_from_config(&profile.strategy_config, "POLY_ENABLE_ETH_TRADING", true);
    let enable_solana =
        bool_from_config(&profile.strategy_config, "POLY_ENABLE_SOLANA_TRADING", true);
    let enable_xrp = bool_from_config(&profile.strategy_config, "POLY_ENABLE_XRP_TRADING", true);
    let funder_wallet = profile.primary_wallet_address();
    let legacy_proxy_wallet = if profile.signature_type == 3 {
        funder_wallet.clone()
    } else {
        profile.proxy_wallet_address.clone()
    };

    serde_json::json!({
        "polymarket": {
            "gamma_api_url": "https://gamma-api.polymarket.com",
            "clob_api_url": "https://clob.polymarket.com",
            "api_key": "",
            "api_secret": "",
            "api_passphrase": "",
            "private_key": "",
            "proxy_wallet_address": legacy_proxy_wallet,
            "deposit_wallet_address": profile.deposit_wallet_address.clone(),
            "funder_wallet_address": funder_wallet,
            "signature_type": profile.signature_type
        },
        "trading": {
            "eth_condition_id": null,
            "btc_condition_id": null,
            "solana_condition_id": null,
            "xrp_condition_id": null,
            "check_interval_ms": 1000,
            "fixed_trade_amount": 1.0,
            "trigger_price": 0.9,
            "min_elapsed_minutes": 10,
            "sell_price": 0.99,
            "hold_to_resolution": true,
            "hold_to_resolution_ladder": null,
            "hold_to_resolution_reactive": null,
            "max_buy_price": 0.95,
            "stop_loss_price": 0.85,
            "hedge_price": 0.5,
            "market_closure_check_interval_seconds": 60,
            "min_time_remaining_seconds": 30,
            "enable_eth_trading": enable_eth,
            "enable_solana_trading": enable_solana,
            "enable_xrp_trading": enable_xrp,
            "dual_limit_price": null,
            "dual_limit_shares": null,
            "order_ttl_seconds": 1200
        }
    })
}

pub fn write_config_json(profile: &Profile, path: &Path) -> Result<PathBuf> {
    let config = build_config_json(profile);
    let json = serde_json::to_string_pretty(&config)?;
    std::fs::write(path, json)?;
    Ok(path.to_path_buf())
}

pub fn generate_config_json(profile: &Profile, data_dir: &Path) -> Result<PathBuf> {
    let config_path = data_dir.join("config.json");
    write_config_json(profile, &config_path)
}

pub fn cleanup_env_file(path: &Path) {
    if path.exists() {
        if let Ok(meta) = std::fs::metadata(path) {
            let zeros = vec![0u8; meta.len() as usize];
            let _ = std::fs::write(path, &zeros);
        }
        let _ = std::fs::remove_file(path);
    }
}

pub fn cleanup_generated_env_files(data_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !file_name.starts_with(".env.generated") {
            continue;
        }
        cleanup_env_file(&path);
    }
}

fn unique_env_path(data_dir: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    data_dir.join(format!(".env.generated.{stamp}"))
}

#[cfg(test)]
mod tests {
    use super::{build_config_json, generate_env_file};
    use crate::profile_manager::Profile;
    use std::collections::HashMap;

    fn sample_profile() -> Profile {
        Profile {
            id: "p1".to_string(),
            name: "desktop".to_string(),
            eoa_wallet_address: "0x1111111111111111111111111111111111111111".to_string(),
            proxy_wallet_address: "0x2222222222222222222222222222222222222222".to_string(),
            deposit_wallet_address: String::new(),
            wallet_address: "0x2222222222222222222222222222222222222222".to_string(),
            signature_type: 2,
            encrypted_secrets: String::new(),
            strategy_config: serde_json::json!({
                "POLY_ENABLE_ETH_TRADING": false,
                "POLY_ENABLE_SOLANA_TRADING": true,
                "POLY_ENABLE_XRP_TRADING": false
            }),
            sizing_config: serde_json::json!({
                "APP_SIMULATION": true,
                "EVPOLY_PREMARKET_BASE_SIZE_USD": 10.0
            }),
            created_at: "now".to_string(),
            last_used: "now".to_string(),
        }
    }

    #[test]
    fn build_config_json_includes_required_runtime_fields() {
        let config = build_config_json(&sample_profile());

        assert_eq!(config["trading"]["check_interval_ms"], 1000);
        assert_eq!(config["trading"]["fixed_trade_amount"], 1.0);
        assert_eq!(config["trading"]["enable_eth_trading"], false);
        assert_eq!(config["trading"]["enable_solana_trading"], true);
        assert_eq!(config["trading"]["enable_xrp_trading"], false);
        assert_eq!(
            config["polymarket"]["proxy_wallet_address"],
            "0x2222222222222222222222222222222222222222"
        );
        assert_eq!(config["polymarket"]["deposit_wallet_address"], "");
        assert_eq!(
            config["polymarket"]["funder_wallet_address"],
            "0x2222222222222222222222222222222222222222"
        );
        assert!(config["trading"]["dual_limit_price"].is_null());
        assert!(config["trading"]["strategy_config"].is_null());
    }

    #[test]
    fn build_config_json_preserves_desktop_metadata() {
        let config = build_config_json(&sample_profile());

        assert_eq!(config["polymarket"]["api_key"], "");
        assert_eq!(config["polymarket"]["signature_type"], 2);
        assert_eq!(config["trading"]["order_ttl_seconds"], 1200);
    }

    #[test]
    fn core_env_template_uses_current_remote_alpha_routes() {
        let defaults = super::core_env_defaults();
        let expected = [
            (
                "EVPOLY_REMOTE_MARKET_DISCOVERY_URL",
                "https://alpha.evplus.ai/v1/discovery/timeframe",
            ),
            (
                "EVPOLY_REMOTE_PREMARKET_ALPHA_URL",
                "https://alpha.evplus.ai/v1/alpha/premarket/ladder",
            ),
            (
                "EVPOLY_REMOTE_ENDGAME_ALPHA_URL",
                "https://alpha.evplus.ai/v1/alpha/endgame/policy",
            ),
            (
                "EVPOLY_REMOTE_EVCURVE_ALPHA_URL",
                "https://alpha.evplus.ai/v1/alpha/evcurve",
            ),
            (
                "EVPOLY_REMOTE_SESSIONBAND_ALPHA_URL",
                "https://alpha.evplus.ai/v1/alpha/sessionband",
            ),
            (
                "EVPOLY_REMOTE_EVSNIPE_DISCOVERY_URL",
                "https://alpha.evplus.ai/v1/discovery/evsnipe",
            ),
            (
                "EVPOLY_REMOTE_MM_SPORT_DEPTH_SKIP_ALPHA_URL",
                "https://alpha.evplus.ai/v1/alpha/mm-sport/depth-skip",
            ),
        ];

        for (key, url) in expected {
            assert_eq!(defaults.get(key).map(String::as_str), Some(url), "{key}");
        }
    }

    #[test]
    fn generate_env_file_reuses_shared_alpha_token_for_missing_strategy_tokens() {
        let profile = sample_profile();
        let mut secrets = HashMap::new();
        secrets.insert(
            "EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN".to_string(),
            "shared-alpha-token".to_string(),
        );

        let temp_dir =
            std::env::temp_dir().join(format!("evpoly-config-io-test-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");

        let env_path = generate_env_file(&profile, &secrets, &temp_dir).expect("generate env");
        let content = std::fs::read_to_string(&env_path).expect("read env");

        assert!(content.contains("POLY_POLYGON_RPC_HTTP_URL=https://1rpc.io/matic"));
        assert!(content.contains("POLY_POLYGON_RPC_HTTP_FALLBACK_URL=https://polygon-rpc.com"));
        assert!(content
            .contains("POLY_PROXY_WALLET_ADDRESS=0x2222222222222222222222222222222222222222"));
        assert!(content.contains("POLY_DEPOSIT_WALLET_ADDRESS="));
        assert!(content
            .contains("POLY_FUNDER_WALLET_ADDRESS=0x2222222222222222222222222222222222222222"));
        assert!(content.contains("EVPOLY_REMOTE_EVCURVE_ALPHA_TOKEN=shared-alpha-token"));
        assert!(content.contains("EVPOLY_REMOTE_SESSIONBAND_ALPHA_TOKEN=shared-alpha-token"));
        assert!(content.contains("EVPOLY_MM_MARKET_MODE=auto"));

        let _ = std::fs::remove_file(env_path);
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn generate_env_file_maps_deposit_wallet_to_funder_keys() {
        let mut profile = sample_profile();
        profile.signature_type = 3;
        profile.proxy_wallet_address = String::new();
        profile.deposit_wallet_address = "0x3333333333333333333333333333333333333333".to_string();
        profile.normalize_wallet_fields();

        let temp_dir = std::env::temp_dir().join(format!(
            "evpoly-config-io-deposit-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");

        let env_path =
            generate_env_file(&profile, &HashMap::new(), &temp_dir).expect("generate env");
        let content = std::fs::read_to_string(&env_path).expect("read env");

        assert!(content.contains("POLY_SIGNATURE_TYPE=3"));
        assert!(content
            .contains("POLY_PROXY_WALLET_ADDRESS=0x3333333333333333333333333333333333333333"));
        assert!(content
            .contains("POLY_DEPOSIT_WALLET_ADDRESS=0x3333333333333333333333333333333333333333"));
        assert!(content
            .contains("POLY_FUNDER_WALLET_ADDRESS=0x3333333333333333333333333333333333333333"));

        let _ = std::fs::remove_file(env_path);
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn generate_env_file_normalizes_legacy_premarket_alpha_url() {
        let mut profile = sample_profile();
        profile.strategy_config = serde_json::json!({
            "EVPOLY_REMOTE_PREMARKET_ALPHA_URL": "https://alpha.evplus.ai/v1/alpha/premarket/should-trade"
        });

        let temp_dir = std::env::temp_dir().join(format!(
            "evpoly-config-io-alpha-url-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");

        let env_path =
            generate_env_file(&profile, &HashMap::new(), &temp_dir).expect("generate env");
        let content = std::fs::read_to_string(&env_path).expect("read env");

        assert!(content.contains(
            "EVPOLY_REMOTE_PREMARKET_ALPHA_URL=https://alpha.evplus.ai/v1/alpha/premarket/ladder"
        ));
        assert!(!content.contains("/v1/alpha/premarket/should-trade"));

        let _ = std::fs::remove_file(env_path);
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn generate_env_file_normalizes_legacy_mm_market_mode_to_auto() {
        let mut profile = sample_profile();
        profile.strategy_config = serde_json::json!({
            "EVPOLY_MM_MARKET_MODE": "target"
        });

        let temp_dir = std::env::temp_dir().join(format!(
            "evpoly-config-io-mm-mode-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");

        let env_path =
            generate_env_file(&profile, &HashMap::new(), &temp_dir).expect("generate env");
        let content = std::fs::read_to_string(&env_path).expect("read env");

        assert!(content.contains("EVPOLY_MM_MARKET_MODE=auto"));

        let _ = std::fs::remove_file(env_path);
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn generate_env_file_preserves_hybrid_mm_market_mode() {
        let mut profile = sample_profile();
        profile.strategy_config = serde_json::json!({
            "EVPOLY_MM_MARKET_MODE": "hybrid"
        });

        let temp_dir = std::env::temp_dir().join(format!(
            "evpoly-config-io-mm-mode-hybrid-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");

        let env_path =
            generate_env_file(&profile, &HashMap::new(), &temp_dir).expect("generate env");
        let content = std::fs::read_to_string(&env_path).expect("read env");

        assert!(content.contains("EVPOLY_MM_MARKET_MODE=hybrid"));

        let _ = std::fs::remove_file(env_path);
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn generate_env_file_normalizes_legacy_redemption_throughput_defaults() {
        let mut profile = sample_profile();
        profile.strategy_config = serde_json::json!({
            "EVPOLY_REDEMPTION_MAX_CONDITIONS_PER_SWEEP": 3.0,
            "EVPOLY_REDEMPTION_AUTO_TRIGGER_PENDING_THRESHOLD": 10.0
        });

        let temp_dir = std::env::temp_dir().join(format!(
            "evpoly-config-io-redemption-defaults-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");

        let env_path =
            generate_env_file(&profile, &HashMap::new(), &temp_dir).expect("generate env");
        let content = std::fs::read_to_string(&env_path).expect("read env");

        assert!(content.contains("EVPOLY_REDEMPTION_MAX_CONDITIONS_PER_SWEEP=10"));
        assert!(content.contains("EVPOLY_REDEMPTION_AUTO_TRIGGER_PENDING_THRESHOLD=3"));

        let _ = std::fs::remove_file(env_path);
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn generate_env_file_writes_integer_like_numbers_without_decimal_suffix() {
        let mut profile = sample_profile();
        profile.strategy_config = serde_json::json!({
            "EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC": 600.0,
            "EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC": 90.0,
            "EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC": 180.0,
            "EVPOLY_MM_AUTO_REFRESH_SEC": 300.0
        });

        let temp_dir = std::env::temp_dir().join(format!(
            "evpoly-config-io-int-values-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");

        let env_path =
            generate_env_file(&profile, &HashMap::new(), &temp_dir).expect("generate env");
        let content = std::fs::read_to_string(&env_path).expect("read env");

        assert!(content.contains("EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC=600"));
        assert!(content.contains("EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC=90"));
        assert!(content.contains("EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC=180"));
        assert!(content.contains("EVPOLY_MM_AUTO_REFRESH_SEC=300"));
        assert!(!content.contains("EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC=600.0"));

        let _ = std::fs::remove_file(env_path);
        let _ = std::fs::remove_dir_all(temp_dir);
    }
}
