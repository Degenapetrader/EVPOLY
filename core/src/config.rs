use clap::Parser;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Run in simulation mode (no real trades)
    /// Default: simulation mode is disabled (false)
    #[arg(short, long, default_value_t = false)]
    pub simulation: bool,

    /// Run in production mode (execute real trades)
    /// This sets simulation to false
    #[arg(long)]
    pub no_simulation: bool,

    /// Print an equity snapshot (USDC collateral + MTM of portfolio tokens) and exit.
    #[arg(long)]
    pub equity: bool,

    /// Configuration file path
    #[arg(short, long, default_value = "config.json")]
    pub config: PathBuf,
}

impl Args {
    /// Get the effective simulation mode
    /// If --no-simulation is used, it overrides the default
    pub fn is_simulation(&self) -> bool {
        if self.no_simulation {
            false
        } else {
            self.simulation
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub polymarket: PolymarketConfig,
    pub trading: TradingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolymarketConfig {
    pub gamma_api_url: String,
    pub clob_api_url: String,
    /// Private key for signing orders (optional, but may be required for order placement)
    /// Format: hex string (with or without 0x prefix) or raw private key
    pub private_key: Option<String>,
    /// Proxy wallet address (Polymarket proxy wallet address where your balance is)
    /// If set, the bot will trade using this proxy wallet instead of the EOA (private key account)
    /// Format: Ethereum address (with or without 0x prefix)
    pub proxy_wallet_address: Option<String>,
    /// Signature type for authentication (optional, defaults to EOA if not set)
    /// 0 = EOA (Externally Owned Account - private key account)
    /// 1 = Proxy (Polymarket proxy wallet)
    /// 2 = GnosisSafe (Gnosis Safe wallet)
    /// If proxy_wallet_address is set, this should be 1 (Proxy)
    pub signature_type: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingConfig {
    pub eth_condition_id: Option<String>,
    pub btc_condition_id: Option<String>,
    pub solana_condition_id: Option<String>,
    pub xrp_condition_id: Option<String>,
    pub check_interval_ms: u64,
    /// Fixed trade amount in USD for BTC Up token purchase
    /// Default: 1.0 ($1.00)
    pub fixed_trade_amount: f64,
    /// Price threshold to trigger buy (BTC Up token must reach this price)
    /// Default: 0.9 ($0.90)
    pub trigger_price: f64,
    /// Minimum minutes that must have elapsed before buying (after this time, check trigger_price)
    /// Default: 10 (10 minutes)
    pub min_elapsed_minutes: u64,
    /// Price at which to sell the token (sell when price reaches this)
    /// Default: 0.99 ($0.99)
    pub sell_price: f64,
    /// If true, never place active exits (profit/stop-loss); hold positions to market resolution/redeem.
    /// This is the single source of truth for hold-to-resolution behavior.
    #[serde(default = "default_hold_to_resolution")]
    pub hold_to_resolution: bool,
    /// Optional per-mode override for ladder entries.
    /// If unset, falls back to hold_to_resolution.
    #[serde(default)]
    pub hold_to_resolution_ladder: Option<bool>,
    /// Optional per-mode override for reactive entries.
    /// If unset, falls back to hold_to_resolution.
    #[serde(default)]
    pub hold_to_resolution_reactive: Option<bool>,
    /// Maximum price to buy at (don't buy if price exceeds this)
    /// Default: 0.95 ($0.95)
    /// Buy when: trigger_price <= bid_price <= max_buy_price
    pub max_buy_price: Option<f64>,
    /// Stop-loss price - sell if price drops below this (stop-loss protection)
    /// Default: 0.85 ($0.85) - sell if price drops 5% below purchase price
    /// If None, stop-loss is disabled
    pub stop_loss_price: Option<f64>,
    /// Hedge price - limit buy price for opposite token when buying at $0.9+ (hedging strategy)
    /// Default: 0.5 ($0.50) - place limit buy order for opposite token at this price
    /// If None, hedging is disabled
    /// Strategy: When buying a token at $0.9+, also place a limit buy for the opposite token at $0.5
    /// This creates a hedge - if market reverses, you'll have both tokens at favorable prices
    pub hedge_price: Option<f64>,
    /// Interval for checking market closure and redemption retries after period ends
    /// Default: 10 (10 seconds) - faster retries for redemption
    pub market_closure_check_interval_seconds: u64,
    /// Minimum time remaining in seconds before allowing a buy
    /// Default: 30 (30 seconds) - don't buy if less than 30 seconds remain
    /// This prevents buying too close to market close when prices can be volatile
    pub min_time_remaining_seconds: Option<u64>,
    /// Enable ETH market trading
    /// Default: true (ETH trading enabled)
    /// If false, only BTC markets will be traded
    pub enable_eth_trading: bool,
    /// Enable Solana market trading
    /// Default: true (Solana trading enabled)
    /// If true, Solana Up/Down markets will also be traded
    pub enable_solana_trading: bool,
    /// Enable XRP market trading
    /// Default: true (XRP trading enabled)
    /// If true, XRP Up/Down markets will also be traded
    pub enable_xrp_trading: bool,
    /// Dual limit-start bot: fixed limit price for Up/Down orders
    pub dual_limit_price: Option<f64>,
    /// Dual limit-start bot: fixed number of shares per order
    pub dual_limit_shares: Option<f64>,
    /// Default TTL for LIMIT orders in seconds when no explicit expiration is provided
    /// Default: 1200 (20 minutes)
    pub order_ttl_seconds: Option<u64>,
}

const MARKET_CLOSURE_CHECK_INTERVAL_SECONDS_HARDCODED: u64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndgameExecutionConfig {
    pub enable: bool,
    pub poll_interval_ms: u64,
    pub safety_poll_ms: u64,
    pub tick_random_jitter_ms: i64,
    pub enabled_symbols_csv: String,
    pub enabled_timeframes_csv: String,
    pub tick_offsets_sec: Vec<u64>,
    pub allow_cross_spread: bool,
    pub per_tick_notional_usd: f64,
    pub per_tick_notional_by_offset: Vec<(u64, f64)>,
    pub per_tick_notional_by_symbol: HashMap<String, Vec<(u64, f64)>>,
    pub per_period_cap_usd: f64,
    pub max_signal_age_ms: i64,
    pub safety_stop_s: u64,
    pub book_freshness_ms: i64,
    pub latency_haircut: f64,
    pub edge_floor_bps: f64,
    pub min_entry_price: f64,
    pub probability_min: f64,
    pub z_cap: f64,
    pub min_sigma_bps_15s: f64,
    pub spread_to_sigma_mult: f64,
    pub sigma_mult: f64,
    pub depth_thick_threshold: f64,
    pub depth_sigma_scale: f64,
    pub depth_sigma_max_mult: f64,
    pub thin_flip_guard_enable: bool,
    pub thin_flip_guard_bps: f64,
    pub thin_flip_guard_max_depth_usd: f64,
    pub guard_v3_enable: bool,
    pub guard_v3_fail_open: bool,
    pub guard_v3_near_scale: f64,
    pub guard_v3_impact_scale: f64,
    pub guard_v3_hard_cutoff: f64,
    pub guard_v3_soft_scale: f64,
    pub guard_v3_last_tick_guard: bool,
    pub guard_v3_min_soft_multiplier: f64,
    pub divergence_threshold: f64,
    pub divergence_min_size_usd: f64,
    pub divergence_min_size_ratio: f64,
}

impl EndgameExecutionConfig {
    pub fn from_env() -> Self {
        let tick_offsets_ms_raw = env_nonempty(&["EVPOLY_ENDGAME_TICK_OFFSETS_MS".to_string()]);
        let (offset_scale_ms, mut tick_offsets_sec) = if let Some(raw_ms) = tick_offsets_ms_raw {
            (1_u64, parse_endgame_offsets(raw_ms.as_str()))
        } else {
            (
                1_000_u64,
                parse_endgame_offsets(
                    env_nonempty(&["EVPOLY_ENDGAME_TICK_OFFSETS_SEC".to_string()])
                        .unwrap_or_else(|| "65,50,35,20,5".to_string())
                        .as_str(),
                )
                .into_iter()
                .map(|offset_sec| offset_sec.saturating_mul(1_000))
                .collect::<Vec<_>>(),
            )
        };
        if tick_offsets_sec.is_empty() {
            tick_offsets_sec = vec![65_000, 50_000, 35_000, 20_000, 5_000];
        }
        tick_offsets_sec.sort_by(|a, b| b.cmp(a));
        tick_offsets_sec.dedup();

        let enabled_timeframes_csv = env_nonempty(&["EVPOLY_ENDGAME_TIMEFRAMES".to_string()])
            .unwrap_or_else(|| "5m,15m,1h,4h".to_string());
        let enabled_symbols_csv = env_nonempty(&["EVPOLY_ENDGAME_SYMBOLS".to_string()])
            .unwrap_or_else(|| "BTC,ETH,SOL,XRP,DOGE,BNB,HYPE".to_string());
        let per_tick_notional_by_offset =
            env_nonempty(&["EVPOLY_ENDGAME_PER_TICK_USD_BY_OFFSET".to_string()])
                .map(|raw| parse_endgame_tick_notionals_by_offset(raw.as_str(), offset_scale_ms))
                .unwrap_or_default();
        let per_tick_notional_by_symbol =
            env_nonempty(&["EVPOLY_ENDGAME_PER_TICK_USD_BY_SYMBOL".to_string()])
                .map(|raw| {
                    parse_endgame_tick_notionals_by_symbol(
                        raw.as_str(),
                        tick_offsets_sec.as_slice(),
                        offset_scale_ms,
                    )
                })
                .unwrap_or_default();

        Self {
            enable: env_bool_named("EVPOLY_STRATEGY_ENDGAME_ENABLE").unwrap_or(true),
            poll_interval_ms: env_u64_any(&["EVPOLY_ENDGAME_POLL_MS".to_string()])
                .unwrap_or(500)
                .max(20),
            safety_poll_ms: env_u64_any(&["EVPOLY_ENDGAME_SAFETY_POLL_MS".to_string()])
                .unwrap_or(500)
                .max(100),
            tick_random_jitter_ms: env_i64_any(&[
                "EVPOLY_ENDGAME_TICK_RANDOM_JITTER_MS".to_string()
            ])
            .unwrap_or(50)
            .clamp(0, 250),
            enabled_symbols_csv,
            enabled_timeframes_csv,
            tick_offsets_sec,
            allow_cross_spread: env_bool_named("EVPOLY_ENDGAME_ALLOW_CROSS_SPREAD").unwrap_or(true),
            per_tick_notional_usd: env_f64_any(&["EVPOLY_ENDGAME_PER_TICK_USD".to_string()])
                .unwrap_or(50.0)
                .max(1.0),
            per_tick_notional_by_offset,
            per_tick_notional_by_symbol,
            // Single cap variable (new): EVPOLY_ENDGAME_PER_PERIOD_CAP_USD.
            // Backward compatibility (legacy): TEST/PROD names are still accepted as fallbacks.
            per_period_cap_usd: env_f64_any(&[
                "EVPOLY_ENDGAME_PER_PERIOD_CAP_USD".to_string(),
                "EVPOLY_ENDGAME_PER_PERIOD_CAP_USD_TEST".to_string(),
                "EVPOLY_ENDGAME_PER_PERIOD_CAP_USD_PROD".to_string(),
            ])
            .unwrap_or(10_000.0)
            .max(1.0),
            max_signal_age_ms: env_i64_any(&["EVPOLY_ENDGAME_MAX_SIGNAL_AGE_MS".to_string()])
                .unwrap_or(60_000)
                .max(5_000),
            safety_stop_s: env_u64_any(&["EVPOLY_ENDGAME_SAFETY_STOP_SEC".to_string()])
                .unwrap_or(2)
                .min(20),
            book_freshness_ms: env_i64_any(&["EVPOLY_ENDGAME_BOOK_FRESHNESS_MS".to_string()])
                .unwrap_or(1_500)
                .max(250),
            latency_haircut: env_f64_any(&["EVPOLY_ENDGAME_LATENCY_HAIRCUT".to_string()])
                .unwrap_or(0.85)
                .clamp(0.10, 1.00),
            edge_floor_bps: env_f64_any(&["EVPOLY_ENDGAME_EDGE_FLOOR_BPS".to_string()])
                .unwrap_or(12.0)
                .max(0.0),
            min_entry_price: env_f64_any(&["EVPOLY_ENDGAME_MIN_ENTRY_PRICE".to_string()])
                .unwrap_or(0.5)
                .clamp(0.0, 0.99),
            probability_min: env_f64_any(&["EVPOLY_ENDGAME_PROB_MIN".to_string()])
                .unwrap_or(0.005)
                .clamp(0.000_001, 0.20),
            z_cap: env_f64_any(&["EVPOLY_ENDGAME_Z_CAP".to_string()])
                .unwrap_or(4.0)
                .clamp(0.5, 8.0),
            min_sigma_bps_15s: env_f64_any(&["EVPOLY_ENDGAME_MIN_SIGMA_BPS_15S".to_string()])
                .unwrap_or(1.0)
                .clamp(0.01, 100.0),
            spread_to_sigma_mult: env_f64_any(&["EVPOLY_ENDGAME_SPREAD_TO_SIGMA_MULT".to_string()])
                .unwrap_or(10.0)
                .clamp(0.1, 100.0),
            sigma_mult: env_f64_any(&["EVPOLY_ENDGAME_SIGMA_MULT".to_string()])
                .unwrap_or(1.15)
                .clamp(0.5, 5.0),
            depth_thick_threshold: env_f64_any(&[
                "EVPOLY_ENDGAME_DEPTH_THICK_THRESHOLD".to_string()
            ])
            .unwrap_or(10.0)
            .clamp(0.1, 100.0),
            depth_sigma_scale: env_f64_any(&["EVPOLY_ENDGAME_DEPTH_SIGMA_SCALE".to_string()])
                .unwrap_or(0.35)
                .clamp(0.0, 5.0),
            depth_sigma_max_mult: env_f64_any(&["EVPOLY_ENDGAME_DEPTH_SIGMA_MAX_MULT".to_string()])
                .unwrap_or(3.0)
                .clamp(1.0, 20.0),
            thin_flip_guard_enable: false,
            thin_flip_guard_bps: 10.0,
            thin_flip_guard_max_depth_usd: 30_000.0,
            guard_v3_enable: false,
            guard_v3_fail_open: false,
            guard_v3_near_scale: 0.9224,
            guard_v3_impact_scale: 1.5806,
            guard_v3_hard_cutoff: 0.6938,
            guard_v3_soft_scale: 0.3248,
            guard_v3_last_tick_guard: false,
            guard_v3_min_soft_multiplier: 0.2072,
            divergence_threshold: env_f64_any(&["EVPOLY_ENDGAME_DIVERGENCE_THRESHOLD".to_string()])
                .unwrap_or(0.18)
                .clamp(0.0, 1.0),
            divergence_min_size_usd: env_f64_any(&[
                "EVPOLY_ENDGAME_DIVERGENCE_MIN_SIZE_USD".to_string()
            ])
            .unwrap_or(10.0)
            .max(1.0),
            divergence_min_size_ratio: env_f64_any(&[
                "EVPOLY_ENDGAME_DIVERGENCE_MIN_SIZE_RATIO".to_string()
            ])
            .unwrap_or(0.10)
            .clamp(0.0, 1.0),
        }
    }

    pub fn enabled_timeframes(&self) -> Vec<crate::strategy::Timeframe> {
        parse_endgame_timeframes(self.enabled_timeframes_csv.as_str())
    }

    pub fn enabled_symbols(&self) -> Vec<String> {
        parse_endgame_symbols(self.enabled_symbols_csv.as_str())
    }

    pub fn period_cap_usd(&self) -> f64 {
        self.per_period_cap_usd
    }

    pub fn timeframe_enabled(&self, timeframe: crate::strategy::Timeframe) -> bool {
        self.enabled_timeframes()
            .iter()
            .any(|candidate| *candidate == timeframe)
    }

    pub fn per_tick_notional_for_offset(&self, offset_sec: u64) -> f64 {
        self.per_tick_notional_by_offset
            .iter()
            .find_map(|(offset, usd)| (*offset == offset_sec).then_some(*usd))
            .unwrap_or(self.per_tick_notional_usd)
            .max(0.0)
    }

    pub fn per_tick_notional_for_symbol_offset(&self, symbol: &str, offset_sec: u64) -> f64 {
        let normalized_symbol = normalize_endgame_symbol(symbol);
        self.per_tick_notional_by_symbol
            .get(normalized_symbol.as_str())
            .and_then(|offset_pairs| {
                offset_pairs
                    .iter()
                    .find_map(|(offset, usd)| (*offset == offset_sec).then_some(*usd))
            })
            .unwrap_or_else(|| self.per_tick_notional_for_offset(offset_sec))
            .max(0.0)
    }
}

fn parse_endgame_timeframes(raw: &str) -> Vec<crate::strategy::Timeframe> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let normalized = part.trim().to_ascii_lowercase();
        let timeframe = match normalized.as_str() {
            "5m" => Some(crate::strategy::Timeframe::M5),
            "15m" => Some(crate::strategy::Timeframe::M15),
            "1h" | "60m" => Some(crate::strategy::Timeframe::H1),
            "4h" | "240m" => Some(crate::strategy::Timeframe::H4),
            _ => None,
        };
        if let Some(tf) = timeframe {
            if !out.contains(&tf) {
                out.push(tf);
            }
        }
    }
    if out.is_empty() {
        out.push(crate::strategy::Timeframe::M5);
        out.push(crate::strategy::Timeframe::M15);
        out.push(crate::strategy::Timeframe::H1);
        out.push(crate::strategy::Timeframe::H4);
    }
    out
}

fn parse_endgame_offsets(raw: &str) -> Vec<u64> {
    raw.split(',')
        .filter_map(|part| part.trim().parse::<u64>().ok())
        .filter(|offset| *offset > 0)
        .collect()
}

fn normalize_endgame_symbol(raw: &str) -> String {
    match raw.trim().to_ascii_uppercase().as_str() {
        "BTC" => "BTC".to_string(),
        "ETH" => "ETH".to_string(),
        "SOL" | "SOLANA" => "SOL".to_string(),
        "XRP" => "XRP".to_string(),
        other => other.to_string(),
    }
}

fn parse_endgame_symbols(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let normalized = normalize_endgame_symbol(part);
        if normalized.is_empty() {
            continue;
        }
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    if out.is_empty() {
        out.push("BTC".to_string());
        out.push("ETH".to_string());
        out.push("SOL".to_string());
        out.push("XRP".to_string());
    }
    out
}

fn parse_endgame_tick_notionals_by_offset(raw: &str, offset_scale_ms: u64) -> Vec<(u64, f64)> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((offset_raw, usd_raw)) = trimmed.split_once(':') else {
            continue;
        };
        let Some(offset_sec) = offset_raw.trim().parse::<u64>().ok() else {
            continue;
        };
        let Some(usd) = usd_raw.trim().parse::<f64>().ok() else {
            continue;
        };
        if offset_sec == 0 || !usd.is_finite() || usd < 0.0 {
            continue;
        }
        out.push((offset_sec.saturating_mul(offset_scale_ms.max(1)), usd));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

fn parse_endgame_tick_notionals_by_symbol(
    raw: &str,
    tick_offsets_sec: &[u64],
    explicit_offset_scale_ms: u64,
) -> HashMap<String, Vec<(u64, f64)>> {
    let mut out = HashMap::new();
    for entry in raw.split(',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((symbol_raw, sizing_raw)) = trimmed.split_once(':') else {
            continue;
        };
        let symbol = normalize_endgame_symbol(symbol_raw);
        if symbol.is_empty() {
            continue;
        }
        let mut parsed: Vec<(u64, f64)> = Vec::new();
        let size_parts = sizing_raw
            .replace('/', "|")
            .split('|')
            .map(|part| part.trim().to_string())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        if size_parts.is_empty() {
            continue;
        }

        let explicit_offsets = size_parts
            .iter()
            .all(|part| part.contains(':') && part.split(':').count() == 2);
        if explicit_offsets {
            for part in size_parts {
                let Some((offset_raw, usd_raw)) = part.split_once(':') else {
                    continue;
                };
                let Some(offset_sec) = offset_raw.trim().parse::<u64>().ok() else {
                    continue;
                };
                let Some(usd) = usd_raw.trim().parse::<f64>().ok() else {
                    continue;
                };
                if offset_sec == 0 || !usd.is_finite() || usd < 0.0 {
                    continue;
                }
                parsed.push((
                    offset_sec.saturating_mul(explicit_offset_scale_ms.max(1)),
                    usd,
                ));
            }
        } else {
            for (idx, part) in size_parts.iter().enumerate() {
                let Some(usd) = part.parse::<f64>().ok() else {
                    continue;
                };
                let Some(offset_sec) = tick_offsets_sec.get(idx).copied() else {
                    break;
                };
                if offset_sec == 0 || !usd.is_finite() || usd < 0.0 {
                    continue;
                }
                parsed.push((offset_sec, usd));
            }
        }

        if parsed.is_empty() {
            continue;
        }
        parsed.sort_by(|a, b| b.0.cmp(&a.0));
        parsed.dedup_by(|a, b| a.0 == b.0);
        out.insert(symbol, parsed);
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureEngineV2Config {
    pub enabled: bool,
    pub version: String,
    pub hl_l4_weight_5m: f64,
    pub hl_l4_weight_15m: f64,
    pub hl_l4_weight_1h: f64,
}

impl FeatureEngineV2Config {
    pub fn from_env() -> Self {
        Self {
            enabled: env_bool_named("EVPOLY_FEATURES_V2_ENABLE").unwrap_or(false),
            version: env_nonempty(&["EVPOLY_FEATURES_V2_VERSION".to_string()])
                .unwrap_or_else(|| "v2-draft".to_string()),
            hl_l4_weight_5m: env_f64_any(&["EVPOLY_HL_L4_WEIGHT_5M".to_string()])
                .unwrap_or(0.65)
                .clamp(0.0, 1.0),
            hl_l4_weight_15m: env_f64_any(&["EVPOLY_HL_L4_WEIGHT_15M".to_string()])
                .unwrap_or(0.50)
                .clamp(0.0, 1.0),
            hl_l4_weight_1h: env_f64_any(&["EVPOLY_HL_L4_WEIGHT_1H".to_string()])
                .unwrap_or(0.35)
                .clamp(0.0, 1.0),
        }
    }
}

fn env_nonempty(keys: &[String]) -> Option<String> {
    for key in keys {
        if let Ok(value) = env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn env_u64_any(keys: &[String]) -> Option<u64> {
    for key in keys {
        if let Ok(value) = env::var(key) {
            if let Ok(parsed) = value.trim().parse::<u64>() {
                return Some(parsed);
            }
        }
    }
    None
}

fn env_i64_any(keys: &[String]) -> Option<i64> {
    for key in keys {
        if let Ok(value) = env::var(key) {
            if let Ok(parsed) = value.trim().parse::<i64>() {
                return Some(parsed);
            }
        }
    }
    None
}

fn env_f64_any(keys: &[String]) -> Option<f64> {
    for key in keys {
        if let Ok(value) = env::var(key) {
            if let Ok(parsed) = value.trim().parse::<f64>() {
                return Some(parsed);
            }
        }
    }
    None
}

fn env_bool_named(key: &str) -> Option<bool> {
    env::var(key).ok().and_then(|v| {
        let normalized = v.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "1" | "true" | "yes" | "y" | "on" => Some(true),
            "0" | "false" | "no" | "n" | "off" => Some(false),
            _ => None,
        }
    })
}

const fn default_hold_to_resolution() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            polymarket: PolymarketConfig {
                gamma_api_url: "https://gamma-api.polymarket.com".to_string(),
                clob_api_url: "https://clob.polymarket.com".to_string(),
                private_key: None,
                proxy_wallet_address: None,
                signature_type: None,
            },
            trading: TradingConfig {
                eth_condition_id: None,
                btc_condition_id: None,
                solana_condition_id: None,
                xrp_condition_id: None,
                check_interval_ms: 1000,
                fixed_trade_amount: 1.0,                          // $1.00
                trigger_price: 0.9,                               // $0.90 trigger price
                min_elapsed_minutes: 10,                          // 10 minutes must have elapsed
                sell_price: 0.99,                                 // Sell at $0.99
                hold_to_resolution: default_hold_to_resolution(), // Prefer redeem-at-resolution over active exits
                hold_to_resolution_ladder: None,
                hold_to_resolution_reactive: None,
                max_buy_price: Some(0.95), // Maximum price to buy at ($0.95)
                stop_loss_price: Some(0.85), // Stop-loss at $0.85 (sell if price drops below this)
                hedge_price: Some(0.5),    // Hedge price at $0.5 (limit buy for opposite token)
                market_closure_check_interval_seconds:
                    MARKET_CLOSURE_CHECK_INTERVAL_SECONDS_HARDCODED,
                min_time_remaining_seconds: Some(30), // 30 seconds - don't buy if less time remains
                enable_eth_trading: true,             // ETH trading enabled by default
                enable_solana_trading: true,          // Solana trading enabled by default
                enable_xrp_trading: true,             // XRP trading enabled by default
                dual_limit_price: None,
                dual_limit_shares: None,
                order_ttl_seconds: Some(1200),
            },
        }
    }
}

impl Config {
    pub fn load(path: &PathBuf) -> anyhow::Result<Self> {
        let path_exists = path.exists();

        let mut config = if path_exists {
            let content = std::fs::read_to_string(path)?;
            serde_json::from_str(&content)?
        } else {
            Config::default()
        };

        // Load key=value pairs from local env files into process env (without overriding existing env vars).
        // Priority: shell/process env > .env.
        let loaded_env_files = Self::load_env_files(&[".env"])?;
        if !loaded_env_files.is_empty() {
            eprintln!(
                "🧾 Loaded env overrides from: {}",
                loaded_env_files.join(", ")
            );
        }

        // Apply env overrides to runtime config.
        // If config.json is missing, this also populates generated config.json with env-backed values.
        config.apply_env_overrides();

        if !path_exists {
            let content = serde_json::to_string_pretty(&config)?;
            std::fs::write(path, content)?;
        }

        Ok(config)
    }

    fn load_env_files(candidates: &[&str]) -> anyhow::Result<Vec<String>> {
        let mut loaded = Vec::new();
        for filename in candidates {
            if Self::load_env_file(filename)? {
                loaded.push((*filename).to_string());
            }
        }
        Ok(loaded)
    }

    fn load_env_file(filename: &str) -> anyhow::Result<bool> {
        let path = Path::new(filename);
        if !path.exists() {
            return Ok(false);
        }

        let content = std::fs::read_to_string(path)?;
        for raw_line in content.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let Some((raw_key, raw_value)) = line.split_once('=') else {
                continue;
            };

            let key = raw_key.trim();
            if key.is_empty() || env::var_os(key).is_some() {
                continue;
            }

            // Support inline comments in the template style: VALUE  # comment
            let mut value = raw_value.trim().to_string();
            if let Some((before_comment, _)) = value.split_once(" #") {
                value = before_comment.trim().to_string();
            }

            // Strip single/double wrapping quotes.
            if value.len() >= 2 {
                let starts_quoted = value.starts_with('"') && value.ends_with('"');
                let starts_single_quoted = value.starts_with('\'') && value.ends_with('\'');
                if starts_quoted || starts_single_quoted {
                    value = value[1..value.len() - 1].to_string();
                }
            }

            env::set_var(key, value);
        }

        Ok(true)
    }

    fn apply_env_overrides(&mut self) {
        if let Some(v) = Self::env_string("POLY_GAMMA_API_URL") {
            self.polymarket.gamma_api_url = v;
        }
        if let Some(v) = Self::env_string("POLY_CLOB_API_URL") {
            self.polymarket.clob_api_url = v;
        }
        if let Some(v) = Self::env_string("POLY_PRIVATE_KEY") {
            self.polymarket.private_key = Some(v);
        }
        if let Some(v) = Self::env_string("POLY_PROXY_WALLET_ADDRESS") {
            self.polymarket.proxy_wallet_address = Some(v);
        }
        if let Some(v) = Self::env_u8("POLY_SIGNATURE_TYPE") {
            self.polymarket.signature_type = Some(v);
        }

        if let Some(v) = Self::env_string("POLY_ETH_CONDITION_ID") {
            self.trading.eth_condition_id = Some(v);
        }
        if let Some(v) = Self::env_string("POLY_BTC_CONDITION_ID") {
            self.trading.btc_condition_id = Some(v);
        }
        if let Some(v) = Self::env_string("POLY_SOLANA_CONDITION_ID") {
            self.trading.solana_condition_id = Some(v);
        }
        if let Some(v) = Self::env_string("POLY_XRP_CONDITION_ID") {
            self.trading.xrp_condition_id = Some(v);
        }

        if let Some(v) = Self::env_u64("POLY_CHECK_INTERVAL_MS") {
            self.trading.check_interval_ms = v;
        }
        if let Some(v) = Self::env_f64("POLY_FIXED_TRADE_AMOUNT") {
            self.trading.fixed_trade_amount = v;
        }
        if let Some(v) = Self::env_f64("POLY_TRIGGER_PRICE") {
            self.trading.trigger_price = v;
        }
        if let Some(v) = Self::env_u64("POLY_MIN_ELAPSED_MINUTES") {
            self.trading.min_elapsed_minutes = v;
        }
        if let Some(v) = Self::env_f64("POLY_SELL_PRICE") {
            self.trading.sell_price = v;
        }
        if let Some(v) = Self::env_bool("POLY_HOLD_TO_RESOLUTION") {
            self.trading.hold_to_resolution = v;
        }
        if let Some(v) = Self::env_bool("POLY_HOLD_TO_RESOLUTION_LADDER") {
            self.trading.hold_to_resolution_ladder = Some(v);
        }
        if let Some(v) = Self::env_bool("POLY_HOLD_TO_RESOLUTION_REACTIVE") {
            self.trading.hold_to_resolution_reactive = Some(v);
        }
        if let Some(v) = Self::env_f64("POLY_MAX_BUY_PRICE") {
            self.trading.max_buy_price = Some(v);
        }
        if let Some(v) = Self::env_f64("POLY_STOP_LOSS_PRICE") {
            self.trading.stop_loss_price = Some(v);
        }
        if let Some(v) = Self::env_f64("POLY_HEDGE_PRICE") {
            self.trading.hedge_price = Some(v);
        }
        // Hardcoded by design to reduce balance-allowance pressure from closure polling.
        // Keep fixed regardless of config.json or env content.
        self.trading.market_closure_check_interval_seconds =
            MARKET_CLOSURE_CHECK_INTERVAL_SECONDS_HARDCODED;
        if let Some(v) = Self::env_u64("POLY_MIN_TIME_REMAINING_SECONDS") {
            self.trading.min_time_remaining_seconds = Some(v);
        }
        if let Some(v) = Self::env_bool("POLY_ENABLE_ETH_TRADING") {
            self.trading.enable_eth_trading = v;
        }
        if let Some(v) = Self::env_bool("POLY_ENABLE_SOLANA_TRADING") {
            self.trading.enable_solana_trading = v;
        }
        if let Some(v) = Self::env_bool("POLY_ENABLE_XRP_TRADING") {
            self.trading.enable_xrp_trading = v;
        }
        if let Some(v) = Self::env_f64("POLY_DUAL_LIMIT_PRICE") {
            self.trading.dual_limit_price = Some(v);
        }
        if let Some(v) = Self::env_f64("POLY_DUAL_LIMIT_SHARES") {
            self.trading.dual_limit_shares = Some(v);
        }
        if let Some(v) = Self::env_u64("POLY_ORDER_TTL_SECONDS") {
            self.trading.order_ttl_seconds = Some(v);
        }
    }

    fn env_string(key: &str) -> Option<String> {
        env::var(key).ok().and_then(|v| {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
    }

    fn env_u8(key: &str) -> Option<u8> {
        env::var(key).ok().and_then(|v| v.trim().parse::<u8>().ok())
    }

    fn env_u64(key: &str) -> Option<u64> {
        env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
    }

    fn env_f64(key: &str) -> Option<f64> {
        env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
    }

    fn env_bool(key: &str) -> Option<bool> {
        env::var(key).ok().and_then(|v| {
            let normalized = v.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "1" | "true" | "yes" | "y" | "on" => Some(true),
                "0" | "false" | "no" | "n" | "off" => Some(false),
                _ => None,
            }
        })
    }
}
