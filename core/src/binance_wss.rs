use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use serde::Deserialize;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::event_log::log_event;
use crate::microflow::{MicroflowConfig, MicroflowEngine};
use crate::signal_state::{trim_vec, SharedSignalState, WhaleTradeEvent};
use serde_json::json;

static BINANCE_PARSE_ERROR_COUNT: AtomicU64 = AtomicU64::new(0);
const BINANCE_FLOW_WINDOWS_SECONDS: [u32; 9] = [15, 30, 60, 180, 300, 600, 900, 1800, 3600];
const BINANCE_BURST_BUCKET_SEC: i64 = 15;
const BINANCE_BURST_HISTORY_BUCKETS: usize = 60;

#[derive(Debug, Clone)]
pub struct BinanceWssConfig {
    pub stream_url: String,
    pub reconnect_min_sec: u64,
    pub reconnect_max_sec: u64,
    pub whale_trade_usd: f64,
    pub high_whale_trade_usd: f64,
    pub whale_cluster_window_sec: i64,
    pub whale_event_queue_cap: usize,
}

impl Default for BinanceWssConfig {
    fn default() -> Self {
        let whale_trade_usd = env_f64("EVPOLY_BINANCE_WHALE_TRADE_USD", 250_000.0);
        let high_whale_trade_usd =
            env_f64("EVPOLY_BINANCE_HIGH_WHALE_TRADE_USD", 750_000.0).max(whale_trade_usd);
        let whale_cluster_window_sec = env_i64("EVPOLY_BINANCE_WHALE_WINDOW_SEC", 300);
        let whale_event_queue_cap = std::env::var("EVPOLY_BINANCE_WHALE_EVENT_QUEUE_CAP")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(2_000)
            .max(128);
        Self {
            stream_url: "wss://stream.binance.com:9443/ws/btcusdt@trade".to_string(),
            reconnect_min_sec: 1,
            reconnect_max_sec: 20,
            whale_trade_usd,
            high_whale_trade_usd,
            whale_cluster_window_sec,
            whale_event_queue_cap,
        }
    }
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(default)
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

#[derive(Debug, Deserialize)]
struct BinanceTradeMsg {
    #[serde(rename = "p")]
    price: String,
    #[serde(rename = "q")]
    qty: String,
    #[serde(rename = "m")]
    buyer_is_maker: bool,
    #[serde(rename = "T")]
    trade_time_ms: i64,
}

#[derive(Debug, Clone, Copy)]
struct MinuteBucket {
    minute_ts: i64,
    abs_notional: f64,
}

#[derive(Debug, Clone, Copy)]
struct SecondBucket {
    second_ts: i64,
    signed_notional: f64,
    buy_abs: f64,
    sell_abs: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct WindowStats {
    signed: f64,
    buy_abs: f64,
    sell_abs: f64,
}

#[derive(Debug, Clone, Default)]
struct BinanceFlowMetrics {
    signed_notional_15s: Option<f64>,
    signed_notional_30s: Option<f64>,
    signed_notional_60s: Option<f64>,
    signed_notional_180s: Option<f64>,
    signed_notional_300s: Option<f64>,
    signed_notional_600s: Option<f64>,
    signed_notional_900s: Option<f64>,
    signed_notional_1800s: Option<f64>,
    signed_notional_3600s: Option<f64>,
    side_imbalance_5m: Option<f64>,
    flow_persistence: Option<f64>,
    burst_zscore: Option<f64>,
}

#[derive(Debug)]
struct BinanceRuntime {
    microflow: MicroflowEngine,
    whale_events_30s: VecDeque<WhaleTradeEvent>,
    minute_buckets: VecDeque<MinuteBucket>,
    second_buckets: VecDeque<SecondBucket>,
}

impl BinanceRuntime {
    fn new() -> Self {
        Self {
            microflow: MicroflowEngine::new(MicroflowConfig::default()),
            whale_events_30s: VecDeque::new(),
            minute_buckets: VecDeque::new(),
            second_buckets: VecDeque::new(),
        }
    }

    fn update_minute_buckets(&mut self, ts_ms: i64, abs_notional: f64) -> f64 {
        let minute_ts = (ts_ms / 60_000) * 60_000;
        if let Some(last) = self.minute_buckets.back_mut() {
            if last.minute_ts == minute_ts {
                last.abs_notional += abs_notional;
            } else {
                self.minute_buckets.push_back(MinuteBucket {
                    minute_ts,
                    abs_notional,
                });
            }
        } else {
            self.minute_buckets.push_back(MinuteBucket {
                minute_ts,
                abs_notional,
            });
        }

        while self.minute_buckets.len() > 31 {
            self.minute_buckets.pop_front();
        }

        // Use rolling 60s notional instead of "current wall-clock minute" bucket.
        // This avoids artificial near-zero ratios at minute boundaries.
        let now_second = ts_ms / 1000;
        let current_1m = self
            .second_buckets
            .iter()
            .filter(|bucket| bucket.second_ts > now_second - 60)
            .map(|bucket| bucket.buy_abs + bucket.sell_abs)
            .sum::<f64>();
        if self.minute_buckets.len() <= 1 {
            return 0.0;
        }

        let mut baseline_sum = 0.0_f64;
        let mut baseline_n = 0_u32;
        for bucket in self
            .minute_buckets
            .iter()
            .take(self.minute_buckets.len().saturating_sub(1))
        {
            baseline_sum += bucket.abs_notional;
            baseline_n += 1;
        }

        if baseline_n == 0 {
            return 0.0;
        }
        let baseline = baseline_sum / baseline_n as f64;
        if baseline <= f64::EPSILON {
            return 0.0;
        }
        current_1m / baseline
    }

    fn update_second_buckets(&mut self, ts_ms: i64, signed_notional: f64, abs_notional: f64) {
        let second_ts = ts_ms / 1000;
        if let Some(last) = self.second_buckets.back_mut() {
            if last.second_ts == second_ts {
                last.signed_notional += signed_notional;
                if signed_notional >= 0.0 {
                    last.buy_abs += abs_notional;
                } else {
                    last.sell_abs += abs_notional;
                }
            } else {
                self.second_buckets.push_back(SecondBucket {
                    second_ts,
                    signed_notional,
                    buy_abs: if signed_notional >= 0.0 {
                        abs_notional
                    } else {
                        0.0
                    },
                    sell_abs: if signed_notional < 0.0 {
                        abs_notional
                    } else {
                        0.0
                    },
                });
            }
        } else {
            self.second_buckets.push_back(SecondBucket {
                second_ts,
                signed_notional,
                buy_abs: if signed_notional >= 0.0 {
                    abs_notional
                } else {
                    0.0
                },
                sell_abs: if signed_notional < 0.0 {
                    abs_notional
                } else {
                    0.0
                },
            });
        }

        while self.second_buckets.len() > 3_700 {
            self.second_buckets.pop_front();
        }
        while self
            .second_buckets
            .front()
            .map(|bucket| bucket.second_ts < second_ts - 3_600)
            .unwrap_or(false)
        {
            self.second_buckets.pop_front();
        }
    }

    fn compute_flow_metrics(&self, ts_ms: i64) -> BinanceFlowMetrics {
        let now_second = ts_ms / 1000;
        let mut window_stats = [WindowStats::default(); BINANCE_FLOW_WINDOWS_SECONDS.len()];
        for bucket in &self.second_buckets {
            let age_sec = now_second.saturating_sub(bucket.second_ts);
            if age_sec < 0 {
                continue;
            }
            for (idx, window_seconds) in BINANCE_FLOW_WINDOWS_SECONDS.iter().enumerate() {
                if age_sec > *window_seconds as i64 {
                    continue;
                }
                window_stats[idx].signed += bucket.signed_notional;
                window_stats[idx].buy_abs += bucket.buy_abs;
                window_stats[idx].sell_abs += bucket.sell_abs;
            }
        }

        let signed_for = |seconds: u32| -> Option<f64> {
            let idx = BINANCE_FLOW_WINDOWS_SECONDS
                .iter()
                .position(|window| *window == seconds)?;
            let value = window_stats[idx].signed;
            if value.abs() <= f64::EPSILON {
                None
            } else {
                Some(value)
            }
        };
        let side_imbalance_5m = BINANCE_FLOW_WINDOWS_SECONDS
            .iter()
            .position(|window| *window == 300)
            .and_then(|idx| {
                let denom = window_stats[idx].buy_abs + window_stats[idx].sell_abs;
                if denom > 0.0 {
                    Some(
                        ((window_stats[idx].buy_abs - window_stats[idx].sell_abs) / denom)
                            .clamp(-1.0, 1.0),
                    )
                } else {
                    None
                }
            });
        let flow_persistence = {
            let mut weighted = 0.0_f64;
            let mut weight_sum = 0.0_f64;
            for seconds in [15_u32, 30, 60, 180, 300] {
                let Some(idx) = BINANCE_FLOW_WINDOWS_SECONDS
                    .iter()
                    .position(|window| *window == seconds)
                else {
                    continue;
                };
                let signed = window_stats[idx].signed;
                if signed.abs() <= f64::EPSILON {
                    continue;
                }
                let weight = signed.abs().sqrt().max(1.0);
                weighted += signed.signum() * weight;
                weight_sum += weight;
            }
            if weight_sum > 0.0 {
                Some((weighted / weight_sum).clamp(-1.0, 1.0))
            } else {
                None
            }
        };

        let burst_zscore = compute_burst_zscore(&self.second_buckets, now_second);

        BinanceFlowMetrics {
            signed_notional_15s: signed_for(15),
            signed_notional_30s: signed_for(30),
            signed_notional_60s: signed_for(60),
            signed_notional_180s: signed_for(180),
            signed_notional_300s: signed_for(300),
            signed_notional_600s: signed_for(600),
            signed_notional_900s: signed_for(900),
            signed_notional_1800s: signed_for(1800),
            signed_notional_3600s: signed_for(3600),
            side_imbalance_5m,
            flow_persistence,
            burst_zscore,
        }
    }
}

fn compute_burst_zscore(second_buckets: &VecDeque<SecondBucket>, now_second: i64) -> Option<f64> {
    if second_buckets.is_empty() {
        return None;
    }
    let latest_bucket = now_second / BINANCE_BURST_BUCKET_SEC;
    let oldest_bucket = latest_bucket.saturating_sub(BINANCE_BURST_HISTORY_BUCKETS as i64 - 1);
    let mut buckets: std::collections::BTreeMap<i64, f64> = std::collections::BTreeMap::new();
    for bucket in second_buckets {
        let burst_bucket = bucket.second_ts / BINANCE_BURST_BUCKET_SEC;
        if burst_bucket < oldest_bucket || burst_bucket > latest_bucket {
            continue;
        }
        *buckets.entry(burst_bucket).or_insert(0.0) += bucket.signed_notional;
    }

    let mut values = Vec::with_capacity(BINANCE_BURST_HISTORY_BUCKETS);
    for bucket in oldest_bucket..=latest_bucket {
        values.push(*buckets.get(&bucket).unwrap_or(&0.0));
    }
    if values.len() < 5 {
        return None;
    }
    let latest = *values.last().unwrap_or(&0.0);
    let baseline = &values[..values.len() - 1];
    let mean = baseline.iter().sum::<f64>() / baseline.len() as f64;
    let variance = baseline
        .iter()
        .map(|value| {
            let d = *value - mean;
            d * d
        })
        .sum::<f64>()
        / baseline.len() as f64;
    let std_dev = variance.sqrt();
    if std_dev <= f64::EPSILON {
        let delta = latest - mean;
        if delta.abs() <= f64::EPSILON {
            return Some(0.0);
        }
        return Some(if delta > 0.0 { 6.0 } else { -6.0 });
    }
    Some((latest - mean) / std_dev)
}

pub fn spawn_binance_trade_feed(
    cfg: BinanceWssConfig,
    state: SharedSignalState,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = cfg.reconnect_min_sec.max(1);
        let mut runtime = BinanceRuntime::new();
        let mut consecutive_failures: u32 = 0;

        loop {
            match connect_async(&cfg.stream_url).await {
                Ok((mut ws_stream, _)) => {
                    consecutive_failures = 0;
                    crate::log_println!("📡 Binance WSS connected: {}", cfg.stream_url);
                    log_event(
                        "binance_connected",
                        json!({
                            "stream_url": cfg.stream_url,
                            "backoff_sec": backoff
                        }),
                    );
                    backoff = cfg.reconnect_min_sec.max(1);
                    let connected_at_ms = chrono::Utc::now().timestamp_millis();
                    let inactivity_timeout_sec =
                        std::env::var("EVPOLY_BINANCE_INACTIVITY_TIMEOUT_SEC")
                            .ok()
                            .and_then(|v| v.parse::<u64>().ok())
                            .unwrap_or(30)
                            .max(5);
                    let flush_interval_ms = std::env::var("EVPOLY_BINANCE_MICROFLOW_FLUSH_MS")
                        .ok()
                        .and_then(|v| v.parse::<i64>().ok())
                        .unwrap_or(1_000)
                        .max(250);
                    let mut last_flush_ms = chrono::Utc::now().timestamp_millis();

                    loop {
                        let now_ms = chrono::Utc::now().timestamp_millis();
                        if now_ms.saturating_sub(last_flush_ms) >= flush_interval_ms {
                            if let Some(event) = runtime.microflow.flush_now(now_ms, "binance") {
                                let mut guard = state.write().await;
                                guard.abnormal_events.push(event);
                                trim_vec(&mut guard.abnormal_events, 128);
                                guard.abnormal_updated_at_ms = now_ms;
                                guard.updated_at_ms = guard.newest_source_update_ms();
                            }
                            last_flush_ms = now_ms;
                        }

                        let next_msg = tokio::time::timeout(
                            Duration::from_secs(inactivity_timeout_sec),
                            ws_stream.next(),
                        )
                        .await;
                        let Some(msg) = (match next_msg {
                            Ok(v) => v,
                            Err(_) => {
                                log_event(
                                    "binance_inactivity_timeout",
                                    json!({
                                        "timeout_sec": inactivity_timeout_sec
                                    }),
                                );
                                break;
                            }
                        }) else {
                            break;
                        };
                        match msg {
                            Ok(Message::Text(text)) => {
                                if let Ok(trade_msg) =
                                    serde_json::from_str::<BinanceTradeMsg>(&text)
                                {
                                    process_trade(&cfg, &state, &mut runtime, trade_msg).await;
                                } else {
                                    let c = BINANCE_PARSE_ERROR_COUNT
                                        .fetch_add(1, Ordering::Relaxed)
                                        + 1;
                                    if c == 1 || c % 100 == 0 {
                                        log_event("binance_parse_error", json!({"count": c}));
                                    }
                                }
                            }
                            Ok(Message::Ping(_))
                            | Ok(Message::Pong(_))
                            | Ok(Message::Binary(_)) => {}
                            Ok(Message::Close(_)) => break,
                            Ok(_) => {}
                            Err(err) => {
                                crate::log_println!("⚠️ Binance WSS read error: {}", err);
                                log_event(
                                    "binance_read_error",
                                    json!({
                                        "error": err.to_string()
                                    }),
                                );
                                break;
                            }
                        }
                    }

                    let lived_ms = chrono::Utc::now()
                        .timestamp_millis()
                        .saturating_sub(connected_at_ms);
                    log_event(
                        "binance_disconnected",
                        json!({
                            "connected_lifetime_ms": lived_ms
                        }),
                    );
                }
                Err(err) => {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    crate::log_println!(
                        "⚠️ Binance WSS connect failed: {} (retry in {}s)",
                        err,
                        backoff
                    );
                    log_event(
                        "binance_connect_failed",
                        json!({
                            "error": err.to_string(),
                            "retry_in_sec": backoff,
                            "consecutive_failures": consecutive_failures
                        }),
                    );
                }
            }

            sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(cfg.reconnect_max_sec.max(backoff));
        }
    })
}

async fn process_trade(
    cfg: &BinanceWssConfig,
    state: &SharedSignalState,
    runtime: &mut BinanceRuntime,
    trade_msg: BinanceTradeMsg,
) {
    let price = trade_msg.price.parse::<f64>().ok();
    let qty = trade_msg.qty.parse::<f64>().ok();
    let (price, qty) = match (price, qty) {
        (Some(p), Some(q)) if p > 0.0 && q > 0.0 => (p, q),
        _ => return,
    };

    let notional_usd = price * qty;
    let signed_notional = if trade_msg.buyer_is_maker {
        -notional_usd
    } else {
        notional_usd
    };
    let side = if signed_notional >= 0.0 {
        "BUY"
    } else {
        "SELL"
    };

    let abnormal =
        runtime
            .microflow
            .ingest_trade(trade_msg.trade_time_ms, signed_notional, "binance");
    runtime.update_second_buckets(trade_msg.trade_time_ms, signed_notional, notional_usd.abs());
    let vol_ratio = runtime.update_minute_buckets(trade_msg.trade_time_ms, notional_usd.abs());
    let flow_metrics = runtime.compute_flow_metrics(trade_msg.trade_time_ms);

    let whale_event = if notional_usd >= cfg.whale_trade_usd {
        Some(WhaleTradeEvent {
            source: "binance".to_string(),
            side: side.to_string(),
            timestamp_ms: trade_msg.trade_time_ms,
            notional_usd,
            high_severity: notional_usd >= cfg.high_whale_trade_usd,
        })
    } else {
        None
    };

    if let Some(event) = whale_event.clone() {
        runtime.whale_events_30s.push_back(event);
    }
    let cutoff_ts = trade_msg.trade_time_ms - (cfg.whale_cluster_window_sec * 1000);
    while runtime
        .whale_events_30s
        .front()
        .map(|e| e.timestamp_ms < cutoff_ts)
        .unwrap_or(false)
    {
        runtime.whale_events_30s.pop_front();
    }
    while runtime.whale_events_30s.len() > cfg.whale_event_queue_cap {
        runtime.whale_events_30s.pop_front();
    }

    let mut whale_buys = 0_u32;
    let mut whale_sells = 0_u32;
    let mut whale_net_usd = 0.0_f64;
    for event in &runtime.whale_events_30s {
        if event.side == "BUY" {
            whale_buys += 1;
            whale_net_usd += event.notional_usd;
        } else {
            whale_sells += 1;
            whale_net_usd -= event.notional_usd;
        }
    }

    let mut guard = state.write().await;
    let now_ms = chrono::Utc::now().timestamp_millis();
    guard.binance_flow.last_price = Some(price);
    guard.binance_flow.last_trade_ts_ms = Some(trade_msg.trade_time_ms);
    guard.binance_flow.volume_ratio_1m = Some(vol_ratio);
    guard.binance_flow.signed_notional_15s = flow_metrics.signed_notional_15s;
    guard.binance_flow.signed_notional_30s = flow_metrics.signed_notional_30s;
    guard.binance_flow.signed_notional_60s = flow_metrics.signed_notional_60s;
    guard.binance_flow.signed_notional_180s = flow_metrics.signed_notional_180s;
    guard.binance_flow.signed_notional_300s = flow_metrics.signed_notional_300s;
    guard.binance_flow.signed_notional_600s = flow_metrics.signed_notional_600s;
    guard.binance_flow.signed_notional_900s = flow_metrics.signed_notional_900s;
    guard.binance_flow.signed_notional_1800s = flow_metrics.signed_notional_1800s;
    guard.binance_flow.signed_notional_3600s = flow_metrics.signed_notional_3600s;
    guard.binance_flow.side_imbalance_5m = flow_metrics.side_imbalance_5m;
    guard.binance_flow.flow_persistence = flow_metrics.flow_persistence;
    guard.binance_flow.burst_zscore = flow_metrics.burst_zscore;
    guard.binance_flow.whale_buys_5m = whale_buys;
    guard.binance_flow.whale_sells_5m = whale_sells;
    guard.binance_flow.whale_net_usd_5m = whale_net_usd;
    guard.binance_flow_updated_at_ms = now_ms;

    if let Some(event) = abnormal {
        guard.abnormal_events.push(event);
        trim_vec(&mut guard.abnormal_events, 128);
        guard.abnormal_updated_at_ms = now_ms;
    }
    if let Some(event) = whale_event {
        guard.whale_events.push(event);
        trim_vec(&mut guard.whale_events, 256);
        guard.whale_updated_at_ms = now_ms;
    }
    guard.updated_at_ms = guard.newest_source_update_ms();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(left: f64, right: f64, tol: f64) {
        assert!(
            (left - right).abs() <= tol,
            "left={} right={} tol={}",
            left,
            right,
            tol
        );
    }

    #[test]
    fn volume_ratio_uses_rolling_60s_abs_notional() {
        let mut runtime = BinanceRuntime::new();
        let ts_ms = (10 * 60_000) + 2_000;
        let now_second = ts_ms / 1000;

        for minute in 0..10_i64 {
            runtime.minute_buckets.push_back(MinuteBucket {
                minute_ts: minute * 60_000,
                abs_notional: 6_000.0,
            });
        }

        for sec in (now_second - 59)..=now_second {
            runtime.second_buckets.push_back(SecondBucket {
                second_ts: sec,
                signed_notional: 100.0,
                buy_abs: 100.0,
                sell_abs: 0.0,
            });
        }

        let ratio = runtime.update_minute_buckets(ts_ms, 100.0);
        approx_eq(ratio, 1.0, 1e-9);
    }

    #[test]
    fn volume_ratio_requires_baseline_history() {
        let mut runtime = BinanceRuntime::new();
        let ts_ms = (5 * 60_000) + 10_000;
        let now_second = ts_ms / 1000;

        for sec in (now_second - 59)..=now_second {
            runtime.second_buckets.push_back(SecondBucket {
                second_ts: sec,
                signed_notional: 50.0,
                buy_abs: 50.0,
                sell_abs: 0.0,
            });
        }

        let ratio = runtime.update_minute_buckets(ts_ms, 50.0);
        approx_eq(ratio, 0.0, 1e-9);
    }
}
