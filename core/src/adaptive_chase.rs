use crate::strategy::Timeframe;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxySource {
    Coinbase,
    Binance,
}

impl ProxySource {
    pub fn as_str(self) -> &'static str {
        match self {
            ProxySource::Coinbase => "coinbase",
            ProxySource::Binance => "binance",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChaseHealthState {
    Healthy,
    PausedStale,
    Reconnecting,
    CutoffReached,
    HardStopped,
}

impl ChaseHealthState {
    pub fn as_str(self) -> &'static str {
        match self {
            ChaseHealthState::Healthy => "healthy",
            ChaseHealthState::PausedStale => "paused_stale",
            ChaseHealthState::Reconnecting => "reconnecting",
            ChaseHealthState::CutoffReached => "cutoff_reached",
            ChaseHealthState::HardStopped => "hard_stopped",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AdaptiveChaseConfig {
    pub kappa_5m: f64,
    pub kappa_15m: f64,
    pub kappa_1h: f64,
    pub kappa_4h: f64,
    pub edge_buffer: f64,
    pub vol_a: f64,
    pub vol_b: f64,
    pub min_replace_interval_ms: i64,
    pub min_price_improve_ticks: u32,
    pub fallback_tick_ms: i64,
    pub max_replaces_per_sec: u32,
    pub healthy_resume_ms: i64,
    pub stale_buffer_mult: f64,
    pub expiry_buffer_max: f64,
    pub max_replaces_total: u32,
    pub cutoff_guard_ms: i64,
    pub reanchor_interval_ms: i64,
    pub reanchor_proxy_move_bps: f64,
    pub reanchor_disloc_move_bps: f64,
    pub fallback_fail_limit: u32,
    pub duplicate_replace_suppress_ms: i64,
}

impl AdaptiveChaseConfig {
    pub fn from_env() -> Self {
        Self {
            kappa_5m: env_f64("EVPOLY_CHASE_KAPPA_5M", 1.6).clamp(0.0, 10.0),
            kappa_15m: env_f64("EVPOLY_CHASE_KAPPA_15M", 1.4).clamp(0.0, 10.0),
            kappa_1h: env_f64("EVPOLY_CHASE_KAPPA_1H", 1.0).clamp(0.0, 10.0),
            kappa_4h: env_f64("EVPOLY_CHASE_KAPPA_4H", 0.8).clamp(0.0, 10.0),
            edge_buffer: env_f64("EVPOLY_CHASE_EDGE_BUFFER", 0.003).clamp(0.0, 0.5),
            vol_a: env_f64("EVPOLY_CHASE_VOL_A", 0.4).clamp(0.0, 10.0),
            vol_b: env_f64("EVPOLY_CHASE_VOL_B", 0.6).clamp(0.0, 10.0),
            min_replace_interval_ms: env_i64("EVPOLY_CHASE_MIN_REPLACE_MS", 120).clamp(10, 5_000),
            min_price_improve_ticks: env_u32("EVPOLY_CHASE_MIN_PRICE_IMPROVE_TICKS", 1).max(1),
            fallback_tick_ms: env_i64("EVPOLY_CHASE_FALLBACK_TICK_MS", 200).clamp(20, 5_000),
            max_replaces_per_sec: env_u32("EVPOLY_CHASE_MAX_REPLACES_PER_SEC", 6).max(1),
            healthy_resume_ms: env_i64("EVPOLY_CHASE_HEALTHY_RESUME_MS", 500).clamp(50, 30_000),
            stale_buffer_mult: env_f64("EVPOLY_CHASE_STALE_BUFFER_MULT", 0.002).clamp(0.0, 0.2),
            expiry_buffer_max: env_f64("EVPOLY_CHASE_EXPIRY_BUFFER_MAX", 0.004).clamp(0.0, 0.5),
            max_replaces_total: env_u32("EVPOLY_CHASE_MAX_REPLACES_TOTAL", 30).max(1),
            cutoff_guard_ms: env_i64("EVPOLY_CHASE_CUTOFF_GUARD_MS", 800).clamp(0, 10_000),
            reanchor_interval_ms: env_i64("EVPOLY_CHASE_REANCHOR_INTERVAL_MS", 2_000)
                .clamp(100, 60_000),
            reanchor_proxy_move_bps: env_f64("EVPOLY_CHASE_REANCHOR_PROXY_BPS", 8.0)
                .clamp(0.0, 500.0),
            reanchor_disloc_move_bps: env_f64("EVPOLY_CHASE_REANCHOR_DISLOC_BPS", 10.0)
                .clamp(0.0, 500.0),
            fallback_fail_limit: env_u32("EVPOLY_CHASE_FALLBACK_FAIL_LIMIT", 3).max(1),
            duplicate_replace_suppress_ms: env_i64(
                "EVPOLY_CHASE_DUPLICATE_REPLACE_SUPPRESS_MS",
                250,
            )
            .clamp(0, 5_000),
        }
    }

    pub fn kappa_for_timeframe(self, timeframe: Timeframe) -> f64 {
        match timeframe {
            Timeframe::M5 => self.kappa_5m,
            Timeframe::M15 => self.kappa_15m,
            Timeframe::H1 => self.kappa_1h,
            Timeframe::H4 => self.kappa_4h,
            Timeframe::D1 => self.kappa_4h,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LiveFairInput {
    pub side_sign: f64,
    pub proxy_mid_ref: f64,
    pub proxy_mid_now: f64,
    pub pm_mid_ref: f64,
    pub pm_mid_now: f64,
    pub max_buy_curve_live: f64,
    pub edge_buffer_base: f64,
    pub proxy_return_1s_bps: f64,
    pub realized_vol_short: f64,
    pub vol_a: f64,
    pub vol_b: f64,
    pub proxy_age_ms: i64,
    pub pm_age_ms: i64,
    pub proxy_stale_ms: i64,
    pub pm_stale_ms: i64,
    pub stale_buffer_mult: f64,
    pub remaining_sec: i64,
    pub phase_total_sec: i64,
    pub expiry_buffer_max: f64,
    pub min_buy_price: f64,
    pub kappa_tf: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct LiveFairState {
    pub d_proxy_bps: f64,
    pub d_pm_bps: f64,
    pub disloc_bps: f64,
    pub fair_proxy_live: f64,
    pub max_buy_curve_live: f64,
    pub edge_buffer_base: f64,
    pub vol_buffer: f64,
    pub stale_buffer: f64,
    pub expiry_buffer: f64,
    pub max_buy_live_raw: f64,
    pub max_buy_live_pre_clamp: f64,
    pub max_buy_live: f64,
    pub hard_stop: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceAction {
    Noop,
    Place,
    Replace,
}

#[derive(Debug, Clone, Copy)]
pub struct ReplacePolicyInput {
    pub working_price: Option<f64>,
    pub target_bid: f64,
    pub max_buy_live: f64,
    pub now_ms: i64,
    pub last_replace_ms: i64,
    pub min_replace_interval_ms: i64,
    pub min_price_improve_ticks: u32,
    pub tick_size: f64,
    pub replaces_this_sec: u32,
    pub max_replaces_per_sec: u32,
}

pub fn decide_replace_action(input: ReplacePolicyInput) -> ReplaceAction {
    if input.replaces_this_sec >= input.max_replaces_per_sec {
        return ReplaceAction::Noop;
    }
    if input.now_ms.saturating_sub(input.last_replace_ms) < input.min_replace_interval_ms {
        return ReplaceAction::Noop;
    }
    let min_delta = f64::from(input.min_price_improve_ticks.max(1)) * input.tick_size.max(0.0);
    match input.working_price {
        None => ReplaceAction::Place,
        Some(working) => {
            if working > input.max_buy_live + 1e-9 {
                return ReplaceAction::Replace;
            }
            if (input.target_bid - working).abs() >= min_delta {
                return ReplaceAction::Replace;
            }
            ReplaceAction::Noop
        }
    }
}

pub fn remaining_notional(target_notional: f64, filled_notional: f64) -> f64 {
    (target_notional - filled_notional).max(0.0)
}

pub fn should_reanchor(
    now_ms: i64,
    anchor_ms: i64,
    d_proxy_bps: f64,
    disloc_bps: f64,
    anchor_disloc_bps: f64,
    cfg: AdaptiveChaseConfig,
) -> bool {
    if now_ms.saturating_sub(anchor_ms) >= cfg.reanchor_interval_ms {
        return true;
    }
    if d_proxy_bps.abs() >= cfg.reanchor_proxy_move_bps {
        return true;
    }
    (disloc_bps - anchor_disloc_bps).abs() >= cfg.reanchor_disloc_move_bps
}

pub fn cutoff_reached(now_ms: i64, close_ts: i64, cutoff_guard_ms: i64) -> bool {
    let close_ms = close_ts.saturating_mul(1_000);
    now_ms >= close_ms.saturating_sub(cutoff_guard_ms.max(0))
}

pub fn compute_live_fair_state(input: LiveFairInput) -> LiveFairState {
    let safe_proxy_ref = input.proxy_mid_ref.max(f64::EPSILON);
    let safe_pm_ref = input.pm_mid_ref.max(f64::EPSILON);

    let d_proxy_bps = input.side_sign * ((input.proxy_mid_now / safe_proxy_ref) - 1.0) * 1e4;
    let d_pm_bps = input.side_sign * ((input.pm_mid_now / safe_pm_ref) - 1.0) * 1e4;
    let disloc_bps = d_proxy_bps - d_pm_bps;

    let fair_shift = input.kappa_tf * disloc_bps / 1e4;
    let fair_proxy_live = (input.pm_mid_now + fair_shift).clamp(0.01, 0.99);

    let vol_buffer = input.vol_a * input.proxy_return_1s_bps.abs() / 1e4
        + input.vol_b * input.realized_vol_short;

    let proxy_stale_ratio = if input.proxy_stale_ms > 0 {
        (input.proxy_age_ms as f64 / input.proxy_stale_ms as f64 - 1.0).max(0.0)
    } else {
        1.0
    };
    let pm_stale_ratio = if input.pm_stale_ms > 0 {
        (input.pm_age_ms as f64 / input.pm_stale_ms as f64 - 1.0).max(0.0)
    } else {
        1.0
    };
    let stale_buffer = (proxy_stale_ratio + pm_stale_ratio).max(0.0) * input.stale_buffer_mult;

    let expiry_progress = if input.phase_total_sec > 0 {
        let rem = input.remaining_sec.clamp(0, input.phase_total_sec) as f64;
        1.0 - rem / input.phase_total_sec as f64
    } else {
        1.0
    };
    let expiry_buffer = expiry_progress.clamp(0.0, 1.0) * input.expiry_buffer_max;

    let max_buy_live_raw = input.max_buy_curve_live.min(fair_proxy_live);
    let max_buy_live_pre_clamp =
        max_buy_live_raw - input.edge_buffer_base - vol_buffer - stale_buffer - expiry_buffer;
    let hard_stop = max_buy_live_pre_clamp < input.min_buy_price;
    let max_buy_live = max_buy_live_pre_clamp.clamp(input.min_buy_price, 0.99);

    LiveFairState {
        d_proxy_bps,
        d_pm_bps,
        disloc_bps,
        fair_proxy_live,
        max_buy_curve_live: input.max_buy_curve_live,
        edge_buffer_base: input.edge_buffer_base,
        vol_buffer,
        stale_buffer,
        expiry_buffer,
        max_buy_live_raw,
        max_buy_live_pre_clamp,
        max_buy_live,
        hard_stop,
    }
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(default)
}

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::{
        compute_live_fair_state, cutoff_reached, decide_replace_action, remaining_notional,
        should_reanchor, AdaptiveChaseConfig, LiveFairInput, ReplaceAction, ReplacePolicyInput,
    };

    fn base_input() -> LiveFairInput {
        LiveFairInput {
            side_sign: 1.0,
            proxy_mid_ref: 100.0,
            proxy_mid_now: 100.0,
            pm_mid_ref: 0.70,
            pm_mid_now: 0.70,
            max_buy_curve_live: 0.80,
            edge_buffer_base: 0.01,
            proxy_return_1s_bps: 0.0,
            realized_vol_short: 0.0,
            vol_a: 0.5,
            vol_b: 0.5,
            proxy_age_ms: 10,
            pm_age_ms: 10,
            proxy_stale_ms: 1000,
            pm_stale_ms: 1000,
            stale_buffer_mult: 0.002,
            remaining_sec: 40,
            phase_total_sec: 60,
            expiry_buffer_max: 0.004,
            min_buy_price: 0.60,
            kappa_tf: 1.5,
        }
    }

    #[test]
    fn dislocation_up_side_increases_fair_when_proxy_up_pm_flat() {
        let mut input = base_input();
        input.proxy_mid_now = 100.5;
        input.pm_mid_now = 0.70;
        let out = compute_live_fair_state(input);
        assert!(out.fair_proxy_live > 0.70);
        assert!(out.d_proxy_bps > 0.0);
        assert!(out.disloc_bps > 0.0);
    }

    #[test]
    fn dislocation_up_side_decreases_fair_when_proxy_down_pm_flat() {
        let mut input = base_input();
        input.proxy_mid_now = 99.5;
        input.pm_mid_now = 0.70;
        let out = compute_live_fair_state(input);
        assert!(out.fair_proxy_live < 0.70);
        assert!(out.d_proxy_bps < 0.0);
        assert!(out.disloc_bps < 0.0);
    }

    #[test]
    fn side_sign_for_hold_down_inverts_proxy_move_effect() {
        let mut input = base_input();
        input.side_sign = -1.0;
        input.proxy_mid_now = 99.5;
        let out = compute_live_fair_state(input);
        assert!(out.disloc_bps > 0.0);
        assert!(out.fair_proxy_live > 0.70);
    }

    #[test]
    fn higher_proxy_return_and_realized_vol_raise_vol_buffer() {
        let input1 = base_input();
        let mut input2 = base_input();
        input2.proxy_return_1s_bps = 25.0;
        input2.realized_vol_short = 0.01;
        let out1 = compute_live_fair_state(input1);
        let out2 = compute_live_fair_state(input2);
        assert!(out2.vol_buffer > out1.vol_buffer);
    }

    #[test]
    fn stale_age_increases_stale_buffer() {
        let input1 = base_input();
        let mut input2 = base_input();
        input2.proxy_age_ms = 2500;
        input2.pm_age_ms = 2200;
        let out1 = compute_live_fair_state(input1);
        let out2 = compute_live_fair_state(input2);
        assert!(out2.stale_buffer > out1.stale_buffer);
    }

    #[test]
    fn later_expiry_increases_expiry_buffer() {
        let mut input1 = base_input();
        input1.remaining_sec = 50;
        let mut input2 = base_input();
        input2.remaining_sec = 5;
        let out1 = compute_live_fair_state(input1);
        let out2 = compute_live_fair_state(input2);
        assert!(out2.expiry_buffer > out1.expiry_buffer);
    }

    #[test]
    fn hard_stop_when_live_cap_below_min_buy() {
        let mut input = base_input();
        input.max_buy_curve_live = 0.62;
        input.edge_buffer_base = 0.03;
        input.min_buy_price = 0.60;
        let out = compute_live_fair_state(input);
        assert!(out.hard_stop);
        assert!(out.max_buy_live_pre_clamp < 0.60);
    }

    #[test]
    fn replace_when_working_above_max_buy_live() {
        let action = decide_replace_action(ReplacePolicyInput {
            working_price: Some(0.75),
            target_bid: 0.70,
            max_buy_live: 0.72,
            now_ms: 1_000,
            last_replace_ms: 700,
            min_replace_interval_ms: 100,
            min_price_improve_ticks: 1,
            tick_size: 0.01,
            replaces_this_sec: 0,
            max_replaces_per_sec: 5,
        });
        assert_eq!(action, ReplaceAction::Replace);
    }

    #[test]
    fn no_replace_when_price_shift_under_min_tick_delta() {
        let action = decide_replace_action(ReplacePolicyInput {
            working_price: Some(0.70),
            target_bid: 0.705,
            max_buy_live: 0.80,
            now_ms: 1_000,
            last_replace_ms: 700,
            min_replace_interval_ms: 100,
            min_price_improve_ticks: 1,
            tick_size: 0.01,
            replaces_this_sec: 0,
            max_replaces_per_sec: 5,
        });
        assert_eq!(action, ReplaceAction::Noop);
    }

    #[test]
    fn replace_blocked_by_min_interval() {
        let action = decide_replace_action(ReplacePolicyInput {
            working_price: Some(0.75),
            target_bid: 0.70,
            max_buy_live: 0.72,
            now_ms: 1_000,
            last_replace_ms: 950,
            min_replace_interval_ms: 100,
            min_price_improve_ticks: 1,
            tick_size: 0.01,
            replaces_this_sec: 0,
            max_replaces_per_sec: 5,
        });
        assert_eq!(action, ReplaceAction::Noop);
    }

    #[test]
    fn replace_blocked_by_circuit_breaker() {
        let action = decide_replace_action(ReplacePolicyInput {
            working_price: Some(0.75),
            target_bid: 0.70,
            max_buy_live: 0.72,
            now_ms: 1_000,
            last_replace_ms: 700,
            min_replace_interval_ms: 100,
            min_price_improve_ticks: 1,
            tick_size: 0.01,
            replaces_this_sec: 5,
            max_replaces_per_sec: 5,
        });
        assert_eq!(action, ReplaceAction::Noop);
    }

    #[test]
    fn partial_fill_only_reprices_remainder() {
        assert!((remaining_notional(500.0, 125.0) - 375.0).abs() < 1e-9);
        assert_eq!(remaining_notional(500.0, 600.0), 0.0);
    }

    #[test]
    fn reanchor_on_interval_or_large_move() {
        let cfg = AdaptiveChaseConfig {
            kappa_5m: 1.0,
            kappa_15m: 1.0,
            kappa_1h: 1.0,
            kappa_4h: 1.0,
            edge_buffer: 0.0,
            vol_a: 0.0,
            vol_b: 0.0,
            min_replace_interval_ms: 100,
            min_price_improve_ticks: 1,
            fallback_tick_ms: 200,
            max_replaces_per_sec: 5,
            healthy_resume_ms: 500,
            stale_buffer_mult: 0.0,
            expiry_buffer_max: 0.0,
            max_replaces_total: 30,
            cutoff_guard_ms: 800,
            reanchor_interval_ms: 2_000,
            reanchor_proxy_move_bps: 8.0,
            reanchor_disloc_move_bps: 10.0,
            fallback_fail_limit: 3,
            duplicate_replace_suppress_ms: 250,
        };
        assert!(should_reanchor(3_000, 0, 0.0, 0.0, 0.0, cfg));
        assert!(should_reanchor(100, 0, 20.0, 1.0, 0.0, cfg));
        assert!(should_reanchor(100, 0, 0.5, 30.0, 0.0, cfg));
    }

    #[test]
    fn cutoff_guard_in_ms() {
        assert!(!cutoff_reached(999_000, 1_000, 800));
        assert!(cutoff_reached(999_201, 1_000, 800));
    }
}
