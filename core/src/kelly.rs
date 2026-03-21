use std::collections::HashMap;

use crate::strategy::Timeframe;

#[derive(Debug, Clone)]
pub struct KellySizingConfig {
    pub enable: bool,
    pub factor_5m: f64,
    pub factor_15m: f64,
    pub factor_1h: f64,
    pub factor_4h: f64,
    pub p0_default: f64,
    pub seed_usd: f64,
    pub prior_strength: f64,
    pub ewma_alpha: f64,
    pub blend_lambda: f64,
    pub window_trades: usize,
    pub mult_min: f64,
    pub mult_max: f64,
    pub fallback_base_pct: f64,
    p0_map: HashMap<String, f64>,
}

#[derive(Debug, Clone)]
pub struct KellyAdjustment {
    pub lane_key: String,
    pub p0: f64,
    pub p_beta: f64,
    pub p_ewma: f64,
    pub p_blend: f64,
    pub wins: u64,
    pub losses: u64,
    pub samples: u64,
    pub bankroll_usd: f64,
    pub entry_price: f64,
    pub kelly_factor: f64,
    pub f_star: f64,
    pub f_used: f64,
    pub original_target_size_usd: f64,
    pub kelly_target_size_usd: f64,
    pub final_target_size_usd: f64,
    pub size_multiplier: f64,
    pub fallback_used: bool,
}

impl KellySizingConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        enable: bool,
        factor_5m: f64,
        factor_15m: f64,
        factor_1h: f64,
        factor_4h: f64,
        p0_default: f64,
        p0_map_csv: &str,
        seed_usd: f64,
        prior_strength: f64,
        ewma_alpha: f64,
        blend_lambda: f64,
        window_trades: usize,
        mult_min: f64,
        mult_max: f64,
        fallback_base_pct: f64,
    ) -> Self {
        let p0_map = parse_kelly_p0_map(p0_map_csv);
        Self {
            enable,
            factor_5m: factor_5m.clamp(0.0, 1.0),
            factor_15m: factor_15m.clamp(0.0, 1.0),
            factor_1h: factor_1h.clamp(0.0, 1.0),
            factor_4h: factor_4h.clamp(0.0, 1.0),
            p0_default: p0_default.clamp(0.01, 0.99),
            seed_usd: seed_usd.max(1.0),
            prior_strength: prior_strength.max(1.0),
            ewma_alpha: ewma_alpha.clamp(0.001, 1.0),
            blend_lambda: blend_lambda.clamp(0.0, 1.0),
            window_trades: window_trades.max(10),
            mult_min: mult_min.clamp(0.01, 10.0),
            mult_max: mult_max.clamp(mult_min.max(0.01), 20.0),
            fallback_base_pct: fallback_base_pct.clamp(0.0, 1.0),
            p0_map,
        }
    }

    pub fn factor(&self, timeframe: Timeframe) -> f64 {
        match timeframe {
            Timeframe::M5 => self.factor_5m,
            Timeframe::M15 => self.factor_15m,
            Timeframe::H1 => self.factor_1h,
            Timeframe::H4 | Timeframe::D1 => self.factor_4h,
        }
    }

    pub fn lane_key(symbol: &str, timeframe: Timeframe) -> String {
        format!(
            "{}:{}",
            normalize_symbol(symbol),
            timeframe.as_str().to_ascii_lowercase()
        )
    }

    pub fn p0_for_lane(&self, symbol: &str, timeframe: Timeframe) -> f64 {
        let key = Self::lane_key(symbol, timeframe);
        self.p0_map
            .get(key.as_str())
            .copied()
            .unwrap_or(self.p0_default)
            .clamp(0.01, 0.99)
    }
}

pub fn compute_kelly_adjustment(
    cfg: &KellySizingConfig,
    timeframe: Timeframe,
    symbol: &str,
    entry_price: f64,
    original_target_size_usd: f64,
    outcomes_desc: &[f64],
    cumulative_settled_pnl_usd: f64,
) -> Option<KellyAdjustment> {
    if !cfg.enable || original_target_size_usd <= 0.0 {
        return None;
    }
    let entry_price = entry_price.clamp(0.01, 0.99);
    let lane_key = KellySizingConfig::lane_key(symbol, timeframe);
    let p0 = cfg.p0_for_lane(symbol, timeframe);
    let outcomes_len = outcomes_desc.len();
    let wins = outcomes_desc.iter().filter(|pnl| **pnl > 0.0).count() as u64;
    let losses = outcomes_len.saturating_sub(wins as usize) as u64;

    let alpha0 = p0 * cfg.prior_strength;
    let beta0 = (1.0 - p0) * cfg.prior_strength;
    let p_beta =
        ((alpha0 + wins as f64) / (alpha0 + beta0 + outcomes_len as f64)).clamp(0.01, 0.99);

    let mut p_ewma = p0;
    for pnl in outcomes_desc.iter().rev() {
        let outcome = if *pnl > 0.0 { 1.0 } else { 0.0 };
        p_ewma = (cfg.ewma_alpha * outcome) + ((1.0 - cfg.ewma_alpha) * p_ewma);
    }
    p_ewma = p_ewma.clamp(0.01, 0.99);
    let p_blend =
        ((cfg.blend_lambda * p_beta) + ((1.0 - cfg.blend_lambda) * p_ewma)).clamp(0.01, 0.99);

    let bankroll_usd = (cfg.seed_usd + cumulative_settled_pnl_usd).max(1.0);
    let b = ((1.0 - entry_price) / entry_price).max(0.0);
    if b <= 0.0 {
        return None;
    }
    let f_star = (((b * p_blend) - (1.0 - p_blend)) / b).clamp(-1.0, 1.0);
    let kelly_factor = cfg.factor(timeframe).clamp(0.0, 1.0);
    let fallback_used = f_star <= 0.0;
    let (f_used, kelly_target_size_usd, size_multiplier, final_target_size_usd) = if fallback_used {
        let final_size = (original_target_size_usd * cfg.fallback_base_pct).max(0.01);
        (
            0.0,
            final_size,
            (final_size / original_target_size_usd).clamp(0.01, 100.0),
            final_size,
        )
    } else {
        let f_used = (kelly_factor * f_star).max(0.0);
        let kelly_target_size_usd = (bankroll_usd * f_used).max(0.01);
        let size_multiplier =
            (kelly_target_size_usd / original_target_size_usd).clamp(cfg.mult_min, cfg.mult_max);
        let final_target_size_usd = (original_target_size_usd * size_multiplier).max(0.01);
        (
            f_used,
            kelly_target_size_usd,
            size_multiplier,
            final_target_size_usd,
        )
    };

    Some(KellyAdjustment {
        lane_key,
        p0,
        p_beta,
        p_ewma,
        p_blend,
        wins,
        losses,
        samples: outcomes_len as u64,
        bankroll_usd,
        entry_price,
        kelly_factor,
        f_star,
        f_used,
        original_target_size_usd,
        kelly_target_size_usd,
        final_target_size_usd,
        size_multiplier,
        fallback_used,
    })
}

fn normalize_symbol(symbol: &str) -> String {
    match symbol.trim().to_ascii_uppercase().as_str() {
        "SOLANA" => "SOL".to_string(),
        other => other.to_string(),
    }
}

fn normalize_timeframe_label(raw: &str) -> Option<String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "5m" | "m5" => Some("5m".to_string()),
        "15m" | "m15" => Some("15m".to_string()),
        "1h" | "h1" | "60m" => Some("1h".to_string()),
        "4h" | "h4" | "240m" => Some("4h".to_string()),
        "1d" | "d1" | "24h" | "daily" => Some("1d".to_string()),
        _ => None,
    }
}

fn parse_kelly_p0_map(raw: &str) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    for piece in raw.split(',') {
        let trimmed = piece.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.splitn(2, '=');
        let lane_raw = parts.next().unwrap_or_default().trim();
        let value_raw = parts.next().unwrap_or_default().trim();
        if lane_raw.is_empty() || value_raw.is_empty() {
            continue;
        }
        let parsed_value = match value_raw.parse::<f64>() {
            Ok(v) if v.is_finite() => v.clamp(0.01, 0.99),
            _ => continue,
        };
        let (symbol_raw, tf_raw) = match lane_raw.split_once(':') {
            Some(v) => v,
            None => continue,
        };
        let symbol = normalize_symbol(symbol_raw);
        if symbol.is_empty() {
            continue;
        }
        let Some(tf) = normalize_timeframe_label(tf_raw) else {
            continue;
        };
        out.insert(format!("{}:{}", symbol, tf), parsed_value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_map_normalizes_lane_keys() {
        let cfg = KellySizingConfig::new(
            true,
            0.1,
            0.15,
            0.2,
            0.3,
            0.75,
            "btc:15m=0.8,ETH:h1=0.62,SOLANA:4h=0.55",
            1_000.0,
            40.0,
            0.1,
            0.7,
            200,
            0.25,
            2.5,
            0.5,
        );
        assert!((cfg.p0_for_lane("BTC", Timeframe::M15) - 0.8).abs() < 1e-9);
        assert!((cfg.p0_for_lane("ETH", Timeframe::H1) - 0.62).abs() < 1e-9);
        assert!((cfg.p0_for_lane("SOL", Timeframe::H4) - 0.55).abs() < 1e-9);
    }

    #[test]
    fn fallback_applies_when_f_star_non_positive() {
        let cfg = KellySizingConfig::new(
            true, 0.1, 0.15, 0.2, 0.3, 0.75, "", 1_000.0, 40.0, 0.1, 0.7, 200, 0.25, 2.5, 0.5,
        );
        let outcomes = vec![-1.0; 20];
        let adj = compute_kelly_adjustment(
            &cfg,
            Timeframe::M15,
            "BTC",
            0.99,
            250.0,
            outcomes.as_slice(),
            -20.0,
        )
        .expect("adjustment");
        assert!(adj.fallback_used);
        assert!((adj.final_target_size_usd - 125.0).abs() < 1e-9);
    }
}
