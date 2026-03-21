use std::collections::{HashMap, HashSet};

use crate::plan3_tables::{Plan3LookupResult, Plan3Tables};
use crate::plandaily_tables::{PlanDailyLookupResult, PlanDailyTables};
use crate::size_policy;
use crate::strategy::{Direction, Timeframe, STRATEGY_ID_EVCURVE_V1};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldSide {
    Up,
    Down,
}

impl HoldSide {
    pub fn as_str(self) -> &'static str {
        match self {
            HoldSide::Up => "UP",
            HoldSide::Down => "DOWN",
        }
    }

    pub fn direction(self) -> Direction {
        match self {
            HoldSide::Up => Direction::Up,
            HoldSide::Down => Direction::Down,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurvePoint {
    pub flip_prob: f64,
    pub max_buy_price: f64,
}

#[derive(Debug, Clone)]
pub struct MaxBuyCurve {
    points: Vec<CurvePoint>,
}

impl MaxBuyCurve {
    pub fn from_csv(value: &str) -> Option<Self> {
        let mut points = value
            .split(',')
            .filter_map(|part| {
                let (x, y) = part.split_once(':')?;
                let flip_prob = x.trim().parse::<f64>().ok()?;
                let max_buy_price = y.trim().parse::<f64>().ok()?;
                if !flip_prob.is_finite() || !max_buy_price.is_finite() {
                    return None;
                }
                Some(CurvePoint {
                    flip_prob: flip_prob.clamp(0.0, 1.0),
                    max_buy_price: max_buy_price.clamp(0.0, 1.0),
                })
            })
            .collect::<Vec<_>>();

        if points.is_empty() {
            return None;
        }
        points.sort_by(|a, b| a.flip_prob.total_cmp(&b.flip_prob));
        Some(Self { points })
    }

    pub fn evaluate(&self, p_flip: f64) -> f64 {
        let p = p_flip.clamp(0.0, 1.0);
        if self.points.is_empty() {
            return 0.0;
        }
        if p <= self.points[0].flip_prob {
            return self.points[0].max_buy_price;
        }
        if let Some(last) = self.points.last() {
            if p >= last.flip_prob {
                return last.max_buy_price;
            }
        }

        for window in self.points.windows(2) {
            let left = window[0];
            let right = window[1];
            if p >= left.flip_prob && p <= right.flip_prob {
                if (right.flip_prob - left.flip_prob).abs() <= f64::EPSILON {
                    return right.max_buy_price;
                }
                let w = (p - left.flip_prob) / (right.flip_prob - left.flip_prob);
                return left.max_buy_price + (right.max_buy_price - left.max_buy_price) * w;
            }
        }

        self.points.last().map(|p| p.max_buy_price).unwrap_or(0.0)
    }

    pub fn points(&self) -> &[CurvePoint] {
        &self.points
    }
}

impl Default for MaxBuyCurve {
    fn default() -> Self {
        Self::from_csv(
            "0:0.99,0.01:0.98,0.02:0.96,0.03:0.94,0.04:0.92,0.05:0.9,0.06:0.88,0.07:0.85,0.08:0.84,0.09:0.83,0.1:0.82,0.11:0.8,0.12:0.8,0.13:0.75,0.14:0.72,0.15:0.7",
        )
        .unwrap_or_else(|| Self {
            points: vec![CurvePoint {
                flip_prob: 0.0,
                max_buy_price: 0.0,
            }],
        })
    }
}

#[derive(Debug, Clone)]
pub struct EvcurveExecutionConfig {
    pub enable: bool,
    pub d1_enable: bool,
    pub poll_interval_ms: u64,
    pub symbols: Vec<String>,
    pub timeframes: Vec<Timeframe>,
    pub max_buy_curve: MaxBuyCurve,
    per_symbol_max_buy_curve: HashMap<String, MaxBuyCurve>,
    pub m15_t2_no_fak_fallback: bool,
    m15_t2_max_buy_curve: Option<MaxBuyCurve>,
    pub min_samples: u32,
    pub max_flip_prob: f64,
    pub min_buy_price: f64,
    pub base_anchor_grace_sec: i64,
    pub tick_jitter_sec: i64,
    pub max_late_ms: i64,
    pub proxy_stale_ms: i64,
    pub pm_quote_fresh_ms: i64,
    pub pm_quote_pair_max_skew_ms: i64,
    pub strategy_cap_usd: f64,
    pub d1_strategy_cap_usd: f64,
    pub d1_zero_min_n: u32,
    pub d1_ev_min_n: u32,
    pub d1_ev_gap: f64,
    base_size_usd: f64,
    per_market_caps: HashMap<(String, Timeframe), f64>,
}

impl EvcurveExecutionConfig {
    pub fn from_env() -> Self {
        let symbols = parse_symbols_env("EVPOLY_EVCURVE_SYMBOLS", &["BTC", "ETH", "SOL", "XRP"]);
        let mut timeframes = parse_timeframes_env(
            "EVPOLY_EVCURVE_TIMEFRAMES",
            &[Timeframe::M15, Timeframe::H1, Timeframe::H4, Timeframe::D1],
        );
        let d1_enable = env_bool("EVPOLY_EVCURVE_D1_ENABLE", true);
        if d1_enable && !timeframes.contains(&Timeframe::D1) {
            timeframes.push(Timeframe::D1);
        }
        let curve = std::env::var("EVPOLY_EVCURVE_MAXBUY_CURVE")
            .ok()
            .and_then(|v| MaxBuyCurve::from_csv(v.as_str()))
            .unwrap_or_default();
        let per_symbol_max_buy_curve = symbols
            .iter()
            .filter_map(|symbol| {
                let key = format!("EVPOLY_EVCURVE_MAXBUY_CURVE_{}", symbol);
                let parsed = std::env::var(key.as_str())
                    .ok()
                    .and_then(|v| MaxBuyCurve::from_csv(v.as_str()));
                parsed.map(|curve| (symbol.clone(), curve))
            })
            .collect::<HashMap<_, _>>();
        let m15_t2_max_buy_curve = std::env::var("EVPOLY_EVCURVE_15M_T2_MAXBUY_CURVE")
            .ok()
            .and_then(|v| MaxBuyCurve::from_csv(v.as_str()))
            .or_else(|| {
                MaxBuyCurve::from_csv(
                    "0:0.99,0.01:0.98,0.02:0.96,0.03:0.94,0.04:0.92,0.05:0.9,0.06:0.88,0.07:0.85,0.08:0.75,0.09:0.72,0.1:0.69,0.11:0.67,0.12:0.65,0.13:0.64,0.14:0.63,0.15:0.62",
                )
            });
        let base_size_usd = size_policy::base_size_usd_from_env("EVPOLY_EVCURVE_BASE_SIZE_USD");

        let mut per_market_caps = HashMap::new();
        for symbol in &symbols {
            for timeframe in &timeframes {
                let default = default_scope_cap(base_size_usd, symbol.as_str(), *timeframe);
                let key = format!(
                    "EVPOLY_EVCURVE_SCOPE_CAP_{}_{}",
                    symbol,
                    timeframe.as_str().to_ascii_uppercase()
                );
                let value = std::env::var(key)
                    .ok()
                    .and_then(|v| v.trim().parse::<f64>().ok())
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .unwrap_or(default);
                per_market_caps.insert((symbol.clone(), *timeframe), value.max(1.0));
            }
        }

        let max_flip_prob = env_f64("EVPOLY_EVCURVE_MAX_FLIP_PROB", 0.15).clamp(0.0, 1.0);
        let min_buy_price = env_f64("EVPOLY_EVCURVE_MIN_BUY_PRICE", 0.60).clamp(0.0, 1.0);

        Self {
            enable: env_bool("EVPOLY_STRATEGY_EVCURVE_ENABLE", true),
            d1_enable,
            poll_interval_ms: env_u64("EVPOLY_EVCURVE_POLL_MS", 250).max(100),
            symbols,
            timeframes,
            max_buy_curve: curve,
            per_symbol_max_buy_curve,
            m15_t2_no_fak_fallback: env_bool("EVPOLY_EVCURVE_15M_T2_NO_FAK", true),
            m15_t2_max_buy_curve,
            min_samples: env_u64("EVPOLY_EVCURVE_MIN_SAMPLES", 100) as u32,
            max_flip_prob,
            min_buy_price,
            base_anchor_grace_sec: env_i64("EVPOLY_EVCURVE_BASE_ANCHOR_GRACE_SEC", 2).max(0),
            tick_jitter_sec: env_i64("EVPOLY_EVCURVE_TICK_JITTER_SEC", 1).max(0),
            max_late_ms: env_i64("EVPOLY_EVCURVE_MAX_LATE_MS", 60_000).max(0),
            proxy_stale_ms: env_i64("EVPOLY_EVCURVE_PROXY_STALE_MS", 1_000).max(100),
            pm_quote_fresh_ms: env_i64("EVPOLY_EVCURVE_PM_QUOTE_FRESH_MS", 1_500).max(100),
            pm_quote_pair_max_skew_ms: env_i64("EVPOLY_EVCURVE_PM_QUOTE_PAIR_SKEW_MS", 800).max(0),
            strategy_cap_usd: env_f64("EVPOLY_EVCURVE_STRATEGY_CAP_USD", 10_000.0).max(1.0),
            d1_strategy_cap_usd: env_f64("EVPOLY_EVCURVE_D1_STRATEGY_CAP_USD", 10_000.0).max(1.0),
            d1_zero_min_n: env_u64("EVPOLY_EVCURVE_D1_ZERO_MIN_N", 10) as u32,
            d1_ev_min_n: env_u64("EVPOLY_EVCURVE_D1_EV_MIN_N", 150) as u32,
            d1_ev_gap: env_f64("EVPOLY_EVCURVE_D1_EV_GAP", 0.08).clamp(0.0, 1.0),
            base_size_usd,
            per_market_caps,
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

    pub fn scope_cap_usd(&self, symbol: &str, timeframe: Timeframe) -> f64 {
        let symbol = normalize_symbol(symbol);
        self.per_market_caps
            .get(&(symbol.clone(), timeframe))
            .copied()
            .unwrap_or_else(|| default_scope_cap(self.base_size_usd, symbol.as_str(), timeframe))
    }

    pub fn max_flip_prob_for_timeframe(&self, _timeframe: Timeframe) -> f64 {
        self.max_flip_prob
    }

    pub fn min_buy_price_for_timeframe(&self, _timeframe: Timeframe) -> f64 {
        self.min_buy_price
    }

    pub fn max_buy_curve_for_symbol(&self, symbol: &str) -> &MaxBuyCurve {
        let symbol_norm = normalize_symbol(symbol);
        self.per_symbol_max_buy_curve
            .get(symbol_norm.as_str())
            .unwrap_or(&self.max_buy_curve)
    }

    pub fn m15_t2_max_buy_hold(&self, p_flip: f64) -> Option<f64> {
        self.m15_t2_max_buy_curve
            .as_ref()
            .map(|curve| curve.evaluate(p_flip))
    }

    pub fn strategy_cap_for_timeframe(&self, timeframe: Timeframe) -> f64 {
        if matches!(timeframe, Timeframe::D1) {
            self.d1_strategy_cap_usd.max(1.0)
        } else {
            self.strategy_cap_usd.max(1.0)
        }
    }

    pub fn fixed_size_usd_for_timeframe(&self, timeframe: Timeframe) -> Option<f64> {
        let _ = timeframe;
        None
    }
}

#[derive(Debug, Clone)]
pub struct EvcurveDecision {
    pub should_buy: bool,
    pub skip_reason: Option<String>,
    pub hold_side: HoldSide,
    pub group_key: String,
    pub tau_sec: i64,
    pub base_mid: f64,
    pub current_mid: f64,
    pub lead_pct: f64,
    pub lead_bin_idx: Option<usize>,
    pub flips: Option<u32>,
    pub n: Option<u32>,
    pub p_flip: Option<f64>,
    pub p_hold: Option<f64>,
    pub max_buy_hold: Option<f64>,
    pub ask_up: Option<f64>,
    pub ask_down: Option<f64>,
    pub chosen_ask: Option<f64>,
    pub tau_low_sec: Option<i64>,
    pub tau_high_sec: Option<i64>,
}

impl EvcurveDecision {
    fn skip(
        reason: &str,
        hold_side: HoldSide,
        group_key: String,
        tau_sec: i64,
        base_mid: f64,
        current_mid: f64,
        lead_pct: f64,
        ask_up: Option<f64>,
        ask_down: Option<f64>,
    ) -> Self {
        Self {
            should_buy: false,
            skip_reason: Some(reason.to_string()),
            hold_side,
            group_key,
            tau_sec,
            base_mid,
            current_mid,
            lead_pct,
            lead_bin_idx: None,
            flips: None,
            n: None,
            p_flip: None,
            p_hold: None,
            max_buy_hold: None,
            ask_up,
            ask_down,
            chosen_ask: None,
            tau_low_sec: None,
            tau_high_sec: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvcurveDecisionCandidate {
    pub sub_strategy: String,
    pub decision: EvcurveDecision,
    pub score: f64,
    pub p_flip_market: Option<f64>,
    pub gap_abs: Option<f64>,
}

fn make_d1_skip_decision(
    reason: &str,
    hold_side: HoldSide,
    group_key: String,
    tau_sec: i64,
    base_mid: f64,
    current_mid: f64,
    lead_pct: f64,
    ask_up: Option<f64>,
    ask_down: Option<f64>,
    lookup: Option<PlanDailyLookupResult>,
) -> EvcurveDecision {
    let (lead_bin_idx, flips, n, p_flip, p_hold, tau_low_sec, tau_high_sec) = lookup
        .map(|v| {
            (
                Some(v.lead_bin_idx),
                Some(v.flips),
                Some(v.n),
                Some(v.p_flip),
                Some(v.p_hold),
                Some(v.tau_low_sec),
                Some(v.tau_high_sec),
            )
        })
        .unwrap_or((None, None, None, None, None, None, None));
    EvcurveDecision {
        should_buy: false,
        skip_reason: Some(reason.to_string()),
        hold_side,
        group_key,
        tau_sec,
        base_mid,
        current_mid,
        lead_pct,
        lead_bin_idx,
        flips,
        n,
        p_flip,
        p_hold,
        max_buy_hold: None,
        ask_up,
        ask_down,
        chosen_ask: None,
        tau_low_sec,
        tau_high_sec,
    }
}

pub fn evaluate_d1_candidates(
    cfg: &EvcurveExecutionConfig,
    tables: &PlanDailyTables,
    symbol: &str,
    period_open_ts: i64,
    tau_sec: i64,
    base_mid: f64,
    current_mid: f64,
    ask_up: Option<f64>,
    ask_down: Option<f64>,
    zero_rule_already_fired: bool,
    ev_rule_already_fired: bool,
) -> Vec<EvcurveDecisionCandidate> {
    let hold_side = if current_mid >= base_mid {
        HoldSide::Up
    } else {
        HoldSide::Down
    };
    let flip_side = match hold_side {
        HoldSide::Up => HoldSide::Down,
        HoldSide::Down => HoldSide::Up,
    };
    let lead_pct = ((current_mid - base_mid).abs() / base_mid.max(f64::EPSILON) * 100.0).max(0.0);
    let Some(group_key) = PlanDailyTables::group_key_for_period_open(period_open_ts) else {
        return vec![EvcurveDecisionCandidate {
            sub_strategy: "d1".to_string(),
            decision: make_d1_skip_decision(
                "invalid_period_open_ts",
                hold_side,
                "unknown".to_string(),
                tau_sec,
                base_mid,
                current_mid,
                lead_pct,
                ask_up,
                ask_down,
                None,
            ),
            score: 0.0,
            p_flip_market: None,
            gap_abs: None,
        }];
    };

    let lookup = tables.lookup(symbol, group_key.as_str(), tau_sec, lead_pct);
    let Some(base_lookup) = lookup.clone() else {
        return vec![EvcurveDecisionCandidate {
            sub_strategy: "d1".to_string(),
            decision: make_d1_skip_decision(
                "no_data",
                hold_side,
                group_key,
                tau_sec,
                base_mid,
                current_mid,
                lead_pct,
                ask_up,
                ask_down,
                None,
            ),
            score: 0.0,
            p_flip_market: None,
            gap_abs: None,
        }];
    };

    let hold_ask = match hold_side {
        HoldSide::Up => ask_up,
        HoldSide::Down => ask_down,
    }
    .filter(|v| v.is_finite() && *v > 0.0);
    let flip_ask = match flip_side {
        HoldSide::Up => ask_up,
        HoldSide::Down => ask_down,
    }
    .filter(|v| v.is_finite() && *v > 0.0);

    let mut out = Vec::new();

    if !zero_rule_already_fired {
        if base_lookup.flips == 0 && base_lookup.n >= cfg.d1_zero_min_n {
            match hold_ask {
                Some(ask) => {
                    let max_buy = 0.99_f64;
                    let should_buy = ask <= max_buy;
                    out.push(EvcurveDecisionCandidate {
                        sub_strategy: "d1_zero".to_string(),
                        decision: EvcurveDecision {
                            should_buy,
                            skip_reason: if should_buy {
                                None
                            } else {
                                Some("ask_too_high".to_string())
                            },
                            hold_side,
                            group_key: group_key.clone(),
                            tau_sec,
                            base_mid,
                            current_mid,
                            lead_pct,
                            lead_bin_idx: Some(base_lookup.lead_bin_idx),
                            flips: Some(base_lookup.flips),
                            n: Some(base_lookup.n),
                            p_flip: Some(base_lookup.p_flip),
                            p_hold: Some(base_lookup.p_hold),
                            max_buy_hold: Some(max_buy),
                            ask_up,
                            ask_down,
                            chosen_ask: Some(ask),
                            tau_low_sec: Some(base_lookup.tau_low_sec),
                            tau_high_sec: Some(base_lookup.tau_high_sec),
                        },
                        score: base_lookup.p_hold,
                        p_flip_market: flip_ask,
                        gap_abs: flip_ask.map(|mkt| (base_lookup.p_flip - mkt).abs()),
                    });
                }
                None => {
                    out.push(EvcurveDecisionCandidate {
                        sub_strategy: "d1_zero".to_string(),
                        decision: make_d1_skip_decision(
                            "book_empty",
                            hold_side,
                            group_key.clone(),
                            tau_sec,
                            base_mid,
                            current_mid,
                            lead_pct,
                            ask_up,
                            ask_down,
                            lookup.clone(),
                        ),
                        score: base_lookup.p_hold,
                        p_flip_market: flip_ask,
                        gap_abs: flip_ask.map(|mkt| (base_lookup.p_flip - mkt).abs()),
                    });
                }
            }
        }
    }

    if !ev_rule_already_fired {
        if base_lookup.n >= cfg.d1_ev_min_n {
            match flip_ask {
                Some(p_flip_market) => {
                    let gap_abs = (base_lookup.p_flip - p_flip_market).abs();
                    if gap_abs >= cfg.d1_ev_gap {
                        let buy_side = if base_lookup.p_flip > p_flip_market {
                            flip_side
                        } else {
                            hold_side
                        };
                        let chosen_ask = match buy_side {
                            HoldSide::Up => ask_up,
                            HoldSide::Down => ask_down,
                        }
                        .filter(|v| v.is_finite() && *v > 0.0);
                        if let Some(ask) = chosen_ask {
                            let max_buy = if buy_side == flip_side {
                                base_lookup.p_flip
                            } else {
                                base_lookup.p_hold
                            }
                            .clamp(0.01, 0.99);
                            let should_buy = ask <= max_buy;
                            let score = if buy_side == flip_side {
                                base_lookup.p_flip
                            } else {
                                base_lookup.p_hold
                            };
                            out.push(EvcurveDecisionCandidate {
                                sub_strategy: "d1_ev".to_string(),
                                decision: EvcurveDecision {
                                    should_buy,
                                    skip_reason: if should_buy {
                                        None
                                    } else {
                                        Some("ask_too_high".to_string())
                                    },
                                    hold_side: buy_side,
                                    group_key: group_key.clone(),
                                    tau_sec,
                                    base_mid,
                                    current_mid,
                                    lead_pct,
                                    lead_bin_idx: Some(base_lookup.lead_bin_idx),
                                    flips: Some(base_lookup.flips),
                                    n: Some(base_lookup.n),
                                    p_flip: Some(base_lookup.p_flip),
                                    p_hold: Some(base_lookup.p_hold),
                                    max_buy_hold: Some(max_buy),
                                    ask_up,
                                    ask_down,
                                    chosen_ask: Some(ask),
                                    tau_low_sec: Some(base_lookup.tau_low_sec),
                                    tau_high_sec: Some(base_lookup.tau_high_sec),
                                },
                                score,
                                p_flip_market: Some(p_flip_market),
                                gap_abs: Some(gap_abs),
                            });
                        } else {
                            out.push(EvcurveDecisionCandidate {
                                sub_strategy: "d1_ev".to_string(),
                                decision: make_d1_skip_decision(
                                    "book_empty",
                                    hold_side,
                                    group_key.clone(),
                                    tau_sec,
                                    base_mid,
                                    current_mid,
                                    lead_pct,
                                    ask_up,
                                    ask_down,
                                    lookup.clone(),
                                ),
                                score: base_lookup.p_hold,
                                p_flip_market: Some(p_flip_market),
                                gap_abs: Some(gap_abs),
                            });
                        }
                    } else {
                        out.push(EvcurveDecisionCandidate {
                            sub_strategy: "d1_ev".to_string(),
                            decision: make_d1_skip_decision(
                                "gap_too_small",
                                hold_side,
                                group_key.clone(),
                                tau_sec,
                                base_mid,
                                current_mid,
                                lead_pct,
                                ask_up,
                                ask_down,
                                lookup.clone(),
                            ),
                            score: base_lookup.p_hold,
                            p_flip_market: Some(p_flip_market),
                            gap_abs: Some(gap_abs),
                        });
                    }
                }
                None => {
                    out.push(EvcurveDecisionCandidate {
                        sub_strategy: "d1_ev".to_string(),
                        decision: make_d1_skip_decision(
                            "book_empty",
                            hold_side,
                            group_key.clone(),
                            tau_sec,
                            base_mid,
                            current_mid,
                            lead_pct,
                            ask_up,
                            ask_down,
                            lookup.clone(),
                        ),
                        score: base_lookup.p_hold,
                        p_flip_market: None,
                        gap_abs: None,
                    });
                }
            }
        } else {
            out.push(EvcurveDecisionCandidate {
                sub_strategy: "d1_ev".to_string(),
                decision: make_d1_skip_decision(
                    "n_too_small",
                    hold_side,
                    group_key.clone(),
                    tau_sec,
                    base_mid,
                    current_mid,
                    lead_pct,
                    ask_up,
                    ask_down,
                    lookup.clone(),
                ),
                score: base_lookup.p_hold,
                p_flip_market: flip_ask,
                gap_abs: flip_ask.map(|mkt| (base_lookup.p_flip - mkt).abs()),
            });
        }
    }

    if out.is_empty() {
        out.push(EvcurveDecisionCandidate {
            sub_strategy: "d1".to_string(),
            decision: make_d1_skip_decision(
                "rule_not_triggered",
                hold_side,
                group_key.clone(),
                tau_sec,
                base_mid,
                current_mid,
                lead_pct,
                ask_up,
                ask_down,
                lookup.clone(),
            ),
            score: base_lookup.p_hold,
            p_flip_market: flip_ask,
            gap_abs: flip_ask.map(|mkt| (base_lookup.p_flip - mkt).abs()),
        });
    }

    out
}

pub fn checkpoints_for_timeframe(timeframe: Timeframe) -> &'static [i64] {
    match timeframe {
        Timeframe::M5 => &[],
        Timeframe::M15 => &[300, 240, 180],
        Timeframe::H1 => &[900, 600, 300],
        Timeframe::H4 => &[7200, 6300, 5400, 4500, 3600, 2700, 1800, 900],
        Timeframe::D1 => &[
            57_600, 54_000, 50_400, 46_800, 43_200, 39_600, 36_000, 32_400, 28_800, 25_200, 21_600,
            18_000, 14_400, 10_800, 7_200, 3_600,
        ],
    }
}

pub fn order_should_use_fak(timeframe: Timeframe, checkpoint_offset_sec: i64) -> bool {
    match timeframe {
        Timeframe::M5 => true,
        Timeframe::M15 => checkpoint_offset_sec <= 60,
        Timeframe::H1 => checkpoint_offset_sec <= 60,
        Timeframe::H4 | Timeframe::D1 => false,
    }
}

pub fn evaluate_decision(
    cfg: &EvcurveExecutionConfig,
    tables: &Plan3Tables,
    timeframe: Timeframe,
    symbol: &str,
    period_open_ts: i64,
    tau_sec: i64,
    base_mid: f64,
    current_mid: f64,
    ask_up: Option<f64>,
    ask_down: Option<f64>,
) -> EvcurveDecision {
    let hold_side = if current_mid >= base_mid {
        HoldSide::Up
    } else {
        HoldSide::Down
    };
    let lead_pct = ((current_mid - base_mid).abs() / base_mid.max(f64::EPSILON) * 100.0).max(0.0);

    let Some(group_key) = Plan3Tables::group_key_for_period_open(timeframe, period_open_ts) else {
        return EvcurveDecision::skip(
            "invalid_period_open_ts",
            hold_side,
            "unknown".to_string(),
            tau_sec,
            base_mid,
            current_mid,
            lead_pct,
            ask_up,
            ask_down,
        );
    };

    let lookup = tables.lookup(timeframe, symbol, group_key.as_str(), tau_sec, lead_pct);
    let Some(Plan3LookupResult {
        flips,
        n,
        p_flip,
        p_hold,
        lead_bin_idx,
        tau_low_sec,
        tau_high_sec,
    }) = lookup
    else {
        return EvcurveDecision::skip(
            "no_data",
            hold_side,
            group_key,
            tau_sec,
            base_mid,
            current_mid,
            lead_pct,
            ask_up,
            ask_down,
        );
    };

    if n < cfg.min_samples {
        return EvcurveDecision {
            should_buy: false,
            skip_reason: Some("n_too_small".to_string()),
            hold_side,
            group_key,
            tau_sec,
            base_mid,
            current_mid,
            lead_pct,
            lead_bin_idx: Some(lead_bin_idx),
            flips: Some(flips),
            n: Some(n),
            p_flip: Some(p_flip),
            p_hold: Some(p_hold),
            max_buy_hold: None,
            ask_up,
            ask_down,
            chosen_ask: None,
            tau_low_sec: Some(tau_low_sec),
            tau_high_sec: Some(tau_high_sec),
        };
    }

    if p_flip > cfg.max_flip_prob_for_timeframe(timeframe) {
        return EvcurveDecision {
            should_buy: false,
            skip_reason: Some("flip_prob_above_cap".to_string()),
            hold_side,
            group_key,
            tau_sec,
            base_mid,
            current_mid,
            lead_pct,
            lead_bin_idx: Some(lead_bin_idx),
            flips: Some(flips),
            n: Some(n),
            p_flip: Some(p_flip),
            p_hold: Some(p_hold),
            max_buy_hold: Some(0.0),
            ask_up,
            ask_down,
            chosen_ask: None,
            tau_low_sec: Some(tau_low_sec),
            tau_high_sec: Some(tau_high_sec),
        };
    }

    let max_buy_hold = cfg.max_buy_curve_for_symbol(symbol).evaluate(p_flip);
    if max_buy_hold <= 0.0 {
        return EvcurveDecision {
            should_buy: false,
            skip_reason: Some("curve_zero".to_string()),
            hold_side,
            group_key,
            tau_sec,
            base_mid,
            current_mid,
            lead_pct,
            lead_bin_idx: Some(lead_bin_idx),
            flips: Some(flips),
            n: Some(n),
            p_flip: Some(p_flip),
            p_hold: Some(p_hold),
            max_buy_hold: Some(max_buy_hold),
            ask_up,
            ask_down,
            chosen_ask: None,
            tau_low_sec: Some(tau_low_sec),
            tau_high_sec: Some(tau_high_sec),
        };
    }

    let chosen_ask = match hold_side {
        HoldSide::Up => ask_up,
        HoldSide::Down => ask_down,
    };
    let Some(ask) = chosen_ask.filter(|v| v.is_finite() && *v > 0.0) else {
        return EvcurveDecision {
            should_buy: false,
            skip_reason: Some("book_empty".to_string()),
            hold_side,
            group_key,
            tau_sec,
            base_mid,
            current_mid,
            lead_pct,
            lead_bin_idx: Some(lead_bin_idx),
            flips: Some(flips),
            n: Some(n),
            p_flip: Some(p_flip),
            p_hold: Some(p_hold),
            max_buy_hold: Some(max_buy_hold),
            ask_up,
            ask_down,
            chosen_ask,
            tau_low_sec: Some(tau_low_sec),
            tau_high_sec: Some(tau_high_sec),
        };
    };

    if ask < cfg.min_buy_price_for_timeframe(timeframe) {
        return EvcurveDecision {
            should_buy: false,
            skip_reason: Some("ask_below_min_buy_price".to_string()),
            hold_side,
            group_key,
            tau_sec,
            base_mid,
            current_mid,
            lead_pct,
            lead_bin_idx: Some(lead_bin_idx),
            flips: Some(flips),
            n: Some(n),
            p_flip: Some(p_flip),
            p_hold: Some(p_hold),
            max_buy_hold: Some(max_buy_hold),
            ask_up,
            ask_down,
            chosen_ask: Some(ask),
            tau_low_sec: Some(tau_low_sec),
            tau_high_sec: Some(tau_high_sec),
        };
    }

    let should_buy = ask <= max_buy_hold;
    EvcurveDecision {
        should_buy,
        skip_reason: if should_buy {
            None
        } else {
            Some("ask_too_high".to_string())
        },
        hold_side,
        group_key,
        tau_sec,
        base_mid,
        current_mid,
        lead_pct,
        lead_bin_idx: Some(lead_bin_idx),
        flips: Some(flips),
        n: Some(n),
        p_flip: Some(p_flip),
        p_hold: Some(p_hold),
        max_buy_hold: Some(max_buy_hold),
        ask_up,
        ask_down,
        chosen_ask: Some(ask),
        tau_low_sec: Some(tau_low_sec),
        tau_high_sec: Some(tau_high_sec),
    }
}

#[derive(Debug, Clone, Default)]
pub struct CheckpointDedupe {
    fired: HashSet<(String, Timeframe, i64, i64)>,
}

impl CheckpointDedupe {
    pub fn mark_if_new(
        &mut self,
        symbol: &str,
        timeframe: Timeframe,
        period_open_ts: i64,
        checkpoint_offset_sec: i64,
    ) -> bool {
        self.fired.insert((
            normalize_symbol(symbol),
            timeframe,
            period_open_ts,
            checkpoint_offset_sec,
        ))
    }

    pub fn prune(&mut self, now_ts: i64) {
        self.fired.retain(|(_, timeframe, period_open_ts, _)| {
            let ttl = timeframe.duration_seconds().saturating_mul(3);
            period_open_ts.saturating_add(ttl) >= now_ts
        });
    }
}

pub fn should_trigger_checkpoint(
    now_ts: i64,
    close_ts: i64,
    checkpoint_offset_sec: i64,
    max_late_ms: i64,
) -> bool {
    let deadline_ts = close_ts.saturating_sub(checkpoint_offset_sec);
    if now_ts < deadline_ts {
        return false;
    }
    if max_late_ms <= 0 {
        return true;
    }
    let late_ms = now_ts
        .saturating_sub(deadline_ts)
        .saturating_mul(1_000)
        .max(0);
    late_ms <= max_late_ms
}

fn default_scope_cap(base_size_usd: f64, symbol: &str, timeframe: Timeframe) -> f64 {
    let tf_multiplier = size_policy::evcurve_timeframe_multiplier(timeframe);
    if tf_multiplier <= 0.0 {
        return 0.0;
    }
    let symbol_size = size_policy::strategy_symbol_size_usd(base_size_usd, symbol);
    if symbol_size <= 0.0 {
        return 0.0;
    }
    (symbol_size * tf_multiplier).max(1.0)
}

fn parse_symbols_env(key: &str, defaults: &[&str]) -> Vec<String> {
    let parsed = std::env::var(key)
        .ok()
        .map(|raw| {
            crate::symbol_ownership::parse_symbols_csv_for_strategy(STRATEGY_ID_EVCURVE_V1, &raw)
        })
        .unwrap_or_default();
    if parsed.is_empty() {
        let defaults = defaults
            .iter()
            .map(|v| normalize_symbol(v))
            .collect::<Vec<_>>();
        crate::symbol_ownership::filter_symbols_for_strategy(STRATEGY_ID_EVCURVE_V1, &defaults)
    } else {
        parsed
    }
}

fn parse_timeframes_env(key: &str, defaults: &[Timeframe]) -> Vec<Timeframe> {
    let parsed = std::env::var(key)
        .ok()
        .map(|raw| {
            raw.split(',')
                .filter_map(|part| parse_timeframe(part.trim()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if parsed.is_empty() {
        defaults.to_vec()
    } else {
        parsed
    }
}

fn parse_timeframe(raw: &str) -> Option<Timeframe> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "15m" => Some(Timeframe::M15),
        "1h" | "60m" => Some(Timeframe::H1),
        "4h" | "240m" => Some(Timeframe::H4),
        "1d" | "d1" | "24h" | "daily" => Some(Timeframe::D1),
        _ => None,
    }
}

fn normalize_symbol(raw: &str) -> String {
    match raw.trim().to_ascii_uppercase().as_str() {
        "BTC" => "BTC".to_string(),
        "ETH" => "ETH".to_string(),
        "SOL" | "SOLANA" => "SOL".to_string(),
        "XRP" => "XRP".to_string(),
        other => other.to_string(),
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
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

    #[test]
    fn curve_interpolation_matches_expected_values() {
        let curve = MaxBuyCurve::from_csv("0:0.99,0.01:0.97,0.02:0.94,0.03:0.0").expect("curve");
        assert!((curve.evaluate(0.00) - 0.99).abs() <= 1e-9);
        assert!((curve.evaluate(0.01) - 0.97).abs() <= 1e-9);
        assert!((curve.evaluate(0.015) - 0.955).abs() <= 1e-9);
        assert!((curve.evaluate(0.03) - 0.0).abs() <= 1e-9);
        assert!((curve.evaluate(0.04) - 0.0).abs() <= 1e-9);
    }

    #[test]
    fn decision_gate_buy_vs_skip() {
        let cfg = EvcurveExecutionConfig {
            enable: true,
            d1_enable: false,
            poll_interval_ms: 250,
            symbols: vec!["BTC".to_string()],
            timeframes: vec![Timeframe::M15],
            max_buy_curve: MaxBuyCurve::from_csv("0:0.99,0.10:0.90").expect("curve"),
            per_symbol_max_buy_curve: HashMap::new(),
            m15_t2_no_fak_fallback: false,
            m15_t2_max_buy_curve: None,
            min_samples: 10,
            max_flip_prob: 0.10,
            min_buy_price: 0.60,
            base_anchor_grace_sec: 2,
            tick_jitter_sec: 1,
            max_late_ms: 2_000,
            proxy_stale_ms: 1_000,
            pm_quote_fresh_ms: 1_500,
            pm_quote_pair_max_skew_ms: 800,
            strategy_cap_usd: 10_000.0,
            d1_strategy_cap_usd: 5_000.0,
            d1_zero_min_n: 10,
            d1_ev_min_n: 150,
            d1_ev_gap: 0.15,
            base_size_usd: 100.0,
            per_market_caps: HashMap::new(),
        };
        let tables = Plan3Tables::from_markdown(
            r#"
### BTC — Opening (:00-:15, 100 windows)
| Lead \ Time | T-5 | T-1 |
|---|---|---|
| 0.0-0.5% | 1/100 | 1/100 |
"#,
        )
        .expect("inline plan3");

        // 2026-02-24 13:00:00 UTC => 08:00:00 ET (Opening window)
        let period_open_ts = 1_771_938_800_i64;

        let buy = evaluate_decision(
            &cfg,
            &tables,
            Timeframe::M15,
            "BTC",
            period_open_ts,
            300,
            100.0,
            100.2,
            Some(0.95),
            Some(0.40),
        );
        assert!(buy.should_buy);

        let skip = evaluate_decision(
            &cfg,
            &tables,
            Timeframe::M15,
            "BTC",
            period_open_ts,
            300,
            100.0,
            100.2,
            Some(0.995),
            Some(0.40),
        );
        assert!(!skip.should_buy);
        assert_eq!(skip.skip_reason.as_deref(), Some("ask_too_high"));
    }

    #[test]
    fn decision_uses_proxy_direction_for_hold_side() {
        let cfg = EvcurveExecutionConfig {
            enable: true,
            d1_enable: false,
            poll_interval_ms: 250,
            symbols: vec!["BTC".to_string()],
            timeframes: vec![Timeframe::M15],
            max_buy_curve: MaxBuyCurve::from_csv("0:0.99,0.10:0.90").expect("curve"),
            per_symbol_max_buy_curve: HashMap::new(),
            m15_t2_no_fak_fallback: false,
            m15_t2_max_buy_curve: None,
            min_samples: 10,
            max_flip_prob: 0.10,
            min_buy_price: 0.60,
            base_anchor_grace_sec: 2,
            tick_jitter_sec: 1,
            max_late_ms: 2_000,
            proxy_stale_ms: 1_000,
            pm_quote_fresh_ms: 1_500,
            pm_quote_pair_max_skew_ms: 800,
            strategy_cap_usd: 10_000.0,
            d1_strategy_cap_usd: 5_000.0,
            d1_zero_min_n: 10,
            d1_ev_min_n: 150,
            d1_ev_gap: 0.15,
            base_size_usd: 100.0,
            per_market_caps: HashMap::new(),
        };
        let tables = Plan3Tables::from_markdown(
            r#"
### BTC — Opening (:00-:15, 100 windows)
| Lead \ Time | T-5 | T-1 |
|---|---|---|
| 0.0-0.5% | 1/100 | 1/100 |
"#,
        )
        .expect("inline plan3");

        let period_open_ts = 1_771_938_800_i64;

        let skip = evaluate_decision(
            &cfg,
            &tables,
            Timeframe::M15,
            "BTC",
            period_open_ts,
            300,
            100.0,
            99.8,
            Some(0.95),
            Some(0.70),
        );
        assert!(skip.should_buy);
        assert_eq!(skip.hold_side, HoldSide::Down);
        assert_eq!(skip.chosen_ask, Some(0.70));
    }

    #[test]
    fn decision_skips_when_ask_below_min_buy_price() {
        let cfg = EvcurveExecutionConfig {
            enable: true,
            d1_enable: false,
            poll_interval_ms: 250,
            symbols: vec!["BTC".to_string()],
            timeframes: vec![Timeframe::M15],
            max_buy_curve: MaxBuyCurve::from_csv("0:0.99,0.10:0.90").expect("curve"),
            per_symbol_max_buy_curve: HashMap::new(),
            m15_t2_no_fak_fallback: false,
            m15_t2_max_buy_curve: None,
            min_samples: 10,
            max_flip_prob: 0.10,
            min_buy_price: 0.60,
            base_anchor_grace_sec: 2,
            tick_jitter_sec: 1,
            max_late_ms: 2_000,
            proxy_stale_ms: 1_000,
            pm_quote_fresh_ms: 1_500,
            pm_quote_pair_max_skew_ms: 800,
            strategy_cap_usd: 10_000.0,
            d1_strategy_cap_usd: 5_000.0,
            d1_zero_min_n: 10,
            d1_ev_min_n: 150,
            d1_ev_gap: 0.15,
            base_size_usd: 100.0,
            per_market_caps: HashMap::new(),
        };
        let tables = Plan3Tables::from_markdown(
            r#"
### BTC — Opening (:00-:15, 100 windows)
| Lead \ Time | T-5 | T-1 |
|---|---|---|
| 0.0-0.5% | 1/100 | 1/100 |
"#,
        )
        .expect("inline plan3");

        let period_open_ts = 1_771_938_800_i64;
        let skip = evaluate_decision(
            &cfg,
            &tables,
            Timeframe::M15,
            "BTC",
            period_open_ts,
            300,
            100.0,
            100.2,
            Some(0.55),
            Some(0.35),
        );
        assert!(!skip.should_buy);
        assert_eq!(skip.skip_reason.as_deref(), Some("ask_below_min_buy_price"));
    }

    #[test]
    fn checkpoint_dedupe_blocks_duplicates() {
        let mut dedupe = CheckpointDedupe::default();
        assert!(dedupe.mark_if_new("BTC", Timeframe::H1, 1_700_000_000, 900));
        assert!(!dedupe.mark_if_new("BTC", Timeframe::H1, 1_700_000_000, 900));
    }

    #[test]
    fn scope_cap_uses_base_symbol_and_timeframe_policy() {
        unsafe {
            std::env::set_var("EVPOLY_EVCURVE_BASE_SIZE_USD", "100");
            std::env::remove_var("EVPOLY_EVCURVE_SCOPE_CAP_BTC_M15");
            std::env::remove_var("EVPOLY_EVCURVE_SCOPE_CAP_ETH_H1");
            std::env::remove_var("EVPOLY_EVCURVE_SCOPE_CAP_SOL_H4");
            std::env::remove_var("EVPOLY_EVCURVE_SCOPE_CAP_BTC_1D");
        }
        let cfg = EvcurveExecutionConfig::from_env();
        assert!((cfg.scope_cap_usd("BTC", Timeframe::M15) - 75.0).abs() < 1e-9);
        assert!((cfg.scope_cap_usd("ETH", Timeframe::H1) - 80.0).abs() < 1e-9);
        assert!((cfg.scope_cap_usd("SOL", Timeframe::H4) - 62.5).abs() < 1e-9);
        assert!((cfg.scope_cap_usd("BTC", Timeframe::D1) - 125.0).abs() < 1e-9);
        unsafe { std::env::remove_var("EVPOLY_EVCURVE_BASE_SIZE_USD") };
    }
}
