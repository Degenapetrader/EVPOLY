#![allow(dead_code)]

use crate::profile_manager::Profile;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const DEFAULT_ENV_TEMPLATE: &str = r#"# EVPOLY runtime env template

# Public API endpoints (safe defaults)
POLY_GAMMA_API_URL=https://gamma-api.polymarket.com
POLY_CLOB_API_URL=https://clob.polymarket.com
POLY_POLYGON_RPC_HTTP_URL=https://1rpc.io/matic
POLY_POLYGON_RPC_HTTP_FALLBACK_URL=https://polygon-rpc.com

POLY_PRIVATE_KEY=

# Wallet mode
# 1 = Proxy wallet (email signup): no gas fee paid by user (relayer/gasless)
# 2 = Safe wallet (web3 wallet signup like MetaMask/Rabby): no gas fee paid by user (relayer/gasless)
# 0 = EOA wallet: user pays gas fee directly
POLY_SIGNATURE_TYPE=1
POLY_PROXY_WALLET_ADDRESS=

# Remote builder signer (required on poly-remote)
EVPOLY_ORDER_SIGNER_PRIMARY_URL=https://signer.evplus.ai/sign/order
EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN=
EVPOLY_ORDER_SIGNER_FALLBACK_URL=
EVPOLY_SUBMIT_SIGNER_URL=
EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN=
RELAYER_API_KEY=
RELAYER_API_KEY_ADDRESS=
EVPOLY_ADMIN_API_TOKEN=

# Optional VPS-hosted market discovery
EVPOLY_REMOTE_MARKET_DISCOVERY_URL=https://alpha.evplus.ai/v1/discovery/timeframe
EVPOLY_REMOTE_MARKET_DISCOVERY_TOKEN=

# VPS-hosted Premarket alpha gate
EVPOLY_REMOTE_PREMARKET_ALPHA_URL=https://alpha.evplus.ai/v1/alpha/premarket/should-trade
EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN=

# VPS-hosted Endgame alpha policy
EVPOLY_REMOTE_ENDGAME_ALPHA_URL=https://alpha.evplus.ai/v1/alpha/endgame/policy
EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN=
EVPOLY_ENDGAME_ALPHA_REQUIRED=true

# VPS-hosted MM rewards alpha
EVPOLY_REMOTE_MM_REWARDS_SELECTION_ALPHA_URL=https://alpha.evplus.ai/v1/alpha/mm-rewards/selection
EVPOLY_REMOTE_MM_REWARDS_ALPHA_TOKEN=

# Optional VPS-hosted EVSnipe market-spec discovery
EVPOLY_REMOTE_EVSNIPE_DISCOVERY_URL=https://alpha.evplus.ai/v1/discovery/evsnipe
EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TOKEN=

# Strategy toggles
EVPOLY_STRATEGY_PREMARKET_ENABLE=true
EVPOLY_STRATEGY_ENDGAME_ENABLE=true
EVPOLY_STRATEGY_EVCURVE_ENABLE=true
EVPOLY_STRATEGY_SESSIONBAND_ENABLE=true
EVPOLY_STRATEGY_EVSNIPE_ENABLE=true
EVPOLY_STRATEGY_MM_REWARDS_ENABLE=false
EVPOLY_STRATEGY_MM_SPORT_ENABLE=false
POLY_ENABLE_ETH_TRADING=true
POLY_ENABLE_SOLANA_TRADING=true
POLY_ENABLE_XRP_TRADING=true
EVPOLY_ENDGAME_SYMBOLS=BTC,ETH,SOL,XRP,DOGE,BNB,HYPE
EVPOLY_EVCURVE_SYMBOLS=BTC,ETH,SOL,XRP,DOGE,BNB,HYPE
EVPOLY_EVSNIPE_SYMBOLS=BTC,ETH,SOL,XRP,DOGE,BNB,HYPE

# Optional MM Sport strategy
EVPOLY_MM_SPORT_EVENT_DRIVEN_ENABLE=true
EVPOLY_MM_SPORT_EVENT_FALLBACK_POLL_MS=1000
EVPOLY_MM_SPORT_WS_STALE_MS=2500
EVPOLY_MM_SPORT_MIN_REWARD_RATE_PER_DAY=300
EVPOLY_MM_SPORT_QUOTE_SIZE_MULT=1.2
EVPOLY_MM_SPORT_MAX_SHARE_RATIO=0.02
EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD=100000
EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC=900

# Optional MM rewards tuning
EVPOLY_MM_REWARD_MIN_TARGET_MULT=1

# Unified strategy base sizing
EVPOLY_PREMARKET_BASE_SIZE_USD=
EVPOLY_PREMARKET_TP_ENABLE=true
EVPOLY_ENDGAME_BASE_SIZE_USD=
EVPOLY_EVCURVE_BASE_SIZE_USD=
EVPOLY_SESSIONBAND_BASE_SIZE_USD=
EVPOLY_EVSNIPE_SIZE_USD=

# Retention
EVPOLY_EVENTS_ROTATE_KEEP=3
EVPOLY_HISTORY_ROTATE_KEEP=3
EVPOLY_DB_BACKUP_RETENTION_DAYS=3
EVPOLY_HISTORY_DIR_RETENTION_DAYS=3
EVPOLY_RETENTION_CRON_EXPR=17 * * * *

# Startup controls
EVPOLY_STARTUP_PENDING_RECONCILE_ENABLE=true
"#;

fn value_to_env_string(v: &serde_json::Value) -> String {
    v.as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| v.to_string())
}

pub fn generate_env_file(
    profile: &Profile,
    secrets: &HashMap<String, String>,
    data_dir: &Path,
) -> Result<PathBuf> {
    let mut env_map: HashMap<String, String> = HashMap::new();

    for line in DEFAULT_ENV_TEMPLATE.lines() {
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
        profile.primary_wallet_address(),
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

    let mut output = String::new();
    let mut written_keys: HashSet<String> = HashSet::new();

    for line in DEFAULT_ENV_TEMPLATE.lines() {
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

pub fn generate_config_json(profile: &Profile, data_dir: &Path) -> Result<PathBuf> {
    let config = serde_json::json!({
        "polymarket": {
            "gamma_api_url": "https://gamma-api.polymarket.com",
            "clob_api_url": "https://clob.polymarket.com",
            "rpc_url": "https://1rpc.io/matic",
            "rpc_fallback_url": "https://polygon-rpc.com"
        },
        "trading": {
            "wallet_address": profile.primary_wallet_address(),
            "eoa_wallet_address": profile.eoa_wallet_address,
            "proxy_wallet_address": profile.proxy_wallet_address,
            "signature_type": profile.signature_type,
            "strategy_config": profile.strategy_config,
            "sizing_config": profile.sizing_config
        }
    });

    let config_path = data_dir.join("config.json");
    let json = serde_json::to_string_pretty(&config)?;
    std::fs::write(&config_path, json)?;
    Ok(config_path)
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
