#[derive(Debug, Clone, Copy, Default)]
pub struct LocalScoreCheck {
    pub midpoint_valid: bool,
    pub spread_cap: f64,
    pub up_spread_ok: bool,
    pub down_spread_ok: bool,
    pub up_size_ok: bool,
    pub down_size_ok: bool,
    pub up_min_size_shares: f64,
    pub down_min_size_shares: f64,
    pub up_scoring: bool,
    pub down_scoring: bool,
    pub q_min_ready: bool,
}

pub fn check_local_scoring(
    midpoint: Option<f64>,
    max_spread: f64,
    up_min_size_shares: f64,
    down_min_size_shares: f64,
    up_price: f64,
    down_price: f64,
    up_size_shares: f64,
    down_size_shares: f64,
    up_anchor_price: Option<f64>,
    down_anchor_price: Option<f64>,
) -> LocalScoreCheck {
    let spread_cap = if max_spread.is_finite() {
        if max_spread > 1.0 {
            (max_spread / 100.0).max(0.0)
        } else {
            max_spread.max(0.0)
        }
    } else {
        0.0
    };
    let up_min_size = if up_min_size_shares.is_finite() {
        up_min_size_shares.max(0.0)
    } else {
        f64::MAX
    };
    let down_min_size = if down_min_size_shares.is_finite() {
        down_min_size_shares.max(0.0)
    } else {
        f64::MAX
    };

    let Some(mid) = midpoint else {
        return LocalScoreCheck {
            spread_cap,
            up_min_size_shares: up_min_size,
            down_min_size_shares: down_min_size,
            ..LocalScoreCheck::default()
        };
    };
    if !mid.is_finite() || !(0.0..=1.0).contains(&mid) {
        return LocalScoreCheck {
            spread_cap,
            up_min_size_shares: up_min_size,
            down_min_size_shares: down_min_size,
            ..LocalScoreCheck::default()
        };
    }

    let up_price_valid = up_price.is_finite() && (0.0..=1.0).contains(&up_price);
    let down_price_valid = down_price.is_finite() && (0.0..=1.0).contains(&down_price);
    let down_yes_equiv = if down_price_valid {
        Some((1.0 - down_price).clamp(0.0, 1.0))
    } else {
        None
    };
    let eps = 1e-9;
    let within_anchor = |price: f64, anchor: Option<f64>| -> Option<bool> {
        anchor
            .filter(|v| v.is_finite() && *v > 0.0 && *v < 1.0)
            .map(|value| {
                let min_scoreable_price = (value - spread_cap).max(0.01);
                let max_scoreable_price = value.min(0.99);
                price >= (min_scoreable_price - eps) && price <= (max_scoreable_price + eps)
            })
    };
    let up_spread_ok = if up_price_valid {
        within_anchor(up_price, up_anchor_price)
            .unwrap_or_else(|| (mid - up_price).abs() <= (spread_cap + eps))
    } else {
        false
    };
    let down_spread_ok = if down_price_valid {
        within_anchor(down_price, down_anchor_price).unwrap_or_else(|| {
            down_yes_equiv
                .map(|yes_equiv| (mid - yes_equiv).abs() <= (spread_cap + eps))
                .unwrap_or(false)
        })
    } else {
        false
    };
    let up_size_ok = up_size_shares.is_finite() && up_size_shares >= up_min_size;
    let down_size_ok = down_size_shares.is_finite() && down_size_shares >= down_min_size;
    let up_ok = up_spread_ok && up_size_ok;
    let down_ok = down_spread_ok && down_size_ok;

    LocalScoreCheck {
        midpoint_valid: true,
        spread_cap,
        up_spread_ok,
        down_spread_ok,
        up_size_ok,
        down_size_ok,
        up_min_size_shares: up_min_size,
        down_min_size_shares: down_min_size,
        up_scoring: up_ok,
        down_scoring: down_ok,
        q_min_ready: up_ok && down_ok,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoring_passes_with_valid_inputs() {
        let out = check_local_scoring(
            Some(0.50),
            0.03,
            100.0,
            100.0,
            0.49,
            0.51,
            120.0,
            130.0,
            Some(0.50),
            Some(0.52),
        );
        assert!(out.midpoint_valid);
        assert!(out.up_scoring);
        assert!(out.down_scoring);
        assert!(out.q_min_ready);
    }

    #[test]
    fn scoring_fails_on_invalid_prices_and_sizes() {
        let out = check_local_scoring(
            Some(0.5),
            0.03,
            10.0,
            10.0,
            f64::NAN,
            2.0,
            -1.0,
            5.0,
            None,
            None,
        );
        assert!(out.midpoint_valid);
        assert!(!out.up_spread_ok);
        assert!(!out.down_spread_ok);
        assert!(!out.up_size_ok);
        assert!(!out.down_size_ok);
        assert!(!out.q_min_ready);
    }

    #[test]
    fn scoring_fails_when_midpoint_missing() {
        let out = check_local_scoring(None, 3.0, 10.0, 10.0, 0.49, 0.51, 10.0, 10.0, None, None);
        assert!(!out.midpoint_valid);
        assert!(!out.q_min_ready);
        assert!(!out.up_scoring);
        assert!(!out.down_scoring);
    }

    #[test]
    fn scoring_uses_same_side_anchor_when_present() {
        let out = check_local_scoring(
            Some(0.10),
            0.05,
            20.0,
            20.0,
            0.84,
            0.89,
            25.0,
            25.0,
            Some(0.86),
            Some(0.89),
        );
        assert!(out.up_scoring);
        assert!(out.down_scoring);
        assert!(out.q_min_ready);
    }
}
