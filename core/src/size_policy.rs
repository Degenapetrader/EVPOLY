use crate::strategy::Timeframe;
use std::sync::OnceLock;

const DEFAULT_BASE_SIZE_USD: f64 = 10.0;
const DEFAULT_ENDGAME_TICK_MULTIPLIERS: [f64; 3] = [0.20, 0.40, 0.40];
const DEFAULT_SESSIONBAND_TAU2_MULTIPLIER: f64 = 0.30;
const DEFAULT_SESSIONBAND_TAU1_MULTIPLIER: f64 = 0.70;

pub fn base_size_usd_from_env(env_key: &str) -> f64 {
    std::env::var(env_key)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(DEFAULT_BASE_SIZE_USD)
}

pub fn symbol_size_multiplier(symbol: &str) -> f64 {
    match normalize_symbol(symbol).as_str() {
        "BTC" => 1.0,
        "ETH" => 0.8,
        "SOL" | "XRP" | "DOGE" | "BNB" | "HYPE" => 0.5,
        _ => 1.0,
    }
}

pub fn premarket_timeframe_multiplier(timeframe: Timeframe) -> f64 {
    match timeframe {
        Timeframe::M5 => 0.75,
        Timeframe::M15 => 1.0,
        Timeframe::H1 | Timeframe::H4 | Timeframe::D1 => 1.25,
    }
}

pub fn evcurve_timeframe_multiplier(timeframe: Timeframe) -> f64 {
    match timeframe {
        Timeframe::M15 => 0.75,
        Timeframe::H1 => 1.0,
        Timeframe::H4 | Timeframe::D1 => 1.25,
        _ => 1.0,
    }
}

pub fn endgame_tick_multiplier(tick_index: u32) -> Option<f64> {
    let multipliers = endgame_tick_multipliers();
    match tick_index {
        0 => Some(multipliers[0]),
        1 => Some(multipliers[1]),
        2 => Some(multipliers[2]),
        _ => None,
    }
}

pub fn sessionband_tau_multiplier(tau_sec: i64) -> Option<f64> {
    let (tau2_multiplier, tau1_multiplier) = sessionband_tau_multipliers();
    match tau_sec {
        2 => Some(tau2_multiplier),
        1 => Some(tau1_multiplier),
        _ => None,
    }
}

fn endgame_tick_multipliers() -> &'static [f64; 3] {
    static MULTIPLIERS: OnceLock<[f64; 3]> = OnceLock::new();
    MULTIPLIERS.get_or_init(|| {
        [
            env_nonnegative_f64(
                "EVPOLY_ENDGAME_TICK0_MULTIPLIER",
                DEFAULT_ENDGAME_TICK_MULTIPLIERS[0],
            ),
            env_nonnegative_f64(
                "EVPOLY_ENDGAME_TICK1_MULTIPLIER",
                DEFAULT_ENDGAME_TICK_MULTIPLIERS[1],
            ),
            env_nonnegative_f64(
                "EVPOLY_ENDGAME_TICK2_MULTIPLIER",
                DEFAULT_ENDGAME_TICK_MULTIPLIERS[2],
            ),
        ]
    })
}

fn sessionband_tau_multipliers() -> (f64, f64) {
    static MULTIPLIERS: OnceLock<(f64, f64)> = OnceLock::new();
    *MULTIPLIERS.get_or_init(|| {
        (
            env_nonnegative_f64(
                "EVPOLY_SESSIONBAND_TAU2_MULTIPLIER",
                DEFAULT_SESSIONBAND_TAU2_MULTIPLIER,
            ),
            env_nonnegative_f64(
                "EVPOLY_SESSIONBAND_TAU1_MULTIPLIER",
                DEFAULT_SESSIONBAND_TAU1_MULTIPLIER,
            ),
        )
    })
}

fn env_nonnegative_f64(env_key: &str, default: f64) -> f64 {
    std::env::var(env_key)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(default)
}

pub fn strategy_symbol_size_usd(base_size_usd: f64, symbol: &str) -> f64 {
    (base_size_usd.max(0.0) * symbol_size_multiplier(symbol)).max(0.0)
}

pub fn strategy_symbol_timeframe_size_usd(
    base_size_usd: f64,
    symbol: &str,
    timeframe_multiplier: f64,
) -> f64 {
    (strategy_symbol_size_usd(base_size_usd, symbol) * timeframe_multiplier.max(0.0)).max(0.0)
}

fn normalize_symbol(symbol: &str) -> String {
    match symbol.trim().to_ascii_uppercase().as_str() {
        "SOLANA" => "SOL".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_multiplier_matches_policy() {
        assert_eq!(symbol_size_multiplier("BTC"), 1.0);
        assert_eq!(symbol_size_multiplier("ETH"), 0.8);
        assert_eq!(symbol_size_multiplier("SOL"), 0.5);
        assert_eq!(symbol_size_multiplier("SOLANA"), 0.5);
        assert_eq!(symbol_size_multiplier("XRP"), 0.5);
        assert_eq!(symbol_size_multiplier("DOGE"), 0.5);
        assert_eq!(symbol_size_multiplier("BNB"), 0.5);
        assert_eq!(symbol_size_multiplier("HYPE"), 0.5);
    }

    #[test]
    fn premarket_timeframe_multiplier_matches_policy() {
        assert_eq!(premarket_timeframe_multiplier(Timeframe::M5), 0.75);
        assert_eq!(premarket_timeframe_multiplier(Timeframe::M15), 1.0);
        assert_eq!(premarket_timeframe_multiplier(Timeframe::H1), 1.25);
        assert_eq!(premarket_timeframe_multiplier(Timeframe::H4), 1.25);
    }

    #[test]
    fn evcurve_timeframe_multiplier_matches_policy() {
        assert_eq!(evcurve_timeframe_multiplier(Timeframe::M15), 0.75);
        assert_eq!(evcurve_timeframe_multiplier(Timeframe::H1), 1.0);
        assert_eq!(evcurve_timeframe_multiplier(Timeframe::H4), 1.25);
        assert_eq!(evcurve_timeframe_multiplier(Timeframe::D1), 1.25);
    }

    #[test]
    fn endgame_tick_multiplier_matches_policy() {
        assert_eq!(endgame_tick_multiplier(0), Some(0.20));
        assert_eq!(endgame_tick_multiplier(1), Some(0.40));
        assert_eq!(endgame_tick_multiplier(2), Some(0.40));
        assert_eq!(endgame_tick_multiplier(3), None);
    }

    #[test]
    fn sessionband_tau_multiplier_matches_policy() {
        assert_eq!(sessionband_tau_multiplier(2), Some(0.30));
        assert_eq!(sessionband_tau_multiplier(1), Some(0.70));
        assert_eq!(sessionband_tau_multiplier(3), None);
    }
}
