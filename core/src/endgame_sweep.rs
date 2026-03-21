use crate::coinbase_ws::{CoinbaseBookLevel, CoinbaseBookState, CoinbaseFeedState};
use crate::config::EndgameExecutionConfig;
use crate::signal_state::SignalState;
use crate::size_policy;
use crate::strategy::{Direction, Timeframe, STRATEGY_ID_ENDGAME_SWEEP_V1};

#[derive(Debug, Clone)]
pub struct EndgameIntentPlan {
    pub strategy_id: String,
    pub timeframe: Timeframe,
    pub market_open_ts: i64,
    pub tick_index: u32,
    pub tau_seconds: u64,
    pub p_up: f64,
    pub p_down: f64,
    pub z_score: f64,
    pub sigma_tau: f64,
    pub spread_bps: f64,
    pub distance_to_base_bps: f64,
    pub opposing_depth_usd: f64,
    pub depth_ratio: f64,
    pub depth_sigma_mult: f64,
    pub thin_flip_guard_triggered: bool,
    pub thin_flip_guard_action: ThinFlipGuardAction,
    pub thin_flip_guard_reason: String,
    pub thin_flip_near_threshold_bps: f64,
    pub thin_flip_min_distance_required_bps: f64,
    pub thin_flip_impact_usd: f64,
    pub thin_flip_impact_ratio: f64,
    pub thin_flip_impact_ratio_threshold: f64,
    pub thin_flip_extra_buffer_bps: f64,
    pub thin_flip_size_multiplier: f64,
    pub thin_flip_disabled_last_tick: bool,
    pub uncertainty_penalty: f64,
    pub buffer_prob: f64,
    pub edge_floor_prob: f64,
    pub bias_multiplier: f64,
    pub direction: Direction,
    pub score: f64,
    pub base_mid_cb: f64,
    pub mid_cb: f64,
    pub spread_cb: f64,
    pub fair_probability: f64,
    pub execution_probability: f64,
    pub reservation_price: f64,
    pub max_price: f64,
    pub edge_bps: f64,
    pub target_size_usd: f64,
    pub bias_alignment: String,
    pub bias_confidence: f64,
    pub impulse_id: String,
}

#[derive(Debug, Clone)]
pub struct EndgameSidePricing {
    pub direction: Direction,
    pub fair_probability: f64,
    pub execution_probability: f64,
    pub fee_rate_execution: f64,
    pub reservation_price: f64,
    pub max_price: f64,
    pub fee_rate_max_price: f64,
    pub edge_bps: f64,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinFlipGuardAction {
    Pass,
    HardSkip,
}

impl ThinFlipGuardAction {
    pub fn as_str(self) -> &'static str {
        match self {
            ThinFlipGuardAction::Pass => "PASS",
            ThinFlipGuardAction::HardSkip => "HARD_SKIP",
        }
    }
}

#[derive(Debug, Clone)]
struct ThinFlipGuardDecision {
    action: ThinFlipGuardAction,
    reason: String,
    triggered: bool,
    near_threshold_bps: f64,
    min_distance_required_bps: f64,
    impact_usd: f64,
    impact_ratio: f64,
    impact_ratio_threshold: f64,
    extra_buffer_bps: f64,
    size_multiplier: f64,
    disabled_last_tick: bool,
}

impl EndgameIntentPlan {
    pub fn side_probability(&self, direction: Direction) -> f64 {
        match direction {
            Direction::Up => self.p_up,
            Direction::Down => self.p_down,
        }
    }

    pub fn side_pricing(&self, direction: Direction) -> EndgameSidePricing {
        side_pricing_from_probability(
            direction.clone(),
            self.side_probability(direction),
            self.uncertainty_penalty,
            self.buffer_prob,
            self.edge_floor_prob,
            self.bias_multiplier,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EndgameExecutionSizing {
    pub shares: f64,
    pub notional_usd: f64,
    pub vwap_price: f64,
    pub taker_fee_rate_at_vwap: f64,
    pub edge_bps_at_vwap: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BookAskLevel {
    pub price: f64,
    pub size: f64,
}

/// Polymarket crypto taker fee curve for binary outcome markets.
/// effective_rate = fee_rate * (p * (1-p))^exponent
/// with fee_rate=0.25 and exponent=2.
pub fn polymarket_taker_fee_rate(p: f64) -> f64 {
    let p = p.clamp(0.001, 0.999);
    let q = 1.0 - p;
    let pq = p * q;
    0.25 * pq * pq
}

pub fn timeframe_open_ts(now_ts: i64, timeframe: Timeframe) -> i64 {
    let duration = timeframe.duration_seconds().max(1);
    (now_ts.div_euclid(duration)) * duration
}

pub fn tick_index_for_tau(offsets: &[u64], tau_ms: u64, safety_stop_ms: u64) -> Option<u32> {
    if offsets.is_empty() || tau_ms <= safety_stop_ms {
        return None;
    }
    for (idx, offset) in offsets.iter().enumerate() {
        let lower = if idx + 1 < offsets.len() {
            offsets[idx + 1]
        } else {
            safety_stop_ms
        };
        if tau_ms <= *offset && tau_ms > lower {
            return u32::try_from(idx).ok();
        }
    }
    None
}

pub fn build_intent_plan(
    cfg: &EndgameExecutionConfig,
    asset_symbol: &str,
    timeframe: Timeframe,
    market_open_ts: i64,
    base_mid_cb: f64,
    signal: &SignalState,
    coinbase: &CoinbaseBookState,
    now_ms: i64,
) -> Option<EndgameIntentPlan> {
    build_intent_plan_for_tick(
        cfg,
        asset_symbol,
        timeframe,
        market_open_ts,
        base_mid_cb,
        signal,
        coinbase,
        now_ms,
        None,
    )
}

pub fn build_intent_plan_for_tick(
    cfg: &EndgameExecutionConfig,
    asset_symbol: &str,
    timeframe: Timeframe,
    market_open_ts: i64,
    base_mid_cb: f64,
    signal: &SignalState,
    coinbase: &CoinbaseBookState,
    now_ms: i64,
    forced_tick_index: Option<u32>,
) -> Option<EndgameIntentPlan> {
    if !cfg.enable || !cfg.timeframe_enabled(timeframe) {
        return None;
    }
    if !signal.has_any_fresh_source(now_ms, cfg.max_signal_age_ms) {
        return None;
    }
    if coinbase.feed_state != CoinbaseFeedState::Healthy {
        return None;
    }
    if coinbase.last_update_ms <= 0
        || now_ms.saturating_sub(coinbase.last_update_ms) > cfg.book_freshness_ms
    {
        return None;
    }

    let now_ts = now_ms.saturating_div(1_000);
    let close_ts = market_open_ts.saturating_add(timeframe.duration_seconds().max(1));
    if now_ts < market_open_ts || now_ts >= close_ts {
        return None;
    }
    let safety_stop_s = cfg.safety_stop_s;
    let latency_haircut = cfg.latency_haircut.clamp(0.0, 1.0);
    let edge_floor_bps = cfg.edge_floor_bps.clamp(0.0, 500.0);
    let tau_seconds = u64::try_from(close_ts.saturating_sub(now_ts)).ok()?;
    let close_ms = close_ts.saturating_mul(1_000);
    let tau_ms = u64::try_from(close_ms.saturating_sub(now_ms)).ok()?;
    let safety_stop_ms = safety_stop_s.saturating_mul(1_000);
    if tau_ms <= safety_stop_ms {
        return None;
    }
    let tick_index = if let Some(idx_u32) = forced_tick_index {
        let idx = usize::try_from(idx_u32).ok()?;
        cfg.tick_offsets_sec.get(idx).copied()?;
        idx_u32
    } else {
        let idx = tick_index_for_tau(cfg.tick_offsets_sec.as_slice(), tau_ms, safety_stop_ms)?;
        cfg.tick_offsets_sec
            .get(idx as usize)
            .copied()
            .unwrap_or_default();
        idx
    };
    let base_size_usd = size_policy::base_size_usd_from_env("EVPOLY_ENDGAME_BASE_SIZE_USD");
    let per_tick_notional_default = size_policy::endgame_tick_multiplier(tick_index).unwrap_or(0.0)
        * size_policy::strategy_symbol_size_usd(base_size_usd, asset_symbol);
    if per_tick_notional_default <= 0.0 {
        return None;
    }
    let per_tick_notional_usd = per_tick_notional_default.max(0.1);

    let best_bid = coinbase.best_bid.filter(|v| v.is_finite() && *v > 0.0)?;
    let best_ask = coinbase.best_ask.filter(|v| v.is_finite() && *v > 0.0)?;
    if best_ask <= best_bid {
        return None;
    }
    let mid_cb = coinbase.mid_price.unwrap_or((best_bid + best_ask) / 2.0);
    if !mid_cb.is_finite() || mid_cb <= 0.0 || !base_mid_cb.is_finite() || base_mid_cb <= 0.0 {
        return None;
    }
    let spread_cb = (best_ask - best_bid).max(0.0);
    if !spread_cb.is_finite() || spread_cb <= 0.0 {
        return None;
    }

    let (bias_alignment, bias_confidence, bias_multiplier) = advisory_bias_state();

    let d = base_mid_cb - mid_cb;
    let tau_scale = ((tau_seconds.max(1) as f64) / 15.0).sqrt();
    let spread_bps = ((spread_cb / mid_cb) * 10_000.0).clamp(0.0, 10_000.0);
    let distance_to_base_bps = ((base_mid_cb - mid_cb).abs() / mid_cb * 10_000.0).max(0.0);
    let sigma_floor_bps_15s = cfg
        .min_sigma_bps_15s
        .max(spread_bps * cfg.spread_to_sigma_mult);
    let sigma_floor_tau = mid_cb * (sigma_floor_bps_15s / 10_000.0) * tau_scale;
    let opposing_depth_usd = opposing_depth_to_reference_usd(coinbase, base_mid_cb, mid_cb);
    let depth_ratio = (opposing_depth_usd / per_tick_notional_usd.max(1.0)).max(0.0);
    let depth_sigma_mult = depth_sigma_multiplier(
        depth_ratio,
        cfg.depth_thick_threshold,
        cfg.depth_sigma_scale,
        cfg.depth_sigma_max_mult,
    );
    let legacy_thin_flip_guard_triggered = cfg.thin_flip_guard_enable
        && distance_to_base_bps <= cfg.thin_flip_guard_bps
        && opposing_depth_usd < cfg.thin_flip_guard_max_depth_usd;
    let sigma_tau = (sigma_floor_tau * cfg.sigma_mult * depth_sigma_mult).max(1e-6);
    let (p_up, p_down, z) =
        symmetric_direction_probabilities(d, sigma_tau, cfg.z_cap, cfg.probability_min);

    let spread_pct = (spread_cb / mid_cb).clamp(0.0, 0.5);
    let timeframe_seconds = (timeframe.duration_seconds().max(1)) as f64;
    let tau_ratio = ((tau_seconds as f64) / timeframe_seconds).clamp(0.0, 1.0);
    let near_close_discount = (1.0 - tau_ratio).clamp(0.0, 1.0);
    let burst_penalty = signal
        .binance_flow
        .burst_zscore
        .map(|v| (v.abs() * 0.002).clamp(0.0, 0.02))
        .unwrap_or(0.0);
    // Endgame acts in the final minute; decay uncertainty buffers as tau shrinks.
    let uncertainty_base = 0.004 + spread_pct * 1.6 + burst_penalty * 0.7;
    let uncertainty_penalty =
        (uncertainty_base * (1.0 - 0.45 * near_close_discount)).clamp(0.002, 0.10);
    let edge_floor_prob = (edge_floor_bps / 10_000.0).clamp(0.0, 0.20);
    let buffer_base = 0.002 + spread_pct * 0.6 + (1.0 - bias_multiplier).max(0.0) * 0.006;
    let buffer_prob = (buffer_base * (1.0 - 0.35 * near_close_discount)).clamp(0.001, 0.08);
    let up_pricing = side_pricing_from_probability(
        Direction::Up,
        p_up,
        uncertainty_penalty,
        buffer_prob,
        edge_floor_prob,
        bias_multiplier,
    );
    let down_pricing = side_pricing_from_probability(
        Direction::Down,
        p_down,
        uncertainty_penalty,
        buffer_prob,
        edge_floor_prob,
        bias_multiplier,
    );
    if up_pricing.edge_bps < edge_floor_bps && down_pricing.edge_bps < edge_floor_bps {
        return None;
    }
    // Endgame must trade the favored probability side only; do not flip side solely
    // because opposite-token orderbook edge looks temporarily better.
    let (direction, selected_side) = if p_up >= p_down {
        (Direction::Up, up_pricing)
    } else {
        (Direction::Down, down_pricing)
    };
    let mut thin_guard = if cfg.guard_v3_enable {
        thin_flip_guard_v3(
            tick_index,
            cfg.tick_offsets_sec.as_slice(),
            tau_seconds,
            distance_to_base_bps,
            sigma_tau,
            mid_cb,
            opposing_depth_usd,
            selected_side.max_price,
            uncertainty_penalty,
            per_tick_notional_usd,
            coinbase.trade_notional_p95_1s,
            cfg.guard_v3_fail_open,
            cfg.guard_v3_near_scale,
            cfg.guard_v3_impact_scale,
            cfg.guard_v3_last_tick_guard,
        )
    } else {
        ThinFlipGuardDecision {
            action: ThinFlipGuardAction::Pass,
            reason: "guard_v3_disabled".to_string(),
            triggered: false,
            near_threshold_bps: 0.0,
            min_distance_required_bps: 0.0,
            impact_usd: 0.0,
            impact_ratio: 0.0,
            impact_ratio_threshold: 0.0,
            extra_buffer_bps: 0.0,
            size_multiplier: 1.0,
            disabled_last_tick: false,
        }
    };
    if !cfg.guard_v3_enable && legacy_thin_flip_guard_triggered {
        thin_guard = ThinFlipGuardDecision {
            action: ThinFlipGuardAction::HardSkip,
            reason: "legacy_static_threshold".to_string(),
            triggered: true,
            near_threshold_bps: cfg.thin_flip_guard_bps.max(0.0),
            min_distance_required_bps: cfg.thin_flip_guard_bps.max(0.0),
            impact_usd: cfg.thin_flip_guard_max_depth_usd.max(1.0),
            impact_ratio: opposing_depth_usd / cfg.thin_flip_guard_max_depth_usd.max(1.0),
            impact_ratio_threshold: 1.0,
            extra_buffer_bps: 0.0,
            size_multiplier: 1.0,
            disabled_last_tick: false,
        };
    }
    if selected_side.edge_bps < edge_floor_bps {
        return None;
    }
    if !selected_side.max_price.is_finite() || selected_side.max_price <= 0.01 {
        return None;
    }
    if selected_side.max_price <= cfg.min_entry_price {
        return None;
    }

    let target_size_usd =
        (per_tick_notional_usd * latency_haircut * thin_guard.size_multiplier).max(1.0);
    let score = selected_side.score;
    let side = match direction {
        Direction::Up => "up",
        Direction::Down => "down",
    };
    let impulse_id = format!(
        "endgame:{}:{}:{}:tick{}",
        timeframe.as_str(),
        market_open_ts,
        side,
        tick_index
    );

    Some(EndgameIntentPlan {
        strategy_id: STRATEGY_ID_ENDGAME_SWEEP_V1.to_string(),
        timeframe,
        market_open_ts,
        tick_index,
        tau_seconds,
        p_up,
        p_down,
        z_score: z,
        sigma_tau,
        spread_bps,
        distance_to_base_bps,
        opposing_depth_usd,
        depth_ratio,
        depth_sigma_mult,
        thin_flip_guard_triggered: legacy_thin_flip_guard_triggered || thin_guard.triggered,
        thin_flip_guard_action: thin_guard.action,
        thin_flip_guard_reason: thin_guard.reason,
        thin_flip_near_threshold_bps: thin_guard.near_threshold_bps,
        thin_flip_min_distance_required_bps: thin_guard.min_distance_required_bps,
        thin_flip_impact_usd: thin_guard.impact_usd,
        thin_flip_impact_ratio: thin_guard.impact_ratio,
        thin_flip_impact_ratio_threshold: thin_guard.impact_ratio_threshold,
        thin_flip_extra_buffer_bps: thin_guard.extra_buffer_bps,
        thin_flip_size_multiplier: thin_guard.size_multiplier,
        thin_flip_disabled_last_tick: thin_guard.disabled_last_tick,
        uncertainty_penalty,
        buffer_prob,
        edge_floor_prob,
        bias_multiplier,
        direction,
        score,
        base_mid_cb,
        mid_cb,
        spread_cb,
        fair_probability: selected_side.fair_probability,
        execution_probability: selected_side.execution_probability,
        reservation_price: selected_side.reservation_price,
        max_price: selected_side.max_price,
        edge_bps: selected_side.edge_bps,
        target_size_usd,
        bias_alignment,
        bias_confidence,
        impulse_id,
    })
}

fn depth_between_prices_usd(levels: &[CoinbaseBookLevel], low: f64, high: f64) -> f64 {
    if !low.is_finite() || !high.is_finite() || high <= low {
        return 0.0;
    }
    levels
        .iter()
        .filter(|level| {
            level.price.is_finite()
                && level.size.is_finite()
                && level.price > 0.0
                && level.size > 0.0
                && level.price >= low
                && level.price <= high
        })
        .map(|level| level.price * level.size)
        .sum::<f64>()
}

fn opposing_depth_to_reference_usd(
    coinbase: &CoinbaseBookState,
    base_mid_cb: f64,
    mid_cb: f64,
) -> f64 {
    let low = base_mid_cb.min(mid_cb);
    let high = base_mid_cb.max(mid_cb);
    if mid_cb >= base_mid_cb {
        depth_between_prices_usd(coinbase.bid_levels.as_slice(), low, high)
    } else {
        depth_between_prices_usd(coinbase.ask_levels.as_slice(), low, high)
    }
}

fn depth_sigma_multiplier(
    depth_ratio: f64,
    thick_threshold: f64,
    depth_sigma_scale: f64,
    depth_sigma_max_mult: f64,
) -> f64 {
    let thick_threshold = thick_threshold.max(0.1);
    let depth_sigma_max_mult = depth_sigma_max_mult.max(1.0);
    if depth_ratio >= thick_threshold {
        return 1.0;
    }
    let adjusted_ratio = depth_ratio.max(0.1);
    let thinness = (thick_threshold / adjusted_ratio).ln().max(0.0);
    (1.0 + thinness * depth_sigma_scale.max(0.0)).clamp(1.0, depth_sigma_max_mult)
}

fn thin_flip_guard_v3(
    tick_index: u32,
    tick_offsets: &[u64],
    tau_seconds: u64,
    distance_to_base_bps: f64,
    sigma_tau: f64,
    mid_cb: f64,
    flip_cost_usd: f64,
    reference_price: f64,
    uncertainty_penalty: f64,
    per_tick_notional_usd: f64,
    impact_usd_from_trades: Option<f64>,
    fail_open: bool,
    near_scale: f64,
    impact_scale: f64,
    last_tick_guard: bool,
) -> ThinFlipGuardDecision {
    const EARLY_TICK_HARD_BLOCK_MAX_INDEX: usize = 3;
    const EARLY_TICK_HARD_BLOCK_BPS: f64 = 2.5;

    let tick_count = tick_offsets.len().max(1);
    let tick_usize = usize::try_from(tick_index).unwrap_or(0);
    let last_tick = tick_usize.saturating_add(1) >= tick_count;
    if last_tick && !last_tick_guard {
        return ThinFlipGuardDecision {
            action: ThinFlipGuardAction::Pass,
            reason: "last_tick_guard_disabled".to_string(),
            triggered: false,
            near_threshold_bps: 0.0,
            min_distance_required_bps: 0.0,
            impact_usd: impact_usd_from_trades.unwrap_or(0.0),
            impact_ratio: 0.0,
            impact_ratio_threshold: 0.0,
            extra_buffer_bps: 0.0,
            size_multiplier: 1.0,
            disabled_last_tick: true,
        };
    }
    if !distance_to_base_bps.is_finite()
        || !sigma_tau.is_finite()
        || sigma_tau <= 0.0
        || !mid_cb.is_finite()
        || mid_cb <= 0.0
    {
        return ThinFlipGuardDecision {
            action: if fail_open {
                ThinFlipGuardAction::Pass
            } else {
                ThinFlipGuardAction::HardSkip
            },
            reason: if fail_open {
                "invalid_guard_inputs_fail_open".to_string()
            } else {
                "invalid_guard_inputs_fail_closed".to_string()
            },
            triggered: !fail_open,
            near_threshold_bps: 0.0,
            min_distance_required_bps: 0.0,
            impact_usd: 0.0,
            impact_ratio: 0.0,
            impact_ratio_threshold: 0.0,
            extra_buffer_bps: 0.0,
            size_multiplier: 1.0,
            disabled_last_tick: false,
        };
    }

    if tick_usize <= EARLY_TICK_HARD_BLOCK_MAX_INDEX
        && distance_to_base_bps <= EARLY_TICK_HARD_BLOCK_BPS
    {
        return ThinFlipGuardDecision {
            action: ThinFlipGuardAction::HardSkip,
            reason: "tick0_3_within_2p5bps_hard_block".to_string(),
            triggered: true,
            near_threshold_bps: EARLY_TICK_HARD_BLOCK_BPS,
            min_distance_required_bps: EARLY_TICK_HARD_BLOCK_BPS,
            impact_usd: impact_usd_from_trades.unwrap_or(0.0).max(0.0),
            impact_ratio: 0.0,
            impact_ratio_threshold: 0.0,
            extra_buffer_bps: 0.0,
            size_multiplier: 1.0,
            disabled_last_tick: false,
        };
    }

    let denominator = (tick_count.saturating_sub(1)).max(1) as f64;
    let early_factor = ((tick_count.saturating_sub(1).saturating_sub(tick_usize)) as f64
        / denominator)
        .clamp(0.0, 1.0);
    let near_scale = near_scale.clamp(0.10, 5.0);
    let impact_scale = impact_scale.clamp(0.10, 10.0);
    let sigma_tau_bps = ((sigma_tau / mid_cb) * 10_000.0).max(0.0);
    let near_mult = (0.25 + 0.55 * early_factor).clamp(0.20, 0.85);
    let near_threshold_bps = (near_mult * sigma_tau_bps * near_scale).max(0.0);

    let price = reference_price.clamp(0.01, 0.99);
    let fee_rate = polymarket_taker_fee_rate(price);
    let tau_scale = ((tau_seconds.max(1) as f64) / 65.0).sqrt();
    let calibration_margin_prob =
        (0.010 + uncertainty_penalty.max(0.0) * 0.60 + 0.010 * early_factor * tau_scale)
            .clamp(0.005, 0.080);
    let p_req = (price + fee_rate + calibration_margin_prob).clamp(0.51, 0.995);
    let z_req = inverse_normal_cdf(p_req).abs();
    let min_distance_required_bps = (z_req * sigma_tau_bps).max(0.0);
    let near_gate_bps = near_threshold_bps.max(min_distance_required_bps);
    let near_triggered = distance_to_base_bps <= near_gate_bps;

    let impact_usd = impact_usd_from_trades
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or_else(|| {
            let base_notional = per_tick_notional_usd.max(1.0);
            // Fallback impact proxy when Coinbase trade flow is unavailable.
            (base_notional * (6.0 + 4.0 * early_factor)).max(5_000.0)
        });
    let impact_ratio = (flip_cost_usd / impact_usd.max(1.0)).max(0.0);
    let impact_ratio_threshold = match tick_usize {
        0 => 3.3,
        1 => 3.2,
        2 => 3.1,
        3 => 3.0,
        _ => ((0.70 + 1.30 * early_factor) * impact_scale).clamp(0.10, 5.0),
    };
    let thin_triggered = impact_ratio <= impact_ratio_threshold;
    let triggered = near_triggered && thin_triggered;
    if !triggered {
        return ThinFlipGuardDecision {
            action: ThinFlipGuardAction::Pass,
            reason: "guard_conditions_not_met".to_string(),
            triggered: false,
            near_threshold_bps: near_gate_bps,
            min_distance_required_bps,
            impact_usd,
            impact_ratio,
            impact_ratio_threshold,
            extra_buffer_bps: 0.0,
            size_multiplier: 1.0,
            disabled_last_tick: false,
        };
    }

    ThinFlipGuardDecision {
        action: ThinFlipGuardAction::HardSkip,
        reason: "triggered_near_strike_and_thin_flip_cost".to_string(),
        triggered: true,
        near_threshold_bps: near_gate_bps,
        min_distance_required_bps,
        impact_usd,
        impact_ratio,
        impact_ratio_threshold,
        extra_buffer_bps: 0.0,
        size_multiplier: 1.0,
        disabled_last_tick: false,
    }
}

fn inverse_normal_cdf(p: f64) -> f64 {
    // Acklam inverse-normal approximation.
    let p = p.clamp(1e-12, 1.0 - 1e-12);
    let a: [f64; 6] = [
        -39.696_830_286_653_76,
        220.946_098_424_520_5,
        -275.928_510_446_968_7,
        138.357_751_867_269,
        -30.664_798_066_147_16,
        2.506_628_277_459_239,
    ];
    let b: [f64; 5] = [
        -54.476_098_798_224_06,
        161.585_836_858_040_9,
        -155.698_979_859_886_6,
        66.801_311_887_719_72,
        -13.280_681_552_885_72,
    ];
    let c: [f64; 6] = [
        -0.007_784_894_002_430_293,
        -0.322_396_458_041_136_5,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    let d: [f64; 4] = [
        0.007_784_695_709_041_462,
        0.322_467_129_070_039_8,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    let plow = 0.024_25;
    let phigh = 1.0 - plow;
    if p < plow {
        let q = (-2.0 * p.ln()).sqrt();
        return (((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5])
            / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0);
    }
    if p > phigh {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        return -(((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5])
            / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0);
    }
    let q = p - 0.5;
    let r = q * q;
    (((((a[0] * r + a[1]) * r + a[2]) * r + a[3]) * r + a[4]) * r + a[5]) * q
        / (((((b[0] * r + b[1]) * r + b[2]) * r + b[3]) * r + b[4]) * r + 1.0)
}

fn symmetric_direction_probabilities(
    d: f64,
    sigma_tau: f64,
    z_cap: f64,
    probability_min: f64,
) -> (f64, f64, f64) {
    let z_cap = z_cap.max(0.1);
    let prob_min = probability_min.clamp(0.000_001, 0.20);
    let z = if sigma_tau.is_finite() && sigma_tau > 0.0 {
        (d / sigma_tau).clamp(-z_cap, z_cap)
    } else {
        0.0
    };
    let p_down = normal_cdf(z).clamp(prob_min, 1.0 - prob_min);
    let p_up = 1.0 - p_down;
    (p_up, p_down, z)
}

fn side_pricing_from_probability(
    direction: Direction,
    fair_probability: f64,
    uncertainty_penalty: f64,
    buffer_prob: f64,
    edge_floor_prob: f64,
    bias_multiplier: f64,
) -> EndgameSidePricing {
    let execution_probability = (fair_probability - uncertainty_penalty).clamp(0.01, 0.99);
    let fee_prob = polymarket_taker_fee_rate(execution_probability);
    let reservation_price = (execution_probability - buffer_prob - fee_prob).clamp(0.01, 0.99);
    let max_price = (reservation_price - edge_floor_prob).clamp(0.01, reservation_price);
    let fee_at_max_price = polymarket_taker_fee_rate(max_price);
    let edge_bps = ((execution_probability - max_price - fee_at_max_price) * 10_000.0).max(0.0);
    let score = ((edge_bps / 100.0) * bias_multiplier).clamp(0.01, 20.0);
    EndgameSidePricing {
        direction,
        fair_probability,
        execution_probability,
        fee_rate_execution: fee_prob,
        reservation_price,
        max_price,
        fee_rate_max_price: fee_at_max_price,
        edge_bps,
        score,
    }
}

pub fn ev_safe_execution_sizing(
    fair_probability: f64,
    edge_floor_bps: f64,
    max_price: f64,
    target_notional_usd: f64,
    asks: &[BookAskLevel],
) -> Option<EndgameExecutionSizing> {
    if !fair_probability.is_finite()
        || !max_price.is_finite()
        || !target_notional_usd.is_finite()
        || fair_probability <= 0.0
        || max_price <= 0.0
        || target_notional_usd <= 0.0
    {
        return None;
    }

    let mut levels = asks
        .iter()
        .filter(|level| {
            level.price.is_finite()
                && level.size.is_finite()
                && level.price > 0.0
                && level.size > 0.0
        })
        .collect::<Vec<_>>();
    levels.sort_by(|a, b| {
        a.price
            .partial_cmp(&b.price)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut remaining_notional = target_notional_usd.max(0.0);
    let mut total_notional = 0.0_f64;
    let mut total_shares = 0.0_f64;
    let edge_floor_prob = (edge_floor_bps / 10_000.0).max(0.0);
    // Keep the running VWAP below a fee-adjusted threshold so partial-taking paths
    // can still satisfy the final fee-aware edge check.
    let fee_buffer_prob = polymarket_taker_fee_rate(fair_probability.clamp(0.001, 0.999));
    let required_vwap =
        (fair_probability - edge_floor_prob - fee_buffer_prob).clamp(0.000_001, 0.999_999);

    for level in levels {
        if remaining_notional <= 0.0 {
            break;
        }
        if level.price > max_price {
            continue;
        }
        let level_notional = level.price * level.size;
        if !level_notional.is_finite() || level_notional <= 0.0 {
            continue;
        }
        let mut take_notional = remaining_notional.min(level_notional);
        if level.price > required_vwap + 1e-9 {
            let edge_margin_notional = required_vwap * total_shares - total_notional;
            if edge_margin_notional <= 1e-9 {
                break;
            }
            let price_penalty = level.price - required_vwap;
            if price_penalty <= 1e-9 {
                continue;
            }
            let max_safe_shares = edge_margin_notional / price_penalty;
            let max_safe_notional = (max_safe_shares * level.price).max(0.0);
            take_notional = take_notional.min(max_safe_notional);
            if take_notional <= 1e-9 {
                break;
            }
        }
        if take_notional <= 0.0 {
            continue;
        }
        let take_shares = take_notional / level.price;
        if !take_shares.is_finite() || take_shares <= 0.0 {
            continue;
        }
        total_notional += take_notional;
        total_shares += take_shares;
        remaining_notional -= take_notional;
        if level.price > required_vwap + 1e-9 {
            let edge_margin_notional = required_vwap * total_shares - total_notional;
            if edge_margin_notional <= 1e-9 {
                break;
            }
        }
    }

    if total_notional <= 0.0 || total_shares <= 0.0 {
        return None;
    }

    let vwap_price = total_notional / total_shares;
    if !vwap_price.is_finite() || vwap_price <= 0.0 {
        return None;
    }
    let fee_at_vwap = polymarket_taker_fee_rate(vwap_price);
    let edge_bps_at_vwap = ((fair_probability - vwap_price - fee_at_vwap) * 10_000.0).max(0.0);
    if edge_bps_at_vwap + 1e-6 < edge_floor_bps {
        return None;
    }

    Some(EndgameExecutionSizing {
        shares: total_shares,
        notional_usd: total_notional,
        vwap_price,
        taker_fee_rate_at_vwap: fee_at_vwap,
        edge_bps_at_vwap,
    })
}

pub fn apply_divergence_size_guard(
    target_notional_usd: f64,
    fair_probability: f64,
    poly_mid_probability: Option<f64>,
    divergence_threshold: f64,
    divergence_min_size_usd: f64,
) -> (f64, bool, Option<f64>) {
    let base_target = target_notional_usd.max(0.0);
    let Some(poly_mid) = poly_mid_probability.filter(|v| v.is_finite() && *v > 0.0) else {
        return (base_target, false, None);
    };
    if !fair_probability.is_finite() || fair_probability <= 0.0 {
        return (base_target, false, None);
    }
    let divergence = (poly_mid - fair_probability).abs();
    if divergence_threshold <= 0.0 || divergence < divergence_threshold {
        return (base_target, false, Some(divergence));
    }
    let reduced_target = base_target.min(divergence_min_size_usd.max(1.0));
    let reduced = reduced_target + 1e-9 < base_target;
    (reduced_target, reduced, Some(divergence))
}

fn advisory_bias_state() -> (String, f64, f64) {
    ("none".to_string(), 0.0, 0.9)
}

fn normal_cdf(x: f64) -> f64 {
    // Abramowitz and Stegun approximation.
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let z = x.abs() / (2.0_f64).sqrt();
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let a1 = 0.254_829_592;
    let a2 = -0.284_496_736;
    let a3 = 1.421_413_741;
    let a4 = -1.453_152_027;
    let a5 = 1.061_405_429;
    let erf = 1.0 - (((((a5 * t + a4) * t + a3) * t + a2) * t + a1) * t * (-(z * z)).exp());
    let erf = sign * erf;
    (0.5 * (1.0 + erf)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_endgame_cfg() -> EndgameExecutionConfig {
        EndgameExecutionConfig {
            enable: true,
            poll_interval_ms: 500,
            safety_poll_ms: 500,
            tick_random_jitter_ms: 0,
            enabled_symbols_csv: "BTC".to_string(),
            enabled_timeframes_csv: "5m".to_string(),
            tick_offsets_sec: vec![65_000, 50_000, 35_000, 20_000, 5_000],
            allow_cross_spread: true,
            per_tick_notional_usd: 50.0,
            per_tick_notional_by_offset: vec![
                (65_000, 50.0),
                (50_000, 75.0),
                (35_000, 100.0),
                (20_000, 125.0),
                (5_000, 250.0),
            ],
            per_tick_notional_by_symbol: std::collections::HashMap::new(),
            per_period_cap_usd: 250.0,
            max_signal_age_ms: 60_000,
            safety_stop_s: 2,
            book_freshness_ms: 1_500,
            latency_haircut: 0.85,
            edge_floor_bps: 12.0,
            min_entry_price: 0.5,
            probability_min: 0.005,
            z_cap: 4.0,
            min_sigma_bps_15s: 1.0,
            spread_to_sigma_mult: 10.0,
            sigma_mult: 1.15,
            depth_thick_threshold: 10.0,
            depth_sigma_scale: 0.35,
            depth_sigma_max_mult: 3.0,
            thin_flip_guard_enable: true,
            thin_flip_guard_bps: 10.0,
            thin_flip_guard_max_depth_usd: 30_000.0,
            guard_v3_enable: true,
            guard_v3_fail_open: false,
            guard_v3_near_scale: 0.9224,
            guard_v3_impact_scale: 1.5806,
            guard_v3_hard_cutoff: 0.6938,
            guard_v3_soft_scale: 0.3248,
            guard_v3_last_tick_guard: false,
            guard_v3_min_soft_multiplier: 0.2072,
            divergence_threshold: 0.18,
            divergence_min_size_usd: 10.0,
            divergence_min_size_ratio: 0.10,
        }
    }

    fn test_signal(now_ms: i64) -> SignalState {
        let mut signal = SignalState::default();
        signal.binance_flow_updated_at_ms = now_ms;
        signal.updated_at_ms = now_ms;
        signal
    }

    fn test_coinbase(mid: f64, spread: f64, now_ms: i64) -> CoinbaseBookState {
        CoinbaseBookState {
            feed_state: CoinbaseFeedState::Healthy,
            product_id: "BTC-USD".to_string(),
            best_bid: Some(mid - spread / 2.0),
            best_ask: Some(mid + spread / 2.0),
            mid_price: Some(mid),
            spread: Some(spread),
            top_bid_depth: Some(10.0),
            top_ask_depth: Some(10.0),
            imbalance: Some(0.0),
            book_update_rate_5s: Some(10.0),
            trade_notional_p95_1s: Some(50_000.0),
            last_trade_notional_usd: Some(25_000.0),
            last_trade_ts_ms: Some(now_ms),
            last_qualifying_trade_notional_usd: Some(25_000.0),
            last_qualifying_trade_ts_ms: Some(now_ms),
            last_qualifying_trade_seq: Some(1),
            max_trade_notional_15s: Some(25_000.0),
            max_trade_notional_15s_ts_ms: Some(now_ms),
            trade_update_rate_5s: Some(3.0),
            recent_trades: vec![],
            period_anchors_15m: vec![],
            bid_levels: vec![
                CoinbaseBookLevel {
                    price: mid - spread / 2.0,
                    size: 100.0,
                },
                CoinbaseBookLevel {
                    price: mid - spread / 2.0 - 5.0,
                    size: 100.0,
                },
            ],
            ask_levels: vec![
                CoinbaseBookLevel {
                    price: mid + spread / 2.0,
                    size: 100.0,
                },
                CoinbaseBookLevel {
                    price: mid + spread / 2.0 + 5.0,
                    size: 100.0,
                },
            ],
            last_update_ms: now_ms,
            last_state_transition_ms: now_ms,
            state_reason: Some("test".to_string()),
        }
    }

    #[test]
    fn endgame_tick_schedule_matches_offsets() {
        let offsets = vec![65_000, 50_000, 35_000, 20_000, 5_000];
        assert_eq!(tick_index_for_tau(&offsets, 64_000, 2_000), Some(0));
        assert_eq!(tick_index_for_tau(&offsets, 50_000, 2_000), Some(1));
        assert_eq!(tick_index_for_tau(&offsets, 49_000, 2_000), Some(1));
        assert_eq!(tick_index_for_tau(&offsets, 35_000, 2_000), Some(2));
        assert_eq!(tick_index_for_tau(&offsets, 34_000, 2_000), Some(2));
        assert_eq!(tick_index_for_tau(&offsets, 4_000, 2_000), Some(4));
        assert_eq!(tick_index_for_tau(&offsets, 2_000, 2_000), None);
    }

    #[test]
    fn endgame_tick_dedupe_key_blocks_repeat_tick_emit() {
        let mut emitted = std::collections::HashSet::new();
        let key = ("5m", 1_771_560_000_i64, "token-1", "BUY", 2_u32);
        assert!(emitted.insert(key));
        assert!(!emitted.insert(key));
    }

    #[test]
    fn polymarket_taker_fee_rate_matches_known_points() {
        let at_50 = polymarket_taker_fee_rate(0.50);
        let at_60 = polymarket_taker_fee_rate(0.60);
        let at_90 = polymarket_taker_fee_rate(0.90);
        assert!((at_50 - 0.015625).abs() < 1e-9);
        assert!((at_60 - 0.0144).abs() < 1e-9);
        assert!((at_90 - 0.002025).abs() < 1e-9);
    }

    #[test]
    fn ev_safe_execution_sizing_honors_qmax_and_edge_floor() {
        let asks = vec![
            BookAskLevel {
                price: 0.41,
                size: 120.0,
            },
            BookAskLevel {
                price: 0.42,
                size: 60.0,
            },
            BookAskLevel {
                price: 0.47,
                size: 500.0,
            },
        ];
        let sizing =
            ev_safe_execution_sizing(0.50, 30.0, 0.43, 50.0, &asks).expect("expected sizing");
        assert!(sizing.notional_usd > 0.0);
        assert!(sizing.vwap_price <= 0.43 + 1e-9);
        assert!(sizing.edge_bps_at_vwap >= 30.0);
    }

    #[test]
    fn ev_safe_execution_sizing_rejects_if_edge_too_low() {
        let asks = vec![BookAskLevel {
            price: 0.49,
            size: 200.0,
        }];
        let sizing = ev_safe_execution_sizing(0.4905, 15.0, 0.50, 50.0, &asks);
        assert!(sizing.is_none());
    }

    #[test]
    fn ev_safe_execution_sizing_rejects_when_fee_erases_edge() {
        let asks = vec![BookAskLevel {
            price: 0.50,
            size: 500.0,
        }];
        // Raw edge is 200 bps, but fee at 0.50 is 156.25 bps, leaving < 50 bps.
        let sizing = ev_safe_execution_sizing(0.52, 50.0, 0.55, 50.0, &asks);
        assert!(sizing.is_none());
    }

    #[test]
    fn divergence_guard_reduces_target_to_min_size_bucket() {
        let (target, reduced, divergence) =
            apply_divergence_size_guard(50.0, 0.90, Some(0.55), 0.20, 10.0);
        assert!(reduced);
        assert!((target - 10.0).abs() < 1e-9);
        assert!(divergence.unwrap_or_default() >= 0.35);
    }

    #[test]
    fn divergence_guard_keeps_target_when_divergence_small() {
        let (target, reduced, divergence) =
            apply_divergence_size_guard(50.0, 0.90, Some(0.82), 0.20, 10.0);
        assert!(!reduced);
        assert!((target - 50.0).abs() < 1e-9);
        assert!(divergence.unwrap_or_default() < 0.20);
    }

    #[test]
    fn depth_sigma_multiplier_inflates_when_depth_is_thin() {
        let thin = depth_sigma_multiplier(0.5, 10.0, 0.35, 3.0);
        let thick = depth_sigma_multiplier(20.0, 10.0, 0.35, 3.0);
        assert!(thin > 1.0);
        assert_eq!(thick, 1.0);
        assert!(thin <= 3.0);
    }

    #[test]
    fn ev_safe_execution_sizing_sorts_unsorted_asks_best_first() {
        let asks_worst_first = vec![
            BookAskLevel {
                price: 0.99,
                size: 20.0,
            },
            BookAskLevel {
                price: 0.80,
                size: 20.0,
            },
            BookAskLevel {
                price: 0.40,
                size: 200.0,
            },
        ];

        let sizing = ev_safe_execution_sizing(0.55, 50.0, 0.45, 50.0, &asks_worst_first)
            .expect("expected sizing from best-priced ask level");
        assert!(sizing.vwap_price <= 0.45);
        assert!(sizing.edge_bps_at_vwap >= 50.0);
    }

    #[test]
    fn ev_safe_execution_sizing_allows_partial_when_full_target_breaks_edge() {
        let asks = vec![
            BookAskLevel {
                price: 0.40,
                size: 25.0, // $10 notional
            },
            BookAskLevel {
                price: 0.60,
                size: 200.0, // deep but expensive
            },
        ];
        let sizing = ev_safe_execution_sizing(0.50, 100.0, 0.70, 50.0, &asks)
            .expect("expected partial sizing instead of full skip");
        assert!(sizing.notional_usd > 0.0);
        assert!(sizing.notional_usd < 50.0);
        assert!(sizing.edge_bps_at_vwap >= 99.99);
    }

    #[test]
    fn advisory_bias_state_is_deterministic() {
        assert_eq!(advisory_bias_state(), ("none".to_string(), 0.0, 0.9));
    }

    #[test]
    fn build_intent_plan_selects_favored_probability_side_only() {
        let cfg = test_endgame_cfg();
        let market_open_ts = 1_771_602_100_i64;
        let now_ms = (market_open_ts + 240) * 1_000;
        let signal = test_signal(now_ms);
        let coinbase = test_coinbase(90.0, 1.0, now_ms);
        let plan = build_intent_plan(
            &cfg,
            "BTC",
            Timeframe::M5,
            market_open_ts,
            100.0,
            &signal,
            &coinbase,
            now_ms,
        )
        .expect("expected endgame plan");
        assert!(plan.p_down > plan.p_up);
        assert_eq!(plan.direction, Direction::Down);
    }

    #[test]
    fn build_intent_plan_respects_min_entry_price_floor() {
        let market_open_ts = 1_771_602_400_i64;
        let now_ms = (market_open_ts + 240) * 1_000;
        let signal = test_signal(now_ms);
        let coinbase = test_coinbase(90.0, 1.0, now_ms);

        let mut cfg = test_endgame_cfg();
        cfg.min_entry_price = 0.99;
        let blocked = build_intent_plan(
            &cfg,
            "BTC",
            Timeframe::M5,
            market_open_ts,
            100.0,
            &signal,
            &coinbase,
            now_ms,
        );
        assert!(blocked.is_none());

        cfg.min_entry_price = 0.5;
        let allowed = build_intent_plan(
            &cfg,
            "BTC",
            Timeframe::M5,
            market_open_ts,
            100.0,
            &signal,
            &coinbase,
            now_ms,
        );
        assert!(allowed.is_some());
    }

    #[test]
    fn build_intent_plan_thin_flip_guard_triggers_for_thin_near_base() {
        let mut cfg = test_endgame_cfg();
        cfg.edge_floor_bps = 0.0;
        cfg.min_entry_price = 0.01;
        cfg.thin_flip_guard_enable = true;
        cfg.thin_flip_guard_bps = 10.0;
        cfg.thin_flip_guard_max_depth_usd = 30_000.0;

        let market_open_ts = 1_771_602_700_i64;
        let now_ms = (market_open_ts + 240) * 1_000;
        let signal = test_signal(now_ms);
        let coinbase = test_coinbase(90.0, 1.0, now_ms);
        let plan = build_intent_plan(
            &cfg,
            "BTC",
            Timeframe::M5,
            market_open_ts,
            90.05,
            &signal,
            &coinbase,
            now_ms,
        )
        .expect("expected endgame plan");
        assert!(plan.distance_to_base_bps <= 10.0);
        assert!(plan.opposing_depth_usd < 30_000.0);
        assert!(plan.thin_flip_guard_triggered);
        assert_eq!(plan.thin_flip_guard_action, ThinFlipGuardAction::HardSkip);
    }

    #[test]
    fn build_intent_plan_thin_flip_guard_not_triggered_when_distance_exceeds_bps() {
        let mut cfg = test_endgame_cfg();
        cfg.edge_floor_bps = 0.0;
        cfg.min_entry_price = 0.01;
        cfg.guard_v3_enable = false;
        cfg.thin_flip_guard_enable = true;
        cfg.thin_flip_guard_bps = 10.0;
        cfg.thin_flip_guard_max_depth_usd = 30_000.0;

        let market_open_ts = 1_771_603_000_i64;
        let now_ms = (market_open_ts + 240) * 1_000;
        let signal = test_signal(now_ms);
        let coinbase = test_coinbase(90.0, 1.0, now_ms);
        let plan = build_intent_plan(
            &cfg,
            "BTC",
            Timeframe::M5,
            market_open_ts,
            100.0,
            &signal,
            &coinbase,
            now_ms,
        )
        .expect("expected endgame plan");
        assert!(plan.distance_to_base_bps > 10.0);
        assert!(!plan.thin_flip_guard_triggered);
        assert_eq!(plan.thin_flip_guard_action, ThinFlipGuardAction::Pass);
    }

    #[test]
    fn build_intent_plan_thin_flip_guard_not_triggered_when_depth_is_thick() {
        let mut cfg = test_endgame_cfg();
        cfg.edge_floor_bps = 0.0;
        cfg.min_entry_price = 0.01;
        cfg.guard_v3_enable = false;
        cfg.thin_flip_guard_enable = true;
        cfg.thin_flip_guard_bps = 10.0;
        cfg.thin_flip_guard_max_depth_usd = 30_000.0;

        let market_open_ts = 1_771_603_300_i64;
        let now_ms = (market_open_ts + 240) * 1_000;
        let signal = test_signal(now_ms);
        let mut coinbase = test_coinbase(90.0, 1.0, now_ms);
        coinbase.ask_levels.push(CoinbaseBookLevel {
            price: 90.01,
            size: 1_000.0,
        });
        let plan = build_intent_plan(
            &cfg,
            "BTC",
            Timeframe::M5,
            market_open_ts,
            90.05,
            &signal,
            &coinbase,
            now_ms,
        )
        .expect("expected endgame plan");
        assert!(plan.distance_to_base_bps <= 10.0);
        assert!(plan.opposing_depth_usd >= 30_000.0);
        assert!(!plan.thin_flip_guard_triggered);
        assert_eq!(plan.thin_flip_guard_action, ThinFlipGuardAction::Pass);
    }

    #[test]
    fn build_intent_plan_thin_flip_guard_respects_disable_flag() {
        let mut cfg = test_endgame_cfg();
        cfg.edge_floor_bps = 0.0;
        cfg.min_entry_price = 0.01;
        cfg.thin_flip_guard_enable = false;
        cfg.guard_v3_enable = false;
        cfg.thin_flip_guard_bps = 10.0;
        cfg.thin_flip_guard_max_depth_usd = 30_000.0;

        let market_open_ts = 1_771_603_600_i64;
        let now_ms = (market_open_ts + 240) * 1_000;
        let signal = test_signal(now_ms);
        let coinbase = test_coinbase(90.0, 1.0, now_ms);
        let plan = build_intent_plan(
            &cfg,
            "BTC",
            Timeframe::M5,
            market_open_ts,
            90.05,
            &signal,
            &coinbase,
            now_ms,
        )
        .expect("expected endgame plan");
        assert!(!plan.thin_flip_guard_triggered);
        assert_eq!(plan.thin_flip_guard_action, ThinFlipGuardAction::Pass);
    }

    #[test]
    fn build_intent_plan_guard_disabled_on_last_tick() {
        let mut cfg = test_endgame_cfg();
        cfg.edge_floor_bps = 0.0;
        cfg.min_entry_price = 0.01;
        cfg.guard_v3_enable = true;
        cfg.safety_stop_s = 0;
        cfg.tick_offsets_sec = vec![3_000, 1_000, 100];

        let market_open_ts = 1_771_603_900_i64;
        let close_ms = market_open_ts.saturating_add(Timeframe::M5.duration_seconds()) * 1_000;
        let now_ms = close_ms.saturating_sub(50);
        let signal = test_signal(now_ms);
        let coinbase = test_coinbase(90.0, 1.0, now_ms);
        let plan = build_intent_plan(
            &cfg,
            "BTC",
            Timeframe::M5,
            market_open_ts,
            90.05,
            &signal,
            &coinbase,
            now_ms,
        )
        .expect("expected endgame plan");
        assert_eq!(plan.tick_index, 2);
        assert!(plan.thin_flip_disabled_last_tick);
        assert_eq!(plan.thin_flip_guard_action, ThinFlipGuardAction::Pass);
    }

    #[test]
    fn build_intent_plan_hard_skips_tick0_when_within_2p5bps() {
        let mut cfg = test_endgame_cfg();
        cfg.edge_floor_bps = 0.0;
        cfg.min_entry_price = 0.01;
        cfg.guard_v3_enable = true;

        let market_open_ts = 1_771_604_200_i64;
        let now_ms = (market_open_ts + 237) * 1_000; // tau=63s => tick0
        let signal = test_signal(now_ms);
        let coinbase = test_coinbase(90.0, 1.0, now_ms);
        let plan = build_intent_plan(
            &cfg,
            "BTC",
            Timeframe::M5,
            market_open_ts,
            90.02, // ~2.22 bps from mid
            &signal,
            &coinbase,
            now_ms,
        )
        .expect("expected endgame plan");

        assert!(plan.distance_to_base_bps <= 2.5);
        assert_eq!(plan.tick_index, 0);
        assert_eq!(plan.thin_flip_guard_action, ThinFlipGuardAction::HardSkip);
        assert_eq!(
            plan.thin_flip_guard_reason,
            "tick0_3_within_2p5bps_hard_block"
        );
    }

    #[test]
    fn build_intent_plan_uses_hardcoded_tick_size_split() {
        unsafe { std::env::set_var("EVPOLY_ENDGAME_BASE_SIZE_USD", "100") };
        let mut cfg = test_endgame_cfg();
        cfg.edge_floor_bps = 0.0;
        cfg.min_entry_price = 0.01;
        cfg.latency_haircut = 1.0;
        cfg.guard_v3_enable = false;
        cfg.thin_flip_guard_enable = false;

        let market_open_ts = 1_771_604_500_i64;
        let now_ms = (market_open_ts + 240) * 1_000;
        let signal = test_signal(now_ms);
        let coinbase = test_coinbase(90.0, 1.0, now_ms);

        let plan_tick0 = build_intent_plan_for_tick(
            &cfg,
            "BTC",
            Timeframe::M5,
            market_open_ts,
            100.0,
            &signal,
            &coinbase,
            now_ms,
            Some(0),
        )
        .expect("tick0 plan");
        let plan_tick1 = build_intent_plan_for_tick(
            &cfg,
            "BTC",
            Timeframe::M5,
            market_open_ts,
            100.0,
            &signal,
            &coinbase,
            now_ms,
            Some(1),
        )
        .expect("tick1 plan");
        let plan_tick2 = build_intent_plan_for_tick(
            &cfg,
            "BTC",
            Timeframe::M5,
            market_open_ts,
            100.0,
            &signal,
            &coinbase,
            now_ms,
            Some(2),
        )
        .expect("tick2 plan");
        let plan_eth_tick1 = build_intent_plan_for_tick(
            &cfg,
            "ETH",
            Timeframe::M5,
            market_open_ts,
            100.0,
            &signal,
            &coinbase,
            now_ms,
            Some(1),
        )
        .expect("eth tick1 plan");

        assert!((plan_tick0.target_size_usd - 20.0).abs() < 1e-6);
        assert!((plan_tick1.target_size_usd - 40.0).abs() < 1e-6);
        assert!((plan_tick2.target_size_usd - 40.0).abs() < 1e-6);
        assert!((plan_eth_tick1.target_size_usd - 32.0).abs() < 1e-6);
        unsafe { std::env::remove_var("EVPOLY_ENDGAME_BASE_SIZE_USD") };
    }
}
