#![allow(dead_code)]

use crate::profile_manager::Profile;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(crate) const DEFAULT_POLYGON_RPC_URL: &str = "https://polygon-bor-rpc.publicnode.com";
pub(crate) const DEFAULT_POLYGON_RPC_FALLBACK_URL: &str = "https://polygon.drpc.org";

const CORE_ENV_TEMPLATE: &str = include_str!("../core-contract/.env.example");

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
    v.as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| v.to_string())
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

pub fn generate_env_file(
    profile: &Profile,
    secrets: &HashMap<String, String>,
    data_dir: &Path,
) -> Result<PathBuf> {
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

    let shared_alpha_token = [
        "EVPOLY_REMOTE_EVCURVE_ALPHA_TOKEN",
        "EVPOLY_REMOTE_SESSIONBAND_ALPHA_TOKEN",
        "EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN",
        "EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN",
        "EVPOLY_REMOTE_MM_REWARDS_ALPHA_TOKEN",
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

    let env_path = data_dir.join(".env.generated");
    std::fs::write(&env_path, &output)?;
    Ok(env_path)
}

fn build_config_json(profile: &Profile) -> serde_json::Value {
    let enable_eth = bool_from_config(&profile.strategy_config, "POLY_ENABLE_ETH_TRADING", true);
    let enable_solana =
        bool_from_config(&profile.strategy_config, "POLY_ENABLE_SOLANA_TRADING", true);
    let enable_xrp = bool_from_config(&profile.strategy_config, "POLY_ENABLE_XRP_TRADING", true);

    serde_json::json!({
        "polymarket": {
            "gamma_api_url": "https://gamma-api.polymarket.com",
            "clob_api_url": "https://clob.polymarket.com",
            "api_key": "",
            "api_secret": "",
            "api_passphrase": "",
            "private_key": "",
            "proxy_wallet_address": profile.proxy_wallet_address.clone(),
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
    fn generate_env_file_reuses_shared_alpha_token_for_missing_strategy_tokens() {
        let profile = sample_profile();
        let mut secrets = HashMap::new();
        secrets.insert(
            "EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN".to_string(),
            "shared-alpha-token".to_string(),
        );

        let temp_dir = std::env::temp_dir().join(format!(
            "evpoly-config-io-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");

        let env_path = generate_env_file(&profile, &secrets, &temp_dir).expect("generate env");
        let content = std::fs::read_to_string(&env_path).expect("read env");

        assert!(content.contains("POLY_POLYGON_RPC_HTTP_URL=https://polygon-bor-rpc.publicnode.com"));
        assert!(content.contains("POLY_POLYGON_RPC_HTTP_FALLBACK_URL=https://polygon.drpc.org"));
        assert!(content.contains("POLY_PROXY_WALLET_ADDRESS=0x2222222222222222222222222222222222222222"));
        assert!(content.contains("EVPOLY_REMOTE_EVCURVE_ALPHA_TOKEN=shared-alpha-token"));
        assert!(content.contains("EVPOLY_REMOTE_SESSIONBAND_ALPHA_TOKEN=shared-alpha-token"));

        let _ = std::fs::remove_file(env_path);
        let _ = std::fs::remove_dir_all(temp_dir);
    }
}
