use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use crate::event_log::log_event;
use crate::microflow::{MicroflowConfig, MicroflowEngine};
use crate::signal_state::{
    trim_vec, SharedSignalState, SrLevelsSnapshot, TrendStateSnapshot, VolumeProfileSnapshot,
};

static HL_FILE_WATCHER_ERROR_COUNT: AtomicU64 = AtomicU64::new(0);
static HL_L4_READ_ERROR_COUNT: AtomicU64 = AtomicU64::new(0);

const HL_FLOW_WINDOWS_SECONDS: [u32; 9] = [15, 30, 60, 180, 300, 600, 900, 1800, 3600];
const BURST_BUCKET_MS: i64 = 15_000;
const BURST_HISTORY_BUCKETS: usize = 60; // 15 minutes in 15s buckets.

#[derive(Debug, Clone, Copy, Default)]
struct L4Sample {
    ts_ms: i64,
    signed_notional_usd: f64,
    notional_usd: f64,
    is_liquidation: bool,
    is_large_trade: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct WindowStats {
    sample_count: u32,
    signed: f64,
    liq_signed: f64,
    total_abs: f64,
    large_abs: f64,
    buy_abs: f64,
    sell_abs: f64,
}

#[derive(Debug, Clone, Default)]
struct HlFlowDerivedMetrics {
    signed_notional_15s: Option<f64>,
    signed_notional_30s: Option<f64>,
    signed_notional_60s: Option<f64>,
    signed_notional_180s: Option<f64>,
    signed_notional_300s: Option<f64>,
    signed_notional_600s: Option<f64>,
    signed_notional_900s: Option<f64>,
    signed_notional_1800s: Option<f64>,
    signed_notional_3600s: Option<f64>,
    cvd_slope: Option<f64>,
    cvd_acceleration: Option<f64>,
    burst_zscore: Option<f64>,
    liquidation_pressure_5m: Option<f64>,
    liquidation_impulse: Option<f64>,
    large_trade_concentration: Option<f64>,
    side_imbalance: Option<f64>,
    flow_persistence: Option<f64>,
    reversal_risk_score: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct HlSignalsConfig {
    pub signals_dir: PathBuf,
    pub l4_hourly_dir: PathBuf,
    pub file_poll_interval_ms: u64,
    pub l4_poll_interval_ms: u64,
    pub large_trade_threshold_usd: f64,
}

impl Default for HlSignalsConfig {
    fn default() -> Self {
        Self {
            signals_dir: PathBuf::from(
                std::env::var("EVPOLY_HL_SIGNALS_DIR")
                    .unwrap_or_else(|_| "/root/clawd/skills/hl-trader/signals".to_string()),
            ),
            l4_hourly_dir: PathBuf::from(
                std::env::var("EVPOLY_HL_L4_HOURLY_DIR")
                    .unwrap_or_else(|_| "/root/hl/data/node_fills_by_block/hourly".to_string()),
            ),
            file_poll_interval_ms: 500,
            l4_poll_interval_ms: 300,
            large_trade_threshold_usd: 250_000.0,
        }
    }
}

pub fn spawn_hl_signal_pipeline(
    cfg: HlSignalsConfig,
    state: SharedSignalState,
) -> Vec<tokio::task::JoinHandle<()>> {
    let file_cfg = cfg.clone();
    let file_state = state.clone();
    let h1 = tokio::spawn(async move {
        run_signal_file_watcher(file_cfg, file_state).await;
    });

    let l4_cfg = cfg;
    let l4_state = state;
    let h2 = tokio::spawn(async move {
        run_l4_fill_tailer(l4_cfg, l4_state).await;
    });

    vec![h1, h2]
}

async fn run_signal_file_watcher(cfg: HlSignalsConfig, state: SharedSignalState) {
    let trend_path = cfg.signals_dir.join("trend_state.json");
    let sr_path = cfg.signals_dir.join("sr_levels.json");
    let vp_path = cfg.signals_dir.join("volume_profile.json");

    let mut mtimes: HashMap<PathBuf, SystemTime> = HashMap::new();
    let mut interval = tokio::time::interval(Duration::from_millis(cfg.file_poll_interval_ms));

    crate::log_println!(
        "🧠 HL signal file watcher started (dir={})",
        cfg.signals_dir.display()
    );

    loop {
        interval.tick().await;

        let mut updates: Vec<(PathBuf, Value)> = Vec::new();
        for path in [&trend_path, &sr_path, &vp_path] {
            if let Some(modified) = get_file_modified(path).await {
                let changed = match mtimes.get(path) {
                    Some(prev) => *prev < modified,
                    None => true,
                };
                if changed {
                    match tokio::fs::read_to_string(path).await {
                        Ok(raw) => {
                            if let Ok(json) = serde_json::from_str::<Value>(&raw) {
                                updates.push((path.clone(), json));
                                mtimes.insert(path.clone(), modified);
                            } else {
                                let c =
                                    HL_FILE_WATCHER_ERROR_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                                if c == 1 || c % 100 == 0 {
                                    log_event(
                                        "hl_signal_file_parse_error",
                                        json!({"path": path.display().to_string(), "count": c}),
                                    );
                                }
                            }
                        }
                        Err(_) => {
                            let c = HL_FILE_WATCHER_ERROR_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                            if c == 1 || c % 100 == 0 {
                                log_event(
                                    "hl_signal_file_read_error",
                                    json!({"path": path.display().to_string(), "count": c}),
                                );
                            }
                        }
                    }
                }
            }
        }

        if updates.is_empty() {
            continue;
        }

        let mut guard = state.write().await;
        let now_ms = chrono::Utc::now().timestamp_millis();
        for (path, json) in updates {
            if path == trend_path {
                guard.trend = parse_trend_snapshot(&json);
                guard.trend_updated_at_ms = now_ms;
            } else if path == sr_path {
                guard.sr_levels = parse_sr_snapshot(&json);
                guard.sr_levels_updated_at_ms = now_ms;
            } else if path == vp_path {
                guard.volume_profile = parse_volume_profile_snapshot(&json);
                guard.volume_profile_updated_at_ms = now_ms;
            }
        }
        guard.updated_at_ms = guard.newest_source_update_ms();
    }
}

async fn run_l4_fill_tailer(cfg: HlSignalsConfig, state: SharedSignalState) {
    let mut current_file: Option<PathBuf> = None;
    let mut current_pos: u64 = 0;
    let mut fill_window: VecDeque<L4Sample> = VecDeque::new();
    let mut microflow = MicroflowEngine::new(MicroflowConfig::default());
    let mut interval = tokio::time::interval(Duration::from_millis(cfg.l4_poll_interval_ms));
    let max_lines_per_tick = std::env::var("EVPOLY_HL_L4_MAX_LINES_PER_TICK")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20_000)
        .max(100);
    let max_bytes_per_tick = std::env::var("EVPOLY_HL_L4_MAX_BYTES_PER_TICK")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(8 * 1024 * 1024)
        .max(4096);

    crate::log_println!(
        "📥 HL L4 tailer started (dir={})",
        cfg.l4_hourly_dir.display()
    );

    loop {
        interval.tick().await;

        let latest = latest_file_in_dir(&cfg.l4_hourly_dir).await;
        if latest.is_none() {
            continue;
        }
        let latest = latest.unwrap();

        if current_file.as_ref() != Some(&latest) {
            current_file = Some(latest.clone());
            current_pos = 0;
            crate::log_println!("📄 Switched HL L4 file: {}", latest.display());
        }

        let file_path = latest.clone();
        let read_res = tokio::task::spawn_blocking(move || {
            read_new_lines(
                &file_path,
                current_pos,
                max_lines_per_tick,
                max_bytes_per_tick,
            )
        })
        .await;
        let (new_pos, lines) = match read_res {
            Ok(Ok(v)) => v,
            _ => {
                let c = HL_L4_READ_ERROR_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                if c == 1 || c % 100 == 0 {
                    log_event("hl_l4_read_error", json!({"count": c}));
                }
                continue;
            }
        };
        current_pos = new_pos;

        if lines.is_empty() {
            continue;
        }

        let mut latest_price = None;
        let mut latest_ts = None;
        let mut last_large_trade = None;
        let mut abnormal_events = Vec::new();

        for line in lines {
            for fill in parse_fill_line(&line) {
                if !is_btc_symbol(&fill.symbol) {
                    continue;
                }

                latest_price = Some(fill.price);
                latest_ts = Some(fill.ts_ms);

                let is_large_trade = fill.notional_usd >= cfg.large_trade_threshold_usd;
                fill_window.push_back(L4Sample {
                    ts_ms: fill.ts_ms,
                    signed_notional_usd: fill.signed_notional_usd,
                    notional_usd: fill.notional_usd,
                    is_liquidation: fill.is_liquidation,
                    is_large_trade,
                });
                while fill_window
                    .front()
                    .map(|sample| sample.ts_ms < fill.ts_ms - 3_600_000)
                    .unwrap_or(false)
                {
                    fill_window.pop_front();
                }

                if is_large_trade {
                    last_large_trade = Some(fill.notional_usd);
                }

                if let Some(event) =
                    microflow.ingest_trade(fill.ts_ms, fill.signed_notional_usd, "hl_l4")
                {
                    abnormal_events.push(event);
                }
            }
        }

        if latest_price.is_none() && abnormal_events.is_empty() {
            continue;
        }

        let anchor_ts_ms = latest_ts.unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        let derived = compute_hl_flow_metrics(&fill_window, anchor_ts_ms);

        let mut guard = state.write().await;
        let now_ms = chrono::Utc::now().timestamp_millis();
        if let Some(price) = latest_price {
            guard.hl_flow.last_price = Some(price);
        }
        if let Some(ts) = latest_ts {
            guard.hl_flow.last_fill_ts_ms = Some(ts);
        }
        guard.hl_flow.cvd_5m = derived.signed_notional_300s;
        guard.hl_flow.liquidation_pressure_5m = derived.liquidation_pressure_5m;
        guard.hl_flow.signed_notional_15s = derived.signed_notional_15s;
        guard.hl_flow.signed_notional_30s = derived.signed_notional_30s;
        guard.hl_flow.signed_notional_60s = derived.signed_notional_60s;
        guard.hl_flow.signed_notional_180s = derived.signed_notional_180s;
        guard.hl_flow.signed_notional_300s = derived.signed_notional_300s;
        guard.hl_flow.signed_notional_600s = derived.signed_notional_600s;
        guard.hl_flow.signed_notional_900s = derived.signed_notional_900s;
        guard.hl_flow.signed_notional_1800s = derived.signed_notional_1800s;
        guard.hl_flow.signed_notional_3600s = derived.signed_notional_3600s;
        guard.hl_flow.cvd_slope = derived.cvd_slope;
        guard.hl_flow.cvd_acceleration = derived.cvd_acceleration;
        guard.hl_flow.burst_zscore = derived.burst_zscore;
        guard.hl_flow.liquidation_impulse = derived.liquidation_impulse;
        guard.hl_flow.large_trade_concentration = derived.large_trade_concentration;
        guard.hl_flow.side_imbalance = derived.side_imbalance;
        guard.hl_flow.flow_persistence = derived.flow_persistence;
        guard.hl_flow.reversal_risk_score = derived.reversal_risk_score;
        if let Some(large) = last_large_trade {
            guard.hl_flow.last_large_trade_usd = Some(large);
        }
        guard.hl_flow_updated_at_ms = now_ms;
        if !abnormal_events.is_empty() {
            guard.abnormal_events.extend(abnormal_events);
            trim_vec(&mut guard.abnormal_events, 128);
            guard.abnormal_updated_at_ms = now_ms;
        }
        guard.updated_at_ms = guard.newest_source_update_ms();
    }
}

fn compute_hl_flow_metrics(
    samples: &VecDeque<L4Sample>,
    anchor_ts_ms: i64,
) -> HlFlowDerivedMetrics {
    let mut window_stats = [WindowStats::default(); HL_FLOW_WINDOWS_SECONDS.len()];
    for sample in samples {
        let age_ms = anchor_ts_ms.saturating_sub(sample.ts_ms);
        for (idx, window_seconds) in HL_FLOW_WINDOWS_SECONDS.iter().enumerate() {
            if age_ms > (*window_seconds as i64) * 1000 {
                continue;
            }
            let stats = &mut window_stats[idx];
            stats.sample_count = stats.sample_count.saturating_add(1);
            stats.signed += sample.signed_notional_usd;
            if sample.is_liquidation {
                stats.liq_signed += sample.signed_notional_usd;
            }
            let abs_notional = sample.notional_usd.abs();
            stats.total_abs += abs_notional;
            if sample.is_large_trade {
                stats.large_abs += abs_notional;
            }
            if sample.signed_notional_usd >= 0.0 {
                stats.buy_abs += abs_notional;
            } else {
                stats.sell_abs += abs_notional;
            }
        }
    }

    let stats_for = |seconds: u32| -> Option<WindowStats> {
        let idx = window_index(seconds)?;
        let stats = window_stats[idx];
        if stats.sample_count > 0 {
            Some(stats)
        } else {
            None
        }
    };
    let signed_for = |seconds: u32| -> Option<f64> { stats_for(seconds).map(|s| s.signed) };
    let liq_for = |seconds: u32| -> Option<f64> { stats_for(seconds).map(|s| s.liq_signed) };

    let cvd_slope = match (signed_for(60), signed_for(300)) {
        (Some(s60), Some(s300)) => Some((s60 / 60.0) - (s300 / 300.0)),
        _ => None,
    };
    let cvd_acceleration = match (signed_for(60), signed_for(300), signed_for(900)) {
        (Some(s60), Some(s300), Some(s900)) => {
            let short_vs_mid = (s60 / 60.0) - (s300 / 300.0);
            let mid_vs_long = (s300 / 300.0) - (s900 / 900.0);
            Some(short_vs_mid - mid_vs_long)
        }
        _ => None,
    };
    let liquidation_impulse = match (liq_for(60), liq_for(300)) {
        (Some(l60), Some(l300)) => Some((l60 / 60.0) - (l300 / 300.0)),
        _ => None,
    };
    let large_trade_concentration = stats_for(300).and_then(|s| {
        if s.total_abs > 0.0 {
            Some((s.large_abs / s.total_abs).clamp(0.0, 1.0))
        } else {
            None
        }
    });
    let side_imbalance = stats_for(300).and_then(|s| {
        let denom = s.buy_abs + s.sell_abs;
        if denom > 0.0 {
            Some(((s.buy_abs - s.sell_abs) / denom).clamp(-1.0, 1.0))
        } else {
            None
        }
    });
    let flow_persistence = compute_flow_persistence(&window_stats);
    let burst_zscore = compute_burst_zscore(samples, anchor_ts_ms);
    let long_flow = signed_for(900)
        .or_else(|| signed_for(1800))
        .or_else(|| signed_for(3600))
        .or_else(|| signed_for(300));
    let reversal_risk_score = match (signed_for(60), long_flow) {
        (Some(short), Some(long)) => {
            let divergence =
                if short.signum() != 0.0 && long.signum() != 0.0 && short.signum() != long.signum()
                {
                    1.0
                } else {
                    0.0
                };
            let magnitude_ratio = (short.abs() / (long.abs() + 1.0)).clamp(0.0, 1.0);
            let burst_factor = burst_zscore
                .map(|v| (v.abs() / 3.0).clamp(0.0, 1.0))
                .unwrap_or(0.0);
            let liq_factor = liquidation_impulse
                .map(|v| (v.abs() / 50_000.0).clamp(0.0, 1.0))
                .unwrap_or(0.0);
            Some(
                (0.45 * divergence
                    + 0.25 * magnitude_ratio
                    + 0.2 * burst_factor
                    + 0.1 * liq_factor)
                    .clamp(0.0, 1.0),
            )
        }
        _ => None,
    };

    HlFlowDerivedMetrics {
        signed_notional_15s: signed_for(15),
        signed_notional_30s: signed_for(30),
        signed_notional_60s: signed_for(60),
        signed_notional_180s: signed_for(180),
        signed_notional_300s: signed_for(300),
        signed_notional_600s: signed_for(600),
        signed_notional_900s: signed_for(900),
        signed_notional_1800s: signed_for(1800),
        signed_notional_3600s: signed_for(3600),
        cvd_slope,
        cvd_acceleration,
        burst_zscore,
        liquidation_pressure_5m: liq_for(300),
        liquidation_impulse,
        large_trade_concentration,
        side_imbalance,
        flow_persistence,
        reversal_risk_score,
    }
}

fn window_index(seconds: u32) -> Option<usize> {
    HL_FLOW_WINDOWS_SECONDS
        .iter()
        .position(|window| *window == seconds)
}

fn compute_flow_persistence(
    window_stats: &[WindowStats; HL_FLOW_WINDOWS_SECONDS.len()],
) -> Option<f64> {
    let mut weighted_direction_sum = 0.0_f64;
    let mut weight_sum = 0.0_f64;
    for seconds in [15_u32, 30, 60, 180, 300] {
        let Some(idx) = window_index(seconds) else {
            continue;
        };
        let stats = window_stats[idx];
        if stats.sample_count == 0 {
            continue;
        }
        let direction = stats.signed.signum();
        if direction == 0.0 {
            continue;
        }
        let weight = stats.total_abs.max(1.0).sqrt();
        weighted_direction_sum += direction * weight;
        weight_sum += weight;
    }
    if weight_sum > 0.0 {
        Some((weighted_direction_sum / weight_sum).clamp(-1.0, 1.0))
    } else {
        None
    }
}

fn compute_burst_zscore(samples: &VecDeque<L4Sample>, anchor_ts_ms: i64) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let latest_bucket = anchor_ts_ms / BURST_BUCKET_MS;
    let oldest_bucket = latest_bucket.saturating_sub(BURST_HISTORY_BUCKETS as i64 - 1);
    let mut buckets: BTreeMap<i64, f64> = BTreeMap::new();
    let max_lookback_ms = (BURST_HISTORY_BUCKETS as i64) * BURST_BUCKET_MS;

    for sample in samples {
        if anchor_ts_ms.saturating_sub(sample.ts_ms) > max_lookback_ms {
            continue;
        }
        let bucket = sample.ts_ms / BURST_BUCKET_MS;
        if bucket < oldest_bucket || bucket > latest_bucket {
            continue;
        }
        *buckets.entry(bucket).or_insert(0.0) += sample.signed_notional_usd;
    }

    let mut values = Vec::with_capacity(BURST_HISTORY_BUCKETS);
    for bucket in oldest_bucket..=latest_bucket {
        values.push(*buckets.get(&bucket).unwrap_or(&0.0));
    }
    if values.len() < 5 {
        return None;
    }
    let latest = *values.last().unwrap_or(&0.0);
    let baseline = &values[..values.len() - 1];
    if baseline.len() < 4 {
        return Some(0.0);
    }

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

async fn get_file_modified(path: &Path) -> Option<SystemTime> {
    tokio::fs::metadata(path)
        .await
        .ok()
        .and_then(|m| m.modified().ok())
}

fn parse_trend_snapshot(json: &Value) -> TrendStateSnapshot {
    let btc = find_btc_payload(json);
    TrendStateSnapshot {
        adx: get_number(&btc, &["adx"]).or_else(|| {
            get_nested_number_paths(
                &btc,
                &[&["components", "adx", "adx"], &["trend", "adx", "adx"]],
            )
        }),
        di_plus: get_number(&btc, &["di_plus", "di+", "plus_di"]).or_else(|| {
            get_nested_number_paths(
                &btc,
                &[
                    &["components", "adx", "di_plus"],
                    &["components", "adx", "di+"],
                    &["trend", "adx", "di_plus"],
                ],
            )
        }),
        di_minus: get_number(&btc, &["di_minus", "di-", "minus_di"]).or_else(|| {
            get_nested_number_paths(
                &btc,
                &[
                    &["components", "adx", "di_minus"],
                    &["components", "adx", "di-"],
                    &["trend", "adx", "di_minus"],
                ],
            )
        }),
        trend_score: get_number(&btc, &["trend_score", "score"]),
        regime: get_string(&btc, &["regime", "trend_regime"]),
    }
}

fn parse_sr_snapshot(json: &Value) -> SrLevelsSnapshot {
    let btc = find_btc_payload(json);
    SrLevelsSnapshot {
        support: get_number_or_first(&btc, &["support", "supports"])
            .or_else(|| get_nested_number(&btc, &["nearest", "support", "price"])),
        resistance: get_number_or_first(&btc, &["resistance", "resistances"])
            .or_else(|| get_nested_number(&btc, &["nearest", "resistance", "price"])),
        z_score: get_number(&btc, &["z_score", "zscore"])
            .or_else(|| get_nested_number(&btc, &["signal", "z_score"])),
    }
}

fn parse_volume_profile_snapshot(json: &Value) -> VolumeProfileSnapshot {
    let btc = find_btc_payload(json);
    VolumeProfileSnapshot {
        poc: get_number(&btc, &["poc", "POC"]).or_else(|| {
            get_nested_number_paths(
                &btc,
                &[
                    &["timeframes", "session_8h", "poc"],
                    &["timeframes", "daily_24h", "poc"],
                    &["timeframes", "weekly_7d", "poc"],
                ],
            )
        }),
        vah: get_number(&btc, &["vah", "VAH"]).or_else(|| {
            get_nested_number_paths(
                &btc,
                &[
                    &["timeframes", "session_8h", "vah"],
                    &["timeframes", "daily_24h", "vah"],
                    &["timeframes", "weekly_7d", "vah"],
                ],
            )
        }),
        val: get_number(&btc, &["val", "VAL"]).or_else(|| {
            get_nested_number_paths(
                &btc,
                &[
                    &["timeframes", "session_8h", "val"],
                    &["timeframes", "daily_24h", "val"],
                    &["timeframes", "weekly_7d", "val"],
                ],
            )
        }),
    }
}

fn find_btc_payload(value: &Value) -> Value {
    let candidates = [
        "BTC", "btc", "BTCUSD", "BTCUSDT", "BTC-USD", "BTC/USD", "btcusdt",
    ];
    if let Some(obj) = value.as_object() {
        for key in &candidates {
            if let Some(v) = obj.get(*key) {
                return v.clone();
            }
        }
        for container_key in ["symbols", "data", "markets"] {
            if let Some(container) = obj.get(container_key).and_then(Value::as_object) {
                for key in &candidates {
                    if let Some(v) = container.get(*key) {
                        return v.clone();
                    }
                }
            }
        }
    }
    value.clone()
}

fn get_number(value: &Value, keys: &[&str]) -> Option<f64> {
    let obj = value.as_object()?;
    for key in keys {
        if let Some(v) = obj.get(*key) {
            if let Some(n) = value_to_f64(v) {
                return Some(n);
            }
        }
    }
    None
}

fn get_nested_number(value: &Value, path: &[&str]) -> Option<f64> {
    let mut cur = value;
    for key in path {
        cur = cur.get(*key)?;
    }
    value_to_f64(cur)
}

fn get_nested_number_paths(value: &Value, paths: &[&[&str]]) -> Option<f64> {
    for path in paths {
        if let Some(v) = get_nested_number(value, path) {
            return Some(v);
        }
    }
    None
}

fn get_number_or_first(value: &Value, keys: &[&str]) -> Option<f64> {
    let obj = value.as_object()?;
    for key in keys {
        if let Some(v) = obj.get(*key) {
            if let Some(n) = value_to_f64(v) {
                return Some(n);
            }
            if let Some(arr) = v.as_array() {
                if let Some(first) = arr.first().and_then(value_to_f64) {
                    return Some(first);
                }
            }
        }
    }
    None
}

fn get_string(value: &Value, keys: &[&str]) -> Option<String> {
    let obj = value.as_object()?;
    for key in keys {
        if let Some(v) = obj.get(*key) {
            if let Some(s) = v.as_str() {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn value_to_f64(v: &Value) -> Option<f64> {
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    if let Some(n) = v.as_i64() {
        return Some(n as f64);
    }
    if let Some(s) = v.as_str() {
        return s.parse::<f64>().ok();
    }
    None
}

#[derive(Debug, Clone)]
struct ParsedFill {
    symbol: String,
    price: f64,
    notional_usd: f64,
    signed_notional_usd: f64,
    ts_ms: i64,
    is_liquidation: bool,
}

fn parse_fill_line(line: &str) -> Vec<ParsedFill> {
    let mut out = Vec::new();
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return out;
    };
    parse_fill_value(&value, &mut out);
    out
}

fn parse_fill_value(value: &Value, out: &mut Vec<ParsedFill>) {
    if let Some(arr) = value.as_array() {
        for item in arr {
            parse_fill_value(item, out);
        }
        return;
    }
    let Some(obj) = value.as_object() else {
        return;
    };

    if let Some(fills) = obj.get("fills") {
        parse_fill_value(fills, out);
    }
    if let Some(events) = obj.get("events") {
        parse_fill_value(events, out);
    }

    let symbol = get_symbol_from_obj(obj).unwrap_or_default();
    if symbol.is_empty() {
        return;
    }
    let price = first_number(obj, &["px", "price", "p"]);
    let size = first_number(obj, &["sz", "size", "qty", "q"]);
    let (Some(price), Some(size)) = (price, size) else {
        return;
    };
    if price <= 0.0 || size <= 0.0 {
        return;
    }

    if let Some(crossed) = first_bool(obj, &["crossed"]) {
        if !crossed {
            return;
        }
    }

    let sign = side_sign_from_obj(obj);
    if sign == 0.0 {
        return;
    }
    let ts_ms = timestamp_ms_from_obj(obj).unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let notional = price * size;
    let is_liquidation = obj
        .get("liquidation")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || first_bool(obj, &["is_liquidation", "isLiquidation"]).unwrap_or(false);

    out.push(ParsedFill {
        symbol,
        price,
        notional_usd: notional,
        signed_notional_usd: sign * notional,
        ts_ms,
        is_liquidation,
    });
}

fn get_symbol_from_obj(obj: &serde_json::Map<String, Value>) -> Option<String> {
    for key in ["coin", "symbol", "asset", "market", "ticker"] {
        if let Some(s) = obj.get(key).and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }
    None
}

fn side_sign_from_obj(obj: &serde_json::Map<String, Value>) -> f64 {
    for key in ["side", "dir", "direction", "takerSide"] {
        if let Some(side) = obj.get(key).and_then(Value::as_str) {
            let s = side.to_ascii_lowercase();
            if s == "a" || s == "ask" {
                return -1.0;
            }
            if s.starts_with('b') || s.contains("buy") || s.contains("long") {
                return 1.0;
            }
            if s.starts_with('s') || s.contains("sell") || s.contains("short") {
                return -1.0;
            }
        }
    }
    if let Some(b) = first_bool(
        obj,
        &["isBuy", "is_buy", "isTakerBuy", "takerBuy", "aggressorBuy"],
    ) {
        return if b { 1.0 } else { -1.0 };
    }
    if let Some(buyer_is_maker) = obj.get("m").and_then(Value::as_bool) {
        return if buyer_is_maker { -1.0 } else { 1.0 };
    }
    0.0
}

fn timestamp_ms_from_obj(obj: &serde_json::Map<String, Value>) -> Option<i64> {
    for key in ["time", "timestamp", "ts", "T"] {
        if let Some(v) = obj.get(key) {
            let raw = if let Some(n) = v.as_i64() {
                n
            } else if let Some(n) = v.as_u64() {
                n as i64
            } else if let Some(s) = v.as_str() {
                match s.parse::<i64>() {
                    Ok(n) => n,
                    Err(_) => continue,
                }
            } else {
                continue;
            };
            if raw > 0 {
                return Some(if raw < 10_000_000_000 {
                    raw * 1000
                } else {
                    raw
                });
            }
        }
    }
    None
}

fn first_number(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(v) = obj.get(*key) {
            if let Some(n) = value_to_f64(v) {
                return Some(n);
            }
        }
    }
    None
}

fn first_bool(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<bool> {
    for key in keys {
        if let Some(v) = obj.get(*key) {
            if let Some(b) = v.as_bool() {
                return Some(b);
            }
            if let Some(s) = v.as_str() {
                let lower = s.to_ascii_lowercase();
                if lower == "true" || lower == "1" {
                    return Some(true);
                }
                if lower == "false" || lower == "0" {
                    return Some(false);
                }
            }
        }
    }
    None
}

fn is_btc_symbol(symbol: &str) -> bool {
    symbol.to_ascii_uppercase().contains("BTC")
}

async fn latest_file_in_dir(dir: &Path) -> Option<PathBuf> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut best: Option<(SystemTime, PathBuf)> = None;
        let mut stack = vec![dir];

        while let Some(current_dir) = stack.pop() {
            let entries = match std::fs::read_dir(&current_dir) {
                Ok(v) => v,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if !path.is_file() {
                    continue;
                }
                let modified = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                match &best {
                    Some((best_time, _)) if *best_time >= modified => {}
                    _ => best = Some((modified, path)),
                }
            }
        }
        best.map(|(_, path)| path)
    })
    .await
    .ok()
    .flatten()
}

fn read_new_lines(
    path: &Path,
    start_pos: u64,
    max_lines: usize,
    max_bytes: usize,
) -> anyhow::Result<(u64, Vec<String>)> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start_pos))?;
    let mut reader = BufReader::new(file);
    let mut lines = Vec::new();
    let mut total_bytes: usize = 0;
    loop {
        let mut buf = String::new();
        let n = reader.read_line(&mut buf)?;
        if n == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(n);
        if !buf.trim().is_empty() {
            lines.push(buf);
        }
        if lines.len() >= max_lines || total_bytes >= max_bytes {
            break;
        }
    }
    let pos = reader.stream_position()?;
    Ok((pos, lines))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(
        anchor_ts_ms: i64,
        age_secs: i64,
        signed: f64,
        is_liq: bool,
        is_large: bool,
    ) -> L4Sample {
        L4Sample {
            ts_ms: anchor_ts_ms.saturating_sub(age_secs.saturating_mul(1000)),
            signed_notional_usd: signed,
            notional_usd: signed.abs(),
            is_liquidation: is_liq,
            is_large_trade: is_large,
        }
    }

    #[test]
    fn hl_flow_metrics_compute_windowed_sums() {
        let anchor = 1_771_500_000_000_i64;
        let mut samples = VecDeque::new();
        samples.push_back(sample(anchor, 5, 100.0, false, false)); // 15s
        samples.push_back(sample(anchor, 40, -30.0, true, false)); // 60s+
        samples.push_back(sample(anchor, 400, 50.0, false, true)); // 600s+
        samples.push_back(sample(anchor, 1200, -20.0, false, false)); // 1800s+

        let metrics = compute_hl_flow_metrics(&samples, anchor);
        assert_eq!(metrics.signed_notional_15s, Some(100.0));
        assert_eq!(metrics.signed_notional_30s, Some(100.0));
        assert_eq!(metrics.signed_notional_60s, Some(70.0));
        assert_eq!(metrics.signed_notional_300s, Some(70.0));
        assert_eq!(metrics.signed_notional_600s, Some(120.0));
        assert_eq!(metrics.signed_notional_1800s, Some(100.0));
        assert_eq!(metrics.signed_notional_3600s, Some(100.0));
        assert_eq!(metrics.liquidation_pressure_5m, Some(-30.0));
        assert!(metrics.flow_persistence.is_some());
    }

    #[test]
    fn hl_flow_metrics_burst_zscore_detects_spike() {
        let anchor = 1_771_500_000_000_i64;
        let mut samples = VecDeque::new();
        // Baseline: low activity over most 15s buckets.
        for i in 1..BURST_HISTORY_BUCKETS {
            let age_secs = (i as i64) * 15;
            samples.push_back(sample(anchor, age_secs, 10.0, false, false));
        }
        // Latest bucket spike.
        samples.push_back(sample(anchor, 0, 500.0, false, true));

        let metrics = compute_hl_flow_metrics(&samples, anchor);
        let z = metrics.burst_zscore.unwrap_or(0.0);
        assert!(z > 3.0, "expected burst zscore > 3, got {}", z);
        assert!(metrics.reversal_risk_score.is_some());
    }
}
