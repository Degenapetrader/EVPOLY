pub fn reservation_price(fair_probability: f64, buffer_bps: f64) -> f64 {
    let buffer_prob = (buffer_bps.max(0.0)) / 10_000.0;
    (fair_probability - buffer_prob).clamp(0.01, 0.99)
}

pub fn max_price_with_ev_floor(reservation_price: f64, ev_floor_bps: f64) -> f64 {
    let ev_floor_prob = ev_floor_bps.max(0.0) / 10_000.0;
    (reservation_price - ev_floor_prob).clamp(0.01, reservation_price.clamp(0.01, 0.99))
}

pub fn edge_bps_at_price(
    fair_probability: f64,
    execution_price: f64,
    total_buffer_bps: f64,
) -> f64 {
    ((fair_probability - execution_price) * 10_000.0 - total_buffer_bps).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservation_and_max_price_are_bounded() {
        let reservation = reservation_price(0.62, 45.0);
        let max_price = max_price_with_ev_floor(reservation, 20.0);
        assert!((0.01..=0.99).contains(&reservation));
        assert!((0.01..=reservation).contains(&max_price));
    }

    #[test]
    fn edge_bps_respects_buffer() {
        let edge = edge_bps_at_price(0.62, 0.60, 10.0);
        assert!((189.0..=191.0).contains(&edge));
    }
}
