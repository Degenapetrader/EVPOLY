use crate::event_log::log_event;
use crate::strategy::Direction;
use futures_util::stream::{FuturesUnordered, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::timeout;

const STRATEGY_ID: &str = "endgame_sweep_v1";
const DEFAULT_EXTERNAL_HISTORY_KEEP_MS: i64 = 4 * 60 * 60 * 1_000;
const DEFAULT_EXTERNAL_HISTORY_SAMPLE_MS: i64 = 5_000;
const DEFAULT_EXTERNAL_THRESHOLD_RECOMPUTE_MS: i64 = 60_000;
const DEFAULT_EXTERNAL_MIN_SAMPLES: usize = 300;
const DEFAULT_EXTERNAL_SAMPLE_MS: u64 = 250;
const DEFAULT_EXTERNAL_COINBASE_SAMPLE_MS: u64 = 250;
const DEFAULT_EXTERNAL_BINANCE_SAMPLE_MS: u64 = 1_000;
const DEFAULT_EXTERNAL_WARM_SAMPLE_MS: u64 = 1_000;
const DEFAULT_EXTERNAL_COLD_SAMPLE_MS: u64 = 5_000;
const DEFAULT_EXTERNAL_HEALTH_LOG_MS: i64 = 5_000;
const DEFAULT_EXTERNAL_BACKOFF_MAX_MS: u64 = 5_000;
const DEFAULT_EXTERNAL_BINANCE_RATE_LIMIT_BACKOFF_MS: u64 = 60_000;
const DEFAULT_EXTERNAL_BINANCE_MAX_PER_TICK: usize = 2;
const DEFAULT_EXTERNAL_SLOW_REST_MAX_PER_TICK: usize = 3;
const DEFAULT_EXTERNAL_PERSIST_PATH: &str = "history/endgame_external_depth_snapshots.jsonl";
const DEFAULT_EXTERNAL_PERSIST_INTERVAL_MS: i64 = 30_000;
const DEFAULT_EXTERNAL_PERSIST_COMPACT_INTERVAL_MS: i64 = 30 * 60 * 1_000;
const EXTERNAL_FAILURE_LOG_MIN_MS: i64 = 5_000;
const EXTERNAL_DEPTH_SEVERITY_STEP_COUNT: usize = 4;
const EXTERNAL_DEPTH_THRESHOLD_WINDOWS_MS: [i64; 3] = [3_600_000, 7_200_000, 14_400_000];
const EXTERNAL_DEPTH_BUCKETS_BPS: [u16; 54] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26,
    27, 28, 29, 30, 35, 40, 45, 50, 55, 60, 65, 70, 75, 80, 85, 90, 95, 100, 110, 120, 130, 140,
    150, 160, 170, 180, 190, 200,
];
const EXTERNAL_DEPTH_BUCKET_COUNT: usize = EXTERNAL_DEPTH_BUCKETS_BPS.len();
const EXTERNAL_DEPTH_BUCKET_MIN_BPS: u16 = 1;
const EXTERNAL_DEPTH_BUCKET_MAX_BPS: u16 = 200;
const BINANCE_SPOT_DEPTH_VENUE: &str = "binance_spot_rest_depth_v1";
const BINANCE_FUTURES_DEPTH_VENUE: &str = "binance_futures_rest_depth_v1";
const COINBASE_DEPTH_VENUE: &str = "coinbase_exchange_rest_depth_v1";
const KRAKEN_DEPTH_VENUE: &str = "kraken_spot_rest_depth_v1";
const BITSTAMP_DEPTH_VENUE: &str = "bitstamp_spot_rest_depth_v1";
const HYPERLIQUID_DEPTH_VENUE: &str = "hyperliquid_spot_rest_depth_v1";
const DOGE_COINBASE_REST_DEPTH_POLL_MS: u64 = 500;

#[derive(Debug, Clone)]
pub struct EndgameCexDepthLevel {
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Clone)]
pub struct EndgameCexDepthSnapshot {
    pub symbol: String,
    pub venue: &'static str,
    pub best_bid: f64,
    pub best_ask: f64,
    pub spread_bps: f64,
    pub bids: Vec<EndgameCexDepthLevel>,
    pub asks: Vec<EndgameCexDepthLevel>,
    pub cost_to_buy_up_bps_usd: [Option<f64>; EXTERNAL_DEPTH_BUCKET_COUNT],
    pub cost_to_sell_down_bps_usd: [Option<f64>; EXTERNAL_DEPTH_BUCKET_COUNT],
    pub updated_ms: i64,
}

impl EndgameCexDepthSnapshot {
    pub fn mid(&self) -> f64 {
        (self.best_bid + self.best_ask) / 2.0
    }

    pub fn age_ms(&self, now_ms: i64) -> i64 {
        now_ms.saturating_sub(self.updated_ms).max(0)
    }
}

#[derive(Debug, Clone)]
pub struct EndgameCexDepthDecision {
    pub enabled: bool,
    pub fail_open: bool,
    pub reason: String,
    pub venue: Option<&'static str>,
    pub tick_band_ms: i64,
    pub max_age_ms: i64,
    pub snapshot_age_ms: Option<i64>,
    pub cex_mid: Option<f64>,
    pub boundary_price: Option<f64>,
    pub distance_bps: Option<f64>,
    pub bucket_bps: Option<u16>,
    pub cost_to_boundary_usd: Option<f64>,
    pub threshold_usd: Option<f64>,
    pub multiplier: f64,
    pub trigger_count: usize,
    pub reduce_trigger_count: usize,
    pub increase_trigger_count: usize,
    pub desired_venues: Vec<&'static str>,
    pub payload: Value,
}

impl EndgameCexDepthDecision {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            fail_open: true,
            reason: "disabled".to_string(),
            venue: None,
            tick_band_ms: 0,
            max_age_ms: 0,
            snapshot_age_ms: None,
            cex_mid: None,
            boundary_price: None,
            distance_bps: None,
            bucket_bps: None,
            cost_to_boundary_usd: None,
            threshold_usd: None,
            multiplier: 1.0,
            trigger_count: 0,
            reduce_trigger_count: 0,
            increase_trigger_count: 0,
            desired_venues: Vec::new(),
            payload: json!({"enabled": false, "status": "disabled"}),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EndgameCexDepthConfig {
    pub enabled: bool,
    pub poll_enabled: bool,
    pub adaptive_poll_enabled: bool,
    pub size_increase_enabled: bool,
    pub poll_ms: u64,
    pub coinbase_poll_ms: u64,
    pub binance_poll_ms: u64,
    pub warm_poll_ms: u64,
    pub cold_poll_ms: u64,
    pub warm_before_sec: i64,
    pub hot_before_sec: i64,
    pub hot_after_sec: i64,
    pub fetch_timeout_ms: u64,
    pub backoff_max_ms: u64,
    pub binance_rate_limit_backoff_ms: u64,
    pub binance_max_per_tick: usize,
    pub slow_rest_max_per_tick: usize,
    pub max_age_ms: i64,
    pub base_max_age_ms: i64,
    pub history_keep_ms: i64,
    pub history_sample_ms: i64,
    pub threshold_recompute_ms: i64,
    pub min_samples: usize,
    pub close_adjustment_per_trigger: f64,
    pub warm_adjustment_per_trigger: f64,
    pub far_adjustment_per_trigger: f64,
    pub increase_adjustment_per_trigger: f64,
    pub reduce_quantile: f64,
    pub increase_quantile: f64,
    pub close_reduce_quantile: f64,
    pub close_increase_quantile: f64,
    pub warm_reduce_quantile: f64,
    pub warm_increase_quantile: f64,
    pub eval_max_tau_sec: i64,
    pub close_max_tau_sec: i64,
    pub warm_max_tau_sec: i64,
    pub delta_enabled: bool,
    pub delta_lookback_ms: i64,
    pub delta_ratio: f64,
    pub delta_min_usd: f64,
    pub delta_min_floor_fraction: f64,
    pub persist_enabled: bool,
    pub persist_path: String,
    pub persist_interval_ms: i64,
    pub persist_compact_interval_ms: i64,
    pub health_snapshot_detail_enabled: bool,
    pub floor_btc_usd: f64,
    pub floor_eth_usd: f64,
    pub floor_sol_usd: f64,
    pub floor_other_usd: f64,
    pub ceiling_btc_usd: f64,
    pub ceiling_eth_usd: f64,
    pub ceiling_sol_usd: f64,
    pub ceiling_other_usd: f64,
}

impl EndgameCexDepthConfig {
    pub fn from_env() -> Self {
        Self {
            enabled: env_bool_any(
                &[
                    "EVPOLY_ENDGAME_CEX_DEPTH_GUARD_ENABLE",
                    "EVPOLY_ENDGAME_CEX_DEPTH_ENABLE",
                ],
                true,
            ),
            poll_enabled: env_bool("EVPOLY_ENDGAME_CEX_DEPTH_POLL_ENABLE", true),
            adaptive_poll_enabled: env_bool("EVPOLY_ENDGAME_CEX_DEPTH_ADAPTIVE_POLL_ENABLE", true),
            size_increase_enabled: env_bool("EVPOLY_ENDGAME_CEX_DEPTH_INCREASE_ENABLE", false),
            poll_ms: env_u64(
                "EVPOLY_ENDGAME_CEX_DEPTH_SAMPLE_MS",
                DEFAULT_EXTERNAL_SAMPLE_MS,
                50,
                10_000,
            ),
            coinbase_poll_ms: env_u64(
                "EVPOLY_ENDGAME_CEX_DEPTH_COINBASE_SAMPLE_MS",
                DEFAULT_EXTERNAL_COINBASE_SAMPLE_MS,
                50,
                10_000,
            ),
            binance_poll_ms: env_u64(
                "EVPOLY_ENDGAME_CEX_DEPTH_BINANCE_SAMPLE_MS",
                DEFAULT_EXTERNAL_BINANCE_SAMPLE_MS,
                250,
                60_000,
            ),
            warm_poll_ms: env_u64(
                "EVPOLY_ENDGAME_CEX_DEPTH_WARM_SAMPLE_MS",
                DEFAULT_EXTERNAL_WARM_SAMPLE_MS,
                250,
                60_000,
            ),
            cold_poll_ms: env_u64(
                "EVPOLY_ENDGAME_CEX_DEPTH_COLD_SAMPLE_MS",
                DEFAULT_EXTERNAL_COLD_SAMPLE_MS,
                250,
                60_000,
            ),
            warm_before_sec: env_i64("EVPOLY_ENDGAME_CEX_DEPTH_WARM_BEFORE_SEC", 180, 0, 900),
            hot_before_sec: env_i64("EVPOLY_ENDGAME_CEX_DEPTH_HOT_BEFORE_SEC", 30, 0, 900),
            hot_after_sec: env_i64("EVPOLY_ENDGAME_CEX_DEPTH_HOT_AFTER_SEC", 5, 0, 900),
            close_adjustment_per_trigger: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_ADJUSTMENT_PER_TRIGGER",
                0.12,
                0.0,
                1.0,
            ),
            increase_adjustment_per_trigger: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_INCREASE_ADJUSTMENT_PER_TRIGGER",
                0.12,
                0.0,
                1.0,
            ),
            warm_adjustment_per_trigger: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_WARM_ADJUSTMENT_PER_TRIGGER",
                0.08,
                0.0,
                1.0,
            ),
            far_adjustment_per_trigger: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_FAR_ADJUSTMENT_PER_TRIGGER",
                0.04,
                0.0,
                1.0,
            ),
            fetch_timeout_ms: env_u64("EVPOLY_ENDGAME_CEX_DEPTH_TIMEOUT_MS", 350, 50, 5_000),
            backoff_max_ms: env_u64(
                "EVPOLY_ENDGAME_CEX_DEPTH_BACKOFF_MAX_MS",
                DEFAULT_EXTERNAL_BACKOFF_MAX_MS,
                0,
                60_000,
            ),
            binance_rate_limit_backoff_ms: env_u64(
                "EVPOLY_ENDGAME_CEX_DEPTH_BINANCE_429_BACKOFF_MS",
                DEFAULT_EXTERNAL_BINANCE_RATE_LIMIT_BACKOFF_MS,
                1_000,
                10 * 60_000,
            ),
            binance_max_per_tick: env_usize(
                "EVPOLY_ENDGAME_CEX_DEPTH_BINANCE_MAX_PER_TICK",
                DEFAULT_EXTERNAL_BINANCE_MAX_PER_TICK,
                1,
                100,
            ),
            slow_rest_max_per_tick: env_usize(
                "EVPOLY_ENDGAME_CEX_DEPTH_SLOW_REST_MAX_PER_TICK",
                DEFAULT_EXTERNAL_SLOW_REST_MAX_PER_TICK,
                1,
                100,
            ),
            max_age_ms: env_i64("EVPOLY_ENDGAME_CEX_DEPTH_MAX_AGE_MS", 2_000, 100, 30_000),
            base_max_age_ms: env_i64(
                "EVPOLY_ENDGAME_CEX_DEPTH_BASE_MAX_AGE_MS",
                15_000,
                100,
                60_000,
            ),
            history_keep_ms: env_i64(
                "EVPOLY_ENDGAME_CEX_DEPTH_HISTORY_KEEP_MS",
                DEFAULT_EXTERNAL_HISTORY_KEEP_MS,
                60_000,
                24 * 60 * 60 * 1_000,
            ),
            history_sample_ms: env_i64(
                "EVPOLY_ENDGAME_CEX_DEPTH_HISTORY_SAMPLE_MS",
                DEFAULT_EXTERNAL_HISTORY_SAMPLE_MS,
                250,
                60_000,
            ),
            threshold_recompute_ms: env_i64(
                "EVPOLY_ENDGAME_CEX_DEPTH_THRESHOLD_RECOMPUTE_MS",
                DEFAULT_EXTERNAL_THRESHOLD_RECOMPUTE_MS,
                5_000,
                10 * 60 * 1_000,
            ),
            min_samples: env_usize(
                "EVPOLY_ENDGAME_CEX_DEPTH_MIN_SAMPLES",
                DEFAULT_EXTERNAL_MIN_SAMPLES,
                1,
                100_000,
            ),
            reduce_quantile: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_REDUCE_QUANTILE",
                0.03,
                0.001,
                0.50,
            ),
            increase_quantile: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_INCREASE_QUANTILE",
                0.97,
                0.50,
                0.999,
            ),
            close_reduce_quantile: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_CLOSE_REDUCE_QUANTILE",
                0.05,
                0.001,
                0.50,
            ),
            close_increase_quantile: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_CLOSE_INCREASE_QUANTILE",
                0.95,
                0.50,
                0.999,
            ),
            warm_reduce_quantile: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_WARM_REDUCE_QUANTILE",
                0.04,
                0.001,
                0.50,
            ),
            warm_increase_quantile: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_WARM_INCREASE_QUANTILE",
                0.96,
                0.50,
                0.999,
            ),
            eval_max_tau_sec: env_i64("EVPOLY_ENDGAME_CEX_DEPTH_EVAL_MAX_TAU_SEC", 180, 0, 900),
            close_max_tau_sec: env_i64("EVPOLY_ENDGAME_CEX_DEPTH_CLOSE_MAX_TAU_SEC", 30, 0, 900),
            warm_max_tau_sec: env_i64("EVPOLY_ENDGAME_CEX_DEPTH_WARM_MAX_TAU_SEC", 90, 0, 900),
            delta_enabled: env_bool("EVPOLY_ENDGAME_CEX_DEPTH_DELTA_ENABLE", true),
            delta_lookback_ms: env_i64(
                "EVPOLY_ENDGAME_CEX_DEPTH_DELTA_LOOKBACK_MS",
                3_000,
                500,
                60_000,
            ),
            delta_ratio: env_f64("EVPOLY_ENDGAME_CEX_DEPTH_DELTA_RATIO", 0.60, 0.01, 10.0),
            delta_min_usd: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_DELTA_MIN_USD",
                10_000.0,
                0.0,
                100_000_000.0,
            ),
            delta_min_floor_fraction: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_DELTA_MIN_FLOOR_FRACTION",
                0.20,
                0.0,
                10.0,
            ),
            persist_enabled: env_bool("EVPOLY_ENDGAME_CEX_DEPTH_PERSIST_ENABLE", true),
            persist_path: std::env::var("EVPOLY_ENDGAME_CEX_DEPTH_PERSIST_PATH")
                .unwrap_or_else(|_| DEFAULT_EXTERNAL_PERSIST_PATH.to_string()),
            persist_interval_ms: env_i64(
                "EVPOLY_ENDGAME_CEX_DEPTH_PERSIST_INTERVAL_MS",
                DEFAULT_EXTERNAL_PERSIST_INTERVAL_MS,
                1_000,
                10 * 60 * 1_000,
            ),
            persist_compact_interval_ms: env_i64(
                "EVPOLY_ENDGAME_CEX_DEPTH_PERSIST_COMPACT_INTERVAL_MS",
                DEFAULT_EXTERNAL_PERSIST_COMPACT_INTERVAL_MS,
                60_000,
                24 * 60 * 60 * 1_000,
            ),
            health_snapshot_detail_enabled: env_bool(
                "EVPOLY_ENDGAME_CEX_DEPTH_HEALTH_SNAPSHOTS_ENABLE",
                false,
            ),
            floor_btc_usd: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_FLOOR_BTC_USD",
                50_000.0,
                0.0,
                100_000_000.0,
            ),
            floor_eth_usd: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_FLOOR_ETH_USD",
                25_000.0,
                0.0,
                100_000_000.0,
            ),
            floor_sol_usd: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_FLOOR_SOL_USD",
                10_000.0,
                0.0,
                100_000_000.0,
            ),
            floor_other_usd: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_FLOOR_OTHER_USD",
                5_000.0,
                0.0,
                100_000_000.0,
            ),
            ceiling_btc_usd: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_CEILING_BTC_USD",
                3_000_000.0,
                1.0,
                1_000_000_000.0,
            ),
            ceiling_eth_usd: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_CEILING_ETH_USD",
                1_500_000.0,
                1.0,
                1_000_000_000.0,
            ),
            ceiling_sol_usd: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_CEILING_SOL_USD",
                500_000.0,
                1.0,
                1_000_000_000.0,
            ),
            ceiling_other_usd: env_f64(
                "EVPOLY_ENDGAME_CEX_DEPTH_CEILING_OTHER_USD",
                100_000.0,
                1.0,
                1_000_000_000.0,
            ),
        }
    }

    fn threshold_config(&self) -> ExternalThresholdConfig {
        ExternalThresholdConfig {
            min_samples: self.min_samples,
            close_adjustment_per_trigger: self.close_adjustment_per_trigger,
            warm_adjustment_per_trigger: self.warm_adjustment_per_trigger,
            far_adjustment_per_trigger: self.far_adjustment_per_trigger,
            increase_adjustment_per_trigger: self.increase_adjustment_per_trigger,
            reduce_quantile: self.reduce_quantile,
            increase_quantile: self.increase_quantile,
            close_reduce_quantile: self.close_reduce_quantile,
            close_increase_quantile: self.close_increase_quantile,
            warm_reduce_quantile: self.warm_reduce_quantile,
            warm_increase_quantile: self.warm_increase_quantile,
            floor_btc_usd: self.floor_btc_usd,
            floor_eth_usd: self.floor_eth_usd,
            floor_sol_usd: self.floor_sol_usd,
            floor_other_usd: self.floor_other_usd,
            ceiling_btc_usd: self.ceiling_btc_usd,
            ceiling_eth_usd: self.ceiling_eth_usd,
            ceiling_sol_usd: self.ceiling_sol_usd,
            ceiling_other_usd: self.ceiling_other_usd,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct EndgameCexDepthCache {
    inner: Arc<StdRwLock<EndgameCexDepthCacheInner>>,
}

#[derive(Debug, Default)]
struct EndgameCexDepthCacheInner {
    latest_by_symbol_venue: HashMap<(String, &'static str), EndgameCexDepthSnapshot>,
    history_by_symbol_venue: HashMap<(String, &'static str), VecDeque<ExternalDepthHistorySample>>,
    delta_history_by_symbol_venue:
        HashMap<(String, &'static str), VecDeque<ExternalDepthHistorySample>>,
    threshold_by_symbol_venue_side_bucket:
        HashMap<(String, &'static str, String, u16), ExternalCostBucketThreshold>,
    last_history_sample_ms_by_symbol_venue: HashMap<(String, &'static str), i64>,
}

impl EndgameCexDepthCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert(&self, snapshot: EndgameCexDepthSnapshot, cfg: &EndgameCexDepthConfig) {
        if snapshot.symbol.trim().is_empty()
            || !snapshot.best_bid.is_finite()
            || !snapshot.best_ask.is_finite()
            || snapshot.best_bid <= 0.0
            || snapshot.best_ask <= snapshot.best_bid
        {
            return;
        }
        let key = (normalize_symbol(snapshot.symbol.as_str()), snapshot.venue);
        let mut recompute_input = None;
        if let Ok(mut inner) = self.inner.write() {
            let now_ms = snapshot.updated_ms;
            let threshold_due = inner.external_threshold_due(key.0.as_str(), key.1, now_ms, cfg);
            let delta_keep_ms = external_delta_history_keep_ms(cfg);
            let delta_history = inner
                .delta_history_by_symbol_venue
                .entry(key.clone())
                .or_default();
            delta_history.push_back(ExternalDepthHistorySample::from_snapshot(&snapshot));
            prune_external_depth_by_ts(delta_history, now_ms.saturating_sub(delta_keep_ms));
            cap_external_depth_history_len(
                delta_history,
                delta_keep_ms,
                DEFAULT_EXTERNAL_SAMPLE_MS as i64,
            );
            inner
                .latest_by_symbol_venue
                .insert(key.clone(), snapshot.clone());
            let last_sample_ms = inner
                .last_history_sample_ms_by_symbol_venue
                .get(&key)
                .copied()
                .unwrap_or(0);
            let should_sample = last_sample_ms <= 0
                || now_ms.saturating_sub(last_sample_ms) >= cfg.history_sample_ms.max(1);
            if should_sample {
                inner
                    .last_history_sample_ms_by_symbol_venue
                    .insert(key.clone(), now_ms);
                let history = inner.history_by_symbol_venue.entry(key).or_default();
                history.push_back(ExternalDepthHistorySample::from_snapshot(&snapshot));
                prune_external_depth_by_ts(history, now_ms.saturating_sub(cfg.history_keep_ms));
                cap_external_depth_history_len(history, cfg.history_keep_ms, cfg.history_sample_ms);
                if threshold_due {
                    recompute_input = Some((
                        normalize_symbol(snapshot.symbol.as_str()),
                        snapshot.venue,
                        history.iter().cloned().collect::<Vec<_>>(),
                        cfg.threshold_config(),
                        now_ms,
                    ));
                }
            }
        }
        if let Some((symbol, venue, history, threshold_cfg, now_ms)) = recompute_input {
            let updates = external_threshold_updates_for_history(
                &threshold_cfg,
                symbol.as_str(),
                venue,
                history.as_slice(),
                now_ms,
            );
            if let Ok(mut inner) = self.inner.write() {
                for (key, threshold) in updates {
                    inner
                        .threshold_by_symbol_venue_side_bucket
                        .insert(key, threshold);
                }
            }
        }
    }

    fn latest(&self, symbol: &str, venue: &'static str) -> Option<EndgameCexDepthSnapshot> {
        self.inner.read().ok().and_then(|inner| {
            inner
                .latest_by_symbol_venue
                .get(&(normalize_symbol(symbol), venue))
                .cloned()
        })
    }

    fn history(&self, symbol: &str, venue: &'static str) -> VecDeque<ExternalDepthHistorySample> {
        self.inner
            .read()
            .ok()
            .and_then(|inner| {
                inner
                    .history_by_symbol_venue
                    .get(&(normalize_symbol(symbol), venue))
                    .cloned()
            })
            .unwrap_or_default()
    }

    fn delta_history(
        &self,
        symbol: &str,
        venue: &'static str,
    ) -> VecDeque<ExternalDepthHistorySample> {
        self.inner
            .read()
            .ok()
            .and_then(|inner| {
                inner
                    .delta_history_by_symbol_venue
                    .get(&(normalize_symbol(symbol), venue))
                    .cloned()
            })
            .unwrap_or_default()
    }

    fn threshold(
        &self,
        symbol: &str,
        venue: &'static str,
        side: &str,
        bucket_bps: u16,
    ) -> Option<ExternalCostBucketThreshold> {
        self.inner.read().ok().and_then(|inner| {
            inner
                .threshold_by_symbol_venue_side_bucket
                .get(&(
                    normalize_symbol(symbol),
                    venue,
                    normalize_direction(side).to_string(),
                    bucket_bps,
                ))
                .and_then(initialized_external_bucket_threshold)
        })
    }

    fn recompute_all_thresholds(&self, now_ms: i64, cfg: &EndgameCexDepthConfig) {
        let threshold_cfg = cfg.threshold_config();
        let histories = self
            .inner
            .read()
            .ok()
            .map(|inner| {
                inner
                    .history_by_symbol_venue
                    .iter()
                    .map(|((symbol, venue), history)| {
                        (
                            symbol.clone(),
                            *venue,
                            history.iter().cloned().collect::<Vec<_>>(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut updates = Vec::new();
        for (symbol, venue, history) in histories {
            updates.extend(external_threshold_updates_for_history(
                &threshold_cfg,
                symbol.as_str(),
                venue,
                history.as_slice(),
                now_ms,
            ));
        }
        if updates.is_empty() {
            return;
        }
        if let Ok(mut inner) = self.inner.write() {
            for (key, threshold) in updates {
                inner
                    .threshold_by_symbol_venue_side_bucket
                    .insert(key, threshold);
            }
        }
    }

    fn load_history_sample(&self, sample: ExternalDepthHistorySample, cfg: &EndgameCexDepthConfig) {
        let key = (normalize_symbol(sample.symbol.as_str()), sample.venue);
        if let Ok(mut inner) = self.inner.write() {
            let history = inner
                .history_by_symbol_venue
                .entry(key.clone())
                .or_default();
            history.push_back(sample.clone());
            make_external_history_ordered_unique(history);
            prune_external_depth_by_ts(
                history,
                chrono::Utc::now()
                    .timestamp_millis()
                    .saturating_sub(cfg.history_keep_ms),
            );
            cap_external_depth_history_len(history, cfg.history_keep_ms, cfg.history_sample_ms);
            let last = history.back().map(|sample| sample.updated_ms).unwrap_or(0);
            if last > 0 {
                inner
                    .last_history_sample_ms_by_symbol_venue
                    .insert(key, last);
            }
        }
    }

    fn max_external_depth_history_updated_ms(&self) -> i64 {
        self.inner
            .read()
            .ok()
            .map(|inner| {
                inner
                    .history_by_symbol_venue
                    .values()
                    .filter_map(|history| history.back().map(|sample| sample.updated_ms))
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    }

    fn history_samples_for_persist(
        &self,
        now_ms: i64,
        cfg: &EndgameCexDepthConfig,
    ) -> Vec<ExternalDepthHistorySample> {
        let min_ms = now_ms.saturating_sub(cfg.history_keep_ms);
        self.inner
            .read()
            .ok()
            .map(|inner| {
                inner
                    .history_by_symbol_venue
                    .values()
                    .flat_map(|history| {
                        history
                            .iter()
                            .filter(|sample| sample.updated_ms >= min_ms)
                            .cloned()
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    fn health_json(&self, cfg: &EndgameCexDepthConfig, now_ms: i64) -> Value {
        let Ok(inner) = self.inner.read() else {
            return json!({"lock": "unavailable"});
        };
        let stale_latest = inner
            .latest_by_symbol_venue
            .values()
            .filter(|snapshot| snapshot.age_ms(now_ms) > cfg.max_age_ms)
            .count();
        let snapshots = if cfg.health_snapshot_detail_enabled {
            json!(inner
                .latest_by_symbol_venue
                .iter()
                .map(|((symbol, venue), snapshot)| json!({
                    "symbol": symbol,
                    "venue": venue,
                    "age_ms": snapshot.age_ms(now_ms),
                    "best_bid": snapshot.best_bid,
                    "best_ask": snapshot.best_ask,
                    "spread_bps": snapshot.spread_bps
                }))
                .collect::<Vec<_>>())
        } else {
            Value::Null
        };
        json!({
            "latest_count": inner.latest_by_symbol_venue.len(),
            "stale_latest_count": stale_latest,
            "history_key_count": inner.history_by_symbol_venue.len(),
            "history_sample_count": inner.history_by_symbol_venue.values().map(VecDeque::len).sum::<usize>(),
            "delta_history_key_count": inner.delta_history_by_symbol_venue.len(),
            "delta_history_sample_count": inner.delta_history_by_symbol_venue.values().map(VecDeque::len).sum::<usize>(),
            "threshold_count": inner.threshold_by_symbol_venue_side_bucket.len(),
            "max_history_updated_ms": inner.history_by_symbol_venue.values().filter_map(|history| history.back().map(|sample| sample.updated_ms)).max(),
            "snapshots": snapshots
        })
    }
}

impl EndgameCexDepthCacheInner {
    fn external_threshold_due(
        &self,
        symbol: &str,
        venue: &'static str,
        now_ms: i64,
        cfg: &EndgameCexDepthConfig,
    ) -> bool {
        self.threshold_by_symbol_venue_side_bucket
            .iter()
            .filter(|((threshold_symbol, threshold_venue, _, _), _)| {
                threshold_symbol == symbol && *threshold_venue == venue
            })
            .map(|(_, threshold)| threshold.updated_ms)
            .max()
            .map(|updated_ms| now_ms.saturating_sub(updated_ms) >= cfg.threshold_recompute_ms)
            .unwrap_or(true)
    }
}

pub fn evaluate_cex_depth(
    cfg: &EndgameCexDepthConfig,
    cache: &EndgameCexDepthCache,
    symbol: &str,
    timeframe: &str,
    market_open_ts: i64,
    direction: Direction,
    boundary_price: f64,
    tau_ms: i64,
    now_ms: i64,
) -> EndgameCexDepthDecision {
    if !cfg.enabled {
        return EndgameCexDepthDecision::disabled();
    }
    let tau_sec = tau_ms.max(0).saturating_add(999) / 1_000;
    let Some(tau_band) = external_depth_tau_band(tau_sec, cfg) else {
        return EndgameCexDepthDecision {
            enabled: true,
            fail_open: false,
            reason: "not_in_external_depth_tau_window".to_string(),
            venue: None,
            tick_band_ms: 0,
            max_age_ms: cfg.max_age_ms,
            snapshot_age_ms: None,
            cex_mid: None,
            boundary_price: Some(boundary_price),
            distance_bps: None,
            bucket_bps: None,
            cost_to_boundary_usd: None,
            threshold_usd: None,
            multiplier: 1.0,
            trigger_count: 0,
            reduce_trigger_count: 0,
            increase_trigger_count: 0,
            desired_venues: external_depth_eval_venues(symbol, timeframe),
            payload: json!({
                "enabled": true,
                "status": "not_in_external_depth_tau_window",
                "tau_sec": tau_sec,
                "max_tau_sec": cfg.eval_max_tau_sec
            }),
        };
    };
    let desired_venues = external_depth_eval_venues(symbol, timeframe);
    if !boundary_price.is_finite() || boundary_price <= 0.0 {
        return EndgameCexDepthDecision {
            enabled: true,
            fail_open: true,
            reason: "boundary_missing".to_string(),
            venue: None,
            tick_band_ms: tau_band.tick_band_ms(),
            max_age_ms: cfg.max_age_ms,
            snapshot_age_ms: None,
            cex_mid: None,
            boundary_price: None,
            distance_bps: None,
            bucket_bps: None,
            cost_to_boundary_usd: None,
            threshold_usd: None,
            multiplier: 1.0,
            trigger_count: 0,
            reduce_trigger_count: 0,
            increase_trigger_count: 0,
            desired_venues,
            payload: json!({"enabled": true, "status": "boundary_missing"}),
        };
    }

    let mut venue_decisions = Vec::new();
    for venue in desired_venues.iter().copied() {
        venue_decisions.push((
            venue,
            evaluate_external_depth_venue(
                cfg,
                cache,
                symbol,
                venue,
                market_open_ts,
                direction,
                boundary_price,
                tau_band,
                now_ms,
            ),
        ));
    }
    let grouped = group_external_depth_decisions(
        symbol,
        timeframe,
        venue_decisions.as_slice(),
        cfg.health_snapshot_detail_enabled,
        cfg.size_increase_enabled,
    );
    let venue_payloads = if cfg.health_snapshot_detail_enabled || grouped.trigger_count > 0 {
        venue_decisions
            .iter()
            .filter_map(|(_, decision)| {
                (!decision.payload.is_null()).then(|| decision.payload.clone())
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let first_usable = venue_decisions
        .iter()
        .find(|(_, decision)| decision.usable)
        .map(|(venue, decision)| (*venue, decision));
    let first_triggered = venue_decisions
        .iter()
        .find(|(_, decision)| {
            decision.reduce_trigger_count > 0 || decision.increase_trigger_count > 0
        })
        .map(|(venue, decision)| (*venue, decision));
    let telemetry_source = first_triggered.or(first_usable);
    let all_fail_open = !venue_decisions.iter().any(|(_, decision)| decision.usable);
    let reason = if grouped.trigger_count > 0 {
        "triggered_size_adjust"
    } else if all_fail_open {
        venue_decisions
            .iter()
            .find_map(|(_, decision)| decision.fail_reason.as_deref())
            .unwrap_or("missing_snapshot")
    } else {
        "pass"
    }
    .to_string();
    EndgameCexDepthDecision {
        enabled: true,
        fail_open: all_fail_open,
        reason: reason.clone(),
        venue: telemetry_source.map(|(venue, _)| venue),
        tick_band_ms: tau_band.tick_band_ms(),
        max_age_ms: cfg.max_age_ms,
        snapshot_age_ms: telemetry_source.and_then(|(_, decision)| decision.snapshot_age_ms),
        cex_mid: telemetry_source.and_then(|(_, decision)| decision.cex_mid),
        boundary_price: telemetry_source
            .and_then(|(_, decision)| decision.base_price)
            .or(Some(boundary_price)),
        distance_bps: telemetry_source.and_then(|(_, decision)| decision.distance_bps),
        bucket_bps: telemetry_source.and_then(|(_, decision)| decision.bucket_bps),
        cost_to_boundary_usd: telemetry_source
            .and_then(|(_, decision)| decision.cost_to_boundary_usd),
        threshold_usd: telemetry_source.and_then(|(_, decision)| decision.threshold_usd),
        multiplier: grouped.size_multiplier.clamp(0.0, 10.0),
        trigger_count: grouped.trigger_count,
        reduce_trigger_count: grouped.reduce_trigger_count,
        increase_trigger_count: grouped.increase_trigger_count,
        desired_venues: desired_venues.clone(),
        payload: json!({
            "enabled": true,
            "status": reason,
            "mode": external_depth_mode_label(symbol, timeframe),
            "symbol": normalize_symbol(symbol),
            "timeframe": timeframe,
            "tau_sec": tau_sec,
            "tau_band": tau_band.as_str(),
            "desired_venues": desired_venues,
            "trigger_count": grouped.trigger_count,
            "reduce_trigger_count": grouped.reduce_trigger_count,
            "increase_trigger_count": grouped.increase_trigger_count,
            "size_multiplier": grouped.size_multiplier,
            "grouping": grouped.payload,
            "venues": venue_payloads,
            "fail_open": all_fail_open,
            "metric": "cost_to_flip_endgame_base_boundary_usd_compared_to_matching_rolling_bps_bucket",
            "base_price_source": "external_depth_venue_open_mid_cache_or_endgame_base_anchor_fallback",
            "fallback_endgame_base_price": boundary_price
        }),
    }
}

pub fn spawn_cex_depth_hub(
    cache: EndgameCexDepthCache,
    cfg: EndgameCexDepthConfig,
    symbols: Vec<String>,
    timeframes: Vec<String>,
) {
    if !cfg.enabled || !cfg.poll_enabled {
        log_event(
            "endgame_external_depth_hub_started",
            json!({
                "strategy_id": STRATEGY_ID,
                "enabled": false,
                "guard_enabled": cfg.enabled,
                "poll_enabled": cfg.poll_enabled
            }),
        );
        return;
    }
    let symbols = symbols
        .into_iter()
        .map(|symbol| normalize_symbol(symbol.as_str()))
        .filter(|symbol| !symbol.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let timeframes = normalize_timeframes(timeframes);
    let client = external_depth_http_client();
    let fetch_targets = external_depth_fetch_targets(&symbols, &timeframes);
    let active_venues = fetch_targets
        .iter()
        .map(|target| target.venue)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    tokio::spawn(async move {
        log_event(
            "endgame_external_depth_hub_started",
            json!({
                "strategy_id": STRATEGY_ID,
                "enabled": true,
                "known_venues": [
                    BINANCE_SPOT_DEPTH_VENUE,
                    BINANCE_FUTURES_DEPTH_VENUE,
                    COINBASE_DEPTH_VENUE,
                    KRAKEN_DEPTH_VENUE,
                    BITSTAMP_DEPTH_VENUE,
                    HYPERLIQUID_DEPTH_VENUE
                ],
                "active_venues": active_venues,
                "symbols": symbols,
                "timeframes": timeframes,
                "fetch_targets": fetch_targets.iter().map(|target| json!({
                    "symbol": target.symbol.as_str(),
                    "venue": target.venue
                })).collect::<Vec<_>>(),
                "routing": {
                    "5m15m_BTC_ETH_SOL_XRP": "alt_cex_group_coinbase_or_kraken_bitstamp_plus_binance",
                    "5m15m_BNB_DOGE": "coinbase_plus_binance",
                    "5m15m_HYPE": "coinbase_plus_hyperliquid",
                    "1h": "binance_only_hype_uses_binance_futures"
                },
                "poll_ms": cfg.poll_ms,
                "coinbase_poll_ms": cfg.coinbase_poll_ms,
                "binance_poll_ms": cfg.binance_poll_ms,
                "timeout_ms": cfg.fetch_timeout_ms,
                "binance_429_backoff_ms": cfg.binance_rate_limit_backoff_ms,
                "binance_max_per_tick": cfg.binance_max_per_tick,
                "slow_rest_max_per_tick": cfg.slow_rest_max_per_tick,
                "max_age_ms": cfg.max_age_ms,
                "history_keep_ms": cfg.history_keep_ms,
                "history_sample_ms": cfg.history_sample_ms,
                "threshold_windows_ms": EXTERNAL_DEPTH_THRESHOLD_WINDOWS_MS,
                "min_samples": cfg.min_samples,
                "reduce_quantile": cfg.reduce_quantile,
                "increase_quantile": cfg.increase_quantile,
                "delta_lookback_ms": cfg.delta_lookback_ms,
                "persist_enabled": cfg.persist_enabled,
                "persist_path": cfg.persist_path,
                "adaptive_poll_enabled": cfg.adaptive_poll_enabled,
                "cold_poll_ms": cfg.cold_poll_ms,
                "warm_poll_ms": cfg.warm_poll_ms,
                "hot_before_sec": cfg.hot_before_sec,
                "hot_after_sec": cfg.hot_after_sec,
                "effect": "cached_endgame_size_adjustment_input"
            }),
        );
        let mut last_persisted_sample_ms = 0_i64;
        if cfg.persist_enabled {
            match load_external_depth_history_from_disk(&cache, &cfg).await {
                Ok(loaded) => {
                    last_persisted_sample_ms = cache.max_external_depth_history_updated_ms();
                    if loaded > 0 {
                        log_event(
                            "endgame_external_depth_history_loaded",
                            json!({
                                "strategy_id": STRATEGY_ID,
                                "path": cfg.persist_path,
                                "loaded_samples": loaded
                            }),
                        );
                    }
                }
                Err(err) => {
                    log_event(
                        "endgame_external_depth_history_load_failed",
                        json!({
                            "strategy_id": STRATEGY_ID,
                            "path": cfg.persist_path,
                            "error": err.to_string()
                        }),
                    );
                }
            }
        }
        let startup_persist_ms = chrono::Utc::now().timestamp_millis();
        let mut last_persist_ms = startup_persist_ms;
        let mut last_persist_compact_ms = startup_persist_ms;
        let mut persist_job: Option<tokio::task::JoinHandle<anyhow::Result<(bool, Option<i64>)>>> =
            None;
        let mut persist_job_compact = false;
        let mut last_health_log_ms = 0_i64;
        let mut success_count = 0_u64;
        let mut failure_count = 0_u64;
        let mut last_failure_log_ms_by_key = HashMap::<(String, String, &'static str), i64>::new();
        let mut fetch_backoff_by_target = HashMap::<(String, String), ExternalFetchBackoff>::new();
        let mut last_fetch_ms_by_target = HashMap::<(String, String), i64>::new();
        let mut binance_rate_limit_until_ms = 0_i64;
        let mut binance_rate_limit_count = 0_u64;
        let mut interval = tokio::time::interval(Duration::from_millis(
            external_depth_loop_tick_ms(&cfg, fetch_targets.as_slice()),
        ));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let loop_now_ms = chrono::Utc::now().timestamp_millis();
            let mut fetches = FuturesUnordered::new();
            let mut backoff_skip_count = 0_usize;
            let mut poll_interval_skip_count = 0_usize;
            let mut binance_global_rate_limit_skip_count = 0_usize;
            let mut binance_budget_skip_count = 0_usize;
            let mut slow_rest_budget_skip_count = 0_usize;
            let mut binance_fetch_count = 0_usize;
            let mut slow_rest_fetch_count = 0_usize;
            let mut hot_target_count = 0_usize;
            let mut warm_target_count = 0_usize;
            let mut cold_target_count = 0_usize;
            let mut hot_fetch_count = 0_usize;
            let mut warm_fetch_count = 0_usize;
            let mut cold_fetch_count = 0_usize;
            for target in fetch_targets.iter().cloned() {
                let target_key = (target.symbol.clone(), target.venue.to_string());
                let is_binance = external_depth_is_binance_venue(target.venue);
                let is_slow_non_binance_rest =
                    external_depth_is_slow_rest_venue(target.venue) && !is_binance;
                let poll_mode =
                    external_depth_target_poll_mode(&target, &timeframes, loop_now_ms, &cfg);
                match poll_mode {
                    ExternalDepthPollMode::Hot => hot_target_count += 1,
                    ExternalDepthPollMode::Warm => warm_target_count += 1,
                    ExternalDepthPollMode::Cold => cold_target_count += 1,
                }
                if is_binance && loop_now_ms < binance_rate_limit_until_ms {
                    binance_global_rate_limit_skip_count += 1;
                    continue;
                }
                if let Some(backoff) = fetch_backoff_by_target.get(&target_key) {
                    if loop_now_ms < backoff.next_allowed_ms {
                        backoff_skip_count += 1;
                        continue;
                    }
                }
                if is_binance && binance_fetch_count >= cfg.binance_max_per_tick {
                    binance_budget_skip_count += 1;
                    continue;
                }
                if is_slow_non_binance_rest && slow_rest_fetch_count >= cfg.slow_rest_max_per_tick {
                    slow_rest_budget_skip_count += 1;
                    continue;
                }
                let poll_ms = external_depth_target_poll_ms_for_mode(&cfg, &target, poll_mode);
                if let Some(last_fetch_ms) = last_fetch_ms_by_target.get(&target_key) {
                    if loop_now_ms.saturating_sub(*last_fetch_ms) < poll_ms as i64 {
                        poll_interval_skip_count += 1;
                        continue;
                    }
                }
                last_fetch_ms_by_target.insert(target_key, loop_now_ms);
                if is_binance {
                    binance_fetch_count += 1;
                }
                if is_slow_non_binance_rest {
                    slow_rest_fetch_count += 1;
                }
                match poll_mode {
                    ExternalDepthPollMode::Hot => hot_fetch_count += 1,
                    ExternalDepthPollMode::Warm => warm_fetch_count += 1,
                    ExternalDepthPollMode::Cold => cold_fetch_count += 1,
                }
                let client = client.clone();
                let started_ms = loop_now_ms;
                let timeout_ms = cfg.fetch_timeout_ms;
                fetches.push(async move {
                    let fetch = fetch_external_depth_snapshot(&client, &target);
                    let result = timeout(Duration::from_millis(timeout_ms), fetch).await;
                    (target, started_ms, timeout_ms, result)
                });
            }
            while let Some((target, started_ms, timeout_ms, result)) = fetches.next().await {
                let target_key = (target.symbol.clone(), target.venue.to_string());
                match result {
                    Ok(Ok(snapshot)) => {
                        success_count = success_count.saturating_add(1);
                        fetch_backoff_by_target.remove(&target_key);
                        cache.upsert(snapshot, &cfg);
                    }
                    Ok(Err(err)) => {
                        failure_count = failure_count.saturating_add(1);
                        let now_ms = chrono::Utc::now().timestamp_millis();
                        let rate_limit_backoff_ms = external_depth_binance_rate_limit_backoff_ms(
                            target.venue,
                            &err,
                            cfg.binance_rate_limit_backoff_ms,
                        );
                        if let Some(rate_limit_backoff_ms) = rate_limit_backoff_ms {
                            binance_rate_limit_count = binance_rate_limit_count.saturating_add(1);
                            binance_rate_limit_until_ms = binance_rate_limit_until_ms
                                .max(now_ms.saturating_add(rate_limit_backoff_ms as i64));
                        }
                        let min_backoff_ms = rate_limit_backoff_ms.unwrap_or(0);
                        let backoff_ms = record_external_fetch_failure_backoff(
                            &mut fetch_backoff_by_target,
                            target.symbol.as_str(),
                            target.venue,
                            now_ms,
                            external_depth_target_poll_ms_for_mode(
                                &cfg,
                                &target,
                                ExternalDepthPollMode::Hot,
                            ),
                            cfg.backoff_max_ms,
                            min_backoff_ms,
                        );
                        let key = (
                            target.symbol.clone(),
                            target.venue.to_string(),
                            "fetch_failed",
                        );
                        if now_ms
                            .saturating_sub(*last_failure_log_ms_by_key.get(&key).unwrap_or(&0))
                            >= EXTERNAL_FAILURE_LOG_MIN_MS
                        {
                            last_failure_log_ms_by_key.insert(key, now_ms);
                            log_event(
                                "endgame_external_depth_fetch_failed",
                                json!({
                                    "strategy_id": STRATEGY_ID,
                                    "venue": target.venue,
                                    "symbol": target.symbol,
                                    "latency_ms": now_ms.saturating_sub(started_ms),
                                    "backoff_ms": backoff_ms,
                                    "rate_limited": rate_limit_backoff_ms.is_some(),
                                    "binance_global_backoff_ms": rate_limit_backoff_ms,
                                    "error": err.to_string()
                                }),
                            );
                        }
                    }
                    Err(_) => {
                        failure_count = failure_count.saturating_add(1);
                        let now_ms = chrono::Utc::now().timestamp_millis();
                        let backoff_ms = record_external_fetch_failure_backoff(
                            &mut fetch_backoff_by_target,
                            target.symbol.as_str(),
                            target.venue,
                            now_ms,
                            external_depth_target_poll_ms_for_mode(
                                &cfg,
                                &target,
                                ExternalDepthPollMode::Hot,
                            ),
                            cfg.backoff_max_ms,
                            0,
                        );
                        let key = (
                            target.symbol.clone(),
                            target.venue.to_string(),
                            "fetch_timeout",
                        );
                        if now_ms
                            .saturating_sub(*last_failure_log_ms_by_key.get(&key).unwrap_or(&0))
                            >= EXTERNAL_FAILURE_LOG_MIN_MS
                        {
                            last_failure_log_ms_by_key.insert(key, now_ms);
                            log_event(
                                "endgame_external_depth_fetch_timeout",
                                json!({
                                    "strategy_id": STRATEGY_ID,
                                    "venue": target.venue,
                                    "symbol": target.symbol,
                                    "timeout_ms": timeout_ms,
                                    "backoff_ms": backoff_ms
                                }),
                            );
                        }
                    }
                }
            }
            let now_ms = chrono::Utc::now().timestamp_millis();
            if now_ms.saturating_sub(last_health_log_ms) >= DEFAULT_EXTERNAL_HEALTH_LOG_MS {
                last_health_log_ms = now_ms;
                log_event(
                    "endgame_external_depth_hub_health",
                    json!({
                        "strategy_id": STRATEGY_ID,
                        "success_count": success_count,
                        "failure_count": failure_count,
                        "backoff_skip_count": backoff_skip_count,
                        "poll_interval_skip_count": poll_interval_skip_count,
                        "binance_global_rate_limit_skip_count": binance_global_rate_limit_skip_count,
                        "binance_budget_skip_count": binance_budget_skip_count,
                        "slow_rest_budget_skip_count": slow_rest_budget_skip_count,
                        "binance_rate_limit_count": binance_rate_limit_count,
                        "binance_rate_limit_cooldown_ms_remaining": binance_rate_limit_until_ms.saturating_sub(now_ms).max(0),
                        "hot_target_count": hot_target_count,
                        "warm_target_count": warm_target_count,
                        "cold_target_count": cold_target_count,
                        "hot_fetch_count": hot_fetch_count,
                        "warm_fetch_count": warm_fetch_count,
                        "cold_fetch_count": cold_fetch_count,
                        "cache": cache.health_json(&cfg, now_ms)
                    }),
                );
            }
            if cfg.persist_enabled {
                if let Some(job) = persist_job.as_mut() {
                    if job.is_finished() {
                        if let Some(job) = persist_job.take() {
                            match job.await {
                                Ok(Ok((persisted, max_updated_ms))) => {
                                    if persisted {
                                        last_persist_ms = now_ms;
                                        if persist_job_compact {
                                            last_persist_compact_ms = now_ms;
                                        }
                                        if let Some(max_updated_ms) = max_updated_ms {
                                            last_persisted_sample_ms =
                                                last_persisted_sample_ms.max(max_updated_ms);
                                        }
                                    }
                                }
                                Ok(Err(err)) => {
                                    log_event(
                                        "endgame_external_depth_history_persist_failed",
                                        json!({
                                            "strategy_id": STRATEGY_ID,
                                            "path": cfg.persist_path,
                                            "error": err.to_string()
                                        }),
                                    );
                                }
                                Err(err) => {
                                    log_event(
                                        "endgame_external_depth_history_persist_failed",
                                        json!({
                                            "strategy_id": STRATEGY_ID,
                                            "path": cfg.persist_path,
                                            "error": err.to_string()
                                        }),
                                    );
                                }
                            }
                        }
                    }
                }
                let persist_due = now_ms.saturating_sub(last_persist_ms) >= cfg.persist_interval_ms;
                let compact_due = now_ms.saturating_sub(last_persist_compact_ms)
                    >= cfg.persist_compact_interval_ms;
                let max_sample_ms = cache.max_external_depth_history_updated_ms();
                if persist_job.is_none()
                    && (persist_due || compact_due)
                    && max_sample_ms > last_persisted_sample_ms
                {
                    let cache_for_job = cache.clone();
                    let cfg_for_job = cfg.clone();
                    persist_job_compact = compact_due;
                    persist_job = Some(tokio::spawn(async move {
                        persist_external_depth_history_to_disk(
                            &cache_for_job,
                            &cfg_for_job,
                            last_persisted_sample_ms,
                            compact_due,
                        )
                        .await
                    }));
                }
            }
        }
    });
}

fn evaluate_external_depth_venue(
    cfg: &EndgameCexDepthConfig,
    cache: &EndgameCexDepthCache,
    symbol: &str,
    venue: &'static str,
    market_open_ts: i64,
    direction: Direction,
    boundary_price: f64,
    tau_band: ExternalDepthTauBand,
    now_ms: i64,
) -> ExternalVenueDecision {
    let Some(depth) = cache.latest(symbol, venue) else {
        return ExternalVenueDecision {
            payload: json!({
                "enabled": true,
                "status": "missing_snapshot",
                "venue": venue,
                "symbol": normalize_symbol(symbol)
            }),
            fail_reason: Some("missing_snapshot".to_string()),
            ..ExternalVenueDecision::default()
        };
    };
    let age_ms = depth.age_ms(now_ms);
    if age_ms > cfg.max_age_ms {
        return ExternalVenueDecision {
            payload: json!({
                "enabled": true,
                "status": "stale_snapshot",
                "symbol": depth.symbol,
                "venue": depth.venue,
                "age_ms": age_ms,
                "max_age_ms": cfg.max_age_ms,
                "updated_ms": depth.updated_ms
            }),
            fail_reason: Some("stale_snapshot".to_string()),
            snapshot_age_ms: Some(age_ms),
            cex_mid: Some(depth.mid()),
            ..ExternalVenueDecision::default()
        };
    }
    let history = cache.history(symbol, venue);
    let market_open_ms = market_open_ts.saturating_mul(1_000);
    let (base_price, base_price_source) =
        external_depth_base_mid_at_or_near_open(&history, market_open_ms, cfg.base_max_age_ms)
            .filter(|price| price.is_finite() && *price > 0.0)
            .map(|price| (price, "external_depth_venue_open_mid_cache"))
            .unwrap_or((boundary_price, "endgame_base_anchor_fallback"));
    let delta_history = cache.delta_history(symbol, venue);
    let mid = depth.mid();
    let direction_label = direction_label(direction);
    let actual_distance_bps = if direction == Direction::Up {
        ((mid / base_price) - 1.0).max(0.0) * 10_000.0
    } else {
        ((base_price / mid) - 1.0).max(0.0) * 10_000.0
    };
    let bucket_bps = external_depth_bucket_for_actual_bps(actual_distance_bps);
    let bucket_idx = external_depth_bucket_index(bucket_bps).unwrap_or(0);
    let relax_eligible = actual_distance_bps.is_finite()
        && actual_distance_bps <= EXTERNAL_DEPTH_BUCKET_MAX_BPS as f64 + 1e-9;
    let (
        cost,
        reached,
        risk_flip_side,
        current_bucket_cost,
        delta_ref_bucket_cost,
        delta_ref_updated_ms,
        cost_metric,
    ) = if direction == Direction::Up {
        let bucket_cost = depth
            .cost_to_sell_down_bps_usd
            .get(bucket_idx)
            .copied()
            .flatten();
        let (base_cost, base_reached) = if relax_eligible {
            cost_to_sell_down_to(depth.bids.as_slice(), base_price)
        } else {
            (f64::INFINITY, false)
        };
        let (cost, reached, metric) = if relax_eligible && base_reached {
            (
                base_cost,
                true,
                "base_boundary_compared_to_bucket_threshold",
            )
        } else {
            (
                bucket_cost.unwrap_or(f64::INFINITY),
                bucket_cost.is_some(),
                if relax_eligible {
                    "bucket_boundary_compared_to_bucket_threshold"
                } else {
                    "max_bucket_boundary_compared_to_bucket_threshold"
                },
            )
        };
        let delta_ref = latest_external_depth_at_or_before(
            &delta_history,
            now_ms.saturating_sub(cfg.delta_lookback_ms),
            cfg.max_age_ms,
        );
        (
            cost,
            reached,
            "DOWN",
            bucket_cost,
            delta_ref.as_ref().and_then(|sample| {
                sample
                    .cost_to_sell_down_bps_usd
                    .get(bucket_idx)
                    .copied()
                    .flatten()
            }),
            delta_ref.as_ref().map(|sample| sample.updated_ms),
            metric,
        )
    } else {
        let bucket_cost = depth
            .cost_to_buy_up_bps_usd
            .get(bucket_idx)
            .copied()
            .flatten();
        let (base_cost, base_reached) = if relax_eligible {
            cost_to_buy_up_to(depth.asks.as_slice(), base_price)
        } else {
            (f64::INFINITY, false)
        };
        let (cost, reached, metric) = if relax_eligible && base_reached {
            (
                base_cost,
                true,
                "base_boundary_compared_to_bucket_threshold",
            )
        } else {
            (
                bucket_cost.unwrap_or(f64::INFINITY),
                bucket_cost.is_some(),
                if relax_eligible {
                    "bucket_boundary_compared_to_bucket_threshold"
                } else {
                    "max_bucket_boundary_compared_to_bucket_threshold"
                },
            )
        };
        let delta_ref = latest_external_depth_at_or_before(
            &delta_history,
            now_ms.saturating_sub(cfg.delta_lookback_ms),
            cfg.max_age_ms,
        );
        (
            cost,
            reached,
            "UP",
            bucket_cost,
            delta_ref.as_ref().and_then(|sample| {
                sample
                    .cost_to_buy_up_bps_usd
                    .get(bucket_idx)
                    .copied()
                    .flatten()
            }),
            delta_ref.as_ref().map(|sample| sample.updated_ms),
            metric,
        )
    };
    if !reached || !cost.is_finite() {
        return ExternalVenueDecision {
            payload: json!({
                "enabled": true,
                "status": "flip_cost_unavailable",
                "symbol": depth.symbol,
                "venue": depth.venue,
                "age_ms": age_ms,
                "max_age_ms": cfg.max_age_ms,
                "best_bid": depth.best_bid,
                "best_ask": depth.best_ask,
                "direction": direction_label,
                "risk_flip_side": risk_flip_side,
                "base_price": base_price,
                "base_price_source": base_price_source,
                "fallback_endgame_base_price": boundary_price,
                "actual_distance_bps": actual_distance_bps,
                "bucket_bps": bucket_bps
            }),
            fail_reason: Some("flip_cost_unavailable".to_string()),
            snapshot_age_ms: Some(age_ms),
            cex_mid: Some(mid),
            distance_bps: Some(actual_distance_bps),
            bucket_bps: Some(bucket_bps),
            base_price: Some(base_price),
            ..ExternalVenueDecision::default()
        };
    }

    let threshold_cfg = cfg.threshold_config();
    let threshold_stats = cache.threshold(symbol, venue, risk_flip_side, bucket_bps);
    let mut trigger_count = 0_usize;
    let mut reduce_trigger_count = 0_usize;
    let mut increase_trigger_count = 0_usize;
    let mut window_decisions = Vec::new();
    let mut window_payloads = Vec::new();
    let detail_payload = cfg.health_snapshot_detail_enabled;
    if let Some(threshold_stats) = threshold_stats.as_ref() {
        for window in threshold_stats.windows_for_band(tau_band).iter() {
            let threshold_band_valid =
                window.increase_threshold_usd > window.reduce_threshold_usd + 1e-9;
            let threshold_ready = !window.cold_start_floor_used;
            let reduce_adjustment = if threshold_ready && threshold_band_valid {
                external_depth_reduce_adjustment_for_cost(
                    cost,
                    window.reduce_severity_thresholds.as_slice(),
                )
            } else {
                0.0
            };
            let increase_adjustment = if threshold_ready && threshold_band_valid {
                external_depth_increase_adjustment_for_cost(
                    cost,
                    window.increase_severity_thresholds.as_slice(),
                )
            } else {
                0.0
            };
            let reduce_triggered = reduce_adjustment > 0.0;
            let increase_triggered = increase_adjustment > 0.0;
            if reduce_triggered {
                trigger_count = trigger_count.saturating_add(1);
                reduce_trigger_count = reduce_trigger_count.saturating_add(1);
            }
            if increase_triggered {
                trigger_count = trigger_count.saturating_add(1);
                increase_trigger_count = increase_trigger_count.saturating_add(1);
            }
            window_decisions.push(ExternalVenueWindowDecision {
                window_ms: window.window_ms,
                reduce_adjustment,
                increase_adjustment,
            });
            if detail_payload || reduce_triggered || increase_triggered {
                window_payloads.push(json!({
                    "tau_band": tau_band.as_str(),
                    "window_ms": window.window_ms,
                    "side": window.side,
                    "bucket_bps": window.bucket_bps,
                    "sample_count": window.sample_count,
                    "cold_start_floor_used": window.cold_start_floor_used,
                    "reduce_threshold_usd": window.reduce_threshold_usd,
                    "increase_threshold_usd": window.increase_threshold_usd,
                    "reduce_quantile": window.reduce_quantile,
                    "increase_quantile": window.increase_quantile,
                    "scaled_floor_usd": window.scaled_floor_usd,
                    "scaled_ceiling_usd": window.scaled_ceiling_usd,
                    "threshold_ready": threshold_ready,
                    "threshold_band_valid": threshold_band_valid,
                    "reduce_triggered": reduce_triggered,
                    "increase_triggered": increase_triggered,
                    "reduce_adjustment": reduce_adjustment,
                    "increase_adjustment": increase_adjustment,
                    "reduce_severity_thresholds": if detail_payload {
                        json!(window.reduce_severity_thresholds.iter().map(|threshold| json!({
                            "quantile": threshold.quantile,
                            "adjustment": threshold.adjustment,
                            "threshold_usd": threshold.threshold_usd,
                            "matched": cost <= threshold.threshold_usd
                        })).collect::<Vec<_>>())
                    } else {
                        Value::Null
                    },
                    "increase_severity_thresholds": if detail_payload {
                        json!(window.increase_severity_thresholds.iter().map(|threshold| json!({
                            "quantile": threshold.quantile,
                            "adjustment": threshold.adjustment,
                            "threshold_usd": threshold.threshold_usd,
                            "matched": cost >= threshold.threshold_usd
                        })).collect::<Vec<_>>())
                    } else {
                        Value::Null
                    }
                }));
            }
        }
    }
    let (floor, _) = scaled_external_floor_ceiling(&threshold_cfg, symbol, bucket_bps);
    let delta_min_usd = cfg.delta_min_usd.max(floor * cfg.delta_min_floor_fraction);
    let mut delta_decision = ExternalVenueDeltaDecision::default();
    let mut delta_payload = json!({
        "enabled": cfg.delta_enabled,
        "lookback_ms": cfg.delta_lookback_ms,
        "ratio": cfg.delta_ratio,
        "min_usd": delta_min_usd,
        "current_bucket_cost_usd": current_bucket_cost,
        "reference_bucket_cost_usd": delta_ref_bucket_cost,
        "reference_updated_ms": delta_ref_updated_ms,
        "reference_age_ms": delta_ref_updated_ms.map(|ts| now_ms.saturating_sub(ts).max(0))
    });
    if cfg.delta_enabled {
        if let (Some(current), Some(reference)) = (current_bucket_cost, delta_ref_bucket_cost) {
            let reduce_triggered = reference > 0.0
                && current <= reference * (1.0 - cfg.delta_ratio)
                && reference - current >= delta_min_usd;
            let increase_triggered = reference > 0.0
                && current >= reference * (1.0 + cfg.delta_ratio)
                && current - reference >= delta_min_usd;
            if reduce_triggered {
                trigger_count = trigger_count.saturating_add(1);
                reduce_trigger_count = reduce_trigger_count.saturating_add(1);
            }
            if increase_triggered {
                trigger_count = trigger_count.saturating_add(1);
                increase_trigger_count = increase_trigger_count.saturating_add(1);
            }
            delta_decision = ExternalVenueDeltaDecision {
                reduce_adjustment: if reduce_triggered {
                    external_depth_adjustment_per_trigger_for_band(cfg, tau_band)
                } else {
                    0.0
                },
                increase_adjustment: if increase_triggered {
                    external_depth_adjustment_per_trigger_for_band(cfg, tau_band)
                } else {
                    0.0
                },
            };
            delta_payload = json!({
                "enabled": true,
                "lookback_ms": cfg.delta_lookback_ms,
                "ratio": cfg.delta_ratio,
                "min_usd": delta_min_usd,
                "current_bucket_cost_usd": current,
                "reference_bucket_cost_usd": reference,
                "reference_updated_ms": delta_ref_updated_ms,
                "reference_age_ms": delta_ref_updated_ms.map(|ts| now_ms.saturating_sub(ts).max(0)),
                "absolute_delta_usd": current - reference,
                "relative_delta": if reference > 0.0 { Some((current / reference) - 1.0) } else { None },
                "reduce_triggered": reduce_triggered,
                "increase_triggered": increase_triggered,
                "reduce_adjustment": delta_decision.reduce_adjustment,
                "increase_adjustment": delta_decision.increase_adjustment
            });
        }
    }
    let representative_threshold = threshold_stats
        .as_ref()
        .and_then(|stats| stats.windows_for_band(tau_band).first())
        .map(|window| window.reduce_threshold_usd);
    let payload = if detail_payload || trigger_count > 0 {
        json!({
            "enabled": true,
            "status": if trigger_count > 0 { "triggered_size_adjust" } else { "pass" },
            "symbol": depth.symbol,
            "venue": depth.venue,
            "age_ms": age_ms,
            "max_age_ms": cfg.max_age_ms,
            "best_bid": depth.best_bid,
            "best_ask": depth.best_ask,
            "spread_bps": depth.spread_bps,
            "direction": direction_label,
            "risk_flip_side": risk_flip_side,
            "base_price": base_price,
            "base_price_source": base_price_source,
            "fallback_endgame_base_price": boundary_price,
            "actual_distance_bps": actual_distance_bps,
            "bucket_bps": bucket_bps,
            "bucket_rule": "ceil(actual_distance_bps) to 1..30, then 35..100 by 5bps, then 110..200 by 10bps; actual_distance_bps >200 uses bucket 200",
            "relax_eligible": relax_eligible,
            "cost_metric": cost_metric,
            "flip_cost_usd": cost,
            "flip_boundary_reached": reached,
            "current_bucket_cost_usd": current_bucket_cost,
            "threshold_updated_ms": threshold_stats.as_ref().map(|stats| stats.updated_ms),
            "threshold_min_samples": cfg.min_samples,
            "tau_band": tau_band.as_str(),
            "adjustment_per_trigger": external_depth_adjustment_per_trigger_for_band(cfg, tau_band),
            "trigger_count": trigger_count,
            "increase_trigger_count": increase_trigger_count,
            "reduce_trigger_count": reduce_trigger_count,
            "threshold_windows": window_payloads,
            "delta": delta_payload
        })
    } else {
        Value::Null
    };
    ExternalVenueDecision {
        usable: true,
        window_decisions,
        delta_decision,
        payload,
        fail_reason: None,
        snapshot_age_ms: Some(age_ms),
        cex_mid: Some(mid),
        distance_bps: Some(actual_distance_bps),
        bucket_bps: Some(bucket_bps),
        base_price: Some(base_price),
        cost_to_boundary_usd: Some(cost),
        threshold_usd: representative_threshold,
        reduce_trigger_count,
        increase_trigger_count,
    }
}

#[derive(Debug, Clone, Default)]
struct ExternalVenueDecision {
    usable: bool,
    window_decisions: Vec<ExternalVenueWindowDecision>,
    delta_decision: ExternalVenueDeltaDecision,
    payload: Value,
    fail_reason: Option<String>,
    snapshot_age_ms: Option<i64>,
    cex_mid: Option<f64>,
    distance_bps: Option<f64>,
    bucket_bps: Option<u16>,
    base_price: Option<f64>,
    cost_to_boundary_usd: Option<f64>,
    threshold_usd: Option<f64>,
    reduce_trigger_count: usize,
    increase_trigger_count: usize,
}

#[derive(Debug, Clone, Default)]
struct ExternalVenueWindowDecision {
    window_ms: i64,
    reduce_adjustment: f64,
    increase_adjustment: f64,
}

#[derive(Debug, Clone, Default)]
struct ExternalVenueDeltaDecision {
    reduce_adjustment: f64,
    increase_adjustment: f64,
}

#[derive(Debug, Clone)]
struct ExternalDepthGroupedDecision {
    trigger_count: usize,
    increase_trigger_count: usize,
    reduce_trigger_count: usize,
    size_multiplier: f64,
    payload: Value,
}

fn group_external_depth_decisions(
    symbol: &str,
    timeframe: &str,
    venue_decisions: &[(&'static str, ExternalVenueDecision)],
    detail_payload: bool,
    external_increase_enabled: bool,
) -> ExternalDepthGroupedDecision {
    let groups = external_depth_trigger_groups(symbol, timeframe);
    let mut trigger_count = 0_usize;
    let mut increase_trigger_count = 0_usize;
    let mut reduce_trigger_count = 0_usize;
    let mut size_multiplier = 1.0_f64;
    let mut group_payloads = Vec::new();
    for group in groups {
        let mut group_trigger_count = 0_usize;
        let mut group_increase_trigger_count = 0_usize;
        let mut group_reduce_trigger_count = 0_usize;
        let mut group_window_payloads = Vec::new();
        let mut group_reasons = Vec::new();
        for window_ms in EXTERNAL_DEPTH_THRESHOLD_WINDOWS_MS {
            let reduce = external_depth_group_window_action_triggered(
                group,
                venue_decisions,
                ExternalDepthAction::Reduce,
                window_ms,
            );
            let increase = external_depth_group_window_action_triggered(
                group,
                venue_decisions,
                ExternalDepthAction::Increase,
                window_ms,
            );
            let reduce_adjustment =
                external_depth_weighted_window_adjustment(reduce.adjustment, window_ms);
            let increase_adjustment =
                external_depth_weighted_window_adjustment(increase.adjustment, window_ms);
            let mut window_trigger_count = 0_usize;
            let mut window_reasons = Vec::new();
            if reduce.triggered {
                trigger_count = trigger_count.saturating_add(1);
                reduce_trigger_count = reduce_trigger_count.saturating_add(1);
                group_reduce_trigger_count = group_reduce_trigger_count.saturating_add(1);
                group_trigger_count = group_trigger_count.saturating_add(1);
                window_trigger_count = window_trigger_count.saturating_add(1);
                size_multiplier *= 1.0 - reduce_adjustment;
                let reason = external_depth_reason_for_group_action(group, "reduce");
                group_reasons.push(reason);
                window_reasons.push(reason);
            }
            if increase.triggered {
                trigger_count = trigger_count.saturating_add(1);
                increase_trigger_count = increase_trigger_count.saturating_add(1);
                group_increase_trigger_count = group_increase_trigger_count.saturating_add(1);
                group_trigger_count = group_trigger_count.saturating_add(1);
                window_trigger_count = window_trigger_count.saturating_add(1);
                if external_increase_enabled {
                    size_multiplier *= 1.0 + increase_adjustment;
                }
                let reason = external_depth_reason_for_group_action(group, "increase");
                group_reasons.push(reason);
                window_reasons.push(reason);
            }
            if detail_payload || window_trigger_count > 0 {
                group_window_payloads.push(json!({
                    "window_ms": window_ms,
                    "window_stack_weight": external_depth_window_stack_weight(window_ms),
                    "trigger_count": window_trigger_count,
                    "reduce_triggered": reduce.triggered,
                    "increase_triggered": increase.triggered,
                    "reduce_raw_adjustment": reduce.adjustment,
                    "increase_raw_adjustment": increase.adjustment,
                    "reduce_adjustment": reduce_adjustment,
                    "increase_adjustment": increase_adjustment,
                    "reduce_supporting_venues": reduce.supporting_venues,
                    "increase_supporting_venues": increase.supporting_venues,
                    "reasons": window_reasons
                }));
            }
        }
        let reduce_delta = external_depth_group_delta_action_triggered(
            group,
            venue_decisions,
            ExternalDepthAction::Reduce,
        );
        let increase_delta = external_depth_group_delta_action_triggered(
            group,
            venue_decisions,
            ExternalDepthAction::Increase,
        );
        let mut delta_trigger_count = 0_usize;
        let mut delta_reasons = Vec::new();
        if reduce_delta.triggered {
            trigger_count = trigger_count.saturating_add(1);
            reduce_trigger_count = reduce_trigger_count.saturating_add(1);
            group_reduce_trigger_count = group_reduce_trigger_count.saturating_add(1);
            group_trigger_count = group_trigger_count.saturating_add(1);
            delta_trigger_count = delta_trigger_count.saturating_add(1);
            size_multiplier *= 1.0 - reduce_delta.adjustment;
            let reason = external_depth_reason_for_group_action(group, "reduce");
            group_reasons.push(reason);
            delta_reasons.push(reason);
        }
        if increase_delta.triggered {
            trigger_count = trigger_count.saturating_add(1);
            increase_trigger_count = increase_trigger_count.saturating_add(1);
            group_increase_trigger_count = group_increase_trigger_count.saturating_add(1);
            group_trigger_count = group_trigger_count.saturating_add(1);
            delta_trigger_count = delta_trigger_count.saturating_add(1);
            if external_increase_enabled {
                size_multiplier *= 1.0 + increase_delta.adjustment;
            }
            let reason = external_depth_reason_for_group_action(group, "increase");
            group_reasons.push(reason);
            delta_reasons.push(reason);
        }
        if detail_payload || group_trigger_count > 0 {
            group_payloads.push(json!({
                "group": group.as_str(),
                "rule": group.rule_label(),
                "venues": group.venues(),
                "trigger_count": group_trigger_count,
                "increase_trigger_count": group_increase_trigger_count,
                "reduce_trigger_count": group_reduce_trigger_count,
                "window_policy": "each_1h_2h_4h_window_counts_separately_with_decay_1_0_0_5_0_25",
                "windows": group_window_payloads,
                "delta": {
                    "trigger_count": delta_trigger_count,
                    "reduce_triggered": reduce_delta.triggered,
                    "increase_triggered": increase_delta.triggered,
                    "reduce_adjustment": reduce_delta.adjustment,
                    "increase_adjustment": increase_delta.adjustment,
                    "reduce_supporting_venues": reduce_delta.supporting_venues,
                    "increase_supporting_venues": increase_delta.supporting_venues,
                    "reasons": delta_reasons
                },
                "reasons": group_reasons
            }));
        }
    }
    ExternalDepthGroupedDecision {
        trigger_count,
        increase_trigger_count,
        reduce_trigger_count,
        size_multiplier,
        payload: json!({
            "mode": external_depth_mode_label(symbol, timeframe),
            "trigger_count": trigger_count,
            "increase_trigger_count": increase_trigger_count,
            "reduce_trigger_count": reduce_trigger_count,
            "size_multiplier": size_multiplier,
            "groups": group_payloads
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalDepthTauBand {
    Close,
    Warm,
    Far,
}

impl ExternalDepthTauBand {
    fn as_str(self) -> &'static str {
        match self {
            Self::Close => "close",
            Self::Warm => "warm",
            Self::Far => "far",
        }
    }

    fn tick_band_ms(self) -> i64 {
        match self {
            Self::Close => 100,
            Self::Warm => 1_000,
            Self::Far => 2_000,
        }
    }
}

fn external_depth_tau_band(
    tau_sec: i64,
    cfg: &EndgameCexDepthConfig,
) -> Option<ExternalDepthTauBand> {
    if tau_sec < 0 || tau_sec > cfg.eval_max_tau_sec {
        return None;
    }
    if tau_sec <= cfg.close_max_tau_sec {
        Some(ExternalDepthTauBand::Close)
    } else if tau_sec <= cfg.warm_max_tau_sec {
        Some(ExternalDepthTauBand::Warm)
    } else {
        Some(ExternalDepthTauBand::Far)
    }
}

fn external_depth_adjustment_per_trigger_for_band(
    cfg: &EndgameCexDepthConfig,
    band: ExternalDepthTauBand,
) -> f64 {
    match band {
        ExternalDepthTauBand::Close => cfg.close_adjustment_per_trigger,
        ExternalDepthTauBand::Warm => cfg.warm_adjustment_per_trigger,
        ExternalDepthTauBand::Far => cfg.far_adjustment_per_trigger,
    }
    .clamp(0.0, 1.0)
}

#[derive(Debug, Clone, Copy)]
enum ExternalDepthTriggerGroup {
    AltCex,
    Coinbase,
    Binance,
    Hyperliquid,
}

impl ExternalDepthTriggerGroup {
    fn as_str(self) -> &'static str {
        match self {
            Self::AltCex => "alt_cex_coinbase_or_kraken_bitstamp",
            Self::Coinbase => "coinbase",
            Self::Binance => "binance",
            Self::Hyperliquid => "hyperliquid",
        }
    }

    fn rule_label(self) -> &'static str {
        match self {
            Self::AltCex => "coinbase_triggered OR (kraken_triggered AND bitstamp_triggered)",
            Self::Coinbase => "coinbase_triggered",
            Self::Binance => "binance_triggered",
            Self::Hyperliquid => "hyperliquid_triggered",
        }
    }

    fn venues(self) -> Vec<&'static str> {
        match self {
            Self::AltCex => vec![
                COINBASE_DEPTH_VENUE,
                KRAKEN_DEPTH_VENUE,
                BITSTAMP_DEPTH_VENUE,
            ],
            Self::Coinbase => vec![COINBASE_DEPTH_VENUE],
            Self::Binance => vec![BINANCE_SPOT_DEPTH_VENUE, BINANCE_FUTURES_DEPTH_VENUE],
            Self::Hyperliquid => vec![HYPERLIQUID_DEPTH_VENUE],
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ExternalDepthAction {
    Reduce,
    Increase,
}

#[derive(Debug, Clone)]
struct ExternalDepthGroupAction {
    triggered: bool,
    supporting_venues: Vec<&'static str>,
    adjustment: f64,
}

fn external_depth_window_stack_weight(window_ms: i64) -> f64 {
    match window_ms {
        3_600_000 => 1.0,
        7_200_000 => 0.5,
        14_400_000 => 0.25,
        _ => 1.0,
    }
}

fn external_depth_weighted_window_adjustment(adjustment: f64, window_ms: i64) -> f64 {
    if !adjustment.is_finite() || adjustment <= 0.0 {
        return 0.0;
    }
    (adjustment * external_depth_window_stack_weight(window_ms)).clamp(0.0, 1.0)
}

fn external_depth_trigger_groups(symbol: &str, timeframe: &str) -> Vec<ExternalDepthTriggerGroup> {
    let symbol = normalize_symbol(symbol);
    if !matches!(timeframe, "5m" | "15m") {
        return vec![ExternalDepthTriggerGroup::Binance];
    }
    match symbol.as_str() {
        "BTC" | "ETH" | "SOL" | "XRP" => vec![
            ExternalDepthTriggerGroup::AltCex,
            ExternalDepthTriggerGroup::Binance,
        ],
        "HYPE" => vec![
            ExternalDepthTriggerGroup::Coinbase,
            ExternalDepthTriggerGroup::Hyperliquid,
        ],
        _ => vec![
            ExternalDepthTriggerGroup::Coinbase,
            ExternalDepthTriggerGroup::Binance,
        ],
    }
}

fn external_depth_mode_label(symbol: &str, timeframe: &str) -> &'static str {
    if !matches!(timeframe, "5m" | "15m") {
        return "binance_only";
    }
    match normalize_symbol(symbol).as_str() {
        "BTC" | "ETH" | "SOL" | "XRP" => "alt_cex_group_plus_binance",
        "HYPE" => "coinbase_plus_hyperliquid",
        _ => "coinbase_plus_binance",
    }
}

fn external_depth_group_window_action_triggered(
    group: ExternalDepthTriggerGroup,
    venue_decisions: &[(&'static str, ExternalVenueDecision)],
    action: ExternalDepthAction,
    window_ms: i64,
) -> ExternalDepthGroupAction {
    let venue_adjustment = |venue: &'static str| -> f64 {
        venue_decisions
            .iter()
            .find(|(candidate, _)| *candidate == venue)
            .map(|(_, decision)| match action {
                ExternalDepthAction::Reduce => decision
                    .window_decisions
                    .iter()
                    .find(|window| window.window_ms == window_ms)
                    .map(|window| window.reduce_adjustment)
                    .unwrap_or(0.0),
                ExternalDepthAction::Increase => decision
                    .window_decisions
                    .iter()
                    .find(|window| window.window_ms == window_ms)
                    .map(|window| window.increase_adjustment)
                    .unwrap_or(0.0),
            })
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
    };
    match group {
        ExternalDepthTriggerGroup::AltCex => {
            let coinbase_adjustment = venue_adjustment(COINBASE_DEPTH_VENUE);
            let kraken_adjustment = venue_adjustment(KRAKEN_DEPTH_VENUE);
            let bitstamp_adjustment = venue_adjustment(BITSTAMP_DEPTH_VENUE);
            let pair_adjustment = if kraken_adjustment > 0.0 && bitstamp_adjustment > 0.0 {
                kraken_adjustment.min(bitstamp_adjustment)
            } else {
                0.0
            };
            let adjustment = coinbase_adjustment.max(pair_adjustment);
            let triggered = adjustment > 0.0;
            let mut supporting_venues = Vec::new();
            if coinbase_adjustment > 0.0 {
                supporting_venues.push(COINBASE_DEPTH_VENUE);
            }
            if pair_adjustment > 0.0 {
                supporting_venues.push(KRAKEN_DEPTH_VENUE);
                supporting_venues.push(BITSTAMP_DEPTH_VENUE);
            }
            ExternalDepthGroupAction {
                triggered,
                supporting_venues,
                adjustment,
            }
        }
        ExternalDepthTriggerGroup::Coinbase => {
            let adjustment = venue_adjustment(COINBASE_DEPTH_VENUE);
            ExternalDepthGroupAction {
                triggered: adjustment > 0.0,
                supporting_venues: if adjustment > 0.0 {
                    vec![COINBASE_DEPTH_VENUE]
                } else {
                    Vec::new()
                },
                adjustment,
            }
        }
        ExternalDepthTriggerGroup::Binance => {
            let spot = venue_adjustment(BINANCE_SPOT_DEPTH_VENUE);
            let futures = venue_adjustment(BINANCE_FUTURES_DEPTH_VENUE);
            let adjustment = spot.max(futures);
            ExternalDepthGroupAction {
                triggered: adjustment > 0.0,
                supporting_venues: if spot > 0.0 {
                    vec![BINANCE_SPOT_DEPTH_VENUE]
                } else if futures > 0.0 {
                    vec![BINANCE_FUTURES_DEPTH_VENUE]
                } else {
                    Vec::new()
                },
                adjustment,
            }
        }
        ExternalDepthTriggerGroup::Hyperliquid => {
            let adjustment = venue_adjustment(HYPERLIQUID_DEPTH_VENUE);
            ExternalDepthGroupAction {
                triggered: adjustment > 0.0,
                supporting_venues: if adjustment > 0.0 {
                    vec![HYPERLIQUID_DEPTH_VENUE]
                } else {
                    Vec::new()
                },
                adjustment,
            }
        }
    }
}

fn external_depth_group_delta_action_triggered(
    group: ExternalDepthTriggerGroup,
    venue_decisions: &[(&'static str, ExternalVenueDecision)],
    action: ExternalDepthAction,
) -> ExternalDepthGroupAction {
    let venue_adjustment = |venue: &'static str| -> f64 {
        venue_decisions
            .iter()
            .find(|(candidate, _)| *candidate == venue)
            .map(|(_, decision)| match action {
                ExternalDepthAction::Reduce => decision.delta_decision.reduce_adjustment,
                ExternalDepthAction::Increase => decision.delta_decision.increase_adjustment,
            })
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
    };
    match group {
        ExternalDepthTriggerGroup::AltCex => {
            let coinbase_adjustment = venue_adjustment(COINBASE_DEPTH_VENUE);
            let kraken_adjustment = venue_adjustment(KRAKEN_DEPTH_VENUE);
            let bitstamp_adjustment = venue_adjustment(BITSTAMP_DEPTH_VENUE);
            let pair_adjustment = if kraken_adjustment > 0.0 && bitstamp_adjustment > 0.0 {
                kraken_adjustment.min(bitstamp_adjustment)
            } else {
                0.0
            };
            let adjustment = coinbase_adjustment.max(pair_adjustment);
            let triggered = adjustment > 0.0;
            let mut supporting_venues = Vec::new();
            if coinbase_adjustment > 0.0 {
                supporting_venues.push(COINBASE_DEPTH_VENUE);
            }
            if pair_adjustment > 0.0 {
                supporting_venues.push(KRAKEN_DEPTH_VENUE);
                supporting_venues.push(BITSTAMP_DEPTH_VENUE);
            }
            ExternalDepthGroupAction {
                triggered,
                supporting_venues,
                adjustment,
            }
        }
        ExternalDepthTriggerGroup::Coinbase => {
            let adjustment = venue_adjustment(COINBASE_DEPTH_VENUE);
            ExternalDepthGroupAction {
                triggered: adjustment > 0.0,
                supporting_venues: if adjustment > 0.0 {
                    vec![COINBASE_DEPTH_VENUE]
                } else {
                    Vec::new()
                },
                adjustment,
            }
        }
        ExternalDepthTriggerGroup::Binance => {
            let spot = venue_adjustment(BINANCE_SPOT_DEPTH_VENUE);
            let futures = venue_adjustment(BINANCE_FUTURES_DEPTH_VENUE);
            let adjustment = spot.max(futures);
            ExternalDepthGroupAction {
                triggered: adjustment > 0.0,
                supporting_venues: if spot > 0.0 {
                    vec![BINANCE_SPOT_DEPTH_VENUE]
                } else if futures > 0.0 {
                    vec![BINANCE_FUTURES_DEPTH_VENUE]
                } else {
                    Vec::new()
                },
                adjustment,
            }
        }
        ExternalDepthTriggerGroup::Hyperliquid => {
            let adjustment = venue_adjustment(HYPERLIQUID_DEPTH_VENUE);
            ExternalDepthGroupAction {
                triggered: adjustment > 0.0,
                supporting_venues: if adjustment > 0.0 {
                    vec![HYPERLIQUID_DEPTH_VENUE]
                } else {
                    Vec::new()
                },
                adjustment,
            }
        }
    }
}

fn external_depth_eval_venues(symbol: &str, timeframe: &str) -> Vec<&'static str> {
    let mut venues = Vec::new();
    for group in external_depth_trigger_groups(symbol, timeframe) {
        for venue in group.venues() {
            if external_depth_venue_supported_for_symbol(venue, symbol) && !venues.contains(&venue)
            {
                venues.push(venue);
            }
        }
    }
    venues
}

fn external_depth_reason_for_group_action(
    group: ExternalDepthTriggerGroup,
    action: &str,
) -> &'static str {
    match (group, action) {
        (ExternalDepthTriggerGroup::AltCex, "increase") => "external_depth_increase_alt_cex",
        (ExternalDepthTriggerGroup::AltCex, _) => "external_depth_reduce_alt_cex",
        (ExternalDepthTriggerGroup::Coinbase, "increase") => "external_depth_increase_coinbase",
        (ExternalDepthTriggerGroup::Coinbase, _) => "external_depth_reduce_coinbase",
        (ExternalDepthTriggerGroup::Binance, "increase") => "external_depth_increase_binance",
        (ExternalDepthTriggerGroup::Binance, _) => "external_depth_reduce_binance",
        (ExternalDepthTriggerGroup::Hyperliquid, "increase") => {
            "external_depth_increase_hyperliquid"
        }
        (ExternalDepthTriggerGroup::Hyperliquid, _) => "external_depth_reduce_hyperliquid",
    }
}

#[derive(Debug, Clone)]
struct ExternalThresholdConfig {
    min_samples: usize,
    close_adjustment_per_trigger: f64,
    warm_adjustment_per_trigger: f64,
    far_adjustment_per_trigger: f64,
    increase_adjustment_per_trigger: f64,
    reduce_quantile: f64,
    increase_quantile: f64,
    close_reduce_quantile: f64,
    close_increase_quantile: f64,
    warm_reduce_quantile: f64,
    warm_increase_quantile: f64,
    floor_btc_usd: f64,
    floor_eth_usd: f64,
    floor_sol_usd: f64,
    floor_other_usd: f64,
    ceiling_btc_usd: f64,
    ceiling_eth_usd: f64,
    ceiling_sol_usd: f64,
    ceiling_other_usd: f64,
}

#[derive(Debug, Clone)]
struct ExternalCostBucketThreshold {
    windows: Vec<ExternalCostWindowThreshold>,
    warm_windows: Vec<ExternalCostWindowThreshold>,
    far_windows: Vec<ExternalCostWindowThreshold>,
    updated_ms: i64,
}

impl ExternalCostBucketThreshold {
    fn windows_for_band(&self, tau_band: ExternalDepthTauBand) -> &[ExternalCostWindowThreshold] {
        match tau_band {
            ExternalDepthTauBand::Close => self.windows.as_slice(),
            ExternalDepthTauBand::Warm => self.warm_windows.as_slice(),
            ExternalDepthTauBand::Far => self.far_windows.as_slice(),
        }
    }
}

#[derive(Debug, Clone)]
struct ExternalCostWindowThreshold {
    window_ms: i64,
    side: String,
    bucket_bps: u16,
    reduce_threshold_usd: f64,
    increase_threshold_usd: f64,
    reduce_severity_thresholds: [ExternalCostSeverityThreshold; EXTERNAL_DEPTH_SEVERITY_STEP_COUNT],
    increase_severity_thresholds:
        [ExternalCostSeverityThreshold; EXTERNAL_DEPTH_SEVERITY_STEP_COUNT],
    sample_count: usize,
    cold_start_floor_used: bool,
    reduce_quantile: f64,
    increase_quantile: f64,
    scaled_floor_usd: f64,
    scaled_ceiling_usd: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct ExternalDepthSeverityStep {
    quantile: f64,
    adjustment: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct ExternalCostSeverityThreshold {
    quantile: f64,
    adjustment: f64,
    threshold_usd: f64,
}

#[derive(Debug, Clone)]
struct ExternalDepthHistorySample {
    symbol: String,
    venue: &'static str,
    best_bid: f64,
    best_ask: f64,
    cost_to_buy_up_bps_usd: [Option<f64>; EXTERNAL_DEPTH_BUCKET_COUNT],
    cost_to_sell_down_bps_usd: [Option<f64>; EXTERNAL_DEPTH_BUCKET_COUNT],
    updated_ms: i64,
}

impl ExternalDepthHistorySample {
    fn from_snapshot(snapshot: &EndgameCexDepthSnapshot) -> Self {
        Self {
            symbol: snapshot.symbol.clone(),
            venue: snapshot.venue,
            best_bid: snapshot.best_bid,
            best_ask: snapshot.best_ask,
            cost_to_buy_up_bps_usd: snapshot.cost_to_buy_up_bps_usd,
            cost_to_sell_down_bps_usd: snapshot.cost_to_sell_down_bps_usd,
            updated_ms: snapshot.updated_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedExternalDepthHistorySample {
    symbol: String,
    venue: String,
    best_bid: f64,
    best_ask: f64,
    cost_to_buy_up_bps_usd: Vec<Option<f64>>,
    cost_to_sell_down_bps_usd: Vec<Option<f64>>,
    updated_ms: i64,
}

impl From<&ExternalDepthHistorySample> for PersistedExternalDepthHistorySample {
    fn from(sample: &ExternalDepthHistorySample) -> Self {
        Self {
            symbol: sample.symbol.clone(),
            venue: sample.venue.to_string(),
            best_bid: sample.best_bid,
            best_ask: sample.best_ask,
            cost_to_buy_up_bps_usd: sample.cost_to_buy_up_bps_usd.to_vec(),
            cost_to_sell_down_bps_usd: sample.cost_to_sell_down_bps_usd.to_vec(),
            updated_ms: sample.updated_ms,
        }
    }
}

impl TryFrom<PersistedExternalDepthHistorySample> for ExternalDepthHistorySample {
    type Error = anyhow::Error;

    fn try_from(sample: PersistedExternalDepthHistorySample) -> anyhow::Result<Self> {
        let venue = external_depth_venue_static(sample.venue.as_str()).ok_or_else(|| {
            anyhow::anyhow!("unsupported_endgame_external_depth_venue: {}", sample.venue)
        })?;
        Ok(Self {
            symbol: normalize_symbol(sample.symbol.as_str()),
            venue,
            best_bid: sample.best_bid,
            best_ask: sample.best_ask,
            cost_to_buy_up_bps_usd: vec_to_cost_array(sample.cost_to_buy_up_bps_usd),
            cost_to_sell_down_bps_usd: vec_to_cost_array(sample.cost_to_sell_down_bps_usd),
            updated_ms: sample.updated_ms,
        })
    }
}

fn external_bucket_threshold_stats(
    cfg: &ExternalThresholdConfig,
    symbol: &str,
    side: &str,
    bucket_bps: u16,
    history: &[ExternalDepthHistorySample],
    now_ms: i64,
) -> ExternalCostBucketThreshold {
    let side = normalize_direction(side);
    let bucket_idx = external_depth_bucket_index(bucket_bps).unwrap_or(0);
    let (floor, ceiling) = scaled_external_floor_ceiling(cfg, symbol, bucket_bps);
    let mut windows = Vec::new();
    let mut warm_windows = Vec::new();
    let mut far_windows = Vec::new();
    for window_ms in EXTERNAL_DEPTH_THRESHOLD_WINDOWS_MS {
        let min_ts_ms = now_ms.saturating_sub(window_ms);
        let mut costs = history
            .iter()
            .filter(|sample| sample.updated_ms >= min_ts_ms && sample.updated_ms <= now_ms)
            .filter_map(|sample| {
                if side == "UP" {
                    sample
                        .cost_to_buy_up_bps_usd
                        .get(bucket_idx)
                        .copied()
                        .flatten()
                } else {
                    sample
                        .cost_to_sell_down_bps_usd
                        .get(bucket_idx)
                        .copied()
                        .flatten()
                }
            })
            .filter(|cost| cost.is_finite() && *cost >= 0.0)
            .collect::<Vec<_>>();
        let sample_count = costs.len();
        let cold_start_floor_used = sample_count < cfg.min_samples;
        costs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        windows.push(external_cost_window_threshold_from_sorted(
            window_ms,
            side,
            bucket_bps,
            costs.as_slice(),
            sample_count,
            cold_start_floor_used,
            floor,
            ceiling,
            cfg.close_reduce_quantile,
            cfg.close_increase_quantile,
            external_threshold_severity_steps(
                cfg.close_reduce_quantile,
                &[cfg.close_adjustment_per_trigger; EXTERNAL_DEPTH_SEVERITY_STEP_COUNT],
                ExternalDepthAction::Reduce,
            )
            .as_slice(),
            external_threshold_severity_steps(
                cfg.close_increase_quantile,
                &[cfg.increase_adjustment_per_trigger; EXTERNAL_DEPTH_SEVERITY_STEP_COUNT],
                ExternalDepthAction::Increase,
            )
            .as_slice(),
        ));
        warm_windows.push(external_cost_window_threshold_from_sorted(
            window_ms,
            side,
            bucket_bps,
            costs.as_slice(),
            sample_count,
            cold_start_floor_used,
            floor,
            ceiling,
            cfg.warm_reduce_quantile,
            cfg.warm_increase_quantile,
            external_threshold_severity_steps(
                cfg.warm_reduce_quantile,
                &[cfg.warm_adjustment_per_trigger; EXTERNAL_DEPTH_SEVERITY_STEP_COUNT],
                ExternalDepthAction::Reduce,
            )
            .as_slice(),
            external_threshold_severity_steps(
                cfg.warm_increase_quantile,
                &[cfg.increase_adjustment_per_trigger; EXTERNAL_DEPTH_SEVERITY_STEP_COUNT],
                ExternalDepthAction::Increase,
            )
            .as_slice(),
        ));
        far_windows.push(external_cost_window_threshold_from_sorted(
            window_ms,
            side,
            bucket_bps,
            costs.as_slice(),
            sample_count,
            cold_start_floor_used,
            floor,
            ceiling,
            cfg.reduce_quantile,
            cfg.increase_quantile,
            external_threshold_severity_steps(
                cfg.reduce_quantile,
                &[cfg.far_adjustment_per_trigger; EXTERNAL_DEPTH_SEVERITY_STEP_COUNT],
                ExternalDepthAction::Reduce,
            )
            .as_slice(),
            external_threshold_severity_steps(
                cfg.increase_quantile,
                &[cfg.increase_adjustment_per_trigger; EXTERNAL_DEPTH_SEVERITY_STEP_COUNT],
                ExternalDepthAction::Increase,
            )
            .as_slice(),
        ));
    }
    ExternalCostBucketThreshold {
        windows,
        warm_windows,
        far_windows,
        updated_ms: now_ms,
    }
}

fn external_threshold_updates_for_history(
    cfg: &ExternalThresholdConfig,
    symbol: &str,
    venue: &'static str,
    history: &[ExternalDepthHistorySample],
    now_ms: i64,
) -> Vec<(
    (String, &'static str, String, u16),
    ExternalCostBucketThreshold,
)> {
    let symbol = normalize_symbol(symbol);
    let mut updates = Vec::with_capacity(EXTERNAL_DEPTH_BUCKET_COUNT * 2);
    for bucket_bps in EXTERNAL_DEPTH_BUCKETS_BPS {
        for side in ["UP", "DOWN"] {
            let threshold =
                external_bucket_threshold_stats(cfg, &symbol, side, bucket_bps, history, now_ms);
            updates.push((
                (symbol.clone(), venue, side.to_string(), bucket_bps),
                threshold,
            ));
        }
    }
    updates
}

fn initialized_external_bucket_threshold(
    threshold: &ExternalCostBucketThreshold,
) -> Option<ExternalCostBucketThreshold> {
    (threshold.updated_ms > 0
        && threshold
            .windows
            .iter()
            .any(|window| window.sample_count > 0))
    .then(|| threshold.clone())
}

fn external_cost_window_threshold_from_sorted(
    window_ms: i64,
    side: &str,
    bucket_bps: u16,
    costs: &[f64],
    sample_count: usize,
    cold_start_floor_used: bool,
    floor: f64,
    ceiling: f64,
    reduce_quantile: f64,
    increase_quantile: f64,
    reduce_severity_steps: &[ExternalDepthSeverityStep],
    increase_severity_steps: &[ExternalDepthSeverityStep],
) -> ExternalCostWindowThreshold {
    let reduce_threshold_usd = if cold_start_floor_used {
        floor.min(ceiling).max(0.0)
    } else {
        quantile_from_sorted(costs, reduce_quantile)
            .unwrap_or(floor)
            .max(floor)
    };
    let increase_threshold_usd = if cold_start_floor_used {
        ceiling.max(floor).max(0.0)
    } else {
        quantile_from_sorted(costs, increase_quantile)
            .unwrap_or(ceiling)
            .max(floor)
    };
    ExternalCostWindowThreshold {
        window_ms,
        side: side.to_string(),
        bucket_bps,
        reduce_threshold_usd,
        increase_threshold_usd,
        reduce_severity_thresholds: external_cost_severity_thresholds_from_sorted(
            costs,
            cold_start_floor_used,
            floor,
            ceiling,
            reduce_severity_steps,
        ),
        increase_severity_thresholds: external_cost_severity_thresholds_from_sorted(
            costs,
            cold_start_floor_used,
            floor,
            ceiling,
            increase_severity_steps,
        ),
        sample_count,
        cold_start_floor_used,
        reduce_quantile,
        increase_quantile,
        scaled_floor_usd: floor,
        scaled_ceiling_usd: ceiling,
    }
}

fn external_threshold_severity_steps(
    start_quantile: f64,
    adjustments: &[f64],
    action: ExternalDepthAction,
) -> [ExternalDepthSeverityStep; EXTERNAL_DEPTH_SEVERITY_STEP_COUNT] {
    let mut out = [ExternalDepthSeverityStep::default(); EXTERNAL_DEPTH_SEVERITY_STEP_COUNT];
    for (idx, adjustment) in adjustments
        .iter()
        .copied()
        .take(EXTERNAL_DEPTH_SEVERITY_STEP_COUNT)
        .enumerate()
    {
        if !adjustment.is_finite() || adjustment <= 0.0 {
            continue;
        }
        let step = idx as f64 * 0.01;
        let quantile = match action {
            ExternalDepthAction::Reduce => (start_quantile - step).clamp(0.001, 0.50),
            ExternalDepthAction::Increase => (start_quantile + step).clamp(0.50, 0.999),
        };
        out[idx] = ExternalDepthSeverityStep {
            quantile,
            adjustment: adjustment.clamp(0.0, 1.0),
        };
    }
    out
}

fn external_cost_severity_thresholds_from_sorted(
    costs: &[f64],
    cold_start_floor_used: bool,
    floor: f64,
    ceiling: f64,
    steps: &[ExternalDepthSeverityStep],
) -> [ExternalCostSeverityThreshold; EXTERNAL_DEPTH_SEVERITY_STEP_COUNT] {
    let mut out = [ExternalCostSeverityThreshold::default(); EXTERNAL_DEPTH_SEVERITY_STEP_COUNT];
    for (idx, step) in steps.iter().enumerate() {
        if step.adjustment <= 0.0 {
            continue;
        }
        let default_threshold = if step.quantile <= 0.50 {
            floor
        } else {
            ceiling
        };
        let threshold_usd = if cold_start_floor_used {
            default_threshold
        } else {
            quantile_from_sorted(costs, step.quantile)
                .unwrap_or(default_threshold)
                .max(floor)
        };
        out[idx] = ExternalCostSeverityThreshold {
            quantile: step.quantile,
            adjustment: step.adjustment,
            threshold_usd,
        };
    }
    out
}

fn external_depth_reduce_adjustment_for_cost(
    cost: f64,
    thresholds: &[ExternalCostSeverityThreshold],
) -> f64 {
    thresholds
        .iter()
        .filter(|threshold| threshold.adjustment > 0.0 && cost <= threshold.threshold_usd)
        .map(|threshold| threshold.adjustment)
        .fold(0.0_f64, f64::max)
        .clamp(0.0, 1.0)
}

fn external_depth_increase_adjustment_for_cost(
    cost: f64,
    thresholds: &[ExternalCostSeverityThreshold],
) -> f64 {
    thresholds
        .iter()
        .filter(|threshold| threshold.adjustment > 0.0 && cost >= threshold.threshold_usd)
        .map(|threshold| threshold.adjustment)
        .fold(0.0_f64, f64::max)
        .clamp(0.0, 1.0)
}

fn quantile_from_sorted(clean: &[f64], q: f64) -> Option<f64> {
    if clean.is_empty() {
        return None;
    }
    let idx = ((clean.len().saturating_sub(1)) as f64 * q.clamp(0.0, 1.0)).round() as usize;
    clean.get(idx.min(clean.len().saturating_sub(1))).copied()
}

fn external_ceiling_usd(cfg: &ExternalThresholdConfig, symbol: &str) -> f64 {
    match normalize_symbol(symbol).as_str() {
        "BTC" => cfg.ceiling_btc_usd,
        "ETH" => cfg.ceiling_eth_usd,
        "SOL" => cfg.ceiling_sol_usd,
        _ => cfg.ceiling_other_usd,
    }
}

fn external_floor_usd_from_threshold_cfg(cfg: &ExternalThresholdConfig, symbol: &str) -> f64 {
    match normalize_symbol(symbol).as_str() {
        "BTC" => cfg.floor_btc_usd,
        "ETH" => cfg.floor_eth_usd,
        "SOL" => cfg.floor_sol_usd,
        _ => cfg.floor_other_usd,
    }
}

fn scaled_external_floor_ceiling(
    cfg: &ExternalThresholdConfig,
    symbol: &str,
    bucket_bps: u16,
) -> (f64, f64) {
    let scale = (bucket_bps as f64 / 5.0).max(0.0);
    let floor = external_floor_usd_from_threshold_cfg(cfg, symbol) * scale;
    let ceiling = (external_ceiling_usd(cfg, symbol) * scale).max(floor);
    (floor.max(0.0), ceiling.max(0.0))
}

#[derive(Debug, Deserialize)]
struct BinanceDepthResponse {
    bids: Vec<[String; 2]>,
    asks: Vec<[String; 2]>,
}

#[derive(Debug, Deserialize)]
struct CoinbaseProductBookResponse {
    pricebook: CoinbasePriceBook,
}

#[derive(Debug, Deserialize)]
struct CoinbasePriceBook {
    bids: Vec<CoinbaseDepthLevelRaw>,
    asks: Vec<CoinbaseDepthLevelRaw>,
}

#[derive(Debug, Deserialize)]
struct CoinbaseDepthLevelRaw {
    price: String,
    size: String,
}

#[derive(Debug, Deserialize)]
struct HyperliquidL2BookResponse {
    time: i64,
    levels: Vec<Vec<HyperliquidDepthLevelRaw>>,
}

#[derive(Debug, Deserialize)]
struct HyperliquidDepthLevelRaw {
    px: String,
    sz: String,
}

#[derive(Debug, Clone)]
struct ExternalDepthFetchTarget {
    symbol: String,
    venue: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalDepthPollMode {
    Hot,
    Warm,
    Cold,
}

#[derive(Debug, Clone, Default)]
struct ExternalFetchBackoff {
    next_allowed_ms: i64,
    failure_streak: u32,
}

async fn fetch_external_depth_snapshot(
    client: &reqwest::Client,
    target: &ExternalDepthFetchTarget,
) -> anyhow::Result<EndgameCexDepthSnapshot> {
    if target.venue == COINBASE_DEPTH_VENUE {
        fetch_coinbase_depth_snapshot(client, target.symbol.as_str()).await
    } else if target.venue == KRAKEN_DEPTH_VENUE {
        fetch_kraken_depth_snapshot(client, target.symbol.as_str()).await
    } else if target.venue == BITSTAMP_DEPTH_VENUE {
        fetch_bitstamp_depth_snapshot(client, target.symbol.as_str()).await
    } else if target.venue == HYPERLIQUID_DEPTH_VENUE {
        fetch_hyperliquid_depth_snapshot(client, target.symbol.as_str()).await
    } else {
        fetch_binance_depth_snapshot(client, target.symbol.as_str()).await
    }
}

async fn fetch_binance_depth_snapshot(
    client: &reqwest::Client,
    symbol: &str,
) -> anyhow::Result<EndgameCexDepthSnapshot> {
    let pair = binance_pair_for_symbol(symbol);
    let venue = binance_depth_venue_for_symbol(symbol);
    let url = if normalize_symbol(symbol) == "HYPE" {
        format!("https://fapi.binance.com/fapi/v1/depth?symbol={pair}&limit=100")
    } else {
        format!("https://api.binance.com/api/v3/depth?symbol={pair}&limit=100")
    };
    let response = client.get(url).send().await?;
    let status = response.status();
    let retry_after_sec = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let body = response.text().await?;
    if !status.is_success() {
        anyhow::bail!(
            "binance_depth_status_{} retry_after_sec={}: {}",
            status.as_u16(),
            retry_after_sec.as_deref().unwrap_or(""),
            body
        );
    }
    let parsed: BinanceDepthResponse = serde_json::from_str(body.as_str())?;
    let mut bids = parsed
        .bids
        .into_iter()
        .filter_map(parse_external_level)
        .collect::<Vec<_>>();
    let mut asks = parsed
        .asks
        .into_iter()
        .filter_map(parse_external_level)
        .collect::<Vec<_>>();
    build_external_depth_snapshot(symbol, venue, &mut bids, &mut asks)
}

async fn fetch_coinbase_depth_snapshot(
    client: &reqwest::Client,
    symbol: &str,
) -> anyhow::Result<EndgameCexDepthSnapshot> {
    let Some(product_id) = coinbase_depth_product_for_symbol(symbol) else {
        anyhow::bail!("coinbase_depth_unsupported_symbol: {}", symbol);
    };
    let url =
        format!("https://api.coinbase.com/api/v3/brokerage/market/product_book?product_id={product_id}&limit=100");
    let response = client
        .get(url)
        .header("User-Agent", "EVPOLY-endgame-depth/1.0")
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("coinbase_depth_status_{}: {}", status.as_u16(), body);
    }
    let parsed: CoinbaseProductBookResponse = serde_json::from_str(body.as_str())?;
    let mut bids = parsed
        .pricebook
        .bids
        .into_iter()
        .filter_map(parse_coinbase_external_level)
        .collect::<Vec<_>>();
    let mut asks = parsed
        .pricebook
        .asks
        .into_iter()
        .filter_map(parse_coinbase_external_level)
        .collect::<Vec<_>>();
    build_external_depth_snapshot(symbol, COINBASE_DEPTH_VENUE, &mut bids, &mut asks)
}

async fn fetch_kraken_depth_snapshot(
    client: &reqwest::Client,
    symbol: &str,
) -> anyhow::Result<EndgameCexDepthSnapshot> {
    let Some(pair) = kraken_depth_pair_for_symbol(symbol) else {
        anyhow::bail!("kraken_depth_unsupported_symbol: {}", symbol);
    };
    let url = format!("https://api.kraken.com/0/public/Depth?pair={pair}&count=100");
    let response = client
        .get(url)
        .header("User-Agent", "EVPOLY-endgame-depth/1.0")
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("kraken_depth_status_{}: {}", status.as_u16(), body);
    }
    let value: Value = serde_json::from_str(body.as_str())?;
    if value
        .get("error")
        .and_then(Value::as_array)
        .map(|errors| !errors.is_empty())
        .unwrap_or(false)
    {
        anyhow::bail!("kraken_depth_error: {}", body);
    }
    let book = value
        .get("result")
        .and_then(Value::as_object)
        .and_then(|result| result.values().next())
        .ok_or_else(|| anyhow::anyhow!("kraken_depth_missing_book"))?;
    let mut bids = parse_external_json_levels(book.get("bids"));
    let mut asks = parse_external_json_levels(book.get("asks"));
    build_external_depth_snapshot(symbol, KRAKEN_DEPTH_VENUE, &mut bids, &mut asks)
}

async fn fetch_bitstamp_depth_snapshot(
    client: &reqwest::Client,
    symbol: &str,
) -> anyhow::Result<EndgameCexDepthSnapshot> {
    let Some(pair) = bitstamp_depth_pair_for_symbol(symbol) else {
        anyhow::bail!("bitstamp_depth_unsupported_symbol: {}", symbol);
    };
    let url = format!("https://www.bitstamp.net/api/v2/order_book/{pair}/?group=1");
    let response = client
        .get(url)
        .header("User-Agent", "EVPOLY-endgame-depth/1.0")
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("bitstamp_depth_status_{}: {}", status.as_u16(), body);
    }
    let value: Value = serde_json::from_str(body.as_str())?;
    let mut bids = parse_external_json_levels(value.get("bids"));
    let mut asks = parse_external_json_levels(value.get("asks"));
    build_external_depth_snapshot(symbol, BITSTAMP_DEPTH_VENUE, &mut bids, &mut asks)
}

async fn fetch_hyperliquid_depth_snapshot(
    client: &reqwest::Client,
    symbol: &str,
) -> anyhow::Result<EndgameCexDepthSnapshot> {
    if normalize_symbol(symbol) != "HYPE" {
        anyhow::bail!("hyperliquid_depth_unsupported_symbol: {}", symbol);
    }
    let response = client
        .post("https://api.hyperliquid.xyz/info")
        .header("User-Agent", "EVPOLY-endgame-depth/1.0")
        .json(&json!({
            "type": "l2Book",
            "coin": "HYPE"
        }))
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("hyperliquid_depth_status_{}: {}", status.as_u16(), body);
    }
    let parsed: HyperliquidL2BookResponse = serde_json::from_str(body.as_str())?;
    let mut bids = parsed
        .levels
        .first()
        .into_iter()
        .flat_map(|levels| levels.iter())
        .filter_map(parse_hyperliquid_external_level)
        .collect::<Vec<_>>();
    let mut asks = parsed
        .levels
        .get(1)
        .into_iter()
        .flat_map(|levels| levels.iter())
        .filter_map(parse_hyperliquid_external_level)
        .collect::<Vec<_>>();
    let mut snapshot =
        build_external_depth_snapshot(symbol, HYPERLIQUID_DEPTH_VENUE, &mut bids, &mut asks)?;
    if parsed.time > 0 {
        snapshot.updated_ms = parsed.time;
    }
    Ok(snapshot)
}

fn build_external_depth_snapshot(
    symbol: &str,
    venue: &'static str,
    bids: &mut Vec<EndgameCexDepthLevel>,
    asks: &mut Vec<EndgameCexDepthLevel>,
) -> anyhow::Result<EndgameCexDepthSnapshot> {
    bids.sort_by(|a, b| b.price.partial_cmp(&a.price).unwrap_or(Ordering::Equal));
    asks.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(Ordering::Equal));
    let best_bid = bids.first().map(|level| level.price).unwrap_or(0.0);
    let best_ask = asks.first().map(|level| level.price).unwrap_or(0.0);
    if best_bid <= 0.0 || best_ask <= 0.0 || best_ask <= best_bid {
        anyhow::bail!(
            "invalid_external_depth_book: venue={} best_bid={} best_ask={}",
            venue,
            best_bid,
            best_ask
        );
    }
    let mid = (best_bid + best_ask) / 2.0;
    let spread_bps = ((best_ask / best_bid) - 1.0) * 10_000.0;
    Ok(EndgameCexDepthSnapshot {
        symbol: normalize_symbol(symbol),
        venue,
        best_bid,
        best_ask,
        spread_bps,
        bids: bids.clone(),
        asks: asks.clone(),
        cost_to_buy_up_bps_usd: cost_to_buy_up_bps_buckets(asks, mid),
        cost_to_sell_down_bps_usd: cost_to_sell_down_bps_buckets(bids, mid),
        updated_ms: chrono::Utc::now().timestamp_millis(),
    })
}

fn parse_external_level(level: [String; 2]) -> Option<EndgameCexDepthLevel> {
    let price = level[0].parse::<f64>().ok()?;
    let size = level[1].parse::<f64>().ok()?;
    (price.is_finite() && price > 0.0 && size.is_finite() && size > 0.0)
        .then_some(EndgameCexDepthLevel { price, size })
}

fn parse_coinbase_external_level(level: CoinbaseDepthLevelRaw) -> Option<EndgameCexDepthLevel> {
    let price = level.price.parse::<f64>().ok()?;
    let size = level.size.parse::<f64>().ok()?;
    (price.is_finite() && price > 0.0 && size.is_finite() && size > 0.0)
        .then_some(EndgameCexDepthLevel { price, size })
}

fn parse_hyperliquid_external_level(
    level: &HyperliquidDepthLevelRaw,
) -> Option<EndgameCexDepthLevel> {
    let price = level.px.parse::<f64>().ok()?;
    let size = level.sz.parse::<f64>().ok()?;
    (price.is_finite() && price > 0.0 && size.is_finite() && size > 0.0)
        .then_some(EndgameCexDepthLevel { price, size })
}

fn parse_external_json_levels(value: Option<&Value>) -> Vec<EndgameCexDepthLevel> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flat_map(|levels| levels.iter())
        .filter_map(|row| {
            let fields = row.as_array()?;
            let price = fields
                .first()
                .and_then(Value::as_str)
                .and_then(|raw| raw.parse::<f64>().ok())?;
            let size = fields
                .get(1)
                .and_then(Value::as_str)
                .and_then(|raw| raw.parse::<f64>().ok())?;
            (price.is_finite() && price > 0.0 && size.is_finite() && size > 0.0)
                .then_some(EndgameCexDepthLevel { price, size })
        })
        .collect()
}

pub fn cost_to_buy_up_to(asks: &[EndgameCexDepthLevel], target_price: f64) -> (f64, bool) {
    if !target_price.is_finite() || target_price <= 0.0 {
        return (f64::INFINITY, false);
    }
    let mut cost = 0.0;
    let mut reached = false;
    let mut asks = asks.to_vec();
    asks.sort_by(|left, right| {
        left.price
            .partial_cmp(&right.price)
            .unwrap_or(Ordering::Equal)
    });
    for level in asks {
        if !level.price.is_finite()
            || !level.size.is_finite()
            || level.price <= 0.0
            || level.size <= 0.0
        {
            continue;
        }
        cost += level.price * level.size;
        if level.price >= target_price {
            reached = true;
            break;
        }
    }
    (cost, reached)
}

pub fn cost_to_sell_down_to(bids: &[EndgameCexDepthLevel], target_price: f64) -> (f64, bool) {
    if !target_price.is_finite() || target_price <= 0.0 {
        return (f64::INFINITY, false);
    }
    let mut cost = 0.0;
    let mut reached = false;
    let mut bids = bids.to_vec();
    bids.sort_by(|left, right| {
        right
            .price
            .partial_cmp(&left.price)
            .unwrap_or(Ordering::Equal)
    });
    for level in bids {
        if !level.price.is_finite()
            || !level.size.is_finite()
            || level.price <= 0.0
            || level.size <= 0.0
        {
            continue;
        }
        cost += level.price * level.size;
        if level.price <= target_price {
            reached = true;
            break;
        }
    }
    (cost, reached)
}

fn cost_to_buy_up_bps_buckets(
    asks: &[EndgameCexDepthLevel],
    mid: f64,
) -> [Option<f64>; EXTERNAL_DEPTH_BUCKET_COUNT] {
    let mut out = [None; EXTERNAL_DEPTH_BUCKET_COUNT];
    if !mid.is_finite() || mid <= 0.0 {
        return out;
    }
    for bucket_bps in EXTERNAL_DEPTH_BUCKETS_BPS {
        let target_price = mid * (1.0 + (bucket_bps as f64 / 10_000.0));
        let (cost, reached) = cost_to_buy_up_to(asks, target_price);
        if reached {
            if let Some(idx) = external_depth_bucket_index(bucket_bps) {
                out[idx] = Some(cost);
            }
        }
    }
    out
}

fn cost_to_sell_down_bps_buckets(
    bids: &[EndgameCexDepthLevel],
    mid: f64,
) -> [Option<f64>; EXTERNAL_DEPTH_BUCKET_COUNT] {
    let mut out = [None; EXTERNAL_DEPTH_BUCKET_COUNT];
    if !mid.is_finite() || mid <= 0.0 {
        return out;
    }
    for bucket_bps in EXTERNAL_DEPTH_BUCKETS_BPS {
        let target_price = mid * (1.0 - (bucket_bps as f64 / 10_000.0));
        let (cost, reached) = cost_to_sell_down_to(bids, target_price);
        if reached {
            if let Some(idx) = external_depth_bucket_index(bucket_bps) {
                out[idx] = Some(cost);
            }
        }
    }
    out
}

fn external_depth_bucket_index(bucket_bps: u16) -> Option<usize> {
    EXTERNAL_DEPTH_BUCKETS_BPS
        .iter()
        .position(|candidate| *candidate == bucket_bps)
}

fn external_depth_bucket_for_actual_bps(actual_bps: f64) -> u16 {
    if !actual_bps.is_finite() || actual_bps <= 0.0 {
        return EXTERNAL_DEPTH_BUCKET_MIN_BPS;
    }
    let rounded = actual_bps.ceil();
    EXTERNAL_DEPTH_BUCKETS_BPS
        .iter()
        .copied()
        .find(|bucket| rounded <= *bucket as f64)
        .unwrap_or(EXTERNAL_DEPTH_BUCKET_MAX_BPS)
}

fn latest_external_depth_at_or_before(
    snapshots: &VecDeque<ExternalDepthHistorySample>,
    ts_ms: i64,
    max_age_ms: i64,
) -> Option<ExternalDepthHistorySample> {
    snapshots
        .iter()
        .rev()
        .find(|snapshot| {
            snapshot.updated_ms <= ts_ms && ts_ms.saturating_sub(snapshot.updated_ms) <= max_age_ms
        })
        .cloned()
}

fn first_external_depth_at_or_after(
    snapshots: &VecDeque<ExternalDepthHistorySample>,
    ts_ms: i64,
    max_after_ms: i64,
) -> Option<ExternalDepthHistorySample> {
    let latest_allowed_ts_ms = ts_ms.saturating_add(max_after_ms.max(0));
    snapshots
        .iter()
        .find(|snapshot| {
            snapshot.updated_ms >= ts_ms && snapshot.updated_ms <= latest_allowed_ts_ms
        })
        .cloned()
}

fn external_depth_sample_mid(snapshot: &ExternalDepthHistorySample) -> Option<f64> {
    if snapshot.best_bid.is_finite()
        && snapshot.best_bid > 0.0
        && snapshot.best_ask.is_finite()
        && snapshot.best_ask > snapshot.best_bid
    {
        Some((snapshot.best_bid + snapshot.best_ask) / 2.0)
    } else {
        None
    }
}

fn external_depth_base_mid_at_or_near_open(
    snapshots: &VecDeque<ExternalDepthHistorySample>,
    open_ms: i64,
    max_after_open_ms: i64,
) -> Option<f64> {
    latest_external_depth_at_or_before(snapshots, open_ms, max_after_open_ms)
        .and_then(|snapshot| external_depth_sample_mid(&snapshot))
        .or_else(|| {
            first_external_depth_at_or_after(snapshots, open_ms, max_after_open_ms)
                .and_then(|snapshot| external_depth_sample_mid(&snapshot))
        })
}

fn external_depth_history_max_samples(history_keep_ms: i64, history_sample_ms: i64) -> usize {
    let sample_ms = history_sample_ms.max(1);
    let keep_ms = history_keep_ms.max(sample_ms);
    usize::try_from(keep_ms.saturating_add(sample_ms - 1) / sample_ms)
        .ok()
        .unwrap_or(0)
        .saturating_add(8)
        .max(8)
}

fn cap_external_depth_history_len(
    deque: &mut VecDeque<ExternalDepthHistorySample>,
    history_keep_ms: i64,
    history_sample_ms: i64,
) {
    let max_samples = external_depth_history_max_samples(history_keep_ms, history_sample_ms);
    while deque.len() > max_samples {
        deque.pop_front();
    }
}

fn prune_external_depth_by_ts(deque: &mut VecDeque<ExternalDepthHistorySample>, min_ts_ms: i64) {
    while deque
        .front()
        .map(|sample| sample.updated_ms < min_ts_ms)
        .unwrap_or(false)
    {
        deque.pop_front();
    }
}

fn external_delta_history_keep_ms(cfg: &EndgameCexDepthConfig) -> i64 {
    cfg.delta_lookback_ms
        .saturating_add(cfg.max_age_ms)
        .saturating_add(2_000)
        .clamp(5_000, 60_000)
}

fn make_external_history_ordered_unique(deque: &mut VecDeque<ExternalDepthHistorySample>) {
    let mut samples = deque.drain(..).collect::<Vec<_>>();
    samples.sort_by(|a, b| a.updated_ms.cmp(&b.updated_ms));
    samples.dedup_by(|a, b| a.updated_ms == b.updated_ms);
    deque.extend(samples);
}

async fn load_external_depth_history_from_disk(
    cache: &EndgameCexDepthCache,
    cfg: &EndgameCexDepthConfig,
) -> anyhow::Result<usize> {
    let path = PathBuf::from(cfg.persist_path.as_str());
    if !path.exists() {
        return Ok(0);
    }
    let file = tokio::fs::File::open(path).await?;
    let mut lines = BufReader::new(file).lines();
    let mut loaded = 0_usize;
    let mut skipped = 0_usize;
    let min_ms = chrono::Utc::now()
        .timestamp_millis()
        .saturating_sub(cfg.history_keep_ms);
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let persisted = match serde_json::from_str::<PersistedExternalDepthHistorySample>(line) {
            Ok(persisted) => persisted,
            Err(_) => {
                skipped = skipped.saturating_add(1);
                continue;
            }
        };
        let sample = match ExternalDepthHistorySample::try_from(persisted) {
            Ok(sample) => sample,
            Err(_) => {
                skipped = skipped.saturating_add(1);
                continue;
            }
        };
        if sample.updated_ms < min_ms {
            continue;
        }
        cache.load_history_sample(sample, cfg);
        loaded = loaded.saturating_add(1);
    }
    if skipped > 0 {
        log_event(
            "endgame_external_depth_history_load_skipped_lines",
            json!({
                "strategy_id": STRATEGY_ID,
                "path": cfg.persist_path,
                "skipped_lines": skipped,
                "loaded_samples": loaded
            }),
        );
    }
    if loaded > 0 {
        cache.recompute_all_thresholds(chrono::Utc::now().timestamp_millis(), cfg);
    }
    Ok(loaded)
}

async fn persist_external_depth_history_to_disk(
    cache: &EndgameCexDepthCache,
    cfg: &EndgameCexDepthConfig,
    since_updated_ms: i64,
    compact: bool,
) -> anyhow::Result<(bool, Option<i64>)> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut samples = cache.history_samples_for_persist(now_ms, cfg);
    if !compact {
        samples.retain(|sample| sample.updated_ms > since_updated_ms);
    }
    if samples.is_empty() {
        return Ok((false, None));
    }
    let path = PathBuf::from(cfg.persist_path.as_str());
    ensure_parent_dir(path.as_path()).await?;
    let mut max_updated_ms = None;
    if !compact {
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.as_path())
            .await?;
        for sample in samples.iter() {
            max_updated_ms = Some(max_updated_ms.unwrap_or(0_i64).max(sample.updated_ms));
            let persisted = PersistedExternalDepthHistorySample::from(sample);
            let line = serde_json::to_vec(&persisted)?;
            file.write_all(line.as_slice()).await?;
            file.write_all(b"\n").await?;
        }
        file.flush().await?;
        return Ok((true, max_updated_ms));
    }

    let tmp_path = path.with_extension("jsonl.tmp");
    let backup_path = path.with_extension("jsonl.bak");
    let mut file = tokio::fs::File::create(tmp_path.as_path()).await?;
    for sample in samples.iter() {
        max_updated_ms = Some(max_updated_ms.unwrap_or(0_i64).max(sample.updated_ms));
        let persisted = PersistedExternalDepthHistorySample::from(sample);
        let line = serde_json::to_vec(&persisted)?;
        file.write_all(line.as_slice()).await?;
        file.write_all(b"\n").await?;
    }
    file.flush().await?;
    drop(file);
    if path.exists() {
        let _ = tokio::fs::remove_file(backup_path.as_path()).await;
        tokio::fs::rename(path.as_path(), backup_path.as_path()).await?;
    }
    if let Err(err) = tokio::fs::rename(tmp_path.as_path(), path.as_path()).await {
        if backup_path.exists() && !path.exists() {
            let _ = tokio::fs::rename(backup_path.as_path(), path.as_path()).await;
        }
        return Err(err.into());
    }
    let _ = tokio::fs::remove_file(backup_path.as_path()).await;
    Ok((true, max_updated_ms))
}

async fn ensure_parent_dir(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    Ok(())
}

fn vec_to_cost_array(values: Vec<Option<f64>>) -> [Option<f64>; EXTERNAL_DEPTH_BUCKET_COUNT] {
    let mut out = [None; EXTERNAL_DEPTH_BUCKET_COUNT];
    for (idx, value) in values
        .into_iter()
        .take(EXTERNAL_DEPTH_BUCKET_COUNT)
        .enumerate()
    {
        out[idx] = value.filter(|cost| cost.is_finite() && *cost >= 0.0);
    }
    out
}

fn external_depth_loop_tick_ms(
    cfg: &EndgameCexDepthConfig,
    targets: &[ExternalDepthFetchTarget],
) -> u64 {
    let mut tick_ms = cfg
        .poll_ms
        .min(cfg.coinbase_poll_ms)
        .min(cfg.binance_poll_ms)
        .min(cfg.warm_poll_ms)
        .max(1);
    for target in targets {
        tick_ms = tick_ms.min(external_depth_target_poll_ms(cfg, target));
    }
    tick_ms.max(1)
}

fn external_depth_target_poll_ms_for_venue(cfg: &EndgameCexDepthConfig, venue: &str) -> u64 {
    if venue == COINBASE_DEPTH_VENUE {
        cfg.coinbase_poll_ms.max(1)
    } else if external_depth_is_slow_rest_venue(venue) {
        cfg.binance_poll_ms.max(1)
    } else {
        cfg.poll_ms.max(1)
    }
}

fn external_depth_target_poll_ms_for_target(
    cfg: &EndgameCexDepthConfig,
    target: &ExternalDepthFetchTarget,
) -> u64 {
    if target.venue == COINBASE_DEPTH_VENUE && normalize_symbol(target.symbol.as_str()) == "DOGE" {
        return DOGE_COINBASE_REST_DEPTH_POLL_MS;
    }
    external_depth_target_poll_ms_for_venue(cfg, target.venue)
}

fn external_depth_target_poll_ms(
    cfg: &EndgameCexDepthConfig,
    target: &ExternalDepthFetchTarget,
) -> u64 {
    external_depth_target_poll_ms_for_target(cfg, target)
}

fn external_depth_target_poll_ms_for_mode(
    cfg: &EndgameCexDepthConfig,
    target: &ExternalDepthFetchTarget,
    mode: ExternalDepthPollMode,
) -> u64 {
    if !cfg.adaptive_poll_enabled {
        return external_depth_target_poll_ms(cfg, target);
    }
    match mode {
        ExternalDepthPollMode::Hot => external_depth_target_poll_ms(cfg, target),
        ExternalDepthPollMode::Warm => cfg.warm_poll_ms.max(1),
        ExternalDepthPollMode::Cold => cfg.cold_poll_ms.max(1),
    }
}

fn external_depth_target_poll_mode(
    target: &ExternalDepthFetchTarget,
    timeframes: &[String],
    now_ms: i64,
    cfg: &EndgameCexDepthConfig,
) -> ExternalDepthPollMode {
    if !cfg.adaptive_poll_enabled {
        return ExternalDepthPollMode::Hot;
    }
    let now_ts = now_ms.div_euclid(1_000);
    let mut warm = false;
    for timeframe in timeframes {
        let Some(period_sec) = external_depth_timeframe_period_sec(timeframe.as_str()) else {
            continue;
        };
        if !external_depth_eval_venues(target.symbol.as_str(), timeframe.as_str())
            .iter()
            .any(|venue| *venue == target.venue)
        {
            continue;
        }
        let current_close_ts = now_ts.div_euclid(period_sec).saturating_mul(period_sec);
        let next_close_ts = current_close_ts.saturating_add(period_sec);
        for close_ts in [current_close_ts, next_close_ts] {
            if now_ts >= close_ts.saturating_sub(cfg.hot_before_sec)
                && now_ts <= close_ts.saturating_add(cfg.hot_after_sec)
            {
                return ExternalDepthPollMode::Hot;
            }
            if now_ts >= close_ts.saturating_sub(cfg.warm_before_sec)
                && now_ts < close_ts.saturating_sub(cfg.hot_before_sec)
            {
                warm = true;
            }
        }
    }
    if warm {
        ExternalDepthPollMode::Warm
    } else {
        ExternalDepthPollMode::Cold
    }
}

fn external_depth_timeframe_period_sec(timeframe: &str) -> Option<i64> {
    match timeframe {
        "5m" => Some(5 * 60),
        "15m" => Some(15 * 60),
        "1h" | "60m" => Some(60 * 60),
        _ => None,
    }
}

fn external_depth_is_binance_venue(venue: &str) -> bool {
    matches!(
        venue,
        BINANCE_SPOT_DEPTH_VENUE | BINANCE_FUTURES_DEPTH_VENUE
    )
}

fn external_depth_is_slow_rest_venue(venue: &str) -> bool {
    external_depth_is_binance_venue(venue)
        || matches!(
            venue,
            KRAKEN_DEPTH_VENUE | BITSTAMP_DEPTH_VENUE | HYPERLIQUID_DEPTH_VENUE
        )
}

fn external_depth_binance_rate_limit_backoff_ms(
    venue: &str,
    err: &anyhow::Error,
    default_backoff_ms: u64,
) -> Option<u64> {
    if !external_depth_is_binance_venue(venue) {
        return None;
    }
    let error = err.to_string();
    let rate_limited = error.contains("binance_depth_status_429")
        || error.contains("binance_depth_status_418")
        || error.contains("Too much request weight")
        || error.contains("\"code\":-1003");
    if !rate_limited {
        return None;
    }
    Some(
        external_depth_retry_after_ms(error.as_str())
            .unwrap_or(default_backoff_ms)
            .max(default_backoff_ms),
    )
}

fn external_depth_retry_after_ms(error: &str) -> Option<u64> {
    let marker = "retry_after_sec=";
    let start = error.find(marker)? + marker.len();
    let retry_after = error[start..]
        .split(|ch: char| ch.is_whitespace() || ch == ':' || ch == ',')
        .next()
        .unwrap_or("")
        .trim();
    if retry_after.is_empty() {
        return None;
    }
    retry_after
        .parse::<u64>()
        .ok()
        .map(|seconds| seconds.saturating_mul(1_000))
}

fn record_external_fetch_failure_backoff(
    backoff_by_target: &mut HashMap<(String, String), ExternalFetchBackoff>,
    symbol: &str,
    venue: &str,
    now_ms: i64,
    poll_ms: u64,
    max_backoff_ms: u64,
    min_backoff_ms: u64,
) -> u64 {
    if max_backoff_ms == 0 && min_backoff_ms == 0 {
        return 0;
    }
    let symbol = normalize_symbol(symbol);
    let backoff = backoff_by_target
        .entry((symbol, venue.to_string()))
        .or_default();
    backoff.failure_streak = backoff.failure_streak.saturating_add(1).min(16);
    let shift = backoff.failure_streak.saturating_sub(1).min(10);
    let factor = 1_u64.checked_shl(shift).unwrap_or(1_024);
    let backoff_ms = if max_backoff_ms == 0 {
        min_backoff_ms
    } else {
        poll_ms
            .saturating_mul(factor)
            .min(max_backoff_ms)
            .max(min_backoff_ms)
    };
    backoff.next_allowed_ms = now_ms.saturating_add(backoff_ms as i64);
    backoff_ms
}

fn external_depth_fetch_targets(
    symbols: &[String],
    timeframes: &[String],
) -> Vec<ExternalDepthFetchTarget> {
    let mut targets = Vec::new();
    let mut seen = HashSet::<(String, &'static str)>::new();
    for symbol in symbols {
        let symbol = normalize_symbol(symbol);
        if symbol.is_empty() {
            continue;
        }
        for timeframe in timeframes {
            for venue in external_depth_eval_venues(symbol.as_str(), timeframe.as_str()) {
                if seen.insert((symbol.clone(), venue)) {
                    targets.push(ExternalDepthFetchTarget {
                        venue,
                        symbol: symbol.clone(),
                    });
                }
            }
        }
    }
    targets
}

fn normalize_timeframes(timeframes: Vec<String>) -> Vec<String> {
    let mut out = timeframes
        .into_iter()
        .map(|timeframe| timeframe.trim().to_ascii_lowercase())
        .filter(|timeframe| !timeframe.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if out.is_empty() {
        out.push("5m".to_string());
    }
    out
}

fn external_depth_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_millis(2_000))
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(32)
        .tcp_keepalive(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

fn normalize_symbol(symbol: &str) -> String {
    let upper = symbol.trim().to_ascii_uppercase();
    match upper.as_str() {
        "WETH" => "ETH".to_string(),
        "XBT" => "BTC".to_string(),
        _ => upper,
    }
}

fn normalize_direction(direction: &str) -> &'static str {
    if direction.eq_ignore_ascii_case("UP") {
        "UP"
    } else {
        "DOWN"
    }
}

fn direction_label(direction: Direction) -> &'static str {
    match direction {
        Direction::Up => "UP",
        Direction::Down => "DOWN",
    }
}

fn binance_pair_for_symbol(symbol: &str) -> String {
    format!("{}USDT", normalize_symbol(symbol))
}

fn binance_depth_venue_for_symbol(symbol: &str) -> &'static str {
    if normalize_symbol(symbol) == "HYPE" {
        BINANCE_FUTURES_DEPTH_VENUE
    } else {
        BINANCE_SPOT_DEPTH_VENUE
    }
}

fn coinbase_depth_product_for_symbol(symbol: &str) -> Option<&'static str> {
    match normalize_symbol(symbol).as_str() {
        "BTC" => Some("BTC-USD"),
        "ETH" => Some("ETH-USD"),
        "SOL" => Some("SOL-USD"),
        "XRP" => Some("XRP-USD"),
        "DOGE" | "DOGECOIN" => Some("DOGE-USD"),
        "BNB" => Some("BNB-USD"),
        "HYPE" => Some("HYPE-USD"),
        _ => None,
    }
}

fn kraken_depth_pair_for_symbol(symbol: &str) -> Option<&'static str> {
    match normalize_symbol(symbol).as_str() {
        "BTC" => Some("XBTUSD"),
        "ETH" => Some("ETHUSD"),
        "SOL" => Some("SOLUSD"),
        "XRP" => Some("XRPUSD"),
        "DOGE" | "DOGECOIN" => Some("DOGEUSD"),
        "BNB" => Some("BNBUSD"),
        "HYPE" => Some("HYPEUSD"),
        _ => None,
    }
}

fn bitstamp_depth_pair_for_symbol(symbol: &str) -> Option<&'static str> {
    match normalize_symbol(symbol).as_str() {
        "BTC" => Some("btcusd"),
        "ETH" => Some("ethusd"),
        "SOL" => Some("solusd"),
        "XRP" => Some("xrpusd"),
        "DOGE" | "DOGECOIN" => Some("dogeusd"),
        "BNB" => Some("bnbusd"),
        "HYPE" => Some("hypeusd"),
        _ => None,
    }
}

fn external_depth_venue_supported_for_symbol(venue: &str, symbol: &str) -> bool {
    match venue {
        COINBASE_DEPTH_VENUE => coinbase_depth_product_for_symbol(symbol).is_some(),
        KRAKEN_DEPTH_VENUE => kraken_depth_pair_for_symbol(symbol).is_some(),
        BITSTAMP_DEPTH_VENUE => bitstamp_depth_pair_for_symbol(symbol).is_some(),
        HYPERLIQUID_DEPTH_VENUE => normalize_symbol(symbol) == "HYPE",
        BINANCE_SPOT_DEPTH_VENUE => normalize_symbol(symbol) != "HYPE",
        BINANCE_FUTURES_DEPTH_VENUE => normalize_symbol(symbol) == "HYPE",
        _ => false,
    }
}

fn external_depth_venue_static(venue: &str) -> Option<&'static str> {
    match venue {
        COINBASE_DEPTH_VENUE => Some(COINBASE_DEPTH_VENUE),
        KRAKEN_DEPTH_VENUE => Some(KRAKEN_DEPTH_VENUE),
        BITSTAMP_DEPTH_VENUE => Some(BITSTAMP_DEPTH_VENUE),
        HYPERLIQUID_DEPTH_VENUE => Some(HYPERLIQUID_DEPTH_VENUE),
        BINANCE_SPOT_DEPTH_VENUE => Some(BINANCE_SPOT_DEPTH_VENUE),
        BINANCE_FUTURES_DEPTH_VENUE => Some(BINANCE_FUTURES_DEPTH_VENUE),
        _ => None,
    }
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn env_bool_any(names: &[&str], default: bool) -> bool {
    for name in names {
        if let Ok(value) = std::env::var(name) {
            return matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            );
        }
    }
    default
}

fn env_i64(name: &str, default: i64, min: i64, max: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

fn env_u64(name: &str, default: u64, min: u64, max: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

fn env_usize(name: &str, default: usize, min: usize, max: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

fn env_f64(name: &str, default: f64, min: f64, max: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .unwrap_or(default)
        .clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(price: f64, size: f64) -> EndgameCexDepthLevel {
        EndgameCexDepthLevel { price, size }
    }

    fn snapshot(symbol: &str, venue: &'static str, updated_ms: i64) -> EndgameCexDepthSnapshot {
        let mut bids = vec![
            level(101.0, 1_000.0),
            level(100.5, 1_000.0),
            level(100.0, 1_000.0),
            level(99.5, 1_000.0),
        ];
        let mut asks = vec![
            level(102.0, 1_000.0),
            level(102.5, 1_000.0),
            level(103.0, 1_000.0),
            level(103.5, 1_000.0),
        ];
        let mut out = build_external_depth_snapshot(symbol, venue, &mut bids, &mut asks).unwrap();
        out.updated_ms = updated_ms;
        out
    }

    #[test]
    fn book99_route_groups_match_symbol_timeframe() {
        assert_eq!(
            external_depth_eval_venues("BTC", "5m"),
            vec![
                COINBASE_DEPTH_VENUE,
                KRAKEN_DEPTH_VENUE,
                BITSTAMP_DEPTH_VENUE,
                BINANCE_SPOT_DEPTH_VENUE
            ]
        );
        assert_eq!(
            external_depth_eval_venues("HYPE", "15m"),
            vec![COINBASE_DEPTH_VENUE, HYPERLIQUID_DEPTH_VENUE]
        );
        assert_eq!(
            external_depth_eval_venues("BTC", "1h"),
            vec![BINANCE_SPOT_DEPTH_VENUE]
        );
        assert_eq!(
            external_depth_eval_venues("HYPE", "1h"),
            vec![BINANCE_FUTURES_DEPTH_VENUE]
        );
    }

    #[test]
    fn cost_helpers_use_flip_side_to_boundary() {
        let bids = vec![level(100.0, 1.0), level(99.0, 2.0), level(98.0, 3.0)];
        let (cost, reached) = cost_to_sell_down_to(&bids, 99.0);
        assert!(reached);
        assert!((cost - 298.0).abs() < 1e-9);
        let asks = vec![level(101.0, 1.0), level(102.0, 2.0), level(103.0, 3.0)];
        let (cost, reached) = cost_to_buy_up_to(&asks, 102.0);
        assert!(reached);
        assert!((cost - 305.0).abs() < 1e-9);
    }

    #[test]
    fn evaluator_fails_open_when_all_requested_venues_are_missing() {
        let cfg = EndgameCexDepthConfig::from_env();
        let cache = EndgameCexDepthCache::new();
        let decision = evaluate_cex_depth(
            &cfg,
            &cache,
            "BTC",
            "5m",
            10,
            Direction::Up,
            100.0,
            100,
            10_000,
        );
        assert!(decision.fail_open);
        assert_eq!(decision.reason, "missing_snapshot");
        assert_eq!(decision.multiplier, 1.0);
    }

    #[test]
    fn evaluator_does_not_trigger_before_rolling_threshold_is_ready() {
        let mut cfg = EndgameCexDepthConfig::from_env();
        cfg.min_samples = 300;
        let cache = EndgameCexDepthCache::new();
        cache.upsert(snapshot("BTC", COINBASE_DEPTH_VENUE, 10_000), &cfg);
        let decision = evaluate_cex_depth(
            &cfg,
            &cache,
            "BTC",
            "5m",
            10,
            Direction::Up,
            100.0,
            100,
            10_050,
        );
        assert!(!decision.fail_open);
        assert_eq!(decision.multiplier, 1.0);
        assert_eq!(decision.trigger_count, 0);
    }

    #[test]
    fn evaluator_uses_endgame_anchor_when_venue_open_sample_is_missing() {
        let mut cfg = EndgameCexDepthConfig::from_env();
        cfg.base_max_age_ms = 500;
        cfg.health_snapshot_detail_enabled = true;
        cfg.min_samples = 300;
        let cache = EndgameCexDepthCache::new();
        cache.upsert(snapshot("BTC", COINBASE_DEPTH_VENUE, 20_000), &cfg);
        let decision = evaluate_cex_depth(
            &cfg,
            &cache,
            "BTC",
            "5m",
            10,
            Direction::Up,
            100.0,
            100,
            20_050,
        );
        assert!(!decision.fail_open, "payload={}", decision.payload);
        assert_eq!(decision.reason, "pass");
        assert_eq!(decision.boundary_price, Some(100.0));
        assert_eq!(
            decision
                .payload
                .get("venues")
                .and_then(|venues| venues.get(0))
                .and_then(|venue| venue.get("base_price_source"))
                .and_then(|source| source.as_str()),
            Some("endgame_base_anchor_fallback")
        );
    }

    #[test]
    fn evaluator_reduces_when_rolling_quantile_depth_collapses() {
        let mut cfg = EndgameCexDepthConfig::from_env();
        cfg.min_samples = 3;
        cfg.history_sample_ms = 1;
        cfg.threshold_recompute_ms = 1;
        cfg.close_adjustment_per_trigger = 0.12;
        cfg.floor_btc_usd = 0.0;
        let cache = EndgameCexDepthCache::new();
        for idx in 0..4 {
            let mut snap = snapshot("BTC", COINBASE_DEPTH_VENUE, 1_000 + idx * 10);
            for cost in snap.cost_to_sell_down_bps_usd.iter_mut().flatten() {
                *cost = 500_000.0;
            }
            cache.upsert(snap, &cfg);
        }
        let mut weak = snapshot("BTC", COINBASE_DEPTH_VENUE, 2_000);
        weak.bids = vec![level(101.0, 1.0), level(100.0, 1.0), level(99.0, 1.0)];
        weak.cost_to_sell_down_bps_usd =
            cost_to_sell_down_bps_buckets(weak.bids.as_slice(), weak.mid());
        cache.upsert(weak, &cfg);
        let decision = evaluate_cex_depth(
            &cfg,
            &cache,
            "BTC",
            "5m",
            1,
            Direction::Up,
            100.0,
            100,
            2_010,
        );
        assert!(!decision.fail_open);
        assert!(decision.trigger_count > 0, "payload={}", decision.payload);
        assert!(decision.multiplier < 1.0);
    }
}
