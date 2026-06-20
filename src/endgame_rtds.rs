use crate::event_log::log_event;
use crate::strategy::{Direction, STRATEGY_ID_ENDGAME_SWEEP_V1};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct EndgameRtdsGuardConfig {
    pub enabled: bool,
    pub min_bps: f64,
    pub stale_ms: i64,
    pub no_guard_extra_bps: f64,
    pub history_keep_ms: i64,
    pub reconnect_ms: u64,
}

impl EndgameRtdsGuardConfig {
    pub fn from_env() -> Self {
        Self {
            enabled: env_bool("EVPOLY_ENDGAME_RTDS_GUARD_ENABLE", true),
            min_bps: env_f64("EVPOLY_ENDGAME_RTDS_GUARD_MIN_BPS", 0.5, 0.0, 100.0),
            stale_ms: env_i64("EVPOLY_ENDGAME_RTDS_GUARD_STALE_MS", 5_000, 250, 60_000),
            no_guard_extra_bps: env_f64("EVPOLY_ENDGAME_RTDS_NO_GUARD_EXTRA_BPS", 1.5, 0.0, 100.0),
            history_keep_ms: env_i64(
                "EVPOLY_ENDGAME_RTDS_HISTORY_KEEP_MS",
                6 * 3_600_000,
                60_000,
                24 * 3_600_000,
            ),
            reconnect_ms: env_u64("EVPOLY_ENDGAME_RTDS_RECONNECT_MS", 1_000, 100, 60_000),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EndgameRtdsPriceSample {
    pub source_ts_ms: i64,
    pub recv_ts_ms: i64,
    pub price: f64,
}

#[derive(Debug, Clone)]
pub struct EndgameRtdsPriceState {
    pub symbol: String,
    pub price: Option<f64>,
    pub source_ts_ms: Option<i64>,
    pub recv_ts_ms: Option<i64>,
    pub recent_samples: VecDeque<EndgameRtdsPriceSample>,
    pub last_error: Option<String>,
}

pub type SharedEndgameRtdsPriceState = Arc<StdRwLock<EndgameRtdsPriceState>>;
pub type EndgameRtdsPriceStates = Arc<HashMap<String, SharedEndgameRtdsPriceState>>;

#[derive(Debug, Clone)]
pub struct EndgameRtdsGuardDecision {
    pub enabled: bool,
    pub ready: bool,
    pub skip_reason: Option<&'static str>,
    pub no_guard_extra_bps: f64,
    pub payload: Value,
}

pub fn new_rtds_price_states(symbols: &[String]) -> EndgameRtdsPriceStates {
    let mut states = HashMap::new();
    for symbol in symbols {
        let normalized = normalize_symbol(symbol);
        if normalized.is_empty() || states.contains_key(normalized.as_str()) {
            continue;
        }
        states.insert(
            normalized.clone(),
            Arc::new(StdRwLock::new(EndgameRtdsPriceState {
                symbol: normalized,
                price: None,
                source_ts_ms: None,
                recv_ts_ms: None,
                recent_samples: VecDeque::new(),
                last_error: None,
            })),
        );
    }
    Arc::new(states)
}

pub fn spawn_rtds_chainlink_feed(states: EndgameRtdsPriceStates, cfg: EndgameRtdsGuardConfig) {
    if !cfg.enabled {
        log_event(
            "endgame_rtds_guard_feed_started",
            json!({
                "strategy_id": STRATEGY_ID_ENDGAME_SWEEP_V1,
                "enabled": false
            }),
        );
        return;
    }
    let symbols = states.keys().cloned().collect::<Vec<_>>();
    tokio::spawn(async move {
        log_event(
            "endgame_rtds_guard_feed_started",
            json!({
                "strategy_id": STRATEGY_ID_ENDGAME_SWEEP_V1,
                "enabled": true,
                "source": "polymarket_rtds_crypto_prices_chainlink",
                "symbols": symbols,
                "stale_ms": cfg.stale_ms,
                "min_bps": cfg.min_bps,
                "no_guard_extra_bps": cfg.no_guard_extra_bps,
                "history_keep_ms": cfg.history_keep_ms
            }),
        );
        loop {
            let client = polymarket_client_sdk_v2::rtds::Client::default();
            let stream = match client.subscribe_chainlink_prices(None) {
                Ok(stream) => stream,
                Err(err) => {
                    record_feed_error(states.as_ref(), format!("subscribe_failed:{err}"));
                    log_event(
                        "endgame_rtds_guard_feed_error",
                        json!({
                            "strategy_id": STRATEGY_ID_ENDGAME_SWEEP_V1,
                            "error": err.to_string(),
                            "phase": "subscribe"
                        }),
                    );
                    tokio::time::sleep(Duration::from_millis(cfg.reconnect_ms)).await;
                    continue;
                }
            };
            let mut stream = Box::pin(stream);
            while let Some(message) = stream.next().await {
                match message {
                    Ok(price) => {
                        let normalized = normalize_symbol(price.symbol.as_str());
                        let Some(state) = states.get(normalized.as_str()) else {
                            continue;
                        };
                        let value = price.value.to_string().parse::<f64>().ok();
                        let Some(value) = value.filter(|v| v.is_finite() && *v > 0.0) else {
                            continue;
                        };
                        let source_ts_ms = price.timestamp;
                        if source_ts_ms <= 0 {
                            continue;
                        }
                        let recv_ts_ms = chrono::Utc::now().timestamp_millis();
                        if let Ok(mut guard) = state.write() {
                            guard.symbol = normalized;
                            guard.price = Some(value);
                            guard.source_ts_ms = Some(source_ts_ms);
                            guard.recv_ts_ms = Some(recv_ts_ms);
                            guard.last_error = None;
                            guard.recent_samples.push_back(EndgameRtdsPriceSample {
                                source_ts_ms,
                                recv_ts_ms,
                                price: value,
                            });
                            let keep_after_ms = recv_ts_ms.saturating_sub(cfg.history_keep_ms);
                            while guard
                                .recent_samples
                                .front()
                                .map(|sample| sample.source_ts_ms < keep_after_ms)
                                .unwrap_or(false)
                            {
                                guard.recent_samples.pop_front();
                            }
                        }
                    }
                    Err(err) => {
                        record_feed_error(states.as_ref(), format!("stream_error:{err}"));
                        log_event(
                            "endgame_rtds_guard_feed_error",
                            json!({
                                "strategy_id": STRATEGY_ID_ENDGAME_SWEEP_V1,
                                "error": err.to_string(),
                                "phase": "stream"
                            }),
                        );
                    }
                }
            }
            record_feed_error(states.as_ref(), "stream_closed".to_string());
            tokio::time::sleep(Duration::from_millis(cfg.reconnect_ms)).await;
        }
    });
}

pub fn evaluate_rtds_guard(
    cfg: &EndgameRtdsGuardConfig,
    states: &EndgameRtdsPriceStates,
    symbol: &str,
    timeframe: &str,
    market_open_ts: i64,
    market_close_ts: i64,
    tau_sec: i64,
    expected_direction: Direction,
    eval_now_ms: i64,
) -> EndgameRtdsGuardDecision {
    let expected_direction_label = direction_label(expected_direction);
    if !cfg.enabled {
        return EndgameRtdsGuardDecision {
            enabled: false,
            ready: false,
            skip_reason: None,
            no_guard_extra_bps: 0.0,
            payload: json!({
                "enabled": false,
                "decision_effect": "disabled",
                "source": "polymarket_rtds_crypto_prices_chainlink",
                "guard_expected_direction": expected_direction_label
            }),
        };
    }

    let normalized = normalize_symbol(symbol);
    let mut cache_busy = false;
    let snapshot = match states.get(normalized.as_str()) {
        Some(state) => match state.try_read() {
            Ok(snapshot) => Some(snapshot.clone()),
            Err(_) => {
                cache_busy = true;
                None
            }
        },
        None => None,
    };
    let history = snapshot
        .as_ref()
        .map(|snapshot| snapshot.recent_samples.clone())
        .unwrap_or_default();
    let open_source_ts_ms = market_open_ts.saturating_mul(1_000);
    let base_sample = history
        .iter()
        .find(|sample| sample.source_ts_ms == open_source_ts_ms)
        .copied();
    let current_sample = snapshot.as_ref().and_then(|snapshot| {
        snapshot
            .price
            .zip(snapshot.source_ts_ms)
            .zip(snapshot.recv_ts_ms)
            .map(
                |((price, source_ts_ms), recv_ts_ms)| EndgameRtdsPriceSample {
                    source_ts_ms,
                    recv_ts_ms,
                    price,
                },
            )
            .filter(|sample| {
                sample.source_ts_ms > 0 && sample.price.is_finite() && sample.price > 0.0
            })
            .or_else(|| latest_sample(&snapshot.recent_samples))
    });
    let recv_age_ms = snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.recv_ts_ms)
        .map(|recv_ts_ms| eval_now_ms.saturating_sub(recv_ts_ms).max(0));
    let current_source_age_ms = current_sample
        .as_ref()
        .map(|sample| eval_now_ms.saturating_sub(sample.source_ts_ms).max(0));
    let stale_limit_ms = cfg.stale_ms.max(1);
    let stale = recv_age_ms
        .map(|age_ms| age_ms > stale_limit_ms)
        .unwrap_or(true)
        || current_source_age_ms
            .map(|age_ms| age_ms > stale_limit_ms)
            .unwrap_or(true);
    let move_bps = base_sample
        .as_ref()
        .zip(current_sample.as_ref())
        .and_then(|(base, current)| {
            if base.price.is_finite() && base.price > 0.0 && current.price.is_finite() {
                Some(((current.price / base.price) - 1.0) * 10_000.0)
            } else {
                None
            }
        });
    let direction = move_bps.map(|bps| {
        if bps > 0.0 {
            "UP"
        } else if bps < 0.0 {
            "DOWN"
        } else {
            "FLAT"
        }
    });
    let ready = snapshot.is_some()
        && base_sample.is_some()
        && current_sample.is_some()
        && !stale
        && !cache_busy
        && snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.last_error.as_ref())
            .is_none();
    let not_ready_reason = if snapshot.is_none() {
        if cache_busy {
            "rtds_cache_busy"
        } else {
            "rtds_state_missing"
        }
    } else if base_sample.is_none() {
        "rtds_exact_open_base_missing"
    } else if current_sample.is_none() {
        "rtds_current_missing"
    } else if stale {
        "rtds_stale"
    } else if snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.last_error.as_ref())
        .is_some()
    {
        "rtds_feed_error"
    } else {
        "not_ready"
    };
    let mut skip_reason = None;
    if ready {
        if direction.map(|actual| actual.eq_ignore_ascii_case(expected_direction_label))
            != Some(true)
        {
            skip_reason = Some("rtds_guard_direction_mismatch");
        } else if move_bps
            .map(|bps| bps.abs() + f64::EPSILON < cfg.min_bps)
            .unwrap_or(true)
        {
            skip_reason = Some("rtds_guard_below_min_bps");
        }
    }
    let no_guard_extra_bps = if ready { 0.0 } else { cfg.no_guard_extra_bps };
    EndgameRtdsGuardDecision {
        enabled: true,
        ready,
        skip_reason,
        no_guard_extra_bps,
        payload: json!({
            "enabled": true,
            "ready": ready,
            "decision_effect": if ready { "hard_guard" } else { "no_guard_distance_surcharge" },
            "source": "polymarket_rtds_crypto_prices_chainlink",
            "reason": if ready { "ready" } else { not_ready_reason },
            "symbol": normalized,
            "state_symbol": snapshot.as_ref().map(|snapshot| snapshot.symbol.as_str()),
            "timeframe": timeframe,
            "market_open_ts": market_open_ts,
            "market_close_ts": market_close_ts,
            "tau_sec": tau_sec,
            "base_required_source_ts_ms": open_source_ts_ms,
            "base_source": base_sample.as_ref().map(|_| "polymarket_rtds_exact_open"),
            "base_exact_rtds_found": base_sample.is_some(),
            "base_source_ts_ms": base_sample.as_ref().map(|sample| sample.source_ts_ms),
            "base_price": base_sample.as_ref().map(|sample| sample.price),
            "current_source_ts_ms": current_sample.as_ref().map(|sample| sample.source_ts_ms),
            "current_recv_ts_ms": current_sample.as_ref().map(|sample| sample.recv_ts_ms),
            "current_price": current_sample.as_ref().map(|sample| sample.price),
            "recv_age_ms": recv_age_ms,
            "current_source_age_ms": current_source_age_ms,
            "stale": stale,
            "stale_ms": stale_limit_ms,
            "move_bps": move_bps,
            "abs_move_bps": move_bps.map(|bps| bps.abs()),
            "direction": direction,
            "recent_samples": history.len(),
            "last_error": snapshot.as_ref().and_then(|snapshot| snapshot.last_error.clone()),
            "guard_enabled": true,
            "guard_min_bps": cfg.min_bps,
            "guard_expected_direction": expected_direction_label,
            "guard_pass": ready && skip_reason.is_none(),
            "guard_skip_reason": skip_reason.map(Value::from).unwrap_or(Value::Null),
            "no_guard_extra_bps": no_guard_extra_bps
        }),
    }
}

fn latest_sample(samples: &VecDeque<EndgameRtdsPriceSample>) -> Option<EndgameRtdsPriceSample> {
    samples
        .iter()
        .rev()
        .find(|sample| sample.source_ts_ms > 0 && sample.price.is_finite() && sample.price > 0.0)
        .copied()
}

fn record_feed_error(states: &HashMap<String, SharedEndgameRtdsPriceState>, error: String) {
    let now_ms = chrono::Utc::now().timestamp_millis();
    for state in states.values() {
        if let Ok(mut guard) = state.write() {
            guard.last_error = Some(error.clone());
            guard.recv_ts_ms = Some(now_ms);
        }
    }
}

fn direction_label(direction: Direction) -> &'static str {
    match direction {
        Direction::Up => "UP",
        Direction::Down => "DOWN",
    }
}

fn normalize_symbol(symbol: &str) -> String {
    symbol
        .trim()
        .split(['/', '-', '_'])
        .next()
        .unwrap_or(symbol)
        .trim_end_matches("USDT")
        .trim_end_matches("USD")
        .to_ascii_uppercase()
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn env_f64(name: &str, default: f64, min: f64, max: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(default)
        .clamp(min, max)
}

fn env_i64(name: &str, default: i64, min: i64, max: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

fn env_u64(name: &str, default: u64, min: u64, max: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_samples() -> EndgameRtdsPriceStates {
        let mut states = HashMap::new();
        let mut recent_samples = VecDeque::new();
        recent_samples.push_back(EndgameRtdsPriceSample {
            source_ts_ms: 10_000,
            recv_ts_ms: 10_000,
            price: 100.0,
        });
        recent_samples.push_back(EndgameRtdsPriceSample {
            source_ts_ms: 19_900,
            recv_ts_ms: 19_900,
            price: 100.02,
        });
        states.insert(
            "BTC".to_string(),
            Arc::new(StdRwLock::new(EndgameRtdsPriceState {
                symbol: "BTC".to_string(),
                price: Some(100.02),
                source_ts_ms: Some(19_900),
                recv_ts_ms: Some(19_900),
                recent_samples,
                last_error: None,
            })),
        );
        Arc::new(states)
    }

    #[test]
    fn rtds_ready_but_below_min_bps_skips() {
        let cfg = EndgameRtdsGuardConfig {
            enabled: true,
            min_bps: 3.0,
            stale_ms: 5_000,
            no_guard_extra_bps: 1.5,
            history_keep_ms: 60_000,
            reconnect_ms: 1_000,
        };
        let decision = evaluate_rtds_guard(
            &cfg,
            &state_with_samples(),
            "BTC",
            "5m",
            10,
            20,
            1,
            Direction::Up,
            20_000,
        );
        assert!(decision.ready);
        assert_eq!(decision.skip_reason, Some("rtds_guard_below_min_bps"));
        assert_eq!(decision.no_guard_extra_bps, 0.0);
    }

    #[test]
    fn rtds_missing_base_adds_surcharge_without_hard_skip() {
        let cfg = EndgameRtdsGuardConfig {
            enabled: true,
            min_bps: 0.5,
            stale_ms: 5_000,
            no_guard_extra_bps: 1.5,
            history_keep_ms: 60_000,
            reconnect_ms: 1_000,
        };
        let decision = evaluate_rtds_guard(
            &cfg,
            &state_with_samples(),
            "BTC",
            "5m",
            11,
            20,
            1,
            Direction::Up,
            20_000,
        );
        assert!(!decision.ready);
        assert_eq!(decision.skip_reason, None);
        assert_eq!(decision.no_guard_extra_bps, 1.5);
    }
}
