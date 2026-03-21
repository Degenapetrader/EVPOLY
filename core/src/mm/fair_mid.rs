use crate::mm::metrics::BboSnapshot;

#[derive(Debug, Clone, Copy)]
pub struct MmFairMidConfig {
    pub enable: bool,
    pub min_price: f64,
    pub max_price: f64,
    pub tick_size: f64,
    pub up_step_bps: f64,
    pub down_step_bps: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct MmFairMidState {
    pub value: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct MmFairMidOutput {
    pub raw: f64,
    pub limited: f64,
    pub limited_applied: bool,
    pub up_cap_abs: f64,
    pub down_cap_abs: f64,
}

pub fn side_reference_price(
    bid: Option<f64>,
    ask: Option<f64>,
    bid_size: Option<f64>,
    ask_size: Option<f64>,
) -> Option<f64> {
    BboSnapshot {
        ts_ms: 0,
        bid,
        ask,
        bid_size,
        ask_size,
    }
    .reference_mid()
}

pub fn combined_yes_mid(
    up_reference_price: Option<f64>,
    down_reference_price: Option<f64>,
    fallback_mid: Option<f64>,
) -> Option<f64> {
    let mid = match (up_reference_price, down_reference_price) {
        (Some(up_ref), Some(down_ref)) => Some(((up_ref + (1.0 - down_ref)) * 0.5).clamp(0.0, 1.0)),
        (Some(up_ref), None) => Some(up_ref.clamp(0.0, 1.0)),
        (None, Some(down_ref)) => Some((1.0 - down_ref).clamp(0.0, 1.0)),
        (None, None) => fallback_mid,
    }?;
    (mid.is_finite() && (0.0..=1.0).contains(&mid)).then_some(mid)
}

pub fn apply_asymmetric_speed_limit(
    prev: Option<MmFairMidState>,
    raw: f64,
    cfg: MmFairMidConfig,
) -> MmFairMidOutput {
    let raw_clamped = raw.clamp(cfg.min_price, cfg.max_price);
    if !cfg.enable {
        return MmFairMidOutput {
            raw: raw_clamped,
            limited: raw_clamped,
            limited_applied: false,
            up_cap_abs: 0.0,
            down_cap_abs: 0.0,
        };
    }

    let tick = cfg.tick_size.max(0.001);
    let Some(prev_state) = prev else {
        return MmFairMidOutput {
            raw: raw_clamped,
            limited: raw_clamped,
            limited_applied: false,
            up_cap_abs: 0.0,
            down_cap_abs: 0.0,
        };
    };

    let prev_value = prev_state.value.clamp(cfg.min_price, cfg.max_price);
    let up_cap_abs = (prev_value * (cfg.up_step_bps / 10_000.0)).max(tick);
    let down_cap_abs = (prev_value * (cfg.down_step_bps / 10_000.0)).max(tick);

    let limited = if raw_clamped > prev_value {
        raw_clamped.min(prev_value + up_cap_abs)
    } else if raw_clamped < prev_value {
        raw_clamped.max(prev_value - down_cap_abs)
    } else {
        raw_clamped
    }
    .clamp(cfg.min_price, cfg.max_price);

    MmFairMidOutput {
        raw: raw_clamped,
        limited,
        limited_applied: (limited - raw_clamped).abs() > (tick * 0.5),
        up_cap_abs,
        down_cap_abs,
    }
}

pub fn next_state(limited: f64) -> MmFairMidState {
    MmFairMidState { value: limited }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asymmetric_speed_limit_slows_up_moves() {
        let cfg = MmFairMidConfig {
            enable: true,
            min_price: 0.01,
            max_price: 0.99,
            tick_size: 0.01,
            up_step_bps: 20.0,
            down_step_bps: 120.0,
        };
        let prev = Some(MmFairMidState { value: 0.50 });
        let out = apply_asymmetric_speed_limit(prev, 0.80, cfg);
        assert!(out.limited < 0.80);
        assert!(out.limited >= 0.50);
    }

    #[test]
    fn asymmetric_speed_limit_allows_faster_down_moves() {
        let cfg = MmFairMidConfig {
            enable: true,
            min_price: 0.01,
            max_price: 0.99,
            tick_size: 0.01,
            up_step_bps: 20.0,
            down_step_bps: 120.0,
        };
        let prev = Some(MmFairMidState { value: 0.50 });
        let out = apply_asymmetric_speed_limit(prev, 0.10, cfg);
        assert!(out.limited <= 0.49);
    }

    #[test]
    fn combined_yes_mid_prefers_up_down_pair() {
        let out = combined_yes_mid(Some(0.70), Some(0.20), Some(0.40)).unwrap();
        assert!((out - 0.75).abs() < 1e-9);
    }
}
