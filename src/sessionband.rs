use std::collections::HashSet;

use crate::config::StrategyExecutionSizeMode;
use crate::size_policy;
use crate::strategy::{Timeframe, STRATEGY_ID_SESSIONBAND_V1};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LeadPriceBand {
    pub lead_min_pct: f64,
    pub lead_max_pct: f64,
    pub price_min: f64,
    pub price_max: f64,
}

impl LeadPriceBand {
    pub fn contains_lead_pct(self, lead_pct: f64) -> bool {
        lead_pct.is_finite() && lead_pct >= self.lead_min_pct && lead_pct < self.lead_max_pct
    }

    pub fn contains_price(self, price: f64) -> bool {
        price.is_finite() && price >= self.price_min && price <= self.price_max
    }
}

#[derive(Debug, Clone)]
pub struct SessionBandExecutionConfig {
    pub enable: bool,
    pub poll_interval_ms: u64,
    pub symbols: Vec<String>,
    pub timeframes: Vec<Timeframe>,
    pub flip_threshold_pct: f64,
    pub prewatch_sec: i64,
    pub bands: Vec<LeadPriceBand>,
    pub proxy_stale_ms: i64,
    pub book_freshness_ms: i64,
    pub single_attempt_per_period: bool,
    pub base_anchor_grace_sec: i64,
    pub strategy_cap_usd: f64,
    pub execution_size_mode: StrategyExecutionSizeMode,
    pub base_size_shares: f64,
    base_size_usd: f64,
    allowed_tau_sec: Option<HashSet<i64>>,
}

impl SessionBandExecutionConfig {
    pub fn from_env() -> Self {
        let symbols =
            parse_symbols_env("EVPOLY_SESSIONBAND_SYMBOLS", &["BTC", "ETH", "SOL", "XRP"]);
        let timeframes = parse_timeframes_env(
            "EVPOLY_SESSIONBAND_TIMEFRAMES",
            &[Timeframe::M5, Timeframe::M15, Timeframe::H1, Timeframe::H4],
        );
        let tau2_enabled = env_bool("EVPOLY_SESSIONBAND_TAU2_ENABLE", true);
        let tau1_enabled = env_bool("EVPOLY_SESSIONBAND_TAU1_ENABLE", true);
        let bands = std::env::var("EVPOLY_SESSIONBAND_BANDS")
            .ok()
            .and_then(|v| parse_bands(v.as_str()))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(default_bands);
        let allowed_tau_sec = parse_tau_allowlist_env("EVPOLY_SESSIONBAND_ALLOWED_TAU_SEC")
            .or_else(|| Some(legacy_tau_allowlist(tau1_enabled, tau2_enabled)));

        Self {
            enable: false,
            poll_interval_ms: env_u64("EVPOLY_SESSIONBAND_POLL_MS", 250).max(100),
            symbols,
            timeframes,
            flip_threshold_pct: env_f64("EVPOLY_SESSIONBAND_FLIP_THRESHOLD_PCT", 2.0)
                .clamp(0.0, 100.0),
            prewatch_sec: env_i64("EVPOLY_SESSIONBAND_PREWATCH_SEC", 1).max(0),
            bands,
            proxy_stale_ms: env_i64("EVPOLY_SESSIONBAND_PROXY_STALE_MS", 1_000).max(100),
            book_freshness_ms: env_i64("EVPOLY_SESSIONBAND_BOOK_FRESHNESS_MS", 1_500).max(100),
            single_attempt_per_period: env_bool(
                "EVPOLY_SESSIONBAND_SINGLE_ATTEMPT_PER_PERIOD",
                true,
            ),
            base_anchor_grace_sec: env_i64("EVPOLY_SESSIONBAND_BASE_ANCHOR_GRACE_SEC", 2).max(0),
            strategy_cap_usd: env_f64("EVPOLY_SESSIONBAND_STRATEGY_CAP_USD", 10_000.0).max(1.0),
            execution_size_mode: StrategyExecutionSizeMode::from_env(
                std::env::var("EVPOLY_SESSIONBAND_EXECUTION_SIZE_MODE")
                    .ok()
                    .as_deref(),
            ),
            base_size_shares: env_f64("EVPOLY_SESSIONBAND_BASE_SIZE_SHARES", 5.0).max(0.0),
            base_size_usd: size_policy::base_size_usd_from_env("EVPOLY_SESSIONBAND_BASE_SIZE_USD"),
            allowed_tau_sec,
        }
    }

    pub fn enabled_symbols(&self) -> Vec<String> {
        self.symbols.clone()
    }

    pub fn enabled_timeframes(&self) -> Vec<Timeframe> {
        self.timeframes.clone()
    }

    pub fn timeframe_enabled(&self, timeframe: Timeframe) -> bool {
        self.timeframes.contains(&timeframe)
    }

    pub fn size_usd_for_symbol(&self, symbol: &str) -> f64 {
        size_policy::strategy_symbol_size_usd(self.base_size_usd, symbol).max(1.0)
    }

    pub fn size_mode(&self) -> StrategyExecutionSizeMode {
        self.execution_size_mode
    }

    pub fn uses_share_size(&self) -> bool {
        self.execution_size_mode.uses_share_size()
    }

    pub fn size_shares_for_symbol(&self, symbol: &str) -> f64 {
        size_policy::strategy_symbol_scaled_size(self.base_size_shares, symbol).max(0.0)
    }

    pub fn scope_cap_usd(&self, symbol: &str, timeframe: Timeframe) -> f64 {
        let _ = timeframe;
        if self.uses_share_size() {
            (self.size_shares_for_symbol(symbol) * 0.99).max(0.0)
        } else {
            self.size_usd_for_symbol(symbol)
        }
    }

    pub fn band_for_lead_pct(&self, lead_pct: f64) -> Option<LeadPriceBand> {
        self.bands
            .iter()
            .copied()
            .find(|band| band.contains_lead_pct(lead_pct))
    }

    pub fn tau_allowed(&self, tau_sec: i64) -> bool {
        if tau_sec <= 0 {
            return false;
        }
        self.allowed_tau_sec
            .as_ref()
            .map(|allowed| allowed.contains(&tau_sec))
            .unwrap_or(true)
    }
}

fn parse_bands(raw: &str) -> Option<Vec<LeadPriceBand>> {
    let mut out = Vec::new();
    for pair in raw.split(',') {
        let part = pair.trim();
        if part.is_empty() {
            continue;
        }
        let (lead_range, price_range) = part.split_once(':')?;
        let (lead_min, lead_max) = parse_range(lead_range)?;
        let (price_min, price_max) = parse_range(price_range)?;
        if lead_min < 0.0
            || lead_max <= lead_min
            || price_min <= 0.0
            || price_max < price_min
            || price_max > 1.0
        {
            return None;
        }
        out.push(LeadPriceBand {
            lead_min_pct: lead_min,
            lead_max_pct: lead_max,
            price_min,
            price_max,
        });
    }
    if out.is_empty() {
        return None;
    }
    out.sort_by(|a, b| a.lead_min_pct.total_cmp(&b.lead_min_pct));
    Some(out)
}

fn parse_range(raw: &str) -> Option<(f64, f64)> {
    let (min_s, max_s) = raw.split_once('-')?;
    let min_v = min_s.trim().parse::<f64>().ok()?;
    let max_v = max_s.trim().parse::<f64>().ok()?;
    if !min_v.is_finite() || !max_v.is_finite() {
        return None;
    }
    Some((min_v, max_v))
}

fn default_bands() -> Vec<LeadPriceBand> {
    vec![
        LeadPriceBand {
            lead_min_pct: 0.0,
            lead_max_pct: 0.1,
            price_min: 0.99,
            price_max: 0.99,
        },
        LeadPriceBand {
            lead_min_pct: 0.1,
            lead_max_pct: 0.5,
            price_min: 0.98,
            price_max: 0.99,
        },
        LeadPriceBand {
            lead_min_pct: 0.5,
            lead_max_pct: 1.0,
            price_min: 0.96,
            price_max: 0.98,
        },
        LeadPriceBand {
            lead_min_pct: 1.0,
            lead_max_pct: 1.5,
            price_min: 0.94,
            price_max: 0.96,
        },
        LeadPriceBand {
            lead_min_pct: 1.5,
            lead_max_pct: 2.0,
            price_min: 0.90,
            price_max: 0.94,
        },
    ]
}

fn parse_tau_allowlist_env(key: &str) -> Option<HashSet<i64>> {
    let raw = std::env::var(key).ok()?;
    let mut out = HashSet::new();
    for part in raw.split(',') {
        let Some(tau_sec) = part.trim().parse::<i64>().ok() else {
            continue;
        };
        if tau_sec > 0 {
            out.insert(tau_sec);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn legacy_tau_allowlist(tau1_enabled: bool, tau2_enabled: bool) -> HashSet<i64> {
    let mut allowed = HashSet::new();
    if tau1_enabled {
        allowed.insert(1);
    }
    if tau2_enabled {
        allowed.insert(2);
    }
    allowed
}

fn normalize_symbol(symbol: &str) -> String {
    match symbol.trim().to_ascii_uppercase().as_str() {
        "SOLANA" => "SOL".to_string(),
        other => other.to_string(),
    }
}

fn parse_symbols_env(key: &str, default: &[&str]) -> Vec<String> {
    let parsed = std::env::var(key)
        .ok()
        .map(|raw| {
            crate::symbol_ownership::parse_symbols_csv_for_strategy(
                STRATEGY_ID_SESSIONBAND_V1,
                &raw,
            )
        })
        .unwrap_or_default();
    if parsed.is_empty() {
        let defaults = default
            .iter()
            .map(|v| normalize_symbol(v))
            .collect::<Vec<_>>();
        return crate::symbol_ownership::filter_symbols_for_strategy(
            STRATEGY_ID_SESSIONBAND_V1,
            defaults.as_slice(),
        );
    }
    crate::symbol_ownership::filter_symbols_for_strategy(STRATEGY_ID_SESSIONBAND_V1, &parsed)
}

fn parse_timeframes_env(key: &str, default: &[Timeframe]) -> Vec<Timeframe> {
    let parsed = std::env::var(key)
        .ok()
        .map(|raw| {
            raw.split(',')
                .filter_map(|item| match item.trim().to_ascii_lowercase().as_str() {
                    "5m" => Some(Timeframe::M5),
                    "15m" => Some(Timeframe::M15),
                    "1h" | "60m" => Some(Timeframe::H1),
                    "4h" | "240m" => Some(Timeframe::H4),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if parsed.is_empty() {
        return default.to_vec();
    }
    let mut deduped = Vec::new();
    for timeframe in parsed {
        if !deduped.contains(&timeframe) {
            deduped.push(timeframe);
        }
    }
    deduped
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .and_then(|v| match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn sessionband_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_sessionband_env<F: FnOnce()>(updates: &[(&str, Option<&str>)], f: F) {
        let _guard = sessionband_env_lock()
            .lock()
            .expect("sessionband env lock poisoned");
        let mut previous: Vec<(&str, Option<String>)> = Vec::with_capacity(updates.len());
        for (name, value) in updates {
            previous.push((*name, std::env::var(name).ok()));
            unsafe {
                match value {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
        }
        f();
        for (name, value) in previous {
            unsafe {
                match value {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn band_parser_parses_default_shape() {
        let raw =
            "0.0-0.1:0.99-0.99,0.1-0.5:0.98-0.99,0.5-1.0:0.96-0.98,1.0-1.5:0.94-0.96,1.5-2.0:0.90-0.94";
        let bands = parse_bands(raw).expect("bands");
        assert_eq!(bands.len(), 5);
        assert!(bands[0].contains_lead_pct(0.05));
        assert!(bands[0].contains_price(0.99));
        assert!(!bands[0].contains_price(0.98));
        assert!(bands[4].contains_lead_pct(1.9));
    }

    #[test]
    fn size_policy_uses_base_and_symbol_multipliers() {
        with_sessionband_env(
            &[
                ("EVPOLY_SESSIONBAND_BASE_SIZE_USD", Some("100")),
                ("EVPOLY_SESSIONBAND_EXECUTION_SIZE_MODE", None),
            ],
            || {
                let cfg = SessionBandExecutionConfig::from_env();
                assert!((cfg.size_usd_for_symbol("BTC") - 100.0).abs() < 1e-9);
                assert!((cfg.size_usd_for_symbol("ETH") - 80.0).abs() < 1e-9);
                assert!((cfg.size_usd_for_symbol("SOL") - 50.0).abs() < 1e-9);
                assert!((cfg.size_usd_for_symbol("XRP") - 50.0).abs() < 1e-9);
                assert!((cfg.scope_cap_usd("ETH", Timeframe::M5) - 80.0).abs() < 1e-9);
                assert!((cfg.scope_cap_usd("ETH", Timeframe::H4) - 80.0).abs() < 1e-9);
            },
        );
    }

    #[test]
    fn enable_is_hardcoded_off() {
        with_sessionband_env(
            &[("EVPOLY_STRATEGY_SESSIONBAND_ENABLE", Some("true"))],
            || {
                let cfg = SessionBandExecutionConfig::from_env();
                assert!(!cfg.enable);
            },
        );
    }

    #[test]
    fn execution_size_mode_defaults_to_usd() {
        with_sessionband_env(
            &[
                ("EVPOLY_SESSIONBAND_EXECUTION_SIZE_MODE", None),
                ("EVPOLY_SESSIONBAND_BASE_SIZE_SHARES", None),
            ],
            || {
                let cfg = SessionBandExecutionConfig::from_env();
                assert_eq!(cfg.size_mode(), StrategyExecutionSizeMode::Usd);
                assert!(!cfg.uses_share_size());
                assert!((cfg.base_size_shares - 5.0).abs() < 1e-9);
            },
        );
    }

    #[test]
    fn share_mode_uses_share_base_for_scope_cap() {
        with_sessionband_env(
            &[
                ("EVPOLY_SESSIONBAND_EXECUTION_SIZE_MODE", Some("shares")),
                ("EVPOLY_SESSIONBAND_BASE_SIZE_SHARES", Some("100")),
            ],
            || {
                let cfg = SessionBandExecutionConfig::from_env();
                assert_eq!(cfg.size_mode(), StrategyExecutionSizeMode::Shares);
                assert!(cfg.uses_share_size());
                assert!((cfg.size_shares_for_symbol("ETH") - 80.0).abs() < 1e-9);
                assert!((cfg.scope_cap_usd("ETH", Timeframe::M5) - 79.2).abs() < 1e-9);
                assert!((cfg.scope_cap_usd("ETH", Timeframe::H4) - 79.2).abs() < 1e-9);
            },
        );
    }

    #[test]
    fn default_timeframes_include_5m() {
        with_sessionband_env(&[("EVPOLY_SESSIONBAND_TIMEFRAMES", None)], || {
            let cfg = SessionBandExecutionConfig::from_env();
            assert_eq!(
                cfg.enabled_timeframes(),
                vec![Timeframe::M5, Timeframe::M15, Timeframe::H1, Timeframe::H4]
            );
        });
    }

    #[test]
    fn default_tau_allowlist_is_one_and_two() {
        with_sessionband_env(
            &[
                ("EVPOLY_SESSIONBAND_ALLOWED_TAU_SEC", None),
                ("EVPOLY_SESSIONBAND_TAU1_ENABLE", None),
                ("EVPOLY_SESSIONBAND_TAU2_ENABLE", None),
            ],
            || {
                let cfg = SessionBandExecutionConfig::from_env();
                assert!(cfg.tau_allowed(1));
                assert!(cfg.tau_allowed(2));
                assert!(!cfg.tau_allowed(3));
            },
        );
    }

    #[test]
    fn explicit_tau_allowlist_overrides_legacy_tau_flags() {
        with_sessionband_env(
            &[
                ("EVPOLY_SESSIONBAND_ALLOWED_TAU_SEC", Some("2")),
                ("EVPOLY_SESSIONBAND_TAU1_ENABLE", Some("true")),
                ("EVPOLY_SESSIONBAND_TAU2_ENABLE", Some("false")),
            ],
            || {
                let cfg = SessionBandExecutionConfig::from_env();
                assert!(!cfg.tau_allowed(1));
                assert!(cfg.tau_allowed(2));
                assert!(!cfg.tau_allowed(3));
            },
        );
    }

    #[test]
    fn legacy_tau_flags_backfill_when_explicit_allowlist_is_missing() {
        with_sessionband_env(
            &[
                ("EVPOLY_SESSIONBAND_ALLOWED_TAU_SEC", None),
                ("EVPOLY_SESSIONBAND_TAU1_ENABLE", Some("false")),
                ("EVPOLY_SESSIONBAND_TAU2_ENABLE", Some("true")),
            ],
            || {
                let cfg = SessionBandExecutionConfig::from_env();
                assert!(!cfg.tau_allowed(1));
                assert!(cfg.tau_allowed(2));
                assert!(!cfg.tau_allowed(3));
            },
        );
    }
}
