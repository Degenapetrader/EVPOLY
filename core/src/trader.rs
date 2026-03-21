use crate::api::{
    BatchPlaceOrderResult, MarketConstraintSnapshot, MergeConditionRequest, PlaceOrderTiming,
    PolymarketApi, RedeemConditionRequest,
};
use crate::config::TradingConfig;
use crate::detector::{BuyOpportunity, PriceDetector, TokenType};
use crate::entry_idempotency::{classify_entry_error, ExecutionErrorClass};
use crate::event_log::log_event;
use crate::models::*;
use crate::signal_state::SharedSignalState;
use crate::simulation::SimulationTracker;
use crate::strategy::STRATEGY_ID_EVCURVE_V1;
use crate::tracking_db::{
    MmWalletInventoryRow, PendingOrderRecord, TrackingDb, TradeEventRecord,
    WalletCashflowEventRecord,
};
use anyhow::{anyhow, Result};
use log::{debug, warn};
use polymarket_client_sdk::clob::types::response::OpenOrderResponse;
use polymarket_client_sdk::clob::types::OrderStatusType;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use tokio::sync::Mutex;

struct BatchPlaceRequest {
    order: OrderRequest,
    response_tx: tokio::sync::oneshot::Sender<Result<(OrderResponse, PlaceOrderTiming)>>,
}

#[derive(Debug, Clone)]
struct PremarketTpJob {
    trade_key: String,
}

#[derive(Debug, Clone, Copy)]
struct PremarketTpBasis {
    avg_entry_price: f64,
    position_size: f64,
}

pub const STRATEGY_ID_MANUAL_PREMARKET_V1: &str = "manual_premarket_v1";
pub const STRATEGY_ID_MANUAL_PREMARKET_TAKER_V1: &str = "manual_premarket_taker_v1";
pub const STRATEGY_ID_MANUAL_CHASE_LIMIT_V1: &str = "manual_chase_limit_v1";
pub const STRATEGY_ID_MANUAL_CHASE_LIMIT_TAKER_V1: &str = "manual_chase_limit_taker_v1";

#[derive(Debug, Clone, Default)]
struct EntryAckEmptyOrderIdLogWindow {
    window_start_ms: i64,
    window_count: u64,
}

fn entry_ack_empty_order_id_windows(
) -> &'static StdMutex<HashMap<String, EntryAckEmptyOrderIdLogWindow>> {
    static WINDOWS: OnceLock<StdMutex<HashMap<String, EntryAckEmptyOrderIdLogWindow>>> =
        OnceLock::new();
    WINDOWS.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[derive(Debug, Clone, Copy, Default)]
struct MmWeakExitEventCounts {
    probe_24h: u64,
    stale_skip_24h: u64,
    probe_3h: u64,
    stale_skip_3h: u64,
}

#[derive(Debug, Clone, Default)]
struct MmGlobalStatusAccumulator {
    market_slug: Option<String>,
    timeframe: Option<String>,
    symbol: Option<String>,
    mode: Option<String>,
    up_token_id: Option<String>,
    down_token_id: Option<String>,
    open_buy_orders: u64,
    open_sell_orders: u64,
    open_token_ids: HashSet<String>,
    inventory_shares: f64,
    inventory_value_usd: f64,
}

fn overwrite_if_missing(slot: &mut Option<String>, candidate: Option<String>) {
    let has_value = slot
        .as_deref()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    if has_value {
        return;
    }
    if let Some(value) = candidate {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            *slot = Some(trimmed.to_string());
        }
    }
}

fn mm_event_log_paths(base_path: &Path) -> Vec<PathBuf> {
    let parent = base_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base_name = base_path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("events.jsonl")
        .to_string();
    let rotated_prefix = format!("{base_name}.");
    let mut paths = Vec::new();
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            let name_matches = entry
                .file_name()
                .to_str()
                .map(|name| name == base_name || name.starts_with(rotated_prefix.as_str()))
                .unwrap_or(false);
            if name_matches {
                paths.push(entry.path());
            }
        }
    }
    if paths.is_empty() {
        paths.push(base_path.to_path_buf());
    }
    paths.sort();
    paths
}

fn summarize_mm_weak_exit_event_counts(now_ms: i64) -> MmWeakExitEventCounts {
    let from_24h = now_ms.saturating_sub(24 * 3_600_000);
    let from_3h = now_ms.saturating_sub(3 * 3_600_000);
    let base_path = std::env::var("EVPOLY_EVENT_LOG_PATH")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("events.jsonl"));
    let mut out = MmWeakExitEventCounts::default();
    for path in mm_event_log_paths(base_path.as_path()) {
        let Ok(file) = File::open(path) else {
            continue;
        };
        for line in BufReader::new(file)
            .lines()
            .map_while(std::result::Result::ok)
        {
            let Ok(value) = serde_json::from_str::<Value>(line.as_str()) else {
                continue;
            };
            let Some(ts_ms) = value.get("ts_ms").and_then(|v| v.as_i64()) else {
                continue;
            };
            if ts_ms < from_24h {
                continue;
            }
            let Some(kind) = value.get("kind").and_then(|v| v.as_str()) else {
                continue;
            };
            match kind {
                "mm_rewards_weak_exit_probe" => {
                    out.probe_24h = out.probe_24h.saturating_add(1);
                    if ts_ms >= from_3h {
                        out.probe_3h = out.probe_3h.saturating_add(1);
                    }
                }
                "mm_rewards_weak_exit_skip_stale_live_balance" => {
                    out.stale_skip_24h = out.stale_skip_24h.saturating_add(1);
                    if ts_ms >= from_3h {
                        out.stale_skip_3h = out.stale_skip_3h.saturating_add(1);
                    }
                }
                _ => {}
            }
        }
    }
    out
}

pub struct Trader {
    api: Arc<PolymarketApi>,
    config: TradingConfig,
    simulation_mode: bool,
    total_profit: Arc<Mutex<f64>>,
    trades_executed: Arc<Mutex<u64>>,
    pending_trades: Arc<Mutex<HashMap<String, PendingTrade>>>, // Key: period_timestamp
    detector: Option<Arc<PriceDetector>>, // Optional detector reference for cycle tracking
    signal_state: Option<SharedSignalState>, // Optional real-time signal state for fast exit overrides
    tracking_db: Option<Arc<TrackingDb>>,    // Optional SQLite tracker
    simulation_tracker: Option<Arc<SimulationTracker>>, // Simulation tracker for PnL and position tracking
    market_constraints_cache: Arc<Mutex<HashMap<String, MarketConstraintCacheEntry>>>,
    batch_place_tx: Option<tokio::sync::mpsc::Sender<BatchPlaceRequest>>,
    premarket_tp_tx: Option<tokio::sync::mpsc::Sender<PremarketTpJob>>,
    premarket_tp_retry_after_ms: Arc<Mutex<HashMap<String, i64>>>,
    premarket_tp_inflight: Arc<Mutex<HashSet<String>>>,
}

struct EntrySlotGuard {
    slot: String,
}

impl EntrySlotGuard {
    fn acquire(slot: String) -> Option<Self> {
        if Trader::try_reserve_entry_slot(&slot) {
            Some(Self { slot })
        } else {
            None
        }
    }
}

impl Drop for EntrySlotGuard {
    fn drop(&mut self) {
        Trader::release_entry_slot(&self.slot);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryExecutionMode {
    Ladder,
    Endgame,
    Evcurve,
    SessionBand,
    Evsnipe,
    MmRewards,
    Restored,
    Legacy,
}

impl EntryExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            EntryExecutionMode::Ladder => "ladder",
            EntryExecutionMode::Endgame => "endgame",
            EntryExecutionMode::Evcurve => "evcurve",
            EntryExecutionMode::SessionBand => "sessionband",
            EntryExecutionMode::Evsnipe => "evsnipe",
            EntryExecutionMode::MmRewards => "mm_rewards",
            EntryExecutionMode::Restored => "restored",
            EntryExecutionMode::Legacy => "legacy",
        }
    }

    fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "ladder" => EntryExecutionMode::Ladder,
            "endgame" => EntryExecutionMode::Endgame,
            "evcurve" => EntryExecutionMode::Evcurve,
            "sessionband" => EntryExecutionMode::SessionBand,
            "evsnipe" => EntryExecutionMode::Evsnipe,
            "mm_rewards" => EntryExecutionMode::MmRewards,
            "restored" => EntryExecutionMode::Restored,
            _ => EntryExecutionMode::Legacy,
        }
    }

    fn is_ladder(self) -> bool {
        matches!(self, EntryExecutionMode::Ladder)
    }

    fn is_endgame(self) -> bool {
        matches!(self, EntryExecutionMode::Endgame)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedemptionSweepTrigger {
    Scheduler,
    StartupCatchup,
    ManualApi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeSweepTrigger {
    Scheduler,
    ManualApi,
}

#[derive(Debug, Clone, Default)]
struct RedemptionSweepStatus {
    last_started_ts: Option<u64>,
    last_finished_ts: Option<u64>,
    last_trigger: Option<String>,
    last_reason: Option<String>,
    last_processed_conditions: usize,
    last_submitted_conditions: usize,
    last_successful_conditions: usize,
    last_skipped_duplicates: usize,
    last_pause_remaining_sec: Option<u64>,
}

#[derive(Debug, Clone, Default)]
struct MergeSweepStatus {
    last_started_ts: Option<u64>,
    last_finished_ts: Option<u64>,
    last_trigger: Option<String>,
    last_reason: Option<String>,
    last_scanned_conditions: usize,
    last_merge_candidates: usize,
    last_submitted_merges: usize,
    last_successful_merges: usize,
}

#[derive(Debug, Clone, Copy)]
struct MarketOrderConstraints {
    min_notional_usd: f64,
    min_size_shares: f64,
    tick_size: f64,
}

#[derive(Debug, Clone, Copy)]
struct MarketConstraintCacheEntry {
    constraints: MarketOrderConstraints,
    fetched_at_ms: i64,
}

#[derive(Debug, Clone)]
struct FillTransition {
    order_id: Option<String>,
    trade_key: Option<String>,
    strategy_id: Option<String>,
    period_timestamp: u64,
    timeframe: Option<String>,
    condition_id: Option<String>,
    token_id: String,
    token_type: Option<String>,
    side: Option<String>,
    price: Option<f64>,
    units: Option<f64>,
    notional_usd: Option<f64>,
    reason: String,
    reconciled: bool,
}

impl Trader {
    pub fn get_simulation_tracker(&self) -> Option<Arc<SimulationTracker>> {
        self.simulation_tracker.clone()
    }

    pub fn tracking_db(&self) -> Option<Arc<TrackingDb>> {
        self.tracking_db.clone()
    }

    pub fn is_simulation_mode(&self) -> bool {
        self.simulation_mode
    }

    pub fn new(
        api: Arc<PolymarketApi>,
        config: TradingConfig,
        simulation_mode: bool,
        detector: Option<Arc<PriceDetector>>,
        signal_state: Option<SharedSignalState>,
        tracking_db: Option<Arc<TrackingDb>>,
    ) -> Result<Self> {
        let pending_trades = Arc::new(Mutex::new(HashMap::new()));
        let market_constraints_cache = Arc::new(Mutex::new(HashMap::new()));
        let premarket_tp_retry_after_ms = Arc::new(Mutex::new(HashMap::new()));
        let premarket_tp_inflight = Arc::new(Mutex::new(HashSet::new()));
        let simulation_tracker = if simulation_mode {
            Some(Arc::new(SimulationTracker::new("simulation.toml")?))
        } else {
            None
        };
        let batch_place_tx = if Self::batch_place_enable() && !simulation_mode {
            let queue_cap = Self::batch_place_queue_cap();
            let flush_ms = Self::batch_place_flush_ms();
            let max_orders = Self::batch_place_max_orders();
            let (tx, mut rx) = tokio::sync::mpsc::channel::<BatchPlaceRequest>(queue_cap);
            let api_for_batch = api.clone();
            tokio::spawn(async move {
                let mut summary_window_start_ms = chrono::Utc::now().timestamp_millis();
                let mut summary_request_count = 0_u64;
                let mut summary_success_count = 0_u64;
                let mut summary_failed_count = 0_u64;
                let mut summary_fallback_count = 0_u64;
                while let Some(first) = rx.recv().await {
                    let mut jobs = vec![first];
                    let deadline =
                        tokio::time::Instant::now() + tokio::time::Duration::from_millis(flush_ms);
                    while jobs.len() < max_orders {
                        let now = tokio::time::Instant::now();
                        if now >= deadline {
                            break;
                        }
                        let wait_for = deadline.saturating_duration_since(now);
                        match tokio::time::timeout(wait_for, rx.recv()).await {
                            Ok(Some(job)) => jobs.push(job),
                            _ => break,
                        }
                    }

                    let orders: Vec<OrderRequest> =
                        jobs.iter().map(|job| job.order.clone()).collect();
                    match api_for_batch.place_orders_with_timing_batch(&orders).await {
                        Ok(results) if results.len() == jobs.len() => {
                            let success_count = results
                                .iter()
                                .filter(|result| {
                                    result.response.is_some() && result.error.is_none()
                                })
                                .count();
                            let failed_count = results.len().saturating_sub(success_count);
                            summary_request_count = summary_request_count
                                .saturating_add(u64::try_from(results.len()).ok().unwrap_or(0));
                            summary_success_count = summary_success_count
                                .saturating_add(u64::try_from(success_count).ok().unwrap_or(0));
                            summary_failed_count = summary_failed_count
                                .saturating_add(u64::try_from(failed_count).ok().unwrap_or(0));
                            for (job, result) in jobs.into_iter().zip(results.into_iter()) {
                                let send_result = match result {
                                    BatchPlaceOrderResult {
                                        response: Some(response),
                                        timing,
                                        ..
                                    } => Ok((response, timing)),
                                    BatchPlaceOrderResult { error, timing, .. } => {
                                        let err_msg = error.unwrap_or_else(|| {
                                            "batch place returned empty response".to_string()
                                        });
                                        Err(anyhow::anyhow!(
                                            "batch place failed: {} (batch_ms={})",
                                            err_msg,
                                            timing.total_api_ms
                                        ))
                                    }
                                };
                                let _ = job.response_tx.send(send_result);
                            }
                        }
                        Ok(results) => {
                            let err = anyhow::anyhow!(
                                "batch place response mismatch: requests={} results={}",
                                jobs.len(),
                                results.len()
                            );
                            summary_request_count = summary_request_count
                                .saturating_add(u64::try_from(jobs.len()).ok().unwrap_or(0));
                            summary_failed_count = summary_failed_count
                                .saturating_add(u64::try_from(jobs.len()).ok().unwrap_or(0));
                            log_event(
                                "batch_place_submit",
                                json!({
                                    "request_count": jobs.len(),
                                    "success_count": 0,
                                    "failed_count": jobs.len(),
                                    "error": err.to_string()
                                }),
                            );
                            for job in jobs {
                                let _ = job.response_tx.send(Err(anyhow::anyhow!(err.to_string())));
                            }
                        }
                        Err(batch_err) => {
                            summary_request_count = summary_request_count
                                .saturating_add(u64::try_from(jobs.len()).ok().unwrap_or(0));
                            summary_failed_count = summary_failed_count
                                .saturating_add(u64::try_from(jobs.len()).ok().unwrap_or(0));
                            summary_fallback_count = summary_fallback_count
                                .saturating_add(u64::try_from(jobs.len()).ok().unwrap_or(0));
                            log_event(
                                "batch_place_submit",
                                json!({
                                    "request_count": jobs.len(),
                                    "success_count": 0,
                                    "failed_count": jobs.len(),
                                    "error": batch_err.to_string(),
                                    "fallback": "single_order"
                                }),
                            );
                            for job in jobs {
                                let fallback =
                                    api_for_batch.place_order_with_timing(&job.order).await;
                                let _ = match fallback {
                                    Ok(ok) => job.response_tx.send(Ok(ok)),
                                    Err(single_err) => job.response_tx.send(Err(anyhow::anyhow!(
                                        "batch place error: {}; single fallback error: {}",
                                        batch_err,
                                        single_err
                                    ))),
                                };
                            }
                        }
                    }
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    if now_ms.saturating_sub(summary_window_start_ms) >= 30_000
                        && summary_request_count > 0
                    {
                        log_event(
                            "batch_place_submit_summary",
                            json!({
                                "window_ms": now_ms.saturating_sub(summary_window_start_ms),
                                "request_count": summary_request_count,
                                "success_count": summary_success_count,
                                "failed_count": summary_failed_count,
                                "fallback_count": summary_fallback_count
                            }),
                        );
                        summary_window_start_ms = now_ms;
                        summary_request_count = 0;
                        summary_success_count = 0;
                        summary_failed_count = 0;
                        summary_fallback_count = 0;
                    }
                }
            });
            Some(tx)
        } else {
            None
        };

        let premarket_tp_tx = if !simulation_mode {
            let (tx, mut rx) =
                tokio::sync::mpsc::channel::<PremarketTpJob>(Self::premarket_tp_queue_cap());
            let api_for_tp = api.clone();
            let pending_for_tp = pending_trades.clone();
            let retry_for_tp = premarket_tp_retry_after_ms.clone();
            let inflight_for_tp = premarket_tp_inflight.clone();
            let tracking_db_for_tp = tracking_db.clone();
            tokio::spawn(async move {
                while let Some(job) = rx.recv().await {
                    Trader::run_premarket_tp_job(
                        api_for_tp.clone(),
                        pending_for_tp.clone(),
                        tracking_db_for_tp.clone(),
                        retry_for_tp.clone(),
                        inflight_for_tp.clone(),
                        job,
                    )
                    .await;
                }
            });
            Some(tx)
        } else {
            None
        };

        Ok(Self {
            api,
            config,
            simulation_mode,
            total_profit: Arc::new(Mutex::new(0.0)),
            trades_executed: Arc::new(Mutex::new(0)),
            pending_trades,
            detector,
            signal_state,
            tracking_db,
            simulation_tracker,
            market_constraints_cache,
            batch_place_tx,
            premarket_tp_tx,
            premarket_tp_retry_after_ms,
            premarket_tp_inflight,
        })
    }

    fn normalize_timeframe_label(timeframe: Option<&str>) -> String {
        match timeframe
            .map(|tf| tf.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("5m") => "5m".to_string(),
            Some("15m") => "15m".to_string(),
            Some("1h") => "1h".to_string(),
            Some("4h") => "4h".to_string(),
            Some("1d") | Some("d1") | Some("24h") | Some("daily") => "1d".to_string(),
            _ => "15m".to_string(),
        }
    }

    fn normalize_strategy_id(strategy_id: Option<&str>) -> String {
        strategy_id
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_ascii_lowercase())
            .unwrap_or_else(crate::strategy::default_strategy_id)
    }

    fn batch_place_enable() -> bool {
        std::env::var("EVPOLY_BATCH_PLACE_ENABLE")
            .ok()
            .map(|v| {
                let normalized = v.trim().to_ascii_lowercase();
                normalized == "1" || normalized == "true" || normalized == "yes"
            })
            .unwrap_or(true)
    }

    fn batch_place_max_orders() -> usize {
        std::env::var("EVPOLY_BATCH_PLACE_MAX_ORDERS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(10)
            .clamp(2, 15)
    }

    fn batch_place_flush_ms() -> u64 {
        std::env::var("EVPOLY_BATCH_PLACE_FLUSH_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(8)
            .clamp(1, 250)
    }

    fn batch_place_queue_cap() -> usize {
        std::env::var("EVPOLY_BATCH_PLACE_QUEUE_CAP")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(2048)
            .max(64)
    }

    fn should_route_via_batch_place(strategy_id: &str, entry_mode: EntryExecutionMode) -> bool {
        (strategy_id == crate::strategy::STRATEGY_ID_PREMARKET_V1
            && matches!(entry_mode, EntryExecutionMode::Ladder))
            || (strategy_id == crate::strategy::STRATEGY_ID_MM_REWARDS_V1
                && matches!(
                    entry_mode,
                    EntryExecutionMode::MmRewards | EntryExecutionMode::Ladder
                ))
    }

    async fn submit_order_with_optional_batch(
        &self,
        order: &OrderRequest,
        strategy_id: &str,
        entry_mode: EntryExecutionMode,
    ) -> Result<(OrderResponse, PlaceOrderTiming)> {
        if let Some(batch_tx) = self.batch_place_tx.as_ref() {
            if Self::should_route_via_batch_place(strategy_id, entry_mode) {
                let (response_tx, response_rx) = tokio::sync::oneshot::channel();
                let request = BatchPlaceRequest {
                    order: order.clone(),
                    response_tx,
                };
                if batch_tx.send(request).await.is_ok() {
                    return response_rx
                        .await
                        .unwrap_or_else(|e| Err(anyhow!("batch place worker dropped: {}", e)));
                }
                log_event(
                    "batch_place_queue_send_failed",
                    json!({
                        "strategy_id": strategy_id,
                        "entry_mode": entry_mode.as_str(),
                        "token_id": order.token_id,
                        "side": order.side,
                        "fallback": "single_order"
                    }),
                );
            }
        }
        self.api.place_order_with_timing(order).await
    }

    async fn reporting_totals(&self) -> (u64, f64) {
        if !self.simulation_mode {
            if let Some(db) = &self.tracking_db {
                if let Ok(rows) = db.summarize_timeframe_performance() {
                    let total_trades = rows.iter().map(|row| row.trades).sum::<u64>();
                    let total_profit = rows.iter().map(|row| row.net_pnl_usd).sum::<f64>();
                    return (total_trades, total_profit);
                }
            }
        }

        let trades = *self.trades_executed.lock().await;
        let profit = *self.total_profit.lock().await;
        (trades, profit)
    }

    fn stop_loss_price_clamped(&self) -> Option<f64> {
        self.config
            .stop_loss_price
            .map(|price| price.clamp(0.01, 0.99))
    }

    fn hold_to_resolution_for_mode(&self, mode: EntryExecutionMode) -> bool {
        match mode {
            EntryExecutionMode::Ladder => self
                .config
                .hold_to_resolution_ladder
                .unwrap_or(self.config.hold_to_resolution),
            EntryExecutionMode::Endgame => self
                .config
                .hold_to_resolution_ladder
                .unwrap_or(self.config.hold_to_resolution),
            EntryExecutionMode::Evcurve => self
                .config
                .hold_to_resolution_ladder
                .unwrap_or(self.config.hold_to_resolution),
            EntryExecutionMode::SessionBand => self
                .config
                .hold_to_resolution_ladder
                .unwrap_or(self.config.hold_to_resolution),
            EntryExecutionMode::Evsnipe => self
                .config
                .hold_to_resolution_ladder
                .unwrap_or(self.config.hold_to_resolution),
            EntryExecutionMode::MmRewards => self
                .config
                .hold_to_resolution_ladder
                .unwrap_or(self.config.hold_to_resolution),
            EntryExecutionMode::Restored | EntryExecutionMode::Legacy => {
                self.config.hold_to_resolution
            }
        }
    }

    fn mode_from_trade(&self, trade: &PendingTrade) -> EntryExecutionMode {
        EntryExecutionMode::parse(trade.entry_mode.as_str())
    }

    fn should_hold_trade_to_resolution(&self, trade: &PendingTrade) -> bool {
        self.hold_to_resolution_for_mode(self.mode_from_trade(trade))
            || trade.no_sell
            || trade.claim_on_closure
    }

    fn should_place_resting_tp_after_fill(&self, trade: &PendingTrade) -> bool {
        let _ = trade;
        true
    }

    fn premarket_tp_enabled() -> bool {
        Self::env_bool_named("EVPOLY_PREMARKET_TP_ENABLE", true)
    }

    fn premarket_tp_queue_cap() -> usize {
        256
    }

    fn premarket_tp_retry_interval_ms() -> i64 {
        30_000
    }

    fn premarket_tp_start_delay_ms() -> i64 {
        5 * 60 * 1000
    }

    fn premarket_tp_api_timeout_ms() -> u64 {
        1_000
    }

    fn premarket_tp_is_timeframe_eligible(timeframe: &str) -> bool {
        matches!(
            Self::normalize_timeframe_label(Some(timeframe)).as_str(),
            "15m" | "1h" | "4h"
        )
    }

    fn premarket_tp_launch_ms(trade: &PendingTrade) -> i64 {
        i64::try_from(trade.market_timestamp)
            .unwrap_or(i64::MAX / 1_000)
            .saturating_mul(1_000)
    }

    fn premarket_tp_due_ms(trade: &PendingTrade) -> i64 {
        Self::premarket_tp_launch_ms(trade).saturating_add(Self::premarket_tp_start_delay_ms())
    }

    fn premarket_tp_price_target(avg_entry_price: f64, top_ask: Option<f64>) -> f64 {
        let top_ask = top_ask.filter(|v| v.is_finite() && *v > 0.0).unwrap_or(0.0);
        (avg_entry_price * 2.0).max(0.60).max(top_ask)
    }

    fn premarket_tp_is_trade_eligible(trade: &PendingTrade, now_ms: i64) -> bool {
        if Self::normalize_strategy_id(Some(trade.strategy_id.as_str()))
            != crate::strategy::STRATEGY_ID_PREMARKET_V1
        {
            return false;
        }
        if !Self::premarket_tp_is_timeframe_eligible(trade.source_timeframe.as_str()) {
            return false;
        }
        if !trade.buy_order_confirmed
            || trade.sold
            || trade.limit_sell_orders_placed
            || trade.no_sell
            || trade.claim_on_closure
        {
            return false;
        }
        now_ms >= Self::premarket_tp_due_ms(trade)
    }

    fn should_defer_to_premarket_tp_worker(trade: &PendingTrade) -> bool {
        Self::premarket_tp_enabled()
            && Self::normalize_strategy_id(Some(trade.strategy_id.as_str()))
                == crate::strategy::STRATEGY_ID_PREMARKET_V1
            && Self::premarket_tp_is_timeframe_eligible(trade.source_timeframe.as_str())
    }

    fn premarket_tp_value_string(row: &Value, keys: &[&str]) -> Option<String> {
        keys.iter()
            .find_map(|key| row.get(*key).and_then(Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }

    fn premarket_tp_value_f64(row: &Value, keys: &[&str]) -> Option<f64> {
        keys.iter().find_map(|key| {
            let value = row.get(*key)?;
            if let Some(v) = value.as_f64() {
                return v.is_finite().then_some(v);
            }
            if let Some(v) = value.as_i64() {
                return Some(v as f64);
            }
            if let Some(v) = value.as_u64() {
                return Some(v as f64);
            }
            value
                .as_str()
                .and_then(|raw| raw.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite())
        })
    }

    fn extract_premarket_tp_basis_from_positions(
        rows: &[Value],
        condition_id: &str,
        token_id: &str,
    ) -> Option<PremarketTpBasis> {
        let want_token = token_id.trim().to_ascii_lowercase();
        let want_condition = condition_id.trim().to_ascii_lowercase();
        rows.iter().find_map(|row| {
            let row_token = Self::premarket_tp_value_string(row, &["asset", "token_id", "tokenId"])
                .unwrap_or_default()
                .to_ascii_lowercase();
            if row_token.is_empty() || row_token != want_token {
                return None;
            }

            let row_condition =
                Self::premarket_tp_value_string(row, &["conditionId", "condition_id"])
                    .unwrap_or_default()
                    .to_ascii_lowercase();
            if !want_condition.is_empty()
                && !row_condition.is_empty()
                && row_condition != want_condition
            {
                return None;
            }

            let position_size = Self::premarket_tp_value_f64(
                row,
                &[
                    "size",
                    "position_size",
                    "positionSize",
                    "shares",
                    "quantity",
                ],
            )?;
            if !position_size.is_finite() || position_size <= 0.0 {
                return None;
            }

            let avg_entry_direct = Self::premarket_tp_value_f64(
                row,
                &[
                    "avg_price",
                    "avgPrice",
                    "average_price",
                    "averagePrice",
                    "entry_price",
                    "entryPrice",
                ],
            );
            let avg_entry_from_cost = Self::premarket_tp_value_f64(
                row,
                &[
                    "cost_basis",
                    "costBasis",
                    "initial_value",
                    "initialValue",
                    "amount",
                    "value",
                ],
            )
            .and_then(|cost| {
                if cost.is_finite() && cost > 0.0 {
                    Some(cost / position_size)
                } else {
                    None
                }
            });

            let avg_entry_price = avg_entry_direct
                .or(avg_entry_from_cost)
                .filter(|v| v.is_finite() && *v > 0.0 && *v < 1.0)?;

            Some(PremarketTpBasis {
                avg_entry_price,
                position_size,
            })
        })
    }

    async fn set_premarket_tp_retry_after(
        retry_after_ms: Arc<Mutex<HashMap<String, i64>>>,
        trade_key: &str,
        retry_after: Option<i64>,
    ) {
        let mut retry = retry_after_ms.lock().await;
        if let Some(ts) = retry_after {
            retry.insert(trade_key.to_string(), ts);
        } else {
            retry.remove(trade_key);
        }
    }

    async fn release_premarket_tp_inflight(inflight: Arc<Mutex<HashSet<String>>>, trade_key: &str) {
        let mut guard = inflight.lock().await;
        guard.remove(trade_key);
    }

    async fn run_premarket_tp_job(
        api: Arc<PolymarketApi>,
        pending_trades: Arc<Mutex<HashMap<String, PendingTrade>>>,
        tracking_db: Option<Arc<TrackingDb>>,
        retry_after_ms: Arc<Mutex<HashMap<String, i64>>>,
        inflight: Arc<Mutex<HashSet<String>>>,
        job: PremarketTpJob,
    ) {
        let trade_key = job.trade_key.clone();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let retry_after_default =
            Some(now_ms.saturating_add(Self::premarket_tp_retry_interval_ms()));

        let trade = {
            let pending = pending_trades.lock().await;
            pending.get(trade_key.as_str()).cloned()
        };

        let Some(trade) = trade else {
            Self::set_premarket_tp_retry_after(retry_after_ms.clone(), trade_key.as_str(), None)
                .await;
            Self::release_premarket_tp_inflight(inflight, trade_key.as_str()).await;
            return;
        };

        if !Self::premarket_tp_enabled() || !Self::premarket_tp_is_trade_eligible(&trade, now_ms) {
            let retry_after = if !Self::premarket_tp_enabled() {
                None
            } else {
                let due_ms = Self::premarket_tp_due_ms(&trade);
                if now_ms < due_ms {
                    Some(due_ms)
                } else {
                    None
                }
            };
            Self::set_premarket_tp_retry_after(
                retry_after_ms.clone(),
                trade_key.as_str(),
                retry_after,
            )
            .await;
            Self::release_premarket_tp_inflight(inflight, trade_key.as_str()).await;
            return;
        }

        let positions = match tokio::time::timeout(
            tokio::time::Duration::from_millis(Self::premarket_tp_api_timeout_ms()),
            api.get_wallet_positions_live(None),
        )
        .await
        {
            Ok(Ok(rows)) => rows,
            Ok(Err(e)) => {
                log_event(
                    "premarket_tp_retry",
                    json!({
                        "trade_key": trade_key.as_str(),
                        "reason": format!("positions_fetch_failed: {}", e),
                        "retry_in_ms": Self::premarket_tp_retry_interval_ms()
                    }),
                );
                Self::set_premarket_tp_retry_after(
                    retry_after_ms.clone(),
                    trade_key.as_str(),
                    retry_after_default,
                )
                .await;
                Self::release_premarket_tp_inflight(inflight, trade_key.as_str()).await;
                return;
            }
            Err(_) => {
                log_event(
                    "premarket_tp_retry",
                    json!({
                        "trade_key": trade_key.as_str(),
                        "reason": "positions_fetch_timeout",
                        "retry_in_ms": Self::premarket_tp_retry_interval_ms()
                    }),
                );
                Self::set_premarket_tp_retry_after(
                    retry_after_ms.clone(),
                    trade_key.as_str(),
                    retry_after_default,
                )
                .await;
                Self::release_premarket_tp_inflight(inflight, trade_key.as_str()).await;
                return;
            }
        };

        let Some(basis) = Self::extract_premarket_tp_basis_from_positions(
            positions.as_slice(),
            trade.condition_id.as_str(),
            trade.token_id.as_str(),
        ) else {
            log_event(
                "premarket_tp_retry",
                json!({
                    "trade_key": trade_key.as_str(),
                    "reason": "positions_missing_entry_or_size",
                    "retry_in_ms": Self::premarket_tp_retry_interval_ms()
                }),
            );
            Self::set_premarket_tp_retry_after(
                retry_after_ms.clone(),
                trade_key.as_str(),
                retry_after_default,
            )
            .await;
            Self::release_premarket_tp_inflight(inflight, trade_key.as_str()).await;
            return;
        };

        let top_ask = match tokio::time::timeout(
            tokio::time::Duration::from_millis(Self::premarket_tp_api_timeout_ms()),
            api.get_best_price(trade.token_id.as_str()),
        )
        .await
        {
            Ok(Ok(Some(best))) => best.ask.and_then(|v| f64::try_from(v).ok()),
            Ok(Ok(None)) => None,
            Ok(Err(e)) => {
                log_event(
                    "premarket_tp_retry",
                    json!({
                        "trade_key": trade_key.as_str(),
                        "reason": format!("top_ask_fetch_failed: {}", e),
                        "retry_in_ms": Self::premarket_tp_retry_interval_ms()
                    }),
                );
                Self::set_premarket_tp_retry_after(
                    retry_after_ms.clone(),
                    trade_key.as_str(),
                    retry_after_default,
                )
                .await;
                Self::release_premarket_tp_inflight(inflight, trade_key.as_str()).await;
                return;
            }
            Err(_) => {
                log_event(
                    "premarket_tp_retry",
                    json!({
                        "trade_key": trade_key.as_str(),
                        "reason": "top_ask_fetch_timeout",
                        "retry_in_ms": Self::premarket_tp_retry_interval_ms()
                    }),
                );
                Self::set_premarket_tp_retry_after(
                    retry_after_ms.clone(),
                    trade_key.as_str(),
                    retry_after_default,
                )
                .await;
                Self::release_premarket_tp_inflight(inflight, trade_key.as_str()).await;
                return;
            }
        };

        let tick_size = match tokio::time::timeout(
            tokio::time::Duration::from_millis(Self::premarket_tp_api_timeout_ms()),
            api.get_market_constraints(trade.condition_id.as_str()),
        )
        .await
        {
            Ok(Ok(snapshot)) => f64::try_from(snapshot.minimum_tick_size)
                .ok()
                .filter(|v| v.is_finite() && *v > 0.0)
                .unwrap_or(0.01),
            _ => 0.01,
        };

        let position_size = basis.position_size;
        if !position_size.is_finite() || position_size <= 0.0 {
            log_event(
                "premarket_tp_retry",
                json!({
                    "trade_key": trade_key.as_str(),
                    "reason": "invalid_position_size",
                    "retry_in_ms": Self::premarket_tp_retry_interval_ms()
                }),
            );
            Self::set_premarket_tp_retry_after(
                retry_after_ms.clone(),
                trade_key.as_str(),
                retry_after_default,
            )
            .await;
            Self::release_premarket_tp_inflight(inflight, trade_key.as_str()).await;
            return;
        }

        let raw_tp = Self::premarket_tp_price_target(basis.avg_entry_price, top_ask);
        let tp_price =
            Self::round_price_to_tick(raw_tp.clamp(0.01, 0.99), tick_size).clamp(0.01, 0.99);
        if !tp_price.is_finite() || tp_price <= 0.0 {
            log_event(
                "premarket_tp_retry",
                json!({
                    "trade_key": trade_key.as_str(),
                    "reason": "invalid_tp_price",
                    "retry_in_ms": Self::premarket_tp_retry_interval_ms()
                }),
            );
            Self::set_premarket_tp_retry_after(
                retry_after_ms.clone(),
                trade_key.as_str(),
                retry_after_default,
            )
            .await;
            Self::release_premarket_tp_inflight(inflight, trade_key.as_str()).await;
            return;
        }

        let sell_order = OrderRequest {
            token_id: trade.token_id.clone(),
            side: "SELL".to_string(),
            size: format!("{:.6}", position_size),
            price: format!("{:.*}", Self::tick_decimal_places(tick_size), tp_price),
            order_type: "LIMIT".to_string(),
            expiration_ts: None,
            post_only: None,
        };

        let place_result = tokio::time::timeout(
            tokio::time::Duration::from_millis(Self::premarket_tp_api_timeout_ms()),
            api.place_order(&sell_order),
        )
        .await;

        let response = match place_result {
            Ok(Ok(response)) => response,
            Ok(Err(e)) => {
                log_event(
                    "premarket_tp_retry",
                    json!({
                        "trade_key": trade_key.as_str(),
                        "reason": format!("tp_place_failed: {}", e),
                        "retry_in_ms": Self::premarket_tp_retry_interval_ms()
                    }),
                );
                Self::set_premarket_tp_retry_after(
                    retry_after_ms.clone(),
                    trade_key.as_str(),
                    retry_after_default,
                )
                .await;
                Self::release_premarket_tp_inflight(inflight, trade_key.as_str()).await;
                return;
            }
            Err(_) => {
                log_event(
                    "premarket_tp_retry",
                    json!({
                        "trade_key": trade_key.as_str(),
                        "reason": "tp_place_timeout",
                        "retry_in_ms": Self::premarket_tp_retry_interval_ms()
                    }),
                );
                Self::set_premarket_tp_retry_after(
                    retry_after_ms.clone(),
                    trade_key.as_str(),
                    retry_after_default,
                )
                .await;
                Self::release_premarket_tp_inflight(inflight, trade_key.as_str()).await;
                return;
            }
        };

        {
            let mut pending = pending_trades.lock().await;
            if let Some(t) = pending.get_mut(trade_key.as_str()) {
                if !t.sold && !t.limit_sell_orders_placed {
                    t.limit_sell_orders_placed = true;
                    t.sell_price = tp_price;
                    t.units = position_size;
                    t.confirmed_balance = Some(position_size);
                }
            }
        }

        if let Some(db) = tracking_db.as_ref() {
            let order_id = response.order_id.clone().unwrap_or_else(|| {
                format!(
                    "local:premarket_tp:{}:{}",
                    trade.market_timestamp, trade.token_id
                )
            });
            let record = PendingOrderRecord {
                order_id,
                trade_key: trade_key.clone(),
                token_id: trade.token_id.clone(),
                condition_id: Some(trade.condition_id.clone()),
                timeframe: trade.source_timeframe.clone(),
                strategy_id: trade.strategy_id.clone(),
                asset_symbol: Some(Self::token_family(&trade.token_type).to_string()),
                entry_mode: trade.entry_mode.clone(),
                period_timestamp: trade.market_timestamp,
                price: tp_price,
                size_usd: tp_price * position_size,
                side: "SELL".to_string(),
                status: "OPEN".to_string(),
            };
            if let Err(e) = db.upsert_pending_order(&record) {
                warn!(
                    "Failed to upsert premarket TP pending order tracking for {}: {}",
                    trade_key, e
                );
            }
        }

        log_event(
            "premarket_tp_placed",
            json!({
                "trade_key": trade_key.as_str(),
                "strategy_id": trade.strategy_id,
                "timeframe": trade.source_timeframe,
                "period_timestamp": trade.market_timestamp,
                "condition_id": trade.condition_id,
                "token_id": trade.token_id,
                "avg_entry_price": basis.avg_entry_price,
                "position_size": position_size,
                "top_ask": top_ask,
                "tp_price": tp_price,
                "min_tp_floor": 0.60,
                "multiple": 2.0
            }),
        );

        Self::set_premarket_tp_retry_after(retry_after_ms.clone(), trade_key.as_str(), None).await;
        Self::release_premarket_tp_inflight(inflight, trade_key.as_str()).await;
    }

    async fn enqueue_due_premarket_tp_jobs(&self, pending_trades: &[(String, PendingTrade)]) {
        if self.simulation_mode || !Self::premarket_tp_enabled() {
            return;
        }
        let Some(tx) = self.premarket_tp_tx.as_ref() else {
            return;
        };

        let now_ms = chrono::Utc::now().timestamp_millis();
        for (trade_key, trade) in pending_trades {
            if !Self::premarket_tp_is_trade_eligible(trade, now_ms) {
                continue;
            }

            let retry_after = {
                let retry = self.premarket_tp_retry_after_ms.lock().await;
                retry.get(trade_key.as_str()).copied().unwrap_or(0)
            };
            if retry_after > now_ms {
                continue;
            }

            let claimed = {
                let mut inflight = self.premarket_tp_inflight.lock().await;
                inflight.insert(trade_key.clone())
            };
            if !claimed {
                continue;
            }

            let send_result = tx.try_send(PremarketTpJob {
                trade_key: trade_key.clone(),
            });
            match send_result {
                Ok(_) => {
                    log_event(
                        "premarket_tp_enqueued",
                        json!({
                            "trade_key": trade_key,
                            "strategy_id": trade.strategy_id,
                            "timeframe": trade.source_timeframe,
                            "period_timestamp": trade.market_timestamp,
                            "token_id": trade.token_id
                        }),
                    );
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    Self::set_premarket_tp_retry_after(
                        self.premarket_tp_retry_after_ms.clone(),
                        trade_key.as_str(),
                        Some(now_ms.saturating_add(Self::premarket_tp_retry_interval_ms())),
                    )
                    .await;
                    Self::release_premarket_tp_inflight(
                        self.premarket_tp_inflight.clone(),
                        trade_key.as_str(),
                    )
                    .await;
                    log_event(
                        "premarket_tp_enqueue_backpressure",
                        json!({
                            "trade_key": trade_key,
                            "retry_in_ms": Self::premarket_tp_retry_interval_ms()
                        }),
                    );
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    Self::set_premarket_tp_retry_after(
                        self.premarket_tp_retry_after_ms.clone(),
                        trade_key.as_str(),
                        Some(now_ms.saturating_add(Self::premarket_tp_retry_interval_ms())),
                    )
                    .await;
                    Self::release_premarket_tp_inflight(
                        self.premarket_tp_inflight.clone(),
                        trade_key.as_str(),
                    )
                    .await;
                    warn!(
                        "premarket TP worker channel closed for trade_key={}",
                        trade_key
                    );
                }
            }
        }
    }

    async fn enforce_trade_hold_to_resolution(&self, trade_key: &str, reason: &str) {
        let mut pending = self.pending_trades.lock().await;
        if let Some(trade) = pending.get_mut(trade_key) {
            let changed =
                !(trade.no_sell && trade.claim_on_closure && trade.limit_sell_orders_placed);
            trade.no_sell = true;
            trade.claim_on_closure = true;
            trade.limit_sell_orders_placed = true;
            if changed {
                crate::log_println!(
                    "✅ Hold-to-resolution policy applied ({}) | period={} token={}",
                    reason,
                    trade.market_timestamp,
                    &trade.token_id[..16]
                );
            }
        }
    }

    fn env_f64(name: &str, default: f64) -> f64 {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(default)
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

    fn constraint_cache_ttl_ms() -> i64 {
        std::env::var("EVPOLY_CONSTRAINT_CACHE_TTL_MS")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(180_000)
            .clamp(1_000, 86_400_000)
    }

    fn constraint_cache_stale_fallback_ms() -> i64 {
        std::env::var("EVPOLY_CONSTRAINT_CACHE_STALE_FALLBACK_MS")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(900_000)
            .clamp(0, 86_400_000)
    }

    fn constraint_cache_max_entries() -> usize {
        std::env::var("EVPOLY_CONSTRAINT_CACHE_MAX")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(5_000)
            .max(100)
    }

    fn is_transient_constraint_error(err_text: &str) -> bool {
        let e = err_text.to_ascii_lowercase();
        e.contains("429")
            || e.contains("too many requests")
            || e.contains("rate limit")
            || e.contains("timeout")
            || e.contains("timed out")
            || e.contains("bad gateway")
            || e.contains("service unavailable")
            || e.contains("gateway timeout")
            || e.contains("temporarily unavailable")
            || e.contains("connection reset")
            || e.contains("connection aborted")
    }

    fn market_constraints_from_snapshot(
        market: MarketConstraintSnapshot,
    ) -> MarketOrderConstraints {
        let min_notional_usd = f64::try_from(market.minimum_order_size)
            .unwrap_or(0.0)
            .max(0.0);
        let min_size_shares = f64::try_from(market.min_size_shares)
            .unwrap_or(0.0)
            .max(0.0);
        let tick_size = f64::try_from(market.minimum_tick_size)
            .unwrap_or(0.01)
            .max(0.000001);
        MarketOrderConstraints {
            min_notional_usd,
            min_size_shares,
            tick_size,
        }
    }

    pub async fn prime_market_constraints_snapshot(
        &self,
        condition_id: &str,
        snapshot: MarketConstraintSnapshot,
    ) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let ttl_ms = Self::constraint_cache_ttl_ms();
        let constraints = Self::market_constraints_from_snapshot(snapshot);
        let mut cache = self.market_constraints_cache.lock().await;
        let stale_prune_before_ms = now_ms.saturating_sub(ttl_ms.saturating_mul(4));
        cache.retain(|_, entry| entry.fetched_at_ms >= stale_prune_before_ms);
        cache.insert(
            condition_id.to_string(),
            MarketConstraintCacheEntry {
                constraints,
                fetched_at_ms: now_ms,
            },
        );
        let max_entries = Self::constraint_cache_max_entries();
        if cache.len() > max_entries {
            let mut by_age = cache
                .iter()
                .map(|(key, entry)| (key.clone(), entry.fetched_at_ms))
                .collect::<Vec<_>>();
            by_age.sort_by_key(|(_, fetched_at_ms)| *fetched_at_ms);
            let remove_count = cache.len().saturating_sub(max_entries);
            for (key, _) in by_age.into_iter().take(remove_count) {
                cache.remove(key.as_str());
            }
        }
    }

    fn force_constraint_refresh_on_tick_error() -> bool {
        Self::env_bool("EVPOLY_FORCE_CONSTRAINT_REFRESH_ON_TICK_ERROR", true)
    }

    fn should_refresh_constraints_for_error(reason: &str) -> bool {
        let r = reason.to_ascii_lowercase();
        r.contains("invalid_order_min_tick_size")
            || r.contains("invalid_order_min_size")
            || r.contains("minimum_order_size")
            || r.contains("min_size")
    }

    fn is_transient_price_error(err_text: &str) -> bool {
        let e = err_text.to_ascii_lowercase();
        e.contains("status: 429")
            || e.contains("status 429")
            || e.contains("error code: 429")
            || e.contains("error code:429")
            || e.contains("status: 500")
            || e.contains("status 500")
            || e.contains("error code: 500")
            || e.contains("error code:500")
            || e.contains("status: 502")
            || e.contains("status 502")
            || e.contains("error code: 502")
            || e.contains("error code:502")
            || e.contains("status: 503")
            || e.contains("status 503")
            || e.contains("error code: 503")
            || e.contains("error code:503")
            || e.contains("status: 504")
            || e.contains("status 504")
            || e.contains("error code: 504")
            || e.contains("error code:504")
            || e.contains("bad gateway")
            || e.contains("service unavailable")
            || e.contains("gateway timeout")
            || e.contains("timed out")
            || e.contains("timeout")
            || e.contains("connection reset")
            || e.contains("connection aborted")
            || e.contains("temporarily unavailable")
    }

    async fn fetch_price_with_retry_for_sim(
        &self,
        token_id: &str,
        side: &str,
        tracker: &Arc<SimulationTracker>,
    ) -> Option<Decimal> {
        let attempts = std::env::var("EVPOLY_PRICE_RETRY_ATTEMPTS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(3)
            .max(1);
        let base_delay_ms = std::env::var("EVPOLY_PRICE_RETRY_BASE_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(200);

        for attempt in 1..=attempts {
            match self.api.get_price(token_id, side).await {
                Ok(price) => return Some(price),
                Err(e) => {
                    let err_text = e.to_string();
                    let transient = Self::is_transient_price_error(&err_text);
                    if transient && attempt < attempts {
                        let delay_ms = base_delay_ms.saturating_mul(1u64 << (attempt - 1));
                        tracker
                            .log_to_file(&format!(
                                "⚠️  SIMULATION: transient {} price fetch error for token {} (attempt {}/{}): {}. retrying in {}ms",
                                side,
                                &token_id[..16],
                                attempt,
                                attempts,
                                e,
                                delay_ms
                            ))
                            .await;
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                        continue;
                    }
                    tracker
                        .log_to_file(&format!(
                            "⚠️  SIMULATION: Failed to fetch {} price for token {} after {}/{} attempt(s): {}",
                            side,
                            &token_id[..16],
                            attempt,
                            attempts,
                            e
                        ))
                        .await;
                    return None;
                }
            }
        }

        None
    }

    fn timeframe_duration_seconds(label: &str) -> u64 {
        match label {
            "5m" => 300,
            "15m" => 900,
            "1h" => 3600,
            "4h" => 14_400,
            "1d" => 86_400,
            _ => 900,
        }
    }

    fn estimate_end_timestamp(period_timestamp: u64, source_timeframe: &str) -> u64 {
        period_timestamp + Self::timeframe_duration_seconds(source_timeframe)
    }

    fn configured_order_ttl_seconds(&self) -> u64 {
        self.config.order_ttl_seconds.unwrap_or(1200).max(61)
    }

    fn premarket_fallback_expiration_seconds(source_timeframe: &str) -> u64 {
        let normalized = Self::normalize_timeframe_label(Some(source_timeframe));
        let (env_key, default_seconds) = match normalized.as_str() {
            "5m" => ("EVPOLY_PREMARKET_EXPIRE_5M_SEC", 260_u64),
            "15m" => ("EVPOLY_PREMARKET_EXPIRE_15M_SEC", 270_u64),
            "1h" => ("EVPOLY_PREMARKET_EXPIRE_1H_SEC", 320_u64),
            "4h" => ("EVPOLY_PREMARKET_EXPIRE_4H_SEC", 500_u64),
            "1d" => ("EVPOLY_PREMARKET_EXPIRE_1D_SEC", 900_u64),
            _ => ("EVPOLY_PREMARKET_EXPIRE_15M_SEC", 270_u64),
        };
        std::env::var(env_key)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(default_seconds)
            .clamp(61, 14_400)
    }

    fn premarket_ladder_expiration_seconds(
        source_timeframe: &str,
        strategy_id: &str,
        submit_price: f64,
    ) -> u64 {
        let normalized = Self::normalize_timeframe_label(Some(source_timeframe));
        let mut fallback = Self::premarket_fallback_expiration_seconds(normalized.as_str());
        if normalized != "5m"
            || strategy_id != crate::strategy::STRATEGY_ID_PREMARKET_V1
            || !submit_price.is_finite()
            || submit_price <= 0.0
        {
            return fallback;
        }

        let high_rung_min_price = std::env::var("EVPOLY_PREMARKET_EXPIRE_5M_HIGH_RUNG_MIN_PRICE")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(0.30);
        let high_rung_seconds = std::env::var("EVPOLY_PREMARKET_EXPIRE_5M_HIGH_RUNG_SEC")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(240)
            .clamp(61, 14_400);
        let base_rung_seconds = std::env::var("EVPOLY_PREMARKET_EXPIRE_5M_BASE_RUNG_SEC")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(fallback)
            .clamp(61, 14_400);

        fallback = base_rung_seconds;
        if submit_price + 1e-9 >= high_rung_min_price {
            high_rung_seconds
        } else {
            fallback
        }
    }

    fn compute_limit_order_expiration_ts(
        now_ts: u64,
        period_timestamp: u64,
        source_timeframe: &str,
        configured_ttl_seconds: u64,
        entry_mode: EntryExecutionMode,
        strategy_id: &str,
        submit_price: f64,
    ) -> u64 {
        let premarket_ladder =
            entry_mode.is_ladder() && strategy_id == crate::strategy::STRATEGY_ID_PREMARKET_V1;
        let mode_deadline = if entry_mode.is_ladder() {
            let ttl_deadline = now_ts.saturating_add(Self::premarket_ladder_expiration_seconds(
                source_timeframe,
                strategy_id,
                submit_price,
            ));
            if premarket_ladder {
                ttl_deadline.min(Self::ladder_cancel_deadline_ts(
                    period_timestamp,
                    source_timeframe,
                ))
            } else {
                ttl_deadline
            }
        } else {
            let ttl_target = now_ts.saturating_add(configured_ttl_seconds.max(61));
            let market_end = Self::estimate_end_timestamp(period_timestamp, source_timeframe);
            ttl_target.min(market_end.saturating_sub(5))
        };
        if premarket_ladder {
            mode_deadline.max(now_ts.saturating_add(1))
        } else {
            mode_deadline.max(now_ts.saturating_add(61))
        }
    }

    fn limit_order_expiration_ts(
        &self,
        period_timestamp: u64,
        source_timeframe: &str,
        entry_mode: EntryExecutionMode,
        strategy_id: &str,
        submit_price: f64,
    ) -> i64 {
        let now_ts = chrono::Utc::now().timestamp().max(0) as u64;
        Self::compute_limit_order_expiration_ts(
            now_ts,
            period_timestamp,
            source_timeframe,
            self.configured_order_ttl_seconds(),
            entry_mode,
            strategy_id,
            submit_price,
        ) as i64
    }

    fn market_end_timestamp(trade: &PendingTrade) -> u64 {
        trade.end_timestamp.unwrap_or_else(|| {
            Self::estimate_end_timestamp(trade.market_timestamp, trade.source_timeframe.as_str())
        })
    }

    fn premarket_cancel_after_launch_seconds(source_timeframe: &str) -> u64 {
        let normalized = Self::normalize_timeframe_label(Some(source_timeframe));
        let (env_key, default_seconds) = match normalized.as_str() {
            "5m" => ("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_5M_SEC", 5_u64),
            "15m" => ("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_15M_SEC", 40_u64),
            "1h" => ("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_1H_SEC", 60_u64),
            "4h" => ("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_4H_SEC", 180_u64),
            "1d" => ("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_1D_SEC", 600_u64),
            _ => ("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_15M_SEC", 40_u64),
        };
        std::env::var(env_key)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(default_seconds)
            .min(600)
    }

    fn ladder_cancel_deadline_ts(period_timestamp: u64, source_timeframe: &str) -> u64 {
        period_timestamp.saturating_add(Self::premarket_cancel_after_launch_seconds(
            source_timeframe,
        ))
    }

    fn endgame_cancel_after_ms() -> u64 {
        std::env::var("EVPOLY_ENDGAME_CANCEL_AFTER_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(5_000)
            .clamp(500, 60_000)
    }

    fn evcurve_cancel_after_ms() -> u64 {
        std::env::var("EVPOLY_EVCURVE_CANCEL_AFTER_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(10_000)
            .clamp(500, 120_000)
    }

    fn mm_rewards_cancel_after_ms() -> u64 {
        std::env::var("EVPOLY_MM_QUOTE_TTL_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(20_000)
            .clamp(500, 300_000)
    }

    fn endgame_order_cancel_due(
        trade: &PendingTrade,
        now: std::time::Instant,
        cancel_after_ms: u64,
    ) -> bool {
        now.duration_since(trade.timestamp).as_millis() >= u128::from(cancel_after_ms)
    }

    fn round_price_to_tick(price: f64, tick: f64) -> f64 {
        if tick <= 0.0 {
            return price;
        }
        (price / tick).round() * tick
    }

    fn round_price_to_tick_down(price: f64, tick: f64) -> f64 {
        if tick <= 0.0 {
            return price;
        }
        (price / tick).floor() * tick
    }

    fn is_post_only_cross_reject(err: &anyhow::Error) -> bool {
        let msg = err.to_string().to_ascii_lowercase();
        msg.contains("post-only")
            && (msg.contains("crosses book")
                || msg.contains("would match")
                || msg.contains("marketable"))
    }

    fn is_tick_aligned(price: f64, tick: f64) -> bool {
        if tick <= 0.0 {
            return true;
        }
        let aligned = Self::round_price_to_tick(price, tick);
        (aligned - price).abs() <= 1e-9_f64.max(tick * 1e-6)
    }

    fn tick_decimal_places(tick: f64) -> usize {
        if !tick.is_finite() || tick <= 0.0 {
            return 2;
        }
        let mut scaled = tick.abs();
        let mut decimals = 0usize;
        while decimals < 8 {
            let rounded = scaled.round();
            if (scaled - rounded).abs() <= 1e-9 {
                break;
            }
            scaled *= 10.0;
            decimals = decimals.saturating_add(1);
        }
        decimals.clamp(2, 8)
    }

    fn round_units_for_order(units: f64) -> f64 {
        (units * 100.0).round() / 100.0
    }

    fn floor_units_for_order(units: f64) -> f64 {
        ((units * 100.0) + 1e-9).floor() / 100.0
    }

    fn ceil_units_for_order(units: f64) -> f64 {
        (units * 100.0).ceil() / 100.0
    }

    fn enforce_min_order_constraints(
        requested_units: f64,
        submit_price: f64,
        constraints: Option<MarketOrderConstraints>,
    ) -> std::result::Result<(f64, f64, bool), String> {
        if !submit_price.is_finite() || submit_price <= 0.0 {
            return Err(format!(
                "invalid_order_min_tick_size: non-positive price {:.8}",
                submit_price
            ));
        }
        if !requested_units.is_finite() || requested_units <= 0.0 {
            return Err(format!(
                "invalid_order_min_size: non-positive units {:.8}",
                requested_units
            ));
        }

        let mut units = Self::round_units_for_order(requested_units);
        if units <= 0.0 {
            return Err("invalid_order_min_size: rounded units became non-positive".to_string());
        }
        let mut adjusted_to_minimum = false;

        if let Some(c) = constraints {
            let mut required_units = units;
            if c.min_size_shares > 0.0 {
                required_units = required_units.max(c.min_size_shares);
            }
            if c.min_notional_usd > 0.0 {
                required_units = required_units.max(c.min_notional_usd / submit_price);
            }
            required_units = Self::ceil_units_for_order(required_units);
            if required_units > units + 1e-9 {
                units = required_units;
                adjusted_to_minimum = true;
            }

            let notional = units * submit_price;
            if c.min_notional_usd > 0.0 && notional + 1e-9 < c.min_notional_usd {
                return Err(format!(
                    "invalid_order_min_size: notional {:.6} below minimum_order_size {:.6}",
                    notional, c.min_notional_usd
                ));
            }
            if c.min_size_shares > 0.0 && units + 1e-9 < c.min_size_shares {
                return Err(format!(
                    "invalid_order_min_size: size {:.6} below min_size {:.6}",
                    units, c.min_size_shares
                ));
            }
            if !Self::is_tick_aligned(submit_price, c.tick_size) {
                return Err(format!(
                    "invalid_order_min_tick_size: price {:.6} not aligned to tick {:.6}",
                    submit_price, c.tick_size
                ));
            }
        }

        Ok((units, units * submit_price, adjusted_to_minimum))
    }

    async fn market_order_constraints(&self, condition_id: &str) -> Result<MarketOrderConstraints> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let ttl_ms = Self::constraint_cache_ttl_ms();
        let mut stale_cached: Option<MarketConstraintCacheEntry> = None;
        {
            let cache = self.market_constraints_cache.lock().await;
            if let Some(found) = cache.get(condition_id).copied() {
                if now_ms.saturating_sub(found.fetched_at_ms) <= ttl_ms {
                    return Ok(found.constraints);
                }
                stale_cached = Some(found);
            }
        }

        let market = match self.api.get_market_constraints(condition_id).await {
            Ok(market) => market,
            Err(e) => {
                let err_text = e.to_string();
                if let Some(found) = stale_cached {
                    let stale_age_ms = now_ms.saturating_sub(found.fetched_at_ms);
                    let stale_fallback_ms = Self::constraint_cache_stale_fallback_ms();
                    if stale_fallback_ms > 0
                        && stale_age_ms <= stale_fallback_ms
                        && Self::is_transient_constraint_error(err_text.as_str())
                    {
                        log_event(
                            "entry_constraint_cache_stale_fallback",
                            json!({
                                "condition_id": condition_id,
                                "stale_age_ms": stale_age_ms,
                                "stale_fallback_ms": stale_fallback_ms,
                                "error": err_text
                            }),
                        );
                        return Ok(found.constraints);
                    }
                }
                return Err(anyhow!(
                    "market_not_ready: failed to fetch market constraints for {}: {}",
                    condition_id,
                    e
                ));
            }
        };
        if !market.accepting_orders || !market.active || market.closed {
            return Err(anyhow!(
                "market_not_ready: market {} accepting_orders={} active={} closed={}",
                condition_id,
                market.accepting_orders,
                market.active,
                market.closed
            ));
        }

        let constraints = Self::market_constraints_from_snapshot(market);

        let mut cache = self.market_constraints_cache.lock().await;
        let stale_prune_before_ms = now_ms.saturating_sub(ttl_ms.saturating_mul(4));
        cache.retain(|_, entry| entry.fetched_at_ms >= stale_prune_before_ms);
        cache.insert(
            condition_id.to_string(),
            MarketConstraintCacheEntry {
                constraints,
                fetched_at_ms: now_ms,
            },
        );
        let max_entries = Self::constraint_cache_max_entries();
        if cache.len() > max_entries {
            let mut by_age = cache
                .iter()
                .map(|(key, entry)| (key.clone(), entry.fetched_at_ms))
                .collect::<Vec<_>>();
            by_age.sort_by_key(|(_, fetched_at_ms)| *fetched_at_ms);
            let remove_count = cache.len().saturating_sub(max_entries);
            for (key, _) in by_age.into_iter().take(remove_count) {
                cache.remove(key.as_str());
            }
        }
        Ok(constraints)
    }

    async fn invalidate_market_constraints_cache(&self, condition_id: &str) {
        let mut cache = self.market_constraints_cache.lock().await;
        cache.remove(condition_id);
    }

    async fn maybe_refresh_market_constraints_for_reason(
        &self,
        condition_id: &str,
        reason: &str,
        phase: &str,
    ) {
        if !Self::force_constraint_refresh_on_tick_error()
            || !Self::should_refresh_constraints_for_error(reason)
        {
            return;
        }
        self.invalidate_market_constraints_cache(condition_id).await;
        match self.market_order_constraints(condition_id).await {
            Ok(refreshed) => {
                log_event(
                    "entry_constraint_cache_refreshed",
                    json!({
                        "condition_id": condition_id,
                        "phase": phase,
                        "reason": reason,
                        "tick_size": refreshed.tick_size,
                        "min_size_shares": refreshed.min_size_shares,
                        "min_notional_usd": refreshed.min_notional_usd
                    }),
                );
            }
            Err(err) => {
                warn!(
                    "constraint cache refresh failed condition={} phase={} reason={} err={}",
                    condition_id, phase, reason, err
                );
            }
        }
    }

    fn limit_trade_key(
        entry_mode: EntryExecutionMode,
        period_timestamp: u64,
        token_id: &str,
        price: f64,
        source_timeframe: &str,
        strategy_id: &str,
        request_id: Option<&str>,
    ) -> String {
        let price_cents = (price * 100.0).round() as i64;
        let price_mills = (price * 1000.0).round() as i64;
        let timeframe = source_timeframe.trim().to_ascii_lowercase();
        let strategy = strategy_id.trim().to_ascii_lowercase();
        let request_suffix_for = |prefix: &str| -> Option<String> {
            request_id.and_then(|raw| {
                raw.rsplit(':').find_map(|part| {
                    let trimmed = part.trim();
                    if trimmed.len() <= prefix.len() {
                        return None;
                    }
                    let (head, tail) = trimmed.split_at(prefix.len());
                    if head.eq_ignore_ascii_case(prefix)
                        && !tail.is_empty()
                        && tail.chars().all(|ch| ch.is_ascii_digit())
                    {
                        Some(trimmed.to_ascii_lowercase())
                    } else {
                        None
                    }
                })
            })
        };
        let request_tick_suffix = request_suffix_for("tick");
        let request_ladder_suffix = request_suffix_for("l");
        match entry_mode {
            EntryExecutionMode::Ladder => {
                format!(
                    "{}_{}_ladder_{}_{}_{}",
                    period_timestamp, token_id, price_cents, timeframe, strategy
                )
            }
            EntryExecutionMode::Endgame => {
                if let Some(tick_suffix) = request_tick_suffix.as_deref() {
                    format!(
                        "{}_{}_endgame_{}_{}_{}_{}",
                        period_timestamp, token_id, price_cents, timeframe, strategy, tick_suffix
                    )
                } else {
                    format!(
                        "{}_{}_endgame_{}_{}_{}",
                        period_timestamp, token_id, price_cents, timeframe, strategy
                    )
                }
            }
            EntryExecutionMode::Evcurve => {
                if let Some(tick_suffix) = request_tick_suffix.as_deref() {
                    format!(
                        "{}_{}_evcurve_{}_{}_{}_{}",
                        period_timestamp, token_id, price_cents, timeframe, strategy, tick_suffix
                    )
                } else {
                    format!(
                        "{}_{}_evcurve_{}_{}_{}",
                        period_timestamp, token_id, price_cents, timeframe, strategy
                    )
                }
            }
            EntryExecutionMode::SessionBand => {
                format!(
                    "{}_{}_sessionband_{}_{}_{}",
                    period_timestamp, token_id, price_cents, timeframe, strategy
                )
            }
            EntryExecutionMode::Evsnipe => {
                format!(
                    "{}_{}_evsnipe_{}_{}_{}",
                    period_timestamp, token_id, price_cents, timeframe, strategy
                )
            }
            EntryExecutionMode::MmRewards => {
                if let Some(ladder_suffix) = request_ladder_suffix.as_deref() {
                    format!(
                        "{}_{}_mm_rewards_{}_{}_{}_{}",
                        period_timestamp, token_id, price_mills, timeframe, strategy, ladder_suffix
                    )
                } else {
                    format!(
                        "{}_{}_mm_rewards_{}_{}_{}",
                        period_timestamp, token_id, price_mills, timeframe, strategy
                    )
                }
            }
            EntryExecutionMode::Restored | EntryExecutionMode::Legacy => {
                format!(
                    "{}_{}_limit_{}_{}_{}",
                    period_timestamp, token_id, price_cents, timeframe, strategy
                )
            }
        }
    }

    fn is_limit_style_key(key: &str) -> bool {
        key.contains("_limit")
            || key.contains("_ladder_")
            || key.contains("_endgame_")
            || key.contains("_evcurve_")
            || key.contains("_evsnipe_")
            || key.contains("_mm_rewards_")
    }

    fn should_run_exchange_reconcile() -> bool {
        static LAST_RECONCILE_MS: AtomicI64 = AtomicI64::new(0);
        let interval_ms = std::env::var("EVPOLY_ORDER_RECONCILE_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(30_000)
            .max(5_000);
        let now_ms = chrono::Utc::now().timestamp_millis();
        let last = LAST_RECONCILE_MS.load(Ordering::Relaxed);
        if now_ms.saturating_sub(last) >= interval_ms {
            LAST_RECONCILE_MS.store(now_ms, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    fn order_reconcile_retry_max_rows() -> usize {
        std::env::var("EVPOLY_ORDER_RECONCILE_RETRY_MAX_ROWS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(32)
            .clamp(0, 5_000)
    }

    fn order_reconcile_retry_min_age_ms() -> i64 {
        std::env::var("EVPOLY_ORDER_RECONCILE_RETRY_MIN_AGE_MS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(300_000)
            .clamp(0, 86_400_000)
    }

    fn order_reconcile_retry_max_period_age_sec() -> i64 {
        std::env::var("EVPOLY_ORDER_RECONCILE_RETRY_MAX_PERIOD_AGE_SEC")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(172_800)
            .clamp(0, 31_536_000)
    }

    fn entry_reservations() -> &'static StdMutex<HashSet<String>> {
        static RESERVATIONS: OnceLock<StdMutex<HashSet<String>>> = OnceLock::new();
        RESERVATIONS.get_or_init(|| StdMutex::new(HashSet::new()))
    }

    fn try_reserve_entry_slot(slot: &str) -> bool {
        let mut guard = Self::entry_reservations()
            .lock()
            .expect("entry reservation mutex poisoned");
        if guard.contains(slot) {
            false
        } else {
            guard.insert(slot.to_string());
            true
        }
    }

    fn release_entry_slot(slot: &str) {
        let mut guard = Self::entry_reservations()
            .lock()
            .expect("entry reservation mutex poisoned");
        guard.remove(slot);
    }

    fn local_pending_order_id(
        entry_mode: EntryExecutionMode,
        source_timeframe: &str,
        period_timestamp: u64,
        token_id: &str,
        trade_key: &str,
    ) -> String {
        format!(
            "local:entry:{}:{}:{}:{}:{}",
            entry_mode.as_str(),
            source_timeframe,
            period_timestamp,
            token_id,
            trade_key
        )
    }

    fn classify_entry_failure_reason(error: &anyhow::Error) -> String {
        match classify_entry_error(error.to_string().as_str()) {
            ExecutionErrorClass::BalanceAllowance => "not enough balance / allowance".to_string(),
            ExecutionErrorClass::MarketNotReady => "market not ready".to_string(),
            ExecutionErrorClass::Retryable => "transient submit failure".to_string(),
            ExecutionErrorClass::ParameterInvalid => {
                "invalid order parameters (min size/tick)".to_string()
            }
            ExecutionErrorClass::Permanent => {
                let raw = error.to_string();
                let first_line = raw.lines().next().unwrap_or("entry submit failed").trim();
                if first_line.is_empty() {
                    "entry submit failed".to_string()
                } else {
                    first_line.to_string()
                }
            }
        }
    }

    fn is_fak_no_match_error(error: &anyhow::Error) -> bool {
        let lower = error.to_string().to_ascii_lowercase();
        lower.contains("no orders found to match with fak order")
            || lower.contains("no orders found to match with fok order")
    }

    fn record_entry_precheck_failure(
        &self,
        strategy_id: &str,
        source_timeframe: &str,
        entry_mode: EntryExecutionMode,
        opportunity: &BuyOpportunity,
        submit_price: Option<f64>,
        units: Option<f64>,
        notional_usd: Option<f64>,
        reason: String,
    ) {
        let ts_ms = chrono::Utc::now().timestamp_millis();
        log_event(
            "entry_precheck_failed",
            json!({
                "strategy_id": strategy_id,
                "timeframe": source_timeframe,
                "entry_mode": entry_mode.as_str(),
                "period_timestamp": opportunity.period_timestamp,
                "token_id": opportunity.token_id,
                "token_type": opportunity.token_type.display_name(),
                "submit_price": submit_price,
                "units": units,
                "notional_usd": notional_usd,
                "reason": reason,
                "ts_ms": ts_ms
            }),
        );
        self.record_trade_event_for_strategy(
            Some(strategy_id),
            Some(format!(
                "entry_failed_precheck:{}:{}:{}:{}:{}",
                strategy_id,
                source_timeframe,
                opportunity.period_timestamp,
                opportunity.token_id,
                ts_ms
            )),
            opportunity.period_timestamp,
            Some(source_timeframe),
            Some(opportunity.condition_id.clone()),
            Some(opportunity.token_id.clone()),
            Some(opportunity.token_type.display_name().to_string()),
            Some("BUY".to_string()),
            "ENTRY_FAILED",
            submit_price,
            units,
            notional_usd,
            None,
            Some(format!("precheck: {}", reason)),
        );
    }

    fn is_exchange_order_id(order_id: &str) -> bool {
        let trimmed = order_id.trim();
        if trimmed.is_empty() {
            return false;
        }
        if trimmed.starts_with("local:") || trimmed.starts_with("restored:") {
            return false;
        }
        let Some(body) = trimmed.strip_prefix("0x") else {
            return false;
        };
        !body.is_empty() && body.chars().all(|c| c.is_ascii_hexdigit())
    }

    fn order_lookup_error_is_terminal(error_text: &str) -> bool {
        let lower = error_text.to_ascii_lowercase();
        lower.contains("404")
            || lower.contains("not found")
            || lower.contains("unknown order")
            || lower.contains("does not exist")
    }

    fn unknown_order_status_terminal_bucket(raw: &str) -> Option<&'static str> {
        let lower = raw.trim().to_ascii_lowercase();
        if lower.is_empty() {
            return None;
        }
        if lower.contains("match") || lower.contains("filled") {
            return Some("FILLED");
        }
        if lower.contains("cancel")
            || lower.contains("unmatch")
            || lower.contains("expire")
            || lower.contains("reject")
        {
            return Some("CANCELED");
        }
        if lower.contains("invalid")
            || lower.contains("unknown order")
            || lower.contains("not found")
            || lower.contains("does not exist")
        {
            return Some("STALE");
        }
        None
    }

    fn terminal_status_for_order(order_status: OrderStatusType) -> Option<&'static str> {
        match order_status {
            OrderStatusType::Matched => Some("FILLED"),
            OrderStatusType::Canceled | OrderStatusType::Unmatched => Some("CANCELED"),
            OrderStatusType::Unknown(raw) => {
                Self::unknown_order_status_terminal_bucket(raw.as_str())
            }
            _ => None,
        }
    }

    fn matched_fill_from_order(order: &OpenOrderResponse) -> Option<(f64, f64, f64)> {
        let matched_units = f64::try_from(order.size_matched).ok()?;
        if !matched_units.is_finite() || matched_units <= 0.0 {
            return None;
        }
        let price = f64::try_from(order.price).ok()?;
        if !price.is_finite() || price <= 0.0 {
            return None;
        }
        let notional_usd = matched_units * price;
        if !notional_usd.is_finite() || notional_usd <= 0.0 {
            return None;
        }
        Some((price, matched_units, notional_usd))
    }

    async fn reconcile_active_pending_orders_with_exchange(&self) -> Result<()> {
        self.reconcile_active_pending_orders_with_exchange_filtered(None)
            .await
    }

    pub async fn reconcile_active_pending_orders_for_strategy(
        &self,
        strategy_id: &str,
    ) -> Result<()> {
        let normalized_strategy_id = Self::normalize_strategy_id(Some(strategy_id));
        self.reconcile_active_pending_orders_with_exchange_filtered(Some(
            normalized_strategy_id.as_str(),
        ))
        .await
    }

    pub async fn reconcile_evcurve_pending_orders_ws_fallback(&self) -> Result<()> {
        self.reconcile_active_pending_orders_for_strategy(STRATEGY_ID_EVCURVE_V1)
            .await
    }

    async fn reconcile_active_pending_orders_with_exchange_filtered(
        &self,
        strategy_filter: Option<&str>,
    ) -> Result<()> {
        if self.simulation_mode {
            return Ok(());
        }
        let Some(db) = self.tracking_db.as_ref() else {
            return Ok(());
        };
        let mut reconcile_rows = db.list_reconcile_pending_orders_active()?;
        let retry_max_rows = Self::order_reconcile_retry_max_rows();
        if retry_max_rows > 0 {
            let mut retry_rows = db.list_reconcile_pending_orders_retry(
                retry_max_rows,
                Self::order_reconcile_retry_min_age_ms(),
                Self::order_reconcile_retry_max_period_age_sec(),
            )?;
            reconcile_rows.append(&mut retry_rows);
        }
        let mut batched_status_updates: Vec<(String, String)> = Vec::new();
        for row in reconcile_rows {
            if let Some(filter) = strategy_filter {
                if !row.strategy_id.eq_ignore_ascii_case(filter) {
                    continue;
                }
            }
            if !Self::is_exchange_order_id(&row.order_id) {
                batched_status_updates.push((row.order_id.clone(), "STALE".to_string()));
                continue;
            }
            let order = match self.api.get_order(&row.order_id).await {
                Ok(v) => v,
                Err(e) => {
                    let err_text = e.to_string();
                    if Self::order_lookup_error_is_terminal(err_text.as_str()) {
                        debug!(
                            "Order reconcile terminal-miss {} status={} err={}",
                            row.order_id, row.status, err_text
                        );
                        batched_status_updates.push((row.order_id.clone(), "STALE".to_string()));
                        self.remove_pending_order_tracking(&row.order_id);
                        let mut pending = self.pending_trades.lock().await;
                        pending.remove(&row.trade_key);
                    } else {
                        debug!(
                            "Order reconcile transient fetch miss {} status={} err={}",
                            row.order_id, row.status, err_text
                        );
                        batched_status_updates
                            .push((row.order_id.clone(), "RECONCILE_FAILED".to_string()));
                    }
                    continue;
                }
            };
            let matched_fill = Self::matched_fill_from_order(&order);
            let terminal = Self::terminal_status_for_order(order.status.clone());
            if let Some(status) = terminal {
                if status == "FILLED" || (status == "CANCELED" && matched_fill.is_some()) {
                    let trade_snapshot = {
                        let mut pending = self.pending_trades.lock().await;
                        if let Some(trade) = pending.get_mut(&row.trade_key) {
                            trade.buy_order_confirmed = true;
                            Some(trade.clone())
                        } else {
                            None
                        }
                    };
                    let (matched_price, matched_units, matched_notional) =
                        matched_fill.unwrap_or((0.0, 0.0, 0.0));
                    let has_matched = matched_price > 0.0 && matched_units > 0.0;
                    self.on_order_filled(FillTransition {
                        order_id: Some(row.order_id.clone()),
                        trade_key: Some(row.trade_key.clone()),
                        strategy_id: Some(row.strategy_id.clone()),
                        period_timestamp: row.period_timestamp,
                        timeframe: Some(row.timeframe.clone()),
                        condition_id: trade_snapshot.as_ref().map(|t| t.condition_id.clone()),
                        token_id: row.token_id.clone(),
                        token_type: trade_snapshot
                            .as_ref()
                            .map(|t| t.token_type.display_name().to_string()),
                        side: Some(row.side.clone()),
                        price: if has_matched {
                            Some(matched_price)
                        } else {
                            Some(row.price)
                        },
                        units: if has_matched {
                            Some(matched_units)
                        } else {
                            trade_snapshot.as_ref().map(|t| t.units)
                        },
                        notional_usd: if has_matched {
                            Some(matched_notional)
                        } else {
                            Some(row.size_usd)
                        },
                        reason: if status == "FILLED" {
                            "exchange_reconcile_terminal_filled".to_string()
                        } else {
                            "exchange_reconcile_canceled_partial_fill".to_string()
                        },
                        reconciled: true,
                    });
                } else {
                    batched_status_updates.push((row.order_id.clone(), status.to_string()));
                    self.remove_pending_order_tracking(&row.order_id);
                    let mut pending = self.pending_trades.lock().await;
                    pending.remove(&row.trade_key);
                }
            } else if row.status.eq_ignore_ascii_case("STALE")
                || row.status.eq_ignore_ascii_case("RECONCILE_FAILED")
            {
                batched_status_updates.push((row.order_id.clone(), "OPEN".to_string()));
            }
        }
        self.flush_pending_order_status_updates(&mut batched_status_updates);
        Ok(())
    }

    fn upsert_pending_order_tracking(
        &self,
        order_id: &str,
        trade_key: &str,
        token_id: &str,
        source_timeframe: &str,
        entry_mode: &str,
        asset_symbol: Option<&str>,
        period_timestamp: u64,
        price: f64,
        size_usd: f64,
        side: &str,
        status: &str,
    ) -> Result<()> {
        self.upsert_pending_order_tracking_for_strategy(
            order_id,
            trade_key,
            token_id,
            source_timeframe,
            entry_mode,
            None,
            asset_symbol,
            period_timestamp,
            price,
            size_usd,
            side,
            status,
            None,
        )
    }

    fn upsert_pending_order_tracking_for_strategy(
        &self,
        order_id: &str,
        trade_key: &str,
        token_id: &str,
        source_timeframe: &str,
        entry_mode: &str,
        strategy_id: Option<&str>,
        asset_symbol: Option<&str>,
        period_timestamp: u64,
        price: f64,
        size_usd: f64,
        side: &str,
        status: &str,
        condition_id: Option<&str>,
    ) -> Result<()> {
        let Some(db) = self.tracking_db.as_ref() else {
            return Ok(());
        };
        let row = PendingOrderRecord {
            order_id: order_id.to_string(),
            trade_key: trade_key.to_string(),
            token_id: token_id.to_string(),
            condition_id: condition_id
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string),
            timeframe: Self::normalize_timeframe_label(Some(source_timeframe)),
            strategy_id: Self::normalize_strategy_id(strategy_id),
            asset_symbol: asset_symbol.map(|v| v.to_string()),
            entry_mode: entry_mode.to_string(),
            period_timestamp,
            price,
            size_usd,
            side: side.to_string(),
            status: status.to_string(),
        };
        db.upsert_pending_order(&row)
            .map_err(|e| anyhow!("failed to upsert pending order {}: {}", order_id, e))
    }

    fn update_pending_order_status(&self, order_id: &str, status: &str) {
        let Some(db) = self.tracking_db.as_ref() else {
            return;
        };
        if let Err(e) = db.update_pending_order_status(order_id, status) {
            warn!(
                "Failed to update pending order status {} -> {}: {}",
                order_id, status, e
            );
        }
    }

    fn flush_pending_order_status_updates(&self, updates: &mut Vec<(String, String)>) {
        if updates.is_empty() {
            return;
        }
        let Some(db) = self.tracking_db.as_ref() else {
            updates.clear();
            return;
        };
        let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
        for (order_id, status) in updates.drain(..) {
            grouped.entry(status).or_default().push(order_id);
        }
        for (status, order_ids) in grouped {
            if let Err(batch_err) = db.update_pending_order_status_batch_any(&order_ids, &status) {
                warn!(
                    "Failed batch pending-order status update status={} count={} err={}; falling back to per-row updates",
                    status,
                    order_ids.len(),
                    batch_err
                );
                for order_id in order_ids {
                    if let Err(single_err) = db.update_pending_order_status(&order_id, &status) {
                        warn!(
                            "Failed to update pending order status {} -> {}: {}",
                            order_id, status, single_err
                        );
                    }
                }
            }
        }
    }

    fn remove_pending_order_tracking(&self, order_id: &str) {
        // Soft-delete model: keep rows for auditability and status reconciliation.
        // Callers should set terminal status (FILLED/CANCELED/STALE) before invoking this.
        debug!("Pending order {} marked terminal (row retained)", order_id);
    }

    pub async fn reconcile_tracked_pending_orders_on_startup(&self) -> Result<()> {
        if self.simulation_mode {
            return Ok(());
        }
        let Some(db) = self.tracking_db.as_ref() else {
            return Ok(());
        };

        let tracked = db.list_reconcile_pending_orders()?;
        if tracked.is_empty() {
            return Ok(());
        }
        let mut batched_status_updates: Vec<(String, String)> = Vec::new();

        let total = tracked.len();
        let order_ids: Vec<&str> = tracked
            .iter()
            .filter(|row| Self::is_exchange_order_id(&row.order_id))
            .map(|row| row.order_id.as_str())
            .collect();
        let mut canceled_set: HashSet<String> = HashSet::new();
        if !order_ids.is_empty() {
            match self.api.cancel_orders(&order_ids).await {
                Ok(resp) => {
                    for oid in resp.canceled {
                        canceled_set.insert(oid);
                    }
                }
                Err(e) => {
                    warn!("Startup reconcile: cancel_orders failed: {}", e);
                    for row in tracked
                        .iter()
                        .filter(|row| Self::is_exchange_order_id(&row.order_id))
                    {
                        batched_status_updates
                            .push((row.order_id.clone(), "RECONCILE_FAILED".to_string()));
                    }
                    self.flush_pending_order_status_updates(&mut batched_status_updates);
                    return Ok(());
                }
            }
        }

        let mut canceled = 0usize;
        let mut stale = 0usize;
        let mut filled = 0usize;
        let mut removed = 0usize;
        for row in tracked {
            let mut matched_fill: Option<(f64, f64, f64)> = None;
            let final_status;
            if !Self::is_exchange_order_id(&row.order_id) {
                final_status = "STALE";
            } else {
                match self.api.get_order(&row.order_id).await {
                    Ok(order) => {
                        matched_fill = Self::matched_fill_from_order(&order);
                        if let Some(status) = Self::terminal_status_for_order(order.status) {
                            final_status = status;
                        } else if canceled_set.contains(&row.order_id) {
                            final_status = "CANCELED";
                        } else {
                            final_status = "STALE";
                        }
                    }
                    Err(e) => {
                        let err_text = e.to_string();
                        debug!(
                            "Startup reconcile order lookup miss {} err={}",
                            row.order_id, err_text
                        );
                        if canceled_set.contains(&row.order_id) {
                            final_status = "CANCELED";
                        } else if Self::order_lookup_error_is_terminal(err_text.as_str()) {
                            final_status = "STALE";
                        } else {
                            final_status = "RECONCILE_FAILED";
                        }
                    }
                }
            }

            let was_canceled = final_status == "CANCELED" || canceled_set.contains(&row.order_id);
            let mark_filled =
                final_status == "FILLED" || (final_status == "CANCELED" && matched_fill.is_some());
            if mark_filled {
                filled += 1;
            } else if final_status == "CANCELED" {
                canceled += 1;
            } else {
                stale += 1;
            }
            if mark_filled {
                let (matched_price, matched_units, matched_notional) =
                    matched_fill.unwrap_or((0.0, 0.0, 0.0));
                let has_matched = matched_price > 0.0 && matched_units > 0.0;
                self.on_order_filled(FillTransition {
                    order_id: Some(row.order_id.clone()),
                    trade_key: Some(row.trade_key.clone()),
                    strategy_id: Some(row.strategy_id.clone()),
                    period_timestamp: row.period_timestamp,
                    timeframe: Some(row.timeframe.clone()),
                    condition_id: row.condition_id.clone(),
                    token_id: row.token_id.clone(),
                    token_type: None,
                    side: Some(row.side.clone()),
                    price: if has_matched {
                        Some(matched_price)
                    } else {
                        Some(row.price)
                    },
                    units: if has_matched {
                        Some(matched_units)
                    } else if row.price > 0.0 {
                        Some(row.size_usd / row.price)
                    } else {
                        None
                    },
                    notional_usd: if has_matched {
                        Some(matched_notional)
                    } else {
                        Some(row.size_usd)
                    },
                    reason: if final_status == "FILLED" {
                        "startup_pending_reconcile_terminal_filled".to_string()
                    } else {
                        "startup_pending_reconcile_canceled_partial_fill".to_string()
                    },
                    reconciled: true,
                });
            } else {
                batched_status_updates.push((row.order_id.clone(), final_status.to_string()));
                self.remove_pending_order_tracking(&row.order_id);
                removed += 1;
            }

            self.record_trade_event(
                Some(format!(
                    "startup_pending_reconcile:{}:{}",
                    row.period_timestamp, row.order_id
                )),
                row.period_timestamp,
                Some(row.timeframe.as_str()),
                None,
                Some(row.token_id.clone()),
                None,
                Some(row.side.clone()),
                "STARTUP_PENDING_RECONCILE",
                Some(row.price),
                None,
                Some(row.size_usd),
                None,
                Some(if was_canceled {
                    "startup_cancelled_tracked_open_order".to_string()
                } else {
                    format!(
                        "startup_pruned_{}_tracked_open_order",
                        final_status.to_ascii_lowercase()
                    )
                }),
            );
        }
        self.flush_pending_order_status_updates(&mut batched_status_updates);

        crate::log_println!(
            "🧹 Startup pending-order reconcile: tracked={} canceled={} filled={} stale={} removed={}",
            total,
            canceled,
            filled,
            stale,
            removed
        );
        Ok(())
    }

    async fn apply_cancel_candidates(
        &self,
        cancel_candidates: Vec<(String, String)>,
        reconcile_reason: &'static str,
    ) -> Result<()> {
        if cancel_candidates.is_empty() {
            return Ok(());
        }

        let unique_exchange_order_ids: Vec<String> = cancel_candidates
            .iter()
            .filter(|(_, oid)| Self::is_exchange_order_id(oid))
            .map(|(_, oid)| oid.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let mut canceled_set: HashSet<String> = HashSet::new();
        if !unique_exchange_order_ids.is_empty() {
            let order_ids: Vec<&str> = unique_exchange_order_ids
                .iter()
                .map(|oid| oid.as_str())
                .collect();
            let response = self.api.cancel_orders(&order_ids).await?;
            canceled_set.extend(response.canceled);
        }

        if canceled_set.is_empty() && !unique_exchange_order_ids.is_empty() {
            debug!(
                "Ladder cancel returned no confirmations for {} candidate order(s)",
                cancel_candidates.len()
            );
        }

        let mut status_updates: Vec<(
            String,
            String,
            &'static str,
            bool,
            bool,
            Option<(f64, f64, f64)>,
        )> = Vec::with_capacity(cancel_candidates.len());
        let status_reconcile_parallel =
            Self::env_bool_named("EVPOLY_PARALLEL_CANCEL_RECONCILE_ENABLE", true);
        let status_reconcile_concurrency = std::env::var("EVPOLY_CANCEL_RECONCILE_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(8)
            .max(1);
        let status_reconcile_concurrency = if status_reconcile_parallel {
            status_reconcile_concurrency
        } else {
            1
        };
        let mut join_set = tokio::task::JoinSet::new();
        for (key, order_id) in cancel_candidates {
            if !Self::is_exchange_order_id(&order_id) {
                status_updates.push((key, order_id, "STALE", true, false, None));
                continue;
            }
            while join_set.len() >= status_reconcile_concurrency {
                if let Some(joined) = join_set.join_next().await {
                    match joined {
                        Ok(update) => status_updates.push(update),
                        Err(e) => warn!(
                            "{}: cancel reconcile task join failed: {}",
                            reconcile_reason, e
                        ),
                    }
                }
            }
            let api = self.api.clone();
            let canceled = canceled_set.contains(&order_id);
            join_set.spawn(async move {
                let mut matched_fill = None;
                let status = match api.get_order(&order_id).await {
                    Ok(order) => {
                        matched_fill = Trader::matched_fill_from_order(&order);
                        if let Some(status) = Trader::terminal_status_for_order(order.status) {
                            status
                        } else if canceled {
                            // Cancel acknowledgement is not terminal. Keep it OPEN until exchange
                            // confirms FILLED/CANCELED/UNMATCHED to avoid false close + overfill.
                            "OPEN"
                        } else {
                            "OPEN"
                        }
                    }
                    Err(_) => {
                        if canceled {
                            // Conservative on status fetch failure after cancel request.
                            "OPEN"
                        } else {
                            "RECONCILE_FAILED"
                        }
                    }
                };
                let remove = matches!(status, "CANCELED" | "STALE");
                let mark_filled =
                    status == "FILLED" || (status == "CANCELED" && matched_fill.is_some());
                (key, order_id, status, remove, mark_filled, matched_fill)
            });
        }
        while let Some(joined) = join_set.join_next().await {
            match joined {
                Ok(update) => status_updates.push(update),
                Err(e) => warn!(
                    "{}: cancel reconcile task join failed: {}",
                    reconcile_reason, e
                ),
            }
        }

        let mut fill_transitions = Vec::new();
        let mut pending = self.pending_trades.lock().await;
        for (key, order_id, status, remove, mark_filled, matched_fill) in status_updates {
            if mark_filled {
                let trade_snapshot = if let Some(trade) = pending.get_mut(&key) {
                    trade.buy_order_confirmed = true;
                    Some(trade.clone())
                } else {
                    None
                };
                let (matched_price, matched_units, matched_notional) =
                    matched_fill.unwrap_or((0.0, 0.0, 0.0));
                let has_matched = matched_price > 0.0 && matched_units > 0.0;
                fill_transitions.push(FillTransition {
                    order_id: Some(order_id.clone()),
                    trade_key: Some(key.clone()),
                    strategy_id: Some(
                        trade_snapshot
                            .as_ref()
                            .map(|t| t.strategy_id.clone())
                            .unwrap_or_else(crate::strategy::default_strategy_id),
                    ),
                    period_timestamp: trade_snapshot
                        .as_ref()
                        .map(|t| t.market_timestamp)
                        .unwrap_or(0),
                    timeframe: trade_snapshot.as_ref().map(|t| t.source_timeframe.clone()),
                    condition_id: trade_snapshot.as_ref().map(|t| t.condition_id.clone()),
                    token_id: trade_snapshot
                        .as_ref()
                        .map(|t| t.token_id.clone())
                        .unwrap_or_default(),
                    token_type: trade_snapshot
                        .as_ref()
                        .map(|t| t.token_type.display_name().to_string()),
                    side: Some("BUY".to_string()),
                    price: if has_matched {
                        Some(matched_price)
                    } else {
                        trade_snapshot.as_ref().map(|t| t.purchase_price)
                    },
                    units: if has_matched {
                        Some(matched_units)
                    } else {
                        trade_snapshot.as_ref().map(|t| t.units)
                    },
                    notional_usd: if has_matched {
                        Some(matched_notional)
                    } else {
                        trade_snapshot.as_ref().map(|t| t.units * t.purchase_price)
                    },
                    reason: if status == "FILLED" {
                        reconcile_reason.to_string()
                    } else {
                        format!("{}_partial_canceled_fill", reconcile_reason)
                    },
                    reconciled: true,
                });
            }
            if remove && pending.remove(&key).is_some() {
                self.remove_pending_order_tracking(&order_id);
            }
            if !mark_filled {
                self.update_pending_order_status(&order_id, status);
            }
        }
        drop(pending);
        for fill in fill_transitions {
            self.on_order_filled(fill);
        }
        Ok(())
    }

    async fn cancel_db_only_pending_rows(
        &self,
        rows: Vec<PendingOrderRecord>,
        reconcile_reason: &'static str,
    ) -> Result<usize> {
        if rows.is_empty() {
            return Ok(0);
        }

        let unique_exchange_order_ids: Vec<String> = rows
            .iter()
            .filter(|row| Self::is_exchange_order_id(row.order_id.as_str()))
            .map(|row| row.order_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let mut canceled_set: HashSet<String> = HashSet::new();
        if !unique_exchange_order_ids.is_empty() {
            let order_ids: Vec<&str> = unique_exchange_order_ids
                .iter()
                .map(|oid| oid.as_str())
                .collect();
            let response = self.api.cancel_orders(&order_ids).await?;
            canceled_set.extend(response.canceled);
        }

        for row in rows {
            if !Self::is_exchange_order_id(row.order_id.as_str()) {
                self.update_pending_order_status(row.order_id.as_str(), "STALE");
                continue;
            }

            let mut matched_fill = None;
            let status = match self.api.get_order(row.order_id.as_str()).await {
                Ok(order) => {
                    matched_fill = Self::matched_fill_from_order(&order);
                    if let Some(status) = Self::terminal_status_for_order(order.status) {
                        status
                    } else {
                        "OPEN"
                    }
                }
                Err(e) => {
                    debug!(
                        "{}: order status lookup miss for db-only order {}: {}",
                        reconcile_reason, row.order_id, e
                    );
                    if canceled_set.contains(row.order_id.as_str()) {
                        "OPEN"
                    } else {
                        "RECONCILE_FAILED"
                    }
                }
            };

            let mark_filled =
                status == "FILLED" || (status == "CANCELED" && matched_fill.is_some());
            if mark_filled {
                let (matched_price, matched_units, matched_notional) =
                    matched_fill.unwrap_or((0.0, 0.0, 0.0));
                let has_matched = matched_price > 0.0 && matched_units > 0.0;
                self.on_order_filled(FillTransition {
                    order_id: Some(row.order_id.clone()),
                    trade_key: Some(row.trade_key.clone()),
                    strategy_id: Some(row.strategy_id.clone()),
                    period_timestamp: row.period_timestamp,
                    timeframe: Some(row.timeframe.clone()),
                    condition_id: row.condition_id.clone(),
                    token_id: row.token_id.clone(),
                    token_type: None,
                    side: Some(row.side.clone()),
                    price: if has_matched {
                        Some(matched_price)
                    } else {
                        Some(row.price)
                    },
                    units: if has_matched {
                        Some(matched_units)
                    } else if row.price > 0.0 {
                        Some(row.size_usd / row.price)
                    } else {
                        None
                    },
                    notional_usd: if has_matched {
                        Some(matched_notional)
                    } else {
                        Some(row.size_usd)
                    },
                    reason: format!("{}_db_only_reconcile", reconcile_reason),
                    reconciled: true,
                });
            } else {
                self.update_pending_order_status(row.order_id.as_str(), status);
            }
        }

        Ok(unique_exchange_order_ids.len())
    }

    async fn cancel_expired_ladder_orders(&self) -> Result<()> {
        if self.simulation_mode {
            return Ok(());
        }

        let now_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let cancel_candidates: Vec<(String, String)> = {
            let pending = self.pending_trades.lock().await;
            pending
                .iter()
                .filter(|(key, trade)| {
                    EntryExecutionMode::parse(trade.entry_mode.as_str()).is_ladder()
                        && key.contains("_ladder_")
                        && !trade.sold
                        && !trade.buy_order_confirmed
                        && !trade.redemption_abandoned
                })
                .filter_map(|(key, trade)| {
                    let order_id = trade.order_id.as_ref()?;
                    if now_ts
                        >= Self::ladder_cancel_deadline_ts(
                            trade.market_timestamp,
                            trade.source_timeframe.as_str(),
                        )
                    {
                        Some((key.clone(), order_id.clone()))
                    } else {
                        None
                    }
                })
                .collect()
        };

        self.apply_cancel_candidates(cancel_candidates, "ladder_expiry_cancel_reconcile")
            .await
    }

    async fn cancel_expired_endgame_orders(&self) -> Result<()> {
        if self.simulation_mode {
            return Ok(());
        }

        let now = std::time::Instant::now();
        let endgame_cancel_after_ms = Self::endgame_cancel_after_ms();
        let evcurve_cancel_after_ms = Self::evcurve_cancel_after_ms();

        let cancel_candidates: Vec<(String, String)> = {
            let pending = self.pending_trades.lock().await;
            pending
                .iter()
                .filter(|(key, trade)| {
                    let mode = EntryExecutionMode::parse(trade.entry_mode.as_str());
                    (mode.is_endgame() && key.contains("_endgame_"))
                        || (mode == EntryExecutionMode::Evcurve && key.contains("_evcurve_"))
                })
                .filter(|(_, trade)| {
                    !trade.sold
                        && !trade.buy_order_confirmed
                        && !trade.redemption_abandoned
                        && trade.order_id.as_ref().is_some()
                })
                .filter_map(|(key, trade)| {
                    let order_id = trade.order_id.as_ref()?;
                    let mode = EntryExecutionMode::parse(trade.entry_mode.as_str());
                    let cancel_after_ms = if mode == EntryExecutionMode::Evcurve {
                        evcurve_cancel_after_ms
                    } else {
                        endgame_cancel_after_ms
                    };
                    if Self::endgame_order_cancel_due(trade, now, cancel_after_ms) {
                        Some((key.clone(), order_id.clone()))
                    } else {
                        None
                    }
                })
                .collect()
        };

        self.apply_cancel_candidates(cancel_candidates, "endgame_ttl_cancel_reconcile")
            .await
    }

    async fn cancel_expired_mm_rewards_orders(&self) -> Result<()> {
        if self.simulation_mode {
            return Ok(());
        }

        let now = std::time::Instant::now();
        let cancel_after_ms = Self::mm_rewards_cancel_after_ms();

        let cancel_candidates: Vec<(String, String)> = {
            let pending = self.pending_trades.lock().await;
            pending
                .iter()
                .filter(|(key, trade)| {
                    EntryExecutionMode::parse(trade.entry_mode.as_str())
                        == EntryExecutionMode::MmRewards
                        && key.contains("_mm_rewards_")
                })
                .filter(|(_, trade)| {
                    !trade.sold
                        && !trade.buy_order_confirmed
                        && !trade.redemption_abandoned
                        && trade.order_id.as_ref().is_some()
                })
                .filter_map(|(key, trade)| {
                    let order_id = trade.order_id.as_ref()?;
                    if Self::endgame_order_cancel_due(trade, now, cancel_after_ms) {
                        Some((key.clone(), order_id.clone()))
                    } else {
                        None
                    }
                })
                .collect()
        };

        self.apply_cancel_candidates(cancel_candidates, "mm_rewards_ttl_cancel_reconcile")
            .await
    }

    pub async fn run_ladder_cancel_maintenance(&self) -> Result<()> {
        if !Self::env_bool_named("EVPOLY_PARALLEL_CANCEL_MAINTENANCE_ENABLE", true) {
            self.cancel_expired_ladder_orders().await?;
            self.cancel_expired_endgame_orders().await?;
            self.cancel_expired_mm_rewards_orders().await?;
            return Ok(());
        }
        let (ladder_res, endgame_res, mm_res) = tokio::join!(
            self.cancel_expired_ladder_orders(),
            self.cancel_expired_endgame_orders(),
            self.cancel_expired_mm_rewards_orders(),
        );
        ladder_res?;
        endgame_res?;
        mm_res?;
        Ok(())
    }

    fn record_trade_event(
        &self,
        event_key: Option<String>,
        period_timestamp: u64,
        timeframe: Option<&str>,
        condition_id: Option<String>,
        token_id: Option<String>,
        token_type: Option<String>,
        side: Option<String>,
        event_type: &str,
        price: Option<f64>,
        units: Option<f64>,
        notional_usd: Option<f64>,
        pnl_usd: Option<f64>,
        reason: Option<String>,
    ) {
        self.record_trade_event_for_strategy(
            None,
            event_key,
            period_timestamp,
            timeframe,
            condition_id,
            token_id,
            token_type,
            side,
            event_type,
            price,
            units,
            notional_usd,
            pnl_usd,
            reason,
        );
    }

    fn record_trade_event_for_strategy(
        &self,
        strategy_id: Option<&str>,
        event_key: Option<String>,
        period_timestamp: u64,
        timeframe: Option<&str>,
        condition_id: Option<String>,
        token_id: Option<String>,
        token_type: Option<String>,
        side: Option<String>,
        event_type: &str,
        price: Option<f64>,
        units: Option<f64>,
        notional_usd: Option<f64>,
        pnl_usd: Option<f64>,
        reason: Option<String>,
    ) {
        let Some(db) = self.tracking_db.as_ref() else {
            return;
        };

        let asset_symbol = Self::asset_symbol_from_reason_request_id(reason.as_deref())
            .or_else(|| Self::asset_symbol_from_token_type_label(token_type.as_deref()));
        let row = TradeEventRecord {
            event_key,
            ts_ms: chrono::Utc::now().timestamp_millis(),
            period_timestamp,
            timeframe: Self::normalize_timeframe_label(timeframe),
            strategy_id: Self::normalize_strategy_id(strategy_id),
            asset_symbol,
            condition_id,
            token_id,
            token_type,
            side,
            event_type: event_type.to_string(),
            price,
            units,
            notional_usd,
            pnl_usd,
            reason,
        };

        if let Err(e) = db.record_trade_event(&row) {
            let err_txt = e.to_string();
            // Startup reconcile is intentionally idempotent across restarts; duplicate
            // event keys are expected and should not pollute runtime warning logs.
            if event_type == "STARTUP_PENDING_RECONCILE"
                && err_txt.contains("duplicate trade_event ignored")
            {
                return;
            }
            if event_type == "RISK_REJECT" && err_txt.contains("duplicate trade_event ignored") {
                return;
            }
            warn!("Failed to record trade event {}: {}", event_type, err_txt);
            return;
        }
    }

    fn record_trade_event_required_strategy(
        &self,
        strategy_id: &str,
        event_key: Option<String>,
        period_timestamp: u64,
        timeframe: Option<&str>,
        condition_id: Option<String>,
        token_id: Option<String>,
        token_type: Option<String>,
        side: Option<String>,
        event_type: &str,
        price: Option<f64>,
        units: Option<f64>,
        notional_usd: Option<f64>,
        pnl_usd: Option<f64>,
        reason: Option<String>,
    ) {
        if strategy_id.trim().is_empty() {
            warn!(
                "Missing required strategy_id for event_type={} period={} token={}",
                event_type,
                period_timestamp,
                token_id.as_deref().unwrap_or("<none>")
            );
            return;
        }
        self.record_trade_event_for_strategy(
            Some(strategy_id),
            event_key,
            period_timestamp,
            timeframe,
            condition_id,
            token_id,
            token_type,
            side,
            event_type,
            price,
            units,
            notional_usd,
            pnl_usd,
            reason,
        );
    }

    fn update_redemption_tracking_state(
        &self,
        trade: &PendingTrade,
        redemption_status: &str,
        redemption_tx_id: Option<&str>,
        redeemed_notional_usd: Option<f64>,
        verification_detail: Option<&str>,
    ) {
        let rate_limit_pause_sec =
            verification_detail.and_then(Self::parse_rate_limit_pause_seconds);
        let entry = json!({
            "ts_ms": chrono::Utc::now().timestamp_millis(),
            "strategy_id": trade.strategy_id,
            "timeframe": trade.source_timeframe,
            "period_timestamp": trade.market_timestamp,
            "condition_id": trade.condition_id,
            "token_id": trade.token_id,
            "token_type": trade.token_type.display_name(),
            "redemption_status": redemption_status,
            "redemption_tx_id": redemption_tx_id,
            "redemption_attempts": trade.redemption_attempts,
            "redeemed_notional_usd": redeemed_notional_usd,
            "verification_detail": verification_detail,
            "rate_limit_pause_sec": rate_limit_pause_sec
        });
        Self::push_redemption_attempt_log(entry.clone());
        log_event("redemption_attempt_status", entry);

        let Some(db) = self.tracking_db.as_ref() else {
            return;
        };
        if let Err(e) = db.update_trade_lifecycle_redemption_state(
            trade.strategy_id.as_str(),
            trade.source_timeframe.as_str(),
            trade.market_timestamp,
            Some(trade.condition_id.as_str()),
            trade.token_id.as_str(),
            redemption_status,
            redemption_tx_id,
            Some(trade.redemption_attempts),
            redeemed_notional_usd,
            verification_detail,
        ) {
            warn!(
                "Failed lifecycle redemption update | strategy={} timeframe={} period={} token={} status={}: {}",
                trade.strategy_id,
                trade.source_timeframe,
                trade.market_timestamp,
                trade.token_id,
                redemption_status,
                e
            );
        }
    }

    fn record_redemption_cashflow(
        &self,
        trade: &PendingTrade,
        event_type: &str,
        amount_usd: Option<f64>,
        units: Option<f64>,
        token_price: Option<f64>,
        tx_id: Option<&str>,
        reason: Option<&str>,
    ) {
        let Some(db) = self.tracking_db.as_ref() else {
            return;
        };
        let tx_component = tx_id
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("none");
        let cashflow_key = format!(
            "cashflow:{}:{}:{}:{}:{}:{}",
            trade.strategy_id,
            trade.source_timeframe,
            trade.market_timestamp,
            trade.token_id,
            event_type.trim().to_ascii_lowercase(),
            tx_component
        );
        let row = WalletCashflowEventRecord {
            cashflow_key,
            ts_ms: chrono::Utc::now().timestamp_millis(),
            strategy_id: trade.strategy_id.clone(),
            timeframe: trade.source_timeframe.clone(),
            period_timestamp: trade.market_timestamp,
            condition_id: Some(trade.condition_id.clone()),
            token_id: Some(trade.token_id.clone()),
            event_type: event_type.to_string(),
            amount_usd,
            units,
            token_price,
            tx_id: tx_id.map(|v| v.to_string()),
            reason: reason.map(|v| v.to_string()),
            source: Some("redemption_sweep".to_string()),
        };
        if let Err(e) = db.record_wallet_cashflow_event(&row) {
            warn!(
                "Failed wallet cashflow insert | strategy={} timeframe={} period={} token={} type={}: {}",
                trade.strategy_id,
                trade.source_timeframe,
                trade.market_timestamp,
                trade.token_id,
                event_type,
                e
            );
        }
    }

    fn record_merge_cashflow(
        &self,
        strategy_id: &str,
        timeframe: &str,
        period_timestamp: u64,
        condition_id: &str,
        pair_shares: f64,
        tx_id: Option<&str>,
        reason: Option<&str>,
        source: &str,
    ) {
        let Some(db) = self.tracking_db.as_ref() else {
            return;
        };
        if pair_shares <= 0.0 || !pair_shares.is_finite() {
            return;
        }
        let tx_component = tx_id
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("none");
        let cashflow_key = format!(
            "cashflow:{}:{}:{}:{}:{}",
            strategy_id.trim().to_ascii_lowercase(),
            condition_id.trim(),
            "merge_success",
            tx_component,
            period_timestamp
        );
        let row = WalletCashflowEventRecord {
            cashflow_key,
            ts_ms: chrono::Utc::now().timestamp_millis(),
            strategy_id: strategy_id.trim().to_ascii_lowercase(),
            timeframe: timeframe.trim().to_ascii_lowercase(),
            period_timestamp,
            condition_id: Some(condition_id.trim().to_string()),
            token_id: None,
            event_type: "MERGE_SUCCESS".to_string(),
            amount_usd: Some(pair_shares),
            units: Some(pair_shares),
            token_price: Some(1.0),
            tx_id: tx_id.map(|v| v.to_string()),
            reason: reason.map(|v| v.to_string()),
            source: Some(source.trim().to_string()),
        };
        if let Err(e) = db.record_wallet_cashflow_event(&row) {
            warn!(
                "Failed merge wallet cashflow insert | strategy={} timeframe={} period={} condition={}: {}",
                strategy_id,
                timeframe,
                period_timestamp,
                condition_id,
                e
            );
        }
    }

    fn consume_mm_merge_inventory(
        &self,
        strategy_id: &str,
        timeframe: &str,
        period_timestamp: u64,
        condition_id: &str,
        up_token_id: &str,
        down_token_id: &str,
        pair_shares: f64,
        tx_id: Option<&str>,
        reason: Option<&str>,
        source: &str,
    ) {
        let Some(db) = self.tracking_db.as_ref() else {
            return;
        };
        if pair_shares <= 0.0 || !pair_shares.is_finite() {
            return;
        }
        match db.consume_mm_merge_inventory(
            strategy_id,
            timeframe,
            period_timestamp,
            condition_id,
            up_token_id,
            down_token_id,
            pair_shares,
            tx_id,
            reason,
            source,
        ) {
            Ok(results) => {
                log_event(
                    "mm_inventory_merge_consumed",
                    json!({
                        "strategy_id": strategy_id.trim().to_ascii_lowercase(),
                        "timeframe": timeframe.trim().to_ascii_lowercase(),
                        "period_timestamp": period_timestamp,
                        "condition_id": condition_id,
                        "up_token_id": up_token_id,
                        "down_token_id": down_token_id,
                        "pair_shares": pair_shares,
                        "tx_id": tx_id,
                        "source": source,
                        "results": results
                            .into_iter()
                            .map(|row| {
                                json!({
                                    "consume_key": row.consume_key,
                                    "token_id": row.token_id,
                                    "requested_units": row.requested_units,
                                    "applied_units": row.applied_units,
                                    "applied_cost_usd": row.applied_cost_usd,
                                    "db_units_before": row.db_units_before,
                                    "db_units_after": row.db_units_after
                                })
                            })
                            .collect::<Vec<_>>()
                    }),
                );
            }
            Err(e) => {
                warn!(
                    "Failed MM merge inventory consume | strategy={} timeframe={} period={} condition={} tx={:?}: {}",
                    strategy_id,
                    timeframe,
                    period_timestamp,
                    condition_id,
                    tx_id,
                    e
                );
            }
        }
    }

    fn build_entry_fill_event_key(
        order_id: Option<&str>,
        fill: &FillTransition,
        ts_ms: i64,
    ) -> String {
        if let Some(order_id) = order_id {
            return format!("entry_fill_order:{}", order_id.trim());
        }
        let strategy_id = Self::normalize_strategy_id(fill.strategy_id.as_deref());
        let timeframe = Self::normalize_timeframe_label(fill.timeframe.as_deref());
        format!(
            "entry_fill:{}:{}:{}:{}:{}",
            strategy_id, timeframe, fill.period_timestamp, fill.token_id, ts_ms
        )
    }

    fn build_exit_event_key(
        strategy_id: &str,
        timeframe: &str,
        period_timestamp: u64,
        token_id: &str,
        event_type: &str,
        reason: Option<&str>,
    ) -> String {
        let strategy = Self::normalize_strategy_id(Some(strategy_id));
        let tf = Self::normalize_timeframe_label(Some(timeframe));
        let event_kind = event_type.trim().to_ascii_uppercase();
        let reason_hint = reason
            .and_then(|v| v.split_whitespace().next())
            .map(|v| {
                v.chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                    .collect::<String>()
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "none".to_string());
        format!(
            "exit:{}:{}:{}:{}:{}:{}",
            strategy,
            tf,
            period_timestamp,
            token_id.trim(),
            event_kind,
            reason_hint
        )
    }

    fn on_order_filled(&self, mut fill: FillTransition) {
        let ts_ms = chrono::Utc::now().timestamp_millis();
        let mut strategy_id = Self::normalize_strategy_id(fill.strategy_id.as_deref());
        let mut timeframe = Self::normalize_timeframe_label(fill.timeframe.as_deref());
        let mut side = fill
            .side
            .clone()
            .unwrap_or_else(|| "BUY".to_string())
            .trim()
            .to_ascii_uppercase();
        if side.is_empty() {
            side = "BUY".to_string();
        }

        if let (Some(db), Some(order_id)) = (self.tracking_db.as_ref(), fill.order_id.as_deref()) {
            match db.get_pending_order_by_id(order_id) {
                Ok(Some(row)) => {
                    if strategy_id == crate::strategy::default_strategy_id()
                        && row.strategy_id != crate::strategy::default_strategy_id()
                    {
                        strategy_id = row.strategy_id;
                    }
                    if fill.period_timestamp == 0 {
                        fill.period_timestamp = row.period_timestamp;
                    }
                    if fill.token_id.trim().is_empty() {
                        fill.token_id = row.token_id.clone();
                    }
                    if fill
                        .condition_id
                        .as_deref()
                        .map(str::trim)
                        .unwrap_or("")
                        .is_empty()
                    {
                        fill.condition_id = row.condition_id.clone();
                    }
                    timeframe = Self::normalize_timeframe_label(Some(row.timeframe.as_str()));
                    if fill.notional_usd.unwrap_or(0.0) <= 0.0 && row.size_usd > 0.0 {
                        fill.notional_usd = Some(row.size_usd);
                    }
                    if fill.price.unwrap_or(0.0) <= 0.0 && row.price > 0.0 {
                        fill.price = Some(row.price);
                    }
                    if fill.side.is_none() {
                        side = row.side;
                    }
                    if fill.trade_key.is_none() {
                        fill.trade_key = Some(row.trade_key);
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(
                        "Failed to load pending order context for {}: {}",
                        order_id, e
                    );
                }
            }
        }

        if fill.notional_usd.unwrap_or(0.0) <= 0.0
            && fill.units.unwrap_or(0.0) > 0.0
            && fill.price.unwrap_or(0.0) > 0.0
        {
            fill.notional_usd = Some(fill.units.unwrap_or(0.0) * fill.price.unwrap_or(0.0));
        }
        if fill.units.unwrap_or(0.0) <= 0.0
            && fill.notional_usd.unwrap_or(0.0) > 0.0
            && fill.price.unwrap_or(0.0) > 0.0
        {
            fill.units = Some(fill.notional_usd.unwrap_or(0.0) / fill.price.unwrap_or(0.0));
        }

        let fill_source = if self.simulation_mode {
            if fill.reconciled {
                "simulation_reconcile"
            } else {
                "simulation_runtime"
            }
        } else if fill.reconciled {
            "exchange_reconcile"
        } else {
            "exchange_runtime"
        };

        let mut reason_json = json!({
            "mode": if fill.reconciled { "reconcile_fill_backfill" } else { "runtime_fill" },
            "fill_source": fill_source,
            "reason": fill.reason,
            "order_id": fill.order_id,
            "trade_key": fill.trade_key,
        });
        if let Some(price) = fill.price {
            reason_json["price"] = json!(price);
        }
        if let Some(units) = fill.units {
            reason_json["units"] = json!(units);
        }
        if let Some(notional) = fill.notional_usd {
            reason_json["notional_usd"] = json!(notional);
        }

        let order_id = fill.order_id.clone();
        let is_sell_fill = side.eq_ignore_ascii_case("SELL");
        let fill_event_type = if is_sell_fill { "EXIT" } else { "ENTRY_FILL" };
        let mut realized_pnl_usd = None;
        if is_sell_fill {
            if let (Some(db), Some(units), Some(price)) =
                (self.tracking_db.as_ref(), fill.units, fill.price)
            {
                if units > 0.0 && price > 0.0 {
                    if let Some(condition_id) = fill
                        .condition_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                    {
                        if let Ok(Some((avg_entry_price, _))) = db
                            .get_entry_fill_avg_price_for_strategy_condition_token(
                                strategy_id.as_str(),
                                condition_id,
                                fill.token_id.as_str(),
                            )
                        {
                            realized_pnl_usd = Some((price - avg_entry_price) * units);
                        }
                    }
                }
            }
        }
        let event_key = if is_sell_fill {
            if let Some(order_id) = order_id.as_deref() {
                format!("exit_fill_order:{}", order_id.trim())
            } else {
                Self::build_exit_event_key(
                    strategy_id.as_str(),
                    timeframe.as_str(),
                    fill.period_timestamp,
                    fill.token_id.as_str(),
                    "EXIT",
                    Some(fill.reason.as_str()),
                )
            }
        } else {
            Self::build_entry_fill_event_key(order_id.as_deref(), &fill, ts_ms)
        };
        let fill_row = TradeEventRecord {
            event_key: Some(event_key.clone()),
            ts_ms,
            period_timestamp: fill.period_timestamp,
            timeframe: timeframe.clone(),
            strategy_id: strategy_id.clone(),
            asset_symbol: Self::asset_symbol_from_token_type_label(fill.token_type.as_deref()),
            condition_id: fill.condition_id.clone(),
            token_id: Some(fill.token_id.clone()),
            token_type: fill.token_type.clone(),
            side: Some(side.clone()),
            event_type: fill_event_type.to_string(),
            price: fill.price,
            units: fill.units,
            notional_usd: fill.notional_usd,
            pnl_usd: realized_pnl_usd,
            reason: Some(reason_json.to_string()),
        };

        if let Some(db) = self.tracking_db.as_ref() {
            if let Some(order_id) = order_id.as_deref() {
                let already_recorded = match db.has_trade_event_key(event_key.as_str()) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(
                            "Failed to check existing fill event for {}: {}",
                            event_key, e
                        );
                        false
                    }
                };
                if already_recorded {
                    self.update_pending_order_status(order_id, "FILLED");
                } else if let Err(e) = db.mark_pending_order_filled_with_event(order_id, &fill_row)
                {
                    warn!(
                        "Failed to persist atomic fill transition for order {}: {}",
                        order_id, e
                    );
                }
                if fill.reconciled && !is_sell_fill {
                    let reconciled_key = format!("entry_fill_reconciled:{}", order_id);
                    let has_reconciled = db
                        .has_trade_event_key(reconciled_key.as_str())
                        .unwrap_or(false);
                    if !has_reconciled {
                        self.record_trade_event_for_strategy(
                            Some(strategy_id.as_str()),
                            Some(reconciled_key),
                            fill.period_timestamp,
                            Some(timeframe.as_str()),
                            fill.condition_id,
                            Some(fill.token_id),
                            fill.token_type,
                            Some(side),
                            "ENTRY_FILL_RECONCILED",
                            fill.price,
                            fill.units,
                            fill.notional_usd,
                            None,
                            Some(reason_json.to_string()),
                        );
                    }
                }
                self.remove_pending_order_tracking(order_id);
                return;
            }
        }

        self.record_trade_event_for_strategy(
            Some(strategy_id.as_str()),
            Some(event_key),
            fill.period_timestamp,
            Some(timeframe.as_str()),
            fill.condition_id,
            Some(fill.token_id),
            fill.token_type,
            Some(side),
            fill_event_type,
            fill.price,
            fill.units,
            fill.notional_usd,
            realized_pnl_usd,
            Some(reason_json.to_string()),
        );
    }

    fn adverse_flow_side(token_type: &TokenType) -> &'static str {
        match token_type {
            TokenType::BtcUp | TokenType::EthUp | TokenType::SolanaUp | TokenType::XrpUp => "SELL",
            TokenType::BtcDown
            | TokenType::EthDown
            | TokenType::SolanaDown
            | TokenType::XrpDown => "BUY",
        }
    }

    fn theta_force_flat_window(theta_pressure: f64) -> u64 {
        if theta_pressure >= 0.95 {
            120
        } else if theta_pressure >= 0.90 {
            90
        } else if theta_pressure >= 0.80 {
            60
        } else {
            45
        }
    }

    fn distribution_take_profit_multiple(theta_pressure: f64) -> f64 {
        if theta_pressure >= 0.95 {
            Self::env_f64("EVPOLY_DISTRIBUTION_TP_MULT_95", 0.99)
        } else if theta_pressure >= 0.90 {
            Self::env_f64("EVPOLY_DISTRIBUTION_TP_MULT_90", 1.03)
        } else {
            Self::env_f64("EVPOLY_DISTRIBUTION_TP_MULT_80", 1.08)
        }
    }

    async fn maybe_distribution_exit(
        &self,
        key: &str,
        trade: &mut PendingTrade,
        current_ask_price: f64,
        theta_pressure: f64,
        time_remaining: u64,
    ) -> bool {
        if !trade.buy_order_confirmed || trade.sold || trade.claim_on_closure {
            return false;
        }
        if theta_pressure < 0.80 {
            return false;
        }

        let take_profit_multiple =
            Self::distribution_take_profit_multiple(theta_pressure).max(0.90);
        let target_price = trade.purchase_price * take_profit_multiple;
        if current_ask_price + 1e-9 < target_price {
            return false;
        }

        let reason = format!(
            "distribution_take_profit (elapsed {:.2}, ask {:.6} >= target {:.6}, remaining {}s)",
            theta_pressure, current_ask_price, target_price, time_remaining
        );

        let trigger_event_type = "DISTRIBUTION_EXIT_TRIGGER";
        self.record_trade_event_required_strategy(
            trade.strategy_id.as_str(),
            Some(Self::build_exit_event_key(
                trade.strategy_id.as_str(),
                trade.source_timeframe.as_str(),
                trade.market_timestamp,
                trade.token_id.as_str(),
                trigger_event_type,
                Some(reason.as_str()),
            )),
            trade.market_timestamp,
            Some(trade.source_timeframe.as_str()),
            Some(trade.condition_id.clone()),
            Some(trade.token_id.clone()),
            Some(trade.token_type.display_name().to_string()),
            Some("SELL".to_string()),
            trigger_event_type,
            Some(current_ask_price),
            Some(trade.units),
            Some(current_ask_price * trade.units),
            None,
            Some(reason.clone()),
        );

        let closed = self
            .force_close_trade_now(key, trade, current_ask_price, &reason, false)
            .await;

        let result_event_type = if closed {
            "DISTRIBUTION_EXIT_FILLED"
        } else {
            "DISTRIBUTION_EXIT_SKIPPED"
        };
        self.record_trade_event_required_strategy(
            trade.strategy_id.as_str(),
            Some(Self::build_exit_event_key(
                trade.strategy_id.as_str(),
                trade.source_timeframe.as_str(),
                trade.market_timestamp,
                trade.token_id.as_str(),
                result_event_type,
                Some(reason.as_str()),
            )),
            trade.market_timestamp,
            Some(trade.source_timeframe.as_str()),
            Some(trade.condition_id.clone()),
            Some(trade.token_id.clone()),
            Some(trade.token_type.display_name().to_string()),
            Some("SELL".to_string()),
            result_event_type,
            Some(current_ask_price),
            Some(trade.units),
            Some(current_ask_price * trade.units),
            None,
            Some(reason),
        );
        closed
    }

    async fn resolve_units_to_sell(&self, trade: &PendingTrade) -> f64 {
        match self.fetch_token_balance_units(&trade.token_id).await {
            Ok(balance_f64) => {
                if balance_f64 > 0.0 && (trade.units == 0.0 || balance_f64 > trade.units) {
                    balance_f64
                } else {
                    trade.units
                }
            }
            Err(_) => trade.units,
        }
    }

    async fn fetch_token_balance_units(&self, token_id: &str) -> Result<f64> {
        let balance = self.api.check_balance_only(token_id).await?;
        let balance_decimal = balance / rust_decimal::Decimal::from(1_000_000u64);
        Ok(f64::try_from(balance_decimal).unwrap_or(0.0).max(0.0))
    }

    async fn reaction_exit_reason(&self, trade: &PendingTrade) -> Option<String> {
        let state = self.signal_state.as_ref()?;
        let guard = state.read().await;
        let adverse_side = Self::adverse_flow_side(&trade.token_type);
        let now_ms = chrono::Utc::now().timestamp_millis();

        let mut fast_hits = 0_u32;
        let mut confirm_hits = 0_u32;
        let mut max_whale = 0.0_f64;
        for event in guard.whale_events.iter().rev().take(64) {
            if event.side != adverse_side {
                continue;
            }
            let age_ms = now_ms - event.timestamp_ms;
            if age_ms < 0 || age_ms > 10_000 {
                continue;
            }
            confirm_hits += 1;
            if age_ms <= 3_000 {
                fast_hits += 1;
            }
            max_whale = max_whale.max(event.notional_usd);
        }

        let mut max_z = 0.0_f64;
        for event in guard.abnormal_events.iter().rev().take(64) {
            if event.side != adverse_side {
                continue;
            }
            let age_ms = now_ms - event.timestamp_ms;
            if age_ms < 0 || age_ms > 10_000 {
                continue;
            }
            confirm_hits += 1;
            if age_ms <= 3_000 {
                fast_hits += 1;
            }
            max_z = max_z.max(event.max_abs_z);
        }

        if max_whale >= 1_500_000.0 && fast_hits >= 1 {
            return Some(format!("adverse whale flow ${:.0} (3s)", max_whale));
        }
        if max_z >= 4.0 && confirm_hits >= 2 {
            return Some(format!(
                "adverse impulse follow-through (z={:.2}, hits={})",
                max_z, confirm_hits
            ));
        }
        None
    }

    async fn force_close_trade_now(
        &self,
        key: &str,
        trade: &mut PendingTrade,
        current_price: f64,
        reason: &str,
        is_stop_loss: bool,
    ) -> bool {
        let units_to_sell = self.resolve_units_to_sell(trade).await;
        if units_to_sell <= 0.0 {
            warn!(
                "Skipping force-close for {} ({}): balance is zero",
                trade.token_type.display_name(),
                reason
            );
            return false;
        }

        crate::log_println!("═══════════════════════════════════════════════════════════");
        crate::log_println!("🚨 FORCE EXIT OVERRIDE");
        crate::log_println!("═══════════════════════════════════════════════════════════");
        crate::log_println!("Reason: {}", reason);
        crate::log_println!(
            "Token: {} ({})",
            trade.token_type.display_name(),
            trade.token_id
        );
        crate::log_println!("Price: ${:.6}", current_price);
        crate::log_println!("Units: {:.6}", units_to_sell);

        match self
            .execute_sell(
                key,
                trade,
                units_to_sell,
                current_price,
                Some("FAK"),
                is_stop_loss,
            )
            .await
        {
            Ok(_) => {
                let remaining_units = self
                    .fetch_token_balance_units(&trade.token_id)
                    .await
                    .unwrap_or(0.0);
                let sold_units = (units_to_sell - remaining_units).max(0.0);
                if sold_units <= 0.000001 {
                    warn!(
                        "Force-close reported success but no fill detected for {}",
                        trade.token_type.display_name()
                    );
                    return false;
                }
                let pnl = (current_price - trade.purchase_price) * sold_units;
                self.record_trade_event_required_strategy(
                    trade.strategy_id.as_str(),
                    Some(Self::build_exit_event_key(
                        trade.strategy_id.as_str(),
                        trade.source_timeframe.as_str(),
                        trade.market_timestamp,
                        trade.token_id.as_str(),
                        "EXIT_SIGNAL",
                        Some(reason),
                    )),
                    trade.market_timestamp,
                    Some(trade.source_timeframe.as_str()),
                    Some(trade.condition_id.clone()),
                    Some(trade.token_id.clone()),
                    Some(trade.token_type.display_name().to_string()),
                    Some("SELL".to_string()),
                    "EXIT_SIGNAL",
                    Some(current_price),
                    Some(sold_units),
                    Some(current_price * sold_units),
                    Some(pnl),
                    Some(reason.to_string()),
                );

                if remaining_units > 0.000001 {
                    trade.sold = false;
                    trade.units = remaining_units;
                    trade.confirmed_balance = Some(remaining_units);
                    let mut pending = self.pending_trades.lock().await;
                    if let Some(t) = pending.get_mut(key) {
                        *t = trade.clone();
                    }
                    drop(pending);
                    crate::log_println!(
                        "   ⚠️  Force-exit partial fill; {:.6} shares remain open",
                        remaining_units
                    );
                    return false;
                }

                trade.sold = true;
                if let Some(ref detector) = self.detector {
                    detector
                        .mark_cycle_completed(trade.token_type.clone())
                        .await;
                }
                let mut pending = self.pending_trades.lock().await;
                pending.remove(key);
                true
            }
            Err(e) => {
                warn!(
                    "Force-close failed for {}: {}",
                    trade.token_type.display_name(),
                    e
                );
                false
            }
        }
    }

    /// Get the opposite token ID for a given token type and condition ID
    /// Returns the token ID of the opposite token (Up <-> Down)
    async fn get_opposite_token_id(
        &self,
        token_type: &TokenType,
        condition_id: &str,
    ) -> Result<String> {
        // Get market details to find the opposite token
        let market_details = self.api.get_market(condition_id).await?;

        let opposite_type = token_type.opposite();
        let target_outcome = match opposite_type {
            TokenType::BtcUp | TokenType::EthUp | TokenType::SolanaUp | TokenType::XrpUp => "UP",
            TokenType::BtcDown
            | TokenType::EthDown
            | TokenType::SolanaDown
            | TokenType::XrpDown => "DOWN",
        };

        // Find the opposite token in the market
        for token in &market_details.tokens {
            let outcome_upper = token.outcome.to_uppercase();
            if outcome_upper.contains(target_outcome)
                || (target_outcome == "UP" && outcome_upper == "1")
                || (target_outcome == "DOWN" && outcome_upper == "0")
            {
                return Ok(token.token_id.clone());
            }
        }

        anyhow::bail!(
            "Could not find opposite token for {} in market {}",
            token_type.display_name(),
            condition_id
        )
    }

    fn token_family(token_type: &crate::detector::TokenType) -> &'static str {
        match token_type {
            crate::detector::TokenType::BtcUp | crate::detector::TokenType::BtcDown => "BTC",
            crate::detector::TokenType::EthUp | crate::detector::TokenType::EthDown => "ETH",
            crate::detector::TokenType::SolanaUp | crate::detector::TokenType::SolanaDown => "SOL",
            crate::detector::TokenType::XrpUp | crate::detector::TokenType::XrpDown => "XRP",
        }
    }

    fn parse_token_type_label(label: &str) -> Option<crate::detector::TokenType> {
        let normalized = label.trim().to_ascii_uppercase().replace('_', " ");
        match normalized.as_str() {
            "BTC UP" => Some(crate::detector::TokenType::BtcUp),
            "BTC DOWN" => Some(crate::detector::TokenType::BtcDown),
            "ETH UP" => Some(crate::detector::TokenType::EthUp),
            "ETH DOWN" => Some(crate::detector::TokenType::EthDown),
            "SOL UP" | "SOLANA UP" => Some(crate::detector::TokenType::SolanaUp),
            "SOL DOWN" | "SOLANA DOWN" => Some(crate::detector::TokenType::SolanaDown),
            "XRP UP" => Some(crate::detector::TokenType::XrpUp),
            "XRP DOWN" => Some(crate::detector::TokenType::XrpDown),
            _ => None,
        }
    }

    fn asset_symbol_from_token_type_label(label: Option<&str>) -> Option<String> {
        let token_type = Self::parse_token_type_label(label?)?;
        Some(Self::token_family(&token_type).to_string())
    }

    fn normalize_asset_symbol(raw: &str) -> Option<&'static str> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "BTC" | "BITCOIN" => Some("BTC"),
            "ETH" | "ETHEREUM" => Some("ETH"),
            "SOL" | "SOLANA" => Some("SOL"),
            "XRP" => Some("XRP"),
            "DOGE" | "DOGECOIN" => Some("DOGE"),
            "BNB" => Some("BNB"),
            "HYPE" => Some("HYPE"),
            _ => None,
        }
    }

    fn asset_symbol_from_request_id(request_id: &str) -> Option<String> {
        request_id
            .split(':')
            .find_map(Self::normalize_asset_symbol)
            .map(str::to_string)
    }

    fn asset_symbol_from_reason_request_id(reason: Option<&str>) -> Option<String> {
        let reason = reason?;
        let payload: Value = serde_json::from_str(reason).ok()?;
        let request_id = payload.get("request_id")?.as_str()?;
        Self::asset_symbol_from_request_id(request_id)
    }

    fn push_unique_restore_candidate(
        candidates: &mut Vec<crate::tracking_db::LatestEntryFillRecord>,
        seen: &mut HashSet<(String, String, String)>,
        row: crate::tracking_db::LatestEntryFillRecord,
    ) {
        let dedupe_key = (
            row.strategy_id.clone(),
            row.condition_id.clone(),
            row.token_id.clone(),
        );
        if seen.insert(dedupe_key) {
            candidates.push(row);
        }
    }

    /// Returns true if we already traded this symbol family in this period.
    /// Optional timeframe scope prevents M5/M15/H1 from blocking each other.
    pub async fn has_active_position_for_timeframe(
        &self,
        period_timestamp: u64,
        token_type: crate::detector::TokenType,
        source_timeframe: Option<&str>,
        entry_mode: Option<EntryExecutionMode>,
    ) -> bool {
        let source_timeframe = source_timeframe.map(|tf| Self::normalize_timeframe_label(Some(tf)));
        let pending = self.pending_trades.lock().await;
        for (_, trade) in pending.iter() {
            if trade.market_timestamp == period_timestamp
                && Self::token_family(&trade.token_type) == Self::token_family(&token_type)
                && !trade.redemption_abandoned
                && !trade.sold
                && trade.buy_order_confirmed
                && entry_mode
                    .map(|m| Self::mode_from_entry_label(trade.entry_mode.as_str()) == m)
                    .unwrap_or(true)
                && source_timeframe
                    .as_ref()
                    .map(|tf| &trade.source_timeframe == tf)
                    .unwrap_or(true)
            {
                return true;
            }
        }
        false
    }

    pub async fn has_active_position(
        &self,
        period_timestamp: u64,
        token_type: crate::detector::TokenType,
    ) -> bool {
        self.has_active_position_for_timeframe(period_timestamp, token_type, None, None)
            .await
    }

    fn mode_from_entry_label(value: &str) -> EntryExecutionMode {
        EntryExecutionMode::parse(value)
    }

    pub async fn active_trade_count_for_timeframe(
        &self,
        period_timestamp: u64,
        source_timeframe: &str,
        entry_mode: Option<EntryExecutionMode>,
        strategy_id: Option<&str>,
        asset_symbol: Option<&str>,
    ) -> usize {
        let source_timeframe = Self::normalize_timeframe_label(Some(source_timeframe));
        let strategy_id = strategy_id.map(|value| Self::normalize_strategy_id(Some(value)));
        let asset_symbol = asset_symbol.map(|value| value.trim().to_ascii_uppercase());
        let pending = self.pending_trades.lock().await;
        pending
            .values()
            .filter(|trade| {
                trade.market_timestamp == period_timestamp
                    && trade.source_timeframe == source_timeframe
                    && entry_mode
                        .map(|m| Self::mode_from_entry_label(trade.entry_mode.as_str()) == m)
                        .unwrap_or(true)
                    && strategy_id
                        .as_ref()
                        .map(|id| trade.strategy_id == *id)
                        .unwrap_or(true)
                    && asset_symbol
                        .as_ref()
                        .map(|asset| Self::token_family(&trade.token_type) == asset.as_str())
                        .unwrap_or(true)
                    && !trade.sold
                    && !trade.redemption_abandoned
            })
            .count()
    }

    pub async fn has_unconfirmed_buy_order_for_scope(
        &self,
        period_timestamp: u64,
        source_timeframe: &str,
        strategy_id: &str,
        entry_mode: EntryExecutionMode,
        token_id: &str,
    ) -> bool {
        let source_timeframe = Self::normalize_timeframe_label(Some(source_timeframe));
        let strategy_id = Self::normalize_strategy_id(Some(strategy_id));
        let token_id = token_id.trim();
        let pending = self.pending_trades.lock().await;
        pending.values().any(|trade| {
            trade.market_timestamp == period_timestamp
                && trade.source_timeframe == source_timeframe
                && trade.strategy_id == strategy_id
                && trade.token_id == token_id
                && Self::mode_from_entry_label(trade.entry_mode.as_str()) == entry_mode
                && !trade.buy_order_confirmed
                && !trade.sold
                && !trade.redemption_abandoned
                && trade
                    .order_id
                    .as_deref()
                    .map(Self::is_exchange_order_id)
                    .unwrap_or(false)
        })
    }

    pub async fn cancel_pending_orders_for_scope(
        &self,
        period_timestamp: u64,
        source_timeframe: &str,
        strategy_id: &str,
        entry_mode: EntryExecutionMode,
        token_id: &str,
        side: &str,
    ) -> Result<usize> {
        self.cancel_pending_orders_for_scope_filtered(
            period_timestamp,
            source_timeframe,
            strategy_id,
            entry_mode,
            token_id,
            side,
            None,
        )
        .await
    }

    pub async fn cancel_pending_orders_for_scope_filtered(
        &self,
        period_timestamp: u64,
        source_timeframe: &str,
        strategy_id: &str,
        entry_mode: EntryExecutionMode,
        token_id: &str,
        side: &str,
        cancel_price_above: Option<f64>,
    ) -> Result<usize> {
        if self.simulation_mode {
            return Ok(0);
        }

        if !side.eq_ignore_ascii_case("BUY") {
            return Ok(0);
        }

        let source_timeframe = Self::normalize_timeframe_label(Some(source_timeframe));
        let normalized_strategy_id = Self::normalize_strategy_id(Some(strategy_id));
        let token_id = token_id.trim().to_string();

        let cancel_candidates: Vec<(String, String)> = {
            let pending = self.pending_trades.lock().await;
            pending
                .iter()
                .filter(|(_, trade)| {
                    trade.market_timestamp == period_timestamp
                        && trade.source_timeframe == source_timeframe
                        && trade.strategy_id == normalized_strategy_id
                        && trade.token_id == token_id
                        && Self::mode_from_entry_label(trade.entry_mode.as_str()) == entry_mode
                        && !trade.buy_order_confirmed
                        && !trade.sold
                        && !trade.redemption_abandoned
                        && cancel_price_above
                            .map(|price| trade.purchase_price > price)
                            .unwrap_or(true)
                })
                .filter_map(|(key, trade)| {
                    trade
                        .order_id
                        .as_ref()
                        .map(|order_id| (key.clone(), order_id.clone()))
                })
                .collect()
        };

        let mut db_only_rows: Vec<PendingOrderRecord> = Vec::new();
        if let Some(db) = self.tracking_db.as_ref() {
            let in_memory_ids: HashSet<String> = cancel_candidates
                .iter()
                .map(|(_, order_id)| order_id.clone())
                .collect();
            if let Ok(rows) = db.list_open_pending_orders_for_strategy_scope(
                source_timeframe.as_str(),
                entry_mode.as_str(),
                normalized_strategy_id.as_str(),
                period_timestamp,
                token_id.as_str(),
                "BUY",
            ) {
                db_only_rows = rows
                    .into_iter()
                    .filter(|row| {
                        !in_memory_ids.contains(row.order_id.as_str())
                            && cancel_price_above
                                .map(|price| row.price > price)
                                .unwrap_or(true)
                    })
                    .collect();
            }
        }

        let candidate_count = cancel_candidates.len();
        let db_only_count = db_only_rows.len();
        if candidate_count == 0 && db_only_count == 0 {
            return Ok(0);
        }
        if candidate_count > 0 {
            self.apply_cancel_candidates(cancel_candidates, "adaptive_chase_replace_reconcile")
                .await?;
        }
        if db_only_count > 0 {
            self.cancel_db_only_pending_rows(db_only_rows, "adaptive_chase_replace_reconcile")
                .await?;
        }
        Ok(candidate_count + db_only_count)
    }

    pub async fn cancel_pending_orders_for_strategy_condition(
        &self,
        strategy_id: &str,
        condition_id: &str,
        reconcile_reason: &'static str,
    ) -> Result<usize> {
        if self.simulation_mode {
            return Ok(0);
        }
        let normalized_strategy_id = Self::normalize_strategy_id(Some(strategy_id));
        let normalized_condition_id = condition_id.trim().to_ascii_lowercase();
        if normalized_condition_id.is_empty() {
            return Ok(0);
        }

        let cancel_candidates: Vec<(String, String)> = {
            let pending = self.pending_trades.lock().await;
            pending
                .iter()
                .filter(|(_, trade)| {
                    trade.strategy_id == normalized_strategy_id
                        && trade
                            .condition_id
                            .trim()
                            .eq_ignore_ascii_case(normalized_condition_id.as_str())
                        && !trade.redemption_abandoned
                })
                .filter_map(|(key, trade)| {
                    trade
                        .order_id
                        .as_ref()
                        .map(|order_id| (key.clone(), order_id.clone()))
                })
                .collect()
        };

        let mut db_only_rows: Vec<PendingOrderRecord> = Vec::new();
        if let Some(db) = self.tracking_db.as_ref() {
            let in_memory_ids: HashSet<String> = cancel_candidates
                .iter()
                .map(|(_, order_id)| order_id.clone())
                .collect();
            if let Ok(rows) = db.list_open_pending_orders_for_strategy_condition(
                normalized_strategy_id.as_str(),
                normalized_condition_id.as_str(),
            ) {
                db_only_rows = rows
                    .into_iter()
                    .filter(|row| !in_memory_ids.contains(row.order_id.as_str()))
                    .collect();
            }
        }

        let candidate_count = cancel_candidates.len();
        let db_only_count = db_only_rows.len();
        if candidate_count == 0 && db_only_count == 0 {
            return Ok(0);
        }
        if candidate_count > 0 {
            self.apply_cancel_candidates(cancel_candidates, reconcile_reason)
                .await?;
        }
        if db_only_count > 0 {
            self.cancel_db_only_pending_rows(db_only_rows, reconcile_reason)
                .await?;
        }
        Ok(candidate_count + db_only_count)
    }

    pub async fn cancel_pending_orders_for_scope_order_ids(
        &self,
        period_timestamp: u64,
        source_timeframe: &str,
        strategy_id: &str,
        entry_mode: EntryExecutionMode,
        token_id: &str,
        side: &str,
        order_ids: &[String],
    ) -> Result<usize> {
        if self.simulation_mode {
            return Ok(0);
        }

        if !side.eq_ignore_ascii_case("BUY") {
            return Ok(0);
        }

        if order_ids.is_empty() {
            return Ok(0);
        }

        let source_timeframe = Self::normalize_timeframe_label(Some(source_timeframe));
        let normalized_strategy_id = Self::normalize_strategy_id(Some(strategy_id));
        let token_id = token_id.trim().to_string();
        let target_ids: std::collections::HashSet<String> = order_ids
            .iter()
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
            .collect();
        if target_ids.is_empty() {
            return Ok(0);
        }

        let cancel_candidates: Vec<(String, String)> = {
            let pending = self.pending_trades.lock().await;
            pending
                .iter()
                .filter(|(_, trade)| {
                    trade.market_timestamp == period_timestamp
                        && trade.source_timeframe == source_timeframe
                        && trade.strategy_id == normalized_strategy_id
                        && trade.token_id == token_id
                        && Self::mode_from_entry_label(trade.entry_mode.as_str()) == entry_mode
                        && !trade.buy_order_confirmed
                        && !trade.sold
                        && !trade.redemption_abandoned
                })
                .filter_map(|(key, trade)| {
                    trade.order_id.as_ref().and_then(|order_id| {
                        if target_ids.contains(order_id.as_str()) {
                            Some((key.clone(), order_id.clone()))
                        } else {
                            None
                        }
                    })
                })
                .collect()
        };

        let mut db_only_rows: Vec<PendingOrderRecord> = Vec::new();
        if let Some(db) = self.tracking_db.as_ref() {
            let in_memory_ids: HashSet<String> = cancel_candidates
                .iter()
                .map(|(_, order_id)| order_id.clone())
                .collect();
            if let Ok(rows) = db.list_open_pending_orders_for_strategy_scope(
                source_timeframe.as_str(),
                entry_mode.as_str(),
                normalized_strategy_id.as_str(),
                period_timestamp,
                token_id.as_str(),
                "BUY",
            ) {
                db_only_rows = rows
                    .into_iter()
                    .filter(|row| {
                        target_ids.contains(row.order_id.as_str())
                            && !in_memory_ids.contains(row.order_id.as_str())
                    })
                    .collect();
            }
        }

        let candidate_count = cancel_candidates.len();
        let db_only_count = db_only_rows.len();
        if candidate_count == 0 && db_only_count == 0 {
            return Ok(0);
        }
        if candidate_count > 0 {
            self.apply_cancel_candidates(cancel_candidates, "adaptive_chase_ladder_reprice")
                .await?;
        }
        if db_only_count > 0 {
            self.cancel_db_only_pending_rows(db_only_rows, "adaptive_chase_ladder_reprice")
                .await?;
        }
        Ok(candidate_count + db_only_count)
    }

    pub async fn cancel_stale_ladder_orders(
        &self,
        period_timestamp: u64,
        source_timeframe: &str,
        symbol_family: &str,
    ) -> Result<usize> {
        self.cancel_stale_ladder_orders_filtered(
            period_timestamp,
            source_timeframe,
            symbol_family,
            None,
        )
        .await
    }

    pub async fn cancel_stale_ladder_orders_filtered(
        &self,
        period_timestamp: u64,
        source_timeframe: &str,
        symbol_family: &str,
        cancel_price_above: Option<f64>,
    ) -> Result<usize> {
        if self.simulation_mode {
            return Ok(0);
        }

        let source_timeframe = Self::normalize_timeframe_label(Some(source_timeframe));
        let stale_entries: Vec<(String, String)> = {
            let pending = self.pending_trades.lock().await;
            pending
                .iter()
                .filter(|(key, t)| {
                    t.market_timestamp == period_timestamp
                        && t.source_timeframe == source_timeframe
                        && Self::token_family(&t.token_type) == symbol_family
                        && EntryExecutionMode::parse(t.entry_mode.as_str()).is_ladder()
                        && !t.buy_order_confirmed
                        && !t.sold
                        && !t.redemption_abandoned
                        && cancel_price_above
                            .map(|price| t.purchase_price > price)
                            .unwrap_or(true)
                        && key.contains("_ladder_")
                })
                .filter_map(|(key, t)| t.order_id.as_ref().map(|oid| (key.clone(), oid.clone())))
                .collect()
        };

        if stale_entries.is_empty() {
            return Ok(0);
        }

        let unique_exchange_order_ids: Vec<String> = stale_entries
            .iter()
            .filter(|(_, oid)| Self::is_exchange_order_id(oid))
            .map(|(_, oid)| oid.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let mut canceled_set: HashSet<String> = HashSet::new();
        if !unique_exchange_order_ids.is_empty() {
            let order_ids: Vec<&str> = unique_exchange_order_ids
                .iter()
                .map(|oid| oid.as_str())
                .collect();
            let cancel_response = self.api.cancel_orders(&order_ids).await?;
            canceled_set.extend(cancel_response.canceled);
        }
        if canceled_set.is_empty() && !unique_exchange_order_ids.is_empty() {
            debug!(
                "Premarket stale ladder cancel returned no confirmations for {} candidate order(s)",
                stale_entries.len()
            );
        }

        let mut status_updates: Vec<(
            String,
            String,
            &'static str,
            bool,
            bool,
            Option<(f64, f64, f64)>,
        )> = Vec::with_capacity(stale_entries.len());
        for (key, order_id) in stale_entries {
            if !Self::is_exchange_order_id(&order_id) {
                status_updates.push((key, order_id, "STALE", true, false, None));
                continue;
            }

            let mut matched_fill = None;
            let status = match self.api.get_order(&order_id).await {
                Ok(order) => {
                    matched_fill = Self::matched_fill_from_order(&order);
                    if let Some(status) = Self::terminal_status_for_order(order.status) {
                        status
                    } else if canceled_set.contains(&order_id) {
                        // Cancel acknowledgement is not terminal. Keep it OPEN until exchange
                        // confirms FILLED/CANCELED/UNMATCHED to avoid false close + overfill.
                        "OPEN"
                    } else {
                        "OPEN"
                    }
                }
                Err(e) => {
                    debug!(
                        "stale_ladder_cancel_reconcile order status lookup miss for {}: {}",
                        order_id, e
                    );
                    if canceled_set.contains(&order_id) {
                        // Conservative on status fetch failure after cancel request: treat as OPEN.
                        // This prevents placing a new order while the old order may still be live.
                        "OPEN"
                    } else {
                        "RECONCILE_FAILED"
                    }
                }
            };
            let remove = matches!(status, "CANCELED" | "STALE");
            let mark_filled =
                status == "FILLED" || (status == "CANCELED" && matched_fill.is_some());
            status_updates.push((key, order_id, status, remove, mark_filled, matched_fill));
        }

        let mut removed = 0usize;
        let mut fill_transitions = Vec::new();
        let mut pending = self.pending_trades.lock().await;
        for (key, order_id, status, remove, mark_filled, matched_fill) in status_updates {
            if mark_filled {
                let trade_snapshot = if let Some(trade) = pending.get_mut(&key) {
                    trade.buy_order_confirmed = true;
                    Some(trade.clone())
                } else {
                    None
                };
                let (matched_price, matched_units, matched_notional) =
                    matched_fill.unwrap_or((0.0, 0.0, 0.0));
                let has_matched = matched_price > 0.0 && matched_units > 0.0;
                fill_transitions.push(FillTransition {
                    order_id: Some(order_id.clone()),
                    trade_key: Some(key.clone()),
                    strategy_id: Some(
                        trade_snapshot
                            .as_ref()
                            .map(|t| t.strategy_id.clone())
                            .unwrap_or_else(crate::strategy::default_strategy_id),
                    ),
                    period_timestamp: trade_snapshot
                        .as_ref()
                        .map(|t| t.market_timestamp)
                        .unwrap_or(0),
                    timeframe: trade_snapshot.as_ref().map(|t| t.source_timeframe.clone()),
                    condition_id: trade_snapshot.as_ref().map(|t| t.condition_id.clone()),
                    token_id: trade_snapshot
                        .as_ref()
                        .map(|t| t.token_id.clone())
                        .unwrap_or_default(),
                    token_type: trade_snapshot
                        .as_ref()
                        .map(|t| t.token_type.display_name().to_string()),
                    side: Some("BUY".to_string()),
                    price: if has_matched {
                        Some(matched_price)
                    } else {
                        trade_snapshot.as_ref().map(|t| t.purchase_price)
                    },
                    units: if has_matched {
                        Some(matched_units)
                    } else {
                        trade_snapshot.as_ref().map(|t| t.units)
                    },
                    notional_usd: if has_matched {
                        Some(matched_notional)
                    } else {
                        trade_snapshot.as_ref().map(|t| t.units * t.purchase_price)
                    },
                    reason: if status == "FILLED" {
                        "stale_ladder_cancel_reconcile".to_string()
                    } else {
                        "stale_ladder_cancel_partial_canceled_fill".to_string()
                    },
                    reconciled: true,
                });
            }
            if remove && pending.remove(&key).is_some() {
                self.remove_pending_order_tracking(&order_id);
                removed += 1;
            }
            if !mark_filled {
                self.update_pending_order_status(&order_id, status);
            }
        }
        drop(pending);
        for fill in fill_transitions {
            self.on_order_filled(fill);
        }
        Ok(removed)
    }

    /// Clean up old abandoned trades (trades from previous periods that failed redemption)
    /// This prevents the pending_trades list from growing indefinitely
    pub async fn cleanup_old_abandoned_trades(&self, current_period_timestamp: u64) {
        let mut pending = self.pending_trades.lock().await;
        let mut to_remove: Vec<(String, u64, Option<String>, &'static str)> = Vec::new();

        for (key, trade) in pending.iter() {
            if trade.market_timestamp >= current_period_timestamp {
                continue;
            }

            if trade.redemption_abandoned {
                to_remove.push((
                    key.clone(),
                    trade.market_timestamp,
                    trade.order_id.clone(),
                    "ABANDONED_CLEANUP",
                ));
                continue;
            }

            // Non-progressing old entries (unfilled or stale pending) should not block new periods.
            if !trade.buy_order_confirmed && !trade.sold {
                to_remove.push((
                    key.clone(),
                    trade.market_timestamp,
                    trade.order_id.clone(),
                    "STALE_NO_PROGRESS_CLEANUP",
                ));
            }
        }

        for (key, old_period, maybe_order_id, status) in to_remove {
            pending.remove(&key);
            if let Some(order_id) = maybe_order_id.as_deref() {
                self.update_pending_order_status(order_id, status);
                self.remove_pending_order_tracking(order_id);
            }
            crate::log_println!(
                "🧹 Cleaned stale trade from period {} (status={})",
                old_period,
                status
            );
        }

        drop(pending);
    }

    /// Sync pending trades with actual portfolio balance
    /// Checks if tokens are still in portfolio - if balance is 0, mark as sold (already redeemed)
    /// This prevents the bot from trying to redeem already-redeemed tokens
    pub async fn sync_trades_with_portfolio(&self) -> Result<()> {
        let mut pending_trades: Vec<(String, PendingTrade)> = {
            let pending = self.pending_trades.lock().await;
            pending
                .iter()
                .map(|(key, trade)| (key.clone(), trade.clone()))
                .collect()
        };
        // Keep redemption queue ordering deterministic: oldest period first.
        pending_trades.sort_by(|(_, a), (_, b)| {
            a.market_timestamp
                .cmp(&b.market_timestamp)
                .then_with(|| a.condition_id.cmp(&b.condition_id))
                .then_with(|| a.token_id.cmp(&b.token_id))
        });

        if pending_trades.is_empty() {
            return Ok(());
        }

        crate::log_println!(
            "🔄 Syncing {} pending trade(s) with portfolio balance...",
            pending_trades.len()
        );

        let startup_budget_ms = Self::startup_balance_scan_budget_ms();
        let startup_started = std::time::Instant::now();
        let total_pending = pending_trades.len();
        let mut updated_count = 0;
        let mut removed_count = 0;
        let mut checked_count = 0usize;
        let mut skipped_budget = 0usize;

        for (idx, (key, trade)) in pending_trades.into_iter().enumerate() {
            if startup_started.elapsed().as_millis() as u64 >= startup_budget_ms {
                skipped_budget = total_pending.saturating_sub(idx);
                warn!(
                    "Startup portfolio sync budget reached ({}ms). Skipping remaining {} trade(s) for this boot.",
                    startup_budget_ms, skipped_budget
                );
                break;
            }

            // Skip if already sold
            if trade.sold {
                continue;
            }

            // Startup portfolio sync should only reconcile already-filled inventory.
            // Pending entry orders have zero token balance by definition; treating them as
            // "already redeemed" drops them from in-memory tracking and breaks 5s ladder cancels.
            if !trade.buy_order_confirmed {
                continue;
            }

            checked_count = checked_count.saturating_add(1);
            // Check actual token balance
            match self.check_balance_only_with_timeout(&trade.token_id).await {
                Ok(balance) => {
                    // Conditional tokens use 1e6 as base unit (like USDC)
                    // Convert from smallest unit to actual shares
                    let balance_decimal = balance / rust_decimal::Decimal::from(1_000_000u64);
                    let balance_f64 = f64::try_from(balance_decimal).unwrap_or(0.0);

                    let epsilon = Self::redemption_balance_epsilon_shares();
                    if balance_f64 <= epsilon {
                        // Tiny residual balances can persist after prior redemptions; treat them as closed.
                        crate::log_println!("   ✅ Trade {}: Token balance {:.6} <= epsilon {:.6} - already redeemed, marking as sold",
                            &trade.token_id[..16], balance_f64, epsilon);

                        let mut pending = self.pending_trades.lock().await;
                        if let Some(t) = pending.get_mut(key.as_str()) {
                            t.sold = true;
                        }
                        pending.remove(&key);
                        drop(pending);
                        Self::clear_inflight_redemption_tx(trade.condition_id.as_str());
                        Self::clear_condition_retry_after_epoch(trade.condition_id.as_str());

                        removed_count += 1;
                    } else {
                        // Balance > 0 - update stored balance if different
                        if (balance_f64 - trade.units).abs() > 0.001 {
                            crate::log_println!(
                                "   🔄 Trade {}: Updating balance from {:.6} to {:.6} shares",
                                &trade.token_id[..16],
                                trade.units,
                                balance_f64
                            );

                            let mut pending = self.pending_trades.lock().await;
                            if let Some(t) = pending.get_mut(key.as_str()) {
                                t.units = balance_f64;
                                t.confirmed_balance = Some(balance_f64);
                            }
                            drop(pending);

                            updated_count += 1;
                        }
                    }
                }
                Err(e) => {
                    debug!(
                        "Failed to check balance for trade {}: {} - skipping sync for this trade",
                        &trade.token_id[..16],
                        e
                    );
                }
            }

            if checked_count % 20 == 0 {
                crate::log_println!(
                    "   ⏳ Startup sync progress: {} balance checks complete",
                    checked_count
                );
            }
        }

        if updated_count > 0 || removed_count > 0 || skipped_budget > 0 {
            crate::log_println!("✅ Portfolio sync complete: {} trade(s) checked, {} updated, {} removed (already redeemed), {} deferred by startup budget", 
                checked_count, updated_count, removed_count, skipped_budget);
        } else {
            crate::log_println!("✅ Portfolio sync complete: All trades match portfolio balance");
        }

        Ok(())
    }

    /// Restore in-memory pending trades from tracking DB + wallet balances after restart.
    /// This prevents missing auto-claim when a process restarts between fill and market resolution.
    pub async fn restore_pending_trades_from_tracking_db(&self) -> Result<()> {
        if self.simulation_mode {
            return Ok(());
        }
        let Some(db) = self.tracking_db.as_ref() else {
            return Ok(());
        };

        let restore_limit = std::env::var("EVPOLY_STARTUP_RESTORE_CANDIDATE_LIMIT")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(2_000)
            .max(100);
        let mut candidates: Vec<crate::tracking_db::LatestEntryFillRecord> = Vec::new();
        let mut seen: HashSet<(String, String, String)> = HashSet::new();

        // Highest priority: lifecycle rows that are resolved + claimable but still not redeemed.
        for row in db.list_redemption_candidates_for_restore(restore_limit)? {
            Self::push_unique_restore_candidate(&mut candidates, &mut seen, row);
            if candidates.len() >= restore_limit {
                break;
            }
        }

        if candidates.len() < restore_limit {
            for row in db.list_open_positions_for_restore(restore_limit)? {
                Self::push_unique_restore_candidate(&mut candidates, &mut seen, row);
                if candidates.len() >= restore_limit {
                    break;
                }
            }
        }
        if candidates.len() < restore_limit {
            for row in db.list_latest_entry_fills(restore_limit)? {
                Self::push_unique_restore_candidate(&mut candidates, &mut seen, row);
                if candidates.len() >= restore_limit {
                    break;
                }
            }
        }
        if candidates.is_empty() {
            return Ok(());
        }

        let mut existing_token_ids: HashSet<String> = {
            let pending = self.pending_trades.lock().await;
            pending
                .values()
                .filter(|t| !t.sold && !t.redemption_abandoned)
                .map(|t| t.token_id.clone())
                .collect()
        };

        let mut restored = 0usize;
        let mut skipped_unknown_token_type = 0usize;
        let mut skipped_zero_balance = 0usize;
        let mut skipped_balance_error = 0usize;
        let skip_balance_check =
            Self::env_bool_named("EVPOLY_STARTUP_RESTORE_SKIP_BALANCE_CHECK", true);
        let startup_budget_ms = Self::startup_balance_scan_budget_ms();
        let startup_started = std::time::Instant::now();
        let total_candidates = candidates.len();
        let mut skipped_budget = 0usize;
        let mut checked_count = 0usize;

        for (idx, row) in candidates.into_iter().enumerate() {
            if startup_started.elapsed().as_millis() as u64 >= startup_budget_ms {
                skipped_budget = total_candidates.saturating_sub(idx);
                warn!(
                    "Startup restore budget reached ({}ms). Deferring {} candidate(s) to later runtime checks.",
                    startup_budget_ms, skipped_budget
                );
                break;
            }

            if existing_token_ids.contains(&row.token_id) {
                continue;
            }

            let Some(token_type_label) = row.token_type.as_deref() else {
                skipped_unknown_token_type += 1;
                continue;
            };
            let Some(token_type) = Self::parse_token_type_label(token_type_label) else {
                skipped_unknown_token_type += 1;
                continue;
            };
            let token_asset_symbol = Self::token_family(&token_type).to_string();

            let balance_f64 = if skip_balance_check {
                // Fast-path restore: approximate units from entry notional/price.
                // Exact wallet balance is verified right before redemption submission.
                row.notional_usd
                    .zip(row.price)
                    .and_then(|(notional, price)| {
                        if notional.is_finite()
                            && price.is_finite()
                            && notional > 0.0
                            && price > 0.0
                        {
                            Some(notional / price)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0.0)
            } else {
                checked_count = checked_count.saturating_add(1);
                let balance = match self.check_balance_only_with_timeout(&row.token_id).await {
                    Ok(v) => v,
                    Err(_) => {
                        skipped_balance_error += 1;
                        continue;
                    }
                };
                let balance_decimal = balance / rust_decimal::Decimal::from(1_000_000u64);
                f64::try_from(balance_decimal).unwrap_or(0.0)
            };
            if balance_f64 <= 0.0 {
                skipped_zero_balance += 1;
                continue;
            }

            let source_timeframe = Self::normalize_timeframe_label(Some(row.timeframe.as_str()));
            let restored_strategy_id = Self::normalize_strategy_id(Some(row.strategy_id.as_str()));
            let purchase_price = row.price.filter(|p| *p > 0.0).unwrap_or(0.5);
            let investment_amount = row
                .notional_usd
                .filter(|n| *n > 0.0)
                .unwrap_or(balance_f64 * purchase_price);
            let synthetic_order_id = format!("restored:{}:{}", row.period_timestamp, row.token_id);

            let trade = PendingTrade {
                token_id: row.token_id.clone(),
                condition_id: row.condition_id.clone(),
                token_type,
                investment_amount,
                units: balance_f64,
                purchase_price,
                sell_price: self.config.sell_price,
                timestamp: std::time::Instant::now(),
                market_timestamp: row.period_timestamp,
                source_timeframe: source_timeframe.clone(),
                strategy_id: restored_strategy_id.clone(),
                entry_mode: EntryExecutionMode::Restored.as_str().to_string(),
                order_id: Some(synthetic_order_id.clone()),
                end_timestamp: Some(Self::estimate_end_timestamp(
                    row.period_timestamp,
                    source_timeframe.as_str(),
                )),
                sold: false,
                confirmed_balance: if skip_balance_check {
                    None
                } else {
                    Some(balance_f64)
                },
                buy_order_confirmed: true,
                limit_sell_orders_placed: true,
                no_sell: true,
                claim_on_closure: true,
                sell_attempts: 0,
                redemption_attempts: 0,
                redemption_abandoned: false,
            };

            let trade_key = format!("restored_{}_{}", row.period_timestamp, row.token_id);
            let mut pending = self.pending_trades.lock().await;
            pending.insert(trade_key, trade);
            drop(pending);
            self.upsert_pending_order_tracking_for_strategy(
                &synthetic_order_id,
                &format!("restored_{}_{}", row.period_timestamp, row.token_id),
                &row.token_id,
                source_timeframe.as_str(),
                EntryExecutionMode::Restored.as_str(),
                Some(restored_strategy_id.as_str()),
                Some(token_asset_symbol.as_str()),
                row.period_timestamp,
                purchase_price,
                investment_amount,
                "BUY",
                "FILLED",
                Some(row.condition_id.as_str()),
            )?;

            existing_token_ids.insert(row.token_id);
            restored += 1;

            if checked_count > 0 && checked_count % 20 == 0 {
                crate::log_println!(
                    "   ⏳ Startup restore progress: {} balance checks complete",
                    checked_count
                );
            }
        }

        crate::log_println!(
            "🔁 Startup recovery: checked={} restored={} skipped_zero_balance={} skipped_unknown_token_type={} skipped_balance_error={} deferred_by_budget={}",
            checked_count,
            restored,
            skipped_zero_balance,
            skipped_unknown_token_type,
            skipped_balance_error,
            skipped_budget
        );

        Ok(())
    }

    /// Mark position as closed (for stop-loss re-entry)
    pub async fn mark_position_closed(&self, period_timestamp: u64) {
        let mut pending = self.pending_trades.lock().await;
        for (_, trade) in pending.iter_mut() {
            if trade.market_timestamp == period_timestamp && !trade.sold {
                trade.sold = true;
                crate::log_println!(
                    "   ✅ Position marked as closed (period: {}) - can re-buy if price recovers",
                    period_timestamp
                );
            }
        }
    }

    /// Execute buy when momentum opportunity is detected
    /// Buys any token (BTC Up/Down, ETH Up/Down) when price reaches trigger_price after 10 minutes
    pub async fn execute_buy(
        &self,
        opportunity: &BuyOpportunity,
        source_timeframe: Option<&str>,
    ) -> Result<()> {
        // Safety check: Verify time remaining is still sufficient before executing buy
        // This acts as a double-check in case market closed between detection and execution
        let min_time_remaining = self.config.min_time_remaining_seconds.unwrap_or(30);
        if opportunity.time_remaining_seconds < min_time_remaining {
            anyhow::bail!(
                "❌ SAFETY CHECK FAILED: Insufficient time remaining for buy\n\
                \n\
                Time remaining: {} seconds\n\
                Minimum required: {} seconds\n\
                \n\
                Buy was blocked to prevent risky trades near market closure.\n\
                This opportunity should not have been created - please check detector logic.",
                opportunity.time_remaining_seconds,
                min_time_remaining
            );
        }
        let source_timeframe = Self::normalize_timeframe_label(source_timeframe);

        let fixed_amount = self.config.fixed_trade_amount;

        // Calculate units for the token
        let units = fixed_amount / opportunity.bid_price;
        let total_cost = units * opportunity.bid_price;
        let expected_profit_at_sell = (self.config.sell_price - opportunity.bid_price) * units;

        crate::log_println!("═══════════════════════════════════════════════════════════");
        crate::log_println!("💰 EXECUTING BUY ORDER");
        crate::log_println!("═══════════════════════════════════════════════════════════");
        crate::log_println!("📊 Order Details:");
        crate::log_println!("   Token Type: {}", opportunity.token_type.display_name());
        crate::log_println!("   Token ID: {}", opportunity.token_id);
        crate::log_println!("   Condition ID: {}", opportunity.condition_id);
        crate::log_println!("   Side: BUY");
        crate::log_println!("   Price: ${:.6}", opportunity.bid_price);
        crate::log_println!("   Units: {:.6}", units);
        crate::log_println!("   Total Cost: ${:.6}", total_cost);
        crate::log_println!("   Investment Amount: ${:.2}", fixed_amount);
        crate::log_println!("");
        crate::log_println!("📈 Trade Parameters:");
        crate::log_println!("   Target sell price: ${:.6}", self.config.sell_price);
        crate::log_println!(
            "   Expected profit at sell: ${:.6}",
            expected_profit_at_sell
        );
        crate::log_println!(
            "   Time elapsed: {}m {}s",
            opportunity.time_elapsed_seconds / 60,
            opportunity.time_elapsed_seconds % 60
        );
        crate::log_println!(
            "   Time remaining: {}s (minimum required: {}s)",
            opportunity.time_remaining_seconds,
            min_time_remaining
        );
        crate::log_println!("   Period timestamp: {}", opportunity.period_timestamp);
        crate::log_println!("");

        if self.simulation_mode {
            crate::log_println!("🎮 SIMULATION MODE - Creating limit buy order");
            crate::log_println!("   ✅ SIMULATION: Limit buy order created:");
            crate::log_println!(
                "      - Token Type: {}",
                opportunity.token_type.display_name()
            );
            crate::log_println!("      - Token: {}", &opportunity.token_id[..16]);
            crate::log_println!("      - Units: {:.6}", units);
            crate::log_println!("      - Price: ${:.6}", opportunity.bid_price);
            crate::log_println!("      - Total: ${:.6}", total_cost);
            crate::log_println!("      - Strategy: Hold until market closure (claim $1.00 if winning, $0.00 if losing)");

            // In simulation mode, create a limit order that will be checked for fills
            if let Some(tracker) = &self.simulation_tracker {
                tracker
                    .add_limit_order(
                        opportunity.token_id.clone(),
                        opportunity.token_type.clone(),
                        opportunity.condition_id.clone(),
                        opportunity.bid_price,
                        units,
                        "BUY".to_string(),
                        opportunity.period_timestamp,
                    )
                    .await;

                // Also track as pending trade for consistency
                // In simulation mode, we hold positions until market closure (no selling)
                let trade_key = format!(
                    "{}_{}_market",
                    opportunity.period_timestamp, opportunity.token_id
                );
                let mut pending = self.pending_trades.lock().await;
                let trade = PendingTrade {
                    token_id: opportunity.token_id.clone(),
                    condition_id: opportunity.condition_id.clone(),
                    token_type: opportunity.token_type.clone(),
                    investment_amount: fixed_amount,
                    units,
                    purchase_price: opportunity.bid_price,
                    sell_price: self.config.sell_price, // Not used in simulation - positions held until closure
                    timestamp: std::time::Instant::now(),
                    market_timestamp: opportunity.period_timestamp,
                    source_timeframe: source_timeframe.clone(),
                    strategy_id: crate::strategy::default_strategy_id(),
                    entry_mode: EntryExecutionMode::Legacy.as_str().to_string(),
                    order_id: None,
                    end_timestamp: Some(Self::estimate_end_timestamp(
                        opportunity.period_timestamp,
                        source_timeframe.as_str(),
                    )),
                    sold: false,
                    confirmed_balance: None,
                    buy_order_confirmed: false,
                    limit_sell_orders_placed: false, // No sell orders in simulation - hold until closure
                    no_sell: true, // Mark as no_sell since we hold until market closure
                    claim_on_closure: true, // Will claim at market closure
                    sell_attempts: 0,
                    redemption_attempts: 0,
                    redemption_abandoned: false,
                };
                pending.insert(trade_key, trade);
            }

            return Ok(());
        } else {
            // Place real market order
            // For BUY market orders, we pass USD value (not units)
            // The exchange will determine how many units we get at market price
            crate::log_println!("🚀 PRODUCTION MODE - Placing order on exchange");
            crate::log_println!("   Order parameters:");
            crate::log_println!(
                "      Token Type: {}",
                opportunity.token_type.display_name()
            );
            crate::log_println!("      Token ID: {}", opportunity.token_id);
            crate::log_println!("      Side: BUY");
            crate::log_println!(
                "      Amount: ${:.6} (market order - units determined by market price)",
                fixed_amount
            );
            crate::log_println!("      Type: FOK (Fill-or-Kill)");

            match self
                .api
                .place_market_order(
                    &opportunity.token_id,
                    fixed_amount, // USD value for BUY market orders
                    "BUY",
                    Some("FOK"),
                )
                .await
            {
                Ok(response) => {
                    crate::log_println!("   ✅ ORDER PLACED SUCCESSFULLY");
                    crate::log_println!("      Order ID: {:?}", response.order_id);
                    crate::log_println!("      Status: {}", response.status);
                    if let Some(msg) = &response.message {
                        crate::log_println!("      Message: {}", msg);
                    }

                    // Continuously check balance until we receive the tokens
                    // Market orders can take a few seconds to settle on-chain
                    crate::log_println!("   🔍 Verifying buy order confirmation...");
                    crate::log_println!("      Polling balance until tokens arrive (checking every 2 seconds, max 60 seconds)...");

                    let max_wait_seconds = 60;
                    let check_interval_seconds = 2;
                    let start_time = std::time::Instant::now();
                    let mut balance_f64 = 0.0;
                    let mut balance_confirmed = false;
                    let mut attempt = 0;

                    loop {
                        attempt += 1;
                        let elapsed = start_time.elapsed().as_secs();

                        // Check if we've exceeded the maximum wait time
                        if elapsed >= max_wait_seconds {
                            crate::log_println!(
                                "   ⏱️  Maximum wait time ({}) seconds reached",
                                max_wait_seconds
                            );
                            break;
                        }

                        match self.api.check_balance_only(&opportunity.token_id).await {
                            Ok(balance) => {
                                // Conditional tokens use 1e6 as base unit (like USDC)
                                // Convert from smallest unit to actual shares
                                let balance_decimal =
                                    balance / rust_decimal::Decimal::from(1_000_000u64);
                                balance_f64 = f64::try_from(balance_decimal).unwrap_or(0.0);
                                crate::log_println!(
                                    "   ✅ Balance check (attempt {}, {}s elapsed): {:.6} shares",
                                    attempt,
                                    elapsed,
                                    balance_f64
                                );

                                if balance_f64 > 0.0 {
                                    // We have tokens! Confirm the balance
                                    if balance_f64 >= units * 0.95 {
                                        crate::log_println!("      ✅ Balance confirmed - tokens received successfully (expected ~{:.6}, got {:.6})", units, balance_f64);
                                    } else {
                                        crate::log_println!("      ⚠️  Balance is {:.6} shares (expected ~{:.6}) - may be due to market price difference", balance_f64, units);
                                    }
                                    balance_confirmed = true;
                                    break;
                                } else {
                                    // No tokens yet, wait and retry
                                    crate::log_println!("      ⏳ Tokens not settled yet, waiting {} seconds before next check...", check_interval_seconds);
                                    tokio::time::sleep(tokio::time::Duration::from_secs(
                                        check_interval_seconds,
                                    ))
                                    .await;
                                }
                            }
                            Err(e) => {
                                warn!(
                                    "⚠️  Balance check failed (attempt {}, {}s elapsed): {}",
                                    attempt, elapsed, e
                                );
                                if elapsed < max_wait_seconds {
                                    crate::log_println!(
                                        "      ⏳ Retrying in {} seconds...",
                                        check_interval_seconds
                                    );
                                    tokio::time::sleep(tokio::time::Duration::from_secs(
                                        check_interval_seconds,
                                    ))
                                    .await;
                                } else {
                                    break;
                                }
                            }
                        }
                    }

                    crate::log_println!("   ✅ BUY ORDER CONFIRMED - Token balance verified");
                    crate::log_println!("      Portfolio Balance: {:.6} shares", balance_f64);
                    crate::log_println!("      Expected Units: {:.6} shares", units);

                    // Log successful buy order to history.toml regardless of balance confirmation status
                    // This ensures all successful buys are logged, even if balance check fails or times out
                    let market_name = opportunity.token_type.display_name();
                    let order_id_str = response
                        .order_id
                        .as_ref()
                        .map(|id| format!("{:?}", id))
                        .unwrap_or_else(|| "N/A".to_string());

                    if balance_confirmed && balance_f64 > 0.0 {
                        // Balance of portfolio is confirmed exactly - use this balance_f64 for all downstream logic
                        crate::log_println!(
                            "      ✅ Balance confirmed - tokens received successfully"
                        );
                        crate::log_println!(
                            "      📊 Confirmed portfolio balance: {:.6} shares",
                            balance_f64
                        );

                        // Track the trade with confirmed balance
                        let trade = PendingTrade {
                            token_id: opportunity.token_id.clone(),
                            condition_id: opportunity.condition_id.clone(),
                            token_type: opportunity.token_type.clone(),
                            investment_amount: fixed_amount,
                            units: balance_f64, // Use actual confirmed balance
                            purchase_price: opportunity.bid_price,
                            sell_price: self.config.sell_price,
                            timestamp: std::time::Instant::now(),
                            market_timestamp: opportunity.period_timestamp,
                            source_timeframe: source_timeframe.clone(),
                            strategy_id: crate::strategy::default_strategy_id(),
                            entry_mode: EntryExecutionMode::Legacy.as_str().to_string(),
                            order_id: response.order_id.clone(),
                            end_timestamp: Some(Self::estimate_end_timestamp(
                                opportunity.period_timestamp,
                                source_timeframe.as_str(),
                            )),
                            sold: false,
                            confirmed_balance: Some(balance_f64),
                            buy_order_confirmed: true,
                            limit_sell_orders_placed: self
                                .hold_to_resolution_for_mode(EntryExecutionMode::Legacy), // Hold mode skips active exits
                            no_sell: self.hold_to_resolution_for_mode(EntryExecutionMode::Legacy),
                            claim_on_closure: self
                                .hold_to_resolution_for_mode(EntryExecutionMode::Legacy),
                            sell_attempts: 0,
                            redemption_attempts: 0,
                            redemption_abandoned: false,
                        };

                        // Use token_id as key to track individual tokens
                        let trade_key =
                            format!("{}_{}", opportunity.period_timestamp, opportunity.token_id);
                        let mut pending = self.pending_trades.lock().await;
                        pending.insert(trade_key.clone(), trade.clone());
                        drop(pending);
                        if let Some(order_id) = response.order_id.as_deref() {
                            self.upsert_pending_order_tracking(
                                order_id,
                                &trade_key,
                                &opportunity.token_id,
                                source_timeframe.as_str(),
                                EntryExecutionMode::Legacy.as_str(),
                                Some(Self::token_family(&opportunity.token_type)),
                                opportunity.period_timestamp,
                                opportunity.bid_price,
                                fixed_amount,
                                "BUY",
                                "FILLED",
                            )?;
                        }
                        self.on_order_filled(FillTransition {
                            order_id: response.order_id.clone(),
                            trade_key: Some(trade_key),
                            strategy_id: Some(crate::strategy::default_strategy_id()),
                            period_timestamp: opportunity.period_timestamp,
                            timeframe: Some(source_timeframe.clone()),
                            condition_id: Some(opportunity.condition_id.clone()),
                            token_id: opportunity.token_id.clone(),
                            token_type: Some(opportunity.token_type.display_name().to_string()),
                            side: Some("BUY".to_string()),
                            price: Some(opportunity.bid_price),
                            units: Some(balance_f64),
                            notional_usd: Some(balance_f64 * opportunity.bid_price),
                            reason: "market_buy".to_string(),
                            reconciled: false,
                        });

                        // Log structured buy order event to history.toml with allowance status
                        let market_name = opportunity.token_type.display_name();
                        let order_id_str = response
                            .order_id
                            .as_ref()
                            .map(|id| format!("{:?}", id))
                            .unwrap_or_else(|| "N/A".to_string());
                        let buy_event = format!(
                            "BUY ORDER | Market: {} | Period: {} | Token: {} | Price: ${:.6} | Units: {:.6} | Cost: ${:.6} | Order ID: {} | Status: CONFIRMED",
                            market_name,
                            opportunity.period_timestamp,
                            &opportunity.token_id[..16],
                            opportunity.bid_price,
                            balance_f64,
                            balance_f64 * opportunity.bid_price,
                            order_id_str
                        );
                        crate::log_trading_event(&buy_event);

                        crate::log_println!("   ✅ Trade stored successfully. Will monitor price and sell at ${:.6} or market close.", 
                              self.config.sell_price);

                        let mut trades = self.trades_executed.lock().await;
                        *trades += 1;
                        drop(trades);
                    } else {
                        // Balance confirmation failed or timeout - still log the successful buy
                        let status_note = if !balance_confirmed {
                            "Balance check timeout/failed"
                        } else if balance_f64 == 0.0 {
                            "Balance is 0 (may still be settling)"
                        } else {
                            "Balance mismatch"
                        };

                        // Log successful buy even if balance confirmation failed
                        let buy_event = format!(
                            "BUY ORDER | Market: {} | Period: {} | Token: {} | Price: ${:.6} | Units: {:.6} | Cost: ${:.6} | Order ID: {} | Status: SUCCESS | Note: {}",
                            market_name,
                            opportunity.period_timestamp,
                            &opportunity.token_id[..16],
                            opportunity.bid_price,
                            balance_f64,
                            balance_f64 * opportunity.bid_price,
                            order_id_str,
                            status_note
                        );
                        crate::log_trading_event(&buy_event);

                        warn!("⚠️  Balance mismatch: Expected ~{:.6} shares, but only have {:.6} shares", units, balance_f64);
                        warn!("   The buy order may not have executed fully, or price changed significantly");
                        // Still store the trade but mark as unconfirmed
                        let trade = PendingTrade {
                            token_id: opportunity.token_id.clone(),
                            condition_id: opportunity.condition_id.clone(),
                            token_type: opportunity.token_type.clone(),
                            investment_amount: fixed_amount,
                            units: balance_f64, // Use actual balance
                            purchase_price: opportunity.bid_price,
                            sell_price: self.config.sell_price,
                            timestamp: std::time::Instant::now(),
                            market_timestamp: opportunity.period_timestamp,
                            source_timeframe: source_timeframe.clone(),
                            strategy_id: crate::strategy::default_strategy_id(),
                            entry_mode: EntryExecutionMode::Legacy.as_str().to_string(),
                            order_id: response.order_id.clone(),
                            end_timestamp: Some(Self::estimate_end_timestamp(
                                opportunity.period_timestamp,
                                source_timeframe.as_str(),
                            )),
                            sold: false,
                            confirmed_balance: Some(balance_f64),
                            buy_order_confirmed: balance_f64 > 0.0, // Confirmed if we have any tokens
                            limit_sell_orders_placed: self
                                .hold_to_resolution_for_mode(EntryExecutionMode::Legacy), // Hold mode skips active exits
                            no_sell: self.hold_to_resolution_for_mode(EntryExecutionMode::Legacy),
                            claim_on_closure: self
                                .hold_to_resolution_for_mode(EntryExecutionMode::Legacy),
                            sell_attempts: 0,
                            redemption_attempts: 0,
                            redemption_abandoned: false,
                        };

                        let trade_key =
                            format!("{}_{}", opportunity.period_timestamp, opportunity.token_id);
                        let mut pending = self.pending_trades.lock().await;
                        pending.insert(trade_key.clone(), trade);
                        drop(pending);
                        if let Some(order_id) = response.order_id.as_deref() {
                            self.upsert_pending_order_tracking(
                                order_id,
                                &trade_key,
                                &opportunity.token_id,
                                source_timeframe.as_str(),
                                EntryExecutionMode::Legacy.as_str(),
                                Some(Self::token_family(&opportunity.token_type)),
                                opportunity.period_timestamp,
                                opportunity.bid_price,
                                fixed_amount,
                                "BUY",
                                if balance_f64 > 0.0 {
                                    "FILLED"
                                } else {
                                    "PLACED"
                                },
                            )?;
                        }
                        if balance_f64 > 0.0 {
                            self.on_order_filled(FillTransition {
                                order_id: response.order_id.clone(),
                                trade_key: Some(trade_key.clone()),
                                strategy_id: Some(crate::strategy::default_strategy_id()),
                                period_timestamp: opportunity.period_timestamp,
                                timeframe: Some(source_timeframe.clone()),
                                condition_id: Some(opportunity.condition_id.clone()),
                                token_id: opportunity.token_id.clone(),
                                token_type: Some(opportunity.token_type.display_name().to_string()),
                                side: Some("BUY".to_string()),
                                price: Some(opportunity.bid_price),
                                units: Some(balance_f64),
                                notional_usd: Some(balance_f64 * opportunity.bid_price),
                                reason: "market_buy_partial_or_delayed".to_string(),
                                reconciled: false,
                            });
                        }

                        let mut trades = self.trades_executed.lock().await;
                        *trades += 1;
                        drop(trades);
                    }

                    return Ok(());
                }
                Err(e) => {
                    crate::log_println!("   ❌ ORDER FAILED");
                    crate::log_println!("      Error: {}", e);
                    warn!(
                        "   ⚠️  Failed to place {} token order: {}",
                        opportunity.token_type.display_name(),
                        e
                    );

                    // Log structured buy order failure to history.toml (simplified error message)
                    let market_name = opportunity.token_type.display_name();
                    let error_msg = format!("{:?}", e);
                    let simple_error = if error_msg.contains("order couldn't be fully filled")
                        || error_msg.contains("FOK orders")
                    {
                        "order couldn't be fully filled (FOK)".to_string()
                    } else if error_msg.contains("not enough balance") {
                        "not enough balance / allowance".to_string()
                    } else if error_msg.contains("Insufficient") {
                        "Insufficient balance or allowance".to_string()
                    } else {
                        // Extract first meaningful line from error
                        error_msg
                            .lines()
                            .find(|line| {
                                !line.trim().is_empty()
                                    && !line.contains("Troubleshooting")
                                    && !line.contains("Order details")
                            })
                            .unwrap_or("Order failed")
                            .to_string()
                    };

                    let buy_event = format!(
                    "BUY ORDER | Market: {} | Period: {} | Token: {} | Price: ${:.6} | Units: {:.6} | Cost: ${:.6} | Status: FAILED | Error: {}",
                    market_name,
                    opportunity.period_timestamp,
                    &opportunity.token_id[..16],
                    opportunity.bid_price,
                    units,
                    total_cost,
                    simple_error
                );
                    crate::log_trading_event(&buy_event);

                    return Err(e);
                }
            }
        }
    }

    /// Execute limit buy order - places limit buy for both Up and Down tokens
    /// When a buy order fills, immediately places ONE limit sell order at sell_price (profit target)
    /// Stop-loss is disabled for limit order version
    pub async fn execute_limit_buy(
        &self,
        opportunity: &BuyOpportunity,
        entry_mode: EntryExecutionMode,
        place_sell_orders: bool,
        size_override: Option<f64>,
        source_timeframe: Option<&str>,
        strategy_id: Option<&str>,
        request_id: Option<&str>,
    ) -> Result<()> {
        let hold_to_resolution = self.hold_to_resolution_for_mode(entry_mode);
        let place_sell_orders = place_sell_orders && !hold_to_resolution;
        let fixed_amount = self.config.fixed_trade_amount;
        let source_timeframe = Self::normalize_timeframe_label(source_timeframe);
        let strategy_id = Self::normalize_strategy_id(strategy_id);
        let request_id = request_id
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string);
        if strategy_id == crate::strategy::STRATEGY_ID_PREMARKET_V1 && entry_mode.is_ladder() {
            let now_ts = chrono::Utc::now().timestamp().max(0) as u64;
            if now_ts >= opportunity.period_timestamp {
                let reason = format!(
                    "premarket_late_guard_blocked: now_ts={} open_ts={} lag_s={}",
                    now_ts,
                    opportunity.period_timestamp,
                    now_ts.saturating_sub(opportunity.period_timestamp)
                );
                self.record_entry_precheck_failure(
                    strategy_id.as_str(),
                    source_timeframe.as_str(),
                    entry_mode,
                    opportunity,
                    Some(opportunity.bid_price),
                    size_override,
                    size_override.map(|v| v.max(0.0) * opportunity.bid_price.max(0.0)),
                    reason.clone(),
                );
                log_event(
                    "premarket_late_guard_blocked",
                    json!({
                        "strategy_id": strategy_id.as_str(),
                        "timeframe": source_timeframe.as_str(),
                        "entry_mode": entry_mode.as_str(),
                        "period_timestamp": opportunity.period_timestamp,
                        "token_id": opportunity.token_id,
                        "condition_id": opportunity.condition_id,
                        "source": "trader_execute_limit_buy",
                        "now_ts": now_ts,
                        "lag_s": now_ts.saturating_sub(opportunity.period_timestamp)
                    }),
                );
                return Err(anyhow!(reason));
            }
        }
        let request_force_fak = request_id
            .as_deref()
            .map(|value| {
                let normalized = value.to_ascii_lowercase();
                normalized == "fak" || normalized.ends_with(":fak") || normalized.contains(":fak:")
            })
            .unwrap_or(false);
        let use_fak_limit_entry = matches!(
            strategy_id.as_str(),
            crate::strategy::STRATEGY_ID_ENDGAME_SWEEP_V1
                | crate::strategy::STRATEGY_ID_SESSIONBAND_V1
                | crate::strategy::STRATEGY_ID_EVSNIPE_V1
        ) || request_force_fak;
        let cross_spread_used = opportunity.use_market_order;
        let post_only_default = Self::entry_post_only_enabled_for_strategy(strategy_id.as_str());
        let post_only_enabled = post_only_default && !cross_spread_used && !use_fak_limit_entry;
        let constraints = if self.simulation_mode {
            None
        } else {
            Some(
                self.market_order_constraints(&opportunity.condition_id)
                    .await?,
            )
        };
        let mut submit_price = opportunity.bid_price;
        let mut post_only_best_bid: Option<f64> = None;
        let mut post_only_best_ask: Option<f64> = None;
        let mut post_only_price_clamped = false;
        if post_only_enabled && !self.simulation_mode {
            let maker_tick = constraints
                .map(|c| c.tick_size)
                .unwrap_or(0.01)
                .max(0.000001);
            match self.api.get_best_price(&opportunity.token_id).await {
                Ok(Some(quote)) => {
                    post_only_best_bid = quote
                        .bid
                        .and_then(|v| f64::try_from(v).ok())
                        .filter(|v| v.is_finite() && *v > 0.0);
                    post_only_best_ask = quote
                        .ask
                        .and_then(|v| f64::try_from(v).ok())
                        .filter(|v| v.is_finite() && *v > 0.0);

                    if let Some(best_bid) = post_only_best_bid {
                        if submit_price > best_bid {
                            submit_price = best_bid;
                            post_only_price_clamped = true;
                        }
                    } else if let Some(best_ask) = post_only_best_ask {
                        if submit_price >= best_ask {
                            let maker_safe = (best_ask - maker_tick).max(0.01);
                            if maker_safe < submit_price {
                                submit_price = maker_safe;
                                post_only_price_clamped = true;
                            }
                        }
                    }
                }
                Ok(None) => {
                    warn!(
                        "post-only enabled but no best bid/ask available for token {} (strategy={}, mode={})",
                        &opportunity.token_id[..opportunity.token_id.len().min(16)],
                        strategy_id,
                        entry_mode.as_str()
                    );
                }
                Err(e) => {
                    warn!(
                        "post-only quote fetch failed for token {} (strategy={}, mode={}): {}",
                        &opportunity.token_id[..opportunity.token_id.len().min(16)],
                        strategy_id,
                        entry_mode.as_str(),
                        e
                    );
                }
            }
        }
        if let Some(c) = constraints {
            submit_price = if post_only_enabled {
                Self::round_price_to_tick_down(submit_price, c.tick_size)
            } else {
                Self::round_price_to_tick(submit_price, c.tick_size)
            };
            if post_only_enabled {
                if let Some(best_bid) = post_only_best_bid {
                    if submit_price > best_bid {
                        let adjusted = Self::round_price_to_tick_down(best_bid, c.tick_size);
                        if adjusted < submit_price {
                            submit_price = adjusted;
                            post_only_price_clamped = true;
                        }
                    }
                }
            }
            if !submit_price.is_finite() || submit_price <= 0.0 {
                let reason = format!(
                    "invalid_order_min_tick_size: non-positive tick-aligned price {:.8}",
                    submit_price
                );
                self.maybe_refresh_market_constraints_for_reason(
                    opportunity.condition_id.as_str(),
                    reason.as_str(),
                    "precheck_price",
                )
                .await;
                self.record_entry_precheck_failure(
                    strategy_id.as_str(),
                    source_timeframe.as_str(),
                    entry_mode,
                    opportunity,
                    Some(submit_price),
                    None,
                    None,
                    reason.clone(),
                );
                return Err(anyhow!(reason));
            }
        }
        let requested_units = size_override.unwrap_or_else(|| fixed_amount / submit_price);
        let (units, mut investment_amount, adjusted_to_minimum) =
            match Self::enforce_min_order_constraints(requested_units, submit_price, constraints) {
                Ok(v) => v,
                Err(reason) => {
                    self.maybe_refresh_market_constraints_for_reason(
                        opportunity.condition_id.as_str(),
                        reason.as_str(),
                        "precheck_constraints",
                    )
                    .await;
                    self.record_entry_precheck_failure(
                        strategy_id.as_str(),
                        source_timeframe.as_str(),
                        entry_mode,
                        opportunity,
                        Some(submit_price),
                        Some(requested_units),
                        Some(requested_units.max(0.0) * submit_price.max(0.0)),
                        reason.clone(),
                    );
                    return Err(anyhow!(reason));
                }
            };
        if adjusted_to_minimum {
            log_event(
                "entry_size_adjusted_to_minimum",
                json!({
                    "strategy_id": strategy_id.as_str(),
                    "timeframe": source_timeframe.as_str(),
                    "entry_mode": entry_mode.as_str(),
                    "period_timestamp": opportunity.period_timestamp,
                    "token_id": opportunity.token_id,
                    "token_type": opportunity.token_type.display_name(),
                    "requested_units": requested_units,
                    "adjusted_units": units,
                    "submit_price": submit_price,
                    "requested_notional_usd": requested_units.max(0.0) * submit_price.max(0.0),
                    "adjusted_notional_usd": investment_amount
                }),
            );
        }
        if entry_mode == EntryExecutionMode::MmRewards {
            let requested_notional_usd = requested_units.max(0.0) * submit_price.max(0.0);
            let base_notional_usd = if requested_notional_usd > 0.0 {
                requested_notional_usd
            } else {
                fixed_amount.max(0.0)
            };
            let mm_notional_hard_cap = (base_notional_usd * 1.05)
                .max(base_notional_usd + 0.50)
                .max(20.0);
            if investment_amount > mm_notional_hard_cap {
                let reason = format!(
                    "mm_rewards_notional_floor_exceeds_budget: target={:.6} adjusted={:.6} cap={:.6}",
                    base_notional_usd, investment_amount, mm_notional_hard_cap
                );
                self.record_entry_precheck_failure(
                    strategy_id.as_str(),
                    source_timeframe.as_str(),
                    entry_mode,
                    opportunity,
                    Some(submit_price),
                    Some(requested_units),
                    Some(investment_amount),
                    reason.clone(),
                );
                log_event(
                    "mm_rewards_skip_notional_floor_budget",
                    json!({
                        "strategy_id": strategy_id.as_str(),
                        "timeframe": source_timeframe.as_str(),
                        "entry_mode": entry_mode.as_str(),
                        "period_timestamp": opportunity.period_timestamp,
                        "condition_id": opportunity.condition_id,
                        "token_id": opportunity.token_id,
                        "submit_price": submit_price,
                        "requested_units": requested_units,
                        "adjusted_units": units,
                        "target_notional_usd": base_notional_usd,
                        "adjusted_notional_usd": investment_amount,
                        "hard_cap_usd": mm_notional_hard_cap
                    }),
                );
                return Ok(());
            }
        }
        let trade_key = Self::limit_trade_key(
            entry_mode,
            opportunity.period_timestamp,
            &opportunity.token_id,
            submit_price,
            source_timeframe.as_str(),
            strategy_id.as_str(),
            request_id.as_deref(),
        );
        let reservation_key = format!(
            "{}:{}:{}",
            entry_mode.as_str(),
            strategy_id.as_str(),
            trade_key
        );
        let Some(_strategy_entry_slot_guard) = EntrySlotGuard::acquire(reservation_key.clone())
        else {
            crate::log_println!(
                "⏸️  Skip duplicate entry: strategy slot reserved mode={} strategy={} tf={} period={} token={}",
                entry_mode.as_str(),
                strategy_id.as_str(),
                source_timeframe,
                opportunity.period_timestamp,
                &opportunity.token_id[..16]
            );
            return Ok(());
        };
        let global_reservation_key = format!(
            "global:buy:{}:{}:{}",
            opportunity.period_timestamp, opportunity.token_id, trade_key
        );
        let Some(_global_entry_slot_guard) =
            EntrySlotGuard::acquire(global_reservation_key.clone())
        else {
            crate::log_println!(
                "⏸️  Skip duplicate entry: global slot reserved strategy={} period={} token={} key={}",
                strategy_id.as_str(),
                opportunity.period_timestamp,
                &opportunity.token_id[..16],
                trade_key
            );
            return Ok(());
        };
        let provisional_order_id = Self::local_pending_order_id(
            entry_mode,
            source_timeframe.as_str(),
            opportunity.period_timestamp,
            &opportunity.token_id,
            trade_key.as_str(),
        );

        {
            let pending = self.pending_trades.lock().await;
            let has_in_memory_pending_buy = pending
                .get(trade_key.as_str())
                .map(|trade| {
                    !trade.sold && !trade.redemption_abandoned && !trade.buy_order_confirmed
                })
                .unwrap_or(false);
            if has_in_memory_pending_buy {
                crate::log_println!(
                    "⏸️  Skip duplicate entry: in-memory pending BUY exists mode={} strategy={} period={} token={} key={}",
                    entry_mode.as_str(),
                    strategy_id.as_str(),
                    opportunity.period_timestamp,
                    &opportunity.token_id[..16],
                    trade_key
                );
                return Ok(());
            }
        }

        // Guard: do not stack multiple entry orders for the same token/timeframe/period.
        // This fixes the observed behavior where repeated monitor snapshots submit many $20 orders
        // before any fill is confirmed, causing total notional to exceed the per-decision cap.
        if !self.simulation_mode {
            if let Some(db) = self.tracking_db.as_ref() {
                if db.has_open_pending_order_for_trade_key(trade_key.as_str(), "BUY")? {
                    crate::log_println!(
                        "⏸️  Skip duplicate entry: pending BUY already open mode={} strategy={} tf={} period={} token={} key={}",
                        entry_mode.as_str(),
                        strategy_id.as_str(),
                        source_timeframe,
                        opportunity.period_timestamp,
                        &opportunity.token_id[..16],
                        trade_key
                    );
                    return Ok(());
                }
            }
        }

        // Only profit target sell price (stop-loss disabled for limit order version)
        let sell_price = self.config.sell_price;

        crate::log_println!("═══════════════════════════════════════════════════════════");
        crate::log_println!("📋 PLACING LIMIT BUY ORDER");
        crate::log_println!("═══════════════════════════════════════════════════════════");
        crate::log_println!("📊 Order Details:");
        crate::log_println!("   Token Type: {}", opportunity.token_type.display_name());
        crate::log_println!("   Token ID: {}", opportunity.token_id);
        crate::log_println!("   Condition ID: {}", opportunity.condition_id);
        crate::log_println!("   Side: BUY (LIMIT)");
        crate::log_println!("   Limit Price: ${:.6}", submit_price);
        crate::log_println!("   Size: {:.6} shares", units);
        crate::log_println!("   Investment Amount: ${:.2}", investment_amount);
        if place_sell_orders {
            crate::log_println!("   When filled, will place limit sell order:");
            crate::log_println!("      - Sell at ${:.6} (profit target)", sell_price);
            crate::log_println!("      - Stop-loss disabled for limit order version");
        } else {
            crate::log_println!(
                "   When filled, no sell orders will be placed (log confirmation only)"
            );
        }
        crate::log_println!(
            "   Time elapsed: {}m {}s",
            opportunity.time_elapsed_seconds / 60,
            opportunity.time_elapsed_seconds % 60
        );
        crate::log_println!("");

        if self.simulation_mode {
            crate::log_println!("🎮 SIMULATION MODE - Limit order NOT placed");
            crate::log_println!("   ✅ SIMULATION: Limit buy order would be placed:");
            crate::log_println!("      - Token: {}", opportunity.token_type.display_name());
            crate::log_println!("      - Price: ${:.6}", submit_price);
            crate::log_println!("      - Size: {:.6} shares", units);
            crate::log_println!("      - Strategy: Hold until market closure (claim $1.00 if winning, $0.00 if losing)");

            // Add to simulation tracker
            if let Some(tracker) = &self.simulation_tracker {
                tracker
                    .add_limit_order(
                        opportunity.token_id.clone(),
                        opportunity.token_type.clone(),
                        opportunity.condition_id.clone(),
                        submit_price,
                        units,
                        "BUY".to_string(),
                        opportunity.period_timestamp,
                    )
                    .await;

                // Also track as pending trade for consistency
                // In simulation mode, we hold positions until market closure (no selling)
                let trade_key = Self::limit_trade_key(
                    entry_mode,
                    opportunity.period_timestamp,
                    &opportunity.token_id,
                    submit_price,
                    source_timeframe.as_str(),
                    strategy_id.as_str(),
                    request_id.as_deref(),
                );
                let mut pending = self.pending_trades.lock().await;
                let trade = PendingTrade {
                    token_id: opportunity.token_id.clone(),
                    condition_id: opportunity.condition_id.clone(),
                    token_type: opportunity.token_type.clone(),
                    investment_amount,
                    units,
                    purchase_price: submit_price,
                    sell_price: self.config.sell_price, // Not used in simulation - positions held until closure
                    timestamp: std::time::Instant::now(),
                    market_timestamp: opportunity.period_timestamp,
                    source_timeframe: source_timeframe.clone(),
                    strategy_id: strategy_id.clone(),
                    entry_mode: entry_mode.as_str().to_string(),
                    order_id: None,
                    end_timestamp: Some(Self::estimate_end_timestamp(
                        opportunity.period_timestamp,
                        source_timeframe.as_str(),
                    )),
                    sold: false,
                    confirmed_balance: None,
                    buy_order_confirmed: false,
                    limit_sell_orders_placed: false, // No sell orders in simulation - hold until closure
                    no_sell: true, // Mark as no_sell since we hold until market closure
                    claim_on_closure: true, // Will claim at market closure
                    sell_attempts: 0,
                    redemption_attempts: 0,
                    redemption_abandoned: false,
                };
                pending.insert(trade_key, trade);
            }

            return Ok(());
        }

        // Place limit buy order
        use crate::models::OrderRequest;
        use rust_decimal::Decimal;

        // Format size to 2 decimal places (API requirement: maximum 2 decimal places)
        let size_formatted = format!("{:.2}", units);
        let price_precision = constraints
            .map(|c| Self::tick_decimal_places(c.tick_size))
            .unwrap_or(2);
        let submit_price_formatted = format!("{:.*}", price_precision, submit_price);

        let mut order = OrderRequest {
            token_id: opportunity.token_id.clone(),
            side: "BUY".to_string(),
            size: size_formatted,
            price: submit_price_formatted,
            order_type: if use_fak_limit_entry {
                "FAK".to_string()
            } else {
                "LIMIT".to_string()
            },
            expiration_ts: if use_fak_limit_entry {
                None
            } else {
                Some(self.limit_order_expiration_ts(
                    opportunity.period_timestamp,
                    source_timeframe.as_str(),
                    entry_mode,
                    strategy_id.as_str(),
                    submit_price,
                ))
            },
            post_only: if post_only_enabled { Some(true) } else { None },
        };

        let submit_ts_ms = chrono::Utc::now().timestamp_millis();
        let submit_instant = std::time::Instant::now();
        self.upsert_pending_order_tracking_for_strategy(
            &provisional_order_id,
            &trade_key,
            &opportunity.token_id,
            source_timeframe.as_str(),
            entry_mode.as_str(),
            Some(strategy_id.as_str()),
            Some(Self::token_family(&opportunity.token_type)),
            opportunity.period_timestamp,
            submit_price,
            investment_amount,
            "BUY",
            "PENDING",
            Some(opportunity.condition_id.as_str()),
        )?;

        let submit_meta = serde_json::json!({
            "phase": "submit",
            "submitted_at_ms": submit_ts_ms,
            "order_kind": "limit_buy",
            "token_id": opportunity.token_id.clone(),
            "trade_key": trade_key.clone(),
            "request_id": request_id.clone(),
            "cross_spread_used": cross_spread_used,
            "post_only": post_only_enabled,
            "post_only_default": post_only_default,
            "post_only_price_clamped": post_only_price_clamped,
            "post_only_best_bid": post_only_best_bid,
            "post_only_best_ask": post_only_best_ask,
            "entry_order_type": order.order_type.as_str(),
        });
        self.record_trade_event_for_strategy(
            Some(strategy_id.as_str()),
            Some(format!(
                "entry_submit:{}:{}:{}:{}",
                opportunity.period_timestamp,
                opportunity.token_id,
                request_id.as_deref().unwrap_or("none"),
                submit_ts_ms
            )),
            opportunity.period_timestamp,
            Some(source_timeframe.as_str()),
            Some(opportunity.condition_id.clone()),
            Some(opportunity.token_id.clone()),
            Some(opportunity.token_type.display_name().to_string()),
            Some("BUY".to_string()),
            "ENTRY_SUBMIT",
            Some(submit_price),
            Some(units),
            Some(investment_amount),
            None,
            Some(submit_meta.to_string()),
        );

        crate::log_println!("🚀 Placing limit buy order on exchange...");
        let api_call_started_ms = chrono::Utc::now().timestamp_millis();
        let mut place_result = self
            .submit_order_with_optional_batch(&order, strategy_id.as_str(), entry_mode)
            .await;
        if let Err(ref err) = place_result {
            if post_only_enabled && !self.simulation_mode && Self::is_post_only_cross_reject(err) {
                let maker_tick = constraints
                    .map(|c| c.tick_size)
                    .unwrap_or(0.01)
                    .max(0.000_001);
                let retry_price = match self.api.get_best_price(&opportunity.token_id).await {
                    Ok(Some(quote)) => {
                        let best_bid = quote
                            .bid
                            .and_then(|v| f64::try_from(v).ok())
                            .filter(|v| v.is_finite() && *v > 0.0);
                        let best_ask = quote
                            .ask
                            .and_then(|v| f64::try_from(v).ok())
                            .filter(|v| v.is_finite() && *v > 0.0);
                        best_bid
                            .map(|bid| Self::round_price_to_tick_down(bid, maker_tick))
                            .or_else(|| {
                                best_ask.map(|ask| {
                                    Self::round_price_to_tick_down(
                                        (ask - maker_tick).max(maker_tick),
                                        maker_tick,
                                    )
                                })
                            })
                            .filter(|v| v.is_finite() && *v > 0.0)
                    }
                    _ => None,
                };
                if let Some(reprice) = retry_price {
                    if (reprice - submit_price).abs() > 1e-9 {
                        submit_price = reprice;
                        investment_amount = units.max(0.0) * submit_price.max(0.0);
                        order.price = format!("{:.*}", price_precision, submit_price);
                        let _ = self.upsert_pending_order_tracking_for_strategy(
                            &provisional_order_id,
                            &trade_key,
                            &opportunity.token_id,
                            source_timeframe.as_str(),
                            entry_mode.as_str(),
                            Some(strategy_id.as_str()),
                            Some(Self::token_family(&opportunity.token_type)),
                            opportunity.period_timestamp,
                            submit_price,
                            investment_amount,
                            "BUY",
                            "PENDING",
                            Some(opportunity.condition_id.as_str()),
                        );
                        log_event(
                            "entry_post_only_reprice_retry",
                            json!({
                                "strategy_id": strategy_id.as_str(),
                                "timeframe": source_timeframe.as_str(),
                                "entry_mode": entry_mode.as_str(),
                                "period_timestamp": opportunity.period_timestamp,
                                "token_id": opportunity.token_id,
                                "old_price": opportunity.bid_price,
                                "retry_price": submit_price
                            }),
                        );
                        place_result = self
                            .submit_order_with_optional_batch(
                                &order,
                                strategy_id.as_str(),
                                entry_mode,
                            )
                            .await;
                    }
                }
            }
        }
        match place_result {
            Ok((response, api_timing)) => {
                let ack_ts_ms = chrono::Utc::now().timestamp_millis();
                let order_ack_latency_ms = ack_ts_ms.saturating_sub(submit_ts_ms);
                let pre_api_ms = api_call_started_ms.saturating_sub(submit_ts_ms);
                let stage_sum_ms = api_timing.get_client_ms
                    + api_timing.prewarm_ms
                    + api_timing.build_order_ms
                    + api_timing.sign_ms
                    + api_timing.post_order_ms
                    + api_timing.backoff_sleep_ms;
                let api_internal_overhead_ms = (api_timing.total_api_ms - stage_sum_ms).max(0);
                let ack_order_id = response
                    .order_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string);
                if ack_order_id.is_none() {
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    let response_status = response.status.trim().to_string();
                    let response_status = if response_status.is_empty() {
                        "unknown".to_string()
                    } else {
                        response_status
                    };
                    let signer_route = api_timing.order_signer_final_route.clone();
                    let window_key = format!(
                        "{}|{}|{}|{}",
                        strategy_id.as_str(),
                        opportunity.token_id,
                        response_status,
                        signer_route
                    );
                    let mut emit_immediate = false;
                    let mut summary_count: Option<u64> = None;
                    if let Ok(mut windows) = entry_ack_empty_order_id_windows().lock() {
                        let is_new_key = !windows.contains_key(window_key.as_str());
                        let entry = windows.entry(window_key.clone()).or_insert_with(|| {
                            EntryAckEmptyOrderIdLogWindow {
                                window_start_ms: now_ms,
                                window_count: 0,
                            }
                        });
                        if is_new_key {
                            emit_immediate = true;
                        }
                        if now_ms.saturating_sub(entry.window_start_ms) >= 60_000 {
                            if entry.window_count > 0 {
                                summary_count = Some(entry.window_count);
                            }
                            entry.window_start_ms = now_ms;
                            entry.window_count = 0;
                        }
                        entry.window_count = entry.window_count.saturating_add(1);
                    }
                    if let Some(count) = summary_count {
                        log_event(
                            "entry_ack_empty_order_id_summary",
                            json!({
                                "strategy_id": strategy_id.as_str(),
                                "token_id": opportunity.token_id,
                                "response_status": response.status.clone(),
                                "api_order_signer_final_route": api_timing.order_signer_final_route.clone(),
                                "window_ms": 60_000,
                                "count": count
                            }),
                        );
                    }
                    if emit_immediate {
                        log_event(
                            "entry_ack_empty_order_id",
                            json!({
                                "strategy_id": strategy_id.as_str(),
                                "timeframe": source_timeframe.as_str(),
                                "entry_mode": entry_mode.as_str(),
                                "period_timestamp": opportunity.period_timestamp,
                                "condition_id": opportunity.condition_id,
                                "token_id": opportunity.token_id,
                                "price": submit_price,
                                "units": units,
                                "size_usd": investment_amount,
                                "request_id": request_id.clone(),
                                "api_order_signer_final_route": api_timing.order_signer_final_route.clone(),
                                "api_order_signer_primary_url": api_timing.order_signer_primary_url.clone(),
                                "api_order_signer_fallback_url": api_timing.order_signer_fallback_url.clone(),
                                "api_order_type_effective": api_timing.order_type_effective.clone(),
                                "response_status": response.status.clone()
                            }),
                        );
                    }
                }
                let tracking_order_id =
                    ack_order_id.unwrap_or_else(|| provisional_order_id.clone());
                let ack_meta = serde_json::json!({
                    "phase": "ack",
                    "submitted_at_ms": submit_ts_ms,
                    "acked_at_ms": ack_ts_ms,
                    "order_ack_latency_ms": order_ack_latency_ms,
                    "order_id": tracking_order_id.clone(),
                    "trade_key": trade_key.clone(),
                    "request_id": request_id.clone(),
                    "pre_api_ms": pre_api_ms,
                    "api_total_ms": api_timing.total_api_ms,
                    "api_attempts_used": api_timing.attempts_used,
                    "api_retry_count": api_timing.retry_count,
                    "api_retry_policy": api_timing.retry_policy,
                    "api_order_type_effective": api_timing.order_type_effective,
                    "api_get_client_ms": api_timing.get_client_ms,
                    "api_prewarm_ms": api_timing.prewarm_ms,
                    "api_prewarm_cache_hit": api_timing.prewarm_cache_hit,
                    "api_build_order_ms": api_timing.build_order_ms,
                    "api_sign_ms": api_timing.sign_ms,
                    "api_build_sign_ms": api_timing.build_sign_ms,
                    "api_post_order_ms": api_timing.post_order_ms,
                    "api_order_signer_primary_url": api_timing.order_signer_primary_url.clone(),
                    "api_order_signer_fallback_url": api_timing.order_signer_fallback_url.clone(),
                    "api_order_signer_final_route": api_timing.order_signer_final_route.clone(),
                    "api_order_signer_primary_post_ms": api_timing.order_signer_primary_post_ms,
                    "api_order_signer_fallback_post_ms": api_timing.order_signer_fallback_post_ms,
                    "api_order_signer_primary_attempts": api_timing.order_signer_primary_attempts,
                    "api_order_signer_fallback_attempts": api_timing.order_signer_fallback_attempts,
                    "api_backoff_sleep_ms": api_timing.backoff_sleep_ms,
                    "api_internal_overhead_ms": api_internal_overhead_ms,
                });
                self.record_trade_event_for_strategy(
                    Some(strategy_id.as_str()),
                    Some(format!(
                        "entry_ack:{}:{}:{}:{}",
                        opportunity.period_timestamp,
                        opportunity.token_id,
                        request_id.as_deref().unwrap_or("none"),
                        ack_ts_ms
                    )),
                    opportunity.period_timestamp,
                    Some(source_timeframe.as_str()),
                    Some(opportunity.condition_id.clone()),
                    Some(opportunity.token_id.clone()),
                    Some(opportunity.token_type.display_name().to_string()),
                    Some("BUY".to_string()),
                    "ENTRY_ACK",
                    Some(submit_price),
                    Some(units),
                    Some(investment_amount),
                    None,
                    Some(ack_meta.to_string()),
                );

                crate::log_println!("   ✅ LIMIT BUY ORDER PLACED");
                crate::log_println!("      Order ID: {:?}", response.order_id);
                crate::log_println!("      Status: {}", response.status);
                if let Some(msg) = &response.message {
                    crate::log_println!("      Message: {}", msg);
                }

                // Store initial balance to detect fills
                let initial_balance = match self.api.check_balance_only(&opportunity.token_id).await
                {
                    Ok(balance) => {
                        let balance_decimal = balance / Decimal::from(1_000_000u64);
                        f64::try_from(balance_decimal).unwrap_or(0.0)
                    }
                    Err(_) => 0.0,
                };

                let mut pending = self.pending_trades.lock().await;

                // Store both sell prices in the trade (we'll use sell_price as the primary one for tracking)
                let trade = PendingTrade {
                    token_id: opportunity.token_id.clone(),
                    condition_id: opportunity.condition_id.clone(),
                    token_type: opportunity.token_type.clone(),
                    investment_amount: investment_amount,
                    units: units, // Expected units
                    purchase_price: submit_price,
                    sell_price, // Primary sell price (profit target)
                    timestamp: submit_instant,
                    market_timestamp: opportunity.period_timestamp,
                    source_timeframe: source_timeframe.clone(),
                    strategy_id: strategy_id.clone(),
                    entry_mode: entry_mode.as_str().to_string(),
                    order_id: Some(tracking_order_id.clone()),
                    end_timestamp: Some(Self::estimate_end_timestamp(
                        opportunity.period_timestamp,
                        source_timeframe.as_str(),
                    )),
                    sold: false,
                    confirmed_balance: Some(initial_balance),
                    buy_order_confirmed: false, // Will be true when limit order fills
                    limit_sell_orders_placed: false, // Will be set to true after placing sell orders
                    no_sell: hold_to_resolution,
                    claim_on_closure: hold_to_resolution,
                    sell_attempts: 0,
                    redemption_attempts: 0,
                    redemption_abandoned: false,
                };

                pending.insert(trade_key.clone(), trade);
                drop(pending);

                self.upsert_pending_order_tracking_for_strategy(
                    &tracking_order_id,
                    &trade_key,
                    &opportunity.token_id,
                    source_timeframe.as_str(),
                    entry_mode.as_str(),
                    Some(strategy_id.as_str()),
                    Some(Self::token_family(&opportunity.token_type)),
                    opportunity.period_timestamp,
                    submit_price,
                    investment_amount,
                    "BUY",
                    "OPEN",
                    Some(opportunity.condition_id.as_str()),
                )?;
                if tracking_order_id != provisional_order_id {
                    self.upsert_pending_order_tracking_for_strategy(
                        &provisional_order_id,
                        &trade_key,
                        &opportunity.token_id,
                        source_timeframe.as_str(),
                        entry_mode.as_str(),
                        Some(strategy_id.as_str()),
                        Some(Self::token_family(&opportunity.token_type)),
                        opportunity.period_timestamp,
                        submit_price,
                        investment_amount,
                        "BUY",
                        "SUPERSEDED",
                        Some(opportunity.condition_id.as_str()),
                    )?;
                }

                if place_sell_orders {
                    crate::log_println!(
                        "   📝 Order tracked - will monitor for fill and place limit sell order:"
                    );
                    crate::log_println!("      - Sell at ${:.6} (profit target)", sell_price);
                    crate::log_println!("      - Stop-loss disabled for limit order version");
                } else {
                    crate::log_println!(
                        "   📝 Order tracked - will monitor for fill and log confirmation only"
                    );
                }
                crate::log_println!("   💡 Initial balance: {:.6} shares", initial_balance);

                // Log the limit order event
                let order_id_str = tracking_order_id.clone();
                let buy_event = format!(
                    "LIMIT BUY ORDER | Market: {} | Period: {} | Token: {} | Limit Price: ${:.6} | Size: {:.6} | Order ID: {} | Target Sell: ${:.6} (profit only, stop-loss disabled)",
                    opportunity.token_type.display_name(),
                    opportunity.period_timestamp,
                    &opportunity.token_id[..16],
                    submit_price,
                    units,
                    order_id_str,
                    sell_price
                );
                crate::log_trading_event(&buy_event);
            }
            Err(e) => {
                if use_fak_limit_entry && Self::is_fak_no_match_error(&e) {
                    let no_fill_ts_ms = chrono::Utc::now().timestamp_millis();
                    self.record_trade_event_for_strategy(
                        Some(strategy_id.as_str()),
                        Some(format!(
                            "entry_no_fill:{}:{}:{}:{}",
                            opportunity.period_timestamp,
                            opportunity.token_id,
                            request_id.as_deref().unwrap_or("none"),
                            no_fill_ts_ms
                        )),
                        opportunity.period_timestamp,
                        Some(source_timeframe.as_str()),
                        Some(opportunity.condition_id.clone()),
                        Some(opportunity.token_id.clone()),
                        Some(opportunity.token_type.display_name().to_string()),
                        Some("BUY".to_string()),
                        "ENTRY_NO_FILL",
                        Some(opportunity.bid_price),
                        Some(units),
                        Some(investment_amount),
                        None,
                        Some("fak_no_match".to_string()),
                    );
                    if let Err(track_err) = self.upsert_pending_order_tracking_for_strategy(
                        &provisional_order_id,
                        &trade_key,
                        &opportunity.token_id,
                        source_timeframe.as_str(),
                        entry_mode.as_str(),
                        Some(strategy_id.as_str()),
                        Some(Self::token_family(&opportunity.token_type)),
                        opportunity.period_timestamp,
                        opportunity.bid_price,
                        investment_amount,
                        "BUY",
                        "ENTRY_NO_FILL",
                        Some(opportunity.condition_id.as_str()),
                    ) {
                        warn!(
                            "Failed to write ENTRY_NO_FILL pending tracking for {}: {}",
                            provisional_order_id, track_err
                        );
                    }
                    log_event(
                        "entry_no_fill_fak",
                        json!({
                            "strategy_id": strategy_id,
                            "timeframe": source_timeframe,
                            "entry_mode": entry_mode.as_str(),
                            "period_timestamp": opportunity.period_timestamp,
                            "token_id": opportunity.token_id,
                            "token_type": opportunity.token_type.display_name(),
                            "request_id": request_id,
                            "price": opportunity.bid_price,
                            "units": units,
                            "notional_usd": investment_amount,
                            "reason": "no_orders_found_to_match_fak"
                        }),
                    );
                    return Ok(());
                }
                let submit_error = e.to_string();
                self.maybe_refresh_market_constraints_for_reason(
                    opportunity.condition_id.as_str(),
                    submit_error.as_str(),
                    "submit_failed",
                )
                .await;
                let failed_ts_ms = chrono::Utc::now().timestamp_millis();
                self.record_trade_event_for_strategy(
                    Some(strategy_id.as_str()),
                    Some(format!(
                        "entry_failed:{}:{}:{}:{}",
                        opportunity.period_timestamp,
                        opportunity.token_id,
                        request_id.as_deref().unwrap_or("none"),
                        failed_ts_ms
                    )),
                    opportunity.period_timestamp,
                    Some(source_timeframe.as_str()),
                    Some(opportunity.condition_id.clone()),
                    Some(opportunity.token_id.clone()),
                    Some(opportunity.token_type.display_name().to_string()),
                    Some("BUY".to_string()),
                    "ENTRY_FAILED",
                    Some(opportunity.bid_price),
                    Some(units),
                    Some(investment_amount),
                    None,
                    Some(Self::classify_entry_failure_reason(&e)),
                );
                if let Err(track_err) = self.upsert_pending_order_tracking_for_strategy(
                    &provisional_order_id,
                    &trade_key,
                    &opportunity.token_id,
                    source_timeframe.as_str(),
                    entry_mode.as_str(),
                    Some(strategy_id.as_str()),
                    Some(Self::token_family(&opportunity.token_type)),
                    opportunity.period_timestamp,
                    opportunity.bid_price,
                    investment_amount,
                    "BUY",
                    "ENTRY_FAILED",
                    Some(opportunity.condition_id.as_str()),
                ) {
                    warn!(
                        "Failed to write ENTRY_FAILED pending tracking for {}: {}",
                        provisional_order_id, track_err
                    );
                }
                eprintln!("   ❌ FAILED TO PLACE LIMIT BUY ORDER: {}", e);
                return Err(e);
            }
        }

        Ok(())
    }

    /// Check pending trades and sell when price reaches sell_price (0.99 or 1.0)
    /// Also handles limit order fills: detects when limit buy orders fill and places limit sell orders
    pub async fn check_pending_trades(&self) -> Result<()> {
        // In simulation mode, check limit orders against current prices
        if self.simulation_mode {
            if let Some(tracker) = &self.simulation_tracker {
                // Log that we're checking
                tracker
                    .log_to_file("🔄 SIMULATION: check_pending_trades called")
                    .await;
                let sim_pending_max_age_secs =
                    std::env::var("EVPOLY_SIM_PENDING_ORDER_MAX_AGE_SEC")
                        .ok()
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(900);
                let no_orderbook_drop_age_secs =
                    std::env::var("EVPOLY_SIM_NO_ORDERBOOK_DROP_AGE_SEC")
                        .ok()
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(120);

                let stale_removed = tracker
                    .prune_stale_pending_orders(sim_pending_max_age_secs)
                    .await;
                if stale_removed > 0 {
                    tracker
                        .log_to_file(&format!(
                            "🧹 SIMULATION: Pruned {} stale pending order(s) older than {}s",
                            stale_removed, sim_pending_max_age_secs
                        ))
                        .await;
                }

                // Get current prices for all tokens we're tracking
                let mut current_prices = HashMap::new();

                // In simulation mode, use simulation tracker's pending orders as source of truth
                // Get token IDs from pending limit orders in simulation tracker
                let pending_order_token_ids = tracker.get_pending_order_token_ids().await;
                let pending_order_count = tracker.get_pending_order_count().await;

                tracker
                    .log_to_file(&format!(
                        "📊 SIMULATION: Found {} pending limit order(s) waiting for fills\n",
                        pending_order_count
                    ))
                    .await;

                // Collect all unique token IDs we need prices for (from simulation tracker only)
                let mut token_ids_to_fetch = std::collections::HashSet::new();
                for token_id in &pending_order_token_ids {
                    token_ids_to_fetch.insert(token_id.clone());
                }

                if token_ids_to_fetch.is_empty() {
                    tracker
                        .log_to_file(
                            "⚠️  SIMULATION: No token IDs to fetch prices for (still syncing filled positions)",
                        )
                        .await;
                } else {
                    tracker
                        .log_to_file(&format!(
                            "🔍 SIMULATION: Fetching prices for {} unique token(s)\n",
                            token_ids_to_fetch.len()
                        ))
                        .await;

                    // Fetch prices for all tokens
                    for token_id in token_ids_to_fetch {
                        if !current_prices.contains_key(&token_id) {
                            // Fetch both sides from /price so fill checks use same source as entry logic.
                            let buy_price = self
                                .fetch_price_with_retry_for_sim(&token_id, "BUY", tracker)
                                .await;

                            let sell_price = self
                                .fetch_price_with_retry_for_sim(&token_id, "SELL", tracker)
                                .await;

                            if buy_price.is_some() || sell_price.is_some() {
                                let sell_missing = sell_price.is_none();
                                let token_price = TokenPrice {
                                    token_id: token_id.clone(),
                                    bid: buy_price, // BUY price path (existing project convention)
                                    ask: sell_price, // SELL price path (used by fill checks for BUY orders)
                                };
                                current_prices.insert(token_id.clone(), token_price);

                                if sell_missing {
                                    tracker
                                        .log_to_file(&format!(
                                            "⚠️  SIMULATION: No ask price available for token {} (BUY orders may not fill)",
                                            &token_id[..16]
                                        ))
                                        .await;
                                }
                            } else {
                                let removed = tracker
                                    .remove_pending_orders_for_token(
                                        &token_id,
                                        no_orderbook_drop_age_secs,
                                    )
                                    .await;
                                if removed > 0 {
                                    tracker
                                        .log_to_file(&format!(
                                            "🧹 SIMULATION: Removed {} pending order(s) for token {} due to unavailable BUY/SELL prices (age >= {}s)",
                                            removed,
                                            &token_id[..16],
                                            no_orderbook_drop_age_secs
                                        ))
                                        .await;
                                } else {
                                    tracker
                                        .log_to_file(&format!(
                                            "⚠️  SIMULATION: No BUY/SELL prices for token {} (keeping fresh orders < {}s)",
                                            &token_id[..16],
                                            no_orderbook_drop_age_secs
                                        ))
                                        .await;
                                }
                            }
                        }
                    }

                    // Log how many prices we fetched
                    if current_prices.is_empty() {
                        tracker
                            .log_to_file("⚠️  SIMULATION: No prices fetched for any tokens")
                            .await;
                    } else {
                        tracker
                            .log_to_file(&format!(
                                "✅ SIMULATION: Fetched prices for {} token(s)\n",
                                current_prices.len()
                            ))
                            .await;
                    }

                    // Check limit orders against current prices
                    tracker.check_limit_orders(&current_prices).await;

                    // Log pending orders summary every check (to see what's happening)
                    tracker.log_pending_orders_summary(&current_prices).await;
                }

                // Check if any simulated fills created new positions that need sell orders
                let pending_trades_after: Vec<(String, PendingTrade)> = {
                    let pending = self.pending_trades.lock().await;
                    pending
                        .iter()
                        .map(|(key, trade)| (key.clone(), trade.clone()))
                        .collect()
                };

                for (key, trade) in &pending_trades_after {
                    // Check if this trade has a filled position (for both limit and market orders)
                    if !trade.buy_order_confirmed && !trade.sold {
                        // Check if the position exists in simulation tracker (order was filled)
                        if tracker
                            .has_position_match(
                                &trade.token_id,
                                trade.market_timestamp,
                                trade.purchase_price,
                            )
                            .await
                        {
                            let mut entry_event_data: Option<(
                                u64,
                                String,
                                String,
                                String,
                                String,
                                f64,
                                f64,
                            )> = None;
                            // Order was filled in simulation - update trade status
                            // In simulation mode, we hold positions until market closure (no selling)
                            let mut pending = self.pending_trades.lock().await;
                            if let Some(t) = pending.get_mut(key.as_str()) {
                                t.buy_order_confirmed = true;
                                t.confirmed_balance = Some(t.units);
                                entry_event_data = Some((
                                    t.market_timestamp,
                                    t.source_timeframe.clone(),
                                    t.condition_id.clone(),
                                    t.token_id.clone(),
                                    t.token_type.display_name().to_string(),
                                    t.purchase_price,
                                    t.units,
                                ));

                                tracker.log_to_file(&format!(
                                    "✅ SIMULATION: Position confirmed for {} - holding until market closure (will claim at $1.00 if winning, $0.00 if losing)",
                                    trade.token_type.display_name()
                                )).await;
                            }
                            drop(pending);
                            if let Some((
                                market_timestamp,
                                source_timeframe,
                                condition_id,
                                token_id,
                                token_type,
                                purchase_price,
                                units,
                            )) = entry_event_data
                            {
                                let simulation_order_to_fill_latency_ms =
                                    i64::try_from(trade.timestamp.elapsed().as_millis())
                                        .unwrap_or(i64::MAX);
                                let fill_meta = serde_json::json!({
                                    "mode": "simulation_limit_buy_fill",
                                    "order_to_fill_latency_ms": simulation_order_to_fill_latency_ms,
                                });
                                self.on_order_filled(FillTransition {
                                    order_id: trade.order_id.clone(),
                                    trade_key: Some(key.clone()),
                                    strategy_id: Some(trade.strategy_id.clone()),
                                    period_timestamp: market_timestamp,
                                    timeframe: Some(source_timeframe.clone()),
                                    condition_id: Some(condition_id),
                                    token_id,
                                    token_type: Some(token_type),
                                    side: Some("BUY".to_string()),
                                    price: Some(purchase_price),
                                    units: Some(units),
                                    notional_usd: Some(units * purchase_price),
                                    reason: fill_meta.to_string(),
                                    reconciled: false,
                                });
                            }
                        }
                    }
                }

                // Note: In simulation mode, we don't create sell orders
                // Positions will be resolved at market closure ($1 for winning tokens, $0 for losing tokens)
            }
        }

        if let Err(e) = self.cancel_expired_ladder_orders().await {
            warn!("Failed to cancel expired ladder orders: {}", e);
        }
        if Self::should_run_exchange_reconcile() {
            if let Err(e) = self.reconcile_active_pending_orders_with_exchange().await {
                warn!("Periodic exchange pending-order reconcile failed: {}", e);
            }
        }

        let pending_trades: Vec<(String, PendingTrade)> = {
            let pending = self.pending_trades.lock().await;
            pending
                .iter()
                .map(|(key, trade)| (key.clone(), trade.clone()))
                .collect()
        };

        if pending_trades.is_empty() {
            return Ok(());
        }

        // Premarket TP is handled by a dedicated worker queue so API retries/timeouts
        // never block this main pending-trade loop or other strategy flows.
        self.enqueue_due_premarket_tp_jobs(pending_trades.as_slice())
            .await;

        // First, check for limit buy order fills
        for (key, trade) in &pending_trades {
            // Skip if already sold or already confirmed
            if trade.sold || trade.buy_order_confirmed {
                continue;
            }

            // Endgame submits can have multiple concurrent orders on the same token/period.
            // Balance-delta attribution is not order-specific and can misattribute fills across ticks.
            // Use order-level exchange reconcile/cancel paths for endgame fill accounting.
            if EntryExecutionMode::parse(trade.entry_mode.as_str()).is_endgame()
                || key.contains("_endgame_")
            {
                continue;
            }

            // For real exchange orders, avoid token-balance delta attribution entirely.
            // It is not order-specific and can over-count fills when multiple orders are live
            // on the same token (e.g. ladder strategies). Exchange reconcile/get_order is canonical.
            let has_exchange_order_id = trade
                .order_id
                .as_deref()
                .map(Self::is_exchange_order_id)
                .unwrap_or(false);
            if !self.simulation_mode && has_exchange_order_id {
                continue;
            }

            // Check if this is a limit order (key contains "_limit")
            if !Self::is_limit_style_key(key) {
                continue;
            }

            // Check current balance to detect fill
            use rust_decimal::Decimal;
            let current_balance = match self.api.check_balance_only(&trade.token_id).await {
                Ok(balance) => {
                    let balance_decimal = balance / Decimal::from(1_000_000u64);
                    f64::try_from(balance_decimal).unwrap_or(0.0)
                }
                Err(_) => continue,
            };

            // Get initial balance from trade
            let initial_balance = trade.confirmed_balance.unwrap_or(0.0);

            // If balance increased, limit buy order filled
            if current_balance > initial_balance + 0.000001 {
                // Small threshold to account for rounding
                crate::log_println!("═══════════════════════════════════════════════════════════");
                crate::log_println!("✅ LIMIT BUY ORDER FILLED");
                crate::log_println!("═══════════════════════════════════════════════════════════");
                crate::log_println!("📊 Fill Details:");
                crate::log_println!("   Token Type: {}", trade.token_type.display_name());
                crate::log_println!("   Token ID: {}", trade.token_id);
                crate::log_println!("   Initial Balance: {:.6} shares", initial_balance);
                crate::log_println!("   Current Balance: {:.6} shares", current_balance);
                crate::log_println!(
                    "   Filled Amount: {:.6} shares",
                    current_balance - initial_balance
                );
                crate::log_println!("   Purchase Price: ${:.6}", trade.purchase_price);
                crate::log_println!("   Target Sell Price: ${:.6}", trade.sell_price);
                crate::log_println!("");

                let filled_units = (current_balance - initial_balance).max(0.0);
                let order_to_fill_latency_ms =
                    i64::try_from(trade.timestamp.elapsed().as_millis()).unwrap_or(i64::MAX);
                let fill_meta = serde_json::json!({
                    "mode": "limit_buy_fill",
                    "order_to_fill_latency_ms": order_to_fill_latency_ms,
                });

                // Update trade with confirmed balance
                {
                    let mut pending = self.pending_trades.lock().await;
                    if let Some(t) = pending.get_mut(key.as_str()) {
                        t.confirmed_balance = Some(current_balance);
                        t.units = current_balance; // Update units to actual filled amount
                        t.buy_order_confirmed = true;
                    }
                }
                self.on_order_filled(FillTransition {
                    order_id: trade.order_id.clone(),
                    trade_key: Some(key.clone()),
                    strategy_id: Some(trade.strategy_id.clone()),
                    period_timestamp: trade.market_timestamp,
                    timeframe: Some(trade.source_timeframe.clone()),
                    condition_id: Some(trade.condition_id.clone()),
                    token_id: trade.token_id.clone(),
                    token_type: Some(trade.token_type.display_name().to_string()),
                    side: Some("BUY".to_string()),
                    price: Some(trade.purchase_price),
                    units: Some(filled_units),
                    notional_usd: Some(filled_units * trade.purchase_price),
                    reason: fill_meta.to_string(),
                    reconciled: false,
                });

                // Hold-to-resolution policy: log confirmation only and skip active exits.
                if self.should_hold_trade_to_resolution(trade) {
                    crate::log_println!(
                        "✅ No-sell mode: confirmation logged, no sell orders will be placed."
                    );
                    self.enforce_trade_hold_to_resolution(key.as_str(), "limit buy fill")
                        .await;
                    continue;
                }

                if !self.should_place_resting_tp_after_fill(trade) {
                    crate::log_println!(
                        "ℹ️  Reactive mode: skipping resting TP placement after fill (managed by active exits)."
                    );
                    continue;
                }

                if Self::should_defer_to_premarket_tp_worker(trade) {
                    crate::log_println!(
                        "ℹ️  Premarket TP worker: deferred TP placement (strategy={}, timeframe={})",
                        trade.strategy_id,
                        trade.source_timeframe
                    );
                    continue;
                }

                // Place limit sell order immediately when active exits are enabled.
                if !self.simulation_mode {
                    crate::log_println!(
                        "📤 Placing limit sell order at ${:.6}...",
                        trade.sell_price
                    );

                    use crate::models::OrderRequest;
                    let sell_order = OrderRequest {
                        token_id: trade.token_id.clone(),
                        side: "SELL".to_string(),
                        size: current_balance.to_string(),
                        price: trade.sell_price.to_string(),
                        order_type: "LIMIT".to_string(),
                        expiration_ts: None,
                        post_only: None,
                    };

                    match self.api.place_order(&sell_order).await {
                        Ok(response) => {
                            crate::log_println!("   ✅ LIMIT SELL ORDER PLACED");
                            crate::log_println!("      Order ID: {:?}", response.order_id);
                            crate::log_println!("      Limit Price: ${:.6}", trade.sell_price);
                            crate::log_println!("      Size: {:.6} shares", current_balance);

                            let order_id_str = response
                                .order_id
                                .as_ref()
                                .map(|id| format!("{:?}", id))
                                .unwrap_or_else(|| "N/A".to_string());
                            let sell_event = format!(
                                "LIMIT SELL ORDER | Market: {} | Period: {} | Token: {} | Limit Price: ${:.6} | Size: {:.6} | Order ID: {}",
                                trade.token_type.display_name(),
                                trade.market_timestamp,
                                &trade.token_id[..16],
                                trade.sell_price,
                                current_balance,
                                order_id_str
                            );
                            crate::log_trading_event(&sell_event);
                        }
                        Err(e) => {
                            eprintln!("   ❌ FAILED TO PLACE LIMIT SELL ORDER: {}", e);
                            warn!("Failed to place limit sell order after buy fill: {}", e);
                        }
                    }
                } else {
                    crate::log_println!(
                        "🎮 SIMULATION: Limit sell order would be placed at ${:.6}",
                        trade.sell_price
                    );
                }
            }
        }

        // Second: Check for market buys that are confirmed and place active exits when enabled.
        // Market buys are stored with buy_order_confirmed: true immediately after confirmation.
        for (key, trade) in &pending_trades {
            // Skip if already sold, not confirmed, or sell orders already placed
            if trade.sold || !trade.buy_order_confirmed || trade.limit_sell_orders_placed {
                continue;
            }

            // Skip limit orders (they're handled above) - only process market buys
            if Self::is_limit_style_key(key) {
                continue;
            }

            // Get current balance
            use rust_decimal::Decimal;
            let current_balance = match self.api.check_balance_only(&trade.token_id).await {
                Ok(balance) => {
                    let balance_decimal = balance / Decimal::from(1_000_000u64);
                    f64::try_from(balance_decimal).unwrap_or(0.0)
                }
                Err(_) => continue,
            };

            // Only proceed if we have tokens
            if current_balance < 0.000001 {
                continue;
            }

            // Place limit sell order for bought token at sell_price (no hedge limit buy)
            if !self.simulation_mode {
                if self.should_hold_trade_to_resolution(trade) {
                    crate::log_println!(
                        "✅ Hold-to-resolution mode: skipping market-buy sell placement."
                    );
                    self.enforce_trade_hold_to_resolution(key.as_str(), "market buy confirmed")
                        .await;
                    continue;
                }

                if !self.should_place_resting_tp_after_fill(trade) {
                    crate::log_println!(
                        "ℹ️  Reactive mode: skipping resting TP placement for market buy (managed by active exits)."
                    );
                    continue;
                }

                if Self::should_defer_to_premarket_tp_worker(trade) {
                    crate::log_println!(
                        "ℹ️  Premarket TP worker: deferred TP placement (strategy={}, timeframe={})",
                        trade.strategy_id,
                        trade.source_timeframe
                    );
                    continue;
                }

                let sell_price = self.config.sell_price;

                crate::log_println!("═══════════════════════════════════════════════════════════");
                crate::log_println!("📤 PLACING ORDER AFTER MARKET BUY");
                crate::log_println!("═══════════════════════════════════════════════════════════");
                crate::log_println!("📊 Order Details:");
                crate::log_println!("   Bought Token: {}", trade.token_type.display_name());
                crate::log_println!("   Token ID: {}", trade.token_id);
                crate::log_println!("   Current Balance: {:.6} shares", current_balance);
                crate::log_println!("");
                crate::log_println!(
                    "   📋 Placing limit SELL for {} at ${:.6} (profit target)",
                    trade.token_type.display_name(),
                    sell_price
                );
                crate::log_println!("");

                use crate::models::OrderRequest;

                // Place limit sell order for bought token at sell_price
                let sell_order_profit = OrderRequest {
                    token_id: trade.token_id.clone(),
                    side: "SELL".to_string(),
                    size: format!("{:.2}", current_balance), // Format to 2 decimal places
                    price: format!("{:.2}", sell_price),     // Format to 2 decimal places
                    order_type: "LIMIT".to_string(),
                    expiration_ts: None,
                    post_only: None,
                };

                match self.api.place_order(&sell_order_profit).await {
                    Ok(response) => {
                        crate::log_println!("   ✅ LIMIT SELL ORDER #1 PLACED (Profit Target)");
                        crate::log_println!("      Token: {}", trade.token_type.display_name());
                        crate::log_println!("      Order ID: {:?}", response.order_id);
                        crate::log_println!("      Limit Price: ${:.6}", sell_price);
                        crate::log_println!("      Size: {:.6} shares", current_balance);

                        let order_id_str = response
                            .order_id
                            .as_ref()
                            .map(|id| format!("{:?}", id))
                            .unwrap_or_else(|| "N/A".to_string());
                        let sell_event = format!(
                            "LIMIT SELL ORDER (PROFIT) | Market: {} | Period: {} | Token: {} | Limit Price: ${:.6} | Size: {:.6} | Order ID: {}",
                            trade.token_type.display_name(),
                            trade.market_timestamp,
                            &trade.token_id[..16],
                            sell_price,
                            current_balance,
                            order_id_str
                        );
                        crate::log_trading_event(&sell_event);
                    }
                    Err(e) => {
                        eprintln!("   ❌ FAILED TO PLACE LIMIT SELL ORDER: {}", e);
                        warn!(
                            "Failed to place limit sell order (profit) after market buy: {}",
                            e
                        );
                    }
                }

                // Mark that sell order has been placed
                {
                    let mut pending = self.pending_trades.lock().await;
                    if let Some(t) = pending.get_mut(key.as_str()) {
                        t.limit_sell_orders_placed = true;
                    }
                }
            } else {
                crate::log_println!("🎮 SIMULATION: One order would be placed for market buy:");
                crate::log_println!(
                    "   Limit SELL for {} at ${:.6} (profit target) - {:.6} shares",
                    trade.token_type.display_name(),
                    self.config.sell_price,
                    current_balance
                );
            }
        }

        let pending_trade_keys: Vec<String> =
            pending_trades.iter().map(|(key, _)| key.clone()).collect();

        // Check for limit sell order fills (detect when balance drops to 0 after placing sell orders)
        // This allows re-entry if price recovers after a stop-loss fill.
        for key in &pending_trade_keys {
            let trade = {
                let pending = self.pending_trades.lock().await;
                pending.get(key.as_str()).cloned()
            };
            let Some(trade) = trade else {
                continue;
            };
            // Skip if already marked as sold or if buy order not confirmed yet
            if trade.sold || !trade.buy_order_confirmed {
                continue;
            }

            // Only check trades that have limit sell orders placed
            // This includes both regular limit sell orders and opposite token limit sell orders
            if !trade.limit_sell_orders_placed {
                continue;
            }

            // Check current balance - if it dropped to 0, a sell order filled
            use rust_decimal::Decimal;
            let current_balance = match self.api.check_balance_only(&trade.token_id).await {
                Ok(balance) => {
                    let balance_decimal = balance / Decimal::from(1_000_000u64);
                    f64::try_from(balance_decimal).unwrap_or(0.0)
                }
                Err(_) => continue,
            };

            // Get last known balance from trade
            let last_balance = trade.confirmed_balance.unwrap_or(0.0);

            // If balance dropped to 0 (or near 0), a sell order filled
            if last_balance > 0.000001 && current_balance < 0.000001 {
                // Determine if this is an opposite token trade
                let is_opposite_token = key.contains("_opposite_");
                let trade_description = if is_opposite_token {
                    "OPPOSITE TOKEN LIMIT SELL ORDER"
                } else {
                    "LIMIT SELL ORDER"
                };

                crate::log_println!("═══════════════════════════════════════════════════════════");
                crate::log_println!("✅ {} FILLED", trade_description);
                crate::log_println!("═══════════════════════════════════════════════════════════");
                crate::log_println!("📊 Sell Fill Details:");
                crate::log_println!("   Token Type: {}", trade.token_type.display_name());
                crate::log_println!("   Token ID: {}", trade.token_id);
                crate::log_println!("   Last Balance: {:.6} shares", last_balance);
                crate::log_println!("   Current Balance: {:.6} shares", current_balance);
                crate::log_println!("   Purchase Price: ${:.6}", trade.purchase_price);
                crate::log_println!("   Sell Price: ${:.6}", trade.sell_price);
                crate::log_println!("   Shares Sold: {:.6}", last_balance);
                crate::log_println!("   Revenue: ${:.6}", last_balance * trade.sell_price);
                crate::log_println!("   Status: FILLED");
                crate::log_println!("");

                // Mark trade as sold
                {
                    let mut pending = self.pending_trades.lock().await;
                    if let Some(t) = pending.get_mut(key.as_str()) {
                        t.sold = true;
                        t.confirmed_balance = Some(0.0);
                    }
                }

                // Log the sell fill event
                let sell_event = format!(
                    "{} FILLED | Market: {} | Period: {} | Token: {} | Purchase Price: ${:.6} | Sell Price: ${:.6} | Shares Sold: {:.6} | Revenue: ${:.6} | Status: FILLED",
                    trade_description,
                    trade.token_type.display_name(),
                    trade.market_timestamp,
                    &trade.token_id[..16],
                    trade.purchase_price,
                    trade.sell_price,
                    last_balance,
                    last_balance * trade.sell_price
                );
                crate::log_trading_event(&sell_event);
            }
        }

        // Continue with regular sell checks for filled orders.
        for key in &pending_trade_keys {
            let Some(mut trade) = ({
                let pending = self.pending_trades.lock().await;
                pending.get(key.as_str()).cloned()
            }) else {
                continue;
            };
            // Skip if already sold
            if trade.sold {
                continue;
            }

            if self.should_hold_trade_to_resolution(&trade) {
                self.enforce_trade_hold_to_resolution(key.as_str(), "active exit monitor guard")
                    .await;
                continue;
            }

            // Get current ASK price (what we receive when selling)
            // Also check if there are actual buyers in the orderbook before attempting to sell
            let price_result = self.api.get_price(&trade.token_id, "SELL").await;
            let current_ask_price = match price_result {
                Ok(p) => {
                    let price_f64 = f64::try_from(p).unwrap_or(0.0);
                    // Log price check every 10th time to avoid spam (or use debug level)
                    debug!("Checking {} token {} ASK price: ${:.6} (target: ${:.6}, purchased at: ${:.6})", 
                           trade.token_type.display_name(), &trade.token_id[..16], price_f64, trade.sell_price, trade.purchase_price);
                    price_f64
                }
                Err(e) => {
                    debug!(
                        "Failed to get ASK price for {} token {}: {}",
                        trade.token_type.display_name(),
                        &trade.token_id[..16],
                        e
                    );
                    continue; // Skip if can't get price
                }
            };

            // Check orderbook to verify there are actual buyers before attempting to sell
            // This prevents "No opposing orders" errors when there's no liquidity
            let has_liquidity = match self.api.get_best_price(&trade.token_id).await {
                Ok(Some(token_price)) => {
                    // Check if there are actual bids (buyers) in the orderbook
                    token_price.bid.is_some()
                }
                Ok(None) => {
                    debug!(
                        "No orderbook data for {} token {} - skipping sell attempt",
                        trade.token_type.display_name(),
                        &trade.token_id[..16]
                    );
                    false
                }
                Err(e) => {
                    debug!(
                        "Failed to check orderbook for {} token {}: {} - will try anyway",
                        trade.token_type.display_name(),
                        &trade.token_id[..16],
                        e
                    );
                    true // Assume liquidity exists if we can't check
                }
            };

            if !has_liquidity {
                debug!(
                    "Skipping sell for {} token {} - no buyers in orderbook (no liquidity)",
                    trade.token_type.display_name(),
                    &trade.token_id[..16]
                );
                continue; // Skip this trade - no liquidity to sell into
            }

            // Skip if already marked for claim (will be handled at market closure)
            if trade.claim_on_closure {
                continue;
            }

            let current_timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let market_end_timestamp = Self::market_end_timestamp(&trade);
            let time_remaining = market_end_timestamp.saturating_sub(current_timestamp);
            let period_duration = Self::timeframe_duration_seconds(&trade.source_timeframe).max(1);
            let elapsed = period_duration.saturating_sub(time_remaining.min(period_duration));
            let theta_pressure = (elapsed as f64 / period_duration as f64).clamp(0.0, 1.0);
            let force_flat_window = Self::theta_force_flat_window(theta_pressure);

            // Second-level fast reaction: impulse/whale adverse flow confirmation -> immediate flatten
            if let Some(reason) = self.reaction_exit_reason(&trade).await {
                if self
                    .force_close_trade_now(&key, &mut trade, current_ask_price, &reason, true)
                    .await
                {
                    continue;
                }
            }

            if self
                .maybe_distribution_exit(
                    &key,
                    &mut trade,
                    current_ask_price,
                    theta_pressure,
                    time_remaining,
                )
                .await
            {
                continue;
            }

            // Theta-pressure overlay: flatten near close earlier as decay pressure increases
            if theta_pressure >= 0.80
                && time_remaining <= force_flat_window
                && current_ask_price >= trade.purchase_price * 0.98
            {
                let reason = format!(
                    "theta force-flat (pressure {:.2}, remaining {}s <= {}s)",
                    theta_pressure, time_remaining, force_flat_window
                );
                if self
                    .force_close_trade_now(&key, &mut trade, current_ask_price, &reason, false)
                    .await
                {
                    continue;
                }
            }

            // OPPOSITE TOKEN STOP-LOSS: Check if opposite token price drops below (1 - stop_loss_price - 0.1)
            // This protects against losses if the opposite token price crashes
            if key.contains("_opposite_") {
                if let Some(stop_loss_price) = self.stop_loss_price_clamped() {
                    let opposite_stop_loss_price = (1.0 - stop_loss_price) - 0.1; // e.g., (1.0 - 0.80) - 0.1 = 0.10

                    // Check if price dropped below opposite token stop-loss threshold
                    if current_ask_price <= opposite_stop_loss_price {
                        // CRITICAL: Re-check actual balance before selling
                        let actual_balance = match self
                            .api
                            .check_balance_only(&trade.token_id)
                            .await
                        {
                            Ok(balance) => {
                                let balance_decimal =
                                    balance / rust_decimal::Decimal::from(1_000_000u64);
                                let balance_f64 = f64::try_from(balance_decimal).unwrap_or(0.0);
                                if balance_f64 > 0.0
                                    && (trade.units == 0.0 || balance_f64 > trade.units)
                                {
                                    balance_f64
                                } else {
                                    trade.units
                                }
                            }
                            Err(e) => {
                                warn!("⚠️  Failed to re-check balance before opposite token stop-loss sell: {}. Using stored units: {:.6}", e, trade.units);
                                trade.units
                            }
                        };

                        if actual_balance == 0.0 {
                            warn!("⚠️  Skipping opposite token stop-loss sell - actual balance is 0.000000 shares.");
                            continue;
                        }

                        let units_to_sell = actual_balance;

                        crate::log_println!(
                            "═══════════════════════════════════════════════════════════"
                        );
                        crate::log_println!("🛑 OPPOSITE TOKEN STOP-LOSS TRIGGERED");
                        crate::log_println!(
                            "═══════════════════════════════════════════════════════════"
                        );
                        crate::log_println!(
                            "⚠️  Opposite token price dropped below stop-loss threshold!"
                        );
                        crate::log_println!("💰 Current ASK Price: ${:.6}", current_ask_price);
                        crate::log_println!(
                            "🛑 Opposite Token Stop-Loss Price: ${:.6}",
                            opposite_stop_loss_price
                        );
                        crate::log_println!("📊 Purchase Price: ${:.6}", trade.purchase_price);
                        crate::log_println!(
                            "   Condition: {} <= {}",
                            current_ask_price,
                            opposite_stop_loss_price
                        );
                        crate::log_println!("");
                        crate::log_println!("📊 Trade Details:");
                        crate::log_println!("   Token Type: {}", trade.token_type.display_name());
                        crate::log_println!("   Token ID: {}", trade.token_id);
                        crate::log_println!("   Units to sell: {:.6}", units_to_sell);
                        crate::log_println!("");
                        crate::log_println!("🔄 Executing stop-loss sell for opposite token...");

                        // Execute stop-loss sell for opposite token
                        match self
                            .execute_sell(
                                &key,
                                &mut trade,
                                units_to_sell,
                                current_ask_price,
                                Some("FAK"),
                                true,
                            )
                            .await
                        {
                            Ok(_) => {
                                crate::log_println!("   ✅ OPPOSITE TOKEN STOP-LOSS SELL EXECUTED");
                                let remaining_units = self
                                    .fetch_token_balance_units(&trade.token_id)
                                    .await
                                    .unwrap_or(0.0);
                                let sold_units = (units_to_sell - remaining_units).max(0.0);
                                if sold_units <= 0.000001 {
                                    warn!(
                                        "Opposite-token stop-loss acknowledged but no fill detected for {}",
                                        trade.token_type.display_name()
                                    );
                                    continue;
                                }

                                // Mark trade as sold only if fully filled.
                                {
                                    let mut pending = self.pending_trades.lock().await;
                                    if let Some(t) = pending.get_mut(key.as_str()) {
                                        if remaining_units > 0.000001 {
                                            t.sold = false;
                                            t.units = remaining_units;
                                            t.confirmed_balance = Some(remaining_units);
                                        } else {
                                            t.sold = true;
                                            t.confirmed_balance = Some(0.0);
                                        }
                                    }
                                }

                                // Log the sell event
                                let sell_event = format!(
                                    "OPPOSITE TOKEN STOP-LOSS SELL | Market: {} | Period: {} | Token: {} | Purchase Price: ${:.6} | Sell Price: ${:.6} | Shares Sold: {:.6} | Status: FILLED",
                                    trade.token_type.display_name(),
                                    trade.market_timestamp,
                                    &trade.token_id[..16],
                                    trade.purchase_price,
                                    current_ask_price,
                                    sold_units
                                );
                                crate::log_trading_event(&sell_event);
                                if remaining_units > 0.000001 {
                                    crate::log_println!(
                                        "   ⚠️  Opposite-token stop-loss partial fill; {:.6} shares remain open",
                                        remaining_units
                                    );
                                }

                                continue; // Move to next trade after handling opposite token stop-loss
                            }
                            Err(e) => {
                                eprintln!(
                                    "   ❌ FAILED TO EXECUTE OPPOSITE TOKEN STOP-LOSS SELL: {}",
                                    e
                                );
                                warn!("Failed to execute opposite token stop-loss sell: {}", e);
                                continue; // Move to next trade
                            }
                        }
                    }
                }
            }

            // NEW STRATEGY: Check for stop-loss condition for market buy trades
            // If price drops to stop_loss_price, sell the bought token and place limit sell for opposite token
            // Only apply to trades that have limit_sell_orders_placed (new strategy) and are NOT limit orders
            if trade.limit_sell_orders_placed && !Self::is_limit_style_key(&key) {
                if let Some(stop_loss_price) = self.stop_loss_price_clamped() {
                    // Only trigger stop-loss if price is at or below threshold
                    if current_ask_price <= stop_loss_price {
                        // CRITICAL: Re-check actual balance before selling
                        let actual_balance = match self
                            .api
                            .check_balance_only(&trade.token_id)
                            .await
                        {
                            Ok(balance) => {
                                let balance_decimal =
                                    balance / rust_decimal::Decimal::from(1_000_000u64);
                                let balance_f64 = f64::try_from(balance_decimal).unwrap_or(0.0);
                                if balance_f64 > 0.0
                                    && (trade.units == 0.0 || balance_f64 > trade.units)
                                {
                                    balance_f64
                                } else {
                                    trade.units
                                }
                            }
                            Err(e) => {
                                warn!("⚠️  Failed to re-check balance before stop-loss sell: {}. Using stored units: {:.6}", e, trade.units);
                                trade.units
                            }
                        };

                        if actual_balance == 0.0 {
                            warn!(
                                "⚠️  Skipping stop-loss sell - actual balance is 0.000000 shares."
                            );
                            continue;
                        }

                        let units_to_sell = actual_balance;

                        crate::log_println!(
                            "═══════════════════════════════════════════════════════════"
                        );
                        crate::log_println!("🛑 STOP-LOSS TRIGGERED - NEW STRATEGY");
                        crate::log_println!(
                            "═══════════════════════════════════════════════════════════"
                        );
                        crate::log_println!("⚠️  Price dropped to stop-loss threshold!");
                        crate::log_println!("💰 Current ASK Price: ${:.6}", current_ask_price);
                        crate::log_println!("🛑 Stop-Loss Price: ${:.6}", stop_loss_price);
                        crate::log_println!("📊 Purchase Price: ${:.6}", trade.purchase_price);
                        crate::log_println!(
                            "   Condition: {} <= {}",
                            current_ask_price,
                            stop_loss_price
                        );
                        crate::log_println!("");
                        crate::log_println!("📊 Trade Details:");
                        crate::log_println!("   Token Type: {}", trade.token_type.display_name());
                        crate::log_println!("   Token ID: {}", trade.token_id);
                        crate::log_println!("   Units to sell: {:.6}", units_to_sell);
                        crate::log_println!("");
                        crate::log_println!(
                            "🔄 Executing stop-loss sell and placing opposite token order..."
                        );

                        // Execute stop-loss sell
                        match self
                            .execute_sell(
                                &key,
                                &mut trade,
                                units_to_sell,
                                current_ask_price,
                                Some("FAK"),
                                true,
                            )
                            .await
                        {
                            Ok(_) => {
                                crate::log_println!("   ✅ STOP-LOSS SELL EXECUTED");

                                // Place limit buy order for opposite token at (1 - stop_loss_price)
                                // This ensures we have the hedge even if the earlier limit buy didn't fill
                                let opposite_token_type = trade.token_type.opposite();
                                let opposite_buy_price = 1.0 - stop_loss_price; // e.g., 1.0 - 0.80 = 0.20

                                match self
                                    .get_opposite_token_id(&trade.token_type, &trade.condition_id)
                                    .await
                                {
                                    Ok(opposite_token_id) => {
                                        // Check if we already have the opposite token
                                        let opposite_balance = match self
                                            .api
                                            .check_balance_only(&opposite_token_id)
                                            .await
                                        {
                                            Ok(balance) => {
                                                let balance_decimal = balance
                                                    / rust_decimal::Decimal::from(1_000_000u64);
                                                f64::try_from(balance_decimal).unwrap_or(0.0)
                                            }
                                            Err(_) => 0.0,
                                        };

                                        if opposite_balance > 0.000001 {
                                            // We already have the opposite token - place limit sell order
                                            let opposite_sell_price = (1.0 - stop_loss_price) + 0.1; // e.g., (1.0 - 0.80) + 0.1 = 0.30

                                            crate::log_println!("   📤 Placing limit sell for opposite token (already have it):");
                                            crate::log_println!(
                                                "      Token: {}",
                                                opposite_token_type.display_name()
                                            );
                                            crate::log_println!(
                                                "      Limit Price: ${:.6}",
                                                opposite_sell_price
                                            );
                                            crate::log_println!(
                                                "      Balance: {:.6} shares",
                                                opposite_balance
                                            );

                                            use crate::models::OrderRequest;
                                            let opposite_sell_order = OrderRequest {
                                                token_id: opposite_token_id.clone(),
                                                side: "SELL".to_string(),
                                                size: format!("{:.2}", opposite_balance),
                                                price: format!("{:.2}", opposite_sell_price),
                                                order_type: "LIMIT".to_string(),
                                                expiration_ts: None,
                                                post_only: None,
                                            };

                                            match self.api.place_order(&opposite_sell_order).await {
                                                Ok(response) => {
                                                    crate::log_println!("   ✅ LIMIT SELL ORDER PLACED FOR OPPOSITE TOKEN");
                                                    crate::log_println!(
                                                        "      Order ID: {:?}",
                                                        response.order_id
                                                    );
                                                    crate::log_println!(
                                                        "      Limit Price: ${:.6}",
                                                        opposite_sell_price
                                                    );

                                                    let order_id_str = response
                                                        .order_id
                                                        .as_ref()
                                                        .map(|id| format!("{:?}", id))
                                                        .unwrap_or_else(|| "N/A".to_string());
                                                    let sell_event = format!(
                                                        "LIMIT SELL ORDER (OPPOSITE AFTER STOP-LOSS) | Market: {} | Period: {} | Token: {} | Limit Price: ${:.6} | Size: {:.6} | Order ID: {}",
                                                        opposite_token_type.display_name(),
                                                        trade.market_timestamp,
                                                        &opposite_token_id[..16],
                                                        opposite_sell_price,
                                                        opposite_balance,
                                                        order_id_str
                                                    );
                                                    crate::log_trading_event(&sell_event);

                                                    // Create a PendingTrade entry to track this opposite token limit sell order
                                                    let opposite_trade = PendingTrade {
                                                        token_id: opposite_token_id.clone(),
                                                        condition_id: trade.condition_id.clone(),
                                                        token_type: opposite_token_type.clone(),
                                                        investment_amount: opposite_balance
                                                            * opposite_buy_price,
                                                        units: opposite_balance,
                                                        purchase_price: opposite_buy_price,
                                                        sell_price: opposite_sell_price,
                                                        timestamp: std::time::Instant::now(),
                                                        market_timestamp: trade.market_timestamp,
                                                        source_timeframe: trade
                                                            .source_timeframe
                                                            .clone(),
                                                        strategy_id:
                                                            crate::strategy::default_strategy_id(),
                                                        entry_mode: trade.entry_mode.clone(),
                                                        order_id: response.order_id.clone(),
                                                        end_timestamp: trade.end_timestamp,
                                                        sold: false,
                                                        confirmed_balance: Some(opposite_balance),
                                                        buy_order_confirmed: true,
                                                        limit_sell_orders_placed: true,
                                                        no_sell: false,
                                                        claim_on_closure: false,
                                                        sell_attempts: 0,
                                                        redemption_attempts: 0,
                                                        redemption_abandoned: false,
                                                    };

                                                    let opposite_trade_key = format!(
                                                        "{}_opposite_{}",
                                                        trade.market_timestamp, opposite_token_id
                                                    );
                                                    let mut pending =
                                                        self.pending_trades.lock().await;
                                                    pending
                                                        .insert(opposite_trade_key, opposite_trade);
                                                    drop(pending);

                                                    crate::log_println!("   📊 Tracking opposite token limit sell order (will monitor for fill)");
                                                }
                                                Err(e) => {
                                                    eprintln!("   ❌ FAILED TO PLACE LIMIT SELL FOR OPPOSITE TOKEN: {}", e);
                                                    warn!("Failed to place limit sell for opposite token: {}", e);
                                                }
                                            }
                                        } else {
                                            // We don't have the opposite token yet - place limit buy order
                                            // Use the same number of shares as the bought token we just sold
                                            let opposite_buy_size = units_to_sell; // Same number of shares

                                            crate::log_println!("   📥 Placing limit buy for opposite token (hedge):");
                                            crate::log_println!(
                                                "      Token: {}",
                                                opposite_token_type.display_name()
                                            );
                                            crate::log_println!(
                                                "      Limit Price: ${:.6}",
                                                opposite_buy_price
                                            );
                                            crate::log_println!(
                                                "      Size: {:.6} shares (same as sold token)",
                                                opposite_buy_size
                                            );

                                            use crate::models::OrderRequest;
                                            let opposite_buy_order = OrderRequest {
                                                token_id: opposite_token_id.clone(),
                                                side: "BUY".to_string(),
                                                size: format!("{:.2}", opposite_buy_size),
                                                price: format!("{:.2}", opposite_buy_price),
                                                order_type: "LIMIT".to_string(),
                                                expiration_ts: None,
                                                post_only: None,
                                            };

                                            match self.api.place_order(&opposite_buy_order).await {
                                                Ok(response) => {
                                                    crate::log_println!("   ✅ LIMIT BUY ORDER PLACED FOR OPPOSITE TOKEN");
                                                    crate::log_println!(
                                                        "      Order ID: {:?}",
                                                        response.order_id
                                                    );
                                                    crate::log_println!(
                                                        "      Limit Price: ${:.6}",
                                                        opposite_buy_price
                                                    );
                                                    crate::log_println!(
                                                        "      Size: {:.6} shares",
                                                        opposite_buy_size
                                                    );

                                                    let order_id_str = response
                                                        .order_id
                                                        .as_ref()
                                                        .map(|id| format!("{:?}", id))
                                                        .unwrap_or_else(|| "N/A".to_string());
                                                    let buy_event = format!(
                                                        "LIMIT BUY ORDER (OPPOSITE AFTER STOP-LOSS) | Market: {} | Period: {} | Token: {} | Limit Price: ${:.6} | Size: {:.6} | Order ID: {}",
                                                        opposite_token_type.display_name(),
                                                        trade.market_timestamp,
                                                        &opposite_token_id[..16],
                                                        opposite_buy_price,
                                                        opposite_buy_size,
                                                        order_id_str
                                                    );
                                                    crate::log_trading_event(&buy_event);

                                                    // Create a PendingTrade entry to track this opposite token limit buy order
                                                    let opposite_trade = PendingTrade {
                                                        token_id: opposite_token_id.clone(),
                                                        condition_id: trade.condition_id.clone(),
                                                        token_type: opposite_token_type.clone(),
                                                        investment_amount: opposite_buy_size
                                                            * opposite_buy_price,
                                                        units: opposite_buy_size,
                                                        purchase_price: opposite_buy_price,
                                                        sell_price: (1.0 - stop_loss_price) + 0.1, // Will sell at 0.30 when filled
                                                        timestamp: std::time::Instant::now(),
                                                        market_timestamp: trade.market_timestamp,
                                                        source_timeframe: trade
                                                            .source_timeframe
                                                            .clone(),
                                                        strategy_id:
                                                            crate::strategy::default_strategy_id(),
                                                        entry_mode: trade.entry_mode.clone(),
                                                        order_id: response.order_id.clone(),
                                                        end_timestamp: trade.end_timestamp,
                                                        sold: false,
                                                        confirmed_balance: Some(0.0), // Not filled yet
                                                        buy_order_confirmed: false, // Limit buy not confirmed yet
                                                        limit_sell_orders_placed: false, // Will place sell order after buy fills
                                                        no_sell: false,
                                                        claim_on_closure: false,
                                                        sell_attempts: 0,
                                                        redemption_attempts: 0,
                                                        redemption_abandoned: false,
                                                    };

                                                    let opposite_trade_key = format!(
                                                        "{}_opposite_limit_{}",
                                                        trade.market_timestamp, opposite_token_id
                                                    );
                                                    let mut pending =
                                                        self.pending_trades.lock().await;
                                                    pending
                                                        .insert(opposite_trade_key, opposite_trade);
                                                    drop(pending);

                                                    if let Some(order_id) =
                                                        response.order_id.as_deref()
                                                    {
                                                        self.upsert_pending_order_tracking(
                                                            order_id,
                                                            &format!(
                                                                "{}_opposite_limit_{}",
                                                                trade.market_timestamp,
                                                                opposite_token_id
                                                            ),
                                                            &opposite_token_id,
                                                            trade.source_timeframe.as_str(),
                                                            trade.entry_mode.as_str(),
                                                            Some(Self::token_family(
                                                                &opposite_token_type,
                                                            )),
                                                            trade.market_timestamp,
                                                            opposite_buy_price,
                                                            opposite_buy_size * opposite_buy_price,
                                                            "BUY",
                                                            "OPEN",
                                                        )?;
                                                    }

                                                    crate::log_println!("   📊 Tracking opposite token limit buy order (will monitor for fill and place sell order)");
                                                }
                                                Err(e) => {
                                                    eprintln!("   ❌ FAILED TO PLACE LIMIT BUY ORDER FOR OPPOSITE TOKEN: {}", e);
                                                    warn!("Failed to place limit buy order for opposite token: {}", e);
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("   ❌ FAILED TO GET OPPOSITE TOKEN ID: {}", e);
                                        warn!("Failed to get opposite token ID: {}", e);
                                    }
                                }

                                // Mark trade as sold
                                {
                                    let remaining_units = self
                                        .fetch_token_balance_units(&trade.token_id)
                                        .await
                                        .unwrap_or(0.0);
                                    let mut pending = self.pending_trades.lock().await;
                                    if let Some(t) = pending.get_mut(key.as_str()) {
                                        if remaining_units > 0.000001 {
                                            t.sold = false;
                                            t.units = remaining_units;
                                            t.confirmed_balance = Some(remaining_units);
                                        } else {
                                            t.sold = true;
                                            t.confirmed_balance = Some(0.0);
                                        }
                                    }
                                }

                                continue; // Move to next trade after handling stop-loss
                            }
                            Err(e) => {
                                eprintln!("   ❌ FAILED TO EXECUTE STOP-LOSS SELL: {}", e);
                                warn!("Failed to execute stop-loss sell: {}", e);
                                continue; // Move to next trade
                            }
                        }
                    }
                }

                // Skip old stop-loss logic for new strategy trades
                continue;
            }

            // OLD STOP-LOSS LOGIC (for non-new-strategy trades) - keep for backward compatibility
            // Skip stop-loss for limit order version trades (identified by limit_sell_orders_placed flag)
            if trade.limit_sell_orders_placed {
                // This is a limit order version trade - skip stop-loss monitoring
                continue;
            }

            // Check for stop-loss condition first (before checking for profit sell)
            // Stop-loss: sell if price drops below stop_loss_price to limit losses
            // Note: If stop-loss sell fails, keep retrying until sold OR price recovers above stop_loss_price
            if let Some(stop_loss_price) = self.stop_loss_price_clamped() {
                // Only trigger stop-loss if price is below threshold
                // If price recovers above stop_loss_price, cancel stop-loss attempt
                if current_ask_price < stop_loss_price {
                    // CRITICAL: Re-check actual balance before selling
                    let actual_balance = match self.api.check_balance_only(&trade.token_id).await {
                        Ok(balance) => {
                            // Conditional tokens use 1e6 as base unit (like USDC)
                            // Convert from smallest unit to actual shares
                            let balance_decimal =
                                balance / rust_decimal::Decimal::from(1_000_000u64);
                            let balance_f64 = f64::try_from(balance_decimal).unwrap_or(0.0);
                            if balance_f64 > 0.0
                                && (trade.units == 0.0 || balance_f64 > trade.units)
                            {
                                crate::log_println!("   🔄 Updating units from stored {:.6} to actual balance {:.6} shares", trade.units, balance_f64);
                                balance_f64
                            } else {
                                trade.units
                            }
                        }
                        Err(e) => {
                            warn!("⚠️  Failed to re-check balance before stop-loss sell: {}. Using stored units: {:.6}", e, trade.units);
                            trade.units
                        }
                    };

                    if actual_balance == 0.0 {
                        warn!("⚠️  Skipping stop-loss sell - actual balance is 0.000000 shares.");
                        continue;
                    }

                    let units_to_sell = actual_balance;

                    crate::log_println!(
                        "═══════════════════════════════════════════════════════════"
                    );
                    crate::log_println!("🛑 STOP-LOSS TRIGGERED - NEW STRATEGY");
                    crate::log_println!(
                        "═══════════════════════════════════════════════════════════"
                    );
                    crate::log_println!("⚠️  Price dropped to stop-loss threshold!");
                    crate::log_println!("💰 Current ASK Price: ${:.6}", current_ask_price);
                    crate::log_println!("🛑 Stop-Loss Price: ${:.6}", stop_loss_price);
                    crate::log_println!("📊 Purchase Price: ${:.6}", trade.purchase_price);
                    crate::log_println!(
                        "   Condition: {} <= {}",
                        current_ask_price,
                        stop_loss_price
                    );
                    crate::log_println!("");
                    crate::log_println!("📊 Trade Details:");
                    crate::log_println!("   Token Type: {}", trade.token_type.display_name());
                    crate::log_println!("   Token ID: {}", trade.token_id);
                    crate::log_println!("   Units to sell: {:.6}", units_to_sell);
                    crate::log_println!("");
                    crate::log_println!(
                        "🔄 Executing stop-loss sell and placing opposite token order..."
                    );

                    // Execute stop-loss sell
                    match self
                        .execute_sell(
                            &key,
                            &mut trade,
                            units_to_sell,
                            current_ask_price,
                            Some("FAK"),
                            true,
                        )
                        .await
                    {
                        Ok(_) => {
                            crate::log_println!("   ✅ STOP-LOSS SELL EXECUTED");

                            // Place limit buy order for opposite token at (1 - stop_loss_price)
                            // This ensures we have the hedge even if the earlier limit buy didn't fill
                            let opposite_token_type = trade.token_type.opposite();
                            let opposite_buy_price = 1.0 - stop_loss_price; // e.g., 1.0 - 0.80 = 0.20

                            match self
                                .get_opposite_token_id(&trade.token_type, &trade.condition_id)
                                .await
                            {
                                Ok(opposite_token_id) => {
                                    // Check if we already have the opposite token
                                    let opposite_balance =
                                        match self.api.check_balance_only(&opposite_token_id).await
                                        {
                                            Ok(balance) => {
                                                let balance_decimal = balance
                                                    / rust_decimal::Decimal::from(1_000_000u64);
                                                f64::try_from(balance_decimal).unwrap_or(0.0)
                                            }
                                            Err(_) => 0.0,
                                        };

                                    if opposite_balance > 0.000001 {
                                        // We already have the opposite token - place limit sell order
                                        let opposite_sell_price = (1.0 - stop_loss_price) + 0.1; // e.g., (1.0 - 0.80) + 0.1 = 0.30

                                        crate::log_println!("   📤 Placing limit sell for opposite token (already have it):");
                                        crate::log_println!(
                                            "      Token: {}",
                                            opposite_token_type.display_name()
                                        );
                                        crate::log_println!(
                                            "      Limit Price: ${:.6}",
                                            opposite_sell_price
                                        );
                                        crate::log_println!(
                                            "      Balance: {:.6} shares",
                                            opposite_balance
                                        );

                                        use crate::models::OrderRequest;
                                        let opposite_sell_order = OrderRequest {
                                            token_id: opposite_token_id.clone(),
                                            side: "SELL".to_string(),
                                            size: format!("{:.2}", opposite_balance),
                                            price: format!("{:.2}", opposite_sell_price),
                                            order_type: "LIMIT".to_string(),
                                            expiration_ts: None,
                                            post_only: None,
                                        };

                                        match self.api.place_order(&opposite_sell_order).await {
                                            Ok(response) => {
                                                crate::log_println!("   ✅ LIMIT SELL ORDER PLACED FOR OPPOSITE TOKEN");
                                                crate::log_println!(
                                                    "      Order ID: {:?}",
                                                    response.order_id
                                                );
                                                crate::log_println!(
                                                    "      Limit Price: ${:.6}",
                                                    opposite_sell_price
                                                );

                                                let order_id_str = response
                                                    .order_id
                                                    .as_ref()
                                                    .map(|id| format!("{:?}", id))
                                                    .unwrap_or_else(|| "N/A".to_string());
                                                let sell_event = format!(
                                                    "LIMIT SELL ORDER (OPPOSITE AFTER STOP-LOSS) | Market: {} | Period: {} | Token: {} | Limit Price: ${:.6} | Size: {:.6} | Order ID: {}",
                                                    opposite_token_type.display_name(),
                                                    trade.market_timestamp,
                                                    &opposite_token_id[..16],
                                                    opposite_sell_price,
                                                    opposite_balance,
                                                    order_id_str
                                                );
                                                crate::log_trading_event(&sell_event);

                                                // Create a PendingTrade entry to track this opposite token limit sell order
                                                let opposite_trade = PendingTrade {
                                                    token_id: opposite_token_id.clone(),
                                                    condition_id: trade.condition_id.clone(),
                                                    token_type: opposite_token_type.clone(),
                                                    investment_amount: opposite_balance
                                                        * opposite_buy_price,
                                                    units: opposite_balance,
                                                    purchase_price: opposite_buy_price,
                                                    sell_price: opposite_sell_price,
                                                    timestamp: std::time::Instant::now(),
                                                    market_timestamp: trade.market_timestamp,
                                                    source_timeframe: trade
                                                        .source_timeframe
                                                        .clone(),
                                                    strategy_id:
                                                        crate::strategy::default_strategy_id(),
                                                    entry_mode: trade.entry_mode.clone(),
                                                    order_id: response.order_id.clone(),
                                                    end_timestamp: trade.end_timestamp,
                                                    sold: false,
                                                    confirmed_balance: Some(opposite_balance),
                                                    buy_order_confirmed: true,
                                                    limit_sell_orders_placed: true,
                                                    no_sell: false,
                                                    claim_on_closure: false,
                                                    sell_attempts: 0,
                                                    redemption_attempts: 0,
                                                    redemption_abandoned: false,
                                                };

                                                let opposite_trade_key = format!(
                                                    "{}_opposite_{}",
                                                    trade.market_timestamp, opposite_token_id
                                                );
                                                let mut pending = self.pending_trades.lock().await;
                                                pending.insert(opposite_trade_key, opposite_trade);
                                                drop(pending);

                                                crate::log_println!("   📊 Tracking opposite token limit sell order (will monitor for fill)");
                                            }
                                            Err(e) => {
                                                eprintln!("   ❌ FAILED TO PLACE LIMIT SELL FOR OPPOSITE TOKEN: {}", e);
                                                warn!("Failed to place limit sell for opposite token: {}", e);
                                            }
                                        }
                                    } else {
                                        // We don't have the opposite token yet - place limit buy order
                                        // Use the same number of shares as the bought token we just sold
                                        let opposite_buy_size = units_to_sell; // Same number of shares

                                        crate::log_println!(
                                            "   📥 Placing limit buy for opposite token (hedge):"
                                        );
                                        crate::log_println!(
                                            "      Token: {}",
                                            opposite_token_type.display_name()
                                        );
                                        crate::log_println!(
                                            "      Limit Price: ${:.6}",
                                            opposite_buy_price
                                        );
                                        crate::log_println!(
                                            "      Size: {:.6} shares (same as sold token)",
                                            opposite_buy_size
                                        );

                                        use crate::models::OrderRequest;
                                        let opposite_buy_order = OrderRequest {
                                            token_id: opposite_token_id.clone(),
                                            side: "BUY".to_string(),
                                            size: format!("{:.2}", opposite_buy_size),
                                            price: format!("{:.2}", opposite_buy_price),
                                            order_type: "LIMIT".to_string(),
                                            expiration_ts: None,
                                            post_only: None,
                                        };

                                        match self.api.place_order(&opposite_buy_order).await {
                                            Ok(response) => {
                                                crate::log_println!("   ✅ LIMIT BUY ORDER PLACED FOR OPPOSITE TOKEN");
                                                crate::log_println!(
                                                    "      Order ID: {:?}",
                                                    response.order_id
                                                );
                                                crate::log_println!(
                                                    "      Limit Price: ${:.6}",
                                                    opposite_buy_price
                                                );
                                                crate::log_println!(
                                                    "      Size: {:.6} shares",
                                                    opposite_buy_size
                                                );

                                                let order_id_str = response
                                                    .order_id
                                                    .as_ref()
                                                    .map(|id| format!("{:?}", id))
                                                    .unwrap_or_else(|| "N/A".to_string());
                                                let buy_event = format!(
                                                    "LIMIT BUY ORDER (OPPOSITE AFTER STOP-LOSS) | Market: {} | Period: {} | Token: {} | Limit Price: ${:.6} | Size: {:.6} | Order ID: {}",
                                                    opposite_token_type.display_name(),
                                                    trade.market_timestamp,
                                                    &opposite_token_id[..16],
                                                    opposite_buy_price,
                                                    opposite_buy_size,
                                                    order_id_str
                                                );
                                                crate::log_trading_event(&buy_event);

                                                // Create a PendingTrade entry to track this opposite token limit buy order
                                                let opposite_trade = PendingTrade {
                                                    token_id: opposite_token_id.clone(),
                                                    condition_id: trade.condition_id.clone(),
                                                    token_type: opposite_token_type.clone(),
                                                    investment_amount: opposite_buy_size
                                                        * opposite_buy_price,
                                                    units: opposite_buy_size,
                                                    purchase_price: opposite_buy_price,
                                                    sell_price: (1.0 - stop_loss_price) + 0.1, // Will sell at 0.30 when filled
                                                    timestamp: std::time::Instant::now(),
                                                    market_timestamp: trade.market_timestamp,
                                                    source_timeframe: trade
                                                        .source_timeframe
                                                        .clone(),
                                                    strategy_id:
                                                        crate::strategy::default_strategy_id(),
                                                    entry_mode: trade.entry_mode.clone(),
                                                    order_id: response.order_id.clone(),
                                                    end_timestamp: trade.end_timestamp,
                                                    sold: false,
                                                    confirmed_balance: Some(0.0), // Not filled yet
                                                    buy_order_confirmed: false, // Limit buy not confirmed yet
                                                    limit_sell_orders_placed: false, // Will place sell order after buy fills
                                                    no_sell: false,
                                                    claim_on_closure: false,
                                                    sell_attempts: 0,
                                                    redemption_attempts: 0,
                                                    redemption_abandoned: false,
                                                };

                                                let opposite_trade_key = format!(
                                                    "{}_opposite_limit_{}",
                                                    trade.market_timestamp, opposite_token_id
                                                );
                                                let mut pending = self.pending_trades.lock().await;
                                                pending.insert(opposite_trade_key, opposite_trade);
                                                drop(pending);

                                                if let Some(order_id) = response.order_id.as_deref()
                                                {
                                                    self.upsert_pending_order_tracking(
                                                        order_id,
                                                        &format!(
                                                            "{}_opposite_limit_{}",
                                                            trade.market_timestamp,
                                                            opposite_token_id
                                                        ),
                                                        &opposite_token_id,
                                                        trade.source_timeframe.as_str(),
                                                        trade.entry_mode.as_str(),
                                                        Some(Self::token_family(
                                                            &opposite_token_type,
                                                        )),
                                                        trade.market_timestamp,
                                                        opposite_buy_price,
                                                        opposite_buy_size * opposite_buy_price,
                                                        "BUY",
                                                        "OPEN",
                                                    )?;
                                                }

                                                crate::log_println!("   📊 Tracking opposite token limit buy order (will monitor for fill and place sell order)");
                                            }
                                            Err(e) => {
                                                eprintln!("   ❌ FAILED TO PLACE LIMIT BUY ORDER FOR OPPOSITE TOKEN: {}", e);
                                                warn!("Failed to place limit buy order for opposite token: {}", e);
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    eprintln!("   ❌ FAILED TO GET OPPOSITE TOKEN ID: {}", e);
                                    warn!("Failed to get opposite token ID: {}", e);
                                }
                            }

                            // Mark trade as sold
                            {
                                let remaining_units = self
                                    .fetch_token_balance_units(&trade.token_id)
                                    .await
                                    .unwrap_or(0.0);
                                let mut pending = self.pending_trades.lock().await;
                                if let Some(t) = pending.get_mut(key.as_str()) {
                                    if remaining_units > 0.000001 {
                                        t.sold = false;
                                        t.units = remaining_units;
                                        t.confirmed_balance = Some(remaining_units);
                                    } else {
                                        t.sold = true;
                                        t.confirmed_balance = Some(0.0);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("   ❌ FAILED TO EXECUTE STOP-LOSS SELL: {}", e);
                            warn!("Failed to execute stop-loss sell: {}", e);
                        }
                    }

                    continue; // Move to next trade after handling stop-loss
                }
            }

            // OLD STOP-LOSS LOGIC (for non-limit-order-version trades) - keep for backward compatibility
            // Skip stop-loss for limit order version trades (identified by limit_sell_orders_placed flag)
            if trade.limit_sell_orders_placed && !Self::is_limit_style_key(&key) {
                // This is a limit order version trade - skip old stop-loss monitoring
                continue;
            }

            // Check for stop-loss condition first (before checking for profit sell)
            // Stop-loss: sell if price drops below stop_loss_price to limit losses
            // Note: If stop-loss sell fails, keep retrying until sold OR price recovers above stop_loss_price
            if let Some(stop_loss_price) = self.stop_loss_price_clamped() {
                // Only trigger stop-loss if price is below threshold
                // If price recovers above stop_loss_price, cancel stop-loss attempt
                if current_ask_price < stop_loss_price {
                    // CRITICAL: Re-check actual balance before selling
                    let actual_balance = match self.api.check_balance_only(&trade.token_id).await {
                        Ok(balance) => {
                            // Conditional tokens use 1e6 as base unit (like USDC)
                            // Convert from smallest unit to actual shares
                            let balance_decimal =
                                balance / rust_decimal::Decimal::from(1_000_000u64);
                            let balance_f64 = f64::try_from(balance_decimal).unwrap_or(0.0);
                            if balance_f64 > 0.0
                                && (trade.units == 0.0 || balance_f64 > trade.units)
                            {
                                crate::log_println!("   🔄 Updating units from stored {:.6} to actual balance {:.6} shares", trade.units, balance_f64);
                                balance_f64
                            } else {
                                trade.units
                            }
                        }
                        Err(e) => {
                            warn!("⚠️  Failed to re-check balance before stop-loss sell: {}. Using stored units: {:.6}", e, trade.units);
                            trade.units
                        }
                    };

                    if actual_balance == 0.0 {
                        warn!("⚠️  Skipping stop-loss sell - actual balance is 0.000000 shares.");
                        continue;
                    }

                    let units_to_sell = actual_balance;
                    let loss = (current_ask_price - trade.purchase_price) * units_to_sell;

                    crate::log_println!(
                        "═══════════════════════════════════════════════════════════"
                    );
                    crate::log_println!("🛑 STOP-LOSS TRIGGERED");
                    crate::log_println!(
                        "═══════════════════════════════════════════════════════════"
                    );
                    crate::log_println!("⚠️  Price dropped below stop-loss threshold!");
                    crate::log_println!("💰 Current ASK Price: ${:.6}", current_ask_price);
                    crate::log_println!("🛑 Stop-Loss Price: ${:.6}", stop_loss_price);
                    crate::log_println!("📊 Purchase Price: ${:.6}", trade.purchase_price);
                    crate::log_println!(
                        "   Condition: {} < {}",
                        current_ask_price,
                        stop_loss_price
                    );
                    crate::log_println!("");
                    crate::log_println!("📊 Trade Details:");
                    crate::log_println!("   Token Type: {}", trade.token_type.display_name());
                    crate::log_println!("   Token ID: {}", trade.token_id);
                    crate::log_println!("   Units to sell: {:.6}", units_to_sell);
                    crate::log_println!("   Purchase price: ${:.6}", trade.purchase_price);
                    crate::log_println!("   Current ASK price: ${:.6}", current_ask_price);
                    crate::log_println!(
                        "   Expected revenue: ${:.6}",
                        current_ask_price * units_to_sell
                    );
                    crate::log_println!(
                        "   Original cost: ${:.6}",
                        trade.purchase_price * units_to_sell
                    );
                    crate::log_println!("   Loss: ${:.6}", loss);
                    crate::log_println!("");
                    crate::log_println!("   🔄 Executing stop-loss sell to limit losses...");

                    // CRITICAL: Refresh backend's cached allowance before selling
                    // Even though setApprovalForAll is set on-chain, the backend cache might be stale
                    // The API checks the cached allowance, not the on-chain approval directly
                    crate::log_println!("   🔄 Refreshing backend's cached allowance (required for API to recognize approval)...");
                    if let Err(e) = self
                        .api
                        .update_balance_allowance_for_sell(&trade.token_id)
                        .await
                    {
                        crate::log_println!(
                            "   ⚠️  Failed to refresh allowance cache: {} (proceeding anyway)",
                            e
                        );
                    } else {
                        crate::log_println!("   ✅ Allowance cache refreshed - waiting 500ms for backend to process...");
                        // Give backend a moment to process the cache update
                        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    }

                    // Optimized retry loop for stop-loss: try selling ASAP, retry immediately on failure
                    // Stop if price recovers above stop-loss threshold (safe level)
                    let max_retry_attempts = 20; // Maximum retry attempts
                    let retry_delay_ms = 1500; // 1.5 seconds between retries
                    let mut sell_succeeded = false;
                    let mut last_price = current_ask_price;

                    for attempt in 1..=max_retry_attempts {
                        crate::log_println!(
                            "   🔄 Stop-loss sell attempt #{}/{} (price: ${:.6})",
                            attempt,
                            max_retry_attempts,
                            last_price
                        );

                        // CRITICAL: Refresh backend's cached allowance before each retry attempt
                        // The backend checks cached allowance, not on-chain approval directly
                        if attempt > 1 {
                            crate::log_println!(
                                "   🔄 Refreshing backend's cached allowance before retry..."
                            );
                            if let Err(e) = self
                                .api
                                .update_balance_allowance_for_sell(&trade.token_id)
                                .await
                            {
                                crate::log_println!("   ⚠️  Failed to refresh allowance cache: {} (retrying anyway)", e);
                            } else {
                                crate::log_println!("   ✅ Allowance cache refreshed - waiting 500ms for backend to process...");
                                // Give backend a moment to process the cache update
                                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                            }
                        }

                        // Execute stop-loss sell with FAK (Fill-and-Kill) to allow partial fills
                        let sell_result = self
                            .execute_sell(
                                &key,
                                &mut trade,
                                units_to_sell,
                                last_price,
                                Some("FAK"),
                                true,
                            )
                            .await;

                        match sell_result {
                            Ok(_) => {
                                sell_succeeded = true;
                                break; // Success - exit retry loop
                            }
                            Err(e) => {
                                let error_str = e.to_string();

                                // Check if price recovered above stop-loss threshold (safe level)
                                // If price recovers, stop retrying
                                let current_price_check =
                                    match self.api.get_price(&trade.token_id, "SELL").await {
                                        Ok(p) => f64::try_from(p).unwrap_or(last_price),
                                        Err(_) => last_price, // Use last known price if fetch fails
                                    };

                                // Stop retrying if price recovered above stop-loss threshold
                                if let Some(stop_loss_threshold) = self.stop_loss_price_clamped() {
                                    if current_price_check >= stop_loss_threshold {
                                        crate::log_println!("   ⏸️  Price recovered above stop-loss threshold (${:.6} >= ${:.6}) - stopping retry", 
                                            current_price_check, stop_loss_threshold);
                                        crate::log_println!("   💡 Position is safe - will monitor for profit sell or stop-loss again");

                                        // Update trade (don't mark as sold, keep monitoring)
                                        let mut pending = self.pending_trades.lock().await;
                                        if let Some(t) = pending.get_mut(key.as_str()) {
                                            *t = trade.clone();
                                        }
                                        drop(pending);

                                        // Log the failure
                                        let market_name = trade.token_type.display_name();
                                        let error_msg = format!("{:?}", e);
                                        let simple_error = if error_msg
                                            .contains("not enough balance")
                                            || error_msg.contains("allowance")
                                        {
                                            "not enough balance / allowance".to_string()
                                        } else if error_msg.contains("No opposing orders")
                                            || error_msg.contains("no orders found")
                                        {
                                            "no liquidity / no buyers".to_string()
                                        } else if error_msg.contains("couldn't be fully filled") {
                                            "insufficient liquidity".to_string()
                                        } else {
                                            error_msg
                                                .lines()
                                                .next()
                                                .unwrap_or("Sell failed")
                                                .to_string()
                                        };

                                        let sell_event = format!(
                                            "SELL ORDER (STOP-LOSS) | Market: {} | Period: {} | Price: ${:.6} | Units: {:.6} | Revenue: ${:.6} | Loss: ${:.6} | Status: FAILED | Attempt: {} | Error: {} | Stopped: Price recovered",
                                            market_name,
                                            trade.market_timestamp,
                                            current_price_check,
                                            units_to_sell,
                                            current_price_check * units_to_sell,
                                            loss,
                                            attempt,
                                            simple_error
                                        );
                                        crate::log_trading_event(&sell_event);

                                        break; // Stop retrying - price recovered
                                    }
                                }

                                last_price = current_price_check;

                                // Log failure but continue retrying
                                if attempt % 5 == 0 || attempt == 1 {
                                    crate::log_println!(
                                        "   ❌ Stop-loss sell attempt {} failed: {}",
                                        attempt,
                                        error_str
                                    );
                                    if attempt < max_retry_attempts {
                                        crate::log_println!(
                                            "   ⏳ Retrying in {}ms... (price: ${:.6})",
                                            retry_delay_ms,
                                            last_price
                                        );
                                    }
                                }

                                // Wait before next retry (except on last attempt)
                                if attempt < max_retry_attempts {
                                    tokio::time::sleep(tokio::time::Duration::from_millis(
                                        retry_delay_ms,
                                    ))
                                    .await;
                                }
                            }
                        }
                    }

                    // Handle final result
                    if !sell_succeeded {
                        // Max retries reached or price recovered - continue monitoring
                        let final_attempt = trade.sell_attempts;
                        if final_attempt >= max_retry_attempts {
                            crate::log_println!("   ⚠️  Maximum stop-loss sell attempts ({}) reached - will retry on next check", max_retry_attempts);
                        }
                        continue; // Continue to next trade - will retry on next check cycle
                    }

                    // Re-check balance to distinguish full fill from partial fill.
                    let remaining_units = self
                        .fetch_token_balance_units(&trade.token_id)
                        .await
                        .unwrap_or(0.0);
                    if remaining_units > 0.000001 {
                        trade.units = remaining_units;
                        trade.confirmed_balance = Some(remaining_units);
                        trade.buy_order_confirmed = true;
                        trade.sold = false;
                        let mut pending = self.pending_trades.lock().await;
                        if let Some(t) = pending.get_mut(key.as_str()) {
                            *t = trade.clone();
                        }
                        drop(pending);
                        crate::log_println!(
                            "   ⚠️  Stop-loss partial fill detected; {:.6} shares remain open",
                            remaining_units
                        );
                        continue;
                    }

                    // Stop-loss sell fully filled - mark as sold and remove from pending trades.
                    trade.sold = true;

                    // Mark cycle completed for this token type (requires reset before next buy)
                    if let Some(ref detector) = self.detector {
                        detector
                            .mark_cycle_completed(trade.token_type.clone())
                            .await;
                    }

                    // Remove from HashMap since trade is sold
                    let mut pending = self.pending_trades.lock().await;
                    pending.remove(key.as_str());
                    drop(pending);

                    crate::log_println!("   ✅ Stop-loss sell executed successfully");
                    crate::log_println!(
                        "   💡 Position closed - can re-buy if price goes back up over ${:.6}",
                        self.config.trigger_price
                    );
                    continue; // Move to next trade
                }
            }

            // Check if we've reached the sell price (0.99 or 1.0)
            // Also check if price is >= 1.0 (market resolution - token is worth $1)
            if current_ask_price >= trade.sell_price || current_ask_price >= 1.0 {
                // CRITICAL: Re-check actual balance before selling
                // The stored units might be 0 if balance check failed initially, but tokens may have arrived later
                let actual_balance = match self.api.check_balance_only(&trade.token_id).await {
                    Ok(balance) => {
                        // Conditional tokens use 1e6 as base unit (like USDC)
                        // Convert from smallest unit to actual shares
                        let balance_decimal = balance / rust_decimal::Decimal::from(1_000_000u64);
                        let balance_f64 = f64::try_from(balance_decimal).unwrap_or(0.0);
                        if balance_f64 > 0.0 && (trade.units == 0.0 || balance_f64 > trade.units) {
                            crate::log_println!("   🔄 Updating units from stored {:.6} to actual balance {:.6} shares", trade.units, balance_f64);
                            balance_f64
                        } else {
                            trade.units // Use stored units if they're valid
                        }
                    }
                    Err(e) => {
                        warn!("⚠️  Failed to re-check balance before selling: {}. Using stored units: {:.6}", e, trade.units);
                        trade.units
                    }
                };

                // Skip if we still have 0 units after re-checking
                if actual_balance == 0.0 {
                    warn!("⚠️  Skipping sell - actual balance is 0.000000 shares. Trade may not have executed.");
                    continue;
                }

                let units_to_sell = actual_balance; // Use actual balance, not stored units

                crate::log_println!("═══════════════════════════════════════════════════════════");
                crate::log_println!("📈 SELL CONDITION MET");
                crate::log_println!("═══════════════════════════════════════════════════════════");
                crate::log_println!("💰 Current Price: ${:.6}", current_ask_price);
                crate::log_println!("🎯 Target Price: ${:.6}", trade.sell_price);
                crate::log_println!(
                    "   Condition: {} >= {}",
                    current_ask_price,
                    trade.sell_price
                );
                crate::log_println!("");
                crate::log_println!("📊 Trade Details:");
                crate::log_println!("   Token Type: {}", trade.token_type.display_name());
                crate::log_println!("   Token ID: {}", trade.token_id);
                crate::log_println!("   Units to sell: {:.6}", units_to_sell);
                crate::log_println!("   Purchase price: ${:.6}", trade.purchase_price);
                crate::log_println!("   Current ASK price: ${:.6}", current_ask_price);
                crate::log_println!(
                    "   Expected revenue: ${:.6}",
                    current_ask_price * units_to_sell
                );
                crate::log_println!(
                    "   Original cost: ${:.6}",
                    trade.purchase_price * units_to_sell
                );
                let profit = (current_ask_price - trade.purchase_price) * units_to_sell;
                crate::log_println!("   Expected profit: ${:.6}", profit);
                crate::log_println!("");

                // Optimized retry loop: try selling ASAP, retry immediately on failure
                // Stop if price recovers to safe level (drops below sell_price)
                let max_retry_attempts = 20; // Maximum retry attempts
                let retry_delay_ms = 1500; // 1.5 seconds between retries
                let mut sell_succeeded = false;
                let mut last_price = current_ask_price;

                for attempt in 1..=max_retry_attempts {
                    trade.sell_attempts = attempt;
                    crate::log_println!(
                        "   🔄 Sell attempt #{}/{} (price: ${:.6})",
                        attempt,
                        max_retry_attempts,
                        last_price
                    );

                    // CRITICAL: Refresh backend's cached allowance before each retry attempt
                    // The backend checks cached allowance, not on-chain approval directly
                    if attempt > 1 {
                        crate::log_println!(
                            "   🔄 Refreshing backend's cached allowance before retry..."
                        );
                        if let Err(e) = self
                            .api
                            .update_balance_allowance_for_sell(&trade.token_id)
                            .await
                        {
                            crate::log_println!(
                                "   ⚠️  Failed to refresh allowance cache: {} (retrying anyway)",
                                e
                            );
                        } else {
                            crate::log_println!("   ✅ Allowance cache refreshed - waiting 500ms for backend to process...");
                            // Give backend a moment to process the cache update
                            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                        }
                    }

                    // Execute sell with FAK (Fill-and-Kill) to allow partial fills
                    let sell_result = self
                        .execute_sell(
                            &key,
                            &mut trade,
                            units_to_sell,
                            last_price,
                            Some("FAK"),
                            false,
                        )
                        .await;

                    match sell_result {
                        Ok(_) => {
                            sell_succeeded = true;
                            break; // Success - exit retry loop
                        }
                        Err(e) => {
                            let error_str = e.to_string();

                            // Check if price recovered to safe level (dropped below sell_price)
                            // If price drops significantly, stop retrying
                            let current_price_check =
                                match self.api.get_price(&trade.token_id, "SELL").await {
                                    Ok(p) => f64::try_from(p).unwrap_or(last_price),
                                    Err(_) => last_price, // Use last known price if fetch fails
                                };

                            // Stop retrying if price dropped below sell_price (recovered to safe level)
                            if current_price_check < trade.sell_price {
                                crate::log_println!("   ⏸️  Price recovered to safe level (${:.6} < ${:.6}) - stopping retry", 
                                    current_price_check, trade.sell_price);
                                crate::log_println!(
                                    "   💡 Will retry when price reaches ${:.6} again",
                                    trade.sell_price
                                );

                                // Update trade with current attempt count
                                let mut pending = self.pending_trades.lock().await;
                                if let Some(t) = pending.get_mut(key.as_str()) {
                                    t.sell_attempts = attempt;
                                }
                                drop(pending);

                                // Log the failure
                                let market_name = trade.token_type.display_name();
                                let error_msg = format!("{:?}", e);
                                let simple_error = if error_msg.contains("not enough balance")
                                    || error_msg.contains("allowance")
                                {
                                    "not enough balance / allowance".to_string()
                                } else if error_msg.contains("No opposing orders")
                                    || error_msg.contains("no orders found")
                                {
                                    "no liquidity / no buyers".to_string()
                                } else if error_msg.contains("couldn't be fully filled") {
                                    "insufficient liquidity".to_string()
                                } else {
                                    error_msg
                                        .lines()
                                        .find(|line| {
                                            !line.trim().is_empty()
                                                && !line.contains("Troubleshooting")
                                        })
                                        .unwrap_or("Sell failed")
                                        .to_string()
                                };

                                let sell_event = format!(
                                    "SELL ORDER (PROFIT) | Market: {} | Period: {} | Price: ${:.6} | Units: {:.6} | Revenue: ${:.6} | Profit: ${:.6} | Status: FAILED | Attempt: {}/{} | Error: {} | Stopped: Price recovered",
                                    market_name,
                                    trade.market_timestamp,
                                    current_price_check,
                                    units_to_sell,
                                    current_price_check * units_to_sell,
                                    (current_price_check - trade.purchase_price) * units_to_sell,
                                    attempt,
                                    max_retry_attempts,
                                    simple_error
                                );
                                crate::log_trading_event(&sell_event);

                                break; // Stop retrying - price recovered
                            }

                            last_price = current_price_check;

                            // Log failure but continue retrying
                            if attempt % 5 == 0 || attempt == 1 {
                                crate::log_println!(
                                    "   ❌ Sell attempt {} failed: {}",
                                    attempt,
                                    error_str
                                );
                                if attempt < max_retry_attempts {
                                    crate::log_println!(
                                        "   ⏳ Retrying in {}ms... (price: ${:.6})",
                                        retry_delay_ms,
                                        last_price
                                    );
                                }
                            }

                            // Wait before next retry (except on last attempt)
                            if attempt < max_retry_attempts {
                                tokio::time::sleep(tokio::time::Duration::from_millis(
                                    retry_delay_ms,
                                ))
                                .await;
                            }
                        }
                    }
                }

                // Handle final result
                if !sell_succeeded {
                    // Max retries reached or price recovered
                    if trade.sell_attempts >= max_retry_attempts {
                        crate::log_println!(
                            "   ⚠️  Maximum sell attempts ({}) reached",
                            max_retry_attempts
                        );
                        crate::log_println!("   📋 Marking trade for claim at market closure");

                        trade.claim_on_closure = true;
                        let mut pending = self.pending_trades.lock().await;
                        if let Some(t) = pending.get_mut(key.as_str()) {
                            *t = trade.clone();
                        }
                        drop(pending);

                        crate::log_println!(
                            "   ✅ Trade will be claimed/redeemed when market closes"
                        );
                    }
                    continue; // Move to next trade
                }

                // Re-check balance to distinguish full fill from partial fill.
                let remaining_units = self
                    .fetch_token_balance_units(&trade.token_id)
                    .await
                    .unwrap_or(0.0);
                if remaining_units > 0.000001 {
                    trade.units = remaining_units;
                    trade.confirmed_balance = Some(remaining_units);
                    trade.buy_order_confirmed = true;
                    trade.sold = false;
                    let mut pending = self.pending_trades.lock().await;
                    if let Some(t) = pending.get_mut(key.as_str()) {
                        *t = trade.clone();
                    }
                    drop(pending);
                    crate::log_println!(
                        "   ⚠️  Profit sell partial fill detected; {:.6} shares remain open",
                        remaining_units
                    );
                    continue;
                }

                // Mark as sold and remove from pending trades - execute_sell already logged the order with order ID.
                trade.sold = true;

                // Mark cycle completed for this token type (requires reset before next buy)
                if let Some(ref detector) = self.detector {
                    detector
                        .mark_cycle_completed(trade.token_type.clone())
                        .await;
                }

                // Remove from HashMap since trade is sold
                let mut pending = self.pending_trades.lock().await;
                pending.remove(key.as_str());
                drop(pending);
            }
        }

        Ok(())
    }

    /// Execute sell order with configurable order type
    /// order_type: None or "FAK" for Fill-and-Kill (allows partial fills), "FOK" for Fill-or-Kill
    /// Default: FAK (allows partial fills, better for limited liquidity situations)
    /// is_stop_loss: true if this is a stop-loss sell, false if it's a profit sell
    async fn execute_sell(
        &self,
        _trade_key: &str,
        trade: &PendingTrade,
        units_to_sell: f64,
        current_price: f64,
        order_type: Option<&str>,
        is_stop_loss: bool,
    ) -> Result<()> {
        let order_type_str = order_type.unwrap_or("FAK");
        let order_type_display = match order_type_str {
            "FAK" => "FAK (Fill-and-Kill - allows partial fills)",
            "FOK" => "FOK (Fill-or-Kill)",
            _ => "FAK (Fill-and-Kill - allows partial fills)",
        };
        if self.simulation_mode {
            let sell_value = current_price * units_to_sell;
            let profit = sell_value - (trade.purchase_price * units_to_sell);

            let mut total = self.total_profit.lock().await;
            *total += profit;
            let total_profit = *total;
            drop(total);

            crate::log_println!("🎮 SIMULATION MODE - Order NOT placed on exchange");
            crate::log_println!("   ✅ SIMULATION: Sell order would execute:");
            crate::log_println!("      - Token Type: {}", trade.token_type.display_name());
            crate::log_println!("      - Token: {}", &trade.token_id[..16]);
            crate::log_println!("      - Units: {:.6}", units_to_sell);
            crate::log_println!("      - Price: ${:.6}", current_price);
            crate::log_println!("      - Revenue: ${:.6}", sell_value);
            crate::log_println!("      - Cost: ${:.6}", trade.purchase_price * units_to_sell);
            crate::log_println!("      - Profit: ${:.6}", profit);
            crate::log_println!("      - Total Profit (all trades): ${:.6}", total_profit);
        } else {
            crate::log_println!("🚀 PRODUCTION MODE - Placing order on exchange");
            crate::log_println!("   Order parameters:");
            crate::log_println!("      Token Type: {}", trade.token_type.display_name());
            crate::log_println!("      Token ID: {}", trade.token_id);
            crate::log_println!("      Side: SELL");
            crate::log_println!(
                "      Shares: {:.6} units (market order - price determined by market)",
                units_to_sell
            );
            crate::log_println!("      Type: {}", order_type_display);

            // Place real market sell order
            // For SELL market orders, we pass number of shares/units
            // Note: If you get "not enough balance / allowance" error, it may be because:
            // 1. Token allowance needs to be set for proxy wallet (SDK should handle this automatically)
            // 2. The shares amount might need to be converted to USD value instead
            //
            // Calculate expected USD value for better error messages
            let expected_usd_value = current_price * units_to_sell;
            crate::log_println!(
                "   Expected USD value: ${:.6} ({} shares × ${:.6})",
                expected_usd_value,
                units_to_sell,
                current_price
            );

            // CRITICAL: Refresh backend's cached allowance before selling
            // Even though setApprovalForAll is set on-chain, the backend cache might be stale
            // The API checks the cached allowance, not the on-chain approval directly
            crate::log_println!("   🔄 Refreshing backend's cached allowance (required for API to recognize approval)...");
            if let Err(e) = self
                .api
                .update_balance_allowance_for_sell(&trade.token_id)
                .await
            {
                crate::log_println!("   ⚠️  Failed to refresh allowance cache: {} (proceeding anyway - backend might still work)", e);
            } else {
                crate::log_println!(
                    "   ✅ Allowance cache refreshed - waiting 500ms for backend to process..."
                );
                // Give backend a moment to process the cache update
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            }

            let pre_sell_balance = self.fetch_token_balance_units(&trade.token_id).await.ok();
            crate::log_println!("\n   📤 Placing SELL order...");
            match self
                .api
                .place_market_order(
                    &trade.token_id,
                    units_to_sell, // Number of shares/units for SELL market orders
                    "SELL",
                    Some(order_type_str),
                )
                .await
            {
                Ok(response) => {
                    crate::log_println!("   ✅ ORDER PLACED SUCCESSFULLY");
                    crate::log_println!("      Order ID: {:?}", response.order_id);
                    crate::log_println!("      Status: {}", response.status);
                    if let Some(msg) = &response.message {
                        crate::log_println!("      Message: {}", msg);
                    }

                    let post_sell_balance =
                        self.fetch_token_balance_units(&trade.token_id).await.ok();
                    let actual_sold_units = match (pre_sell_balance, post_sell_balance) {
                        (Some(before), Some(after)) => (before - after).max(0.0).min(units_to_sell),
                        _ => units_to_sell,
                    };
                    if actual_sold_units <= 0.000001 {
                        anyhow::bail!("Sell order acknowledged but no fill detected");
                    }
                    let remaining_units = post_sell_balance
                        .unwrap_or((units_to_sell - actual_sold_units).max(0.0))
                        .max(0.0);

                    // Calculate realized PnL from actual filled quantity.
                    let sell_value = current_price * actual_sold_units;
                    let pnl = sell_value - (trade.purchase_price * actual_sold_units);
                    let mut total = self.total_profit.lock().await;
                    *total += pnl;
                    drop(total);

                    // Log structured sell order to history.toml (profit or stop-loss)
                    let market_name = trade.token_type.display_name();
                    let order_id_str = response
                        .order_id
                        .as_ref()
                        .map(|id| format!("{:?}", id))
                        .unwrap_or_else(|| "N/A".to_string());

                    // Determine sell type
                    let sell_type = if is_stop_loss { "STOP-LOSS" } else { "PROFIT" };

                    let sell_event = format!(
                        "SELL ORDER ({}) | Market: {} | Period: {} | Price: ${:.6} | Units: {:.6} | Revenue: ${:.6} | {}: ${:.6} | Order ID: {} | Status: SUCCESS",
                        sell_type,
                        market_name,
                        trade.market_timestamp,
                        current_price,
                        actual_sold_units,
                        sell_value,
                        if pnl >= 0.0 { "Profit" } else { "Loss" },
                        pnl,
                        order_id_str
                    );
                    crate::log_trading_event(&sell_event);

                    let exit_reason = if is_stop_loss {
                        "stop_loss"
                    } else {
                        "profit_target"
                    };
                    let exit_reason = if remaining_units > 0.000001 {
                        format!(
                            "{}_partial_fill_remainder_{:.6}",
                            exit_reason, remaining_units
                        )
                    } else {
                        exit_reason.to_string()
                    };
                    self.record_trade_event_required_strategy(
                        trade.strategy_id.as_str(),
                        Some(Self::build_exit_event_key(
                            trade.strategy_id.as_str(),
                            trade.source_timeframe.as_str(),
                            trade.market_timestamp,
                            trade.token_id.as_str(),
                            "EXIT",
                            Some(exit_reason.as_str()),
                        )),
                        trade.market_timestamp,
                        Some(trade.source_timeframe.as_str()),
                        Some(trade.condition_id.clone()),
                        Some(trade.token_id.clone()),
                        Some(trade.token_type.display_name().to_string()),
                        Some("SELL".to_string()),
                        "EXIT",
                        Some(current_price),
                        Some(actual_sold_units),
                        Some(sell_value),
                        Some(pnl),
                        Some(exit_reason),
                    );
                    let (_, total_profit) = self.reporting_totals().await;

                    crate::log_println!("   📊 Trade Results:");
                    crate::log_println!("      Revenue: ${:.6}", sell_value);
                    crate::log_println!(
                        "      Cost: ${:.6}",
                        trade.purchase_price * actual_sold_units
                    );
                    crate::log_println!(
                        "      {}: ${:.6}",
                        if pnl >= 0.0 { "Profit" } else { "Loss" },
                        pnl
                    );
                    if remaining_units > 0.000001 {
                        crate::log_println!(
                            "      Partial fill: {:.6} filled, {:.6} remaining",
                            actual_sold_units,
                            remaining_units
                        );
                    }
                    crate::log_println!("      Total Profit (all trades): ${:.6}", total_profit);
                }
                Err(e) => {
                    // Enhanced error logging for sell failures
                    let error_str = format!("{:?}", e);
                    let error_msg = format!("{}", e);

                    crate::log_println!(
                        "═══════════════════════════════════════════════════════════"
                    );
                    crate::log_println!("❌ SELL ORDER FAILED - DETAILED ERROR ANALYSIS");
                    crate::log_println!(
                        "═══════════════════════════════════════════════════════════"
                    );
                    crate::log_println!("📊 Order Details:");
                    crate::log_println!("   Token Type: {}", trade.token_type.display_name());
                    crate::log_println!("   Token ID: {}", &trade.token_id[..16]);
                    crate::log_println!("   Condition ID: {}", &trade.condition_id[..16]);
                    crate::log_println!("   Period: {}", trade.market_timestamp);
                    crate::log_println!("   Side: SELL");
                    crate::log_println!("   Units to Sell: {:.6}", units_to_sell);
                    crate::log_println!("   Current Price: ${:.6}", current_price);
                    crate::log_println!(
                        "   Expected Revenue: ${:.6}",
                        current_price * units_to_sell
                    );
                    crate::log_println!("   Sell Attempt: {}/{}", trade.sell_attempts + 1, 3);

                    crate::log_println!("\n🔍 Error Details:");
                    crate::log_println!("   Full Error: {}", error_msg);
                    crate::log_println!("   Error String: {}", error_str);

                    // Check on-chain approval status
                    crate::log_println!("\n🔐 Approval Status Check:");
                    match self.api.check_is_approved_for_all().await {
                        Ok(true) => {
                            crate::log_println!(
                                "   ✅ setApprovalForAll: SET (Exchange is approved on-chain)"
                            );
                        }
                        Ok(false) => {
                            crate::log_println!("   ❌ setApprovalForAll: NOT SET (Exchange is NOT approved on-chain)");
                            crate::log_println!("   💡 This is likely the root cause - no on-chain approval means allowance will always be 0");
                            crate::log_println!("   💡 Solution: Run: cargo run --bin test_allowance -- --approve-only");
                        }
                        Err(e) => {
                            crate::log_println!(
                                "   ⚠️  Could not check setApprovalForAll status: {}",
                                e
                            );
                        }
                    }

                    // Re-check balance and allowance for detailed diagnostics
                    crate::log_println!("\n📊 Current Balance & Allowance:");
                    match self.api.check_balance_allowance(&trade.token_id).await {
                        Ok((balance, allowance)) => {
                            let balance_decimal =
                                balance / rust_decimal::Decimal::from(1_000_000u64);
                            let allowance_decimal =
                                allowance / rust_decimal::Decimal::from(1_000_000u64);
                            let balance_f64 = f64::try_from(balance_decimal).unwrap_or(0.0);
                            let allowance_f64 = f64::try_from(allowance_decimal).unwrap_or(0.0);

                            crate::log_println!("   Token Balance: {:.6} shares", balance_f64);
                            crate::log_println!("   Token Allowance: {:.6} shares", allowance_f64);
                            crate::log_println!("   Required: {:.6} shares", units_to_sell);

                            if balance_f64 < units_to_sell {
                                crate::log_println!("   ❌ INSUFFICIENT BALANCE: Balance ({:.6}) < Required ({:.6})", balance_f64, units_to_sell);
                            } else {
                                crate::log_println!(
                                    "   ✅ Balance sufficient: {:.6} >= {:.6}",
                                    balance_f64,
                                    units_to_sell
                                );
                            }

                            if allowance_f64 < units_to_sell {
                                crate::log_println!("   ❌ INSUFFICIENT ALLOWANCE: Allowance ({:.6}) < Required ({:.6})", allowance_f64, units_to_sell);
                                crate::log_println!("   💡 This is the root cause - Exchange contract cannot spend your tokens");
                                crate::log_println!("   💡 update_balance_allowance only refreshes cache - it doesn't set on-chain approval");
                            } else {
                                crate::log_println!(
                                    "   ✅ Allowance sufficient: {:.6} >= {:.6}",
                                    allowance_f64,
                                    units_to_sell
                                );
                            }
                        }
                        Err(e) => {
                            crate::log_println!("   ⚠️  Could not check balance/allowance: {}", e);
                        }
                    }

                    // Analyze error type
                    crate::log_println!("\n🔍 Error Analysis:");
                    let is_allowance_error = error_str.contains("allowance")
                        || (error_str.contains("not enough") && error_str.contains("allowance"));
                    let is_balance_error =
                        error_str.contains("balance") && !error_str.contains("allowance");
                    let is_fill_error =
                        error_str.contains("couldn't be fully filled") || error_str.contains("FOK");
                    let is_no_orders_error = error_str.contains("No opposing orders")
                        || error_str.contains("no orders found");

                    if is_allowance_error {
                        crate::log_println!("   Error Type: ALLOWANCE ERROR");
                        crate::log_println!(
                            "   Root Cause: Exchange contract is not approved to spend your tokens"
                        );
                        crate::log_println!("   Solution: Set setApprovalForAll on-chain:");
                        crate::log_println!(
                            "      cargo run --bin test_allowance -- --approve-only"
                        );
                    } else if is_balance_error {
                        crate::log_println!("   Error Type: BALANCE ERROR");
                        crate::log_println!(
                            "   Root Cause: You don't have enough tokens in your portfolio"
                        );
                        crate::log_println!("   Solution: Check your Polymarket portfolio - tokens may have been sold/redeemed");
                    } else if is_fill_error {
                        crate::log_println!("   Error Type: FILL ERROR");
                        crate::log_println!(
                            "   Root Cause: Order couldn't be fully filled (FOK/FAK order)"
                        );
                        crate::log_println!("   Solution: This is normal for market orders - partial fills may occur");
                    } else if is_no_orders_error {
                        crate::log_println!("   Error Type: NO ORDERS ERROR");
                        crate::log_println!("   Root Cause: No opposing orders in the order book");
                        crate::log_println!(
                            "   Solution: Wait for market liquidity or try again later"
                        );
                    } else {
                        crate::log_println!("   Error Type: UNKNOWN");
                        crate::log_println!(
                            "   Root Cause: Unknown error - see full error message above"
                        );
                    }

                    crate::log_println!(
                        "\n═══════════════════════════════════════════════════════════"
                    );

                    // Return error - the retry logic in check_pending_trades will handle it
                    // After max attempts, it will mark for claim
                    return Err(e);
                }
            }
        }

        crate::log_println!("═══════════════════════════════════════════════════════════");
        crate::log_println!("");

        Ok(())
    }

    fn redemption_retry_after_epoch_store() -> &'static std::sync::Mutex<Option<u64>> {
        static STORE: std::sync::OnceLock<std::sync::Mutex<Option<u64>>> =
            std::sync::OnceLock::new();
        STORE.get_or_init(|| std::sync::Mutex::new(None))
    }

    fn redemption_manual_request_store() -> &'static std::sync::Mutex<bool> {
        static STORE: std::sync::OnceLock<std::sync::Mutex<bool>> = std::sync::OnceLock::new();
        STORE.get_or_init(|| std::sync::Mutex::new(false))
    }

    fn redemption_last_scheduled_hour_store() -> &'static std::sync::Mutex<Option<u64>> {
        static STORE: std::sync::OnceLock<std::sync::Mutex<Option<u64>>> =
            std::sync::OnceLock::new();
        STORE.get_or_init(|| std::sync::Mutex::new(None))
    }

    fn redemption_last_success_epoch_store() -> &'static std::sync::Mutex<Option<u64>> {
        static STORE: std::sync::OnceLock<std::sync::Mutex<Option<u64>>> =
            std::sync::OnceLock::new();
        STORE.get_or_init(|| std::sync::Mutex::new(None))
    }

    fn redemption_last_sweep_epoch_store() -> &'static std::sync::Mutex<Option<u64>> {
        static STORE: std::sync::OnceLock<std::sync::Mutex<Option<u64>>> =
            std::sync::OnceLock::new();
        STORE.get_or_init(|| std::sync::Mutex::new(None))
    }

    fn redemption_last_auto_trigger_epoch_store() -> &'static std::sync::Mutex<Option<u64>> {
        static STORE: std::sync::OnceLock<std::sync::Mutex<Option<u64>>> =
            std::sync::OnceLock::new();
        STORE.get_or_init(|| std::sync::Mutex::new(None))
    }

    fn redemption_sweep_status_store() -> &'static std::sync::Mutex<RedemptionSweepStatus> {
        static STORE: std::sync::OnceLock<std::sync::Mutex<RedemptionSweepStatus>> =
            std::sync::OnceLock::new();
        STORE.get_or_init(|| std::sync::Mutex::new(RedemptionSweepStatus::default()))
    }

    fn redemption_inflight_tx_store() -> &'static std::sync::Mutex<HashMap<String, String>> {
        static STORE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, String>>> =
            std::sync::OnceLock::new();
        STORE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
    }

    fn redemption_attempt_log_store() -> &'static std::sync::Mutex<VecDeque<Value>> {
        static STORE: std::sync::OnceLock<std::sync::Mutex<VecDeque<Value>>> =
            std::sync::OnceLock::new();
        STORE.get_or_init(|| std::sync::Mutex::new(VecDeque::new()))
    }

    fn redemption_condition_retry_after_store() -> &'static std::sync::Mutex<HashMap<String, u64>> {
        static STORE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, u64>>> =
            std::sync::OnceLock::new();
        STORE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
    }

    fn redemption_positions_warning_epoch_store() -> &'static std::sync::Mutex<Option<u64>> {
        static STORE: std::sync::OnceLock<std::sync::Mutex<Option<u64>>> =
            std::sync::OnceLock::new();
        STORE.get_or_init(|| std::sync::Mutex::new(None))
    }

    fn merge_manual_request_store() -> &'static std::sync::Mutex<bool> {
        static STORE: std::sync::OnceLock<std::sync::Mutex<bool>> = std::sync::OnceLock::new();
        STORE.get_or_init(|| std::sync::Mutex::new(false))
    }

    fn merge_last_scheduled_hour_store() -> &'static std::sync::Mutex<Option<u64>> {
        static STORE: std::sync::OnceLock<std::sync::Mutex<Option<u64>>> =
            std::sync::OnceLock::new();
        STORE.get_or_init(|| std::sync::Mutex::new(None))
    }

    fn merge_last_auto_trigger_epoch_store() -> &'static std::sync::Mutex<Option<u64>> {
        static STORE: std::sync::OnceLock<std::sync::Mutex<Option<u64>>> =
            std::sync::OnceLock::new();
        STORE.get_or_init(|| std::sync::Mutex::new(None))
    }

    fn merge_sweep_status_store() -> &'static std::sync::Mutex<MergeSweepStatus> {
        static STORE: std::sync::OnceLock<std::sync::Mutex<MergeSweepStatus>> =
            std::sync::OnceLock::new();
        STORE.get_or_init(|| std::sync::Mutex::new(MergeSweepStatus::default()))
    }

    fn merge_recent_log_store() -> &'static std::sync::Mutex<VecDeque<Value>> {
        static STORE: std::sync::OnceLock<std::sync::Mutex<VecDeque<Value>>> =
            std::sync::OnceLock::new();
        STORE.get_or_init(|| std::sync::Mutex::new(VecDeque::new()))
    }

    fn auto_trigger_ratio_snapshot_store() -> &'static std::sync::Mutex<Option<(u64, f64)>> {
        static STORE: std::sync::OnceLock<std::sync::Mutex<Option<(u64, f64)>>> =
            std::sync::OnceLock::new();
        STORE.get_or_init(|| std::sync::Mutex::new(None))
    }

    fn auto_trigger_ratio_min_interval_sec() -> u64 {
        300
    }

    fn get_inflight_redemption_tx(condition_id: &str) -> Option<String> {
        Self::redemption_inflight_tx_store()
            .lock()
            .ok()
            .and_then(|guard| guard.get(condition_id).cloned())
    }

    fn set_inflight_redemption_tx(condition_id: &str, tx_id: &str) {
        if let Ok(mut guard) = Self::redemption_inflight_tx_store().lock() {
            guard.insert(condition_id.to_string(), tx_id.to_string());
        }
    }

    fn clear_inflight_redemption_tx(condition_id: &str) {
        if let Ok(mut guard) = Self::redemption_inflight_tx_store().lock() {
            guard.remove(condition_id);
        }
    }

    fn condition_retry_after_epoch(condition_id: &str) -> Option<u64> {
        Self::redemption_condition_retry_after_store()
            .lock()
            .ok()
            .and_then(|guard| guard.get(condition_id).copied())
    }

    fn set_condition_retry_after_epoch(condition_id: &str, retry_after_epoch: u64) {
        if let Ok(mut guard) = Self::redemption_condition_retry_after_store().lock() {
            match guard.get(condition_id).copied() {
                Some(existing) if existing >= retry_after_epoch => {}
                _ => {
                    guard.insert(condition_id.to_string(), retry_after_epoch);
                }
            }
        }
    }

    fn clear_condition_retry_after_epoch(condition_id: &str) {
        if let Ok(mut guard) = Self::redemption_condition_retry_after_store().lock() {
            guard.remove(condition_id);
        }
    }

    fn redemption_recent_attempt_limit() -> usize {
        std::env::var("EVPOLY_REDEMPTION_RECENT_ATTEMPTS_LIMIT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(50)
            .clamp(10, 500)
    }

    fn push_redemption_attempt_log(entry: Value) {
        let limit = Self::redemption_recent_attempt_limit();
        if let Ok(mut guard) = Self::redemption_attempt_log_store().lock() {
            guard.push_back(entry);
            while guard.len() > limit {
                guard.pop_front();
            }
        }
    }

    fn recent_redemption_attempts_json() -> Vec<Value> {
        Self::redemption_attempt_log_store()
            .lock()
            .ok()
            .map(|guard| guard.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    }

    fn parse_rate_limit_pause_seconds(detail: &str) -> Option<u64> {
        let marker = "relayer_quota_pause=";
        let (_, tail) = detail.split_once(marker)?;
        let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse::<u64>().ok()
    }

    fn env_bool_named(name: &str, default: bool) -> bool {
        std::env::var(name)
            .ok()
            .and_then(|v| {
                let normalized = v.trim().to_ascii_lowercase();
                match normalized.as_str() {
                    "1" | "true" | "yes" | "on" => Some(true),
                    "0" | "false" | "no" | "off" => Some(false),
                    _ => None,
                }
            })
            .unwrap_or(default)
    }

    fn env_bool_optional(name: &str) -> Option<bool> {
        std::env::var(name).ok().and_then(|v| {
            let normalized = v.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "1" | "true" | "yes" | "on" => Some(true),
                "0" | "false" | "no" | "off" => Some(false),
                _ => None,
            }
        })
    }

    fn strategy_specific_entry_post_only_alias(strategy_id: &str) -> Option<&'static str> {
        match strategy_id {
            crate::strategy::STRATEGY_ID_PREMARKET_V1 => Some("EVPOLY_ENTRY_POST_ONLY_PREMARKET"),
            STRATEGY_ID_MANUAL_PREMARKET_V1 => Some("EVPOLY_ENTRY_POST_ONLY_MANUAL_PREMARKET"),
            STRATEGY_ID_MANUAL_PREMARKET_TAKER_V1 => {
                Some("EVPOLY_ENTRY_POST_ONLY_MANUAL_PREMARKET_TAKER")
            }
            crate::strategy::STRATEGY_ID_ENDGAME_SWEEP_V1 => Some("EVPOLY_ENTRY_POST_ONLY_ENDGAME"),
            crate::strategy::STRATEGY_ID_EVCURVE_V1 => Some("EVPOLY_ENTRY_POST_ONLY_EVCURVE"),
            crate::strategy::STRATEGY_ID_SESSIONBAND_V1 => {
                Some("EVPOLY_ENTRY_POST_ONLY_SESSIONBAND")
            }
            crate::strategy::STRATEGY_ID_EVSNIPE_V1 => Some("EVPOLY_ENTRY_POST_ONLY_EVSNIPE"),
            crate::strategy::STRATEGY_ID_MM_REWARDS_V1 => Some("EVPOLY_ENTRY_POST_ONLY_MM_REWARDS"),
            crate::strategy::STRATEGY_ID_MM_SPORT_V1 => Some("EVPOLY_ENTRY_POST_ONLY_MM_SPORT"),
            STRATEGY_ID_MANUAL_CHASE_LIMIT_V1 => Some("EVPOLY_ENTRY_POST_ONLY_MANUAL_CHASE"),
            STRATEGY_ID_MANUAL_CHASE_LIMIT_TAKER_V1 => {
                Some("EVPOLY_ENTRY_POST_ONLY_MANUAL_CHASE_TAKER")
            }
            _ => None,
        }
    }

    fn strategy_post_only_default_override(strategy_id: &str) -> Option<bool> {
        match strategy_id {
            // MM rewards defaults to maker-only unless explicitly overridden by env.
            crate::strategy::STRATEGY_ID_MM_REWARDS_V1
            | crate::strategy::STRATEGY_ID_MM_SPORT_V1 => Some(true),
            STRATEGY_ID_MANUAL_PREMARKET_V1 | STRATEGY_ID_MANUAL_CHASE_LIMIT_V1 => Some(true),
            STRATEGY_ID_MANUAL_PREMARKET_TAKER_V1 | STRATEGY_ID_MANUAL_CHASE_LIMIT_TAKER_V1 => {
                Some(false)
            }
            _ => None,
        }
    }

    fn strategy_scoped_env_key(strategy_id: &str, suffix: &str) -> String {
        let id = strategy_id
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() {
                    ch.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect::<String>();
        format!("EVPOLY_{}_{}", id, suffix.trim().to_ascii_uppercase())
    }

    fn entry_post_only_enabled_for_strategy(strategy_id: &str) -> bool {
        let normalized = Self::normalize_strategy_id(Some(strategy_id));
        if normalized == crate::strategy::STRATEGY_ID_MM_SPORT_V1 {
            return true;
        }
        let global_default = Self::env_bool_named("EVPOLY_ENTRY_POST_ONLY", false);
        let default_fallback = Self::strategy_post_only_default_override(normalized.as_str())
            .unwrap_or(global_default);
        let strategy_env_key =
            Self::strategy_scoped_env_key(normalized.as_str(), "ENTRY_POST_ONLY");
        if let Some(value) = Self::env_bool_optional(strategy_env_key.as_str()) {
            return value;
        }
        if let Some(alias_key) = Self::strategy_specific_entry_post_only_alias(normalized.as_str())
        {
            if let Some(value) = Self::env_bool_optional(alias_key) {
                return value;
            }
        }
        default_fallback
    }

    fn startup_balance_check_timeout_ms() -> u64 {
        std::env::var("EVPOLY_STARTUP_BALANCE_CHECK_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(5_000)
            .clamp(500, 60_000)
    }

    fn startup_balance_scan_budget_ms() -> u64 {
        std::env::var("EVPOLY_STARTUP_BALANCE_SCAN_BUDGET_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(20_000)
            .clamp(1_000, 600_000)
    }

    fn redemption_positions_warning_cooldown_sec() -> u64 {
        std::env::var("EVPOLY_REDEMPTION_POSITIONS_WARNING_COOLDOWN_SEC")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(120)
            .clamp(5, 3_600)
    }

    fn should_emit_redemption_positions_warning(now_epoch: u64) -> bool {
        let cooldown = Self::redemption_positions_warning_cooldown_sec();
        if let Ok(mut guard) = Self::redemption_positions_warning_epoch_store().lock() {
            if let Some(last_epoch) = *guard {
                if now_epoch.saturating_sub(last_epoch) < cooldown {
                    return false;
                }
            }
            *guard = Some(now_epoch);
            return true;
        }
        true
    }

    async fn fetch_api_redeemable_positions_for_sweep(
        &self,
    ) -> Vec<crate::api::WalletRedeemablePosition> {
        if self.simulation_mode {
            return Vec::new();
        }
        match self.api.get_wallet_redeemable_positions(None).await {
            Ok(rows) => rows,
            Err(e) => {
                let now_epoch = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if Self::should_emit_redemption_positions_warning(now_epoch) {
                    warn!("Failed API redeemable position scan before sweep: {:#}", e);
                } else {
                    debug!(
                        "Suppressed duplicate redemption /positions warning: {:#}",
                        e
                    );
                }
                Vec::new()
            }
        }
    }

    async fn verify_redeem_effect_for_conditions(
        &self,
        condition_ids: &[String],
    ) -> HashSet<String> {
        let targets = condition_ids
            .iter()
            .map(|condition_id| condition_id.trim().to_string())
            .filter(|condition_id| !condition_id.is_empty())
            .collect::<HashSet<_>>();
        if targets.is_empty() {
            return HashSet::new();
        }
        if self.simulation_mode {
            return targets;
        }

        const VERIFY_ATTEMPTS: usize = 3;
        const VERIFY_POLL_MS: u64 = 2_000;
        let mut verified: HashSet<String> = HashSet::new();
        for attempt in 0..VERIFY_ATTEMPTS {
            match self.api.get_wallet_redeemable_positions(None).await {
                Ok(rows) => {
                    let live = rows
                        .into_iter()
                        .map(|row| row.condition_id)
                        .collect::<HashSet<_>>();
                    verified = targets
                        .difference(&live)
                        .cloned()
                        .collect::<HashSet<String>>();
                    if verified.len() == targets.len() {
                        break;
                    }
                }
                Err(err) => {
                    warn!(
                        "Redemption effect verification API scan failed (attempt {}/{}): {}",
                        attempt + 1,
                        VERIFY_ATTEMPTS,
                        err
                    );
                }
            }
            if attempt + 1 < VERIFY_ATTEMPTS {
                tokio::time::sleep(tokio::time::Duration::from_millis(VERIFY_POLL_MS)).await;
            }
        }

        verified
    }

    async fn verify_redeem_effect_for_condition(&self, condition_id: &str) -> bool {
        let condition_id_trimmed = condition_id.trim();
        if condition_id_trimmed.is_empty() {
            return false;
        }
        let condition_ids = vec![condition_id_trimmed.to_string()];
        self.verify_redeem_effect_for_conditions(condition_ids.as_slice())
            .await
            .contains(condition_id_trimmed)
    }

    async fn check_balance_only_with_timeout(
        &self,
        token_id: &str,
    ) -> Result<rust_decimal::Decimal> {
        let timeout_ms = Self::startup_balance_check_timeout_ms();
        match tokio::time::timeout(
            tokio::time::Duration::from_millis(timeout_ms),
            self.api.check_balance_only(token_id),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(anyhow!(
                "startup balance check timeout after {}ms for token {}",
                timeout_ms,
                &token_id[..token_id.len().min(16)]
            )),
        }
    }

    fn redemption_sweep_enabled() -> bool {
        true
    }

    fn redemption_schedule_enabled() -> bool {
        true
    }

    fn redemption_auto_trigger_enabled() -> bool {
        true
    }

    fn redemption_auto_trigger_pending_threshold() -> usize {
        std::env::var("EVPOLY_REDEMPTION_AUTO_TRIGGER_PENDING_THRESHOLD")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(10)
            .clamp(1, 10_000)
    }

    fn redemption_auto_trigger_available_ratio_threshold() -> f64 {
        std::env::var("EVPOLY_REDEMPTION_AUTO_TRIGGER_AVAILABLE_RATIO_THRESHOLD")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.20)
            .clamp(0.01, 0.99)
    }

    fn redemption_auto_trigger_cooldown_sec() -> u64 {
        std::env::var("EVPOLY_REDEMPTION_AUTO_TRIGGER_COOLDOWN_SEC")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(900)
            .clamp(60, 86_400)
    }

    fn redemption_auto_trigger_balance_timeout_ms() -> u64 {
        std::env::var("EVPOLY_REDEMPTION_AUTO_TRIGGER_BALANCE_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(4_000)
            .clamp(250, 30_000)
    }

    fn redemption_startup_catchup_enabled() -> bool {
        Self::env_bool_named("EVPOLY_REDEMPTION_STARTUP_CATCHUP_ENABLE", true)
    }

    fn redemption_restore_pending_from_db_enabled() -> bool {
        Self::env_bool_named("EVPOLY_REDEMPTION_RESTORE_PENDING_FROM_DB_ENABLE", false)
    }

    fn redemption_startup_catchup_stale_minutes() -> u64 {
        std::env::var("EVPOLY_REDEMPTION_STARTUP_CATCHUP_STALE_MINUTES")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(65)
            .max(1)
    }

    fn redemption_submit_startup_catchup_enabled() -> bool {
        Self::env_bool_named("EVPOLY_REDEMPTION_STARTUP_SUBMIT_ENABLE", false)
    }

    fn redemption_sweep_minute() -> u32 {
        std::env::var("EVPOLY_REDEMPTION_SWEEP_MINUTE")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(22)
            .min(59)
    }

    fn redemption_sweep_interval_hours() -> u64 {
        std::env::var("EVPOLY_REDEMPTION_SWEEP_INTERVAL_HOURS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1)
            .clamp(1, 24)
    }

    fn redemption_max_conditions_per_sweep() -> usize {
        std::env::var("EVPOLY_REDEMPTION_MAX_CONDITIONS_PER_SWEEP")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(3)
            .clamp(1, 128)
    }

    fn redemption_max_conditions_per_manual_sweep() -> usize {
        std::env::var("EVPOLY_REDEMPTION_MAX_CONDITIONS_PER_MANUAL_SWEEP")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or_else(Self::redemption_max_conditions_per_sweep)
            .clamp(Self::redemption_max_conditions_per_sweep(), 512)
    }

    fn redemption_condition_retry_seconds() -> u64 {
        std::env::var("EVPOLY_REDEMPTION_CONDITION_RETRY_SECONDS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(3600)
            .clamp(60, 86_400)
    }

    fn redemption_balance_epsilon_shares() -> f64 {
        std::env::var("EVPOLY_REDEMPTION_BALANCE_EPSILON_SHARES")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.01)
            .clamp(0.0, 1.0)
    }

    fn redemption_batch_submit_enabled() -> bool {
        true
    }

    fn redemption_batch_conditions_per_request() -> usize {
        10
    }

    fn merge_batch_conditions_per_request() -> usize {
        10
    }

    fn merge_sweep_enabled() -> bool {
        true
    }

    fn merge_schedule_enabled() -> bool {
        true
    }

    fn merge_auto_trigger_enabled() -> bool {
        true
    }

    fn merge_auto_trigger_pending_threshold() -> usize {
        std::env::var("EVPOLY_MERGE_AUTO_TRIGGER_PENDING_THRESHOLD")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(10)
            .clamp(1, 10_000)
    }

    fn merge_auto_trigger_available_ratio_threshold() -> f64 {
        std::env::var("EVPOLY_MERGE_AUTO_TRIGGER_AVAILABLE_RATIO_THRESHOLD")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.20)
            .clamp(0.01, 0.99)
    }

    fn merge_auto_trigger_cooldown_sec() -> u64 {
        std::env::var("EVPOLY_MERGE_AUTO_TRIGGER_COOLDOWN_SEC")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(900)
            .clamp(60, 86_400)
    }

    fn merge_sweep_minute() -> u32 {
        std::env::var("EVPOLY_MERGE_SWEEP_MINUTE")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(40)
            .min(59)
    }

    fn merge_sweep_interval_hours() -> u64 {
        std::env::var("EVPOLY_MERGE_SWEEP_INTERVAL_HOURS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1)
            .clamp(1, 24)
    }

    fn merge_min_pairs_shares() -> f64 {
        std::env::var("EVPOLY_MERGE_MIN_PAIRS_SHARES")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(1.0)
            .clamp(0.01, 1_000_000.0)
    }

    fn merge_max_conditions_per_sweep() -> usize {
        std::env::var("EVPOLY_MERGE_MAX_CONDITIONS_PER_SWEEP")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0)
            .min(10_000)
    }

    fn merge_recent_log_limit() -> usize {
        std::env::var("EVPOLY_MERGE_RECENT_LOG_LIMIT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(50)
            .clamp(10, 500)
    }

    fn push_merge_recent_log(entry: Value) {
        let limit = Self::merge_recent_log_limit();
        if let Ok(mut guard) = Self::merge_recent_log_store().lock() {
            guard.push_back(entry);
            while guard.len() > limit {
                guard.pop_front();
            }
        }
    }

    fn merge_recent_logs_json() -> Vec<Value> {
        Self::merge_recent_log_store()
            .lock()
            .ok()
            .map(|guard| guard.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    }

    fn mm_dust_sell_enabled() -> bool {
        Self::env_bool_named("EVPOLY_MM_DUST_SELL_ENABLE", true)
    }

    fn mm_dust_sell_max_notional_usd() -> f64 {
        std::env::var("EVPOLY_MM_DUST_SELL_MAX_NOTIONAL_USD")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(20.0)
            .clamp(0.10, 1_000.0)
    }

    fn mm_dust_sell_max_shares() -> f64 {
        std::env::var("EVPOLY_MM_DUST_SELL_MAX_SHARES")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(25.0)
            .clamp(0.01, 10_000.0)
    }

    fn mm_dust_sell_scan_limit() -> usize {
        std::env::var("EVPOLY_MM_DUST_SELL_SCAN_LIMIT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(2_000)
            .clamp(1, 20_000)
    }

    fn mm_dust_sell_max_orders() -> usize {
        std::env::var("EVPOLY_MM_DUST_SELL_MAX_ORDERS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(12)
            .clamp(1, 1_000)
    }

    fn mm_dust_sell_min_submit_shares() -> f64 {
        std::env::var("EVPOLY_MM_DUST_SELL_MIN_SUBMIT_SHARES")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.01)
            .clamp(0.0001, 100.0)
    }

    fn mm_dust_sell_balance_haircut() -> f64 {
        std::env::var("EVPOLY_MM_DUST_SELL_BALANCE_HAIRCUT")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.98)
            .clamp(0.10, 1.0)
    }

    fn mm_dust_sell_balance_timeout_ms() -> u64 {
        std::env::var("EVPOLY_MM_DUST_SELL_BALANCE_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(2_500)
            .clamp(250, 15_000)
    }

    fn mm_dust_sell_inter_order_delay_ms() -> u64 {
        std::env::var("EVPOLY_MM_DUST_SELL_INTER_ORDER_DELAY_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1_000)
            .clamp(0, 30_000)
    }

    fn parse_merge_market_selector(raw: &str) -> Option<(String, Option<String>)> {
        let mut value = raw.trim().trim_matches('/').to_string();
        if value.is_empty() {
            return None;
        }
        if let Some((_, suffix)) = value.split_once("polymarket.com/event/") {
            value = suffix.trim().trim_matches('/').to_string();
        } else if let Some((_, suffix)) = value.split_once("/event/") {
            value = suffix.trim().trim_matches('/').to_string();
        }
        if let Some(suffix) = value.strip_prefix("event/") {
            value = suffix.trim().trim_matches('/').to_string();
        }
        let parts = value
            .split('/')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        match parts.as_slice() {
            [single] => Some((single.to_string(), None)),
            [event_slug, market_slug, ..] => {
                Some((event_slug.to_string(), Some(market_slug.to_string())))
            }
            _ => None,
        }
    }

    fn merge_market_selectors_from_env() -> Vec<(String, Option<String>)> {
        let mut raw_entries: Vec<String> = Vec::new();
        if let Ok(raw) = std::env::var("EVPOLY_MM_SINGLE_MARKET_SLUGS") {
            raw_entries.extend(
                raw.split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(ToString::to_string),
            );
        }
        if let Ok(raw) = std::env::var("EVPOLY_MM_SINGLE_MARKET_SLUG") {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                raw_entries.push(trimmed.to_string());
            }
        }
        let mut seen: HashSet<String> = HashSet::new();
        let mut selectors: Vec<(String, Option<String>)> = Vec::new();
        for raw in raw_entries {
            if let Some((event_slug, market_slug)) = Self::parse_merge_market_selector(&raw) {
                let dedupe_key = if let Some(ms) = market_slug.as_ref() {
                    format!("{}/{}", event_slug, ms)
                } else {
                    event_slug.clone()
                };
                if seen.insert(dedupe_key) {
                    selectors.push((event_slug, market_slug));
                }
            }
        }
        selectors
    }

    fn seed_merge_condition_tokens_from_wallet_rows(
        condition_tokens: &mut HashMap<String, HashSet<String>>,
        rows: Vec<MmWalletInventoryRow>,
    ) {
        for row in rows {
            if !row.wallet_shares.is_finite() || row.wallet_shares <= 0.0 {
                continue;
            }
            let condition_id = row.condition_id.trim();
            let token_id = row.token_id.trim();
            if condition_id.is_empty() || token_id.is_empty() {
                continue;
            }
            condition_tokens
                .entry(condition_id.to_string())
                .or_default()
                .insert(token_id.to_string());
        }
    }

    fn merge_extract_market_token_ids(market: &Market) -> Vec<String> {
        let mut token_ids: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        if let Some(tokens) = market.tokens.as_ref() {
            for token in tokens {
                let token_id = token.token_id.trim();
                if token_id.is_empty() {
                    continue;
                }
                if seen.insert(token_id.to_string()) {
                    token_ids.push(token_id.to_string());
                }
            }
        }
        if token_ids.len() >= 2 {
            return token_ids;
        }

        if let Some(raw_ids) = market.clob_token_ids.as_ref() {
            if let Ok(parsed) = serde_json::from_str::<Vec<String>>(raw_ids.as_str()) {
                for token_id in parsed {
                    let normalized = token_id.trim();
                    if normalized.is_empty() {
                        continue;
                    }
                    if seen.insert(normalized.to_string()) {
                        token_ids.push(normalized.to_string());
                    }
                }
            }
        }

        token_ids
    }

    fn auto_trigger_cooldown_remaining_sec(
        store: &std::sync::Mutex<Option<u64>>,
        now_epoch: u64,
        cooldown_sec: u64,
    ) -> u64 {
        store
            .lock()
            .ok()
            .and_then(|guard| *guard)
            .map(|last| {
                let elapsed = now_epoch.saturating_sub(last);
                cooldown_sec.saturating_sub(elapsed)
            })
            .unwrap_or(0)
    }

    fn try_claim_auto_trigger_slot(
        store: &std::sync::Mutex<Option<u64>>,
        now_epoch: u64,
        cooldown_sec: u64,
    ) -> bool {
        if let Ok(mut guard) = store.lock() {
            if let Some(last) = *guard {
                if now_epoch.saturating_sub(last) < cooldown_sec {
                    return false;
                }
            }
            *guard = Some(now_epoch);
            return true;
        }
        false
    }

    fn estimate_pending_inventory_notional_usd(pending_trades: &[(String, PendingTrade)]) -> f64 {
        pending_trades
            .iter()
            .filter_map(|(_, trade)| {
                if trade.sold || trade.redemption_abandoned {
                    return None;
                }
                let cost = trade.units * trade.purchase_price;
                if cost.is_finite() && cost > 0.0 {
                    Some(cost)
                } else if trade.investment_amount.is_finite() && trade.investment_amount > 0.0 {
                    Some(trade.investment_amount)
                } else {
                    None
                }
            })
            .sum::<f64>()
            .max(0.0)
    }

    async fn available_ratio_from_pending(
        &self,
        pending_trades: &[(String, PendingTrade)],
        timeout_ms: u64,
    ) -> Option<(f64, f64, f64)> {
        let locked_notional = Self::estimate_pending_inventory_notional_usd(pending_trades);
        let now_epoch = chrono::Utc::now().timestamp().max(0) as u64;
        if let Ok(guard) = Self::auto_trigger_ratio_snapshot_store().lock() {
            if let Some((cached_epoch, cached_usdc_balance)) = *guard {
                if now_epoch.saturating_sub(cached_epoch)
                    < Self::auto_trigger_ratio_min_interval_sec()
                {
                    let portfolio_estimate = (cached_usdc_balance + locked_notional).max(0.0);
                    if portfolio_estimate > 0.0 {
                        let available_ratio =
                            (cached_usdc_balance / portfolio_estimate).clamp(0.0, 1.0);
                        return Some((available_ratio, cached_usdc_balance, portfolio_estimate));
                    }
                }
            }
        }

        let usdc_balance = match tokio::time::timeout(
            tokio::time::Duration::from_millis(timeout_ms),
            self.api.check_usdc_balance_allowance(),
        )
        .await
        {
            Ok(Ok((usdc_balance_raw, _allowance))) => {
                f64::try_from(usdc_balance_raw / rust_decimal::Decimal::from(1_000_000u64))
                    .unwrap_or(0.0)
                    .max(0.0)
            }
            _ => return None,
        };

        if let Ok(mut guard) = Self::auto_trigger_ratio_snapshot_store().lock() {
            *guard = Some((now_epoch, usdc_balance));
        }

        let portfolio_estimate = (usdc_balance + locked_notional).max(0.0);
        if portfolio_estimate <= 0.0 {
            return None;
        }
        let available_ratio = (usdc_balance / portfolio_estimate).clamp(0.0, 1.0);
        Some((available_ratio, usdc_balance, portfolio_estimate))
    }

    fn estimate_merge_pair_conditions(rows: &[MmWalletInventoryRow]) -> usize {
        let mut token_counts: HashMap<&str, usize> = HashMap::new();
        for row in rows {
            if !row.wallet_shares.is_finite() || row.wallet_shares <= 0.0 {
                continue;
            }
            let condition_id = row.condition_id.trim();
            let token_id = row.token_id.trim();
            if condition_id.is_empty() || token_id.is_empty() {
                continue;
            }
            let entry = token_counts.entry(condition_id).or_insert(0);
            *entry = entry.saturating_add(1);
        }
        token_counts.values().filter(|count| **count >= 2).count()
    }

    fn request_manual_merge_sweep_internal() -> bool {
        if let Ok(mut guard) = Self::merge_manual_request_store().lock() {
            let was_pending = *guard;
            *guard = true;
            return !was_pending;
        }
        false
    }

    fn take_manual_merge_request() -> bool {
        if let Ok(mut guard) = Self::merge_manual_request_store().lock() {
            let requested = *guard;
            *guard = false;
            return requested;
        }
        false
    }

    fn manual_merge_request_pending() -> bool {
        Self::merge_manual_request_store()
            .lock()
            .ok()
            .map(|guard| *guard)
            .unwrap_or(false)
    }

    fn try_claim_merge_scheduled_slot(now_epoch: u64) -> bool {
        let minute = (now_epoch / 60) % 60;
        // Merge scheduler is invoked from the redemption scheduler pass, so allow
        // "at/after configured minute" rather than exact minute equality.
        if (minute as u32) < Self::merge_sweep_minute() {
            return false;
        }
        let hour_key = now_epoch / 3600;
        let interval_hours = Self::merge_sweep_interval_hours();
        if hour_key % interval_hours != 0 {
            return false;
        }
        if let Ok(mut guard) = Self::merge_last_scheduled_hour_store().lock() {
            if guard.as_ref().copied() == Some(hour_key) {
                return false;
            }
            *guard = Some(hour_key);
            return true;
        }
        false
    }

    fn update_merge_sweep_status(
        started_ts: u64,
        finished_ts: u64,
        trigger: &str,
        reason: &str,
        scanned_conditions: usize,
        merge_candidates: usize,
        submitted_merges: usize,
        successful_merges: usize,
    ) {
        if let Ok(mut status) = Self::merge_sweep_status_store().lock() {
            status.last_started_ts = Some(started_ts);
            status.last_finished_ts = Some(finished_ts);
            status.last_trigger = Some(trigger.to_string());
            status.last_reason = Some(reason.to_string());
            status.last_scanned_conditions = scanned_conditions;
            status.last_merge_candidates = merge_candidates;
            status.last_submitted_merges = submitted_merges;
            status.last_successful_merges = successful_merges;
        }
    }

    fn current_merge_sweep_status_json() -> Value {
        let status = Self::merge_sweep_status_store()
            .lock()
            .ok()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        json!({
            "sweep_enabled": Self::merge_sweep_enabled(),
            "schedule_enabled": Self::merge_schedule_enabled(),
            "auto_trigger_enabled": Self::merge_auto_trigger_enabled(),
            "auto_trigger_pending_threshold": Self::merge_auto_trigger_pending_threshold(),
            "auto_trigger_available_ratio_threshold": Self::merge_auto_trigger_available_ratio_threshold(),
            "auto_trigger_cooldown_sec": Self::merge_auto_trigger_cooldown_sec(),
            "sweep_minute": Self::merge_sweep_minute(),
            "sweep_interval_hours": Self::merge_sweep_interval_hours(),
            "manual_request_pending": Self::manual_merge_request_pending(),
            "min_pairs_shares": Self::merge_min_pairs_shares(),
            "max_conditions_per_sweep": Self::merge_max_conditions_per_sweep(),
            "last_started_ts": status.last_started_ts,
            "last_finished_ts": status.last_finished_ts,
            "last_trigger": status.last_trigger,
            "last_reason": status.last_reason,
            "last_scanned_conditions": status.last_scanned_conditions,
            "last_merge_candidates": status.last_merge_candidates,
            "last_submitted_merges": status.last_submitted_merges,
            "last_successful_merges": status.last_successful_merges,
            "recent_logs": Self::merge_recent_logs_json()
        })
    }

    fn set_last_redemption_success_epoch(now_epoch: u64) {
        if let Ok(mut guard) = Self::redemption_last_success_epoch_store().lock() {
            *guard = Some(now_epoch);
        }
    }

    fn set_last_redemption_sweep_epoch(now_epoch: u64) {
        if let Ok(mut guard) = Self::redemption_last_sweep_epoch_store().lock() {
            *guard = Some(now_epoch);
        }
    }

    fn should_run_startup_catchup(now_epoch: u64) -> bool {
        if !Self::redemption_startup_catchup_enabled() {
            return false;
        }
        let stale_after = Self::redemption_startup_catchup_stale_minutes().saturating_mul(60);
        let last_success = Self::redemption_last_success_epoch_store()
            .lock()
            .ok()
            .and_then(|guard| *guard);
        last_success
            .map(|ts| now_epoch.saturating_sub(ts) >= stale_after)
            .unwrap_or(true)
    }

    fn try_claim_scheduled_sweep_slot(now_epoch: u64) -> bool {
        let minute = (now_epoch / 60) % 60;
        if minute as u32 != Self::redemption_sweep_minute() {
            return false;
        }
        let hour_key = now_epoch / 3600;
        let interval_hours = Self::redemption_sweep_interval_hours();
        if hour_key % interval_hours != 0 {
            return false;
        }
        if let Ok(mut guard) = Self::redemption_last_scheduled_hour_store().lock() {
            if guard.as_ref().copied() == Some(hour_key) {
                return false;
            }
            *guard = Some(hour_key);
            return true;
        }
        false
    }

    fn request_manual_redemption_sweep_internal() -> bool {
        if let Ok(mut guard) = Self::redemption_manual_request_store().lock() {
            let was_pending = *guard;
            *guard = true;
            return !was_pending;
        }
        false
    }

    fn take_manual_redemption_request() -> bool {
        if let Ok(mut guard) = Self::redemption_manual_request_store().lock() {
            let requested = *guard;
            *guard = false;
            return requested;
        }
        false
    }

    fn manual_redemption_request_pending() -> bool {
        Self::redemption_manual_request_store()
            .lock()
            .ok()
            .map(|guard| *guard)
            .unwrap_or(false)
    }

    fn update_redemption_sweep_status(
        started_ts: u64,
        finished_ts: u64,
        trigger: &str,
        reason: &str,
        processed_conditions: usize,
        submitted_conditions: usize,
        successful_conditions: usize,
        skipped_duplicates: usize,
        pause_remaining_sec: Option<u64>,
    ) {
        if let Ok(mut status) = Self::redemption_sweep_status_store().lock() {
            status.last_started_ts = Some(started_ts);
            status.last_finished_ts = Some(finished_ts);
            status.last_trigger = Some(trigger.to_string());
            status.last_reason = Some(reason.to_string());
            status.last_processed_conditions = processed_conditions;
            status.last_submitted_conditions = submitted_conditions;
            status.last_successful_conditions = successful_conditions;
            status.last_skipped_duplicates = skipped_duplicates;
            status.last_pause_remaining_sec = pause_remaining_sec;
        }
    }

    fn current_redemption_sweep_status_json() -> Value {
        let status = Self::redemption_sweep_status_store()
            .lock()
            .ok()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let pause_remaining_sec = Self::redemption_retry_after_epoch_store()
            .lock()
            .ok()
            .and_then(|guard| *guard)
            .and_then(|retry_after| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                retry_after.checked_sub(now)
            });
        let last_successful_redemption_ts = Self::redemption_last_success_epoch_store()
            .lock()
            .ok()
            .and_then(|guard| *guard);
        let last_sweep_ts = Self::redemption_last_sweep_epoch_store()
            .lock()
            .ok()
            .and_then(|guard| *guard);
        let minute = Self::redemption_sweep_minute();
        json!({
            "sweep_enabled": Self::redemption_sweep_enabled(),
            "schedule_enabled": Self::redemption_schedule_enabled(),
            "auto_trigger_enabled": Self::redemption_auto_trigger_enabled(),
            "auto_trigger_pending_threshold": Self::redemption_auto_trigger_pending_threshold(),
            "auto_trigger_available_ratio_threshold": Self::redemption_auto_trigger_available_ratio_threshold(),
            "auto_trigger_cooldown_sec": Self::redemption_auto_trigger_cooldown_sec(),
            "sweep_minute": minute,
            "startup_catchup_enabled": Self::redemption_startup_catchup_enabled(),
            "startup_submit_enabled": Self::redemption_submit_startup_catchup_enabled(),
            "startup_catchup_stale_minutes": Self::redemption_startup_catchup_stale_minutes(),
            "manual_request_pending": Self::redemption_manual_request_store().lock().ok().map(|g| *g).unwrap_or(false),
            "max_conditions_per_sweep": Self::redemption_max_conditions_per_sweep(),
            "condition_retry_seconds": Self::redemption_condition_retry_seconds(),
            "pause_remaining_sec": pause_remaining_sec,
            "last_successful_redemption_ts": last_successful_redemption_ts,
            "last_sweep_ts": last_sweep_ts,
            "last_started_ts": status.last_started_ts,
            "last_finished_ts": status.last_finished_ts,
            "last_trigger": status.last_trigger,
            "last_reason": status.last_reason,
            "last_processed_conditions": status.last_processed_conditions,
            "last_submitted_conditions": status.last_submitted_conditions,
            "last_successful_conditions": status.last_successful_conditions,
            "last_skipped_duplicates": status.last_skipped_duplicates,
            "last_pause_remaining_sec": status.last_pause_remaining_sec,
            "recent_attempts": Self::recent_redemption_attempts_json()
        })
    }

    fn parse_relayer_reset_seconds(error_text: &str) -> Option<u64> {
        let marker = "resets in ";
        let idx = error_text.find(marker)?;
        let tail = error_text[idx + marker.len()..].trim();

        let normalized = tail.to_ascii_lowercase();
        let mut total_seconds = 0_u64;
        for token in normalized.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
            if token.is_empty() {
                continue;
            }
            let digits: String = token.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                continue;
            }
            let value = digits.parse::<u64>().ok()?;
            let suffix = token[digits.len()..].trim_matches('.');
            if suffix.starts_with('h') || suffix.starts_with("hour") {
                total_seconds = total_seconds.saturating_add(value.saturating_mul(3600));
            } else if suffix.starts_with('m') || suffix.starts_with("min") {
                total_seconds = total_seconds.saturating_add(value.saturating_mul(60));
            } else if suffix.starts_with('s') || suffix.starts_with("sec") {
                total_seconds = total_seconds.saturating_add(value);
            } else if suffix.is_empty() {
                return Some(value);
            }
        }

        if total_seconds > 0 {
            Some(total_seconds)
        } else {
            None
        }
    }

    fn redemption_pause_min_seconds() -> u64 {
        std::env::var("EVPOLY_REDEMPTION_PAUSE_MIN_SECONDS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30)
            .clamp(5, 3600)
    }

    fn redemption_pause_max_seconds() -> u64 {
        std::env::var("EVPOLY_REDEMPTION_PAUSE_MAX_SECONDS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(7_200)
            .clamp(30, 86_400)
    }

    fn sanitize_redemption_pause_seconds(pause_seconds: u64) -> u64 {
        let min_pause = Self::redemption_pause_min_seconds();
        let max_pause = Self::redemption_pause_max_seconds().max(min_pause);
        pause_seconds.clamp(min_pause, max_pause)
    }

    fn set_redemption_retry_pause(&self, pause_seconds: u64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let sanitized_pause_seconds = Self::sanitize_redemption_pause_seconds(pause_seconds);
        let retry_after = now.saturating_add(sanitized_pause_seconds);

        if let Ok(mut guard) = Self::redemption_retry_after_epoch_store().lock() {
            match *guard {
                Some(existing) if existing >= retry_after => {}
                _ => *guard = Some(retry_after),
            }
        }
    }

    fn redemption_retry_pause_remaining_seconds(&self) -> Option<u64> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        if let Ok(mut guard) = Self::redemption_retry_after_epoch_store().lock() {
            if let Some(retry_after) = *guard {
                if retry_after > now {
                    return Some(retry_after - now);
                }
                *guard = None;
            }
        }
        None
    }

    fn is_relayer_quota_error(error_text: &str) -> bool {
        error_text.contains("quota exceeded")
            || error_text.contains("Too Many Requests")
            || error_text.contains("error code: 1015")
    }

    pub fn request_manual_redemption_sweep(&self) -> Value {
        let accepted = Self::request_manual_redemption_sweep_internal();
        let status = Self::current_redemption_sweep_status_json();
        json!({
            "accepted": accepted,
            "queued": true,
            "status": status
        })
    }

    pub fn redemption_sweep_status(&self) -> Value {
        Self::current_redemption_sweep_status_json()
    }

    pub fn request_manual_merge_sweep(&self) -> Value {
        let accepted = Self::request_manual_merge_sweep_internal();
        let status = Self::current_merge_sweep_status_json();
        json!({
            "accepted": accepted,
            "queued": true,
            "status": status
        })
    }

    pub fn merge_sweep_status(&self) -> Value {
        Self::current_merge_sweep_status_json()
    }

    pub fn reconcile_strategy_inventory_from_wallet_tables(
        &self,
        strategy_id: &str,
        min_drift_shares: Option<f64>,
        limit: Option<usize>,
    ) -> Result<Value> {
        let Some(db) = self.tracking_db.as_ref() else {
            return Err(anyhow!("tracking_db_unavailable"));
        };
        let strategy_id_normalized = strategy_id.trim().to_ascii_lowercase();
        if strategy_id_normalized != crate::strategy::STRATEGY_ID_MM_REWARDS_V1
            && strategy_id_normalized != crate::strategy::STRATEGY_ID_MM_SPORT_V1
        {
            return Err(anyhow!(
                "unsupported_strategy_id: {} (allowed: {}, {})",
                strategy_id,
                crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
                crate::strategy::STRATEGY_ID_MM_SPORT_V1
            ));
        }
        let min_drift_shares = min_drift_shares
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(1.0);
        let limit = limit.unwrap_or(256).clamp(1, 10_000);
        let rows = db.reconcile_mm_inventory_drift_from_wallet_tables(
            strategy_id_normalized.as_str(),
            min_drift_shares,
            limit,
        )?;
        Ok(json!({
            "ok": true,
            "strategy_id": strategy_id_normalized,
            "min_drift_shares": min_drift_shares,
            "count": rows.len(),
            "rows": rows.into_iter().map(|row| {
                json!({
                    "consume_key": row.consume_key,
                    "action": row.action,
                    "condition_id": row.condition_id,
                    "token_id": row.token_id,
                    "market_slug": row.market_slug,
                    "timeframe": row.timeframe,
                    "symbol": row.symbol,
                    "mode": row.mode,
                    "db_inventory_shares_before": row.db_inventory_shares_before,
                    "wallet_inventory_shares": row.wallet_inventory_shares,
                    "drift_shares_before": row.drift_shares_before,
                    "requested_consume_shares": row.requested_consume_shares,
                    "applied_consume_shares": row.applied_consume_shares,
                    "requested_add_shares": row.requested_add_shares,
                    "applied_add_shares": row.applied_add_shares,
                    "db_inventory_shares_after": row.db_inventory_shares_after,
                    "applied_cost_usd": row.applied_cost_usd,
                    "last_wallet_activity_ts_ms": row.last_wallet_activity_ts_ms,
                    "last_wallet_activity_type": row.last_wallet_activity_type
                })
            }).collect::<Vec<_>>()
        }))
    }

    pub fn reconcile_mm_inventory_from_wallet_tables(
        &self,
        min_drift_shares: Option<f64>,
        limit: Option<usize>,
    ) -> Result<Value> {
        self.reconcile_strategy_inventory_from_wallet_tables(
            crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
            min_drift_shares,
            limit,
        )
    }

    pub async fn dust_sell_mm_inventory(
        &self,
        max_notional_usd: Option<f64>,
        max_shares: Option<f64>,
        scan_limit: Option<usize>,
        max_orders: Option<usize>,
    ) -> Result<Value> {
        let Some(db) = self.tracking_db.as_ref() else {
            return Err(anyhow!("tracking_db_unavailable"));
        };
        if !Self::mm_dust_sell_enabled() {
            return Ok(json!({
                "ok": true,
                "enabled": false,
                "reason": "mm_dust_sell_disabled"
            }));
        }

        let max_notional_usd = max_notional_usd
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or_else(Self::mm_dust_sell_max_notional_usd);
        let max_shares = max_shares
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or_else(Self::mm_dust_sell_max_shares);
        let scan_limit = scan_limit
            .unwrap_or_else(Self::mm_dust_sell_scan_limit)
            .clamp(1, 20_000);
        let max_orders = max_orders
            .unwrap_or_else(Self::mm_dust_sell_max_orders)
            .clamp(1, 1_000);
        let min_submit_shares = Self::mm_dust_sell_min_submit_shares();
        let balance_haircut = Self::mm_dust_sell_balance_haircut();
        let balance_timeout_ms = Self::mm_dust_sell_balance_timeout_ms();
        let inter_order_delay_ms = Self::mm_dust_sell_inter_order_delay_ms();

        if let Err(e) = self.api.authenticate().await {
            let error_text = format!("clob_auth_failed: {}", e);
            log_event(
                "mm_dust_sell_auth_failed",
                json!({
                    "error": error_text,
                    "strategy_id": crate::strategy::STRATEGY_ID_MM_REWARDS_V1
                }),
            );
            return Ok(json!({
                "ok": false,
                "enabled": true,
                "error": error_text,
                "strategy_id": crate::strategy::STRATEGY_ID_MM_REWARDS_V1
            }));
        }

        let estimate_notional = |row: &MmWalletInventoryRow| -> f64 {
            if row.wallet_value_usd.is_finite() && row.wallet_value_usd > 0.0 {
                row.wallet_value_usd
            } else {
                row.wallet_avg_price
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .map(|price| price * row.wallet_shares.max(0.0))
                    .unwrap_or(0.0)
            }
        };

        let scanned_rows = db
            .list_mm_wallet_inventory_by_strategy(
                crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
                scan_limit,
            )?
            .into_iter()
            .filter(|row| {
                row.wallet_shares.is_finite()
                    && row.wallet_shares > min_submit_shares
                    && row.wallet_shares <= max_shares + 1e-9
                    && !row.condition_id.trim().is_empty()
                    && !row.token_id.trim().is_empty()
                    && {
                        let est = estimate_notional(row);
                        est.is_finite() && est > 0.0 && est <= max_notional_usd + 1e-9
                    }
            })
            .collect::<Vec<_>>();

        let mut candidates = scanned_rows;
        candidates.sort_by(|a, b| {
            estimate_notional(a)
                .partial_cmp(&estimate_notional(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let candidate_count = candidates.len();
        let mut capped_candidates = 0_usize;
        let mut attempted_orders = 0_usize;
        let mut submitted_orders = 0_usize;
        let mut skipped_candidates = 0_usize;
        let mut failed_orders = 0_usize;
        let mut result_rows: Vec<Value> = Vec::new();
        let mut reason_counts: HashMap<String, usize> = HashMap::new();
        let mut bump_reason = |key: &str| {
            let entry = reason_counts.entry(key.to_string()).or_insert(0);
            *entry = entry.saturating_add(1);
        };

        for row in candidates {
            if attempted_orders >= max_orders {
                capped_candidates = capped_candidates.saturating_add(1);
                bump_reason("capped:max_orders");
                continue;
            }

            let condition_id = row.condition_id.trim().to_string();
            let token_id = row.token_id.trim().to_string();
            let row_estimated_notional = estimate_notional(&row);
            let row_market_slug = row.market_slug.clone().unwrap_or_default();

            let mut live_balance_source = "live".to_string();
            let live_balance_shares = match tokio::time::timeout(
                tokio::time::Duration::from_millis(balance_timeout_ms),
                self.api.check_balance_only(token_id.as_str()),
            )
            .await
            {
                Ok(Ok(balance_raw)) => {
                    f64::try_from(balance_raw / Decimal::from(1_000_000u64)).unwrap_or(0.0)
                }
                Ok(Err(_)) | Err(_) => {
                    live_balance_source = "wallet_snapshot_haircut".to_string();
                    row.wallet_shares * balance_haircut
                }
            }
            .max(0.0);

            let requested_shares = live_balance_shares.min(row.wallet_shares).max(0.0);
            if requested_shares < min_submit_shares {
                skipped_candidates = skipped_candidates.saturating_add(1);
                bump_reason("skipped:insufficient_live_shares");
                result_rows.push(json!({
                    "status": "skipped",
                    "reason": "insufficient_live_shares",
                    "condition_id": condition_id,
                    "token_id": token_id,
                    "market_slug": row_market_slug,
                    "wallet_shares": row.wallet_shares,
                    "requested_shares": requested_shares,
                    "live_balance_source": live_balance_source
                }));
                continue;
            }

            let best_price = match self.api.get_best_price(token_id.as_str()).await {
                Ok(v) => v,
                Err(e) => {
                    skipped_candidates = skipped_candidates.saturating_add(1);
                    bump_reason("skipped:best_price_unavailable");
                    result_rows.push(json!({
                        "status": "skipped",
                        "reason": "best_price_unavailable",
                        "error": e.to_string(),
                        "condition_id": condition_id,
                        "token_id": token_id,
                        "market_slug": row_market_slug,
                        "wallet_shares": row.wallet_shares,
                        "requested_shares": requested_shares
                    }));
                    continue;
                }
            };

            let reference_price = best_price
                .as_ref()
                .and_then(|book| {
                    book.bid
                        .as_ref()
                        .or(book.ask.as_ref())
                        .map(|v| v.to_string().parse::<f64>().ok().unwrap_or(0.0))
                })
                .filter(|v| v.is_finite() && *v > 0.0)
                .or_else(|| row.wallet_avg_price.filter(|v| v.is_finite() && *v > 0.0))
                .or_else(|| {
                    if row_estimated_notional > 0.0 && row.wallet_shares > 0.0 {
                        Some(row_estimated_notional / row.wallet_shares)
                    } else {
                        None
                    }
                });
            let Some(reference_price) = reference_price else {
                skipped_candidates = skipped_candidates.saturating_add(1);
                bump_reason("skipped:reference_price_unavailable");
                result_rows.push(json!({
                    "status": "skipped",
                    "reason": "reference_price_unavailable",
                    "condition_id": condition_id,
                    "token_id": token_id,
                    "market_slug": row_market_slug,
                    "wallet_shares": row.wallet_shares,
                    "requested_shares": requested_shares
                }));
                continue;
            };

            // Dust cleanup should not be blocked by rewards/exchange min-size hints.
            // Submit what we actually have (floored to order precision) and let the exchange
            // accept/reject the order directly.
            let submit_shares = Self::floor_units_for_order(requested_shares);
            if submit_shares + 1e-9 < min_submit_shares {
                skipped_candidates = skipped_candidates.saturating_add(1);
                bump_reason("skipped:rounded_submit_below_min_submit_shares");
                result_rows.push(json!({
                    "status": "skipped",
                    "reason": "rounded_submit_below_min_submit_shares",
                    "condition_id": condition_id,
                    "token_id": token_id,
                    "market_slug": row_market_slug,
                    "wallet_shares": row.wallet_shares,
                    "requested_shares": requested_shares,
                    "submit_shares": submit_shares,
                    "min_submit_shares": min_submit_shares
                }));
                continue;
            }

            let submit_notional = submit_shares * reference_price;
            let adjusted_to_minimum = false;
            if submit_notional > max_notional_usd + 1e-9 {
                skipped_candidates = skipped_candidates.saturating_add(1);
                bump_reason("skipped:submit_notional_exceeds_dust_cap");
                result_rows.push(json!({
                    "status": "skipped",
                    "reason": "submit_notional_exceeds_dust_cap",
                    "condition_id": condition_id,
                    "token_id": token_id,
                    "market_slug": row_market_slug,
                    "wallet_shares": row.wallet_shares,
                    "requested_shares": requested_shares,
                    "submit_shares": submit_shares,
                    "submit_notional_usd": submit_notional,
                    "max_notional_usd": max_notional_usd
                }));
                continue;
            }

            if attempted_orders > 0 && inter_order_delay_ms > 0 {
                tokio::time::sleep(tokio::time::Duration::from_millis(inter_order_delay_ms)).await;
            }

            attempted_orders = attempted_orders.saturating_add(1);
            let allowance_refresh_error = self
                .api
                .update_balance_allowance_for_sell(token_id.as_str())
                .await
                .err()
                .map(|e| e.to_string());

            match self
                .api
                .place_market_order(token_id.as_str(), submit_shares, "SELL", Some("FAK"))
                .await
            {
                Ok(resp) => {
                    submitted_orders = submitted_orders.saturating_add(1);
                    bump_reason("submitted");
                    let submit_event = json!({
                        "condition_id": condition_id,
                        "token_id": token_id,
                        "market_slug": row.market_slug,
                        "wallet_shares": row.wallet_shares,
                        "wallet_value_usd": row.wallet_value_usd,
                        "requested_shares": requested_shares,
                        "submit_shares": submit_shares,
                        "submit_notional_usd": submit_notional,
                        "reference_price": reference_price,
                        "adjusted_to_minimum": adjusted_to_minimum,
                        "constraints_enforced": false,
                        "live_balance_source": live_balance_source,
                        "allowance_refresh_error": allowance_refresh_error,
                        "order_id": resp.order_id,
                        "order_status": resp.status,
                        "message": resp.message
                    });
                    log_event("mm_dust_sell_submitted", submit_event.clone());
                    result_rows.push(json!({
                        "status": "submitted",
                        "condition_id": condition_id,
                        "token_id": token_id,
                        "market_slug": row_market_slug,
                        "wallet_shares": row.wallet_shares,
                        "wallet_value_usd": row.wallet_value_usd,
                        "requested_shares": requested_shares,
                        "submit_shares": submit_shares,
                        "submit_notional_usd": submit_notional,
                        "reference_price": reference_price,
                        "adjusted_to_minimum": adjusted_to_minimum,
                        "constraints_enforced": false,
                        "live_balance_source": live_balance_source,
                        "allowance_refresh_error": allowance_refresh_error,
                        "order_id": submit_event.get("order_id").cloned().unwrap_or(Value::Null),
                        "order_status": submit_event.get("order_status").cloned().unwrap_or(Value::Null),
                        "message": submit_event.get("message").cloned().unwrap_or(Value::Null)
                    }));
                }
                Err(e) => {
                    failed_orders = failed_orders.saturating_add(1);
                    bump_reason("failed:submit_error");
                    let error_text = e.to_string();
                    let fail_event = json!({
                        "condition_id": condition_id,
                        "token_id": token_id,
                        "market_slug": row.market_slug,
                        "wallet_shares": row.wallet_shares,
                        "wallet_value_usd": row.wallet_value_usd,
                        "requested_shares": requested_shares,
                        "submit_shares": submit_shares,
                        "submit_notional_usd": submit_notional,
                        "reference_price": reference_price,
                        "adjusted_to_minimum": adjusted_to_minimum,
                        "live_balance_source": live_balance_source,
                        "allowance_refresh_error": allowance_refresh_error,
                        "error": error_text
                    });
                    log_event("mm_dust_sell_submit_failed", fail_event.clone());
                    result_rows.push(json!({
                        "status": "failed",
                        "condition_id": condition_id,
                        "token_id": token_id,
                        "market_slug": row_market_slug,
                        "wallet_shares": row.wallet_shares,
                        "wallet_value_usd": row.wallet_value_usd,
                        "requested_shares": requested_shares,
                        "submit_shares": submit_shares,
                        "submit_notional_usd": submit_notional,
                        "reference_price": reference_price,
                        "adjusted_to_minimum": adjusted_to_minimum,
                        "live_balance_source": live_balance_source,
                        "allowance_refresh_error": allowance_refresh_error,
                        "error": fail_event.get("error").cloned().unwrap_or(Value::Null)
                    }));
                }
            }
        }

        let rows_total = result_rows.len();
        let rows_sample_limit = 50usize;
        let rows_sample = result_rows
            .iter()
            .take(rows_sample_limit)
            .cloned()
            .collect::<Vec<_>>();
        log_event(
            "mm_dust_sell_summary",
            json!({
                "strategy_id": crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
                "max_notional_usd": max_notional_usd,
                "max_shares": max_shares,
                "scan_limit": scan_limit,
                "max_orders": max_orders,
                "min_submit_shares": min_submit_shares,
                "balance_haircut": balance_haircut,
                "balance_timeout_ms": balance_timeout_ms,
                "inter_order_delay_ms": inter_order_delay_ms,
                "candidates": candidate_count,
                "candidates_capped": capped_candidates,
                "attempted_orders": attempted_orders,
                "submitted": submitted_orders,
                "failed": failed_orders,
                "skipped": skipped_candidates,
                "reason_counts": reason_counts,
                "rows_total": rows_total,
                "rows_sample_truncated": rows_total > rows_sample_limit,
                "rows_sample": rows_sample
            }),
        );

        Ok(json!({
            "ok": true,
            "enabled": true,
            "strategy_id": crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
            "max_notional_usd": max_notional_usd,
            "max_shares": max_shares,
            "scan_limit": scan_limit,
            "max_orders": max_orders,
            "min_submit_shares": min_submit_shares,
            "balance_haircut": balance_haircut,
            "balance_timeout_ms": balance_timeout_ms,
            "inter_order_delay_ms": inter_order_delay_ms,
            "candidates": candidate_count,
            "candidates_capped": capped_candidates,
            "attempted_orders": attempted_orders,
            "submitted": submitted_orders,
            "failed": failed_orders,
            "skipped": skipped_candidates,
            "reason_counts": reason_counts,
            "rows": result_rows
        }))
    }

    pub fn mm_rewards_status(&self) -> Value {
        let Some(db) = self.tracking_db.as_ref() else {
            return json!({
                "ok": false,
                "error": "tracking_db_unavailable"
            });
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        let cashflow_24h = db
            .summarize_wallet_cashflow_by_event(
                crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
                Some(now_ms.saturating_sub(24 * 3600 * 1000)),
            )
            .ok()
            .unwrap_or_default();
        let cashflow_all = db
            .summarize_wallet_cashflow_by_event(crate::strategy::STRATEGY_ID_MM_REWARDS_V1, None)
            .ok()
            .unwrap_or_default();
        let unattributed_cashflow_24h = db
            .summarize_wallet_cashflow_unattributed_by_event(
                crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
                Some(now_ms.saturating_sub(24 * 3600 * 1000)),
            )
            .ok()
            .unwrap_or_default();
        let unattributed_cashflow_all = db
            .summarize_wallet_cashflow_unattributed_by_event(
                crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
                None,
            )
            .ok()
            .unwrap_or_default();
        let attribution_24h = db
            .summarize_mm_market_attribution(
                crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
                Some(now_ms.saturating_sub(24 * 3600 * 1000)),
            )
            .ok()
            .unwrap_or_default();
        let attribution_all = db
            .summarize_mm_market_attribution(crate::strategy::STRATEGY_ID_MM_REWARDS_V1, None)
            .ok()
            .unwrap_or_default();
        let inventory_snapshot = db
            .get_mm_inventory_drift_snapshot(crate::strategy::STRATEGY_ID_MM_REWARDS_V1, 256)
            .ok();
        let cashflow_rows_to_json = |rows: Vec<crate::tracking_db::WalletCashflowSummaryRow>| {
            rows.into_iter()
                .map(|row| {
                    json!({
                        "event_type": row.event_type,
                        "events": row.events,
                        "total_amount_usd": row.total_amount_usd
                    })
                })
                .collect::<Vec<_>>()
        };
        let attribution_rows_to_json =
            |rows: &[crate::tracking_db::MmMarketAttributionRow]| -> Vec<Value> {
                rows.iter()
                    .map(|row| {
                        json!({
                            "condition_id": row.condition_id,
                            "market_slug": row.market_slug,
                            "timeframe": row.timeframe,
                            "symbol": row.symbol,
                            "mode": row.mode,
                            "settled_trades": row.settled_trades,
                            "trade_realized_pnl_usd": row.trade_realized_pnl_usd,
                            "merge_events": row.merge_events,
                            "merge_cashflow_usd": row.merge_cashflow_usd,
                            "reward_events": row.reward_events,
                            "reward_cashflow_usd": row.reward_cashflow_usd,
                            "redemption_events": row.redemption_events,
                            "redemption_cashflow_usd": row.redemption_cashflow_usd,
                            "open_positions": row.open_positions,
                            "open_entry_units": row.open_entry_units,
                            "open_entry_notional_usd": row.open_entry_notional_usd,
                            "open_realized_pnl_usd": row.open_realized_pnl_usd,
                            "db_inventory_shares": row.db_inventory_shares,
                            "wallet_inventory_shares": row.wallet_inventory_shares,
                            "inventory_drift_shares": row.inventory_drift_shares,
                            "wallet_inventory_value_live_usd": row.wallet_inventory_value_live_usd,
                            "wallet_inventory_value_bid_usd": row.wallet_inventory_value_bid_usd,
                            "wallet_inventory_value_mid_usd": row.wallet_inventory_value_mid_usd,
                            "last_wallet_activity_ts_ms": row.last_wallet_activity_ts_ms,
                            "last_wallet_activity_type": row.last_wallet_activity_type,
                            "net_pnl_usd": row.net_pnl_usd
                        })
                    })
                    .collect::<Vec<_>>()
            };
        let inventory_drift_rows_to_json =
            |rows: &[crate::tracking_db::MmInventoryDriftRow]| -> Vec<Value> {
                rows.iter()
                    .map(|row| {
                        json!({
                            "condition_id": row.condition_id,
                            "token_id": row.token_id,
                            "market_slug": row.market_slug,
                            "timeframe": row.timeframe,
                            "symbol": row.symbol,
                            "mode": row.mode,
                            "period_timestamp": row.period_timestamp,
                            "db_open_positions": row.db_open_positions,
                            "db_inventory_shares": row.db_inventory_shares,
                            "db_remaining_cost_usd": row.db_remaining_cost_usd,
                            "wallet_inventory_shares": row.wallet_inventory_shares,
                            "wallet_avg_price": row.wallet_avg_price,
                            "wallet_cur_price": row.wallet_cur_price,
                            "wallet_current_value_usd": row.wallet_current_value_usd,
                            "wallet_best_bid": row.wallet_best_bid,
                            "wallet_mid_price": row.wallet_mid_price,
                            "wallet_inventory_value_bid_usd": row.wallet_inventory_value_bid_usd,
                            "wallet_inventory_value_mid_usd": row.wallet_inventory_value_mid_usd,
                            "wallet_unrealized_mid_pnl_usd": row.wallet_unrealized_mid_pnl_usd,
                            "wallet_snapshot_ts_ms": row.wallet_snapshot_ts_ms,
                            "mid_snapshot_ts_ms": row.mid_snapshot_ts_ms,
                            "last_wallet_activity_ts_ms": row.last_wallet_activity_ts_ms,
                            "last_wallet_activity_type": row.last_wallet_activity_type,
                            "last_inventory_consumed_units": row.last_inventory_consumed_units,
                            "inventory_drift_shares": row.inventory_drift_shares
                        })
                    })
                    .collect::<Vec<_>>()
            };
        let attribution_totals =
            |rows: &[crate::tracking_db::MmMarketAttributionRow]|
             -> crate::tracking_db::MmMarketAttributionTotals {
                let mut totals = crate::tracking_db::MmMarketAttributionTotals::default();
                totals.markets = rows.len() as u64;
                for row in rows {
                    totals.settled_trades = totals.settled_trades.saturating_add(row.settled_trades);
                    totals.trade_realized_pnl_usd += row.trade_realized_pnl_usd;
                    totals.merge_events = totals.merge_events.saturating_add(row.merge_events);
                    totals.merge_cashflow_usd += row.merge_cashflow_usd;
                    totals.reward_events = totals.reward_events.saturating_add(row.reward_events);
                    totals.reward_cashflow_usd += row.reward_cashflow_usd;
                    totals.redemption_events =
                        totals.redemption_events.saturating_add(row.redemption_events);
                    totals.redemption_cashflow_usd += row.redemption_cashflow_usd;
                    totals.open_positions = totals.open_positions.saturating_add(row.open_positions);
                    totals.open_entry_notional_usd += row.open_entry_notional_usd;
                    totals.db_inventory_shares += row.db_inventory_shares;
                    if let Some(v) = row.wallet_inventory_shares {
                        totals.wallet_inventory_shares =
                            Some(totals.wallet_inventory_shares.unwrap_or(0.0) + v);
                    }
                    if let Some(v) = row.inventory_drift_shares {
                        totals.inventory_drift_shares =
                            Some(totals.inventory_drift_shares.unwrap_or(0.0) + v);
                    }
                    if let Some(v) = row.wallet_inventory_value_live_usd {
                        totals.wallet_inventory_value_live_usd =
                            Some(totals.wallet_inventory_value_live_usd.unwrap_or(0.0) + v);
                    }
                    if let Some(v) = row.wallet_inventory_value_bid_usd {
                        totals.wallet_inventory_value_bid_usd =
                            Some(totals.wallet_inventory_value_bid_usd.unwrap_or(0.0) + v);
                    }
                    if let Some(v) = row.wallet_inventory_value_mid_usd {
                        totals.wallet_inventory_value_mid_usd =
                            Some(totals.wallet_inventory_value_mid_usd.unwrap_or(0.0) + v);
                    }
                    totals.net_pnl_usd += row.net_pnl_usd;
                }
                totals
            };
        let attribution_totals_to_json =
            |totals: &crate::tracking_db::MmMarketAttributionTotals| {
                json!({
                    "markets": totals.markets,
                    "settled_trades": totals.settled_trades,
                    "trade_realized_pnl_usd": totals.trade_realized_pnl_usd,
                    "merge_events": totals.merge_events,
                    "merge_cashflow_usd": totals.merge_cashflow_usd,
                    "reward_events": totals.reward_events,
                    "reward_cashflow_usd": totals.reward_cashflow_usd,
                    "redemption_events": totals.redemption_events,
                    "redemption_cashflow_usd": totals.redemption_cashflow_usd,
                    "open_positions": totals.open_positions,
                    "open_entry_notional_usd": totals.open_entry_notional_usd,
                    "db_inventory_shares": totals.db_inventory_shares,
                    "wallet_inventory_shares": totals.wallet_inventory_shares,
                    "inventory_drift_shares": totals.inventory_drift_shares,
                    "wallet_inventory_value_live_usd": totals.wallet_inventory_value_live_usd,
                    "wallet_inventory_value_bid_usd": totals.wallet_inventory_value_bid_usd,
                    "wallet_inventory_value_mid_usd": totals.wallet_inventory_value_mid_usd,
                    "net_pnl_usd": totals.net_pnl_usd
                })
            };
        let rollout_flags = json!({
            "markout_enable": std::env::var("EVPOLY_MM_MARKOUT_ENABLE").ok().unwrap_or_else(|| "true".to_string()),
            "markout_dynamic_enable": std::env::var("EVPOLY_MM_MARKOUT_DYNAMIC_ENABLE").ok().unwrap_or_else(|| "false".to_string()),
            "event_driven_enable": std::env::var("EVPOLY_MM_EVENT_DRIVEN_ENABLE").ok().unwrap_or_else(|| "true".to_string()),
            "market_mode": std::env::var("EVPOLY_MM_MARKET_MODE").ok().unwrap_or_else(|| "target".to_string()),
            "auto_top_n": std::env::var("EVPOLY_MM_AUTO_TOP_N").ok().unwrap_or_else(|| "5".to_string())
        });
        let filled_side_24h = db
            .summarize_pending_filled_side_for_strategy(
                crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
                Some(now_ms.saturating_sub(24 * 3_600_000)),
                Some(now_ms),
            )
            .ok()
            .unwrap_or_default();
        let sell_to_buy_fill_usd_pct_24h = if filled_side_24h.buy_notional_usd > 0.0 {
            (filled_side_24h.sell_notional_usd / filled_side_24h.buy_notional_usd * 100.0)
                .clamp(0.0, 10_000.0)
        } else {
            0.0
        };
        let weak_exit_events = summarize_mm_weak_exit_event_counts(now_ms);
        let weak_exit_stale_skip_rate_24h = if weak_exit_events.probe_24h > 0 {
            (weak_exit_events.stale_skip_24h as f64 / weak_exit_events.probe_24h as f64)
                .clamp(0.0, 1.0)
        } else {
            0.0
        };
        let weak_exit_stale_skip_rate_3h = if weak_exit_events.probe_3h > 0 {
            (weak_exit_events.stale_skip_3h as f64 / weak_exit_events.probe_3h as f64)
                .clamp(0.0, 1.0)
        } else {
            0.0
        };
        let totals_24h = attribution_totals(attribution_24h.as_slice());
        let totals_all = attribution_totals(attribution_all.as_slice());
        let attribution_all_by_condition = attribution_all
            .iter()
            .map(|row| (row.condition_id.clone(), row))
            .collect::<HashMap<_, _>>();
        let wallet_backed_sources = inventory_snapshot
            .as_ref()
            .map(|snapshot| {
                json!({
                    "wallet_address": snapshot.wallet_address,
                    "wallet_positions_available": snapshot.wallet_positions_available,
                    "wallet_activity_available": snapshot.wallet_activity_available,
                    "wallet_mid_marks_available": snapshot.wallet_mid_marks_available
                })
            })
            .unwrap_or_else(|| {
                json!({
                    "wallet_address": null,
                    "wallet_positions_available": false,
                    "wallet_activity_available": false,
                    "wallet_mid_marks_available": false
                })
            });
        // IMPORTANT: `/admin/mm/status` `markets` intentionally exposes an active status_scope subset
        // from `mm_market_states_v1` (markets touched by the live MM loop recently). It is not a global
        // source of truth for all open orders/inventory across the DB.
        let status_scope_notes = json!({
            "markets_scope": "status_scope_active_mm_market_states",
            "markets_table": "mm_market_states_v1",
            "markets_scope_warning": "The `markets` array is a status_scope subset and can exclude markets that still have OPEN pending_orders or wallet inventory.",
            "global_open_orders_reference": "Use pending_orders WHERE strategy_id='mm_rewards_v1' AND status='OPEN' for global open-order counts.",
            "global_inventory_reference": "Use wallet_positions_live_latest_v1 for global wallet inventory counts/values.",
            "global_status_endpoint": "/admin/mm/status/global",
            "global_status_note": "Use /admin/mm/status/global for token-aware classification of active_quote / inventory_exit / idle."
        });
        match db.list_mm_market_states(crate::strategy::STRATEGY_ID_MM_REWARDS_V1) {
            Ok(rows) => {
                let mut markets_with_weak_inventory = 0_u64;
                let mut markets_with_open_sells_when_weak_inventory = 0_u64;
                let markets = rows
                    .into_iter()
                    .map(|row| {
                        let up_live = row
                            .up_token_id
                            .as_deref()
                            .and_then(|token_id| {
                                db.get_mm_live_token_state(
                                    crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
                                    row.condition_id.as_str(),
                                    row.timeframe.as_str(),
                                    row.period_timestamp,
                                    token_id,
                                )
                                .ok()
                            })
                            .unwrap_or_default();
                        let down_live = row
                            .down_token_id
                            .as_deref()
                            .and_then(|token_id| {
                                db.get_mm_live_token_state(
                                    crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
                                    row.condition_id.as_str(),
                                    row.timeframe.as_str(),
                                    row.period_timestamp,
                                    token_id,
                                )
                                .ok()
                            })
                            .unwrap_or_default();
                        let live_open_buy_orders =
                            up_live.active_buy_orders.saturating_add(down_live.active_buy_orders);
                        let live_open_sell_orders =
                            up_live.active_sell_orders.saturating_add(down_live.active_sell_orders);
                        let weak_inventory_shares = row.weak_inventory_shares.unwrap_or(0.0);
                        let inventory_for_exit_shares = weak_inventory_shares
                            .max(row.up_inventory_shares.unwrap_or(0.0))
                            .max(row.down_inventory_shares.unwrap_or(0.0));
                        if inventory_for_exit_shares > 0.0 {
                            markets_with_weak_inventory =
                                markets_with_weak_inventory.saturating_add(1);
                            if live_open_sell_orders > 0 {
                                markets_with_open_sells_when_weak_inventory =
                                    markets_with_open_sells_when_weak_inventory.saturating_add(1);
                            }
                        }
                        let attribution = attribution_all_by_condition.get(row.condition_id.as_str());
                        json!({
                            "strategy_id": row.strategy_id,
                            "condition_id": row.condition_id,
                            "market_slug": row.market_slug,
                            "timeframe": row.timeframe,
                            "symbol": row.symbol,
                            "mode": row.mode,
                            "period_timestamp": row.period_timestamp,
                            "state": row.state,
                            "reason": row.reason,
                            "reward_rate_hint": row.reward_rate_hint,
                            "open_buy_orders": live_open_buy_orders,
                            "open_sell_orders": live_open_sell_orders,
                            "snapshot_open_buy_orders": row.open_buy_orders,
                            "snapshot_open_sell_orders": row.open_sell_orders,
                            "favor_inventory_shares": row.favor_inventory_shares,
                            "weak_inventory_shares": row.weak_inventory_shares,
                            "db_inventory_shares": attribution.map(|v| v.db_inventory_shares),
                            "wallet_inventory_shares": attribution.and_then(|v| v.wallet_inventory_shares),
                            "inventory_drift_shares": attribution.and_then(|v| v.inventory_drift_shares),
                            "wallet_inventory_value_live_usd": attribution.and_then(|v| v.wallet_inventory_value_live_usd),
                            "wallet_inventory_value_bid_usd": attribution.and_then(|v| v.wallet_inventory_value_bid_usd),
                            "wallet_inventory_value_mid_usd": attribution.and_then(|v| v.wallet_inventory_value_mid_usd),
                            "last_wallet_activity_ts_ms": attribution.and_then(|v| v.last_wallet_activity_ts_ms),
                            "last_wallet_activity_type": attribution.and_then(|v| v.last_wallet_activity_type.clone()),
                            "up_token_id": row.up_token_id,
                            "down_token_id": row.down_token_id,
                            "up_wallet_inventory_shares": up_live.wallet_inventory_shares,
                            "down_wallet_inventory_shares": down_live.wallet_inventory_shares,
                            "up_db_inventory_shares": up_live.db_inventory_shares,
                            "down_db_inventory_shares": down_live.db_inventory_shares,
                            "snapshot_up_inventory_shares": row.up_inventory_shares,
                            "snapshot_down_inventory_shares": row.down_inventory_shares,
                            "up_open_buy_orders": up_live.active_buy_orders,
                            "down_open_buy_orders": down_live.active_buy_orders,
                            "up_open_sell_orders": up_live.active_sell_orders,
                            "down_open_sell_orders": down_live.active_sell_orders,
                            "ladder_top_price": row.ladder_top_price,
                            "ladder_bottom_price": row.ladder_bottom_price,
                            "last_event_ms": row.last_event_ms,
                            "updated_at_ms": row.updated_at_ms
                        })
                    })
                    .collect::<Vec<_>>();
                let open_sell_coverage_when_weak_inventory_pct = if markets_with_weak_inventory > 0
                {
                    (markets_with_open_sells_when_weak_inventory as f64
                        / markets_with_weak_inventory as f64
                        * 100.0)
                        .clamp(0.0, 100.0)
                } else {
                    0.0
                };
                json!({
                    "ok": true,
                    "scope_notes": status_scope_notes,
                    "count": markets.len(),
                    "markets": markets,
                    "cashflow_24h": cashflow_rows_to_json(cashflow_24h),
                    "cashflow_all": cashflow_rows_to_json(cashflow_all),
                    "cashflow_unattributed_24h": cashflow_rows_to_json(unattributed_cashflow_24h),
                    "cashflow_unattributed_all": cashflow_rows_to_json(unattributed_cashflow_all),
                    "attribution_24h": attribution_rows_to_json(attribution_24h.as_slice()),
                    "attribution_all": attribution_rows_to_json(attribution_all.as_slice()),
                    "attribution_totals_24h": attribution_totals_to_json(&totals_24h),
                    "attribution_totals_all": attribution_totals_to_json(&totals_all),
                    "inventory_drift_tokens": inventory_snapshot
                        .as_ref()
                        .map(|snapshot| inventory_drift_rows_to_json(snapshot.rows.as_slice()))
                        .unwrap_or_default(),
                    "wallet_backed_sources": wallet_backed_sources,
                    "rollout_flags": rollout_flags,
                    "kpis": {
                        "buy_fill_orders_24h": filled_side_24h.buy_fills,
                        "sell_fill_orders_24h": filled_side_24h.sell_fills,
                        "buy_fill_notional_usd_24h": filled_side_24h.buy_notional_usd,
                        "sell_fill_notional_usd_24h": filled_side_24h.sell_notional_usd,
                        "sell_to_buy_fill_usd_pct_24h": sell_to_buy_fill_usd_pct_24h,
                        "markets_with_weak_inventory": markets_with_weak_inventory,
                        "markets_with_open_sells_when_weak_inventory_gt0": markets_with_open_sells_when_weak_inventory,
                        "open_sell_coverage_when_weak_inventory_pct": open_sell_coverage_when_weak_inventory_pct,
                        "weak_exit_probe_events_24h": weak_exit_events.probe_24h,
                        "weak_exit_stale_live_balance_skips_24h": weak_exit_events.stale_skip_24h,
                        "weak_exit_stale_skip_rate_24h": weak_exit_stale_skip_rate_24h,
                        "weak_exit_probe_events_3h": weak_exit_events.probe_3h,
                        "weak_exit_stale_live_balance_skips_3h": weak_exit_events.stale_skip_3h,
                        "weak_exit_stale_skip_rate_3h": weak_exit_stale_skip_rate_3h
                    }
                })
            }
            Err(e) => json!({
                "ok": false,
                "error": e.to_string(),
                "scope_notes": status_scope_notes,
                "cashflow_24h": cashflow_rows_to_json(cashflow_24h),
                "cashflow_all": cashflow_rows_to_json(cashflow_all),
                "cashflow_unattributed_24h": cashflow_rows_to_json(unattributed_cashflow_24h),
                "cashflow_unattributed_all": cashflow_rows_to_json(unattributed_cashflow_all),
                "attribution_24h": attribution_rows_to_json(attribution_24h.as_slice()),
                "attribution_all": attribution_rows_to_json(attribution_all.as_slice()),
                "attribution_totals_24h": attribution_totals_to_json(&totals_24h),
                "attribution_totals_all": attribution_totals_to_json(&totals_all),
                "inventory_drift_tokens": inventory_snapshot
                    .as_ref()
                    .map(|snapshot| inventory_drift_rows_to_json(snapshot.rows.as_slice()))
                    .unwrap_or_default(),
                "wallet_backed_sources": wallet_backed_sources,
                "rollout_flags": rollout_flags,
                "kpis": {
                    "buy_fill_orders_24h": filled_side_24h.buy_fills,
                    "sell_fill_orders_24h": filled_side_24h.sell_fills,
                    "buy_fill_notional_usd_24h": filled_side_24h.buy_notional_usd,
                    "sell_fill_notional_usd_24h": filled_side_24h.sell_notional_usd,
                    "sell_to_buy_fill_usd_pct_24h": sell_to_buy_fill_usd_pct_24h,
                    "markets_with_weak_inventory": 0,
                    "markets_with_open_sells_when_weak_inventory_gt0": 0,
                    "open_sell_coverage_when_weak_inventory_pct": 0.0,
                    "weak_exit_probe_events_24h": weak_exit_events.probe_24h,
                    "weak_exit_stale_live_balance_skips_24h": weak_exit_events.stale_skip_24h,
                    "weak_exit_stale_skip_rate_24h": weak_exit_stale_skip_rate_24h,
                    "weak_exit_probe_events_3h": weak_exit_events.probe_3h,
                    "weak_exit_stale_live_balance_skips_3h": weak_exit_events.stale_skip_3h,
                    "weak_exit_stale_skip_rate_3h": weak_exit_stale_skip_rate_3h
                }
            }),
        }
    }

    pub fn mm_rewards_status_global(&self) -> Value {
        let Some(db) = self.tracking_db.as_ref() else {
            return json!({
                "ok": false,
                "error": "tracking_db_unavailable"
            });
        };
        let strategy_id = crate::strategy::STRATEGY_ID_MM_REWARDS_V1;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let stale_threshold_sec = std::env::var("EVPOLY_MM_GLOBAL_STATUS_STALE_SNAPSHOT_SEC")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(5_400)
            .clamp(60, 86_400);

        let mut markets: HashMap<String, MmGlobalStatusAccumulator> = HashMap::new();

        if let Ok(rows) = db.list_mm_market_states(strategy_id) {
            for row in rows {
                let key = row.condition_id.trim().to_ascii_lowercase();
                if key.is_empty() {
                    continue;
                }
                let entry = markets.entry(key).or_default();
                overwrite_if_missing(&mut entry.market_slug, row.market_slug);
                overwrite_if_missing(&mut entry.timeframe, Some(row.timeframe));
                overwrite_if_missing(&mut entry.symbol, row.symbol);
                overwrite_if_missing(&mut entry.mode, row.mode);
                overwrite_if_missing(&mut entry.up_token_id, row.up_token_id);
                overwrite_if_missing(&mut entry.down_token_id, row.down_token_id);
            }
        }

        let mut open_order_rows = 0_u64;
        let mut open_order_conditions: HashSet<String> = HashSet::new();
        if let Ok(rows) = db.list_active_pending_orders() {
            for row in rows {
                if !row.strategy_id.eq_ignore_ascii_case(strategy_id) {
                    continue;
                }
                if !row.status.eq_ignore_ascii_case("OPEN") {
                    continue;
                }
                let Some(condition_id) = row
                    .condition_id
                    .as_ref()
                    .map(|v| v.trim().to_ascii_lowercase())
                    .filter(|v| !v.is_empty())
                else {
                    continue;
                };
                let token_id = row.token_id.trim().to_string();
                let entry = markets.entry(condition_id.clone()).or_default();
                if !token_id.is_empty() {
                    entry.open_token_ids.insert(token_id);
                }
                if row.side.eq_ignore_ascii_case("BUY") {
                    entry.open_buy_orders = entry.open_buy_orders.saturating_add(1);
                } else if row.side.eq_ignore_ascii_case("SELL") {
                    entry.open_sell_orders = entry.open_sell_orders.saturating_add(1);
                }
                open_order_rows = open_order_rows.saturating_add(1);
                open_order_conditions.insert(condition_id);
            }
        }

        let mut inventory_conditions: HashSet<String> = HashSet::new();
        let mut inventory_shares_total = 0.0_f64;
        let mut inventory_value_usd_total = 0.0_f64;
        if let Ok(rows) = db.list_mm_wallet_inventory_by_strategy(strategy_id, 20_000) {
            for row in rows {
                if row.wallet_shares <= 1e-9 {
                    continue;
                }
                let condition_id = row.condition_id.trim().to_ascii_lowercase();
                if condition_id.is_empty() {
                    continue;
                }
                let entry = markets.entry(condition_id.clone()).or_default();
                overwrite_if_missing(&mut entry.market_slug, row.market_slug);
                overwrite_if_missing(&mut entry.timeframe, row.timeframe);
                overwrite_if_missing(&mut entry.symbol, row.symbol);
                overwrite_if_missing(&mut entry.mode, row.mode);
                entry.inventory_shares += row.wallet_shares.max(0.0);
                entry.inventory_value_usd += row.wallet_value_usd.max(0.0);
                inventory_shares_total += row.wallet_shares.max(0.0);
                inventory_value_usd_total += row.wallet_value_usd.max(0.0);
                inventory_conditions.insert(condition_id);
            }
        }

        // Global status should reflect live scope only:
        // keep markets that currently have OPEN orders and/or wallet inventory.
        // Drop state-only rows with no live exposure.
        markets.retain(|_, market| {
            market.inventory_shares > 1e-9
                || market
                    .open_buy_orders
                    .saturating_add(market.open_sell_orders)
                    > 0
        });

        let wallet_snapshot_ts_ms = db.latest_wallet_positions_snapshot_ts_ms().ok().flatten();
        let wallet_snapshot_age_sec =
            wallet_snapshot_ts_ms.map(|ts| now_ms.saturating_sub(ts).saturating_div(1_000).max(0));
        let wallet_snapshot_stale = wallet_snapshot_age_sec
            .map(|age| age > stale_threshold_sec)
            .unwrap_or(true);

        let wallet_sync_health = db.summarize_wallet_sync_health(24).ok().flatten();
        let consecutive_non_ok_runs = wallet_sync_health
            .as_ref()
            .map(|v| v.consecutive_non_ok_runs)
            .unwrap_or(0);
        let recent_db_locked_errors = wallet_sync_health
            .as_ref()
            .map(|v| v.recent_db_locked_errors)
            .unwrap_or(0);
        let recent_api_unavailable_errors = wallet_sync_health
            .as_ref()
            .map(|v| v.recent_api_unavailable_errors)
            .unwrap_or(0);
        let sync_alert = consecutive_non_ok_runs >= 2;

        let mut sync_alert_reasons = Vec::new();
        if consecutive_non_ok_runs >= 2 {
            sync_alert_reasons.push("consecutive_non_ok_runs>=2".to_string());
        }
        if recent_db_locked_errors >= 2 {
            sync_alert_reasons.push("recent_db_locked_errors>=2".to_string());
        }
        if recent_api_unavailable_errors >= 2 {
            sync_alert_reasons.push("recent_api_unavailable_errors>=2".to_string());
        }
        if wallet_snapshot_stale {
            sync_alert_reasons.push("wallet_snapshot_stale".to_string());
        }

        let mut active_quote = 0_u64;
        let mut inventory_exit = 0_u64;
        let mut idle = 0_u64;
        let mut idle_reasons: HashMap<String, u64> = HashMap::new();

        // Category precedence is explicit and mutually exclusive:
        // 1) inventory_exit, 2) active_quote, 3) idle.
        let mut rows = Vec::new();
        for (condition_id, market) in markets {
            let has_inventory = market.inventory_shares > 1e-9;
            let has_open_orders = market
                .open_buy_orders
                .saturating_add(market.open_sell_orders)
                > 0;
            let has_open_sell = market.open_sell_orders > 0;
            let open_token_count = market.open_token_ids.len();

            let has_two_outcome_open_quote = if let (Some(up), Some(down)) =
                (market.up_token_id.clone(), market.down_token_id.clone())
            {
                let up_trimmed = up.trim();
                let down_trimmed = down.trim();
                !up_trimmed.is_empty()
                    && !down_trimmed.is_empty()
                    && market.open_token_ids.contains(up_trimmed)
                    && market.open_token_ids.contains(down_trimmed)
            } else {
                open_token_count >= 2
            };

            let (bucket, bucket_reason) = if has_inventory && has_open_sell {
                inventory_exit = inventory_exit.saturating_add(1);
                (
                    "inventory_exit",
                    "wallet_inventory_shares>0 && open_sell_orders>0",
                )
            } else if has_two_outcome_open_quote {
                active_quote = active_quote.saturating_add(1);
                ("active_quote", "open_orders_on_both_outcome_tokens")
            } else {
                idle = idle.saturating_add(1);
                let reason = if has_inventory && !has_open_sell {
                    "inventory_without_open_sell"
                } else if has_open_orders && open_token_count <= 1 {
                    "single_token_orders_only"
                } else if !has_open_orders && has_inventory {
                    "inventory_without_open_orders"
                } else if !has_open_orders {
                    "no_open_orders"
                } else {
                    "unclassified_idle"
                };
                let reason_key = reason.to_string();
                let current = idle_reasons.get(reason_key.as_str()).copied().unwrap_or(0);
                idle_reasons.insert(reason_key, current.saturating_add(1));
                ("idle", reason)
            };

            let mut open_token_ids = market.open_token_ids.into_iter().collect::<Vec<_>>();
            open_token_ids.sort();
            rows.push((
                match bucket {
                    "inventory_exit" => 0_u8,
                    "active_quote" => 1_u8,
                    _ => 2_u8,
                },
                market.inventory_shares,
                condition_id.clone(),
                json!({
                    "condition_id": condition_id,
                    "market_slug": market.market_slug,
                    "timeframe": market.timeframe,
                    "symbol": market.symbol,
                    "mode": market.mode,
                    "up_token_id": market.up_token_id,
                    "down_token_id": market.down_token_id,
                    "open_buy_orders": market.open_buy_orders,
                    "open_sell_orders": market.open_sell_orders,
                    "open_token_count": open_token_count,
                    "open_token_ids": open_token_ids,
                    "inventory_shares": market.inventory_shares,
                    "inventory_value_usd": market.inventory_value_usd,
                    "bucket": bucket,
                    "bucket_reason": bucket_reason,
                    "two_outcome_open_quote": has_two_outcome_open_quote
                }),
            ));
        }

        rows.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
                .then_with(|| a.2.cmp(&b.2))
        });

        json!({
            "ok": true,
            "scope": "global_orders_inventory_union",
            "scope_notes": {
                "classification_source": "pending_orders(status='OPEN', strategy_id='mm_rewards_v1') UNION wallet_positions_live_latest_v1(position_size>0)",
                "classification_precedence": [
                    "inventory_exit: wallet_inventory_shares>0 && open_sell_orders>0",
                    "active_quote: open orders on both outcome tokens (token-aware, not BUY/SELL-side only)",
                    "idle: everything else"
                ],
                "status_scope_warning": "Use this endpoint for global classification. /admin/mm/status remains a live status_scope subset from mm_market_states_v1."
            },
            "totals": {
                "markets": rows.len(),
                "open_order_rows": open_order_rows,
                "open_order_conditions": open_order_conditions.len(),
                "inventory_conditions": inventory_conditions.len(),
                "inventory_shares_total": inventory_shares_total,
                "inventory_value_usd_total": inventory_value_usd_total
            },
            "category_counts": {
                "active_quote": active_quote,
                "inventory_exit": inventory_exit,
                "idle": idle
            },
            "idle_reasons": idle_reasons,
            "wallet_sync": {
                "latest_wallet_snapshot_ts_ms": wallet_snapshot_ts_ms,
                "wallet_snapshot_age_sec": wallet_snapshot_age_sec,
                "wallet_snapshot_stale_threshold_sec": stale_threshold_sec,
                "wallet_snapshot_stale": wallet_snapshot_stale,
                "latest_sync_run_ts_ms": wallet_sync_health.as_ref().and_then(|v| v.latest_ts_ms),
                "latest_sync_run_status": wallet_sync_health.as_ref().and_then(|v| v.latest_status.clone()),
                "latest_sync_run_error": wallet_sync_health.as_ref().and_then(|v| v.latest_error.clone()),
                "recent_runs_checked": wallet_sync_health.as_ref().map(|v| v.recent_runs_checked).unwrap_or(0),
                "recent_non_ok_runs": wallet_sync_health.as_ref().map(|v| v.recent_non_ok_runs).unwrap_or(0),
                "consecutive_non_ok_runs": consecutive_non_ok_runs,
                "recent_db_locked_errors": recent_db_locked_errors,
                "recent_api_unavailable_errors": recent_api_unavailable_errors,
                "alert": sync_alert,
                "alert_reasons": sync_alert_reasons
            },
            "as_of_ts_ms": now_ms,
            "markets": rows.into_iter().map(|v| v.3).collect::<Vec<_>>()
        })
    }

    pub async fn check_market_closure_startup_catchup(&self) -> Result<()> {
        self.check_market_closure_with_trigger(RedemptionSweepTrigger::StartupCatchup)
            .await
    }

    /// Check and settle trades when markets close
    /// For momentum strategy: If token wasn't sold, it will be worth $1 if Up won, $0 if Down won
    pub async fn check_market_closure(&self) -> Result<()> {
        self.check_market_closure_with_trigger(RedemptionSweepTrigger::Scheduler)
            .await
    }

    /// Trigger a redemption sweep immediately from admin/manual API.
    pub async fn run_manual_redemption_sweep_now(&self) -> Result<()> {
        self.check_market_closure_with_trigger(RedemptionSweepTrigger::ManualApi)
            .await
    }

    pub async fn run_manual_merge_sweep_now(&self) -> Result<()> {
        self.run_merge_sweep_with_trigger(MergeSweepTrigger::ManualApi)
            .await
    }

    pub async fn run_manual_merge_condition_now(&self, condition_id: &str) -> Result<Value> {
        if self.simulation_mode {
            return Err(anyhow!(
                "manual merge condition is disabled in simulation mode"
            ));
        }
        let normalized = condition_id.trim();
        if normalized.is_empty() {
            return Err(anyhow!("condition_id is required"));
        }

        let market = self.api.get_market(normalized).await?;
        if market.tokens.len() < 2 {
            return Err(anyhow!(
                "condition {} has insufficient token outcomes for merge",
                normalized
            ));
        }

        let mut outcome_balances: Vec<Value> = Vec::new();
        let mut pair_shares = f64::INFINITY;
        let mut balance_check_failed = false;
        for token in market.tokens.iter().take(2) {
            match self.api.check_balance_only(token.token_id.as_str()).await {
                Ok(balance) => {
                    let shares = f64::try_from(balance / rust_decimal::Decimal::from(1_000_000u64))
                        .unwrap_or(0.0)
                        .max(0.0);
                    pair_shares = pair_shares.min(shares);
                    outcome_balances.push(json!({
                        "token_id": token.token_id,
                        "outcome": token.outcome,
                        "shares": shares
                    }));
                }
                Err(e) => {
                    balance_check_failed = true;
                    outcome_balances.push(json!({
                        "token_id": token.token_id,
                        "outcome": token.outcome,
                        "shares": null,
                        "balance_check_error": e.to_string()
                    }));
                }
            }
        }
        if !pair_shares.is_finite() {
            pair_shares = 0.0;
        }

        let min_pairs = Self::merge_min_pairs_shares();
        if balance_check_failed {
            let result = json!({
                "submitted": false,
                "condition_id": normalized,
                "reason": "balance_check_failed",
                "pair_shares": pair_shares,
                "min_pairs_shares": min_pairs,
                "outcome_balances": outcome_balances
            });
            Self::push_merge_recent_log(result.clone());
            return Ok(result);
        }

        if pair_shares + 1e-9 < min_pairs {
            let result = json!({
                "submitted": false,
                "condition_id": normalized,
                "reason": "insufficient_pairs",
                "pair_shares": pair_shares,
                "min_pairs_shares": min_pairs,
                "outcome_balances": outcome_balances
            });
            Self::push_merge_recent_log(result.clone());
            return Ok(result);
        }

        let result = match self.api.merge_complete_sets(normalized, pair_shares).await {
            Ok(response) => {
                if response.success {
                    let now_period = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let merge_reason = "manual_condition_merge";
                    self.consume_mm_merge_inventory(
                        crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
                        "mm",
                        now_period,
                        normalized,
                        market.tokens[0].token_id.as_str(),
                        market.tokens[1].token_id.as_str(),
                        pair_shares,
                        response.transaction_hash.as_deref(),
                        Some(merge_reason),
                        "merge_manual",
                    );
                    self.record_merge_cashflow(
                        crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
                        "mm",
                        now_period,
                        normalized,
                        pair_shares,
                        response.transaction_hash.as_deref(),
                        Some(merge_reason),
                        "merge_manual",
                    );
                    let _ = self.sync_trades_with_portfolio().await;
                }
                json!({
                    "submitted": true,
                    "condition_id": normalized,
                    "pair_shares": pair_shares,
                    "min_pairs_shares": min_pairs,
                    "balance_check_failed": balance_check_failed,
                    "success": response.success,
                    "tx_hash": response.transaction_hash,
                    "message": response.message,
                    "outcome_balances": outcome_balances
                })
            }
            Err(e) => json!({
                "submitted": true,
                "condition_id": normalized,
                "pair_shares": pair_shares,
                "min_pairs_shares": min_pairs,
                "balance_check_failed": balance_check_failed,
                "success": false,
                "error": e.to_string(),
                "outcome_balances": outcome_balances
            }),
        };
        Self::push_merge_recent_log(result.clone());
        log_event("merge_condition_submit", result.clone());
        Ok(result)
    }

    async fn check_market_closure_with_trigger(
        &self,
        trigger: RedemptionSweepTrigger,
    ) -> Result<()> {
        // In simulation mode, check simulation tracker positions for market closure
        if self.simulation_mode {
            if let Some(tracker) = &self.simulation_tracker {
                // Get all positions from simulation tracker
                let positions = tracker.get_all_positions().await;

                if positions.is_empty() {
                    return Ok(());
                }

                let current_timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();

                // Check each position for market closure
                for position in positions {
                    // Market closes at end_timestamp from the original pending trade.
                    let market_end_timestamp = {
                        let pending = self.pending_trades.lock().await;
                        pending
                            .values()
                            .find(|t| {
                                t.market_timestamp == position.period_timestamp
                                    && t.token_id == position.token_id
                            })
                            .map(Self::market_end_timestamp)
                            .unwrap_or(position.period_timestamp + 900)
                    };
                    let _seconds_until_close =
                        market_end_timestamp.saturating_sub(current_timestamp);

                    if current_timestamp < market_end_timestamp - 30 {
                        // Market hasn't closed yet
                        continue;
                    }

                    // Market has closed - check resolution
                    let condition_id = position.condition_id.clone();
                    let token_id = position.token_id.clone();
                    let token_type = position.token_type.clone();

                    tracker
                        .log_to_file(&format!(
                            "🔍 Market closed - checking resolution for {} (period: {})",
                            token_type.display_name(),
                            position.period_timestamp
                        ))
                        .await;

                    let (market_closed, token_winner) =
                        match self.check_market_result(&condition_id, &token_id).await {
                            Ok(result) => result,
                            Err(e) => {
                                tracker
                                    .log_to_file(&format!(
                                "⚠️  Failed to check market result: {} - will retry on next check",
                                e
                            ))
                                    .await;
                                continue; // Retry on next check
                            }
                        };

                    if market_closed {
                        // Log market end event
                        tracker
                            .log_market_end(
                                &token_type.display_name(),
                                position.period_timestamp,
                                &condition_id,
                            )
                            .await;
                        let final_value = if token_winner { 1.0 } else { 0.0 };
                        let payout = position.units * final_value;
                        let pnl = payout - position.investment_amount;
                        let (source_timeframe, strategy_id) = {
                            let pending = self.pending_trades.lock().await;
                            let maybe_trade = pending.values().find(|t| {
                                t.market_timestamp == position.period_timestamp
                                    && t.token_id == position.token_id
                            });
                            let source_timeframe = maybe_trade
                                .map(|t| t.source_timeframe.clone())
                                .unwrap_or_else(|| "15m".to_string());
                            let strategy_id = maybe_trade
                                .map(|t| t.strategy_id.clone())
                                .unwrap_or_else(crate::strategy::default_strategy_id);
                            (source_timeframe, strategy_id)
                        };
                        self.record_trade_event_required_strategy(
                            strategy_id.as_str(),
                            Some(Self::build_exit_event_key(
                                strategy_id.as_str(),
                                source_timeframe.as_str(),
                                position.period_timestamp,
                                position.token_id.as_str(),
                                "EXIT",
                                Some("simulation_market_close"),
                            )),
                            position.period_timestamp,
                            Some(source_timeframe.as_str()),
                            Some(condition_id.clone()),
                            Some(position.token_id.clone()),
                            Some(position.token_type.display_name().to_string()),
                            Some("SELL".to_string()),
                            "EXIT",
                            Some(final_value),
                            Some(position.units),
                            Some(payout),
                            Some(pnl),
                            Some("simulation_market_close".to_string()),
                        );

                        // Determine if market resolved Up or Down
                        let market_resolved_up = match token_type {
                            crate::detector::TokenType::BtcUp
                            | crate::detector::TokenType::EthUp
                            | crate::detector::TokenType::SolanaUp
                            | crate::detector::TokenType::XrpUp => token_winner,
                            crate::detector::TokenType::BtcDown
                            | crate::detector::TokenType::EthDown
                            | crate::detector::TokenType::SolanaDown
                            | crate::detector::TokenType::XrpDown => !token_winner,
                        };

                        // Resolve all positions for this market
                        let (spent, earned, pnl) = tracker
                            .resolve_market_positions(&condition_id, market_resolved_up)
                            .await;

                        // Get total spending and earnings
                        let (total_spent, total_earned, total_realized_pnl) =
                            tracker.get_total_spending_and_earnings().await;

                        // Log market resolution summary
                        tracker
                            .log_to_file(&format!(
                                "═══════════════════════════════════════════════════════════\n\
                             🏁 MARKET RESOLUTION SUMMARY\n\
                             ═══════════════════════════════════════════════════════════\n\
                             Market: {} | Period: {} | Condition: {}\n\
                             Resolution: {} {}\n\
                             \n\
                             This Market:\n\
                             - Total Spent: ${:.2}\n\
                             - Total Earned: ${:.2}\n\
                             - Net PnL: ${:.2} {}\n\
                             \n\
                             Overall Totals:\n\
                             - Total Spent (All Markets): ${:.2}\n\
                             - Total Earned (All Markets): ${:.2}\n\
                             - Total Realized PnL: ${:.2} {}\n\
                             ═══════════════════════════════════════════════════════════",
                                token_type.display_name(),
                                position.period_timestamp,
                                &condition_id[..16],
                                if market_resolved_up { "UP" } else { "DOWN" },
                                if market_resolved_up { "✅" } else { "❌" },
                                spent,
                                earned,
                                pnl,
                                if pnl >= 0.0 { "✅" } else { "❌" },
                                total_spent,
                                total_earned,
                                total_realized_pnl,
                                if total_realized_pnl >= 0.0 {
                                    "✅"
                                } else {
                                    "❌"
                                }
                            ))
                            .await;

                        let scorecard = tracker.get_scorecard_report().await;
                        tracker.log_to_file(&scorecard).await;
                    }
                }

                return Ok(());
            }
        }

        let pending_trades: Vec<(String, PendingTrade)> = {
            let pending = self.pending_trades.lock().await;
            pending
                .iter()
                .map(|(key, trade)| (key.clone(), trade.clone()))
                .collect()
        };

        // Claim path is API-first by design; skip DB restore even if env is enabled.
        // DB lifecycle rows can be stale relative to live wallet redeemable state.
        let _restore_from_db_requested = Self::redemption_restore_pending_from_db_enabled();

        let api_redeemable_positions = self.fetch_api_redeemable_positions_for_sweep().await;

        let current_timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        if matches!(trigger, RedemptionSweepTrigger::ManualApi) {
            let _ = Self::request_manual_redemption_sweep_internal();
        }
        let api_redeemable_condition_count = api_redeemable_positions
            .iter()
            .map(|row| row.condition_id.as_str())
            .collect::<HashSet<_>>()
            .len();
        let unsold_count = api_redeemable_condition_count;
        let manual_requested = Self::manual_redemption_request_pending();
        let mut sweep_trigger_name = "scheduler";
        let mut sweep_reason = String::new();
        let mut run_redeem_pass = false;
        if !Self::redemption_sweep_enabled() {
            sweep_trigger_name = "legacy_always_on";
            sweep_reason = "redemption_sweep_disabled_fallback".to_string();
            run_redeem_pass = true;
        } else if manual_requested {
            if matches!(trigger, RedemptionSweepTrigger::ManualApi) {
                sweep_trigger_name = "manual_api";
                sweep_reason = "manual_request_immediate".to_string();
            } else {
                sweep_trigger_name = "manual_request";
                sweep_reason = "manual_request_queued".to_string();
            }
            run_redeem_pass = true;
        } else {
            if Self::redemption_auto_trigger_enabled() {
                let cooldown_sec = Self::redemption_auto_trigger_cooldown_sec();
                let cooldown_remaining = Self::auto_trigger_cooldown_remaining_sec(
                    Self::redemption_last_auto_trigger_epoch_store(),
                    current_timestamp,
                    cooldown_sec,
                );
                if cooldown_remaining == 0 {
                    let pending_threshold = Self::redemption_auto_trigger_pending_threshold();
                    let pending_candidates = api_redeemable_condition_count;

                    let mut auto_trigger_reason: Option<String> = None;
                    if pending_candidates >= pending_threshold {
                        auto_trigger_reason = Some(format!(
                            "pending_conditions={}>=threshold={}",
                            pending_candidates, pending_threshold
                        ));
                    }
                    if auto_trigger_reason.is_none() {
                        let ratio_threshold =
                            Self::redemption_auto_trigger_available_ratio_threshold();
                        if let Some((available_ratio, usdc_balance, portfolio_estimate)) = self
                            .available_ratio_from_pending(
                                &pending_trades,
                                Self::redemption_auto_trigger_balance_timeout_ms(),
                            )
                            .await
                        {
                            if available_ratio < ratio_threshold {
                                auto_trigger_reason = Some(format!(
                                    "available_ratio={:.4}<threshold={:.4},usdc={:.2},portfolio_est={:.2}",
                                    available_ratio, ratio_threshold, usdc_balance, portfolio_estimate
                                ));
                            }
                        }
                    }

                    if let Some(reason) = auto_trigger_reason {
                        if Self::try_claim_auto_trigger_slot(
                            Self::redemption_last_auto_trigger_epoch_store(),
                            current_timestamp,
                            cooldown_sec,
                        ) {
                            sweep_trigger_name = "auto_trigger";
                            sweep_reason = reason;
                            run_redeem_pass = true;
                        }
                    }
                }
            }

            if !run_redeem_pass && Self::redemption_schedule_enabled() {
                let startup_due = matches!(trigger, RedemptionSweepTrigger::StartupCatchup)
                    && Self::should_run_startup_catchup(current_timestamp);
                if Self::try_claim_scheduled_sweep_slot(current_timestamp) {
                    if startup_due && Self::redemption_submit_startup_catchup_enabled() {
                        sweep_trigger_name = "startup_catchup";
                        sweep_reason = format!(
                            "minute_match={},interval_hours={}",
                            Self::redemption_sweep_minute(),
                            Self::redemption_sweep_interval_hours()
                        );
                    } else {
                        sweep_trigger_name = "scheduled_interval";
                        sweep_reason = format!(
                            "minute_match={},interval_hours={}",
                            Self::redemption_sweep_minute(),
                            Self::redemption_sweep_interval_hours()
                        );
                    }
                    run_redeem_pass = true;
                }
            }
        }
        if !run_redeem_pass {
            return Ok(());
        }
        if manual_requested {
            Self::take_manual_redemption_request();
        }
        if unsold_count > 0 {
            crate::log_println!(
                "🧹 Redemption sweep start trigger={} reason={} unsold_trades={}",
                sweep_trigger_name,
                sweep_reason,
                unsold_count
            );
        }
        let sweep_started_ts = current_timestamp;
        #[derive(Clone)]
        struct RedemptionBatchCandidate {
            key: String,
            trade: PendingTrade,
            next_attempt: u32,
            max_redemption_attempts: u32,
            total_cost: f64,
            total_value: f64,
            token_value: f64,
            settled_units: f64,
            profit: f64,
        }
        let mut redemption_quota_pause_logged = false;
        let mut redemption_batch_queue: Vec<RedemptionBatchCandidate> = Vec::new();
        let mut processed_conditions: HashSet<String> = HashSet::new();
        let mut submitted_conditions = 0usize;
        let mut successful_conditions = 0usize;
        let mut skipped_duplicate_conditions = 0usize;
        let mut skipped_condition_retry_window = 0usize;
        let mut skipped_batch_cap = 0usize;
        let max_conditions_per_sweep = if matches!(trigger, RedemptionSweepTrigger::ManualApi) {
            Self::redemption_max_conditions_per_manual_sweep()
        } else {
            Self::redemption_max_conditions_per_sweep()
        };
        let condition_retry_seconds = Self::redemption_condition_retry_seconds();
        let mut pause_remaining_snapshot: Option<u64> = None;

        // API-first direct redemption pass:
        // claim only live redeemable winners reported by Polymarket data API.
        // This avoids DB-stale candidate dependence and skips loser paths entirely.
        let api_redeemable_condition_ids: HashSet<String> = api_redeemable_positions
            .iter()
            .map(|row| row.condition_id.clone())
            .collect();
        if !api_redeemable_positions.is_empty() {
            let mut redeemable_by_condition: HashMap<String, (String, f64)> = HashMap::new();
            for row in api_redeemable_positions {
                let entry = redeemable_by_condition
                    .entry(row.condition_id)
                    .or_insert((row.token_id.clone(), 0.0));
                entry.1 += row.shares.max(0.0);
            }
            let mut api_direct_queue: Vec<(String, String, f64)> = Vec::new();
            for (condition_id, (token_id, redeemable_shares)) in redeemable_by_condition {
                if !condition_id.is_empty() {
                    if !processed_conditions.insert(condition_id.clone()) {
                        skipped_duplicate_conditions =
                            skipped_duplicate_conditions.saturating_add(1);
                        continue;
                    }
                    if submitted_conditions >= max_conditions_per_sweep {
                        skipped_batch_cap = skipped_batch_cap.saturating_add(1);
                        continue;
                    }
                    if let Some(retry_after_epoch) =
                        Self::condition_retry_after_epoch(condition_id.as_str())
                    {
                        if retry_after_epoch > current_timestamp {
                            skipped_condition_retry_window =
                                skipped_condition_retry_window.saturating_add(1);
                            continue;
                        }
                        Self::clear_condition_retry_after_epoch(condition_id.as_str());
                    }
                    if let Some(remaining) = self.redemption_retry_pause_remaining_seconds() {
                        pause_remaining_snapshot = Some(remaining);
                        break;
                    }

                    submitted_conditions = submitted_conditions.saturating_add(1);
                    api_direct_queue.push((condition_id, token_id, redeemable_shares));
                }
            }

            if !api_direct_queue.is_empty() {
                let batch_chunk_size = Self::redemption_batch_conditions_per_request()
                    .min(max_conditions_per_sweep.max(1))
                    .max(1);
                for chunk in api_direct_queue.chunks(batch_chunk_size) {
                    let batch_requests: Vec<RedeemConditionRequest> = chunk
                        .iter()
                        .map(|(condition_id, _, _)| RedeemConditionRequest {
                            condition_id: condition_id.clone(),
                            outcome_label: "WIN".to_string(),
                        })
                        .collect();

                    match self.api.redeem_tokens_batch(&batch_requests).await {
                        Ok(resp) => {
                            let verified_conditions = if resp.success {
                                let condition_ids = chunk
                                    .iter()
                                    .map(|(condition_id, _, _)| condition_id.clone())
                                    .collect::<Vec<_>>();
                                self.verify_redeem_effect_for_conditions(condition_ids.as_slice())
                                    .await
                            } else {
                                HashSet::new()
                            };
                            for (condition_id, _, redeemable_shares) in chunk.iter() {
                                let condition_verified =
                                    verified_conditions.contains(condition_id.as_str());
                                if resp.success && condition_verified {
                                    successful_conditions = successful_conditions.saturating_add(1);
                                    Self::clear_inflight_redemption_tx(condition_id.as_str());
                                    Self::clear_condition_retry_after_epoch(condition_id.as_str());
                                } else if resp.success {
                                    Self::clear_inflight_redemption_tx(condition_id.as_str());
                                    Self::set_condition_retry_after_epoch(
                                        condition_id.as_str(),
                                        current_timestamp.saturating_add(condition_retry_seconds),
                                    );
                                } else if let Some(tx_id) = resp.transaction_hash.as_deref() {
                                    Self::set_inflight_redemption_tx(condition_id.as_str(), tx_id);
                                    Self::set_condition_retry_after_epoch(
                                        condition_id.as_str(),
                                        current_timestamp.saturating_add(condition_retry_seconds),
                                    );
                                } else {
                                    Self::set_condition_retry_after_epoch(
                                        condition_id.as_str(),
                                        current_timestamp.saturating_add(condition_retry_seconds),
                                    );
                                }
                                log_event(
                                    "redemption_api_direct_submit",
                                    json!({
                                        "trigger": sweep_trigger_name,
                                        "condition_id": condition_id,
                                        "redeemable_shares": redeemable_shares,
                                        "success": resp.success,
                                        "effect_verified": condition_verified,
                                        "transaction_hash": resp.transaction_hash,
                                        "message": resp.message,
                                        "batch_submit": true,
                                        "batch_size": chunk.len()
                                    }),
                                );
                            }
                        }
                        Err(e) => {
                            let error_text = e.to_string();
                            if Self::is_relayer_quota_error(&error_text) {
                                let reset_seconds = Self::sanitize_redemption_pause_seconds(
                                    Self::parse_relayer_reset_seconds(&error_text).unwrap_or(600),
                                );
                                self.set_redemption_retry_pause(reset_seconds);
                                pause_remaining_snapshot = Some(reset_seconds);
                            }
                            for (condition_id, _, redeemable_shares) in chunk.iter() {
                                Self::set_condition_retry_after_epoch(
                                    condition_id.as_str(),
                                    current_timestamp.saturating_add(condition_retry_seconds),
                                );
                                log_event(
                                    "redemption_api_direct_submit_failed",
                                    json!({
                                        "trigger": sweep_trigger_name,
                                        "condition_id": condition_id,
                                        "redeemable_shares": redeemable_shares,
                                        "error": error_text,
                                        "batch_submit": true,
                                        "batch_size": chunk.len()
                                    }),
                                );
                            }
                        }
                    }
                }
            }
        }

        for (key, trade) in pending_trades {
            // Skip if already sold
            if trade.sold {
                continue;
            }

            // Market closes at end_timestamp for timeframe-aware positions.
            let market_end_timestamp = Self::market_end_timestamp(&trade);
            let seconds_until_close = market_end_timestamp.saturating_sub(current_timestamp);

            if current_timestamp < market_end_timestamp - 30 {
                // Market hasn't closed yet - log periodically (every 30 seconds) to show we're monitoring
                if seconds_until_close % 30 == 0 || seconds_until_close < 60 {
                    crate::log_println!("⏳ Monitoring trade for market closure: {} token (period: {}), market closes in {}s", 
                        trade.token_type.display_name(), trade.market_timestamp, seconds_until_close);
                }
                continue; // Market hasn't closed yet
            }

            // API-first guard: if live wallet balance is already zero, skip market/result checks and
            // clear this pending row as already redeemed/not held.
            let live_balance_shares = match self
                .check_balance_only_with_timeout(trade.token_id.as_str())
                .await
            {
                Ok(raw_balance) => {
                    let balance_decimal = raw_balance / rust_decimal::Decimal::from(1_000_000u64);
                    f64::try_from(balance_decimal).unwrap_or(0.0).max(0.0)
                }
                Err(e) => {
                    crate::log_println!(
                        "   ⚠️  Failed live balance check for token {}: {} - will retry on next check",
                        &trade.token_id[..trade.token_id.len().min(16)],
                        e
                    );
                    continue;
                }
            };
            if live_balance_shares <= Self::redemption_balance_epsilon_shares() {
                self.update_redemption_tracking_state(
                    &trade,
                    "already_redeemed",
                    None,
                    Some(0.0),
                    Some("api_balance_zero_no_claim_required"),
                );
                let mut pending = self.pending_trades.lock().await;
                if let Some(t) = pending.get_mut(key.as_str()) {
                    t.sold = true;
                }
                pending.remove(&key);
                drop(pending);
                Self::clear_inflight_redemption_tx(trade.condition_id.as_str());
                Self::clear_condition_retry_after_epoch(trade.condition_id.as_str());
                successful_conditions = successful_conditions.saturating_add(1);
                continue;
            }

            // Market has closed (or is about to close) - log that we're checking resolution
            crate::log_println!("🔍 Market closed - checking resolution and attempting redemption for {} token (period: {})", 
                trade.token_type.display_name(), trade.market_timestamp);

            // Check if token won
            crate::log_println!(
                "   📊 Checking market resolution for condition {}...",
                &trade.condition_id[..16]
            );
            let (market_closed, token_winner, resolution_source) =
                match self.check_market_result_with_fallback(&trade).await {
                    Ok(result) => result,
                    Err(e) => {
                        crate::log_println!(
                            "   ⚠️  Failed to check market result: {} - will retry on next check",
                            e
                        );
                        continue; // Retry on next check
                    }
                };

            if market_closed {
                crate::log_println!(
                    "   ✅ Market is closed and resolved (source: {})",
                    resolution_source
                );
                crate::log_println!(
                    "   📊 Token outcome: {} token {}",
                    trade.token_type.display_name(),
                    if token_winner {
                        "WON (worth $1.00)"
                    } else {
                        "LOST (worth $0.00)"
                    }
                );

                // In simulation mode, log market end and resolve all positions for this market
                if self.simulation_mode {
                    if let Some(tracker) = &self.simulation_tracker {
                        // Log market end event
                        tracker
                            .log_market_end(
                                &trade.token_type.display_name(),
                                trade.market_timestamp,
                                &trade.condition_id,
                            )
                            .await;

                        // Determine if market resolved Up or Down based on the token type
                        // If we have an Up token and it won, market resolved Up
                        // If we have a Down token and it won, market resolved Down
                        let market_resolved_up = match trade.token_type {
                            crate::detector::TokenType::BtcUp
                            | crate::detector::TokenType::EthUp
                            | crate::detector::TokenType::SolanaUp
                            | crate::detector::TokenType::XrpUp => {
                                token_winner // Up token won = market resolved Up
                            }
                            crate::detector::TokenType::BtcDown
                            | crate::detector::TokenType::EthDown
                            | crate::detector::TokenType::SolanaDown
                            | crate::detector::TokenType::XrpDown => {
                                !token_winner // Down token won = market resolved Down (so Up = false)
                            }
                        };

                        // Resolve all positions for this market
                        let (spent, earned, pnl) = tracker
                            .resolve_market_positions(&trade.condition_id, market_resolved_up)
                            .await;

                        // Get total spending and earnings
                        let (total_spent, total_earned, total_realized_pnl) =
                            tracker.get_total_spending_and_earnings().await;

                        // Log market resolution summary
                        tracker
                            .log_to_file(&format!(
                                "═══════════════════════════════════════════════════════════\n\
                             🏁 MARKET RESOLUTION SUMMARY\n\
                             ═══════════════════════════════════════════════════════════\n\
                             Market: {} | Period: {} | Condition: {}\n\
                             Resolution: {}\n\
                             \n\
                             This Market:\n\
                             - Total Spent: ${:.2}\n\
                             - Total Earned: ${:.2}\n\
                             - Net PnL: ${:.2}\n\
                             \n\
                             Overall Totals:\n\
                             - Total Spent (All Markets): ${:.2}\n\
                             - Total Earned (All Markets): ${:.2}\n\
                             - Total Realized PnL: ${:.2}\n\
                             ═══════════════════════════════════════════════════════════",
                                trade.token_type.display_name(),
                                trade.market_timestamp,
                                &trade.condition_id[..16],
                                if market_resolved_up { "UP" } else { "DOWN" },
                                spent,
                                earned,
                                pnl,
                                total_spent,
                                total_earned,
                                total_realized_pnl
                            ))
                            .await;

                        let scorecard = tracker.get_scorecard_report().await;
                        tracker.log_to_file(&scorecard).await;
                    }
                }

                // Log MARKET ENDED event to history.toml
                let market_name = trade.token_type.display_name();
                let market_end_event = format!(
                    "MARKET ENDED | Market: {} | Period: {} | Condition: {}",
                    market_name, trade.market_timestamp, trade.condition_id
                );
                crate::log_trading_event(&market_end_event);

                // Determine token value at resolution
                let token_value = if token_winner {
                    1.0 // Token won, worth $1
                } else {
                    0.0 // Token lost, worth $0
                };

                // Use DB position cost/units when available so EXIT PnL matches settlement
                // accounting even when fills differ from provisional trade snapshots.
                let mut settled_units = trade.units;
                let mut total_cost = trade.units * trade.purchase_price;
                if let Some(db) = self.tracking_db.as_ref() {
                    match db.position_entry_basis_for_scope(
                        trade.strategy_id.as_str(),
                        trade.source_timeframe.as_str(),
                        trade.market_timestamp,
                        trade.token_id.as_str(),
                    ) {
                        Ok(Some((entry_units, entry_cost_usd))) => {
                            if entry_units.is_finite() && entry_units > 0.0 {
                                settled_units = entry_units;
                            }
                            if entry_cost_usd.is_finite() && entry_cost_usd > 0.0 {
                                total_cost = entry_cost_usd;
                            }
                        }
                        Ok(None) => {}
                        Err(e) => warn!(
                            "Failed to load position basis for strategy={} tf={} period={} token={}: {}",
                            trade.strategy_id,
                            trade.source_timeframe,
                            trade.market_timestamp,
                            trade.token_id,
                            e
                        ),
                    }
                }
                let total_value = settled_units * token_value;
                let profit = total_value - total_cost;

                // Log structured market result to history.toml
                let result_event = format!(
                    "MARKET RESULT | Market: {} | Period: {} | Outcome: {} | Token Value: ${:.6} | Cost: ${:.6} | Value: ${:.6} | Profit: ${:.6}",
                    market_name,
                    trade.market_timestamp,
                    if token_winner { "WON" } else { "LOST" },
                    token_value,
                    total_cost,
                    total_value,
                    profit
                );
                crate::log_trading_event(&result_event);

                // Accounting is settlement-first: once market resolution is known, emit EXIT
                // regardless of redemption submit outcome. Redemption remains an operational step.
                let market_close_exit_key = Self::build_exit_event_key(
                    trade.strategy_id.as_str(),
                    trade.source_timeframe.as_str(),
                    trade.market_timestamp,
                    trade.token_id.as_str(),
                    "EXIT",
                    Some("production_market_close"),
                );
                let mut exit_already_recorded = false;
                if let Some(db) = self.tracking_db.as_ref() {
                    match db.has_trade_event_key(market_close_exit_key.as_str()) {
                        Ok(v) => exit_already_recorded = v,
                        Err(e) => warn!(
                            "Failed to check existing market-close EXIT event key={}: {}",
                            market_close_exit_key, e
                        ),
                    }
                }
                if !exit_already_recorded {
                    self.record_trade_event_required_strategy(
                        trade.strategy_id.as_str(),
                        Some(market_close_exit_key),
                        trade.market_timestamp,
                        Some(trade.source_timeframe.as_str()),
                        Some(trade.condition_id.clone()),
                        Some(trade.token_id.clone()),
                        Some(trade.token_type.display_name().to_string()),
                        Some("SELL".to_string()),
                        "EXIT",
                        Some(token_value),
                        Some(settled_units),
                        Some(total_value),
                        Some(profit),
                        Some("production_market_close".to_string()),
                    );
                    let mut total = self.total_profit.lock().await;
                    *total += profit;
                    drop(total);
                }

                let redeemable_live_api =
                    api_redeemable_condition_ids.contains(&trade.condition_id);
                // Redeem only when token is a winner AND wallet API reports it redeemable.
                let should_redeem = !self.simulation_mode && token_winner && redeemable_live_api;

                if !should_redeem {
                    if token_winner && !redeemable_live_api {
                        self.update_redemption_tracking_state(
                            &trade,
                            "pending",
                            None,
                            None,
                            Some("waiting_api_redeemable_winner"),
                        );
                        continue;
                    }
                    self.update_redemption_tracking_state(
                        &trade,
                        "not_required",
                        None,
                        Some(total_value),
                        Some("losing_token_no_claim_required"),
                    );
                    let mut pending = self.pending_trades.lock().await;
                    if let Some(t) = pending.get_mut(key.as_str()) {
                        t.sold = true;
                    }
                    pending.remove(&key);
                    drop(pending);
                    Self::clear_inflight_redemption_tx(trade.condition_id.as_str());
                    Self::clear_condition_retry_after_epoch(trade.condition_id.as_str());
                    successful_conditions = successful_conditions.saturating_add(1);
                    continue;
                }

                if !processed_conditions.insert(trade.condition_id.clone()) {
                    skipped_duplicate_conditions = skipped_duplicate_conditions.saturating_add(1);
                    debug!(
                        "Skipping duplicate redemption submit for condition {} in current sweep",
                        &trade.condition_id[..16]
                    );
                    continue;
                }

                if submitted_conditions >= max_conditions_per_sweep {
                    skipped_batch_cap = skipped_batch_cap.saturating_add(1);
                    continue;
                }

                if let Some(retry_after_epoch) =
                    Self::condition_retry_after_epoch(trade.condition_id.as_str())
                {
                    if retry_after_epoch > current_timestamp {
                        skipped_condition_retry_window =
                            skipped_condition_retry_window.saturating_add(1);
                        continue;
                    }
                    Self::clear_condition_retry_after_epoch(trade.condition_id.as_str());
                }

                let redemption_successful = if should_redeem {
                    if let Some(remaining) = self.redemption_retry_pause_remaining_seconds() {
                        self.update_redemption_tracking_state(
                            &trade,
                            "paused",
                            None,
                            None,
                            Some(format!("global_retry_pause_remaining={}s", remaining).as_str()),
                        );
                        pause_remaining_snapshot = Some(remaining);
                        if !redemption_quota_pause_logged {
                            crate::log_println!(
                                "   ⏸️ Redemption paused due to relayer quota limit ({}s remaining). Will retry after reset.",
                                remaining
                            );
                            redemption_quota_pause_logged = true;
                        }
                        continue;
                    }

                    // API-first live balance was already fetched before market-result checks.
                    let current_balance = live_balance_shares;
                    crate::log_println!(
                        "   📊 Current token balance: {:.6} shares",
                        current_balance
                    );

                    // If balance is near-zero, tokens are effectively already redeemed - mark as sold and skip redemption.
                    let redemption_balance_epsilon = Self::redemption_balance_epsilon_shares();
                    if current_balance <= redemption_balance_epsilon {
                        crate::log_println!(
                            "   ✅ Token balance {:.6} <= epsilon {:.6} - tokens already redeemed (or dust)",
                            current_balance,
                            redemption_balance_epsilon
                        );
                        crate::log_println!("   📋 Marking trade as sold - no redemption needed");

                        // Log structured redemption status (already redeemed) to history.toml
                        let market_name = trade.token_type.display_name();
                        let redeem_event = format!(
                            "REDEMPTION STATUS | Market: {} | Period: {} | Status: ALREADY_REDEEMED",
                            market_name,
                            trade.market_timestamp
                        );
                        crate::log_trading_event(&redeem_event);
                        self.update_redemption_tracking_state(
                            &trade,
                            "already_redeemed",
                            None,
                            Some(total_value),
                            Some("wallet_balance_zero_before_submit"),
                        );
                        self.record_redemption_cashflow(
                            &trade,
                            "REDEMPTION_ALREADY_REDEEMED",
                            Some(total_value),
                            Some(settled_units),
                            Some(token_value),
                            None,
                            Some("wallet_balance_zero_before_submit"),
                        );

                        // Mark as sold and remove trade
                        let mut pending = self.pending_trades.lock().await;
                        if let Some(t) = pending.get_mut(key.as_str()) {
                            t.sold = true;
                        }
                        pending.remove(&key);
                        drop(pending);
                        Self::clear_inflight_redemption_tx(trade.condition_id.as_str());
                        successful_conditions = successful_conditions.saturating_add(1);

                        crate::log_println!("💰 Market Closed - Trade Already Redeemed");
                        crate::log_println!("   Token Type: {}", trade.token_type.display_name());
                        crate::log_println!(
                            "   Outcome: {} token {}",
                            trade.token_type.display_name(),
                            if token_winner { "won" } else { "lost" }
                        );
                        crate::log_println!("   Token value at resolution: ${:.6}", token_value);
                        crate::log_println!(
                            "   Total cost: ${:.6} | Total value: ${:.6} | Profit: ${:.6}",
                            total_cost,
                            total_value,
                            profit
                        );
                        crate::log_println!("   ✅ Trade closed (tokens were already redeemed)");
                        continue; // Move to next trade
                    }

                    // Balance > 0 - proceed with redemption.
                    let mut trade_mut = trade.clone();
                    let max_redemption_attempts = 20;
                    let mut confirmed_tx_id: Option<String> = None;

                    let existing_tx_confirmed = if let Some(inflight_tx) =
                        Self::get_inflight_redemption_tx(trade.condition_id.as_str())
                    {
                        crate::log_println!(
                            "   🔁 Existing redemption tx {} found. Rechecking status before new submission...",
                            inflight_tx
                        );
                        match self.api.check_relayer_transaction(&inflight_tx).await {
                            Ok(status) if status.success => {
                                Self::clear_inflight_redemption_tx(trade.condition_id.as_str());
                                confirmed_tx_id = Some(inflight_tx.clone());
                                crate::log_println!(
                                    "   ✅ Existing redemption transaction confirmed: {}",
                                    inflight_tx
                                );
                                true
                            }
                            Ok(_) => {
                                crate::log_println!(
                                    "   ⏳ Redemption tx {} still pending; skipping duplicate submission this cycle",
                                    inflight_tx
                                );
                                self.update_redemption_tracking_state(
                                    &trade,
                                    "submitted",
                                    Some(inflight_tx.as_str()),
                                    None,
                                    Some("redemption_tx_pending_confirmation"),
                                );
                                continue;
                            }
                            Err(e) => {
                                warn!(
                                    "Pending redemption tx {} failed status check: {}. Submitting a new redemption.",
                                    inflight_tx, e
                                );
                                Self::clear_inflight_redemption_tx(trade.condition_id.as_str());
                                false
                            }
                        }
                    } else {
                        false
                    };
                    if existing_tx_confirmed {
                        let effect_verified = self
                            .verify_redeem_effect_for_condition(trade.condition_id.as_str())
                            .await;
                        if effect_verified {
                            Self::clear_condition_retry_after_epoch(trade.condition_id.as_str());
                            let market_name = trade.token_type.display_name();
                            let redeem_event = format!(
                                "REDEMPTION SUCCESS | Market: {} | Period: {} | Attempt: {} | Status: SUCCESS",
                                market_name,
                                trade.market_timestamp,
                                trade.redemption_attempts.max(1)
                            );
                            crate::log_trading_event(&redeem_event);
                            self.update_redemption_tracking_state(
                                &trade,
                                "success",
                                confirmed_tx_id.as_deref(),
                                Some(total_value),
                                Some("redemption_tx_confirmed"),
                            );
                            self.record_redemption_cashflow(
                                &trade,
                                "REDEMPTION_CONFIRMED",
                                Some(total_value),
                                Some(settled_units),
                                Some(token_value),
                                confirmed_tx_id.as_deref(),
                                Some("redemption_tx_confirmed"),
                            );
                            true
                        } else {
                            Self::set_condition_retry_after_epoch(
                                trade.condition_id.as_str(),
                                current_timestamp.saturating_add(condition_retry_seconds),
                            );
                            self.update_redemption_tracking_state(
                                &trade,
                                "submitted",
                                confirmed_tx_id.as_deref(),
                                None,
                                Some("redemption_tx_confirmed_effect_unverified"),
                            );
                            crate::log_println!(
                                "   ⏳ Redemption tx confirmed but effect not verified yet for {}. Will retry verification.",
                                trade.condition_id
                            );
                            continue;
                        }
                    } else {
                        let next_attempt = trade.redemption_attempts.saturating_add(1);
                        if trade.claim_on_closure {
                            crate::log_println!(
                                "   📋 Auto-redeeming tokens (marked due to insufficient liquidity) - attempt {} (balance: {:.6})",
                                next_attempt,
                                current_balance
                            );
                        } else if token_winner {
                            crate::log_println!(
                                "   📋 Auto-redeeming winning tokens (token won - worth $1.00) - attempt {} (balance: {:.6})",
                                next_attempt,
                                current_balance
                            );
                        } else {
                            crate::log_println!(
                                "   📋 Auto-redeeming losing tokens (token lost - worth $0.00, but redeeming to close position) - attempt {} (balance: {:.6})",
                                next_attempt,
                                current_balance
                            );
                        }

                        if Self::redemption_batch_submit_enabled() {
                            submitted_conditions = submitted_conditions.saturating_add(1);
                            trade_mut.redemption_attempts = next_attempt;
                            self.update_redemption_tracking_state(
                                &trade_mut,
                                "queued",
                                None,
                                None,
                                Some("redemption_batch_queued"),
                            );
                            redemption_batch_queue.push(RedemptionBatchCandidate {
                                key: key.clone(),
                                trade: trade_mut.clone(),
                                next_attempt,
                                max_redemption_attempts,
                                total_cost,
                                total_value,
                                token_value,
                                settled_units,
                                profit,
                            });
                            continue;
                        }

                        // Redeem tokens - pass trade data directly to avoid lookup issues.
                        submitted_conditions = submitted_conditions.saturating_add(1);
                        match self.redeem_token_by_id_with_trade(&trade_mut).await {
                            Ok(resp) => {
                                trade_mut.redemption_attempts = next_attempt;
                                if resp.success {
                                    let effect_verified = self
                                        .verify_redeem_effect_for_condition(
                                            trade.condition_id.as_str(),
                                        )
                                        .await;
                                    if effect_verified {
                                        Self::clear_inflight_redemption_tx(
                                            trade.condition_id.as_str(),
                                        );
                                        Self::clear_condition_retry_after_epoch(
                                            trade.condition_id.as_str(),
                                        );
                                        crate::log_println!(
                                            "   ✅ Tokens redeemed successfully (attempt {})",
                                            trade_mut.redemption_attempts
                                        );

                                        // Log structured redemption success to history.toml
                                        let market_name = trade.token_type.display_name();
                                        let redeem_event = format!(
                                        "REDEMPTION SUCCESS | Market: {} | Period: {} | Attempt: {} | Status: SUCCESS",
                                        market_name,
                                        trade.market_timestamp,
                                        trade_mut.redemption_attempts
                                    );
                                        crate::log_trading_event(&redeem_event);
                                        self.update_redemption_tracking_state(
                                            &trade_mut,
                                            "success",
                                            resp.transaction_hash.as_deref(),
                                            Some(total_value),
                                            Some("redemption_submit_success"),
                                        );
                                        self.record_redemption_cashflow(
                                            &trade_mut,
                                            "REDEMPTION_SUCCESS",
                                            Some(total_value),
                                            Some(settled_units),
                                            Some(token_value),
                                            resp.transaction_hash.as_deref(),
                                            Some("redemption_submit_success"),
                                        );
                                        true
                                    } else {
                                        Self::clear_inflight_redemption_tx(
                                            trade.condition_id.as_str(),
                                        );
                                        Self::set_condition_retry_after_epoch(
                                            trade.condition_id.as_str(),
                                            current_timestamp
                                                .saturating_add(condition_retry_seconds),
                                        );
                                        let mut pending = self.pending_trades.lock().await;
                                        if let Some(t) = pending.get_mut(key.as_str()) {
                                            t.redemption_attempts = trade_mut.redemption_attempts;
                                        }
                                        drop(pending);
                                        self.update_redemption_tracking_state(
                                            &trade_mut,
                                            "submitted",
                                            resp.transaction_hash.as_deref(),
                                            None,
                                            Some("redemption_submit_effect_unverified"),
                                        );
                                        crate::log_println!(
                                            "   ⏳ Redemption submit confirmed but effect not verified yet for {}. Will retry verification.",
                                            trade.condition_id
                                        );
                                        continue;
                                    }
                                } else if let Some(tx_id) = resp.transaction_hash.as_deref() {
                                    Self::set_inflight_redemption_tx(
                                        trade.condition_id.as_str(),
                                        tx_id,
                                    );
                                    Self::set_condition_retry_after_epoch(
                                        trade.condition_id.as_str(),
                                        current_timestamp.saturating_add(condition_retry_seconds),
                                    );
                                    let mut pending = self.pending_trades.lock().await;
                                    if let Some(t) = pending.get_mut(key.as_str()) {
                                        t.redemption_attempts = trade_mut.redemption_attempts;
                                    }
                                    drop(pending);
                                    self.update_redemption_tracking_state(
                                        &trade_mut,
                                        "submitted",
                                        Some(tx_id),
                                        None,
                                        Some("redemption_submitted_waiting_confirmation"),
                                    );
                                    crate::log_println!(
                                    "   ⏳ Redemption submitted (tx {}). Awaiting confirmation; no duplicate submit this cycle.",
                                    tx_id
                                );
                                    continue;
                                } else {
                                    let err_msg = resp.message.unwrap_or_else(|| {
                                        "Redemption unconfirmed without tx id".to_string()
                                    });
                                    trade_mut.redemption_attempts = next_attempt;
                                    warn!(
                                        "Redemption response had no tx id (attempt {}/{}): {}",
                                        trade_mut.redemption_attempts,
                                        max_redemption_attempts,
                                        err_msg
                                    );
                                    let mut pending = self.pending_trades.lock().await;
                                    if let Some(t) = pending.get_mut(key.as_str()) {
                                        *t = trade_mut.clone();
                                    }
                                    drop(pending);
                                    self.update_redemption_tracking_state(
                                        &trade_mut,
                                        "retrying",
                                        None,
                                        None,
                                        Some(err_msg.as_str()),
                                    );
                                    Self::set_condition_retry_after_epoch(
                                        trade.condition_id.as_str(),
                                        current_timestamp.saturating_add(condition_retry_seconds),
                                    );
                                    continue;
                                }
                            }
                            Err(e) => {
                                let error_text = e.to_string();

                                if Self::is_relayer_quota_error(&error_text) {
                                    let reset_seconds = Self::sanitize_redemption_pause_seconds(
                                        Self::parse_relayer_reset_seconds(&error_text)
                                            .unwrap_or(600),
                                    );
                                    self.set_redemption_retry_pause(reset_seconds);
                                    Self::set_condition_retry_after_epoch(
                                        trade.condition_id.as_str(),
                                        current_timestamp.saturating_add(reset_seconds),
                                    );
                                    pause_remaining_snapshot = Some(reset_seconds);
                                    crate::log_println!(
                                    "   ⏸️ Relayer quota hit. Pausing redemption retries for {}s (attempt counter unchanged).",
                                    reset_seconds
                                );

                                    let market_name = trade.token_type.display_name();
                                    let redeem_event = format!(
                                    "REDEMPTION PAUSED | Market: {} | Period: {} | Reason: RELAYER_QUOTA | ResumeInSec: {}",
                                    market_name,
                                    trade.market_timestamp,
                                    reset_seconds
                                );
                                    crate::log_trading_event(&redeem_event);
                                    self.update_redemption_tracking_state(
                                        &trade_mut,
                                        "paused",
                                        None,
                                        None,
                                        Some(
                                            format!(
                                                "relayer_quota_pause={}s error={}",
                                                reset_seconds, error_text
                                            )
                                            .as_str(),
                                        ),
                                    );
                                    continue;
                                }

                                trade_mut.redemption_attempts = next_attempt;
                                crate::log_println!(
                                    "   ❌ Failed to redeem tokens (attempt {}/{}): {}",
                                    trade_mut.redemption_attempts,
                                    max_redemption_attempts,
                                    error_text
                                );
                                Self::set_condition_retry_after_epoch(
                                    trade.condition_id.as_str(),
                                    current_timestamp.saturating_add(condition_retry_seconds),
                                );

                                if trade_mut.redemption_attempts >= max_redemption_attempts {
                                    crate::log_println!(
                                        "   ⚠️  Maximum redemption attempts ({}) reached",
                                        max_redemption_attempts
                                    );
                                    crate::log_println!("   📋 Marking trade as abandoned - will not block new positions");
                                    trade_mut.redemption_abandoned = true;

                                    // Log structured redemption failure to history.toml
                                    let market_name = trade.token_type.display_name();
                                    let redeem_event = format!(
                                    "REDEMPTION FAILED | Market: {} | Period: {} | Attempts: {} | Status: ABANDONED | Error: {}",
                                    market_name,
                                    trade.market_timestamp,
                                    trade_mut.redemption_attempts,
                                    error_text.chars().take(100).collect::<String>() // Truncate long errors
                                );
                                    crate::log_trading_event(&redeem_event);

                                    // Update trade in HashMap
                                    let mut pending = self.pending_trades.lock().await;
                                    if let Some(t) = pending.get_mut(key.as_str()) {
                                        *t = trade_mut.clone();
                                    }
                                    drop(pending);
                                    self.update_redemption_tracking_state(
                                        &trade_mut,
                                        "abandoned",
                                        None,
                                        None,
                                        Some(error_text.as_str()),
                                    );

                                    crate::log_println!("   ✅ Trade abandoned - you can manually redeem later if needed");
                                    crate::log_println!("   💡 New positions in new markets will NOT be blocked by this trade");
                                } else {
                                    warn!(
                                    "⚠️  Token redemption failed: {} - queued for next scheduled sweep (attempt {}/{})",
                                    error_text,
                                    trade_mut.redemption_attempts,
                                    max_redemption_attempts
                                );

                                    // Log structured redemption retry to history.toml
                                    let market_name = trade.token_type.display_name();
                                    let redeem_event = format!(
                                    "REDEMPTION RETRY | Market: {} | Period: {} | Attempt: {}/{} | Status: RETRYING",
                                    market_name,
                                    trade.market_timestamp,
                                    trade_mut.redemption_attempts,
                                    max_redemption_attempts
                                );
                                    crate::log_trading_event(&redeem_event);

                                    // Update trade with incremented attempts counter
                                    let mut pending = self.pending_trades.lock().await;
                                    if let Some(t) = pending.get_mut(key.as_str()) {
                                        *t = trade_mut.clone();
                                    }
                                    drop(pending);
                                    self.update_redemption_tracking_state(
                                        &trade_mut,
                                        "retrying",
                                        None,
                                        None,
                                        Some(error_text.as_str()),
                                    );
                                }

                                continue;
                            }
                        }
                    }
                } else {
                    // Simulation mode - just log what would happen
                    if token_winner {
                        crate::log_println!(
                            "   🎮 SIMULATION: Would redeem winning tokens (worth $1.00)"
                        );
                    } else {
                        crate::log_println!(
                            "   🎮 SIMULATION: Would redeem losing tokens (worth $0.00)"
                        );
                    }
                    true // In simulation, consider it successful
                };

                // Only log settlement and remove trade if redemption was successful
                // If redemption failed, the trade remains for retry
                if redemption_successful {
                    successful_conditions = successful_conditions.saturating_add(1);
                    let (_, total_profit) = self.reporting_totals().await;
                    crate::log_println!("💰 Market Closed - Momentum Trade Settled");
                    crate::log_println!("   Token Type: {}", trade.token_type.display_name());
                    crate::log_println!(
                        "   Outcome: {} token {}",
                        trade.token_type.display_name(),
                        if token_winner { "won" } else { "lost" }
                    );
                    crate::log_println!("   Token value at resolution: ${:.6}", token_value);
                    crate::log_println!(
                        "   Total cost: ${:.6} | Total value: ${:.6} | Profit: ${:.6}",
                        total_cost,
                        total_value,
                        profit
                    );
                    crate::log_println!("   Total profit (all trades): ${:.6}", total_profit);
                    crate::log_println!("   ✅ Trade closed and removed from pending trades");

                    // Mark as sold and remove trade
                    let mut pending = self.pending_trades.lock().await;
                    if let Some(t) = pending.get_mut(key.as_str()) {
                        t.sold = true;
                    }
                    pending.remove(&key);
                    drop(pending);
                    Self::clear_inflight_redemption_tx(trade.condition_id.as_str());
                    Self::clear_condition_retry_after_epoch(trade.condition_id.as_str());
                }
            } else {
                crate::log_println!(
                    "   ⏳ Market not resolved yet (closed=false) - will retry on next check"
                );
            }
        }

        if Self::redemption_batch_submit_enabled() && !redemption_batch_queue.is_empty() {
            let batch_chunk_size = Self::redemption_batch_conditions_per_request()
                .min(max_conditions_per_sweep.max(1));
            crate::log_println!(
                "🧺 Redemption batch queue flush: queued_conditions={} chunk_size={}",
                redemption_batch_queue.len(),
                batch_chunk_size
            );
            for chunk in redemption_batch_queue.chunks(batch_chunk_size) {
                let batch_requests: Vec<RedeemConditionRequest> = chunk
                    .iter()
                    .map(|candidate| RedeemConditionRequest {
                        condition_id: candidate.trade.condition_id.clone(),
                        outcome_label: match candidate.trade.token_type {
                            crate::detector::TokenType::BtcUp
                            | crate::detector::TokenType::EthUp
                            | crate::detector::TokenType::SolanaUp
                            | crate::detector::TokenType::XrpUp => "Up".to_string(),
                            crate::detector::TokenType::BtcDown
                            | crate::detector::TokenType::EthDown
                            | crate::detector::TokenType::SolanaDown
                            | crate::detector::TokenType::XrpDown => "Down".to_string(),
                        },
                    })
                    .collect();

                match self.api.redeem_tokens_batch(&batch_requests).await {
                    Ok(resp) if resp.success => {
                        let condition_ids = chunk
                            .iter()
                            .map(|candidate| candidate.trade.condition_id.clone())
                            .collect::<Vec<_>>();
                        let verified_conditions = self
                            .verify_redeem_effect_for_conditions(condition_ids.as_slice())
                            .await;
                        for candidate in chunk.iter().cloned() {
                            let mut trade_mut = candidate.trade.clone();
                            trade_mut.redemption_attempts = candidate.next_attempt;
                            let condition_verified =
                                verified_conditions.contains(trade_mut.condition_id.as_str());
                            if condition_verified {
                                Self::clear_inflight_redemption_tx(trade_mut.condition_id.as_str());
                                Self::clear_condition_retry_after_epoch(
                                    trade_mut.condition_id.as_str(),
                                );
                                let market_name = trade_mut.token_type.display_name();
                                let redeem_event = format!(
                                    "REDEMPTION SUCCESS | Market: {} | Period: {} | Attempt: {} | Status: SUCCESS",
                                    market_name, trade_mut.market_timestamp, trade_mut.redemption_attempts
                                );
                                crate::log_trading_event(&redeem_event);
                                self.update_redemption_tracking_state(
                                    &trade_mut,
                                    "success",
                                    resp.transaction_hash.as_deref(),
                                    Some(candidate.total_value),
                                    Some("redemption_batch_submit_success"),
                                );
                                self.record_redemption_cashflow(
                                    &trade_mut,
                                    "REDEMPTION_SUCCESS",
                                    Some(candidate.total_value),
                                    Some(candidate.settled_units),
                                    Some(candidate.token_value),
                                    resp.transaction_hash.as_deref(),
                                    Some("redemption_batch_submit_success"),
                                );
                                let mut pending = self.pending_trades.lock().await;
                                if let Some(t) = pending.get_mut(candidate.key.as_str()) {
                                    t.sold = true;
                                    t.redemption_attempts = trade_mut.redemption_attempts;
                                }
                                pending.remove(candidate.key.as_str());
                                drop(pending);
                                successful_conditions = successful_conditions.saturating_add(1);
                                let (_, total_profit) = self.reporting_totals().await;
                                crate::log_println!(
                                    "💰 Market Closed - Momentum Trade Settled (batch)"
                                );
                                crate::log_println!(
                                    "   Token Type: {}",
                                    trade_mut.token_type.display_name()
                                );
                                crate::log_println!(
                                    "   Outcome: {} token {}",
                                    trade_mut.token_type.display_name(),
                                    if candidate.token_value > 0.0 {
                                        "won"
                                    } else {
                                        "lost"
                                    }
                                );
                                crate::log_println!(
                                    "   Token value at resolution: ${:.6}",
                                    candidate.token_value
                                );
                                crate::log_println!(
                                    "   Total cost: ${:.6} | Total value: ${:.6} | Profit: ${:.6}",
                                    candidate.total_cost,
                                    candidate.total_value,
                                    candidate.profit
                                );
                                crate::log_println!(
                                    "   Total profit (all trades): ${:.6}",
                                    total_profit
                                );
                                crate::log_println!(
                                    "   ✅ Trade closed and removed from pending trades"
                                );
                            } else {
                                Self::clear_inflight_redemption_tx(trade_mut.condition_id.as_str());
                                Self::set_condition_retry_after_epoch(
                                    trade_mut.condition_id.as_str(),
                                    current_timestamp.saturating_add(condition_retry_seconds),
                                );
                                let mut pending = self.pending_trades.lock().await;
                                if let Some(t) = pending.get_mut(candidate.key.as_str()) {
                                    t.redemption_attempts = trade_mut.redemption_attempts;
                                }
                                drop(pending);
                                self.update_redemption_tracking_state(
                                    &trade_mut,
                                    "submitted",
                                    resp.transaction_hash.as_deref(),
                                    None,
                                    Some("redemption_batch_submit_effect_unverified"),
                                );
                                crate::log_println!(
                                    "   ⏳ Redemption batch tx confirmed but effect not verified yet for {}. Will retry verification.",
                                    trade_mut.condition_id
                                );
                            }
                        }
                    }
                    Ok(resp) => {
                        if let Some(tx_id) = resp.transaction_hash.as_deref() {
                            for candidate in chunk.iter().cloned() {
                                let mut trade_mut = candidate.trade.clone();
                                trade_mut.redemption_attempts = candidate.next_attempt;
                                Self::set_inflight_redemption_tx(
                                    trade_mut.condition_id.as_str(),
                                    tx_id,
                                );
                                Self::set_condition_retry_after_epoch(
                                    trade_mut.condition_id.as_str(),
                                    current_timestamp.saturating_add(condition_retry_seconds),
                                );
                                let mut pending = self.pending_trades.lock().await;
                                if let Some(t) = pending.get_mut(candidate.key.as_str()) {
                                    t.redemption_attempts = trade_mut.redemption_attempts;
                                }
                                drop(pending);
                                self.update_redemption_tracking_state(
                                    &trade_mut,
                                    "submitted",
                                    Some(tx_id),
                                    None,
                                    Some("redemption_batch_submitted_waiting_confirmation"),
                                );
                            }
                            crate::log_println!(
                                "   ⏳ Redemption batch submitted (tx {}). Awaiting confirmation.",
                                tx_id
                            );
                            continue;
                        }

                        let err_msg = resp.message.unwrap_or_else(|| {
                            "Batch redemption unconfirmed without tx id".to_string()
                        });
                        for candidate in chunk.iter().cloned() {
                            let mut trade_mut = candidate.trade.clone();
                            trade_mut.redemption_attempts = candidate.next_attempt;
                            Self::set_condition_retry_after_epoch(
                                trade_mut.condition_id.as_str(),
                                current_timestamp.saturating_add(condition_retry_seconds),
                            );
                            if trade_mut.redemption_attempts >= candidate.max_redemption_attempts {
                                trade_mut.redemption_abandoned = true;
                                let market_name = trade_mut.token_type.display_name();
                                let redeem_event = format!(
                                    "REDEMPTION FAILED | Market: {} | Period: {} | Attempts: {} | Status: ABANDONED | Error: {}",
                                    market_name,
                                    trade_mut.market_timestamp,
                                    trade_mut.redemption_attempts,
                                    err_msg.chars().take(100).collect::<String>()
                                );
                                crate::log_trading_event(&redeem_event);
                                let mut pending = self.pending_trades.lock().await;
                                if let Some(t) = pending.get_mut(candidate.key.as_str()) {
                                    *t = trade_mut.clone();
                                }
                                drop(pending);
                                self.update_redemption_tracking_state(
                                    &trade_mut,
                                    "abandoned",
                                    None,
                                    None,
                                    Some(err_msg.as_str()),
                                );
                            } else {
                                let market_name = trade_mut.token_type.display_name();
                                let redeem_event = format!(
                                    "REDEMPTION RETRY | Market: {} | Period: {} | Attempt: {}/{} | Status: RETRYING",
                                    market_name,
                                    trade_mut.market_timestamp,
                                    trade_mut.redemption_attempts,
                                    candidate.max_redemption_attempts
                                );
                                crate::log_trading_event(&redeem_event);
                                let mut pending = self.pending_trades.lock().await;
                                if let Some(t) = pending.get_mut(candidate.key.as_str()) {
                                    *t = trade_mut.clone();
                                }
                                drop(pending);
                                self.update_redemption_tracking_state(
                                    &trade_mut,
                                    "retrying",
                                    None,
                                    None,
                                    Some(err_msg.as_str()),
                                );
                            }
                        }
                    }
                    Err(e) => {
                        let error_text = e.to_string();
                        if Self::is_relayer_quota_error(&error_text) {
                            let reset_seconds = Self::sanitize_redemption_pause_seconds(
                                Self::parse_relayer_reset_seconds(&error_text).unwrap_or(600),
                            );
                            self.set_redemption_retry_pause(reset_seconds);
                            pause_remaining_snapshot = Some(reset_seconds);
                            if !redemption_quota_pause_logged {
                                crate::log_println!(
                                    "   ⏸️ Relayer quota hit. Pausing redemption retries for {}s (batch path).",
                                    reset_seconds
                                );
                                redemption_quota_pause_logged = true;
                            }
                            for candidate in chunk.iter().cloned() {
                                let trade_mut = candidate.trade.clone();
                                Self::set_condition_retry_after_epoch(
                                    trade_mut.condition_id.as_str(),
                                    current_timestamp.saturating_add(reset_seconds),
                                );
                                let market_name = trade_mut.token_type.display_name();
                                let redeem_event = format!(
                                    "REDEMPTION PAUSED | Market: {} | Period: {} | Reason: RELAYER_QUOTA | ResumeInSec: {}",
                                    market_name, trade_mut.market_timestamp, reset_seconds
                                );
                                crate::log_trading_event(&redeem_event);
                                self.update_redemption_tracking_state(
                                    &trade_mut,
                                    "paused",
                                    None,
                                    None,
                                    Some(
                                        format!(
                                            "relayer_quota_pause={}s error={}",
                                            reset_seconds, error_text
                                        )
                                        .as_str(),
                                    ),
                                );
                            }
                            continue;
                        }

                        crate::log_println!(
                            "   ⚠️ Batch redemption failed: {}. Falling back to per-condition redemption submits for this chunk.",
                            error_text
                        );
                        for candidate in chunk.iter().cloned() {
                            let mut trade_mut = candidate.trade.clone();
                            trade_mut.redemption_attempts = candidate.next_attempt;
                            match self.redeem_token_by_id_with_trade(&trade_mut).await {
                                Ok(single_resp) if single_resp.success => {
                                    let condition_verified = self
                                        .verify_redeem_effect_for_condition(
                                            trade_mut.condition_id.as_str(),
                                        )
                                        .await;
                                    if condition_verified {
                                        Self::clear_inflight_redemption_tx(
                                            trade_mut.condition_id.as_str(),
                                        );
                                        Self::clear_condition_retry_after_epoch(
                                            trade_mut.condition_id.as_str(),
                                        );
                                        let market_name = trade_mut.token_type.display_name();
                                        let redeem_event = format!(
                                            "REDEMPTION SUCCESS | Market: {} | Period: {} | Attempt: {} | Status: SUCCESS_FALLBACK_SINGLE",
                                            market_name, trade_mut.market_timestamp, trade_mut.redemption_attempts
                                        );
                                        crate::log_trading_event(&redeem_event);
                                        self.update_redemption_tracking_state(
                                            &trade_mut,
                                            "success",
                                            single_resp.transaction_hash.as_deref(),
                                            Some(candidate.total_value),
                                            Some("redemption_batch_fallback_single_success"),
                                        );
                                        self.record_redemption_cashflow(
                                            &trade_mut,
                                            "REDEMPTION_SUCCESS",
                                            Some(candidate.total_value),
                                            Some(candidate.settled_units),
                                            Some(candidate.token_value),
                                            single_resp.transaction_hash.as_deref(),
                                            Some("redemption_batch_fallback_single_success"),
                                        );
                                        let mut pending = self.pending_trades.lock().await;
                                        if let Some(t) = pending.get_mut(candidate.key.as_str()) {
                                            t.sold = true;
                                            t.redemption_attempts = trade_mut.redemption_attempts;
                                        }
                                        pending.remove(candidate.key.as_str());
                                        drop(pending);
                                        successful_conditions =
                                            successful_conditions.saturating_add(1);
                                    } else {
                                        Self::clear_inflight_redemption_tx(
                                            trade_mut.condition_id.as_str(),
                                        );
                                        Self::set_condition_retry_after_epoch(
                                            trade_mut.condition_id.as_str(),
                                            current_timestamp
                                                .saturating_add(condition_retry_seconds),
                                        );
                                        let mut pending = self.pending_trades.lock().await;
                                        if let Some(t) = pending.get_mut(candidate.key.as_str()) {
                                            t.redemption_attempts = trade_mut.redemption_attempts;
                                        }
                                        drop(pending);
                                        self.update_redemption_tracking_state(
                                            &trade_mut,
                                            "submitted",
                                            single_resp.transaction_hash.as_deref(),
                                            None,
                                            Some("redemption_batch_fallback_single_effect_unverified"),
                                        );
                                    }
                                }
                                Ok(single_resp) => {
                                    if let Some(tx_id) = single_resp.transaction_hash.as_deref() {
                                        Self::set_inflight_redemption_tx(
                                            trade_mut.condition_id.as_str(),
                                            tx_id,
                                        );
                                        Self::set_condition_retry_after_epoch(
                                            trade_mut.condition_id.as_str(),
                                            current_timestamp
                                                .saturating_add(condition_retry_seconds),
                                        );
                                        let mut pending = self.pending_trades.lock().await;
                                        if let Some(t) = pending.get_mut(candidate.key.as_str()) {
                                            t.redemption_attempts = trade_mut.redemption_attempts;
                                        }
                                        drop(pending);
                                        self.update_redemption_tracking_state(
                                            &trade_mut,
                                            "submitted",
                                            Some(tx_id),
                                            None,
                                            Some(
                                                "redemption_batch_fallback_single_submitted_waiting_confirmation",
                                            ),
                                        );
                                        continue;
                                    }

                                    let single_error_text =
                                        single_resp.message.unwrap_or_else(|| {
                                            "Fallback redemption unconfirmed without tx id"
                                                .to_string()
                                        });
                                    Self::set_condition_retry_after_epoch(
                                        trade_mut.condition_id.as_str(),
                                        current_timestamp.saturating_add(condition_retry_seconds),
                                    );
                                    if trade_mut.redemption_attempts
                                        >= candidate.max_redemption_attempts
                                    {
                                        trade_mut.redemption_abandoned = true;
                                        let market_name = trade_mut.token_type.display_name();
                                        let redeem_event = format!(
                                            "REDEMPTION FAILED | Market: {} | Period: {} | Attempts: {} | Status: ABANDONED | Error: {}",
                                            market_name,
                                            trade_mut.market_timestamp,
                                            trade_mut.redemption_attempts,
                                            single_error_text.chars().take(100).collect::<String>()
                                        );
                                        crate::log_trading_event(&redeem_event);
                                        let mut pending = self.pending_trades.lock().await;
                                        if let Some(t) = pending.get_mut(candidate.key.as_str()) {
                                            *t = trade_mut.clone();
                                        }
                                        drop(pending);
                                        self.update_redemption_tracking_state(
                                            &trade_mut,
                                            "abandoned",
                                            None,
                                            None,
                                            Some(single_error_text.as_str()),
                                        );
                                    } else {
                                        let market_name = trade_mut.token_type.display_name();
                                        let redeem_event = format!(
                                            "REDEMPTION RETRY | Market: {} | Period: {} | Attempt: {}/{} | Status: RETRYING",
                                            market_name,
                                            trade_mut.market_timestamp,
                                            trade_mut.redemption_attempts,
                                            candidate.max_redemption_attempts
                                        );
                                        crate::log_trading_event(&redeem_event);
                                        let mut pending = self.pending_trades.lock().await;
                                        if let Some(t) = pending.get_mut(candidate.key.as_str()) {
                                            *t = trade_mut.clone();
                                        }
                                        drop(pending);
                                        self.update_redemption_tracking_state(
                                            &trade_mut,
                                            "retrying",
                                            None,
                                            None,
                                            Some(single_error_text.as_str()),
                                        );
                                    }
                                }
                                Err(single_err) => {
                                    let single_error_text = single_err.to_string();
                                    if Self::is_relayer_quota_error(&single_error_text) {
                                        let reset_seconds = Self::sanitize_redemption_pause_seconds(
                                            Self::parse_relayer_reset_seconds(&single_error_text)
                                                .unwrap_or(600),
                                        );
                                        self.set_redemption_retry_pause(reset_seconds);
                                        pause_remaining_snapshot = Some(reset_seconds);
                                        Self::set_condition_retry_after_epoch(
                                            trade_mut.condition_id.as_str(),
                                            current_timestamp.saturating_add(reset_seconds),
                                        );
                                        self.update_redemption_tracking_state(
                                            &trade_mut,
                                            "paused",
                                            None,
                                            None,
                                            Some(
                                                format!(
                                                    "relayer_quota_pause={}s error={}",
                                                    reset_seconds, single_error_text
                                                )
                                                .as_str(),
                                            ),
                                        );
                                        continue;
                                    }

                                    Self::set_condition_retry_after_epoch(
                                        trade_mut.condition_id.as_str(),
                                        current_timestamp.saturating_add(condition_retry_seconds),
                                    );
                                    if trade_mut.redemption_attempts
                                        >= candidate.max_redemption_attempts
                                    {
                                        trade_mut.redemption_abandoned = true;
                                        let market_name = trade_mut.token_type.display_name();
                                        let redeem_event = format!(
                                            "REDEMPTION FAILED | Market: {} | Period: {} | Attempts: {} | Status: ABANDONED | Error: {}",
                                            market_name,
                                            trade_mut.market_timestamp,
                                            trade_mut.redemption_attempts,
                                            single_error_text.chars().take(100).collect::<String>()
                                        );
                                        crate::log_trading_event(&redeem_event);
                                        let mut pending = self.pending_trades.lock().await;
                                        if let Some(t) = pending.get_mut(candidate.key.as_str()) {
                                            *t = trade_mut.clone();
                                        }
                                        drop(pending);
                                        self.update_redemption_tracking_state(
                                            &trade_mut,
                                            "abandoned",
                                            None,
                                            None,
                                            Some(single_error_text.as_str()),
                                        );
                                    } else {
                                        let market_name = trade_mut.token_type.display_name();
                                        let redeem_event = format!(
                                            "REDEMPTION RETRY | Market: {} | Period: {} | Attempt: {}/{} | Status: RETRYING",
                                            market_name,
                                            trade_mut.market_timestamp,
                                            trade_mut.redemption_attempts,
                                            candidate.max_redemption_attempts
                                        );
                                        crate::log_trading_event(&redeem_event);
                                        let mut pending = self.pending_trades.lock().await;
                                        if let Some(t) = pending.get_mut(candidate.key.as_str()) {
                                            *t = trade_mut.clone();
                                        }
                                        drop(pending);
                                        self.update_redemption_tracking_state(
                                            &trade_mut,
                                            "retrying",
                                            None,
                                            None,
                                            Some(single_error_text.as_str()),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if skipped_batch_cap > 0 || skipped_condition_retry_window > 0 {
            crate::log_println!(
                "🧹 Redemption sweep queue status: submitted={} cap={} deferred_by_cap={} deferred_by_condition_retry={}",
                submitted_conditions,
                max_conditions_per_sweep,
                skipped_batch_cap,
                skipped_condition_retry_window
            );
        }

        let sweep_finished_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        Self::set_last_redemption_sweep_epoch(sweep_finished_ts);
        if successful_conditions > 0 {
            Self::set_last_redemption_success_epoch(sweep_finished_ts);
        }
        Self::update_redemption_sweep_status(
            sweep_started_ts,
            sweep_finished_ts,
            sweep_trigger_name,
            sweep_reason.as_str(),
            processed_conditions.len(),
            submitted_conditions,
            successful_conditions,
            skipped_duplicate_conditions,
            pause_remaining_snapshot,
        );
        log_event(
            "redemption_sweep_completed",
            json!({
                "trigger": sweep_trigger_name,
                "reason": sweep_reason,
                "started_ts": sweep_started_ts,
                "finished_ts": sweep_finished_ts,
                "processed_conditions": processed_conditions.len(),
                "submitted_conditions": submitted_conditions,
                "successful_conditions": successful_conditions,
                "skipped_duplicate_conditions": skipped_duplicate_conditions,
                "max_conditions_per_sweep": max_conditions_per_sweep,
                "skipped_batch_cap": skipped_batch_cap,
                "skipped_condition_retry_window": skipped_condition_retry_window,
                "pause_remaining_sec": pause_remaining_snapshot
            }),
        );

        if let Err(e) = self
            .run_merge_sweep_with_trigger(MergeSweepTrigger::Scheduler)
            .await
        {
            warn!("Merge sweep scheduler pass failed: {}", e);
        }

        // Dust-sell sweep is intentionally piggybacked on the same scheduled redemption slot
        // (default minute=22, interval=1h) to avoid adding an independent scheduler loop.
        if sweep_trigger_name == "scheduled_interval" {
            match self.dust_sell_mm_inventory(None, None, None, None).await {
                Ok(result) => {
                    log_event(
                        "mm_dust_sell_scheduled_completed",
                        json!({
                            "trigger": sweep_trigger_name,
                            "reason": sweep_reason,
                            "enabled": result.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false),
                            "candidates": result.get("candidates").and_then(|v| v.as_u64()).unwrap_or(0),
                            "candidates_capped": result.get("candidates_capped").and_then(|v| v.as_u64()).unwrap_or(0),
                            "attempted_orders": result.get("attempted_orders").and_then(|v| v.as_u64()).unwrap_or(0),
                            "submitted": result.get("submitted").and_then(|v| v.as_u64()).unwrap_or(0),
                            "failed": result.get("failed").and_then(|v| v.as_u64()).unwrap_or(0),
                            "skipped": result.get("skipped").and_then(|v| v.as_u64()).unwrap_or(0),
                            "reason_counts": result.get("reason_counts").cloned().unwrap_or_else(|| json!({}))
                        }),
                    );
                }
                Err(e) => {
                    warn!("Scheduled dust-sell sweep failed: {}", e);
                    log_event(
                        "mm_dust_sell_scheduled_failed",
                        json!({
                            "trigger": sweep_trigger_name,
                            "reason": sweep_reason,
                            "error": e.to_string()
                        }),
                    );
                }
            }
        }

        Ok(())
    }

    async fn run_merge_sweep_with_trigger(&self, trigger: MergeSweepTrigger) -> Result<()> {
        if self.simulation_mode {
            return Ok(());
        }

        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let manual_requested = if matches!(trigger, MergeSweepTrigger::ManualApi) {
            true
        } else {
            Self::manual_merge_request_pending()
        };

        let mut trigger_name = "scheduler";
        let mut reason = String::new();
        let mut should_run = false;
        if manual_requested {
            trigger_name = if matches!(trigger, MergeSweepTrigger::ManualApi) {
                "manual_api"
            } else {
                "manual_request"
            };
            reason = "manual_merge_request".to_string();
            should_run = true;
        } else if Self::merge_sweep_enabled() {
            if Self::merge_auto_trigger_enabled() {
                let cooldown_sec = Self::merge_auto_trigger_cooldown_sec();
                let cooldown_remaining = Self::auto_trigger_cooldown_remaining_sec(
                    Self::merge_last_auto_trigger_epoch_store(),
                    now_epoch,
                    cooldown_sec,
                );
                if cooldown_remaining == 0 {
                    let pending_threshold = Self::merge_auto_trigger_pending_threshold();
                    let pair_conditions = self
                        .tracking_db
                        .as_ref()
                        .and_then(|db| {
                            db.list_wallet_mergeable_inventory(
                                pending_threshold.saturating_mul(8).max(128).min(20_000),
                            )
                            .ok()
                        })
                        .map(|rows| Self::estimate_merge_pair_conditions(&rows))
                        .unwrap_or(0usize);
                    let mut auto_trigger_reason: Option<String> = None;
                    if pair_conditions >= pending_threshold {
                        auto_trigger_reason = Some(format!(
                            "merge_pair_conditions={}>=threshold={}",
                            pair_conditions, pending_threshold
                        ));
                    }
                    if auto_trigger_reason.is_none() {
                        let ratio_threshold = Self::merge_auto_trigger_available_ratio_threshold();
                        let pending_snapshot: Vec<(String, PendingTrade)> = {
                            let pending = self.pending_trades.lock().await;
                            pending
                                .iter()
                                .map(|(key, trade)| (key.clone(), trade.clone()))
                                .collect()
                        };
                        if let Some((available_ratio, usdc_balance, portfolio_estimate)) = self
                            .available_ratio_from_pending(
                                &pending_snapshot,
                                Self::redemption_auto_trigger_balance_timeout_ms(),
                            )
                            .await
                        {
                            if available_ratio < ratio_threshold {
                                auto_trigger_reason = Some(format!(
                                    "available_ratio={:.4}<threshold={:.4},usdc={:.2},portfolio_est={:.2}",
                                    available_ratio, ratio_threshold, usdc_balance, portfolio_estimate
                                ));
                            }
                        }
                    }
                    if let Some(auto_trigger_reason) = auto_trigger_reason {
                        if Self::try_claim_auto_trigger_slot(
                            Self::merge_last_auto_trigger_epoch_store(),
                            now_epoch,
                            cooldown_sec,
                        ) {
                            trigger_name = "auto_trigger";
                            reason = auto_trigger_reason;
                            should_run = true;
                        }
                    }
                }
            }

            if !should_run
                && Self::merge_schedule_enabled()
                && Self::try_claim_merge_scheduled_slot(now_epoch)
            {
                trigger_name = "scheduled_interval";
                reason = format!(
                    "minute_match={},interval_hours={}",
                    Self::merge_sweep_minute(),
                    Self::merge_sweep_interval_hours()
                );
                should_run = true;
            }
        }

        if !should_run {
            return Ok(());
        }
        if manual_requested {
            Self::take_manual_merge_request();
        }

        #[derive(Clone)]
        struct MergeCandidate {
            condition_id: String,
            up_token_id: String,
            down_token_id: String,
            up_balance_shares: f64,
            down_balance_shares: f64,
            pair_shares: f64,
        }

        let started_ts = now_epoch;
        let min_pairs = Self::merge_min_pairs_shares();
        let max_conditions = Self::merge_max_conditions_per_sweep();

        let pending_snapshot: Vec<PendingTrade> = {
            let pending = self.pending_trades.lock().await;
            pending.values().cloned().collect()
        };
        let mut condition_tokens: HashMap<String, HashSet<String>> = HashMap::new();
        for trade in pending_snapshot {
            if trade.sold || trade.redemption_abandoned || !trade.buy_order_confirmed {
                continue;
            }
            let token_id = trade.token_id.trim();
            if token_id.is_empty() {
                continue;
            }
            condition_tokens
                .entry(trade.condition_id.clone())
                .or_default()
                .insert(token_id.to_string());
        }
        if let Some(db) = self.tracking_db.as_ref() {
            if let Ok(wallet_merge_rows) = db.list_wallet_mergeable_inventory(20_000) {
                Self::seed_merge_condition_tokens_from_wallet_rows(
                    &mut condition_tokens,
                    wallet_merge_rows,
                );
            }
            if let Ok(wallet_rows) = db.list_mm_wallet_inventory_by_strategy(
                crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
                20_000,
            ) {
                Self::seed_merge_condition_tokens_from_wallet_rows(
                    &mut condition_tokens,
                    wallet_rows,
                );
            }
        }

        // Include explicitly configured MM single-market selectors so merge sweep
        // can operate on non-crypto binary markets even when token_type mapping
        // is not part of the crypto Up/Down enum set.
        for (event_slug, market_slug) in Self::merge_market_selectors_from_env() {
            let market_fetch = tokio::time::timeout(
                tokio::time::Duration::from_millis(2_000),
                self.api
                    .get_market_from_event_slug(event_slug.as_str(), market_slug.as_deref()),
            )
            .await;
            let Ok(Ok(market)) = market_fetch else {
                continue;
            };
            let token_ids = Self::merge_extract_market_token_ids(&market);
            if token_ids.len() < 2 {
                continue;
            }
            let entry = condition_tokens
                .entry(market.condition_id.clone())
                .or_default();
            for token_id in token_ids.into_iter().take(2) {
                entry.insert(token_id);
            }
        }

        let scanned_conditions = condition_tokens.len();
        let mut candidates: Vec<MergeCandidate> = Vec::new();
        for (condition_id, token_ids) in condition_tokens {
            if token_ids.len() < 2 {
                continue;
            }
            let mut sorted_ids: Vec<String> = token_ids.into_iter().collect();
            sorted_ids.sort_unstable();
            let Some(up_token_id) = sorted_ids.first().cloned() else {
                continue;
            };
            let Some(down_token_id) = sorted_ids.get(1).cloned() else {
                continue;
            };
            let (up_balance_res, down_balance_res) = tokio::join!(
                tokio::time::timeout(
                    tokio::time::Duration::from_millis(2_000),
                    self.api.check_balance_only(up_token_id.as_str()),
                ),
                tokio::time::timeout(
                    tokio::time::Duration::from_millis(2_000),
                    self.api.check_balance_only(down_token_id.as_str()),
                )
            );
            let up_balance = match up_balance_res {
                Ok(Ok(v)) => v,
                _ => continue,
            };
            let down_balance = match down_balance_res {
                Ok(Ok(v)) => v,
                _ => continue,
            };
            let up_balance_shares =
                f64::try_from(up_balance / rust_decimal::Decimal::from(1_000_000u64))
                    .unwrap_or(0.0);
            let down_balance_shares =
                f64::try_from(down_balance / rust_decimal::Decimal::from(1_000_000u64))
                    .unwrap_or(0.0);
            let pair_shares = up_balance_shares.min(down_balance_shares);
            if !pair_shares.is_finite() || pair_shares < min_pairs {
                continue;
            }
            candidates.push(MergeCandidate {
                condition_id,
                up_token_id,
                down_token_id,
                up_balance_shares,
                down_balance_shares,
                pair_shares,
            });
        }
        candidates.sort_by(|a, b| {
            b.pair_shares
                .partial_cmp(&a.pair_shares)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let merge_candidates = candidates.len();
        let selected_candidates: Vec<MergeCandidate> = if max_conditions > 0 {
            candidates.into_iter().take(max_conditions).collect()
        } else {
            candidates
        };
        let submitted_merges = selected_candidates.len();
        let mut successful_merges = 0usize;
        let chunk_size = Self::merge_batch_conditions_per_request().max(1);
        for chunk in selected_candidates.chunks(chunk_size) {
            let batch_requests: Vec<MergeConditionRequest> = chunk
                .iter()
                .map(|candidate| MergeConditionRequest {
                    condition_id: candidate.condition_id.clone(),
                    pair_shares: candidate.pair_shares,
                })
                .collect();

            let mut fallback_reason: Option<String> = None;
            match self.api.merge_complete_sets_batch(&batch_requests).await {
                Ok(resp) => {
                    let success = resp.success;
                    let resp_tx_hash = resp.transaction_hash.clone();
                    let resp_message = resp.message.clone();
                    if success {
                        successful_merges = successful_merges.saturating_add(chunk.len());
                        for candidate in chunk.iter() {
                            self.consume_mm_merge_inventory(
                                crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
                                "mm",
                                started_ts,
                                candidate.condition_id.as_str(),
                                candidate.up_token_id.as_str(),
                                candidate.down_token_id.as_str(),
                                candidate.pair_shares,
                                resp_tx_hash.as_deref(),
                                Some(reason.as_str()),
                                "merge_sweep",
                            );
                            self.record_merge_cashflow(
                                crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
                                "mm",
                                started_ts,
                                candidate.condition_id.as_str(),
                                candidate.pair_shares,
                                resp_tx_hash.as_deref(),
                                Some(reason.as_str()),
                                "merge_sweep",
                            );
                        }
                        let _ = self.sync_trades_with_portfolio().await;
                    } else {
                        fallback_reason = Some(format!(
                            "batch_success_false:{}",
                            resp_message.clone().unwrap_or_default()
                        ));
                    }
                    for candidate in chunk.iter() {
                        let entry = json!({
                            "ts_ms": chrono::Utc::now().timestamp_millis(),
                            "trigger": trigger_name,
                            "reason": reason,
                            "condition_id": candidate.condition_id,
                            "up_token_id": candidate.up_token_id,
                            "down_token_id": candidate.down_token_id,
                            "up_balance_shares": candidate.up_balance_shares,
                            "down_balance_shares": candidate.down_balance_shares,
                            "pair_shares": candidate.pair_shares,
                            "submitted": true,
                            "success": success,
                            "tx_hash": resp_tx_hash.clone(),
                            "message": resp_message.clone(),
                            "batch_submit": true,
                            "batch_size": chunk.len()
                        });
                        Self::push_merge_recent_log(entry.clone());
                        log_event("merge_sweep_submit", entry);
                    }
                }
                Err(e) => {
                    fallback_reason = Some(e.to_string());
                    let err_text = e.to_string();
                    for candidate in chunk.iter() {
                        let entry = json!({
                            "ts_ms": chrono::Utc::now().timestamp_millis(),
                            "trigger": trigger_name,
                            "reason": reason,
                            "condition_id": candidate.condition_id,
                            "up_token_id": candidate.up_token_id,
                            "down_token_id": candidate.down_token_id,
                            "up_balance_shares": candidate.up_balance_shares,
                            "down_balance_shares": candidate.down_balance_shares,
                            "pair_shares": candidate.pair_shares,
                            "submitted": true,
                            "success": false,
                            "error": err_text,
                            "batch_submit": true,
                            "batch_size": chunk.len()
                        });
                        Self::push_merge_recent_log(entry.clone());
                        log_event("merge_sweep_submit_failed", entry);
                    }
                }
            }

            if let Some(batch_err) = fallback_reason {
                warn!(
                    "Merge sweep batch submit failed; retrying per condition with immediate fallback (trigger={} reason={} chunk_size={} error={})",
                    trigger_name,
                    reason,
                    chunk.len(),
                    batch_err
                );
                let mut chunk_single_success = 0usize;
                for candidate in chunk.iter() {
                    match self
                        .api
                        .merge_complete_sets(candidate.condition_id.as_str(), candidate.pair_shares)
                        .await
                    {
                        Ok(resp) => {
                            if resp.success {
                                chunk_single_success = chunk_single_success.saturating_add(1);
                                successful_merges = successful_merges.saturating_add(1);
                                self.consume_mm_merge_inventory(
                                    crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
                                    "mm",
                                    started_ts,
                                    candidate.condition_id.as_str(),
                                    candidate.up_token_id.as_str(),
                                    candidate.down_token_id.as_str(),
                                    candidate.pair_shares,
                                    resp.transaction_hash.as_deref(),
                                    Some(reason.as_str()),
                                    "merge_sweep",
                                );
                                self.record_merge_cashflow(
                                    crate::strategy::STRATEGY_ID_MM_REWARDS_V1,
                                    "mm",
                                    started_ts,
                                    candidate.condition_id.as_str(),
                                    candidate.pair_shares,
                                    resp.transaction_hash.as_deref(),
                                    Some(reason.as_str()),
                                    "merge_sweep",
                                );
                            }
                            let entry = json!({
                                "ts_ms": chrono::Utc::now().timestamp_millis(),
                                "trigger": trigger_name,
                                "reason": reason,
                                "condition_id": candidate.condition_id,
                                "up_token_id": candidate.up_token_id,
                                "down_token_id": candidate.down_token_id,
                                "up_balance_shares": candidate.up_balance_shares,
                                "down_balance_shares": candidate.down_balance_shares,
                                "pair_shares": candidate.pair_shares,
                                "submitted": true,
                                "success": resp.success,
                                "tx_hash": resp.transaction_hash,
                                "message": resp.message,
                                "batch_submit": false,
                                "batch_size": 1,
                                "fallback_from_batch": true,
                                "batch_error": batch_err.as_str()
                            });
                            Self::push_merge_recent_log(entry.clone());
                            log_event("merge_sweep_submit", entry);
                        }
                        Err(e) => {
                            let entry = json!({
                                "ts_ms": chrono::Utc::now().timestamp_millis(),
                                "trigger": trigger_name,
                                "reason": reason,
                                "condition_id": candidate.condition_id,
                                "up_token_id": candidate.up_token_id,
                                "down_token_id": candidate.down_token_id,
                                "up_balance_shares": candidate.up_balance_shares,
                                "down_balance_shares": candidate.down_balance_shares,
                                "pair_shares": candidate.pair_shares,
                                "submitted": true,
                                "success": false,
                                "error": e.to_string(),
                                "batch_submit": false,
                                "batch_size": 1,
                                "fallback_from_batch": true,
                                "batch_error": batch_err.as_str()
                            });
                            Self::push_merge_recent_log(entry.clone());
                            log_event("merge_sweep_submit_failed", entry);
                        }
                    }
                }
                if chunk_single_success > 0 {
                    let _ = self.sync_trades_with_portfolio().await;
                }
            }
        }

        let finished_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(started_ts);
        Self::update_merge_sweep_status(
            started_ts,
            finished_ts,
            trigger_name,
            reason.as_str(),
            scanned_conditions,
            merge_candidates,
            submitted_merges,
            successful_merges,
        );
        log_event(
            "merge_sweep_completed",
            json!({
                "trigger": trigger_name,
                "reason": reason,
                "started_ts": started_ts,
                "finished_ts": finished_ts,
                "scanned_conditions": scanned_conditions,
                "merge_candidates": merge_candidates,
                "submitted_merges": submitted_merges,
                "successful_merges": successful_merges,
                "min_pairs_shares": min_pairs,
                "max_conditions_per_sweep": max_conditions
            }),
        );

        Ok(())
    }

    async fn check_market_result(
        &self,
        condition_id: &str,
        token_id: &str,
    ) -> Result<(bool, bool)> {
        let market = self.api.get_market(condition_id).await?;

        let winner_count = market.tokens.iter().filter(|t| t.winner).count();
        // `closed=true` can be observed before winner flags are finalized.
        // Treat market as settle-ready only when at least one winner flag exists.
        let is_resolved = market.closed && winner_count > 0;
        let is_winner = is_resolved
            && market
                .tokens
                .iter()
                .any(|t| t.token_id == token_id && t.winner);

        Ok((is_resolved, is_winner))
    }

    fn settlement_book_bid_threshold() -> f64 {
        std::env::var("EVPOLY_SETTLEMENT_BOOK_BID_THRESHOLD")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(0.99)
            .clamp(0.90, 0.9999)
    }

    fn settlement_book_inference_delay_sec() -> u64 {
        std::env::var("EVPOLY_SETTLEMENT_BOOK_INFERENCE_DELAY_SEC")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(30)
            .clamp(0, 900)
    }

    fn settlement_proxy_inference_delay_sec() -> u64 {
        std::env::var("EVPOLY_SETTLEMENT_PROXY_INFERENCE_DELAY_SEC")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(120)
            .clamp(0, 3_600)
    }

    fn token_symbol_for_type(token_type: &crate::detector::TokenType) -> &'static str {
        match token_type {
            crate::detector::TokenType::BtcUp | crate::detector::TokenType::BtcDown => "BTC",
            crate::detector::TokenType::EthUp | crate::detector::TokenType::EthDown => "ETH",
            crate::detector::TokenType::SolanaUp | crate::detector::TokenType::SolanaDown => "SOL",
            crate::detector::TokenType::XrpUp | crate::detector::TokenType::XrpDown => "XRP",
        }
    }

    fn token_winner_from_up_move(token_type: &crate::detector::TokenType, up_wins: bool) -> bool {
        match token_type {
            crate::detector::TokenType::BtcUp
            | crate::detector::TokenType::EthUp
            | crate::detector::TokenType::SolanaUp
            | crate::detector::TokenType::XrpUp => up_wins,
            crate::detector::TokenType::BtcDown
            | crate::detector::TokenType::EthDown
            | crate::detector::TokenType::SolanaDown
            | crate::detector::TokenType::XrpDown => !up_wins,
        }
    }

    async fn infer_token_winner_from_book_bids(
        &self,
        market: &MarketDetails,
        token_id: &str,
    ) -> Result<Option<bool>> {
        let threshold = Self::settlement_book_bid_threshold();
        let mut best: Option<(String, f64)> = None;

        for token in &market.tokens {
            let best_price = match self.api.get_best_price(token.token_id.as_str()).await {
                Ok(v) => v,
                Err(e) => {
                    debug!(
                        "Settlement bid inference: get_best_price failed token={} err={}",
                        token.token_id, e
                    );
                    continue;
                }
            };
            let bid = best_price
                .as_ref()
                .and_then(|p| p.bid)
                .and_then(|b| b.to_string().parse::<f64>().ok());
            let Some(bid_px) = bid else {
                continue;
            };
            if !bid_px.is_finite() || bid_px <= 0.0 {
                continue;
            }
            match best.as_mut() {
                Some((best_token, best_bid)) if bid_px > *best_bid => {
                    *best_token = token.token_id.clone();
                    *best_bid = bid_px;
                }
                None => {
                    best = Some((token.token_id.clone(), bid_px));
                }
                _ => {}
            }
        }

        let Some((winner_token_id, winner_bid)) = best else {
            return Ok(None);
        };
        if winner_bid < threshold {
            return Ok(None);
        }
        Ok(Some(winner_token_id == token_id))
    }

    async fn check_market_result_with_fallback(
        &self,
        trade: &PendingTrade,
    ) -> Result<(bool, bool, &'static str)> {
        let market = self.api.get_market(trade.condition_id.as_str()).await?;
        let winner_count = market.tokens.iter().filter(|t| t.winner).count();
        let api_resolved = market.closed && winner_count > 0;
        if api_resolved {
            let is_winner = market
                .tokens
                .iter()
                .any(|t| t.token_id == trade.token_id && t.winner);
            return Ok((true, is_winner, "api_winner"));
        }

        let now_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let market_end_ts = Self::market_end_timestamp(trade);
        let elapsed_after_close = now_ts.saturating_sub(market_end_ts);

        if elapsed_after_close >= Self::settlement_book_inference_delay_sec() {
            if let Some(is_winner) = self
                .infer_token_winner_from_book_bids(&market, trade.token_id.as_str())
                .await?
            {
                return Ok((true, is_winner, "book_bid_inference"));
            }
        }

        if elapsed_after_close >= Self::settlement_proxy_inference_delay_sec() {
            let symbol = Self::token_symbol_for_type(&trade.token_type);
            if let Some(up_wins) = self
                .api
                .infer_updown_outcome_from_proxy_candle(
                    symbol,
                    trade.source_timeframe.as_str(),
                    trade.market_timestamp,
                )
                .await?
            {
                let token_winner = Self::token_winner_from_up_move(&trade.token_type, up_wins);
                return Ok((true, token_winner, "proxy_candle_inference"));
            }
        }

        Ok((false, false, "unresolved"))
    }

    /// Redeem tokens using trade data directly (avoids lookup issues).
    async fn redeem_token_by_id_with_trade(&self, trade: &PendingTrade) -> Result<RedeemResponse> {
        // Determine outcome string based on token type
        // For Up/Down markets: Up = "Up", Down = "Down"
        let outcome = match trade.token_type {
            crate::detector::TokenType::BtcUp
            | crate::detector::TokenType::EthUp
            | crate::detector::TokenType::SolanaUp
            | crate::detector::TokenType::XrpUp => "Up",
            crate::detector::TokenType::BtcDown
            | crate::detector::TokenType::EthDown
            | crate::detector::TokenType::SolanaDown
            | crate::detector::TokenType::XrpDown => "Down",
        };

        crate::log_println!("═══════════════════════════════════════════════════════════");
        crate::log_println!("🔄 ATTEMPTING TOKEN REDEMPTION");
        crate::log_println!("═══════════════════════════════════════════════════════════");
        crate::log_println!("📊 Redemption Details:");
        crate::log_println!("   Token Type: {}", trade.token_type.display_name());
        crate::log_println!("   Token ID: {}...", &trade.token_id[..16]);
        crate::log_println!("   Condition ID: {}...", &trade.condition_id[..16]);
        crate::log_println!("   Outcome: {}", outcome);
        crate::log_println!("   Units to redeem: {:.6}", trade.units);
        crate::log_println!("   Purchase price: ${:.6}", trade.purchase_price);
        crate::log_println!("");
        crate::log_println!("   🔄 Calling Polymarket API to redeem tokens...");

        // Call the API to redeem tokens
        match self
            .api
            .redeem_tokens(&trade.condition_id, &trade.token_id, outcome)
            .await
        {
            Ok(resp) => {
                if resp.success {
                    crate::log_println!("   ✅ Redemption API call successful");
                } else {
                    let msg = resp
                        .message
                        .as_deref()
                        .unwrap_or("Redemption submitted but not confirmed yet");
                    crate::log_println!("   ⚠️  Redemption not confirmed yet: {}", msg);
                }
                Ok(resp)
            }
            Err(e) => {
                let err_text = e.to_string();
                if Self::is_relayer_quota_error(&err_text) {
                    crate::log_println!(
                        "   ⏸️ Redemption API rate-limited (quota/backoff path): {}",
                        err_text
                    );
                } else {
                    crate::log_println!("   ❌ Redemption API call failed: {}", err_text);
                }
                Err(e)
            }
        }
    }

    /// Legacy method - kept for compatibility but uses trade lookup
    #[allow(dead_code)]
    async fn redeem_token_by_id(&self, token_id: &str, _units: f64) -> Result<()> {
        // Get the condition ID and outcome from the token
        // We need to find which trade this token belongs to
        let trade = {
            let pending = self.pending_trades.lock().await;
            pending
                .values()
                .find(|t| t.token_id == token_id)
                .ok_or_else(|| anyhow::anyhow!("Trade not found for token {}", &token_id[..16]))?
                .clone()
        };

        self.redeem_token_by_id_with_trade(&trade).await.map(|_| ())
    }

    /// Reset for new period.
    /// Keeps unresolved confirmed positions for market-close handling, and prunes stale leftovers.
    pub async fn reset_period(&self, current_period: u64) {
        let mut pending = self.pending_trades.lock().await;
        pending.retain(|_, trade| {
            if trade.market_timestamp >= current_period {
                return true;
            }
            // Keep unresolved filled positions for redemption flow.
            if trade.buy_order_confirmed && !trade.sold && !trade.redemption_abandoned {
                return true;
            }
            false
        });
        drop(pending);
    }

    /// Print summary of all trades (for testing/verification)
    pub async fn print_trade_summary(&self) {
        // In simulation mode, print simulation position summary
        if self.simulation_mode {
            if let Some(tracker) = &self.simulation_tracker {
                // Get list of token IDs from positions and pending trades
                let mut token_ids: Vec<String> = tracker.get_position_token_ids().await;

                // Also add token IDs from pending trades that might have limit orders
                {
                    let pending = self.pending_trades.lock().await;
                    for trade in pending.values() {
                        if !token_ids.contains(&trade.token_id) {
                            token_ids.push(trade.token_id.clone());
                        }
                    }
                }

                // Get current prices for all positions
                let mut current_prices: HashMap<String, TokenPrice> = HashMap::new();
                for token_id in &token_ids {
                    if !current_prices.contains_key(token_id) {
                        if let Ok(Some(best)) = self.api.get_best_price(token_id).await {
                            let bid = best.bid;
                            let ask = best.ask;
                            let token_price = TokenPrice {
                                token_id: token_id.clone(),
                                bid,
                                ask,
                            };
                            current_prices.insert(token_id.clone(), token_price);
                        }
                    }
                }

                let summary = tracker.get_position_summary(&current_prices).await;
                tracker.log_position_summary(&current_prices).await;
                crate::log_to_history(&summary);
            }
            return;
        }

        let (n, profit) = self.reporting_totals().await;

        // Copy needed pending data under lock, then release to minimize hold time.
        let (pending_count, pending_list): (usize, Vec<(String, crate::models::PendingTrade)>) = {
            let pending = self.pending_trades.lock().await;
            let pc = pending.values().filter(|t| !t.sold).count();
            let list = pending
                .iter()
                .filter(|(_, t)| !t.sold)
                .map(|(k, t)| (k.clone(), t.clone()))
                .collect();
            (pc, list)
        };

        let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
        let p = format!("[{}] ", ts);

        // Build entire block in one buffer, single write to stderr + history file
        let mut out = String::with_capacity(1024);
        out.push_str(&format!(
            "{}═══════════════════════════════════════════════════════════\n",
            p
        ));
        out.push_str(&format!("{}📊 TRADE SUMMARY\n", p));
        out.push_str(&format!(
            "{}═══════════════════════════════════════════════════════════\n",
            p
        ));
        out.push_str(&format!("{}Total Trades Executed: {}\n", p, n));
        out.push_str(&format!("{}Total Profit: ${:.6}\n", p, profit));
        out.push_str(&format!("{}Pending Trades: {}\n", p, pending_count));
        out.push_str(&format!("{} \n", p));

        if pending_count == 0 {
            out.push_str(&format!("{}No pending trades.\n", p));
        } else {
            for (key, trade) in &pending_list {
                out.push_str(&format!("{}Trade #{}:\n", p, key));
                out.push_str(&format!(
                    "{}   Token Type: {}\n",
                    p,
                    trade.token_type.display_name()
                ));
                out.push_str(&format!("{}   Token ID: {}\n", p, trade.token_id));
                out.push_str(&format!("{}   Condition ID: {}\n", p, trade.condition_id));
                out.push_str(&format!("{}   Units: {:.6}\n", p, trade.units));
                out.push_str(&format!(
                    "{}   Purchase Price: ${:.6}\n",
                    p, trade.purchase_price
                ));
                out.push_str(&format!(
                    "{}   Target Sell Price: ${:.6}\n",
                    p, trade.sell_price
                ));
                out.push_str(&format!(
                    "{}   Market Timestamp: {}\n",
                    p, trade.market_timestamp
                ));
                out.push_str(&format!("{}   Status: PENDING\n", p));
                out.push_str(&format!(
                    "{}   Investment: ${:.6}\n",
                    p, trade.investment_amount
                ));
                out.push_str(&format!("{} \n", p));
            }
        }
        out.push_str(&format!(
            "{}═══════════════════════════════════════════════════════════\n",
            p
        ));
        out.push_str(&format!("{} \n", p));

        crate::log_to_history(&out);
    }
}

#[cfg(test)]
mod tests {
    use super::{EntryExecutionMode, MarketOrderConstraints, Trader};
    use crate::detector::TokenType;
    use crate::models::PendingTrade;
    use crate::tracking_db::MmWalletInventoryRow;

    fn redemption_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("redemption test lock poisoned")
    }

    fn reset_redemption_sweep_test_state() {
        if let Ok(mut guard) = Trader::redemption_manual_request_store().lock() {
            *guard = false;
        }
        if let Ok(mut guard) = Trader::redemption_last_scheduled_hour_store().lock() {
            *guard = None;
        }
        if let Ok(mut guard) = Trader::redemption_last_success_epoch_store().lock() {
            *guard = None;
        }
        if let Ok(mut guard) = Trader::redemption_last_sweep_epoch_store().lock() {
            *guard = None;
        }
    }

    fn tp_candidate_trade(
        strategy_id: &str,
        timeframe: &str,
        market_timestamp: u64,
    ) -> PendingTrade {
        PendingTrade {
            token_id: "token-tp".to_string(),
            condition_id: "condition-tp".to_string(),
            token_type: TokenType::BtcUp,
            investment_amount: 100.0,
            units: 100.0,
            purchase_price: 0.25,
            sell_price: 0.99,
            timestamp: std::time::Instant::now(),
            market_timestamp,
            source_timeframe: timeframe.to_string(),
            strategy_id: strategy_id.to_string(),
            entry_mode: EntryExecutionMode::Ladder.as_str().to_string(),
            order_id: Some("order-tp".to_string()),
            end_timestamp: Some(market_timestamp + 900),
            sold: false,
            confirmed_balance: Some(100.0),
            buy_order_confirmed: true,
            limit_sell_orders_placed: false,
            no_sell: false,
            claim_on_closure: false,
            sell_attempts: 0,
            redemption_attempts: 0,
            redemption_abandoned: false,
        }
    }

    #[test]
    fn ladder_trade_key_keeps_rung_identity() {
        let key_a = Trader::limit_trade_key(
            EntryExecutionMode::Ladder,
            1_771_430_700,
            "token-a",
            0.31,
            "5m",
            "premarket_v1",
            None,
        );
        let key_b = Trader::limit_trade_key(
            EntryExecutionMode::Ladder,
            1_771_430_700,
            "token-a",
            0.33,
            "5m",
            "premarket_v1",
            None,
        );
        assert_ne!(key_a, key_b);
    }

    #[test]
    fn endgame_trade_key_includes_tick_from_request_id() {
        let key_tick0 = Trader::limit_trade_key(
            EntryExecutionMode::Endgame,
            1_771_430_700,
            "token-a",
            0.99,
            "5m",
            "endgame_sweep_v1",
            Some("endgame:5m:1771430700:token-a:tick0"),
        );
        let key_tick1 = Trader::limit_trade_key(
            EntryExecutionMode::Endgame,
            1_771_430_700,
            "token-a",
            0.99,
            "5m",
            "endgame_sweep_v1",
            Some("endgame:5m:1771430700:token-a:tick1"),
        );
        assert_ne!(key_tick0, key_tick1);
        assert!(key_tick0.ends_with("_tick0"));
        assert!(key_tick1.ends_with("_tick1"));
    }

    #[test]
    fn endgame_trade_key_without_tick_request_id_keeps_legacy_shape() {
        let key = Trader::limit_trade_key(
            EntryExecutionMode::Endgame,
            1_771_430_700,
            "token-a",
            0.99,
            "5m",
            "endgame_sweep_v1",
            Some("endgame:5m:1771430700:token-a"),
        );
        assert!(key.ends_with("_endgame_99_5m_endgame_sweep_v1"));
    }

    #[test]
    fn is_exchange_order_id_requires_non_empty_hex() {
        assert!(!Trader::is_exchange_order_id(""));
        assert!(!Trader::is_exchange_order_id("   "));
        assert!(!Trader::is_exchange_order_id("local:abc"));
        assert!(!Trader::is_exchange_order_id("restored:abc"));
        assert!(!Trader::is_exchange_order_id("abc123"));
        assert!(!Trader::is_exchange_order_id("0xnothex"));
        assert!(Trader::is_exchange_order_id("0xabc123"));
        assert!(Trader::is_exchange_order_id(
            "0x23b457271bce9fa09b4f79125c9ec09e968235a462de82e318ef4eb6fe0ffeb0"
        ));
    }

    #[test]
    fn terminal_status_maps_unknown_invalid_to_stale() {
        use polymarket_client_sdk::clob::types::OrderStatusType;

        assert_eq!(
            Trader::terminal_status_for_order(OrderStatusType::Unknown("INVALID".to_string())),
            Some("STALE")
        );
    }

    #[test]
    fn terminal_status_keeps_unknown_non_terminal_open() {
        use polymarket_client_sdk::clob::types::OrderStatusType;

        assert_eq!(
            Trader::terminal_status_for_order(OrderStatusType::Unknown("LIVE_PENDING".to_string())),
            None
        );
    }

    #[test]
    fn premarket_cancel_window_is_timeframe_configurable() {
        // SAFETY: unit test mutates process env in a single-threaded assertion scope.
        unsafe {
            std::env::set_var("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_5M_SEC", "11");
        }
        assert_eq!(Trader::premarket_cancel_after_launch_seconds("5m"), 11);
        assert_eq!(Trader::ladder_cancel_deadline_ts(1_000, "5m"), 1_011);
        // SAFETY: restoring the env variable after test assertion.
        unsafe {
            std::env::remove_var("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_5M_SEC");
        }
    }

    #[test]
    fn premarket_ladder_expiration_uses_timeframe_specific_defaults() {
        let now_ts = 1_000_000_u64;
        let period_ts = now_ts + 240;

        let exp_5m = Trader::compute_limit_order_expiration_ts(
            now_ts,
            period_ts,
            "5m",
            1_200,
            EntryExecutionMode::Ladder,
            crate::strategy::STRATEGY_ID_PREMARKET_V1,
            0.24,
        );
        let exp_5m_top_rung = Trader::compute_limit_order_expiration_ts(
            now_ts,
            period_ts,
            "5m",
            1_200,
            EntryExecutionMode::Ladder,
            crate::strategy::STRATEGY_ID_PREMARKET_V1,
            0.40,
        );
        let exp_15m = Trader::compute_limit_order_expiration_ts(
            now_ts,
            period_ts,
            "15m",
            1_200,
            EntryExecutionMode::Ladder,
            crate::strategy::STRATEGY_ID_PREMARKET_V1,
            0.24,
        );
        let exp_1h = Trader::compute_limit_order_expiration_ts(
            now_ts,
            period_ts,
            "1h",
            1_200,
            EntryExecutionMode::Ladder,
            crate::strategy::STRATEGY_ID_PREMARKET_V1,
            0.24,
        );
        let exp_4h = Trader::compute_limit_order_expiration_ts(
            now_ts,
            period_ts,
            "4h",
            1_200,
            EntryExecutionMode::Ladder,
            crate::strategy::STRATEGY_ID_PREMARKET_V1,
            0.24,
        );

        assert_eq!(exp_5m, now_ts + 245);
        assert_eq!(exp_5m_top_rung, now_ts + 240);
        assert_eq!(exp_15m, now_ts + 270);
        assert_eq!(exp_1h, now_ts + 300);
        assert_eq!(exp_4h, now_ts + 420);
    }

    #[test]
    fn premarket_ladder_expiration_honors_env_override() {
        // SAFETY: unit test mutates process env in a single-threaded assertion scope.
        unsafe {
            std::env::set_var("EVPOLY_PREMARKET_EXPIRE_4H_SEC", "650");
            std::env::set_var("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_4H_SEC", "1200");
        }
        let now_ts = 2_000_000_u64;
        let period_ts = now_ts + 120;
        let exp = Trader::compute_limit_order_expiration_ts(
            now_ts,
            period_ts,
            "4h",
            1_200,
            EntryExecutionMode::Ladder,
            crate::strategy::STRATEGY_ID_PREMARKET_V1,
            0.24,
        );
        assert_eq!(exp, now_ts + 650);
        // SAFETY: restoring mutated env variable after assertion.
        unsafe {
            std::env::remove_var("EVPOLY_PREMARKET_EXPIRE_4H_SEC");
            std::env::remove_var("EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_4H_SEC");
        }
    }

    #[test]
    fn premarket_tp_scope_and_due_gate_match_requirements() {
        let market_timestamp = 1_771_500_000_u64;
        let due_ms = i64::try_from(market_timestamp).unwrap_or(0) * 1000 + 300_000;

        let trade_15m = tp_candidate_trade(
            crate::strategy::STRATEGY_ID_PREMARKET_V1,
            "15m",
            market_timestamp,
        );
        let trade_1h = tp_candidate_trade(
            crate::strategy::STRATEGY_ID_PREMARKET_V1,
            "1h",
            market_timestamp,
        );
        let trade_4h = tp_candidate_trade(
            crate::strategy::STRATEGY_ID_PREMARKET_V1,
            "4h",
            market_timestamp,
        );
        let trade_5m = tp_candidate_trade(
            crate::strategy::STRATEGY_ID_PREMARKET_V1,
            "5m",
            market_timestamp,
        );
        let trade_other_strategy = tp_candidate_trade("endgame_sweep_v1", "15m", market_timestamp);

        assert!(!Trader::premarket_tp_is_trade_eligible(
            &trade_15m,
            due_ms - 1
        ));
        assert!(Trader::premarket_tp_is_trade_eligible(&trade_15m, due_ms));
        assert!(Trader::premarket_tp_is_trade_eligible(&trade_1h, due_ms));
        assert!(Trader::premarket_tp_is_trade_eligible(&trade_4h, due_ms));
        assert!(!Trader::premarket_tp_is_trade_eligible(&trade_5m, due_ms));
        assert!(!Trader::premarket_tp_is_trade_eligible(
            &trade_other_strategy,
            due_ms
        ));
    }

    #[test]
    fn premarket_tp_price_rule_examples_match_spec() {
        let case_floor = Trader::premarket_tp_price_target(0.25, Some(0.55));
        let case_top_ask = Trader::premarket_tp_price_target(0.25, Some(0.70));
        let case_double = Trader::premarket_tp_price_target(0.36, None);

        assert!((case_floor - 0.60).abs() < 1e-9);
        assert!((case_top_ask - 0.70).abs() < 1e-9);
        assert!((case_double - 0.72).abs() < 1e-9);
    }

    #[test]
    fn exit_event_key_includes_strategy_and_timeframe() {
        let key_premarket = Trader::build_exit_event_key(
            "premarket_v1",
            "5m",
            1_771_560_900,
            "token-a",
            "EXIT",
            Some("market_close"),
        );
        let key_endgame = Trader::build_exit_event_key(
            "endgame_sweep_v1",
            "5m",
            1_771_560_900,
            "token-a",
            "EXIT",
            Some("market_close"),
        );
        let key_premarket_tf = Trader::build_exit_event_key(
            "premarket_v1",
            "15m",
            1_771_560_900,
            "token-a",
            "EXIT",
            Some("market_close"),
        );
        assert_ne!(key_premarket, key_endgame);
        assert_ne!(key_premarket, key_premarket_tf);
    }

    #[test]
    fn manual_redemption_request_is_queued_once_until_consumed() {
        let _guard = redemption_test_lock();
        reset_redemption_sweep_test_state();

        assert!(Trader::request_manual_redemption_sweep_internal());
        assert!(!Trader::request_manual_redemption_sweep_internal());
        assert!(Trader::take_manual_redemption_request());
        assert!(!Trader::take_manual_redemption_request());
    }

    #[test]
    fn scheduled_redemption_slot_runs_once_per_hour_at_target_minute() {
        let _guard = redemption_test_lock();
        reset_redemption_sweep_test_state();
        // SAFETY: unit test mutates process env in isolated test scope.
        unsafe {
            std::env::set_var("EVPOLY_REDEMPTION_SWEEP_INTERVAL_HOURS", "1");
        }

        let minute = Trader::redemption_sweep_minute() as u64;
        let hour0_slot = minute * 60;
        let wrong_minute_slot = ((minute + 1) % 60) * 60;
        let hour1_slot = 3600 + hour0_slot;

        assert!(!Trader::try_claim_scheduled_sweep_slot(wrong_minute_slot));
        assert!(Trader::try_claim_scheduled_sweep_slot(hour0_slot));
        assert!(!Trader::try_claim_scheduled_sweep_slot(hour0_slot + 15));
        assert!(Trader::try_claim_scheduled_sweep_slot(hour1_slot));

        // SAFETY: restore env after mutation.
        unsafe {
            std::env::remove_var("EVPOLY_REDEMPTION_SWEEP_INTERVAL_HOURS");
        }
    }

    #[test]
    fn scheduled_redemption_slot_respects_interval_hours() {
        let _guard = redemption_test_lock();
        reset_redemption_sweep_test_state();
        // SAFETY: unit test mutates process env in isolated test scope.
        unsafe {
            std::env::set_var("EVPOLY_REDEMPTION_SWEEP_INTERVAL_HOURS", "4");
        }

        let minute = Trader::redemption_sweep_minute() as u64;
        let hour0_slot = minute * 60;
        let hour1_slot = 3600 + hour0_slot;
        let hour4_slot = (4 * 3600) + hour0_slot;

        assert!(Trader::try_claim_scheduled_sweep_slot(hour0_slot));
        assert!(!Trader::try_claim_scheduled_sweep_slot(hour1_slot));
        assert!(Trader::try_claim_scheduled_sweep_slot(hour4_slot));

        // SAFETY: restore env after mutation.
        unsafe {
            std::env::remove_var("EVPOLY_REDEMPTION_SWEEP_INTERVAL_HOURS");
        }
    }

    #[test]
    fn startup_catchup_respects_stale_threshold() {
        let _guard = redemption_test_lock();
        reset_redemption_sweep_test_state();

        let now = 100_000_u64;
        assert!(Trader::should_run_startup_catchup(now));

        let stale_seconds = Trader::redemption_startup_catchup_stale_minutes() * 60;
        Trader::set_last_redemption_success_epoch(now);
        assert!(!Trader::should_run_startup_catchup(
            now + stale_seconds.saturating_sub(1)
        ));
        assert!(Trader::should_run_startup_catchup(now + stale_seconds));
    }

    #[test]
    fn startup_catchup_uses_last_success_not_last_sweep() {
        let _guard = redemption_test_lock();
        reset_redemption_sweep_test_state();
        let now = 200_000_u64;
        let stale_seconds = Trader::redemption_startup_catchup_stale_minutes() * 60;

        Trader::set_last_redemption_sweep_epoch(now);
        assert!(Trader::should_run_startup_catchup(now + stale_seconds + 1));

        Trader::set_last_redemption_success_epoch(now);
        assert!(!Trader::should_run_startup_catchup(
            now + stale_seconds.saturating_sub(1)
        ));
    }

    #[test]
    fn parse_relayer_reset_seconds_handles_common_formats() {
        assert_eq!(
            Trader::parse_relayer_reset_seconds("quota exceeded; resets in 74266 seconds"),
            Some(74_266)
        );
        assert_eq!(
            Trader::parse_relayer_reset_seconds("rate limited, resets in 1h 2m 3s"),
            Some(3_723)
        );
        assert_eq!(
            Trader::parse_relayer_reset_seconds("rate limited, resets in 4m"),
            Some(240)
        );
    }

    #[test]
    fn sanitize_redemption_pause_seconds_clamps_bounds() {
        assert_eq!(Trader::sanitize_redemption_pause_seconds(0), 30);
        assert_eq!(Trader::sanitize_redemption_pause_seconds(10), 30);
        assert_eq!(Trader::sanitize_redemption_pause_seconds(90), 90);
        assert_eq!(Trader::sanitize_redemption_pause_seconds(90_000), 7_200);
    }

    #[test]
    fn constraint_refresh_classifier_catches_tick_and_size_failures() {
        assert!(Trader::should_refresh_constraints_for_error(
            "invalid_order_min_tick_size: price 0.123 not aligned to tick 0.001"
        ));
        assert!(Trader::should_refresh_constraints_for_error(
            "invalid_order_min_size: notional 1 below minimum_order_size 5"
        ));
        assert!(!Trader::should_refresh_constraints_for_error(
            "market_not_ready: closed market"
        ));
    }

    #[test]
    fn enforce_min_order_constraints_upsizes_for_min_notional() {
        let constraints = MarketOrderConstraints {
            min_notional_usd: 5.0,
            min_size_shares: 0.0,
            tick_size: 0.01,
        };
        let (units, notional, adjusted) =
            Trader::enforce_min_order_constraints(1.0, 0.40, Some(constraints)).unwrap();
        assert!(adjusted);
        assert_eq!(units, 12.5);
        assert!(notional >= 5.0);
    }

    #[test]
    fn enforce_min_order_constraints_upsizes_for_min_size() {
        let constraints = MarketOrderConstraints {
            min_notional_usd: 0.0,
            min_size_shares: 10.0,
            tick_size: 0.01,
        };
        let (units, notional, adjusted) =
            Trader::enforce_min_order_constraints(1.0, 0.50, Some(constraints)).unwrap();
        assert!(adjusted);
        assert_eq!(units, 10.0);
        assert_eq!(notional, 5.0);
    }

    #[test]
    fn enforce_min_order_constraints_rejects_non_positive_price() {
        let err = Trader::enforce_min_order_constraints(
            1.0,
            0.0,
            Some(MarketOrderConstraints {
                min_notional_usd: 0.0,
                min_size_shares: 0.0,
                tick_size: 0.01,
            }),
        )
        .unwrap_err();
        assert!(err.contains("invalid_order_min_tick_size"));
    }

    #[test]
    fn tick_decimal_places_scales_for_subcent_ticks() {
        assert_eq!(Trader::tick_decimal_places(0.01), 2);
        assert_eq!(Trader::tick_decimal_places(0.001), 3);
        assert_eq!(Trader::tick_decimal_places(0.0001), 4);
        assert_eq!(Trader::tick_decimal_places(1.0), 2);
    }

    #[test]
    fn merge_max_conditions_per_sweep_allows_zero_for_unlimited() {
        unsafe {
            std::env::set_var("EVPOLY_MERGE_MAX_CONDITIONS_PER_SWEEP", "0");
        }
        assert_eq!(Trader::merge_max_conditions_per_sweep(), 0);
        unsafe {
            std::env::remove_var("EVPOLY_MERGE_MAX_CONDITIONS_PER_SWEEP");
        }
    }

    #[test]
    fn merge_wallet_rows_seed_condition_tokens() {
        let mut seeded = std::collections::HashMap::new();
        Trader::seed_merge_condition_tokens_from_wallet_rows(
            &mut seeded,
            vec![
                MmWalletInventoryRow {
                    condition_id: "cond-a".to_string(),
                    token_id: "tok-up".to_string(),
                    market_slug: None,
                    timeframe: Some("1d".to_string()),
                    symbol: Some("MISC".to_string()),
                    mode: Some("spike".to_string()),
                    wallet_shares: 10.0,
                    wallet_avg_price: Some(0.5),
                    wallet_value_usd: 5.0,
                    snapshot_ts_ms: Some(1),
                },
                MmWalletInventoryRow {
                    condition_id: "cond-a".to_string(),
                    token_id: "tok-down".to_string(),
                    market_slug: None,
                    timeframe: Some("1d".to_string()),
                    symbol: Some("MISC".to_string()),
                    mode: Some("spike".to_string()),
                    wallet_shares: 12.0,
                    wallet_avg_price: Some(0.5),
                    wallet_value_usd: 6.0,
                    snapshot_ts_ms: Some(1),
                },
                MmWalletInventoryRow {
                    condition_id: "cond-b".to_string(),
                    token_id: "tok-zero".to_string(),
                    market_slug: None,
                    timeframe: Some("1d".to_string()),
                    symbol: Some("MISC".to_string()),
                    mode: Some("spike".to_string()),
                    wallet_shares: 0.0,
                    wallet_avg_price: None,
                    wallet_value_usd: 0.0,
                    snapshot_ts_ms: Some(1),
                },
            ],
        );
        let cond_a = seeded.get("cond-a").expect("cond-a missing");
        assert_eq!(cond_a.len(), 2);
        assert!(cond_a.contains("tok-up"));
        assert!(cond_a.contains("tok-down"));
        assert!(!seeded.contains_key("cond-b"));
    }
}
