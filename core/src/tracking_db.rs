use anyhow::{anyhow, Result};
use log::warn;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::panic::Location;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::strategy::{StrategyAction, StrategyDecision, Timeframe};

#[cfg(test)]
const STRATEGY_ID_TEST_PRIMARY: &str = "test_primary_v1";
#[cfg(test)]
const STRATEGY_ID_TEST_SECONDARY: &str = "test_secondary_v1";
#[cfg(test)]
const STRATEGY_ID_TEST_TERTIARY: &str = "test_tertiary_v1";

fn runtime_contention_warn_ms() -> i64 {
    static WARN_MS: OnceLock<i64> = OnceLock::new();
    *WARN_MS.get_or_init(|| {
        std::env::var("EVPOLY_RUNTIME_CONTENTION_WARN_MS")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(250)
            .max(1)
    })
}

fn warn_db_mutex_contention(scope: &'static str, waited_ms: i64) {
    if waited_ms < runtime_contention_warn_ms() {
        return;
    }
    warn!(
        "tracking_db mutex contention scope={} waited_ms={} warn_threshold_ms={}",
        scope,
        waited_ms,
        runtime_contention_warn_ms()
    );
}

fn db_lock_profile_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("EVPOLY_DB_LOCK_PROFILE_ENABLE")
            .ok()
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    })
}

fn db_lock_hold_warn_ms() -> i64 {
    static WARN_MS: OnceLock<i64> = OnceLock::new();
    *WARN_MS.get_or_init(|| {
        std::env::var("EVPOLY_DB_HOLD_WARN_MS")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(250)
            .max(1)
    })
}

#[derive(Debug, Clone, Copy)]
pub struct DbLockContentionSnapshot {
    pub window_ms: i64,
    pub sample_count: usize,
    pub p50_wait_ms: i64,
    pub p95_wait_ms: i64,
    pub max_wait_ms: i64,
    pub throttle_trigger_p95_ms: i64,
    pub recommended_premarket_factor: f64,
}

fn db_lock_window_ms() -> i64 {
    static WINDOW_MS: OnceLock<i64> = OnceLock::new();
    *WINDOW_MS.get_or_init(|| {
        std::env::var("EVPOLY_DB_CONTENTION_WINDOW_MS")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(120_000)
            .clamp(10_000, 30 * 60 * 1_000)
    })
}

fn db_lock_sample_limit() -> usize {
    static SAMPLE_LIMIT: OnceLock<usize> = OnceLock::new();
    *SAMPLE_LIMIT.get_or_init(|| {
        std::env::var("EVPOLY_DB_CONTENTION_SAMPLE_LIMIT")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(8_192)
            .clamp(512, 200_000)
    })
}

fn db_contention_throttle_p95_ms() -> i64 {
    static P95_MS: OnceLock<i64> = OnceLock::new();
    *P95_MS.get_or_init(|| {
        std::env::var("EVPOLY_DB_CONTENTION_THROTTLE_P95_MS")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(2_000)
            .clamp(200, 120_000)
    })
}

fn db_contention_throttle_min_factor() -> f64 {
    static MIN_FACTOR: OnceLock<f64> = OnceLock::new();
    *MIN_FACTOR.get_or_init(|| {
        std::env::var("EVPOLY_DB_CONTENTION_THROTTLE_MIN_FACTOR")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(0.25)
            .clamp(0.05, 1.0)
    })
}

fn db_lock_samples_store() -> &'static Mutex<VecDeque<(i64, i64)>> {
    static STORE: OnceLock<Mutex<VecDeque<(i64, i64)>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn percentile_wait_ms(sorted: &[i64], pct: f64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let pos = ((sorted.len() - 1) as f64 * pct).round() as usize;
    sorted[pos.min(sorted.len().saturating_sub(1))]
}

fn record_db_lock_wait_sample(wait_ms: i64) {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let keep_window_ms = db_lock_window_ms().saturating_mul(3);
    let keep_after_ms = now_ms.saturating_sub(keep_window_ms);
    if let Ok(mut guard) = db_lock_samples_store().lock() {
        guard.push_back((now_ms, wait_ms.max(0)));
        while guard.len() > db_lock_sample_limit() {
            guard.pop_front();
        }
        while let Some((ts_ms, _)) = guard.front().copied() {
            if ts_ms >= keep_after_ms {
                break;
            }
            guard.pop_front();
        }
    }
}

pub fn db_lock_contention_snapshot() -> DbLockContentionSnapshot {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let window_ms = db_lock_window_ms();
    let keep_after_ms = now_ms.saturating_sub(window_ms);
    let mut waits: Vec<i64> = Vec::new();
    if let Ok(mut guard) = db_lock_samples_store().lock() {
        while let Some((ts_ms, _)) = guard.front().copied() {
            if ts_ms >= keep_after_ms {
                break;
            }
            guard.pop_front();
        }
        waits.reserve(guard.len());
        for (ts_ms, wait_ms) in guard.iter().copied() {
            if ts_ms >= keep_after_ms {
                waits.push(wait_ms.max(0));
            }
        }
    }
    waits.sort_unstable();
    let sample_count = waits.len();
    let p50_wait_ms = percentile_wait_ms(waits.as_slice(), 0.50);
    let p95_wait_ms = percentile_wait_ms(waits.as_slice(), 0.95);
    let max_wait_ms = waits.last().copied().unwrap_or(0);
    let trigger_ms = db_contention_throttle_p95_ms();
    let min_factor = db_contention_throttle_min_factor();
    let recommended_factor = if sample_count == 0 || p95_wait_ms <= trigger_ms {
        1.0
    } else {
        ((trigger_ms as f64) / (p95_wait_ms as f64)).clamp(min_factor, 1.0)
    };
    DbLockContentionSnapshot {
        window_ms,
        sample_count,
        p50_wait_ms,
        p95_wait_ms,
        max_wait_ms,
        throttle_trigger_p95_ms: trigger_ms,
        recommended_premarket_factor: recommended_factor,
    }
}

pub fn db_contention_premarket_throttle_factor() -> f64 {
    db_lock_contention_snapshot().recommended_premarket_factor
}

#[inline]
fn maybe_log_db_lock_profile(
    scope: &'static str,
    caller: &'static Location<'static>,
    wait_ms: i64,
    hold_ms: i64,
    rows_est: Option<usize>,
) {
    record_db_lock_wait_sample(wait_ms);
    let warn_threshold = runtime_contention_warn_ms().max(db_lock_hold_warn_ms());
    let above_warn = wait_ms >= runtime_contention_warn_ms() || hold_ms >= db_lock_hold_warn_ms();
    if above_warn {
        warn!(
            "tracking_db lock profile scope={} caller={} wait_ms={} hold_ms={} warn_wait_ms={} warn_hold_ms={} rows_est={}",
            scope,
            caller,
            wait_ms,
            hold_ms,
            runtime_contention_warn_ms(),
            db_lock_hold_warn_ms(),
            rows_est.unwrap_or(0)
        );
    }
    if db_lock_profile_enabled() || above_warn {
        crate::event_log::log_event(
            "db_lock_profile",
            json!({
                "scope": scope,
                "caller": caller.to_string(),
                "wait_ms": wait_ms,
                "hold_ms": hold_ms,
                "warn_wait_ms": runtime_contention_warn_ms(),
                "warn_hold_ms": db_lock_hold_warn_ms(),
                "rows_est": rows_est
            }),
        );
    }
    if wait_ms >= warn_threshold {
        warn_db_mutex_contention(scope, wait_ms);
    }
}

#[derive(Debug, Clone)]
pub struct TradeEventRecord {
    pub event_key: Option<String>,
    pub ts_ms: i64,
    pub period_timestamp: u64,
    pub timeframe: String,
    pub strategy_id: String,
    pub asset_symbol: Option<String>,
    pub condition_id: Option<String>,
    pub token_id: Option<String>,
    pub token_type: Option<String>,
    pub side: Option<String>,
    pub event_type: String,
    pub price: Option<f64>,
    pub units: Option<f64>,
    pub notional_usd: Option<f64>,
    pub pnl_usd: Option<f64>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TimeframePerformance {
    pub timeframe: String,
    pub trades: u64,
    pub wins: u64,
    pub losses: u64,
    pub net_pnl_usd: f64,
}

#[derive(Debug, Clone)]
pub struct SignalAccuracyRow {
    pub timeframe: String,
    pub signal: String,
    pub samples: u64,
    pub wins: u64,
    pub win_rate_pct: f64,
}

#[derive(Debug, Clone)]
pub struct SnapbackFeatureBinSummary {
    pub timeframe: String,
    pub feature: String,
    pub bucket: String,
    pub direction: String,
    pub samples: u64,
    pub wins: u64,
    pub win_rate_pct: f64,
    pub net_pnl_usd: f64,
    pub avg_pnl_usd: f64,
    pub median_pnl_usd: f64,
    pub drawdown_proxy_usd: f64,
}

#[derive(Debug, Clone)]
pub struct StrategyFeatureBinSummary {
    pub timeframe: String,
    pub feature: String,
    pub bucket: String,
    pub samples: u64,
    pub wins: u64,
    pub win_rate_pct: f64,
    pub net_pnl_usd: f64,
    pub avg_pnl_usd: f64,
    pub median_pnl_usd: f64,
    pub drawdown_proxy_usd: f64,
}

#[derive(Debug, Clone)]
struct StrategyFeatureSnapshotScopeMatch {
    market_key: Option<String>,
    asset_symbol: Option<String>,
    condition_id: Option<String>,
    token_id: String,
    impulse_id: String,
    decision_id: Option<String>,
    rung_id: Option<String>,
    slice_id: Option<String>,
    request_id: Option<String>,
    order_id: Option<String>,
    trade_key: Option<String>,
    position_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SettledPerformanceV2 {
    pub settled_trades: u64,
    pub wins: u64,
    pub losses: u64,
    pub win_rate_pct: f64,
    pub net_settled_pnl_usd: f64,
}

#[derive(Debug, Clone)]
pub struct OpenMtmSummaryV2 {
    pub open_positions: u64,
    pub net_units: f64,
    pub cost_basis_usd: f64,
    pub mark_value_usd: f64,
    pub unrealized_pnl_usd: f64,
}

#[derive(Debug, Clone)]
pub struct StrategyMtmSummaryV2 {
    pub strategy_id: String,
    pub settled_pnl_usd: f64,
    pub open_positions: u64,
    pub net_units: f64,
    pub cost_basis_usd: f64,
    pub mark_value_usd: f64,
    pub unrealized_pnl_usd: f64,
    pub mtm_pnl_usd: f64,
}

#[derive(Debug, Clone)]
pub struct WalletCashflowEventRecord {
    pub cashflow_key: String,
    pub ts_ms: i64,
    pub strategy_id: String,
    pub timeframe: String,
    pub period_timestamp: u64,
    pub condition_id: Option<String>,
    pub token_id: Option<String>,
    pub event_type: String,
    pub amount_usd: Option<f64>,
    pub units: Option<f64>,
    pub token_price: Option<f64>,
    pub tx_id: Option<String>,
    pub reason: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MmMarketStateUpsertRecord {
    pub strategy_id: String,
    pub condition_id: String,
    pub market_slug: Option<String>,
    pub timeframe: String,
    pub symbol: Option<String>,
    pub mode: Option<String>,
    pub period_timestamp: u64,
    pub state: String,
    pub reason: Option<String>,
    pub reward_rate_hint: Option<f64>,
    pub open_buy_orders: u64,
    pub open_sell_orders: u64,
    pub favor_inventory_shares: Option<f64>,
    pub weak_inventory_shares: Option<f64>,
    pub up_token_id: Option<String>,
    pub down_token_id: Option<String>,
    pub up_inventory_shares: Option<f64>,
    pub down_inventory_shares: Option<f64>,
    pub imbalance_shares: Option<f64>,
    pub imbalance_side: Option<String>,
    pub avg_entry_up: Option<f64>,
    pub avg_entry_down: Option<f64>,
    pub up_best_bid: Option<f64>,
    pub down_best_bid: Option<f64>,
    pub mtm_pnl_exit_usd: Option<f64>,
    pub time_in_imbalance_ms: Option<i64>,
    pub max_imbalance_shares: Option<f64>,
    pub last_rebalance_duration_ms: Option<i64>,
    pub rebalance_fill_rate: Option<f64>,
    pub exit_slippage_vs_bbo_bps: Option<f64>,
    pub market_mode_runtime: Option<String>,
    pub ladder_top_price: Option<f64>,
    pub ladder_bottom_price: Option<f64>,
    pub last_event_ms: i64,
}

#[derive(Debug, Clone)]
pub struct MmMarketStateRow {
    pub strategy_id: String,
    pub condition_id: String,
    pub market_slug: Option<String>,
    pub timeframe: String,
    pub symbol: Option<String>,
    pub mode: Option<String>,
    pub period_timestamp: u64,
    pub state: String,
    pub reason: Option<String>,
    pub reward_rate_hint: Option<f64>,
    pub open_buy_orders: u64,
    pub open_sell_orders: u64,
    pub favor_inventory_shares: Option<f64>,
    pub weak_inventory_shares: Option<f64>,
    pub up_token_id: Option<String>,
    pub down_token_id: Option<String>,
    pub up_inventory_shares: Option<f64>,
    pub down_inventory_shares: Option<f64>,
    pub imbalance_shares: Option<f64>,
    pub imbalance_side: Option<String>,
    pub avg_entry_up: Option<f64>,
    pub avg_entry_down: Option<f64>,
    pub up_best_bid: Option<f64>,
    pub down_best_bid: Option<f64>,
    pub mtm_pnl_exit_usd: Option<f64>,
    pub time_in_imbalance_ms: Option<i64>,
    pub max_imbalance_shares: Option<f64>,
    pub last_rebalance_duration_ms: Option<i64>,
    pub rebalance_fill_rate: Option<f64>,
    pub exit_slippage_vs_bbo_bps: Option<f64>,
    pub market_mode_runtime: Option<String>,
    pub ladder_top_price: Option<f64>,
    pub ladder_bottom_price: Option<f64>,
    pub last_event_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Default)]
pub struct MmTokenLiveStateRow {
    pub wallet_inventory_shares: Option<f64>,
    pub db_inventory_shares: Option<f64>,
    pub active_buy_orders: u64,
    pub active_sell_orders: u64,
}

#[derive(Debug, Clone)]
pub struct StrategyWalletChecksumRow {
    pub strategy_id: String,
    pub settled_positions: u64,
    pub settled_payout_usd: f64,
    pub settled_cost_basis_usd: f64,
    pub settled_pnl_usd: f64,
    pub redeemed_cash_usd: f64,
    pub redeem_events: u64,
    pub unresolved_redeem_usd: f64,
}

#[derive(Debug, Clone)]
pub struct WalletCashflowSummaryRow {
    pub event_type: String,
    pub events: u64,
    pub total_amount_usd: f64,
}

#[derive(Debug, Clone)]
pub struct MmWalletInventoryRow {
    pub condition_id: String,
    pub token_id: String,
    pub market_slug: Option<String>,
    pub timeframe: Option<String>,
    pub symbol: Option<String>,
    pub mode: Option<String>,
    pub wallet_shares: f64,
    pub wallet_avg_price: Option<f64>,
    pub wallet_value_usd: f64,
    pub snapshot_ts_ms: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct WalletSyncHealthSummary {
    pub latest_ts_ms: Option<i64>,
    pub latest_status: Option<String>,
    pub latest_error: Option<String>,
    pub recent_runs_checked: u64,
    pub recent_non_ok_runs: u64,
    pub consecutive_non_ok_runs: u64,
    pub recent_db_locked_errors: u64,
    pub recent_api_unavailable_errors: u64,
}

#[derive(Debug, Clone)]
pub struct InventoryConsumptionResult {
    pub consume_key: String,
    pub strategy_id: String,
    pub timeframe: String,
    pub period_timestamp: u64,
    pub condition_id: String,
    pub token_id: String,
    pub requested_units: f64,
    pub applied_units: f64,
    pub applied_cost_usd: f64,
    pub db_units_before: f64,
    pub db_units_after: f64,
    pub wallet_units_after: Option<f64>,
    pub source: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MmInventoryDriftRow {
    pub condition_id: String,
    pub token_id: String,
    pub market_slug: Option<String>,
    pub timeframe: Option<String>,
    pub symbol: Option<String>,
    pub mode: Option<String>,
    pub period_timestamp: Option<u64>,
    pub db_open_positions: u64,
    pub db_inventory_shares: f64,
    pub db_remaining_cost_usd: f64,
    pub wallet_inventory_shares: Option<f64>,
    pub wallet_avg_price: Option<f64>,
    pub wallet_cur_price: Option<f64>,
    pub wallet_current_value_usd: Option<f64>,
    pub wallet_best_bid: Option<f64>,
    pub wallet_mid_price: Option<f64>,
    pub wallet_inventory_value_bid_usd: Option<f64>,
    pub wallet_inventory_value_mid_usd: Option<f64>,
    pub wallet_unrealized_mid_pnl_usd: Option<f64>,
    pub wallet_snapshot_ts_ms: Option<i64>,
    pub mid_snapshot_ts_ms: Option<i64>,
    pub last_wallet_activity_ts_ms: Option<i64>,
    pub last_wallet_activity_type: Option<String>,
    pub last_inventory_consumed_units: Option<f64>,
    pub inventory_drift_shares: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct MmInventoryDriftSnapshot {
    pub wallet_address: Option<String>,
    pub wallet_positions_available: bool,
    pub wallet_activity_available: bool,
    pub wallet_mid_marks_available: bool,
    pub rows: Vec<MmInventoryDriftRow>,
}

#[derive(Debug, Clone)]
pub struct MmInventoryReconcileRow {
    pub consume_key: String,
    pub action: String,
    pub condition_id: String,
    pub token_id: String,
    pub market_slug: Option<String>,
    pub timeframe: Option<String>,
    pub symbol: Option<String>,
    pub mode: Option<String>,
    pub db_inventory_shares_before: f64,
    pub wallet_inventory_shares: f64,
    pub drift_shares_before: f64,
    pub requested_consume_shares: f64,
    pub applied_consume_shares: f64,
    pub requested_add_shares: f64,
    pub applied_add_shares: f64,
    pub db_inventory_shares_after: f64,
    pub applied_cost_usd: f64,
    pub last_wallet_activity_ts_ms: Option<i64>,
    pub last_wallet_activity_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MmMarketAttributionRow {
    pub condition_id: String,
    pub market_slug: Option<String>,
    pub timeframe: Option<String>,
    pub symbol: Option<String>,
    pub mode: Option<String>,
    pub settled_trades: u64,
    pub trade_realized_pnl_usd: f64,
    pub merge_events: u64,
    pub merge_cashflow_usd: f64,
    pub reward_events: u64,
    pub reward_cashflow_usd: f64,
    pub redemption_events: u64,
    pub redemption_cashflow_usd: f64,
    pub open_positions: u64,
    pub open_entry_units: f64,
    pub open_entry_notional_usd: f64,
    pub open_realized_pnl_usd: f64,
    pub db_inventory_shares: f64,
    pub wallet_inventory_shares: Option<f64>,
    pub inventory_drift_shares: Option<f64>,
    pub wallet_inventory_value_live_usd: Option<f64>,
    pub wallet_inventory_value_bid_usd: Option<f64>,
    pub wallet_inventory_value_mid_usd: Option<f64>,
    pub last_wallet_activity_ts_ms: Option<i64>,
    pub last_wallet_activity_type: Option<String>,
    pub net_pnl_usd: f64,
}

#[derive(Debug, Clone, Default)]
pub struct MmMarketAttributionTotals {
    pub markets: u64,
    pub settled_trades: u64,
    pub trade_realized_pnl_usd: f64,
    pub merge_events: u64,
    pub merge_cashflow_usd: f64,
    pub reward_events: u64,
    pub reward_cashflow_usd: f64,
    pub redemption_events: u64,
    pub redemption_cashflow_usd: f64,
    pub open_positions: u64,
    pub open_entry_notional_usd: f64,
    pub db_inventory_shares: f64,
    pub wallet_inventory_shares: Option<f64>,
    pub inventory_drift_shares: Option<f64>,
    pub wallet_inventory_value_live_usd: Option<f64>,
    pub wallet_inventory_value_bid_usd: Option<f64>,
    pub wallet_inventory_value_mid_usd: Option<f64>,
    pub net_pnl_usd: f64,
}

#[derive(Debug, Clone)]
pub struct ReportingScope {
    pub from_ts_ms: Option<i64>,
    pub to_ts_ms: Option<i64>,
    pub include_legacy_default: bool,
    pub prod_only: bool,
}

impl ReportingScope {
    pub fn from_env() -> Self {
        let from_ts_ms = std::env::var("EVPOLY_REPORTING_CUTOVER_TS_MS")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .filter(|v| *v > 0);
        let include_legacy_default = env_bool("EVPOLY_REPORTING_INCLUDE_LEGACY_DEFAULT", false);
        let prod_only = env_bool("EVPOLY_REPORTING_PROD_ONLY", true);
        Self {
            from_ts_ms,
            to_ts_ms: None,
            include_legacy_default,
            prod_only,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CanonicalFillSummary {
    pub strategy_id: String,
    pub fills: u64,
    pub notional_usd: f64,
}

#[derive(Debug, Clone)]
pub struct StrategyActivitySummary {
    pub strategy_id: String,
    pub entry_submits: u64,
    pub entry_acks: u64,
    pub entry_fills: u64,
    pub entry_fill_notional_usd: f64,
    pub open_pending_orders: u64,
    pub open_pending_notional_usd: f64,
}

#[derive(Debug, Clone, Default)]
pub struct UiDashboardDbSummary {
    pub total_trades: u64,
    pub total_pnl: f64,
    pub winning_trades: u64,
    pub losing_trades: u64,
    pub avg_ack_latency_ms: Option<f64>,
    pub ack_sample_count: u64,
    pub open_positions_count: u64,
    pub recent_orders_count: u64,
    pub last_event_ts_ms: Option<i64>,
    pub last_event_type: Option<String>,
    pub last_event_strategy_id: Option<String>,
    pub last_event_pnl_usd: Option<f64>,
    pub last_exit_ts_ms: Option<i64>,
    pub last_exit_strategy_id: Option<String>,
    pub last_exit_pnl_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct LatestStrategyEvent {
    pub strategy_id: String,
    pub ts_ms: i64,
    pub event_type: String,
    pub pnl_usd: Option<f64>,
    pub condition_id: Option<String>,
    pub token_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PendingFilledSideSummary {
    pub buy_fills: u64,
    pub sell_fills: u64,
    pub buy_notional_usd: f64,
    pub sell_notional_usd: f64,
}

#[derive(Debug, Clone)]
pub struct ParallelOverlapRow {
    pub period_timestamp: u64,
    pub token_id: String,
    pub side: String,
    pub strategy_count: u64,
    pub submit_count: u64,
}

#[derive(Debug, Clone)]
pub struct CanonicalTrackingSummary {
    pub strategy_id: String,
    pub timeframe: String,
    pub entry_submits: u64,
    pub entry_acks: u64,
    pub entry_fills: u64,
    pub entry_fill_notional_usd: f64,
}

#[derive(Debug, Clone)]
pub struct RolloutStrategyGateSummary {
    pub strategy_id: String,
    pub entry_submits: u64,
    pub entry_failed: u64,
    pub entry_failed_ratio_pct: f64,
    pub submit_storm_groups: u64,
    pub liqaccum_direction_lock_rejects: u64,
    pub liqaccum_plan_rows: u64,
    pub liqaccum_direction_lock_reject_ratio_pct: f64,
}

#[derive(Debug, Clone)]
pub struct RolloutIntegrityGateSummary {
    pub duplicate_snapshot_rows: u64,
    pub invalid_transition_rejections: u64,
}

#[derive(Debug, Clone)]
pub struct ParameterSuggestionRecord {
    pub id: Option<i64>,
    pub strategy_id: String,
    pub name: String,
    pub current_value: f64,
    pub suggested_value: f64,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub confidence: f64,
    pub reason: String,
    pub source: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct RuntimeParameterStateRecord {
    pub strategy_id: String,
    pub name: String,
    pub value_json: String,
    pub version: i64,
    pub updated_at_ms: i64,
    pub updated_by: String,
}

#[derive(Debug, Clone)]
pub struct PendingOrderRecord {
    pub order_id: String,
    pub trade_key: String,
    pub token_id: String,
    pub condition_id: Option<String>,
    pub timeframe: String,
    pub strategy_id: String,
    pub asset_symbol: Option<String>,
    pub entry_mode: String,
    pub period_timestamp: u64,
    pub price: f64,
    pub size_usd: f64,
    pub side: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct LatestEntryFillRecord {
    pub period_timestamp: u64,
    pub timeframe: String,
    pub strategy_id: String,
    pub asset_symbol: Option<String>,
    pub condition_id: String,
    pub token_id: String,
    pub token_type: Option<String>,
    pub price: Option<f64>,
    pub notional_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct OpenPositionInventoryRow {
    pub strategy_id: String,
    pub condition_id: String,
    pub token_id: String,
    pub timeframe: String,
    pub period_timestamp: u64,
    pub entry_units: f64,
    pub exit_units: f64,
    pub net_units: f64,
}

#[derive(Debug, Clone)]
pub struct EntryFillEventRecord {
    pub event_key: String,
    pub ts_ms: i64,
    pub period_timestamp: u64,
    pub timeframe: String,
    pub strategy_id: String,
    pub condition_id: String,
    pub token_id: String,
    pub side: String,
    pub price: Option<f64>,
    pub notional_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct StrategyFillEventRecord {
    pub event_key: String,
    pub ts_ms: i64,
    pub strategy_id: String,
    pub condition_id: String,
    pub token_id: String,
    pub side: String,
    pub event_type: String,
}

#[derive(Debug, Clone)]
pub struct SnapbackFeatureSnapshotIntentRecord {
    pub strategy_id: String,
    pub timeframe: String,
    pub period_timestamp: u64,
    pub condition_id: String,
    pub token_id: String,
    pub impulse_id: String,
    pub position_key: Option<String>,
    pub entry_side: Option<String>,
    pub intent_ts_ms: i64,
    pub request_id: Option<String>,
    pub order_id: Option<String>,
    pub signed_notional_30s: Option<f64>,
    pub signed_notional_300s: Option<f64>,
    pub signed_notional_900s: Option<f64>,
    pub burst_zscore: Option<f64>,
    pub side_imbalance_5m: Option<f64>,
    pub impulse_strength: f64,
    pub impulse_mode: Option<String>,
    pub impulse_size_multiplier: Option<f64>,
    pub direction_policy_split: Option<f64>,
    pub trigger_signed_flow: Option<f64>,
    pub trigger_flow_source: Option<String>,
    pub fair_probability: Option<f64>,
    pub reservation_price: Option<f64>,
    pub max_price: Option<f64>,
    pub limit_price: Option<f64>,
    pub edge_bps: Option<f64>,
    pub target_size_usd: Option<f64>,
    pub bias_alignment: Option<String>,
    pub bias_confidence: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct SnapbackFeatureSnapshotKey {
    pub strategy_id: String,
    pub timeframe: String,
    pub period_timestamp: u64,
    pub token_id: String,
    pub impulse_id: String,
}

#[derive(Debug, Clone, Default)]
pub struct SnapbackFeatureSnapshotOutcomePatch {
    pub condition_id: Option<String>,
    pub position_key: Option<String>,
    pub request_id: Option<String>,
    pub order_id: Option<String>,
    pub fill_ts_ms: Option<i64>,
    pub exit_ts_ms: Option<i64>,
    pub settled_ts_ms: Option<i64>,
    pub exit_reason: Option<String>,
    pub realized_pnl_usd: Option<f64>,
    pub settled_pnl_usd: Option<f64>,
    pub hold_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct StrategyFeatureSnapshotIntentRecord {
    pub strategy_id: String,
    pub timeframe: String,
    pub period_timestamp: u64,
    pub market_key: Option<String>,
    pub asset_symbol: Option<String>,
    pub condition_id: Option<String>,
    pub token_id: String,
    pub impulse_id: String,
    pub decision_id: Option<String>,
    pub rung_id: Option<String>,
    pub slice_id: Option<String>,
    pub request_id: Option<String>,
    pub order_id: Option<String>,
    pub trade_key: Option<String>,
    pub position_key: Option<String>,
    pub intent_status: Option<String>,
    pub execution_status: Option<String>,
    pub intent_ts_ms: i64,
    pub submit_ts_ms: Option<i64>,
    pub ack_ts_ms: Option<i64>,
    pub fill_ts_ms: Option<i64>,
    pub exit_ts_ms: Option<i64>,
    pub settled_ts_ms: Option<i64>,
    pub entry_notional_usd: Option<f64>,
    pub exit_notional_usd: Option<f64>,
    pub realized_pnl_usd: Option<f64>,
    pub settled_pnl_usd: Option<f64>,
    pub hold_ms: Option<i64>,
    pub feature_json: Option<String>,
    pub decision_context_json: Option<String>,
    pub execution_context_json: Option<String>,
}

pub struct TrackingDb {
    conn: Mutex<Connection>,
    read_conns: Vec<Mutex<Connection>>,
    read_rr: AtomicUsize,
}

const LEGACY_STRATEGY_ID: &str = "legacy_default";
const DEFAULT_MARKET_KEY: &str = "btc";
const DEFAULT_ASSET_SYMBOL: &str = "BTC";
const DEFAULT_RUN_ID: &str = "legacy_run";
const DEFAULT_STRATEGY_COHORT: &str = "default";
const SNAPSHOT_SCOPE_SENTINEL: &str = "~";
const STRATEGY_ID_SCHEMA_VERSION: i64 = 2;
const OUTCOME_SCOPE_SCHEMA_VERSION: i64 = 3;
const MARKET_DIMENSION_SCHEMA_VERSION: i64 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeSource {
    Settlement,
    Exit,
    Hybrid,
}

impl OutcomeSource {
    pub fn from_raw(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "exit" => Self::Exit,
            "hybrid" => Self::Hybrid,
            _ => Self::Settlement,
        }
    }

    pub fn from_env() -> Self {
        Self::from_raw(
            std::env::var("EVPOLY_REPORTING_OUTCOME_SOURCE")
                .unwrap_or_else(|_| "settlement".to_string())
                .as_str(),
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Settlement => "settlement",
            Self::Exit => "exit",
            Self::Hybrid => "hybrid",
        }
    }
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let pragma = format!("PRAGMA table_info({})", table);
    let mut stmt = conn.prepare(pragma.as_str())?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let existing: String = row.get(1)?;
        if existing.eq_ignore_ascii_case(column) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        params![table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn ensure_column(conn: &Connection, table: &str, column: &str, definition: &str) -> Result<()> {
    if table_has_column(conn, table, column)? {
        return Ok(());
    }

    let alter = format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, definition);
    conn.execute(alter.as_str(), [])?;
    Ok(())
}

fn normalize_side(side: &str) -> String {
    side.trim().to_ascii_uppercase()
}

fn configured_wallet_address() -> Option<String> {
    ["POLY_PROXY_WALLET_ADDRESS", "EVPOLY_WALLET_SYNC_ADDRESS"]
        .iter()
        .find_map(|key| {
            std::env::var(key)
                .ok()
                .map(|v| v.trim().to_ascii_lowercase())
                .filter(|v| !v.is_empty())
        })
}

fn maybe_positive_f64(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite())
}

fn sum_option_f64(target: &mut Option<f64>, value: Option<f64>) {
    if let Some(v) = maybe_positive_f64(value) {
        *target = Some(target.unwrap_or(0.0) + v);
    }
}

fn normalize_pending_status(status: &str) -> String {
    status.trim().to_ascii_uppercase()
}

const ACTIVE_PENDING_ORDER_STATUSES_SQL: &str = "'OPEN','PENDING','PLACED','LIVE'";

fn normalize_entry_mode(mode: &str) -> String {
    let normalized = mode.trim().to_ascii_uppercase();
    if normalized.is_empty() {
        "UNKNOWN".to_string()
    } else {
        normalized
    }
}

fn normalize_strategy_id(strategy_id: &str) -> String {
    let normalized = strategy_id.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        LEGACY_STRATEGY_ID.to_string()
    } else {
        normalized
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|v| match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        })
        .unwrap_or(default)
}

fn normalize_market_key(market_key: Option<&str>, asset_symbol: Option<&str>) -> String {
    let normalized = market_key.unwrap_or("").trim().to_ascii_lowercase();
    if !normalized.is_empty() {
        return normalized;
    }
    let from_asset = asset_symbol.unwrap_or("").trim().to_ascii_lowercase();
    if !from_asset.is_empty() {
        from_asset
    } else {
        DEFAULT_MARKET_KEY.to_string()
    }
}

fn normalize_snapshot_status(value: Option<&str>, fallback: &str) -> String {
    let normalized = value.unwrap_or("").trim().to_ascii_lowercase();
    if normalized.is_empty() {
        fallback.to_string()
    } else {
        normalized
    }
}

fn normalize_snapshot_optional_scope(value: Option<&str>) -> String {
    let normalized = value.unwrap_or("").trim();
    if normalized.is_empty() {
        SNAPSHOT_SCOPE_SENTINEL.to_string()
    } else {
        normalized.to_ascii_lowercase()
    }
}

fn normalize_snapshot_optional_payload(value: Option<&str>) -> Option<String> {
    let normalized = value.unwrap_or("").trim();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized.to_string())
    }
}

fn is_terminal_execution_status(status: &str) -> bool {
    matches!(status, "canceled" | "expired" | "failed" | "settled")
}

fn is_valid_intent_status_transition(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    matches!(
        (from, to),
        ("planned", "submitted")
            | ("planned", "skipped")
            | ("planned", "failed_before_submit")
            | ("submitted", "skipped")
            | ("submitted", "failed_before_submit")
    )
}

fn is_valid_execution_status_transition(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    if is_terminal_execution_status(from) {
        return false;
    }
    matches!(
        (from, to),
        ("none", "ack")
            | ("none", "fill_partial")
            | ("none", "filled")
            | ("none", "exit")
            | ("none", "failed")
            | ("none", "canceled")
            | ("none", "expired")
            | ("ack", "fill_partial")
            | ("ack", "filled")
            | ("ack", "exit")
            | ("ack", "settled")
            | ("ack", "canceled")
            | ("ack", "expired")
            | ("ack", "failed")
            | ("fill_partial", "filled")
            | ("fill_partial", "canceled")
            | ("fill_partial", "expired")
            | ("fill_partial", "failed")
            | ("filled", "exit")
            | ("filled", "settled")
            | ("exit", "settled")
    )
}

fn intent_status_rank(status: &str) -> i32 {
    match status {
        "planned" => 0,
        "submitted" => 1,
        "skipped" | "failed_before_submit" => 2,
        _ => -1,
    }
}

fn execution_status_rank(status: &str) -> i32 {
    match status {
        "none" => 0,
        "ack" => 1,
        "fill_partial" => 2,
        "filled" => 3,
        "exit" => 4,
        "settled" => 5,
        "canceled" | "expired" | "failed" => 6,
        _ => -1,
    }
}

fn is_stale_intent_downgrade(from: &str, to: &str) -> bool {
    let from_rank = intent_status_rank(from);
    let to_rank = intent_status_rank(to);
    from_rank >= 0 && to_rank >= 0 && from_rank >= to_rank
}

fn is_stale_execution_downgrade(from: &str, to: &str) -> bool {
    let from_rank = execution_status_rank(from);
    let to_rank = execution_status_rank(to);
    from_rank >= 0 && to_rank >= 0 && from_rank >= to_rank
}

fn make_strategy_feature_snapshot_id(
    strategy_id: &str,
    timeframe: &str,
    period_timestamp: u64,
    market_key: &str,
    token_id: &str,
    impulse_id: &str,
    decision_id: Option<&str>,
    rung_id: Option<&str>,
    slice_id: Option<&str>,
) -> String {
    let decision_scope = normalize_snapshot_optional_scope(decision_id);
    let rung_scope = normalize_snapshot_optional_scope(rung_id);
    let slice_scope = normalize_snapshot_optional_scope(slice_id);
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}",
        normalize_strategy_id(strategy_id),
        timeframe.trim().to_ascii_lowercase(),
        period_timestamp,
        normalize_market_key(Some(market_key), None),
        token_id.trim().to_ascii_lowercase(),
        impulse_id.trim().to_ascii_lowercase(),
        decision_scope,
        rung_scope,
        slice_scope
    )
}

fn parse_reason_json_field(reason: Option<&str>, field: &str) -> Option<String> {
    let raw = reason?.trim();
    if raw.is_empty() {
        return None;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    parsed
        .get(field)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

fn parse_reason_json_i64_field(reason: Option<&str>, field: &str) -> Option<i64> {
    let raw = reason?.trim();
    if raw.is_empty() {
        return None;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let value = parsed.get(field)?;
    match value {
        serde_json::Value::Number(num) => num
            .as_i64()
            .or_else(|| num.as_u64().and_then(|v| i64::try_from(v).ok())),
        serde_json::Value::String(txt) => txt.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn parse_reason_nested_json_i64_field(
    reason: Option<&str>,
    outer_field: &str,
    inner_field: &str,
) -> Option<i64> {
    let raw = reason?.trim();
    if raw.is_empty() {
        return None;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let outer = parsed.get(outer_field)?;
    let nested = match outer {
        serde_json::Value::String(txt) => serde_json::from_str::<serde_json::Value>(txt).ok()?,
        serde_json::Value::Object(_) => outer.clone(),
        _ => return None,
    };
    let inner = nested.get(inner_field)?;
    match inner {
        serde_json::Value::Number(num) => num
            .as_i64()
            .or_else(|| num.as_u64().and_then(|v| i64::try_from(v).ok())),
        serde_json::Value::String(txt) => txt.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn extract_order_id_from_trade_event(event: &TradeEventRecord) -> Option<String> {
    if let Some(order_id) = parse_reason_json_field(event.reason.as_deref(), "order_id") {
        return Some(order_id);
    }
    if let Some(event_key) = event.event_key.as_deref() {
        let normalized = event_key.trim();
        if let Some(order_id) = normalized.strip_prefix("entry_fill_order:") {
            let order_id = order_id.trim();
            if !order_id.is_empty() {
                return Some(order_id.to_string());
            }
        }
        if let Some(order_id) = normalized.strip_prefix("entry_fill_reconciled:") {
            let order_id = order_id.trim();
            if !order_id.is_empty() {
                return Some(order_id.to_string());
            }
        }
    }
    None
}

fn has_canonical_entry_fill_for_order(conn: &Connection, order_id: &str) -> Result<bool> {
    let normalized_order_id = order_id.trim().to_ascii_lowercase();
    let canonical_event_key = format!("entry_fill_order:{}", normalized_order_id);
    let count: i64 = conn.query_row(
        r#"
SELECT COUNT(1)
FROM trade_events
WHERE event_type='ENTRY_FILL'
  AND (
        event_key=?1
        OR (
            json_valid(reason)
            AND LOWER(TRIM(COALESCE(json_extract(reason, '$.order_id'), ''))) = ?2
        )
      )
"#,
        params![canonical_event_key, normalized_order_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn should_apply_reconciled_fill_monetary(
    conn: &Connection,
    event: &TradeEventRecord,
) -> Result<bool> {
    if !event
        .event_type
        .trim()
        .eq_ignore_ascii_case("ENTRY_FILL_RECONCILED")
    {
        return Ok(true);
    }
    let Some(order_id) = extract_order_id_from_trade_event(event) else {
        return Ok(true);
    };
    Ok(!has_canonical_entry_fill_for_order(
        conn,
        order_id.as_str(),
    )?)
}

fn extract_trade_key_from_trade_event(event: &TradeEventRecord) -> Option<String> {
    parse_reason_json_field(event.reason.as_deref(), "trade_key")
}

fn extract_request_id_from_trade_event(event: &TradeEventRecord) -> Option<String> {
    parse_reason_json_field(event.reason.as_deref(), "request_id")
}

fn infer_snapshot_status_from_event(event: &TradeEventRecord) -> (&'static str, &'static str) {
    let event_type = event.event_type.trim().to_ascii_uppercase();
    let is_market_close = is_market_close_reason(event.reason.as_deref());
    match event_type.as_str() {
        "ENTRY_SUBMIT" => ("submitted", "none"),
        "ENTRY_ACK" => ("submitted", "ack"),
        "ENTRY_FILL" | "ENTRY_FILL_RECONCILED" => ("submitted", "filled"),
        "ENTRY_FAILED" => ("failed_before_submit", "failed"),
        "RISK_REJECT" => ("failed_before_submit", "failed"),
        "EXIT" | "EXIT_SIGNAL" | "DISTRIBUTION_EXIT_FILLED" => {
            if is_market_close {
                ("submitted", "settled")
            } else {
                ("submitted", "exit")
            }
        }
        _ => ("planned", "none"),
    }
}

fn infer_impulse_id_from_trade_event(
    strategy_id: &str,
    timeframe: &str,
    period_timestamp: u64,
    token_id: &str,
    event: &TradeEventRecord,
    order_id: Option<&str>,
    trade_key: Option<&str>,
) -> String {
    if let Some(order_id) = order_id {
        return format!("order:{}", order_id.trim().to_ascii_lowercase());
    }
    if let Some(trade_key) = trade_key {
        return format!("trade:{}", trade_key.trim().to_ascii_lowercase());
    }
    if let Some(event_key) = event.event_key.as_deref() {
        let normalized = event_key.trim().to_ascii_lowercase();
        if !normalized.is_empty() {
            return format!("event:{}", normalized);
        }
    }
    format!(
        "event:{}:{}:{}:{}:{}",
        normalize_strategy_id(strategy_id),
        timeframe.trim().to_ascii_lowercase(),
        period_timestamp,
        token_id.trim().to_ascii_lowercase(),
        event.event_type.trim().to_ascii_lowercase()
    )
}

fn infer_asset_symbol_from_token_type(token_type: Option<&str>) -> Option<String> {
    let normalized = token_type
        .map(|v| v.trim().to_ascii_uppercase())
        .filter(|v| !v.is_empty())?;
    if normalized.starts_with("BTC") {
        Some("BTC".to_string())
    } else if normalized.starts_with("ETH") {
        Some("ETH".to_string())
    } else if normalized.starts_with("SOL") {
        Some("SOL".to_string())
    } else if normalized.starts_with("XRP") {
        Some("XRP".to_string())
    } else {
        None
    }
}

fn normalize_asset_symbol(
    asset_symbol: Option<&str>,
    token_type: Option<&str>,
    market_key: Option<&str>,
) -> String {
    let normalized = asset_symbol.unwrap_or("").trim().to_ascii_uppercase();
    if !normalized.is_empty() {
        return normalized;
    }
    if let Some(inferred) = infer_asset_symbol_from_token_type(token_type) {
        return inferred;
    }
    let from_market_key = market_key.unwrap_or("").trim().to_ascii_uppercase();
    if !from_market_key.is_empty() {
        from_market_key
    } else {
        DEFAULT_ASSET_SYMBOL.to_string()
    }
}

fn normalize_run_id(run_id: Option<&str>) -> String {
    run_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .unwrap_or_else(|| DEFAULT_RUN_ID.to_string())
}

fn normalize_strategy_cohort(cohort: Option<&str>) -> String {
    cohort
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_else(|| DEFAULT_STRATEGY_COHORT.to_string())
}

fn runtime_run_id() -> &'static str {
    static RUN_ID: OnceLock<String> = OnceLock::new();
    RUN_ID
        .get_or_init(|| {
            let from_env = std::env::var("EVPOLY_RUN_ID")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty());
            if let Some(env_value) = from_env.as_deref() {
                normalize_run_id(Some(env_value))
            } else {
                let generated = format!(
                    "run_{}_{}",
                    chrono::Utc::now().timestamp(),
                    std::process::id()
                );
                normalize_run_id(Some(generated.as_str()))
            }
        })
        .as_str()
}

fn runtime_strategy_cohort() -> &'static str {
    static STRATEGY_COHORT: OnceLock<String> = OnceLock::new();
    STRATEGY_COHORT
        .get_or_init(|| {
            let from_env = std::env::var("EVPOLY_STRATEGY_COHORT")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty());
            normalize_strategy_cohort(from_env.as_deref())
        })
        .as_str()
}

fn reporting_prod_only() -> bool {
    env_bool("EVPOLY_REPORTING_PROD_ONLY", true)
}

fn prod_event_filter_sql(alias: &str) -> String {
    let prefix = if alias.trim().is_empty() {
        "".to_string()
    } else {
        format!("{alias}.")
    };
    format!(
        "LOWER(COALESCE({}strategy_cohort,'')) NOT IN ('dryrun','simulation','paper') \
AND LOWER(COALESCE({}run_id,'')) NOT LIKE 'sim_%' \
AND LOWER(COALESCE({}run_id,'')) NOT LIKE 'dryrun%'",
        prefix, prefix, prefix
    )
}

fn strategy_id_select_expr(has_strategy_id_column: bool) -> &'static str {
    if has_strategy_id_column {
        "COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default')"
    } else {
        "'legacy_default'"
    }
}

fn position_key(
    strategy_id: &str,
    timeframe: &str,
    period_timestamp: u64,
    token_id: &str,
) -> String {
    format!(
        "{}:{}:{}:{}",
        normalize_strategy_id(strategy_id),
        timeframe.trim().to_ascii_lowercase(),
        period_timestamp,
        token_id.trim()
    )
}

fn lifecycle_key(
    strategy_id: &str,
    timeframe: &str,
    period_timestamp: u64,
    token_id: &str,
) -> String {
    position_key(strategy_id, timeframe, period_timestamp, token_id)
}

fn is_market_close_reason(reason: Option<&str>) -> bool {
    reason
        .map(|v| v.to_ascii_lowercase().contains("market_close"))
        .unwrap_or(false)
}

fn bucket_signed_notional(value: f64) -> &'static str {
    let abs = value.abs();
    if abs < 100_000.0 {
        "<100k"
    } else if abs < 300_000.0 {
        "100k-300k"
    } else if abs < 700_000.0 {
        "300k-700k"
    } else if abs < 1_500_000.0 {
        "700k-1.5m"
    } else {
        ">=1.5m"
    }
}

fn bucket_burst_zscore(value: f64) -> &'static str {
    let abs = value.abs();
    if abs < 1.5 {
        "<1.5"
    } else if abs < 2.5 {
        "1.5-2.5"
    } else if abs < 3.5 {
        "2.5-3.5"
    } else if abs < 5.0 {
        "3.5-5.0"
    } else {
        ">=5.0"
    }
}

fn bucket_side_imbalance(value: f64) -> &'static str {
    let abs = value.abs();
    if abs < 0.20 {
        "<0.20"
    } else if abs < 0.40 {
        "0.20-0.40"
    } else if abs < 0.60 {
        "0.40-0.60"
    } else if abs < 0.80 {
        "0.60-0.80"
    } else {
        ">=0.80"
    }
}

fn bucket_impulse_strength(value: f64) -> &'static str {
    if value < 0.20 {
        "<0.20"
    } else if value < 0.40 {
        "0.20-0.40"
    } else if value < 0.70 {
        "0.40-0.70"
    } else if value < 1.00 {
        "0.70-1.00"
    } else {
        ">=1.00"
    }
}

fn bucket_probability(value: f64) -> &'static str {
    if value < 0.0 {
        "<0"
    } else if value < 0.20 {
        "0-0.20"
    } else if value < 0.40 {
        "0.20-0.40"
    } else if value < 0.60 {
        "0.40-0.60"
    } else if value < 0.80 {
        "0.60-0.80"
    } else if value < 1.00 {
        "0.80-1.00"
    } else {
        ">=1.00"
    }
}

fn bucket_bps(value: f64) -> &'static str {
    if value < 0.0 {
        "<0"
    } else if value < 10.0 {
        "0-10"
    } else if value < 25.0 {
        "10-25"
    } else if value < 50.0 {
        "25-50"
    } else if value < 100.0 {
        "50-100"
    } else {
        ">=100"
    }
}

fn bucket_usd(value: f64) -> &'static str {
    let abs = value.abs();
    if abs < 5.0 {
        "<5"
    } else if abs < 15.0 {
        "5-15"
    } else if abs < 30.0 {
        "15-30"
    } else if abs < 60.0 {
        "30-60"
    } else if abs < 120.0 {
        "60-120"
    } else if abs < 250.0 {
        "120-250"
    } else if abs < 500.0 {
        "250-500"
    } else {
        ">=500"
    }
}

fn bucket_seconds(value: f64) -> &'static str {
    if value < 15.0 {
        "<15s"
    } else if value < 60.0 {
        "15-60s"
    } else if value < 180.0 {
        "60-180s"
    } else if value < 600.0 {
        "180-600s"
    } else if value < 1800.0 {
        "600-1800s"
    } else {
        ">=1800s"
    }
}

fn bucket_count(value: f64) -> &'static str {
    let rounded = value.round() as i64;
    match rounded {
        i64::MIN..=-1 => "<0",
        0 => "0",
        1 => "1",
        2 => "2",
        3 => "3",
        4 => "4",
        5 => "5",
        6..=8 => "6-8",
        9..=12 => "9-12",
        _ => ">=13",
    }
}

fn bucket_generic_numeric(value: f64) -> &'static str {
    let abs = value.abs();
    if value < 0.0 {
        "neg"
    } else if abs < f64::EPSILON {
        "zero"
    } else if abs < 1.0 {
        "0-1"
    } else if abs < 10.0 {
        "1-10"
    } else if abs < 100.0 {
        "10-100"
    } else {
        ">=100"
    }
}

fn bucket_numeric_feature(feature: &str, value: f64) -> String {
    let key = feature.trim().to_ascii_lowercase();
    if key.contains("signed_notional") {
        return bucket_signed_notional(value).to_string();
    }
    if key.contains("burst_zscore") || key.contains("zscore") {
        return bucket_burst_zscore(value).to_string();
    }
    if key.contains("side_imbalance") {
        return bucket_side_imbalance(value).to_string();
    }
    if key.contains("impulse_strength") {
        return bucket_impulse_strength(value).to_string();
    }
    if key.ends_with("_bps")
        || key.contains("edge_bps")
        || key.contains("buffer_bps")
        || key.contains("uncertainty_bps")
    {
        return bucket_bps(value).to_string();
    }
    if key.contains("price")
        || key.contains("probability")
        || key.contains("confidence")
        || key.contains("ratio")
        || key.contains("imbalance")
    {
        return bucket_probability(value).to_string();
    }
    if key.ends_with("_usd") || key.contains("notional") || key.contains("size_usd") {
        return bucket_usd(value).to_string();
    }
    if key.ends_with("_seconds") || key.ends_with("_ms") {
        return bucket_seconds(value).to_string();
    }
    if key.contains("count") || key.contains("rung") || key.contains("slice") {
        return bucket_count(value).to_string();
    }
    bucket_generic_numeric(value).to_string()
}

fn map_feature_bucket(feature: &str, value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Number(number) => {
            number.as_f64().map(|v| bucket_numeric_feature(feature, v))
        }
        serde_json::Value::Bool(v) => Some(if *v {
            "true".to_string()
        } else {
            "false".to_string()
        }),
        serde_json::Value::String(raw) => {
            let normalized = raw.trim().to_ascii_lowercase();
            if normalized.is_empty() {
                None
            } else if matches!(
                feature,
                "trigger_flow_source" | "bias_alignment" | "urgency" | "event"
            ) {
                Some(normalized)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

fn ensure_settlement_v2_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS fills_v2 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_key TEXT NOT NULL UNIQUE,
    ts_ms INTEGER NOT NULL,
    period_timestamp INTEGER NOT NULL,
    timeframe TEXT NOT NULL,
    strategy_id TEXT NOT NULL,
    condition_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    token_type TEXT,
    side TEXT NOT NULL,
    price REAL,
    units REAL,
    notional_usd REAL,
    fee_usd REAL NOT NULL DEFAULT 0.0,
    source_event_type TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS positions_v2 (
    position_key TEXT PRIMARY KEY,
    strategy_id TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    period_timestamp INTEGER NOT NULL,
    condition_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    token_type TEXT,
    entry_units REAL NOT NULL DEFAULT 0.0,
    entry_notional_usd REAL NOT NULL DEFAULT 0.0,
    entry_fee_usd REAL NOT NULL DEFAULT 0.0,
    exit_units REAL NOT NULL DEFAULT 0.0,
    exit_notional_usd REAL NOT NULL DEFAULT 0.0,
    exit_fee_usd REAL NOT NULL DEFAULT 0.0,
    inventory_consumed_units REAL NOT NULL DEFAULT 0.0,
    inventory_consumed_cost_usd REAL NOT NULL DEFAULT 0.0,
    settled_payout_usd REAL NOT NULL DEFAULT 0.0,
    realized_pnl_usd REAL NOT NULL DEFAULT 0.0,
    status TEXT NOT NULL DEFAULT 'OPEN',
    opened_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    settled_at_ms INTEGER
);

CREATE TABLE IF NOT EXISTS settlements_v2 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    settlement_key TEXT NOT NULL UNIQUE,
    ts_ms INTEGER NOT NULL,
    position_key TEXT NOT NULL,
    period_timestamp INTEGER NOT NULL,
    timeframe TEXT NOT NULL,
    strategy_id TEXT NOT NULL,
    condition_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    token_type TEXT,
    units REAL NOT NULL,
    entry_cost_usd REAL NOT NULL,
    payout_usd REAL NOT NULL,
    fee_usd REAL NOT NULL DEFAULT 0.0,
    total_cost_basis_usd REAL NOT NULL,
    settled_pnl_usd REAL NOT NULL,
    outcome_label TEXT NOT NULL,
    source_event_key TEXT,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS marks_v2 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    mark_key TEXT NOT NULL UNIQUE,
    ts_ms INTEGER NOT NULL,
    position_key TEXT NOT NULL,
    period_timestamp INTEGER NOT NULL,
    timeframe TEXT NOT NULL,
    strategy_id TEXT NOT NULL,
    condition_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    price REAL NOT NULL,
    units REAL,
    notional_usd REAL,
    source_event_type TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS inventory_consumption_events_v1 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    consume_key TEXT NOT NULL UNIQUE,
    ts_ms INTEGER NOT NULL,
    strategy_id TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    period_timestamp INTEGER NOT NULL,
    condition_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    requested_units REAL NOT NULL,
    applied_units REAL NOT NULL,
    applied_cost_usd REAL NOT NULL DEFAULT 0.0,
    wallet_units_after REAL,
    db_units_before REAL NOT NULL DEFAULT 0.0,
    db_units_after REAL NOT NULL DEFAULT 0.0,
    source TEXT NOT NULL,
    reason TEXT,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS inventory_consumption_allocations_v1 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    consume_key TEXT NOT NULL,
    allocation_seq INTEGER NOT NULL,
    position_key TEXT NOT NULL,
    strategy_id TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    period_timestamp INTEGER NOT NULL,
    condition_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    consumed_units REAL NOT NULL,
    consumed_cost_usd REAL NOT NULL DEFAULT 0.0,
    created_at_ms INTEGER NOT NULL,
    UNIQUE(consume_key, position_key)
);

CREATE INDEX IF NOT EXISTS idx_fills_v2_ts ON fills_v2(ts_ms DESC);
CREATE INDEX IF NOT EXISTS idx_fills_v2_position ON fills_v2(strategy_id, timeframe, period_timestamp, token_id);
CREATE INDEX IF NOT EXISTS idx_positions_v2_status ON positions_v2(status, timeframe, period_timestamp);
CREATE INDEX IF NOT EXISTS idx_positions_v2_updated ON positions_v2(updated_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_settlements_v2_ts ON settlements_v2(ts_ms DESC);
CREATE INDEX IF NOT EXISTS idx_settlements_v2_strategy_ts ON settlements_v2(strategy_id, ts_ms DESC);
CREATE INDEX IF NOT EXISTS idx_marks_v2_position_ts ON marks_v2(position_key, ts_ms DESC);
CREATE INDEX IF NOT EXISTS idx_inventory_consumption_events_strategy_ts
    ON inventory_consumption_events_v1(strategy_id, ts_ms DESC);
CREATE INDEX IF NOT EXISTS idx_inventory_consumption_events_condition_token
    ON inventory_consumption_events_v1(condition_id, token_id, ts_ms DESC);
CREATE INDEX IF NOT EXISTS idx_inventory_consumption_allocations_consume
    ON inventory_consumption_allocations_v1(consume_key, allocation_seq ASC);
CREATE INDEX IF NOT EXISTS idx_inventory_consumption_allocations_position
    ON inventory_consumption_allocations_v1(position_key, created_at_ms DESC);
"#,
    )?;

    ensure_column(
        conn,
        "positions_v2",
        "inventory_consumed_units",
        "REAL NOT NULL DEFAULT 0.0",
    )?;
    ensure_column(
        conn,
        "positions_v2",
        "inventory_consumed_cost_usd",
        "REAL NOT NULL DEFAULT 0.0",
    )?;

    conn.execute_batch(
        r#"
INSERT OR IGNORE INTO fills_v2 (
    event_key, ts_ms, period_timestamp, timeframe, strategy_id, condition_id, token_id, token_type,
    side, price, units, notional_usd, fee_usd, source_event_type, created_at_ms
)
SELECT
    COALESCE(event_key, printf('legacy_entry_fill:%d:%d', id, ts_ms)),
    ts_ms,
    period_timestamp,
    LOWER(TRIM(timeframe)),
    COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default'),
    condition_id,
    token_id,
    token_type,
    UPPER(COALESCE(NULLIF(TRIM(side), ''), 'BUY')),
    price,
    units,
    notional_usd,
    0.0,
    event_type,
    ts_ms
FROM trade_events
WHERE event_type='ENTRY_FILL'
  AND condition_id IS NOT NULL
  AND token_id IS NOT NULL;

INSERT INTO positions_v2 (
    position_key, strategy_id, timeframe, period_timestamp, condition_id, token_id, token_type,
    entry_units, entry_notional_usd, entry_fee_usd,
    exit_units, exit_notional_usd, exit_fee_usd,
    settled_payout_usd, realized_pnl_usd, status, opened_at_ms, updated_at_ms
)
SELECT
    COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default') || ':' ||
        LOWER(TRIM(timeframe)) || ':' || period_timestamp || ':' || token_id,
    COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default'),
    LOWER(TRIM(timeframe)),
    period_timestamp,
    condition_id,
    token_id,
    MAX(token_type),
    COALESCE(SUM(units), 0.0),
    COALESCE(SUM(notional_usd), 0.0),
    COALESCE(SUM(fee_usd), 0.0),
    0.0,
    0.0,
    0.0,
    0.0,
    0.0,
    'OPEN',
    MIN(ts_ms),
    MAX(ts_ms)
FROM fills_v2
GROUP BY strategy_id, timeframe, period_timestamp, condition_id, token_id
ON CONFLICT(position_key) DO UPDATE SET
    entry_units=MAX(positions_v2.entry_units, excluded.entry_units),
    entry_notional_usd=MAX(positions_v2.entry_notional_usd, excluded.entry_notional_usd),
    entry_fee_usd=MAX(positions_v2.entry_fee_usd, excluded.entry_fee_usd),
    token_type=COALESCE(positions_v2.token_type, excluded.token_type),
    updated_at_ms=MAX(positions_v2.updated_at_ms, excluded.updated_at_ms);

INSERT OR IGNORE INTO settlements_v2 (
    settlement_key, ts_ms, position_key, period_timestamp, timeframe, strategy_id, condition_id, token_id,
    token_type, units, entry_cost_usd, payout_usd, fee_usd, total_cost_basis_usd,
    settled_pnl_usd, outcome_label, source_event_key, created_at_ms
)
SELECT
    COALESCE(event_key, printf('legacy_settlement:%d:%d', id, ts_ms)),
    ts_ms,
    COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default') || ':' ||
        LOWER(TRIM(timeframe)) || ':' || period_timestamp || ':' || token_id,
    period_timestamp,
    LOWER(TRIM(timeframe)),
    COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default'),
    condition_id,
    token_id,
    token_type,
    COALESCE(units, 0.0),
    CASE
        WHEN notional_usd IS NOT NULL AND pnl_usd IS NOT NULL THEN MAX(notional_usd - pnl_usd, 0.0)
        ELSE 0.0
    END,
    COALESCE(notional_usd, 0.0),
    0.0,
    CASE
        WHEN notional_usd IS NOT NULL AND pnl_usd IS NOT NULL THEN MAX(notional_usd - pnl_usd, 0.0)
        ELSE 0.0
    END,
    COALESCE(pnl_usd, 0.0),
    CASE
        WHEN COALESCE(pnl_usd, 0.0) > 0 THEN 'WIN'
        WHEN COALESCE(pnl_usd, 0.0) < 0 THEN 'LOSS'
        ELSE 'FLAT'
    END,
    event_key,
    ts_ms
FROM trade_events
WHERE event_type='EXIT'
  AND reason LIKE '%market_close%'
  AND condition_id IS NOT NULL
  AND token_id IS NOT NULL;

UPDATE positions_v2
SET
    status='SETTLED',
    settled_at_ms=COALESCE(
        (
            SELECT s.ts_ms
            FROM settlements_v2 s
            WHERE s.position_key=positions_v2.position_key
            ORDER BY s.ts_ms DESC, s.id DESC
            LIMIT 1
        ),
        settled_at_ms
    ),
    settled_payout_usd=COALESCE(
        (
            SELECT s.payout_usd
            FROM settlements_v2 s
            WHERE s.position_key=positions_v2.position_key
            ORDER BY s.ts_ms DESC, s.id DESC
            LIMIT 1
        ),
        settled_payout_usd
    ),
    realized_pnl_usd=COALESCE(
        (
            SELECT s.settled_pnl_usd
            FROM settlements_v2 s
            WHERE s.position_key=positions_v2.position_key
            ORDER BY s.ts_ms DESC, s.id DESC
            LIMIT 1
        ),
        realized_pnl_usd
    ),
    updated_at_ms=COALESCE(
        (
            SELECT s.ts_ms
            FROM settlements_v2 s
            WHERE s.position_key=positions_v2.position_key
            ORDER BY s.ts_ms DESC, s.id DESC
            LIMIT 1
        ),
        updated_at_ms
    )
WHERE EXISTS (
    SELECT 1 FROM settlements_v2 s WHERE s.position_key=positions_v2.position_key
);

INSERT OR IGNORE INTO marks_v2 (
    mark_key, ts_ms, position_key, period_timestamp, timeframe, strategy_id,
    condition_id, token_id, price, units, notional_usd, source_event_type, created_at_ms
)
SELECT
    COALESCE(event_key, printf('legacy_mark:%d:%d', id, ts_ms)),
    ts_ms,
    COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default') || ':' ||
        LOWER(TRIM(timeframe)) || ':' || period_timestamp || ':' || token_id,
    period_timestamp,
    LOWER(TRIM(timeframe)),
    COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default'),
    condition_id,
    token_id,
    price,
    units,
    notional_usd,
    event_type,
    ts_ms
FROM trade_events
WHERE event_type IN ('ENTRY_FILL', 'EXIT')
  AND condition_id IS NOT NULL
  AND token_id IS NOT NULL
  AND price IS NOT NULL;
"#,
    )?;
    Ok(())
}

fn ensure_lifecycle_wallet_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS trade_lifecycle_v1 (
    lifecycle_key TEXT PRIMARY KEY,
    strategy_id TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    period_timestamp INTEGER NOT NULL,
    condition_id TEXT,
    token_id TEXT NOT NULL,
    token_type TEXT,
    side TEXT,
    entry_fill_ts_ms INTEGER,
    entry_units REAL NOT NULL DEFAULT 0.0,
    entry_notional_usd REAL NOT NULL DEFAULT 0.0,
    entry_event_key TEXT,
    resolved_ts_ms INTEGER,
    resolved_outcome_label TEXT,
    resolved_payout_usd REAL,
    resolved_pnl_usd REAL,
    exit_event_key TEXT,
    resolution_source TEXT NOT NULL DEFAULT 'polymarket_market_api',
    verification_status TEXT NOT NULL DEFAULT 'not_checked',
    verification_detail TEXT,
    redemption_status TEXT NOT NULL DEFAULT 'pending',
    redemption_tx_id TEXT,
    redemption_attempts INTEGER NOT NULL DEFAULT 0,
    redeemed_ts_ms INTEGER,
    redeemed_notional_usd REAL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS wallet_cashflow_events_v1 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cashflow_key TEXT NOT NULL UNIQUE,
    ts_ms INTEGER NOT NULL,
    strategy_id TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    period_timestamp INTEGER NOT NULL,
    condition_id TEXT,
    token_id TEXT,
    event_type TEXT NOT NULL,
    amount_usd REAL,
    units REAL,
    token_price REAL,
    tx_id TEXT,
    reason TEXT,
    source TEXT NOT NULL DEFAULT 'redemption_sweep',
    created_at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_trade_lifecycle_strategy_period
    ON trade_lifecycle_v1(strategy_id, timeframe, period_timestamp);
CREATE INDEX IF NOT EXISTS idx_trade_lifecycle_redemption_status
    ON trade_lifecycle_v1(redemption_status, updated_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_trade_lifecycle_resolution_status
    ON trade_lifecycle_v1(verification_status, updated_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_wallet_cashflow_strategy_ts
    ON wallet_cashflow_events_v1(strategy_id, ts_ms DESC);
CREATE INDEX IF NOT EXISTS idx_wallet_cashflow_period
    ON wallet_cashflow_events_v1(period_timestamp, timeframe, strategy_id);
"#,
    )?;

    // Backfill lifecycle rows from canonical fill + settlement tables.
    conn.execute_batch(
        r#"
INSERT OR IGNORE INTO trade_lifecycle_v1 (
    lifecycle_key, strategy_id, timeframe, period_timestamp, condition_id, token_id, token_type, side,
    entry_fill_ts_ms, entry_units, entry_notional_usd, entry_event_key,
    created_at_ms, updated_at_ms
)
SELECT
    p.position_key,
    p.strategy_id,
    p.timeframe,
    p.period_timestamp,
    p.condition_id,
    p.token_id,
    p.token_type,
    'BUY',
    (
        SELECT MAX(f.ts_ms)
        FROM fills_v2 f
        WHERE f.strategy_id = p.strategy_id
          AND f.timeframe = p.timeframe
          AND f.period_timestamp = p.period_timestamp
          AND f.token_id = p.token_id
    ),
    COALESCE(p.entry_units, 0.0),
    COALESCE(p.entry_notional_usd, 0.0),
    (
        SELECT f.event_key
        FROM fills_v2 f
        WHERE f.strategy_id = p.strategy_id
          AND f.timeframe = p.timeframe
          AND f.period_timestamp = p.period_timestamp
          AND f.token_id = p.token_id
        ORDER BY f.ts_ms DESC, f.id DESC
        LIMIT 1
    ),
    p.opened_at_ms,
    p.updated_at_ms
FROM positions_v2 p;

UPDATE trade_lifecycle_v1
SET
    resolved_ts_ms = (
        SELECT s.ts_ms
        FROM settlements_v2 s
        WHERE s.position_key = trade_lifecycle_v1.lifecycle_key
        ORDER BY s.ts_ms DESC, s.id DESC
        LIMIT 1
    ),
    resolved_outcome_label = (
        SELECT s.outcome_label
        FROM settlements_v2 s
        WHERE s.position_key = trade_lifecycle_v1.lifecycle_key
        ORDER BY s.ts_ms DESC, s.id DESC
        LIMIT 1
    ),
    resolved_payout_usd = (
        SELECT s.payout_usd
        FROM settlements_v2 s
        WHERE s.position_key = trade_lifecycle_v1.lifecycle_key
        ORDER BY s.ts_ms DESC, s.id DESC
        LIMIT 1
    ),
    resolved_pnl_usd = (
        SELECT s.settled_pnl_usd
        FROM settlements_v2 s
        WHERE s.position_key = trade_lifecycle_v1.lifecycle_key
        ORDER BY s.ts_ms DESC, s.id DESC
        LIMIT 1
    ),
    exit_event_key = (
        SELECT s.source_event_key
        FROM settlements_v2 s
        WHERE s.position_key = trade_lifecycle_v1.lifecycle_key
        ORDER BY s.ts_ms DESC, s.id DESC
        LIMIT 1
    ),
    updated_at_ms = MAX(
        updated_at_ms,
        COALESCE((
            SELECT s.ts_ms
            FROM settlements_v2 s
            WHERE s.position_key = trade_lifecycle_v1.lifecycle_key
            ORDER BY s.ts_ms DESC, s.id DESC
            LIMIT 1
        ), updated_at_ms)
    )
WHERE EXISTS (
    SELECT 1
    FROM settlements_v2 s
    WHERE s.position_key = trade_lifecycle_v1.lifecycle_key
);
"#,
    )?;
    Ok(())
}

fn ensure_mm_market_state_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS mm_market_states_v1 (
    strategy_id TEXT NOT NULL,
    condition_id TEXT NOT NULL,
    market_slug TEXT,
    timeframe TEXT NOT NULL,
    symbol TEXT,
    mode TEXT,
    period_timestamp INTEGER NOT NULL,
    state TEXT NOT NULL,
    reason TEXT,
    reward_rate_hint REAL,
    open_buy_orders INTEGER NOT NULL DEFAULT 0,
    open_sell_orders INTEGER NOT NULL DEFAULT 0,
    favor_inventory_shares REAL,
    weak_inventory_shares REAL,
    up_token_id TEXT,
    down_token_id TEXT,
    up_inventory_shares REAL,
    down_inventory_shares REAL,
    imbalance_shares REAL,
    imbalance_side TEXT,
    avg_entry_up REAL,
    avg_entry_down REAL,
    up_best_bid REAL,
    down_best_bid REAL,
    mtm_pnl_exit_usd REAL,
    time_in_imbalance_ms INTEGER,
    max_imbalance_shares REAL,
    last_rebalance_duration_ms INTEGER,
    rebalance_fill_rate REAL,
    exit_slippage_vs_bbo_bps REAL,
    market_mode_runtime TEXT,
    ladder_top_price REAL,
    ladder_bottom_price REAL,
    last_event_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(strategy_id, condition_id)
);

CREATE INDEX IF NOT EXISTS idx_mm_market_states_strategy_state
    ON mm_market_states_v1(strategy_id, state, updated_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_mm_market_states_updated
    ON mm_market_states_v1(updated_at_ms DESC);
"#,
    )?;
    ensure_column(&conn, "mm_market_states_v1", "up_token_id", "TEXT")?;
    ensure_column(&conn, "mm_market_states_v1", "down_token_id", "TEXT")?;
    ensure_column(&conn, "mm_market_states_v1", "up_inventory_shares", "REAL")?;
    ensure_column(
        &conn,
        "mm_market_states_v1",
        "down_inventory_shares",
        "REAL",
    )?;
    ensure_column(&conn, "mm_market_states_v1", "imbalance_shares", "REAL")?;
    ensure_column(&conn, "mm_market_states_v1", "imbalance_side", "TEXT")?;
    ensure_column(&conn, "mm_market_states_v1", "avg_entry_up", "REAL")?;
    ensure_column(&conn, "mm_market_states_v1", "avg_entry_down", "REAL")?;
    ensure_column(&conn, "mm_market_states_v1", "up_best_bid", "REAL")?;
    ensure_column(&conn, "mm_market_states_v1", "down_best_bid", "REAL")?;
    ensure_column(&conn, "mm_market_states_v1", "mtm_pnl_exit_usd", "REAL")?;
    ensure_column(
        &conn,
        "mm_market_states_v1",
        "time_in_imbalance_ms",
        "INTEGER",
    )?;
    ensure_column(&conn, "mm_market_states_v1", "max_imbalance_shares", "REAL")?;
    ensure_column(
        &conn,
        "mm_market_states_v1",
        "last_rebalance_duration_ms",
        "INTEGER",
    )?;
    ensure_column(&conn, "mm_market_states_v1", "rebalance_fill_rate", "REAL")?;
    ensure_column(
        &conn,
        "mm_market_states_v1",
        "exit_slippage_vs_bbo_bps",
        "REAL",
    )?;
    ensure_column(&conn, "mm_market_states_v1", "market_mode_runtime", "TEXT")?;
    Ok(())
}

fn ensure_strategy_feature_snapshot_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS strategy_feature_snapshots_v1 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    snapshot_id TEXT NOT NULL,
    strategy_id TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    period_timestamp INTEGER NOT NULL,
    market_key TEXT NOT NULL DEFAULT 'btc',
    asset_symbol TEXT NOT NULL DEFAULT 'BTC',
    condition_id TEXT,
    token_id TEXT NOT NULL,
    impulse_id TEXT NOT NULL,
    decision_id TEXT,
    rung_id TEXT,
    slice_id TEXT,
    request_id TEXT,
    order_id TEXT,
    trade_key TEXT,
    position_key TEXT,
    intent_status TEXT NOT NULL DEFAULT 'planned',
    execution_status TEXT NOT NULL DEFAULT 'none',
    intent_ts_ms INTEGER NOT NULL,
    submit_ts_ms INTEGER,
    ack_ts_ms INTEGER,
    fill_ts_ms INTEGER,
    exit_ts_ms INTEGER,
    settled_ts_ms INTEGER,
    entry_notional_usd REAL,
    exit_notional_usd REAL,
    realized_pnl_usd REAL,
    settled_pnl_usd REAL,
    hold_ms INTEGER,
    feature_json TEXT,
    decision_context_json TEXT,
    execution_context_json TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS uidx_strategy_feature_snapshots_snapshot_id
    ON strategy_feature_snapshots_v1(snapshot_id);
CREATE UNIQUE INDEX IF NOT EXISTS uidx_strategy_feature_snapshots_logical_scope
    ON strategy_feature_snapshots_v1(
        strategy_id,
        timeframe,
        period_timestamp,
        market_key,
        token_id,
        impulse_id,
        COALESCE(decision_id, ''),
        COALESCE(rung_id, ''),
        COALESCE(slice_id, '')
    );
CREATE INDEX IF NOT EXISTS idx_strategy_feature_snapshots_market_slot
    ON strategy_feature_snapshots_v1(strategy_id, timeframe, period_timestamp, market_key);
CREATE INDEX IF NOT EXISTS idx_strategy_feature_snapshots_strategy_time
    ON strategy_feature_snapshots_v1(strategy_id, intent_ts_ms DESC);
CREATE INDEX IF NOT EXISTS idx_strategy_feature_snapshots_status_time
    ON strategy_feature_snapshots_v1(strategy_id, execution_status, intent_ts_ms DESC);
CREATE INDEX IF NOT EXISTS idx_strategy_feature_snapshots_order_id
    ON strategy_feature_snapshots_v1(order_id);
CREATE INDEX IF NOT EXISTS idx_strategy_feature_snapshots_request_id
    ON strategy_feature_snapshots_v1(request_id);
CREATE INDEX IF NOT EXISTS idx_strategy_feature_snapshots_trade_key
    ON strategy_feature_snapshots_v1(trade_key);
CREATE INDEX IF NOT EXISTS idx_strategy_feature_snapshots_position_key
    ON strategy_feature_snapshots_v1(position_key);
CREATE INDEX IF NOT EXISTS idx_strategy_feature_snapshots_decision_id
    ON strategy_feature_snapshots_v1(decision_id);

CREATE TABLE IF NOT EXISTS strategy_feature_snapshot_transition_rejections (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    snapshot_id TEXT NOT NULL,
    strategy_id TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    period_timestamp INTEGER NOT NULL,
    current_intent_status TEXT,
    next_intent_status TEXT,
    current_execution_status TEXT,
    next_execution_status TEXT,
    reason TEXT NOT NULL,
    rejected_at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_strategy_feature_snapshot_transition_rejections_snapshot
    ON strategy_feature_snapshot_transition_rejections(snapshot_id, rejected_at_ms DESC);
"#,
    )?;
    Ok(())
}

fn ensure_evcurve_period_base_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS evcurve_period_bases_v1 (
    symbol TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    period_open_ts INTEGER NOT NULL,
    base_mid REAL NOT NULL,
    first_seen_ts_ms INTEGER NOT NULL,
    PRIMARY KEY(symbol, timeframe, period_open_ts)
);

CREATE INDEX IF NOT EXISTS idx_evcurve_period_bases_ts
    ON evcurve_period_bases_v1(period_open_ts DESC);
"#,
    )?;
    Ok(())
}

fn infer_execution_status_from_snapback_row(
    order_id: Option<&str>,
    fill_ts_ms: Option<i64>,
    exit_ts_ms: Option<i64>,
    settled_ts_ms: Option<i64>,
) -> &'static str {
    if settled_ts_ms.is_some() {
        if fill_ts_ms.is_some() {
            "settled"
        } else if exit_ts_ms.is_some() {
            "exit"
        } else if order_id
            .map(str::trim)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            "ack"
        } else {
            "none"
        }
    } else if exit_ts_ms.is_some() {
        "exit"
    } else if fill_ts_ms.is_some() {
        "filled"
    } else if order_id
        .map(str::trim)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        "ack"
    } else {
        "none"
    }
}

fn backfill_snapback_feature_snapshots_into_unified(conn: &Connection) -> Result<usize> {
    let inserted = conn.execute(
        r#"
INSERT OR IGNORE INTO strategy_feature_snapshots_v1 (
    snapshot_id,
    strategy_id,
    timeframe,
    period_timestamp,
    market_key,
    asset_symbol,
    condition_id,
    token_id,
    impulse_id,
    decision_id,
    rung_id,
    slice_id,
    request_id,
    order_id,
    trade_key,
    position_key,
    intent_status,
    execution_status,
    intent_ts_ms,
    submit_ts_ms,
    ack_ts_ms,
    fill_ts_ms,
    exit_ts_ms,
    settled_ts_ms,
    entry_notional_usd,
    exit_notional_usd,
    realized_pnl_usd,
    settled_pnl_usd,
    hold_ms,
    feature_json,
    decision_context_json,
    execution_context_json,
    created_at_ms,
    updated_at_ms
)
SELECT
    LOWER(TRIM(strategy_id)) || '|' ||
    LOWER(TRIM(timeframe)) || '|' ||
    period_timestamp || '|' ||
    'btc' || '|' ||
    LOWER(TRIM(token_id)) || '|' ||
    LOWER(TRIM(impulse_id)) || '|~|~|~',
    COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default'),
    LOWER(TRIM(timeframe)),
    period_timestamp,
    'btc',
    'BTC',
    condition_id,
    token_id,
    impulse_id,
    NULL,
    NULL,
    NULL,
    request_id,
    order_id,
    NULL,
    position_key,
    CASE
        WHEN request_id IS NOT NULL AND TRIM(request_id) != '' THEN 'submitted'
        ELSE 'planned'
    END,
    CASE
        WHEN settled_ts_ms IS NOT NULL THEN 'settled'
        WHEN exit_ts_ms IS NOT NULL THEN 'exit'
        WHEN fill_ts_ms IS NOT NULL THEN 'filled'
        WHEN order_id IS NOT NULL AND TRIM(order_id) != '' THEN 'ack'
        ELSE 'none'
    END,
    intent_ts_ms,
    CASE
        WHEN request_id IS NOT NULL AND TRIM(request_id) != '' THEN intent_ts_ms
        ELSE NULL
    END,
    CASE
        WHEN order_id IS NOT NULL AND TRIM(order_id) != '' THEN intent_ts_ms
        ELSE NULL
    END,
    fill_ts_ms,
    exit_ts_ms,
    settled_ts_ms,
    target_size_usd,
    NULL,
    realized_pnl_usd,
    settled_pnl_usd,
    hold_ms,
    NULL,
    NULL,
    NULL,
    COALESCE(created_at_ms, intent_ts_ms),
    COALESCE(updated_at_ms, intent_ts_ms)
FROM snapback_feature_snapshots_v1
"#,
        [],
    )?;
    Ok(inserted)
}

fn upsert_trade_lifecycle_entry_fill(
    conn: &Connection,
    event: &TradeEventRecord,
    strategy_id: &str,
    timeframe: &str,
    period_timestamp: u64,
    condition_id: &str,
    token_id: &str,
    token_type: Option<&str>,
    side: &str,
    ts_ms: i64,
) -> Result<()> {
    let lifecycle_key = lifecycle_key(strategy_id, timeframe, period_timestamp, token_id);
    let entry_units = event.units.unwrap_or(0.0).max(0.0);
    let entry_notional_usd = event.notional_usd.unwrap_or(0.0).max(0.0);
    conn.execute(
        r#"
INSERT INTO trade_lifecycle_v1 (
    lifecycle_key, strategy_id, timeframe, period_timestamp, condition_id, token_id, token_type, side,
    entry_fill_ts_ms, entry_units, entry_notional_usd, entry_event_key,
    created_at_ms, updated_at_ms
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
ON CONFLICT(lifecycle_key) DO UPDATE SET
    condition_id=COALESCE(trade_lifecycle_v1.condition_id, excluded.condition_id),
    token_type=COALESCE(trade_lifecycle_v1.token_type, excluded.token_type),
    side=COALESCE(trade_lifecycle_v1.side, excluded.side),
    entry_fill_ts_ms=CASE
        WHEN trade_lifecycle_v1.entry_fill_ts_ms IS NULL THEN excluded.entry_fill_ts_ms
        ELSE MAX(trade_lifecycle_v1.entry_fill_ts_ms, excluded.entry_fill_ts_ms)
    END,
    entry_units=trade_lifecycle_v1.entry_units + excluded.entry_units,
    entry_notional_usd=trade_lifecycle_v1.entry_notional_usd + excluded.entry_notional_usd,
    entry_event_key=COALESCE(excluded.entry_event_key, trade_lifecycle_v1.entry_event_key),
    updated_at_ms=MAX(trade_lifecycle_v1.updated_at_ms, excluded.updated_at_ms)
"#,
        params![
            lifecycle_key,
            strategy_id,
            timeframe,
            period_timestamp as i64,
            condition_id,
            token_id,
            token_type,
            side,
            ts_ms,
            entry_units,
            entry_notional_usd,
            event.event_key,
            ts_ms,
            ts_ms
        ],
    )?;
    Ok(())
}

fn upsert_trade_lifecycle_market_close(
    conn: &Connection,
    event: &TradeEventRecord,
    strategy_id: &str,
    timeframe: &str,
    period_timestamp: u64,
    condition_id: &str,
    token_id: &str,
    token_type: Option<&str>,
    side: &str,
    ts_ms: i64,
) -> Result<()> {
    let lifecycle_key = lifecycle_key(strategy_id, timeframe, period_timestamp, token_id);
    let payout_usd = event.notional_usd.unwrap_or(0.0).max(0.0);
    let pnl_usd = event.pnl_usd.unwrap_or(0.0);
    let outcome = if pnl_usd > 0.0 {
        "WIN"
    } else if pnl_usd < 0.0 {
        "LOSS"
    } else {
        "FLAT"
    };
    conn.execute(
        r#"
INSERT INTO trade_lifecycle_v1 (
    lifecycle_key, strategy_id, timeframe, period_timestamp, condition_id, token_id, token_type, side,
    resolved_ts_ms, resolved_outcome_label, resolved_payout_usd, resolved_pnl_usd, exit_event_key,
    resolution_source, verification_status, updated_at_ms, created_at_ms
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 'polymarket_market_api', 'primary_passed', ?14, ?15)
ON CONFLICT(lifecycle_key) DO UPDATE SET
    condition_id=COALESCE(trade_lifecycle_v1.condition_id, excluded.condition_id),
    token_type=COALESCE(trade_lifecycle_v1.token_type, excluded.token_type),
    side=COALESCE(trade_lifecycle_v1.side, excluded.side),
    resolved_ts_ms=excluded.resolved_ts_ms,
    resolved_outcome_label=excluded.resolved_outcome_label,
    resolved_payout_usd=excluded.resolved_payout_usd,
    resolved_pnl_usd=excluded.resolved_pnl_usd,
    exit_event_key=COALESCE(excluded.exit_event_key, trade_lifecycle_v1.exit_event_key),
    resolution_source='polymarket_market_api',
    verification_status='primary_passed',
    updated_at_ms=MAX(trade_lifecycle_v1.updated_at_ms, excluded.updated_at_ms)
"#,
        params![
            lifecycle_key,
            strategy_id,
            timeframe,
            period_timestamp as i64,
            condition_id,
            token_id,
            token_type,
            side,
            ts_ms,
            outcome,
            payout_usd,
            pnl_usd,
            event.event_key,
            ts_ms,
            ts_ms
        ],
    )?;
    Ok(())
}

fn dual_write_trade_event_v2(
    conn: &Connection,
    event: &TradeEventRecord,
    strategy_id: &str,
) -> Result<()> {
    let event_type = event.event_type.trim().to_ascii_uppercase();
    let timeframe = event.timeframe.trim().to_ascii_lowercase();
    let period_timestamp = event.period_timestamp;
    let Some(condition_id) = event
        .condition_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Ok(());
    };
    let Some(token_id) = event
        .token_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Ok(());
    };
    let token_type = event.token_type.as_deref();
    let side = normalize_side(event.side.as_deref().unwrap_or("BUY"));
    let position_key = position_key(strategy_id, timeframe.as_str(), period_timestamp, token_id);
    let ts_ms = event.ts_ms;
    let apply_reconciled_monetary = should_apply_reconciled_fill_monetary(conn, event)?;

    if event_type == "ENTRY_FILL"
        || (event_type == "ENTRY_FILL_RECONCILED" && apply_reconciled_monetary)
    {
        let fill_key = event.event_key.clone().unwrap_or_else(|| {
            format!(
                "v2_entry_fill:{}:{}:{}:{}",
                event_type, period_timestamp, token_id, ts_ms
            )
        });
        conn.execute(
            r#"
INSERT INTO fills_v2 (
    event_key, ts_ms, period_timestamp, timeframe, strategy_id, condition_id, token_id, token_type,
    side, price, units, notional_usd, fee_usd, source_event_type, created_at_ms
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0.0, ?13, ?14)
ON CONFLICT(event_key) DO NOTHING
"#,
            params![
                fill_key,
                ts_ms,
                period_timestamp as i64,
                timeframe,
                strategy_id,
                condition_id,
                token_id,
                token_type,
                side,
                event.price,
                event.units,
                event.notional_usd,
                event_type,
                ts_ms
            ],
        )?;

        let entry_units = event.units.unwrap_or(0.0).max(0.0);
        let entry_notional_usd = event.notional_usd.unwrap_or(0.0).max(0.0);
        conn.execute(
            r#"
INSERT INTO positions_v2 (
    position_key, strategy_id, timeframe, period_timestamp, condition_id, token_id, token_type,
    entry_units, entry_notional_usd, entry_fee_usd,
    exit_units, exit_notional_usd, exit_fee_usd,
    settled_payout_usd, realized_pnl_usd, status, opened_at_ms, updated_at_ms
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 'OPEN', ?10, ?11)
ON CONFLICT(position_key) DO UPDATE SET
    token_type=COALESCE(positions_v2.token_type, excluded.token_type),
    entry_units=positions_v2.entry_units + excluded.entry_units,
    entry_notional_usd=positions_v2.entry_notional_usd + excluded.entry_notional_usd,
    entry_fee_usd=positions_v2.entry_fee_usd + excluded.entry_fee_usd,
    status=CASE WHEN positions_v2.status='SETTLED' THEN 'SETTLED' ELSE 'OPEN' END,
    updated_at_ms=excluded.updated_at_ms
"#,
            params![
                position_key,
                strategy_id,
                timeframe,
                period_timestamp as i64,
                condition_id,
                token_id,
                token_type,
                entry_units,
                entry_notional_usd,
                ts_ms,
                ts_ms
            ],
        )?;
        upsert_trade_lifecycle_entry_fill(
            conn,
            event,
            strategy_id,
            timeframe.as_str(),
            period_timestamp,
            condition_id,
            token_id,
            token_type,
            side.as_str(),
            ts_ms,
        )?;
    } else if event_type == "EXIT"
        || event_type == "EXIT_SIGNAL"
        || event_type == "DISTRIBUTION_EXIT_FILLED"
    {
        let exit_units = event.units.unwrap_or(0.0).max(0.0);
        let exit_notional_usd = event.notional_usd.unwrap_or(0.0).max(0.0);
        let exit_pnl = event.pnl_usd.unwrap_or(0.0);
        conn.execute(
            r#"
INSERT INTO positions_v2 (
    position_key, strategy_id, timeframe, period_timestamp, condition_id, token_id, token_type,
    entry_units, entry_notional_usd, entry_fee_usd,
    exit_units, exit_notional_usd, exit_fee_usd,
    settled_payout_usd, realized_pnl_usd, status, opened_at_ms, updated_at_ms
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0.0, 0.0, 0.0, ?8, ?9, 0.0, 0.0, ?10, 'CLOSED', ?11, ?12)
ON CONFLICT(position_key) DO UPDATE SET
    token_type=COALESCE(positions_v2.token_type, excluded.token_type),
    exit_units=positions_v2.exit_units + excluded.exit_units,
    exit_notional_usd=positions_v2.exit_notional_usd + excluded.exit_notional_usd,
    exit_fee_usd=positions_v2.exit_fee_usd + excluded.exit_fee_usd,
    realized_pnl_usd=positions_v2.realized_pnl_usd + excluded.realized_pnl_usd,
    status=CASE
        WHEN positions_v2.status='SETTLED' THEN 'SETTLED'
        WHEN (
            positions_v2.exit_units
            + COALESCE(positions_v2.inventory_consumed_units, 0.0)
            + excluded.exit_units
        ) >= positions_v2.entry_units - 1e-9
             AND positions_v2.entry_units > 0
            THEN 'CLOSED'
        ELSE 'OPEN'
    END,
    updated_at_ms=excluded.updated_at_ms
"#,
            params![
                position_key,
                strategy_id,
                timeframe,
                period_timestamp as i64,
                condition_id,
                token_id,
                token_type,
                exit_units,
                exit_notional_usd,
                exit_pnl,
                ts_ms,
                ts_ms
            ],
        )?;

        if event_type == "EXIT" && is_market_close_reason(event.reason.as_deref()) {
            upsert_trade_lifecycle_market_close(
                conn,
                event,
                strategy_id,
                timeframe.as_str(),
                period_timestamp,
                condition_id,
                token_id,
                token_type,
                side.as_str(),
                ts_ms,
            )?;
            let entry_cost_from_position: Option<f64> = conn
                .query_row(
                    "SELECT entry_notional_usd + entry_fee_usd FROM positions_v2 WHERE position_key=?1",
                    params![position_key],
                    |row| row.get(0),
                )
                .optional()?;
            let derived_cost = match (event.notional_usd, event.pnl_usd) {
                (Some(notional), Some(pnl)) => (notional - pnl).max(0.0),
                _ => 0.0,
            };
            let entry_cost_usd = entry_cost_from_position.unwrap_or(derived_cost).max(0.0);
            let payout_usd = event.notional_usd.unwrap_or(0.0).max(0.0);
            let total_cost_basis_usd = entry_cost_usd;
            let settled_pnl_usd = payout_usd - total_cost_basis_usd;
            let outcome_label = if settled_pnl_usd > 0.0 {
                "WIN"
            } else if settled_pnl_usd < 0.0 {
                "LOSS"
            } else {
                "FLAT"
            };
            let settlement_key = event.event_key.clone().unwrap_or_else(|| {
                format!("v2_settlement:{}:{}:{}", period_timestamp, token_id, ts_ms)
            });
            conn.execute(
                r#"
INSERT INTO settlements_v2 (
    settlement_key, ts_ms, position_key, period_timestamp, timeframe, strategy_id, condition_id, token_id,
    token_type, units, entry_cost_usd, payout_usd, fee_usd, total_cost_basis_usd, settled_pnl_usd,
    outcome_label, source_event_key, created_at_ms
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0.0, ?13, ?14, ?15, ?16, ?17)
ON CONFLICT(settlement_key) DO NOTHING
"#,
                params![
                    settlement_key,
                    ts_ms,
                    position_key,
                    period_timestamp as i64,
                    timeframe,
                    strategy_id,
                    condition_id,
                    token_id,
                    token_type,
                    event.units.unwrap_or(0.0).max(0.0),
                    entry_cost_usd,
                    payout_usd,
                    total_cost_basis_usd,
                    settled_pnl_usd,
                    outcome_label,
                    event.event_key,
                    ts_ms
                ],
            )?;
            conn.execute(
                r#"
UPDATE positions_v2
SET
    status='SETTLED',
    settled_payout_usd=?2,
    realized_pnl_usd=?3,
    settled_at_ms=?4,
    updated_at_ms=?4,
    exit_units=MAX(exit_units, ?5),
    exit_notional_usd=MAX(exit_notional_usd, ?6)
WHERE position_key=?1
"#,
                params![
                    position_key,
                    payout_usd,
                    settled_pnl_usd,
                    ts_ms,
                    event.units.unwrap_or(0.0).max(0.0),
                    payout_usd
                ],
            )?;
        }
    } else {
        return Ok(());
    }

    if let Some(price) = event.price.filter(|v| v.is_finite() && *v > 0.0) {
        let mark_key = event.event_key.clone().unwrap_or_else(|| {
            format!(
                "v2_mark:{}:{}:{}:{}",
                event_type, period_timestamp, token_id, ts_ms
            )
        });
        conn.execute(
            r#"
INSERT INTO marks_v2 (
    mark_key, ts_ms, position_key, period_timestamp, timeframe, strategy_id, condition_id, token_id,
    price, units, notional_usd, source_event_type, created_at_ms
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
ON CONFLICT(mark_key) DO NOTHING
"#,
            params![
                mark_key,
                ts_ms,
                position_key,
                period_timestamp as i64,
                timeframe,
                strategy_id,
                condition_id,
                token_id,
                price,
                event.units,
                event.notional_usd,
                event_type,
                ts_ms
            ],
        )?;
    }

    Ok(())
}

fn migrate_strategy_id_schema(conn: &mut Connection) -> Result<()> {
    let strategy_decisions_has_strategy_id =
        table_has_column(conn, "strategy_decisions", "strategy_id")?;
    let strategy_decision_contexts_has_strategy_id =
        table_has_column(conn, "strategy_decision_contexts", "strategy_id")?;
    let skipped_markets_has_strategy_id = table_has_column(conn, "skipped_markets", "strategy_id")?;
    let pending_orders_has_strategy_id = table_has_column(conn, "pending_orders", "strategy_id")?;
    let pending_orders_has_condition_id = table_has_column(conn, "pending_orders", "condition_id")?;
    let pending_orders_has_entry_mode = table_has_column(conn, "pending_orders", "entry_mode")?;
    let trade_events_has_strategy_id = table_has_column(conn, "trade_events", "strategy_id")?;

    let strategy_decisions_strategy_expr =
        strategy_id_select_expr(strategy_decisions_has_strategy_id);
    let strategy_decision_contexts_strategy_expr =
        strategy_id_select_expr(strategy_decision_contexts_has_strategy_id);
    let skipped_markets_strategy_expr = strategy_id_select_expr(skipped_markets_has_strategy_id);
    let pending_orders_strategy_expr = strategy_id_select_expr(pending_orders_has_strategy_id);
    let pending_orders_entry_mode_expr = if pending_orders_has_entry_mode {
        "UPPER(TRIM(COALESCE(entry_mode, 'LEGACY')))"
    } else {
        "'LEGACY'"
    };
    let pending_orders_condition_expr = if pending_orders_has_condition_id {
        "NULLIF(TRIM(condition_id), '')"
    } else {
        "NULL"
    };
    let trade_events_strategy_expr = strategy_id_select_expr(trade_events_has_strategy_id);

    let tx = conn.transaction()?;

    tx.execute_batch(
        r#"
DROP TABLE IF EXISTS strategy_decisions_new;
CREATE TABLE strategy_decisions_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    period_timestamp INTEGER NOT NULL,
    timeframe TEXT NOT NULL,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    decided_at_ms INTEGER NOT NULL,
    action TEXT NOT NULL,
    direction TEXT,
    confidence REAL NOT NULL,
    source TEXT,
    mode TEXT,
    reasoning TEXT
);
"#,
    )?;
    tx.execute_batch(
        format!(
            r#"
INSERT INTO strategy_decisions_new (
    id, period_timestamp, timeframe, strategy_id, decided_at_ms, action, direction, confidence, source, mode, reasoning
)
SELECT
    id,
    period_timestamp,
    timeframe,
    strategy_id_norm,
    decided_at_ms,
    action,
    direction,
    confidence,
    source,
    mode,
    reasoning
FROM (
    SELECT
        id,
        period_timestamp,
        timeframe,
        {strategy_expr} AS strategy_id_norm,
        decided_at_ms,
        action,
        direction,
        confidence,
        source,
        mode,
        reasoning,
        ROW_NUMBER() OVER (
            PARTITION BY period_timestamp, timeframe, {strategy_expr}
            ORDER BY decided_at_ms DESC, id DESC
        ) AS rn
    FROM strategy_decisions
) ranked
WHERE rn = 1
ORDER BY id;
"#,
            strategy_expr = strategy_decisions_strategy_expr
        )
        .as_str(),
    )?;
    tx.execute_batch(
        r#"
DROP TABLE strategy_decisions;
ALTER TABLE strategy_decisions_new RENAME TO strategy_decisions;
"#,
    )?;

    tx.execute_batch(
        r#"
DROP TABLE IF EXISTS strategy_decision_contexts_new;
CREATE TABLE strategy_decision_contexts_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    period_timestamp INTEGER NOT NULL,
    timeframe TEXT NOT NULL,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    decided_at_ms INTEGER NOT NULL,
    source TEXT NOT NULL,
    mode TEXT NOT NULL,
    context_json TEXT NOT NULL,
    signal_digest_json TEXT NOT NULL,
    parse_error TEXT,
    created_at_ms INTEGER NOT NULL
);
"#,
    )?;
    tx.execute_batch(
        format!(
            r#"
INSERT INTO strategy_decision_contexts_new (
    id, period_timestamp, timeframe, strategy_id, decided_at_ms, source, mode,
    context_json, signal_digest_json, parse_error, created_at_ms
)
SELECT
    id,
    period_timestamp,
    timeframe,
    strategy_id_norm,
    decided_at_ms,
    source,
    mode,
    context_json,
    signal_digest_json,
    parse_error,
    created_at_ms
FROM (
    SELECT
        id,
        period_timestamp,
        timeframe,
        {strategy_expr} AS strategy_id_norm,
        decided_at_ms,
        source,
        mode,
        context_json,
        signal_digest_json,
        parse_error,
        created_at_ms,
        ROW_NUMBER() OVER (
            PARTITION BY period_timestamp, timeframe, {strategy_expr}
            ORDER BY decided_at_ms DESC, created_at_ms DESC, id DESC
        ) AS rn
    FROM strategy_decision_contexts
) ranked
WHERE rn = 1
ORDER BY id;
"#,
            strategy_expr = strategy_decision_contexts_strategy_expr
        )
        .as_str(),
    )?;
    tx.execute_batch(
        r#"
DROP TABLE strategy_decision_contexts;
ALTER TABLE strategy_decision_contexts_new RENAME TO strategy_decision_contexts;
"#,
    )?;

    tx.execute_batch(
        r#"
DROP TABLE IF EXISTS skipped_markets_new;
CREATE TABLE skipped_markets_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    period_timestamp INTEGER NOT NULL,
    timeframe TEXT NOT NULL,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    decided_at_ms INTEGER NOT NULL,
    reason TEXT NOT NULL,
    source TEXT,
    confidence REAL NOT NULL,
    outcome_recorded INTEGER NOT NULL DEFAULT 0,
    outcome_label TEXT,
    outcome_pnl_usd REAL,
    resolved_at_ms INTEGER
);
"#,
    )?;
    tx.execute_batch(
        format!(
            r#"
INSERT INTO skipped_markets_new (
    id, period_timestamp, timeframe, strategy_id, decided_at_ms, reason, source, confidence,
    outcome_recorded, outcome_label, outcome_pnl_usd, resolved_at_ms
)
SELECT
    id,
    period_timestamp,
    timeframe,
    strategy_id_norm,
    decided_at_ms,
    reason,
    source,
    confidence,
    outcome_recorded,
    outcome_label,
    outcome_pnl_usd,
    resolved_at_ms
FROM (
    SELECT
        id,
        period_timestamp,
        timeframe,
        {strategy_expr} AS strategy_id_norm,
        decided_at_ms,
        reason,
        source,
        confidence,
        outcome_recorded,
        outcome_label,
        outcome_pnl_usd,
        resolved_at_ms,
        ROW_NUMBER() OVER (
            PARTITION BY period_timestamp, timeframe, {strategy_expr}
            ORDER BY decided_at_ms DESC, id DESC
        ) AS rn
    FROM skipped_markets
) ranked
WHERE rn = 1
ORDER BY id;
"#,
            strategy_expr = skipped_markets_strategy_expr
        )
        .as_str(),
    )?;
    tx.execute_batch(
        r#"
DROP TABLE skipped_markets;
ALTER TABLE skipped_markets_new RENAME TO skipped_markets;
"#,
    )?;

    tx.execute_batch(
        r#"
DROP TABLE IF EXISTS pending_orders_new;
CREATE TABLE pending_orders_new (
    order_id TEXT PRIMARY KEY,
    trade_key TEXT NOT NULL,
    token_id TEXT NOT NULL,
    condition_id TEXT,
    timeframe TEXT NOT NULL,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    entry_mode TEXT NOT NULL DEFAULT 'LEGACY',
    period_timestamp INTEGER NOT NULL,
    price REAL NOT NULL,
    size_usd REAL NOT NULL,
    side TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
"#,
    )?;
    tx.execute_batch(
        format!(
            r#"
INSERT INTO pending_orders_new (
    order_id, trade_key, token_id, condition_id, timeframe, strategy_id, entry_mode, period_timestamp,
    price, size_usd, side, status, created_at_ms, updated_at_ms
)
SELECT
    order_id,
    trade_key,
    token_id,
    {condition_id_expr},
    timeframe,
    {strategy_expr},
    {entry_mode_expr},
    period_timestamp,
    price,
    size_usd,
    UPPER(TRIM(side)),
    UPPER(TRIM(status)),
    created_at_ms,
    updated_at_ms
FROM pending_orders;
        "#,
            strategy_expr = pending_orders_strategy_expr,
            entry_mode_expr = pending_orders_entry_mode_expr,
            condition_id_expr = pending_orders_condition_expr
        )
        .as_str(),
    )?;
    tx.execute_batch(
        r#"
DROP TABLE pending_orders;
ALTER TABLE pending_orders_new RENAME TO pending_orders;
"#,
    )?;

    tx.execute_batch(
        r#"
DROP TABLE IF EXISTS trade_events_new;
CREATE TABLE trade_events_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_key TEXT UNIQUE,
    ts_ms INTEGER NOT NULL,
    period_timestamp INTEGER NOT NULL,
    timeframe TEXT NOT NULL,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    condition_id TEXT,
    token_id TEXT,
    token_type TEXT,
    side TEXT,
    event_type TEXT NOT NULL,
    price REAL,
    units REAL,
    notional_usd REAL,
    pnl_usd REAL,
    reason TEXT
);
"#,
    )?;
    tx.execute_batch(
        format!(
            r#"
INSERT INTO trade_events_new (
    id, event_key, ts_ms, period_timestamp, timeframe, strategy_id,
    condition_id, token_id, token_type, side, event_type, price, units, notional_usd, pnl_usd, reason
)
SELECT
    id,
    event_key,
    ts_ms,
    period_timestamp,
    timeframe,
    {strategy_expr},
    condition_id,
    token_id,
    token_type,
    side,
    event_type,
    price,
    units,
    notional_usd,
    pnl_usd,
    reason
FROM trade_events
ORDER BY id;
"#,
            strategy_expr = trade_events_strategy_expr
        )
        .as_str(),
    )?;
    tx.execute_batch(
        r#"
DROP TABLE trade_events;
ALTER TABLE trade_events_new RENAME TO trade_events;
"#,
    )?;

    tx.execute_batch(
        r#"
CREATE UNIQUE INDEX IF NOT EXISTS uidx_strategy_decisions_period_tf_strategy
    ON strategy_decisions(period_timestamp, timeframe, strategy_id);
CREATE UNIQUE INDEX IF NOT EXISTS uidx_strategy_decision_contexts_period_tf_strategy
    ON strategy_decision_contexts(period_timestamp, timeframe, strategy_id);
CREATE UNIQUE INDEX IF NOT EXISTS uidx_skipped_markets_period_tf_strategy
    ON skipped_markets(period_timestamp, timeframe, strategy_id);

CREATE INDEX IF NOT EXISTS idx_trade_events_period_tf ON trade_events(period_timestamp, timeframe);
CREATE INDEX IF NOT EXISTS idx_trade_events_type_tf ON trade_events(event_type, timeframe);
CREATE INDEX IF NOT EXISTS idx_trade_events_ts_ms ON trade_events(ts_ms DESC);
CREATE INDEX IF NOT EXISTS idx_trade_events_strategy_ts ON trade_events(strategy_id, ts_ms DESC);

CREATE INDEX IF NOT EXISTS idx_pending_orders_tf_period_status
    ON pending_orders(timeframe, period_timestamp, status);
CREATE INDEX IF NOT EXISTS idx_pending_orders_token_status
    ON pending_orders(token_id, status);
CREATE INDEX IF NOT EXISTS idx_pending_orders_strategy_period_status
    ON pending_orders(strategy_id, period_timestamp, status);
CREATE UNIQUE INDEX IF NOT EXISTS uidx_parameter_suggestions_strategy_name_status
    ON parameter_suggestions(strategy_id, name, status);
CREATE UNIQUE INDEX IF NOT EXISTS uidx_outcome_reflections_strategy_closed
    ON outcome_reflections(strategy_id, closed_trade_count);
CREATE INDEX IF NOT EXISTS idx_runtime_parameter_events_name_time
    ON runtime_parameter_events(strategy_id, name, applied_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_parameter_suggestion_history_name_time
    ON parameter_suggestion_history(strategy_id, name, archived_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_strategy_decisions_period_tf
    ON strategy_decisions(period_timestamp, timeframe);
CREATE INDEX IF NOT EXISTS idx_strategy_decision_contexts_period_tf
    ON strategy_decision_contexts(period_timestamp, timeframe);
CREATE INDEX IF NOT EXISTS idx_skipped_markets_period_tf
    ON skipped_markets(period_timestamp, timeframe);
"#,
    )?;

    tx.commit()?;
    Ok(())
}

fn migrate_outcome_scope_schema(conn: &mut Connection) -> Result<()> {
    let outcome_reflections_has_strategy_id =
        table_has_column(conn, "outcome_reflections", "strategy_id")?;
    let parameter_suggestions_has_strategy_id =
        table_has_column(conn, "parameter_suggestions", "strategy_id")?;
    let parameter_suggestion_history_has_strategy_id =
        table_has_column(conn, "parameter_suggestion_history", "strategy_id")?;
    let runtime_parameter_state_has_strategy_id =
        table_has_column(conn, "runtime_parameter_state", "strategy_id")?;
    let runtime_parameter_events_has_strategy_id =
        table_has_column(conn, "runtime_parameter_events", "strategy_id")?;

    let outcome_reflections_strategy_expr =
        strategy_id_select_expr(outcome_reflections_has_strategy_id);
    let parameter_suggestions_strategy_expr =
        strategy_id_select_expr(parameter_suggestions_has_strategy_id);
    let parameter_suggestion_history_strategy_expr =
        strategy_id_select_expr(parameter_suggestion_history_has_strategy_id);
    let runtime_parameter_state_strategy_expr =
        strategy_id_select_expr(runtime_parameter_state_has_strategy_id);
    let runtime_parameter_events_strategy_expr =
        strategy_id_select_expr(runtime_parameter_events_has_strategy_id);

    let tx = conn.transaction()?;

    tx.execute_batch(
        r#"
DROP TABLE IF EXISTS outcome_reflections_new;
CREATE TABLE outcome_reflections_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    closed_trade_count INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    source TEXT NOT NULL,
    summary_json TEXT NOT NULL,
    reflection_note TEXT,
    UNIQUE(strategy_id, closed_trade_count)
);
"#,
    )?;
    tx.execute_batch(
        format!(
            r#"
INSERT INTO outcome_reflections_new (
    id, strategy_id, closed_trade_count, created_at_ms, source, summary_json, reflection_note
)
SELECT
    id,
    strategy_id_norm,
    closed_trade_count,
    created_at_ms,
    source,
    summary_json,
    reflection_note
FROM (
    SELECT
        id,
        {strategy_expr} AS strategy_id_norm,
        closed_trade_count,
        created_at_ms,
        source,
        summary_json,
        reflection_note,
        ROW_NUMBER() OVER (
            PARTITION BY {strategy_expr}, closed_trade_count
            ORDER BY created_at_ms DESC, id DESC
        ) AS rn
    FROM outcome_reflections
) ranked
WHERE rn = 1
ORDER BY id;
"#,
            strategy_expr = outcome_reflections_strategy_expr
        )
        .as_str(),
    )?;
    tx.execute_batch(
        r#"
DROP TABLE outcome_reflections;
ALTER TABLE outcome_reflections_new RENAME TO outcome_reflections;
"#,
    )?;

    tx.execute_batch(
        r#"
DROP TABLE IF EXISTS parameter_suggestions_new;
CREATE TABLE parameter_suggestions_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    name TEXT NOT NULL,
    current_value REAL NOT NULL,
    suggested_value REAL NOT NULL,
    min_value REAL,
    max_value REAL,
    confidence REAL NOT NULL,
    reason TEXT NOT NULL,
    source TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    approved_by TEXT,
    approved_at_ms INTEGER,
    applied_at_ms INTEGER,
    rollback_at_ms INTEGER,
    apply_error TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(strategy_id, name, status)
);
"#,
    )?;
    tx.execute_batch(
        format!(
            r#"
INSERT INTO parameter_suggestions_new (
    id, strategy_id, name, current_value, suggested_value, min_value, max_value, confidence,
    reason, source, status, approved_by, approved_at_ms, applied_at_ms, rollback_at_ms,
    apply_error, created_at_ms, updated_at_ms
)
SELECT
    id,
    strategy_id_norm,
    name,
    current_value,
    suggested_value,
    min_value,
    max_value,
    confidence,
    reason,
    source,
    status,
    approved_by,
    approved_at_ms,
    applied_at_ms,
    rollback_at_ms,
    apply_error,
    created_at_ms,
    updated_at_ms
FROM (
    SELECT
        id,
        {strategy_expr} AS strategy_id_norm,
        name,
        current_value,
        suggested_value,
        min_value,
        max_value,
        confidence,
        reason,
        source,
        status,
        approved_by,
        approved_at_ms,
        applied_at_ms,
        rollback_at_ms,
        apply_error,
        created_at_ms,
        updated_at_ms,
        ROW_NUMBER() OVER (
            PARTITION BY {strategy_expr}, name, status
            ORDER BY updated_at_ms DESC, id DESC
        ) AS rn
    FROM parameter_suggestions
) ranked
WHERE rn = 1
ORDER BY id;
"#,
            strategy_expr = parameter_suggestions_strategy_expr
        )
        .as_str(),
    )?;
    tx.execute_batch(
        r#"
DROP TABLE parameter_suggestions;
ALTER TABLE parameter_suggestions_new RENAME TO parameter_suggestions;
"#,
    )?;

    tx.execute_batch(
        r#"
DROP TABLE IF EXISTS parameter_suggestion_history_new;
CREATE TABLE parameter_suggestion_history_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    suggestion_id INTEGER,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    name TEXT NOT NULL,
    previous_status TEXT NOT NULL,
    current_value REAL NOT NULL,
    suggested_value REAL NOT NULL,
    min_value REAL,
    max_value REAL,
    confidence REAL NOT NULL,
    reason TEXT NOT NULL,
    source TEXT NOT NULL,
    archived_at_ms INTEGER NOT NULL
);
"#,
    )?;
    tx.execute_batch(
        format!(
            r#"
INSERT INTO parameter_suggestion_history_new (
    id, suggestion_id, strategy_id, name, previous_status, current_value, suggested_value,
    min_value, max_value, confidence, reason, source, archived_at_ms
)
SELECT
    id,
    suggestion_id,
    {strategy_expr},
    name,
    previous_status,
    current_value,
    suggested_value,
    min_value,
    max_value,
    confidence,
    reason,
    source,
    archived_at_ms
FROM parameter_suggestion_history
ORDER BY id;
"#,
            strategy_expr = parameter_suggestion_history_strategy_expr
        )
        .as_str(),
    )?;
    tx.execute_batch(
        r#"
DROP TABLE parameter_suggestion_history;
ALTER TABLE parameter_suggestion_history_new RENAME TO parameter_suggestion_history;
"#,
    )?;

    tx.execute_batch(
        r#"
DROP TABLE IF EXISTS runtime_parameter_state_new;
CREATE TABLE runtime_parameter_state_new (
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    name TEXT NOT NULL,
    value_json TEXT NOT NULL,
    version INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    updated_by TEXT NOT NULL,
    PRIMARY KEY(strategy_id, name)
);
"#,
    )?;
    tx.execute_batch(
        format!(
            r#"
INSERT INTO runtime_parameter_state_new (
    strategy_id, name, value_json, version, updated_at_ms, updated_by
)
SELECT
    {strategy_expr},
    name,
    value_json,
    version,
    updated_at_ms,
    updated_by
FROM runtime_parameter_state
ORDER BY name;
"#,
            strategy_expr = runtime_parameter_state_strategy_expr
        )
        .as_str(),
    )?;
    tx.execute_batch(
        r#"
DROP TABLE runtime_parameter_state;
ALTER TABLE runtime_parameter_state_new RENAME TO runtime_parameter_state;
"#,
    )?;

    tx.execute_batch(
        r#"
DROP TABLE IF EXISTS runtime_parameter_events_new;
CREATE TABLE runtime_parameter_events_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    name TEXT NOT NULL,
    old_value_json TEXT,
    new_value_json TEXT NOT NULL,
    reason TEXT NOT NULL,
    source_suggestion_id INTEGER,
    applied_at_ms INTEGER NOT NULL,
    applied_by TEXT NOT NULL
);
"#,
    )?;
    tx.execute_batch(
        format!(
            r#"
INSERT INTO runtime_parameter_events_new (
    id, strategy_id, name, old_value_json, new_value_json, reason, source_suggestion_id,
    applied_at_ms, applied_by
)
SELECT
    id,
    {strategy_expr},
    name,
    old_value_json,
    new_value_json,
    reason,
    source_suggestion_id,
    applied_at_ms,
    applied_by
FROM runtime_parameter_events
ORDER BY id;
"#,
            strategy_expr = runtime_parameter_events_strategy_expr
        )
        .as_str(),
    )?;
    tx.execute_batch(
        r#"
DROP TABLE runtime_parameter_events;
ALTER TABLE runtime_parameter_events_new RENAME TO runtime_parameter_events;
"#,
    )?;

    tx.execute_batch(
        r#"
CREATE INDEX IF NOT EXISTS idx_runtime_parameter_events_name_time
    ON runtime_parameter_events(strategy_id, name, applied_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_parameter_suggestion_history_name_time
    ON parameter_suggestion_history(strategy_id, name, archived_at_ms DESC);
"#,
    )?;

    tx.commit()?;
    Ok(())
}

fn migrate_market_dimension_schema(conn: &mut Connection) -> Result<()> {
    let strategy_decisions_has_market_key =
        table_has_column(conn, "strategy_decisions", "market_key")?;
    let strategy_decisions_has_asset_symbol =
        table_has_column(conn, "strategy_decisions", "asset_symbol")?;
    let strategy_decision_contexts_has_market_key =
        table_has_column(conn, "strategy_decision_contexts", "market_key")?;
    let strategy_decision_contexts_has_asset_symbol =
        table_has_column(conn, "strategy_decision_contexts", "asset_symbol")?;
    let skipped_markets_has_market_key = table_has_column(conn, "skipped_markets", "market_key")?;
    let skipped_markets_has_asset_symbol =
        table_has_column(conn, "skipped_markets", "asset_symbol")?;
    let pending_orders_has_asset_symbol = table_has_column(conn, "pending_orders", "asset_symbol")?;
    let trade_events_has_asset_symbol = table_has_column(conn, "trade_events", "asset_symbol")?;

    let tx = conn.transaction()?;

    if !strategy_decisions_has_market_key {
        tx.execute(
            "ALTER TABLE strategy_decisions ADD COLUMN market_key TEXT NOT NULL DEFAULT 'btc'",
            [],
        )?;
    }
    if !strategy_decisions_has_asset_symbol {
        tx.execute(
            "ALTER TABLE strategy_decisions ADD COLUMN asset_symbol TEXT NOT NULL DEFAULT 'BTC'",
            [],
        )?;
    }
    if !strategy_decision_contexts_has_market_key {
        tx.execute(
            "ALTER TABLE strategy_decision_contexts ADD COLUMN market_key TEXT NOT NULL DEFAULT 'btc'",
            [],
        )?;
    }
    if !strategy_decision_contexts_has_asset_symbol {
        tx.execute(
            "ALTER TABLE strategy_decision_contexts ADD COLUMN asset_symbol TEXT NOT NULL DEFAULT 'BTC'",
            [],
        )?;
    }
    if !skipped_markets_has_market_key {
        tx.execute(
            "ALTER TABLE skipped_markets ADD COLUMN market_key TEXT NOT NULL DEFAULT 'btc'",
            [],
        )?;
    }
    if !skipped_markets_has_asset_symbol {
        tx.execute(
            "ALTER TABLE skipped_markets ADD COLUMN asset_symbol TEXT NOT NULL DEFAULT 'BTC'",
            [],
        )?;
    }
    if !pending_orders_has_asset_symbol {
        tx.execute(
            "ALTER TABLE pending_orders ADD COLUMN asset_symbol TEXT NOT NULL DEFAULT 'BTC'",
            [],
        )?;
    }
    if !trade_events_has_asset_symbol {
        tx.execute(
            "ALTER TABLE trade_events ADD COLUMN asset_symbol TEXT NOT NULL DEFAULT 'BTC'",
            [],
        )?;
    }

    tx.execute(
        r#"
UPDATE strategy_decisions
SET market_key = LOWER(TRIM(COALESCE(NULLIF(market_key,''), NULLIF(asset_symbol,''), 'btc'))),
    asset_symbol = UPPER(TRIM(COALESCE(NULLIF(asset_symbol,''), NULLIF(market_key,''), 'BTC')))
"#,
        [],
    )?;
    tx.execute(
        r#"
UPDATE strategy_decision_contexts
SET market_key = LOWER(TRIM(COALESCE(NULLIF(market_key,''), NULLIF(asset_symbol,''), 'btc'))),
    asset_symbol = UPPER(TRIM(COALESCE(NULLIF(asset_symbol,''), NULLIF(market_key,''), 'BTC')))
"#,
        [],
    )?;
    tx.execute(
        r#"
UPDATE skipped_markets
SET market_key = LOWER(TRIM(COALESCE(NULLIF(market_key,''), NULLIF(asset_symbol,''), 'btc'))),
    asset_symbol = UPPER(TRIM(COALESCE(NULLIF(asset_symbol,''), NULLIF(market_key,''), 'BTC')))
"#,
        [],
    )?;
    tx.execute(
        r#"
UPDATE pending_orders
SET asset_symbol = UPPER(TRIM(COALESCE(NULLIF(asset_symbol,''), 'BTC')))
"#,
        [],
    )?;
    tx.execute(
        r#"
UPDATE trade_events
SET asset_symbol = UPPER(TRIM(COALESCE(
        NULLIF(asset_symbol,''),
        CASE
            WHEN UPPER(COALESCE(token_type,'')) LIKE 'ETH %' THEN 'ETH'
            WHEN UPPER(COALESCE(token_type,'')) LIKE 'SOL %' THEN 'SOL'
            WHEN UPPER(COALESCE(token_type,'')) LIKE 'XRP %' THEN 'XRP'
            WHEN UPPER(COALESCE(token_type,'')) LIKE 'BTC %' THEN 'BTC'
            ELSE 'BTC'
        END
    )))
"#,
        [],
    )?;

    tx.execute_batch(
        r#"
DROP INDEX IF EXISTS uidx_strategy_decisions_period_tf_strategy;
DROP INDEX IF EXISTS uidx_strategy_decision_contexts_period_tf_strategy;
DROP INDEX IF EXISTS uidx_skipped_markets_period_tf_strategy;
DROP INDEX IF EXISTS uidx_strategy_decisions_period_tf_strategy_market;
DROP INDEX IF EXISTS uidx_strategy_decision_contexts_period_tf_strategy_market;
DROP INDEX IF EXISTS uidx_skipped_markets_period_tf_strategy_market;
DROP INDEX IF EXISTS idx_trade_events_strategy_asset_ts;
DROP INDEX IF EXISTS idx_pending_orders_strategy_asset_period_status;

CREATE UNIQUE INDEX IF NOT EXISTS uidx_strategy_decisions_period_tf_strategy_market
    ON strategy_decisions(period_timestamp, timeframe, strategy_id, market_key);
CREATE UNIQUE INDEX IF NOT EXISTS uidx_strategy_decision_contexts_period_tf_strategy_market
    ON strategy_decision_contexts(period_timestamp, timeframe, strategy_id, market_key);
CREATE UNIQUE INDEX IF NOT EXISTS uidx_skipped_markets_period_tf_strategy_market
    ON skipped_markets(period_timestamp, timeframe, strategy_id, market_key);
CREATE INDEX IF NOT EXISTS idx_trade_events_strategy_asset_ts
    ON trade_events(strategy_id, asset_symbol, ts_ms DESC);
CREATE INDEX IF NOT EXISTS idx_pending_orders_strategy_asset_period_status
    ON pending_orders(strategy_id, asset_symbol, period_timestamp, status);
"#,
    )?;

    tx.commit()?;
    Ok(())
}

impl TrackingDb {
    fn effective_reporting_scope(scope: Option<&ReportingScope>) -> ReportingScope {
        scope.cloned().unwrap_or_else(ReportingScope::from_env)
    }

    pub fn reporting_scope_from_env() -> ReportingScope {
        ReportingScope::from_env()
    }

    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let db_path = path.as_ref().to_path_buf();
        let mut conn = Connection::open(&db_path)?;
        let busy_timeout_ms = std::env::var("EVPOLY_DB_BUSY_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(5_000)
            .max(250);
        conn.busy_timeout(std::time::Duration::from_millis(busy_timeout_ms))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(
            r#"
CREATE TABLE IF NOT EXISTS trade_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_key TEXT UNIQUE,
    ts_ms INTEGER NOT NULL,
    period_timestamp INTEGER NOT NULL,
    timeframe TEXT NOT NULL,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    run_id TEXT NOT NULL DEFAULT 'legacy_run',
    strategy_cohort TEXT NOT NULL DEFAULT 'default',
    asset_symbol TEXT NOT NULL DEFAULT 'BTC',
    condition_id TEXT,
    token_id TEXT,
    token_type TEXT,
    side TEXT,
    event_type TEXT NOT NULL,
    price REAL,
    units REAL,
    notional_usd REAL,
    pnl_usd REAL,
    reason TEXT
);

CREATE INDEX IF NOT EXISTS idx_trade_events_period_tf ON trade_events(period_timestamp, timeframe);
CREATE INDEX IF NOT EXISTS idx_trade_events_type_tf ON trade_events(event_type, timeframe);
CREATE INDEX IF NOT EXISTS idx_trade_events_ts_ms ON trade_events(ts_ms DESC);

CREATE TABLE IF NOT EXISTS strategy_decisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    period_timestamp INTEGER NOT NULL,
    timeframe TEXT NOT NULL,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    run_id TEXT NOT NULL DEFAULT 'legacy_run',
    strategy_cohort TEXT NOT NULL DEFAULT 'default',
    market_key TEXT NOT NULL DEFAULT 'btc',
    asset_symbol TEXT NOT NULL DEFAULT 'BTC',
    decided_at_ms INTEGER NOT NULL,
    action TEXT NOT NULL,
    direction TEXT,
    confidence REAL NOT NULL,
    source TEXT,
    mode TEXT,
    reasoning TEXT
);

CREATE TABLE IF NOT EXISTS skipped_markets (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    period_timestamp INTEGER NOT NULL,
    timeframe TEXT NOT NULL,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    run_id TEXT NOT NULL DEFAULT 'legacy_run',
    strategy_cohort TEXT NOT NULL DEFAULT 'default',
    market_key TEXT NOT NULL DEFAULT 'btc',
    asset_symbol TEXT NOT NULL DEFAULT 'BTC',
    decided_at_ms INTEGER NOT NULL,
    reason TEXT NOT NULL,
    source TEXT,
    confidence REAL NOT NULL,
    outcome_recorded INTEGER NOT NULL DEFAULT 0,
    outcome_label TEXT,
    outcome_pnl_usd REAL,
    resolved_at_ms INTEGER
);

CREATE TABLE IF NOT EXISTS outcome_reflections (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    closed_trade_count INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    source TEXT NOT NULL,
    summary_json TEXT NOT NULL,
    reflection_note TEXT,
    UNIQUE(strategy_id, closed_trade_count)
);

CREATE TABLE IF NOT EXISTS parameter_suggestions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    name TEXT NOT NULL,
    current_value REAL NOT NULL,
    suggested_value REAL NOT NULL,
    min_value REAL,
    max_value REAL,
    confidence REAL NOT NULL,
    reason TEXT NOT NULL,
    source TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    approved_by TEXT,
    approved_at_ms INTEGER,
    applied_at_ms INTEGER,
    rollback_at_ms INTEGER,
    apply_error TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(strategy_id, name, status)
);

CREATE TABLE IF NOT EXISTS parameter_suggestion_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    suggestion_id INTEGER,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    name TEXT NOT NULL,
    previous_status TEXT NOT NULL,
    current_value REAL NOT NULL,
    suggested_value REAL NOT NULL,
    min_value REAL,
    max_value REAL,
    confidence REAL NOT NULL,
    reason TEXT NOT NULL,
    source TEXT NOT NULL,
    archived_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS strategy_decision_contexts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    period_timestamp INTEGER NOT NULL,
    timeframe TEXT NOT NULL,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    run_id TEXT NOT NULL DEFAULT 'legacy_run',
    strategy_cohort TEXT NOT NULL DEFAULT 'default',
    market_key TEXT NOT NULL DEFAULT 'btc',
    asset_symbol TEXT NOT NULL DEFAULT 'BTC',
    decided_at_ms INTEGER NOT NULL,
    source TEXT NOT NULL,
    mode TEXT NOT NULL,
    context_json TEXT NOT NULL,
    signal_digest_json TEXT NOT NULL,
    parse_error TEXT,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS runtime_parameter_state (
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    name TEXT NOT NULL,
    value_json TEXT NOT NULL,
    version INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    updated_by TEXT NOT NULL,
    PRIMARY KEY(strategy_id, name)
);

CREATE TABLE IF NOT EXISTS runtime_parameter_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    name TEXT NOT NULL,
    old_value_json TEXT,
    new_value_json TEXT NOT NULL,
    reason TEXT NOT NULL,
    source_suggestion_id INTEGER,
    applied_at_ms INTEGER NOT NULL,
    applied_by TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS pending_orders (
    order_id TEXT PRIMARY KEY,
    trade_key TEXT NOT NULL,
    token_id TEXT NOT NULL,
    condition_id TEXT,
    timeframe TEXT NOT NULL,
    strategy_id TEXT NOT NULL DEFAULT 'legacy_default',
    run_id TEXT NOT NULL DEFAULT 'legacy_run',
    strategy_cohort TEXT NOT NULL DEFAULT 'default',
    asset_symbol TEXT NOT NULL DEFAULT 'BTC',
    entry_mode TEXT NOT NULL DEFAULT 'LEGACY',
    period_timestamp INTEGER NOT NULL,
    price REAL NOT NULL,
    size_usd REAL NOT NULL,
    side TEXT NOT NULL,
    status TEXT NOT NULL,
    filled_at_ms INTEGER,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_pending_orders_tf_period_status
    ON pending_orders(timeframe, period_timestamp, status);
CREATE INDEX IF NOT EXISTS idx_pending_orders_token_status
    ON pending_orders(token_id, status);
CREATE INDEX IF NOT EXISTS idx_strategy_decisions_period_tf
    ON strategy_decisions(period_timestamp, timeframe);
CREATE INDEX IF NOT EXISTS idx_strategy_decision_contexts_period_tf
    ON strategy_decision_contexts(period_timestamp, timeframe);
CREATE INDEX IF NOT EXISTS idx_skipped_markets_period_tf
    ON skipped_markets(period_timestamp, timeframe);
CREATE INDEX IF NOT EXISTS idx_runtime_parameter_events_name_time
    ON runtime_parameter_events(strategy_id, name, applied_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_parameter_suggestion_history_name_time
    ON parameter_suggestion_history(strategy_id, name, archived_at_ms DESC);

CREATE TABLE IF NOT EXISTS snapback_feature_snapshots_v1 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    strategy_id TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    period_timestamp INTEGER NOT NULL,
    condition_id TEXT,
    token_id TEXT NOT NULL,
    impulse_id TEXT NOT NULL,
    position_key TEXT,
    entry_side TEXT,
    intent_ts_ms INTEGER NOT NULL,
    request_id TEXT,
    order_id TEXT,
    signed_notional_30s REAL,
    signed_notional_300s REAL,
    signed_notional_900s REAL,
    burst_zscore REAL,
    side_imbalance_5m REAL,
    impulse_strength REAL NOT NULL,
    trigger_signed_flow REAL,
    trigger_flow_source TEXT,
    fair_probability REAL,
    reservation_price REAL,
    max_price REAL,
    limit_price REAL,
    edge_bps REAL,
    target_size_usd REAL,
    bias_alignment TEXT,
    bias_confidence REAL,
    fill_ts_ms INTEGER,
    exit_ts_ms INTEGER,
    settled_ts_ms INTEGER,
    exit_reason TEXT,
    realized_pnl_usd REAL,
    settled_pnl_usd REAL,
    hold_ms INTEGER,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(strategy_id, timeframe, period_timestamp, token_id, impulse_id)
);

CREATE INDEX IF NOT EXISTS idx_snapback_snapshots_lookup
    ON snapback_feature_snapshots_v1(strategy_id, timeframe, period_timestamp, token_id, intent_ts_ms DESC);
CREATE INDEX IF NOT EXISTS idx_snapback_snapshots_outcomes
    ON snapback_feature_snapshots_v1(strategy_id, timeframe, fill_ts_ms, exit_ts_ms, settled_ts_ms);
CREATE INDEX IF NOT EXISTS idx_snapback_snapshots_position_key
    ON snapback_feature_snapshots_v1(position_key);
CREATE INDEX IF NOT EXISTS idx_snapback_snapshots_request_id
    ON snapback_feature_snapshots_v1(request_id);
CREATE INDEX IF NOT EXISTS idx_snapback_snapshots_order_id
    ON snapback_feature_snapshots_v1(order_id);
"#,
        )?;
        let mut schema_version: i64 =
            conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if schema_version < STRATEGY_ID_SCHEMA_VERSION {
            migrate_strategy_id_schema(&mut conn)?;
            conn.pragma_update(None, "user_version", STRATEGY_ID_SCHEMA_VERSION)?;
            schema_version = STRATEGY_ID_SCHEMA_VERSION;
        }
        ensure_column(
            &conn,
            "trade_events",
            "strategy_id",
            "TEXT NOT NULL DEFAULT 'legacy_default'",
        )?;
        ensure_column(
            &conn,
            "strategy_decisions",
            "strategy_id",
            "TEXT NOT NULL DEFAULT 'legacy_default'",
        )?;
        ensure_column(
            &conn,
            "strategy_decision_contexts",
            "strategy_id",
            "TEXT NOT NULL DEFAULT 'legacy_default'",
        )?;
        ensure_column(
            &conn,
            "skipped_markets",
            "strategy_id",
            "TEXT NOT NULL DEFAULT 'legacy_default'",
        )?;
        ensure_column(
            &conn,
            "pending_orders",
            "strategy_id",
            "TEXT NOT NULL DEFAULT 'legacy_default'",
        )?;
        ensure_column(
            &conn,
            "trade_events",
            "run_id",
            "TEXT NOT NULL DEFAULT 'legacy_run'",
        )?;
        ensure_column(
            &conn,
            "trade_events",
            "strategy_cohort",
            "TEXT NOT NULL DEFAULT 'default'",
        )?;
        ensure_column(
            &conn,
            "strategy_decisions",
            "run_id",
            "TEXT NOT NULL DEFAULT 'legacy_run'",
        )?;
        ensure_column(
            &conn,
            "strategy_decisions",
            "strategy_cohort",
            "TEXT NOT NULL DEFAULT 'default'",
        )?;
        ensure_column(
            &conn,
            "strategy_decision_contexts",
            "run_id",
            "TEXT NOT NULL DEFAULT 'legacy_run'",
        )?;
        ensure_column(
            &conn,
            "strategy_decision_contexts",
            "strategy_cohort",
            "TEXT NOT NULL DEFAULT 'default'",
        )?;
        ensure_column(
            &conn,
            "skipped_markets",
            "run_id",
            "TEXT NOT NULL DEFAULT 'legacy_run'",
        )?;
        ensure_column(
            &conn,
            "skipped_markets",
            "strategy_cohort",
            "TEXT NOT NULL DEFAULT 'default'",
        )?;
        ensure_column(
            &conn,
            "pending_orders",
            "run_id",
            "TEXT NOT NULL DEFAULT 'legacy_run'",
        )?;
        ensure_column(
            &conn,
            "pending_orders",
            "strategy_cohort",
            "TEXT NOT NULL DEFAULT 'default'",
        )?;
        ensure_column(&conn, "parameter_suggestions", "approved_by", "TEXT")?;
        ensure_column(&conn, "parameter_suggestions", "approved_at_ms", "INTEGER")?;
        ensure_column(&conn, "parameter_suggestions", "applied_at_ms", "INTEGER")?;
        ensure_column(&conn, "parameter_suggestions", "rollback_at_ms", "INTEGER")?;
        ensure_column(&conn, "parameter_suggestions", "apply_error", "TEXT")?;
        ensure_column(
            &conn,
            "outcome_reflections",
            "strategy_id",
            "TEXT NOT NULL DEFAULT 'legacy_default'",
        )?;
        ensure_column(
            &conn,
            "parameter_suggestions",
            "strategy_id",
            "TEXT NOT NULL DEFAULT 'legacy_default'",
        )?;
        ensure_column(
            &conn,
            "parameter_suggestion_history",
            "strategy_id",
            "TEXT NOT NULL DEFAULT 'legacy_default'",
        )?;
        ensure_column(
            &conn,
            "runtime_parameter_state",
            "strategy_id",
            "TEXT NOT NULL DEFAULT 'legacy_default'",
        )?;
        ensure_column(
            &conn,
            "runtime_parameter_events",
            "strategy_id",
            "TEXT NOT NULL DEFAULT 'legacy_default'",
        )?;
        if schema_version < OUTCOME_SCOPE_SCHEMA_VERSION {
            migrate_outcome_scope_schema(&mut conn)?;
            conn.pragma_update(None, "user_version", OUTCOME_SCOPE_SCHEMA_VERSION)?;
            schema_version = OUTCOME_SCOPE_SCHEMA_VERSION;
        }
        if schema_version < MARKET_DIMENSION_SCHEMA_VERSION {
            migrate_market_dimension_schema(&mut conn)?;
            conn.pragma_update(None, "user_version", MARKET_DIMENSION_SCHEMA_VERSION)?;
        }
        ensure_column(
            &conn,
            "trade_events",
            "asset_symbol",
            "TEXT NOT NULL DEFAULT 'BTC'",
        )?;
        ensure_column(
            &conn,
            "strategy_decisions",
            "market_key",
            "TEXT NOT NULL DEFAULT 'btc'",
        )?;
        ensure_column(
            &conn,
            "strategy_decisions",
            "asset_symbol",
            "TEXT NOT NULL DEFAULT 'BTC'",
        )?;
        ensure_column(
            &conn,
            "strategy_decision_contexts",
            "market_key",
            "TEXT NOT NULL DEFAULT 'btc'",
        )?;
        ensure_column(
            &conn,
            "strategy_decision_contexts",
            "asset_symbol",
            "TEXT NOT NULL DEFAULT 'BTC'",
        )?;
        ensure_column(
            &conn,
            "skipped_markets",
            "market_key",
            "TEXT NOT NULL DEFAULT 'btc'",
        )?;
        ensure_column(
            &conn,
            "skipped_markets",
            "asset_symbol",
            "TEXT NOT NULL DEFAULT 'BTC'",
        )?;
        ensure_column(
            &conn,
            "pending_orders",
            "asset_symbol",
            "TEXT NOT NULL DEFAULT 'BTC'",
        )?;
        ensure_column(
            &conn,
            "pending_orders",
            "entry_mode",
            "TEXT NOT NULL DEFAULT 'LEGACY'",
        )?;
        ensure_column(&conn, "pending_orders", "condition_id", "TEXT")?;
        ensure_column(&conn, "pending_orders", "filled_at_ms", "INTEGER")?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_pending_orders_status_filled_at ON pending_orders(status, filled_at_ms DESC)",
            [],
        )?;
        let pending_orders_terminal_prune_idx_sql = format!(
            "CREATE INDEX IF NOT EXISTS idx_pending_orders_terminal_prune ON pending_orders(updated_at_ms) WHERE status NOT IN ({})",
            ACTIVE_PENDING_ORDER_STATUSES_SQL
        );
        conn.execute(pending_orders_terminal_prune_idx_sql.as_str(), [])?;
        let run_legacy_boot_normalization = std::env::var("EVPOLY_DB_LEGACY_NORMALIZE_ON_BOOT")
            .ok()
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false);
        if run_legacy_boot_normalization {
            conn.execute(
                "UPDATE trade_events SET strategy_id=COALESCE(NULLIF(TRIM(strategy_id),''),'legacy_default')",
                [],
            )?;
            conn.execute(
                "UPDATE trade_events SET run_id=COALESCE(NULLIF(TRIM(run_id),''),'legacy_run'), strategy_cohort=LOWER(TRIM(COALESCE(NULLIF(strategy_cohort,''),'default')))",
                [],
            )?;
            conn.execute(
                "UPDATE trade_events SET asset_symbol=UPPER(TRIM(COALESCE(NULLIF(asset_symbol,''), CASE WHEN UPPER(COALESCE(token_type,'')) LIKE 'ETH %' THEN 'ETH' WHEN UPPER(COALESCE(token_type,'')) LIKE 'SOL %' THEN 'SOL' WHEN UPPER(COALESCE(token_type,'')) LIKE 'XRP %' THEN 'XRP' WHEN UPPER(COALESCE(token_type,'')) LIKE 'BTC %' THEN 'BTC' ELSE 'BTC' END)))",
                [],
            )?;
            conn.execute(
                "UPDATE strategy_decisions SET strategy_id=COALESCE(NULLIF(TRIM(strategy_id),''),'legacy_default')",
                [],
            )?;
            conn.execute(
                "UPDATE strategy_decisions SET run_id=COALESCE(NULLIF(TRIM(run_id),''),'legacy_run'), strategy_cohort=LOWER(TRIM(COALESCE(NULLIF(strategy_cohort,''),'default')))",
                [],
            )?;
            conn.execute(
                "UPDATE strategy_decisions SET market_key=LOWER(TRIM(COALESCE(NULLIF(market_key,''), NULLIF(asset_symbol,''), 'btc'))), asset_symbol=UPPER(TRIM(COALESCE(NULLIF(asset_symbol,''), NULLIF(market_key,''), 'BTC')))",
                [],
            )?;
            conn.execute(
                "UPDATE strategy_decision_contexts SET strategy_id=COALESCE(NULLIF(TRIM(strategy_id),''),'legacy_default')",
                [],
            )?;
            conn.execute(
                "UPDATE strategy_decision_contexts SET run_id=COALESCE(NULLIF(TRIM(run_id),''),'legacy_run'), strategy_cohort=LOWER(TRIM(COALESCE(NULLIF(strategy_cohort,''),'default')))",
                [],
            )?;
            conn.execute(
                "UPDATE strategy_decision_contexts SET market_key=LOWER(TRIM(COALESCE(NULLIF(market_key,''), NULLIF(asset_symbol,''), 'btc'))), asset_symbol=UPPER(TRIM(COALESCE(NULLIF(asset_symbol,''), NULLIF(market_key,''), 'BTC')))",
                [],
            )?;
            conn.execute(
                "UPDATE skipped_markets SET strategy_id=COALESCE(NULLIF(TRIM(strategy_id),''),'legacy_default')",
                [],
            )?;
            conn.execute(
                "UPDATE skipped_markets SET run_id=COALESCE(NULLIF(TRIM(run_id),''),'legacy_run'), strategy_cohort=LOWER(TRIM(COALESCE(NULLIF(strategy_cohort,''),'default')))",
                [],
            )?;
            conn.execute(
                "UPDATE skipped_markets SET market_key=LOWER(TRIM(COALESCE(NULLIF(market_key,''), NULLIF(asset_symbol,''), 'btc'))), asset_symbol=UPPER(TRIM(COALESCE(NULLIF(asset_symbol,''), NULLIF(market_key,''), 'BTC')))",
                [],
            )?;
            conn.execute(
                "UPDATE pending_orders SET side=UPPER(TRIM(side)), status=UPPER(TRIM(status)), entry_mode=UPPER(TRIM(COALESCE(entry_mode,'LEGACY'))), strategy_id=COALESCE(NULLIF(TRIM(strategy_id),''),'legacy_default'), run_id=COALESCE(NULLIF(TRIM(run_id),''),'legacy_run'), strategy_cohort=LOWER(TRIM(COALESCE(NULLIF(strategy_cohort,''),'default'))), asset_symbol=UPPER(TRIM(COALESCE(NULLIF(asset_symbol,''),'BTC'))), condition_id=NULLIF(TRIM(COALESCE(condition_id,'')),'')",
                [],
            )?;
            conn.execute(
                r#"
UPDATE pending_orders
SET filled_at_ms = COALESCE(
        filled_at_ms,
        (
            SELECT MIN(te.ts_ms)
            FROM trade_events te
            WHERE te.event_key = ('entry_fill_order:' || pending_orders.order_id)
               OR te.event_key = ('entry_fill_reconciled:' || pending_orders.order_id)
        ),
        CASE
            WHEN status='FILLED' THEN updated_at_ms
            ELSE NULL
        END
    )
WHERE filled_at_ms IS NULL
"#,
                [],
            )?;
            conn.execute(
                "UPDATE outcome_reflections SET strategy_id=COALESCE(NULLIF(TRIM(strategy_id),''),'legacy_default')",
                [],
            )?;
            conn.execute(
                "UPDATE parameter_suggestions SET strategy_id=COALESCE(NULLIF(TRIM(strategy_id),''),'legacy_default')",
                [],
            )?;
            conn.execute(
                "UPDATE parameter_suggestion_history SET strategy_id=COALESCE(NULLIF(TRIM(strategy_id),''),'legacy_default')",
                [],
            )?;
            conn.execute(
                "UPDATE runtime_parameter_state SET strategy_id=COALESCE(NULLIF(TRIM(strategy_id),''),'legacy_default')",
                [],
            )?;
            conn.execute(
                "UPDATE runtime_parameter_events SET strategy_id=COALESCE(NULLIF(TRIM(strategy_id),''),'legacy_default')",
                [],
            )?;
        }
        conn.execute_batch(
            r#"
DROP INDEX IF EXISTS uidx_strategy_decisions_period_tf_strategy;
DROP INDEX IF EXISTS uidx_strategy_decision_contexts_period_tf_strategy;
DROP INDEX IF EXISTS uidx_skipped_markets_period_tf_strategy;
DROP INDEX IF EXISTS uidx_strategy_decisions_period_tf_strategy_market;
DROP INDEX IF EXISTS uidx_strategy_decision_contexts_period_tf_strategy_market;
DROP INDEX IF EXISTS uidx_skipped_markets_period_tf_strategy_market;
DROP INDEX IF EXISTS idx_trade_events_strategy_asset_ts;
DROP INDEX IF EXISTS idx_pending_orders_strategy_asset_period_status;

CREATE UNIQUE INDEX IF NOT EXISTS uidx_strategy_decisions_period_tf_strategy_market
    ON strategy_decisions(period_timestamp, timeframe, strategy_id, market_key);
CREATE UNIQUE INDEX IF NOT EXISTS uidx_strategy_decision_contexts_period_tf_strategy_market
    ON strategy_decision_contexts(period_timestamp, timeframe, strategy_id, market_key);
CREATE UNIQUE INDEX IF NOT EXISTS uidx_skipped_markets_period_tf_strategy_market
    ON skipped_markets(period_timestamp, timeframe, strategy_id, market_key);
CREATE INDEX IF NOT EXISTS idx_trade_events_strategy_ts
    ON trade_events(strategy_id, ts_ms DESC);
CREATE INDEX IF NOT EXISTS idx_pending_orders_strategy_period_status
    ON pending_orders(strategy_id, period_timestamp, status);
CREATE INDEX IF NOT EXISTS idx_pending_orders_active_market_scope
    ON pending_orders(strategy_id, entry_mode, timeframe, period_timestamp, condition_id, token_id, side, status, updated_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_trade_events_strategy_asset_ts
    ON trade_events(strategy_id, asset_symbol, ts_ms DESC);
CREATE INDEX IF NOT EXISTS idx_pending_orders_strategy_asset_period_status
    ON pending_orders(strategy_id, asset_symbol, period_timestamp, status);
CREATE INDEX IF NOT EXISTS idx_trade_events_run_ts
    ON trade_events(run_id, ts_ms DESC);
CREATE INDEX IF NOT EXISTS idx_pending_orders_run_status
    ON pending_orders(run_id, status, updated_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_trade_events_submit_scope
    ON trade_events(strategy_id, timeframe, period_timestamp, token_id, side, event_type);
CREATE INDEX IF NOT EXISTS idx_pending_orders_trade_key_side_status
    ON pending_orders(trade_key, side, status, updated_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_pending_orders_period_token_side_status
    ON pending_orders(period_timestamp, token_id, side, status, updated_at_ms DESC);
"#,
        )?;
        let dedupe_archived_at_ms = chrono::Utc::now().timestamp_millis();
        conn.execute(
            r#"
INSERT INTO parameter_suggestion_history (
    suggestion_id, strategy_id, name, previous_status, current_value, suggested_value, min_value, max_value,
    confidence, reason, source, archived_at_ms
)
SELECT
    ps.id, ps.strategy_id, ps.name, ps.status, ps.current_value, ps.suggested_value, ps.min_value, ps.max_value,
    ps.confidence, ps.reason, ps.source, ?1
FROM parameter_suggestions ps
WHERE EXISTS (
    SELECT 1
    FROM parameter_suggestions newer
    WHERE newer.strategy_id = ps.strategy_id
      AND newer.name = ps.name
      AND (
          newer.updated_at_ms > ps.updated_at_ms
          OR (newer.updated_at_ms = ps.updated_at_ms AND newer.id > ps.id)
      )
)
"#,
            params![dedupe_archived_at_ms],
        )?;
        conn.execute(
            r#"
DELETE FROM parameter_suggestions
WHERE id IN (
    SELECT ps.id
    FROM parameter_suggestions ps
    WHERE EXISTS (
        SELECT 1
        FROM parameter_suggestions newer
        WHERE newer.strategy_id = ps.strategy_id
          AND newer.name = ps.name
          AND (
              newer.updated_at_ms > ps.updated_at_ms
              OR (newer.updated_at_ms = ps.updated_at_ms AND newer.id > ps.id)
          )
    )
)
"#,
            [],
        )?;
        ensure_settlement_v2_schema(&conn)?;
        ensure_lifecycle_wallet_schema(&conn)?;
        ensure_mm_market_state_schema(&conn)?;
        // Repair historical ENTRY_FILL rows that missed condition_id by inferring from
        // the same token_id, then replay missing v2 fill rows idempotently.
        conn.execute(
            r#"
UPDATE trade_events
SET condition_id = (
    SELECT t2.condition_id
    FROM trade_events t2
    WHERE t2.token_id = trade_events.token_id
      AND t2.condition_id IS NOT NULL
      AND TRIM(t2.condition_id) != ''
    ORDER BY t2.ts_ms DESC, t2.id DESC
    LIMIT 1
)
WHERE event_type='ENTRY_FILL'
  AND token_id IS NOT NULL
  AND TRIM(token_id) != ''
  AND (condition_id IS NULL OR TRIM(condition_id) = '')
  AND EXISTS (
      SELECT 1
      FROM trade_events t3
      WHERE t3.token_id = trade_events.token_id
        AND t3.condition_id IS NOT NULL
        AND TRIM(t3.condition_id) != ''
  )
"#,
            [],
        )?;
        conn.execute_batch(
            r#"
INSERT OR IGNORE INTO fills_v2 (
    event_key, ts_ms, period_timestamp, timeframe, strategy_id, condition_id, token_id, token_type,
    side, price, units, notional_usd, fee_usd, source_event_type, created_at_ms
)
SELECT
    COALESCE(event_key, printf('legacy_entry_fill:%d:%d', id, ts_ms)),
    ts_ms,
    period_timestamp,
    LOWER(TRIM(timeframe)),
    COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default'),
    condition_id,
    token_id,
    token_type,
    UPPER(COALESCE(NULLIF(TRIM(side), ''), 'BUY')),
    price,
    units,
    notional_usd,
    0.0,
    event_type,
    ts_ms
FROM trade_events
WHERE event_type='ENTRY_FILL'
  AND condition_id IS NOT NULL
  AND token_id IS NOT NULL;

INSERT INTO positions_v2 (
    position_key, strategy_id, timeframe, period_timestamp, condition_id, token_id, token_type,
    entry_units, entry_notional_usd, entry_fee_usd,
    exit_units, exit_notional_usd, exit_fee_usd,
    settled_payout_usd, realized_pnl_usd, status, opened_at_ms, updated_at_ms
)
SELECT
    COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default') || ':' ||
        LOWER(TRIM(timeframe)) || ':' || period_timestamp || ':' || token_id,
    COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default'),
    LOWER(TRIM(timeframe)),
    period_timestamp,
    condition_id,
    token_id,
    MAX(token_type),
    COALESCE(SUM(units), 0.0),
    COALESCE(SUM(notional_usd), 0.0),
    COALESCE(SUM(fee_usd), 0.0),
    0.0,
    0.0,
    0.0,
    0.0,
    0.0,
    'OPEN',
    MIN(ts_ms),
    MAX(ts_ms)
FROM fills_v2
GROUP BY strategy_id, timeframe, period_timestamp, condition_id, token_id
ON CONFLICT(position_key) DO UPDATE SET
    entry_units=MAX(positions_v2.entry_units, excluded.entry_units),
    entry_notional_usd=MAX(positions_v2.entry_notional_usd, excluded.entry_notional_usd),
    entry_fee_usd=MAX(positions_v2.entry_fee_usd, excluded.entry_fee_usd),
    token_type=COALESCE(positions_v2.token_type, excluded.token_type),
    updated_at_ms=MAX(positions_v2.updated_at_ms, excluded.updated_at_ms);
"#,
        )?;
        ensure_lifecycle_wallet_schema(&conn)?;
        ensure_mm_market_state_schema(&conn)?;
        ensure_strategy_feature_snapshot_schema(&conn)?;
        ensure_evcurve_period_base_schema(&conn)?;
        backfill_snapback_feature_snapshots_into_unified(&conn)?;
        let read_pool_size = std::env::var("EVPOLY_DB_READ_POOL_SIZE")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(4)
            .max(1);
        let mut read_conns = Vec::with_capacity(read_pool_size);
        for _ in 0..read_pool_size {
            let read_conn = Connection::open(&db_path)?;
            read_conn.busy_timeout(std::time::Duration::from_millis(busy_timeout_ms))?;
            read_conn.pragma_update(None, "journal_mode", "WAL")?;
            read_conn.pragma_update(None, "synchronous", "NORMAL")?;
            read_conns.push(Mutex::new(read_conn));
        }
        Ok(Self {
            conn: Mutex::new(conn),
            read_conns,
            read_rr: AtomicUsize::new(0),
        })
    }

    #[track_caller]
    fn with_conn<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        self.with_conn_profiled("with_conn", None, f)
    }

    #[track_caller]
    fn with_conn_read<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        self.with_conn_read_profiled("with_conn_read", None, f)
    }

    #[track_caller]
    fn with_conn_mut<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T>,
    {
        self.with_conn_mut_profiled("with_conn_mut", None, f)
    }

    #[track_caller]
    fn with_conn_profiled<T, F>(
        &self,
        scope: &'static str,
        rows_est: Option<usize>,
        f: F,
    ) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let caller = Location::caller();
        let can_block_in_place = tokio::runtime::Handle::try_current()
            .ok()
            .map(|h| {
                matches!(
                    h.runtime_flavor(),
                    tokio::runtime::RuntimeFlavor::MultiThread
                )
            })
            .unwrap_or(false);
        if can_block_in_place {
            tokio::task::block_in_place(|| {
                let wait_started = Instant::now();
                let conn = self
                    .conn
                    .lock()
                    .map_err(|_| anyhow!("tracking db mutex poisoned"))?;
                let wait_ms = i64::try_from(wait_started.elapsed().as_millis())
                    .ok()
                    .unwrap_or(i64::MAX);
                let hold_started = Instant::now();
                let result = f(&conn);
                let hold_ms = i64::try_from(hold_started.elapsed().as_millis())
                    .ok()
                    .unwrap_or(i64::MAX);
                maybe_log_db_lock_profile(scope, caller, wait_ms, hold_ms, rows_est);
                result
            })
        } else {
            let wait_started = Instant::now();
            let conn = self
                .conn
                .lock()
                .map_err(|_| anyhow!("tracking db mutex poisoned"))?;
            let wait_ms = i64::try_from(wait_started.elapsed().as_millis())
                .ok()
                .unwrap_or(i64::MAX);
            let hold_started = Instant::now();
            let result = f(&conn);
            let hold_ms = i64::try_from(hold_started.elapsed().as_millis())
                .ok()
                .unwrap_or(i64::MAX);
            maybe_log_db_lock_profile(scope, caller, wait_ms, hold_ms, rows_est);
            result
        }
    }

    #[track_caller]
    fn with_conn_read_profiled<T, F>(
        &self,
        scope: &'static str,
        rows_est: Option<usize>,
        f: F,
    ) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let caller = Location::caller();
        let can_block_in_place = tokio::runtime::Handle::try_current()
            .ok()
            .map(|h| {
                matches!(
                    h.runtime_flavor(),
                    tokio::runtime::RuntimeFlavor::MultiThread
                )
            })
            .unwrap_or(false);
        let read_pool_len = self.read_conns.len().max(1);
        let idx = self
            .read_rr
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_rem(read_pool_len);
        let reader_mutex = &self.read_conns[idx];
        if can_block_in_place {
            tokio::task::block_in_place(|| {
                let wait_started = Instant::now();
                let conn = reader_mutex
                    .lock()
                    .map_err(|_| anyhow!("tracking db read mutex poisoned"))?;
                let wait_ms = i64::try_from(wait_started.elapsed().as_millis())
                    .ok()
                    .unwrap_or(i64::MAX);
                let hold_started = Instant::now();
                let result = f(&conn);
                let hold_ms = i64::try_from(hold_started.elapsed().as_millis())
                    .ok()
                    .unwrap_or(i64::MAX);
                maybe_log_db_lock_profile(scope, caller, wait_ms, hold_ms, rows_est);
                result
            })
        } else {
            let wait_started = Instant::now();
            let conn = reader_mutex
                .lock()
                .map_err(|_| anyhow!("tracking db read mutex poisoned"))?;
            let wait_ms = i64::try_from(wait_started.elapsed().as_millis())
                .ok()
                .unwrap_or(i64::MAX);
            let hold_started = Instant::now();
            let result = f(&conn);
            let hold_ms = i64::try_from(hold_started.elapsed().as_millis())
                .ok()
                .unwrap_or(i64::MAX);
            maybe_log_db_lock_profile(scope, caller, wait_ms, hold_ms, rows_est);
            result
        }
    }

    #[track_caller]
    fn with_conn_mut_profiled<T, F>(
        &self,
        scope: &'static str,
        rows_est: Option<usize>,
        f: F,
    ) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T>,
    {
        let caller = Location::caller();
        let can_block_in_place = tokio::runtime::Handle::try_current()
            .ok()
            .map(|h| {
                matches!(
                    h.runtime_flavor(),
                    tokio::runtime::RuntimeFlavor::MultiThread
                )
            })
            .unwrap_or(false);
        if can_block_in_place {
            tokio::task::block_in_place(|| {
                let wait_started = Instant::now();
                let mut conn = self
                    .conn
                    .lock()
                    .map_err(|_| anyhow!("tracking db mutex poisoned"))?;
                let wait_ms = i64::try_from(wait_started.elapsed().as_millis())
                    .ok()
                    .unwrap_or(i64::MAX);
                let hold_started = Instant::now();
                let result = f(&mut conn);
                let hold_ms = i64::try_from(hold_started.elapsed().as_millis())
                    .ok()
                    .unwrap_or(i64::MAX);
                maybe_log_db_lock_profile(scope, caller, wait_ms, hold_ms, rows_est);
                result
            })
        } else {
            let wait_started = Instant::now();
            let mut conn = self
                .conn
                .lock()
                .map_err(|_| anyhow!("tracking db mutex poisoned"))?;
            let wait_ms = i64::try_from(wait_started.elapsed().as_millis())
                .ok()
                .unwrap_or(i64::MAX);
            let hold_started = Instant::now();
            let result = f(&mut conn);
            let hold_ms = i64::try_from(hold_started.elapsed().as_millis())
                .ok()
                .unwrap_or(i64::MAX);
            maybe_log_db_lock_profile(scope, caller, wait_ms, hold_ms, rows_est);
            result
        }
    }

    pub fn record_trade_event(&self, event: &TradeEventRecord) -> Result<()> {
        let strategy_id = normalize_strategy_id(event.strategy_id.as_str());
        let run_id = runtime_run_id();
        let strategy_cohort = runtime_strategy_cohort();
        let asset_symbol = normalize_asset_symbol(
            event.asset_symbol.as_deref(),
            event.token_type.as_deref(),
            None,
        );
        self.with_conn(|conn| {
            let inserted = conn.execute(
                r#"
INSERT INTO trade_events (
    event_key, ts_ms, period_timestamp, timeframe, strategy_id, run_id, strategy_cohort, asset_symbol,
    condition_id, token_id, token_type, side, event_type, price, units, notional_usd, pnl_usd, reason
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
ON CONFLICT(event_key) DO NOTHING
"#,
                params![
                    event.event_key,
                    event.ts_ms,
                    event.period_timestamp as i64,
                    event.timeframe,
                    strategy_id,
                    run_id,
                    strategy_cohort,
                    asset_symbol,
                    event.condition_id,
                    event.token_id,
                    event.token_type,
                    event.side,
                    event.event_type,
                    event.price,
                    event.units,
                    event.notional_usd,
                    event.pnl_usd,
                    event.reason,
                ],
            )?;
            if inserted == 0 && event.event_key.is_some() {
                return Err(anyhow!(
                    "duplicate trade_event ignored for key={}",
                    event.event_key.as_deref().unwrap_or("<missing>")
                ));
            }
            dual_write_trade_event_v2(conn, event, strategy_id.as_str())?;
            Ok(())
        })?;

        // Keep snapshot attribution as a DB-level invariant so every event writer
        // (not only Trader call paths) updates unified feature tracking.
        if let Err(e) = self.upsert_strategy_feature_snapshot_from_trade_event(event) {
            warn!(
                "Failed to upsert strategy feature snapshot from event {}: {}",
                event.event_type, e
            );
        }
        Ok(())
    }

    pub fn update_trade_lifecycle_redemption_state(
        &self,
        strategy_id: &str,
        timeframe: &str,
        period_timestamp: u64,
        condition_id: Option<&str>,
        token_id: &str,
        redemption_status: &str,
        redemption_tx_id: Option<&str>,
        redemption_attempts: Option<u32>,
        redeemed_notional_usd: Option<f64>,
        verification_detail: Option<&str>,
    ) -> Result<()> {
        let normalized_strategy = normalize_strategy_id(strategy_id);
        let normalized_timeframe = timeframe.trim().to_ascii_lowercase();
        let normalized_token_id = token_id.trim().to_string();
        if normalized_timeframe.is_empty() || normalized_token_id.is_empty() {
            return Err(anyhow!(
                "lifecycle redemption update requires timeframe and token_id"
            ));
        }
        let lifecycle_key = lifecycle_key(
            normalized_strategy.as_str(),
            normalized_timeframe.as_str(),
            period_timestamp,
            normalized_token_id.as_str(),
        );
        let status = redemption_status.trim().to_ascii_lowercase();
        if status.is_empty() {
            return Err(anyhow!("lifecycle redemption status cannot be empty"));
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        let redeemed_ts_ms = if matches!(status.as_str(), "success" | "already_redeemed") {
            Some(now_ms)
        } else {
            None
        };
        self.with_conn(|conn| {
            conn.execute(
                r#"
INSERT INTO trade_lifecycle_v1 (
    lifecycle_key, strategy_id, timeframe, period_timestamp, condition_id, token_id,
    redemption_status, redemption_tx_id, redemption_attempts, redeemed_ts_ms, redeemed_notional_usd,
    verification_detail, created_at_ms, updated_at_ms
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
ON CONFLICT(lifecycle_key) DO UPDATE SET
    condition_id=COALESCE(trade_lifecycle_v1.condition_id, excluded.condition_id),
    redemption_status=excluded.redemption_status,
    redemption_tx_id=COALESCE(excluded.redemption_tx_id, trade_lifecycle_v1.redemption_tx_id),
    redemption_attempts=MAX(COALESCE(trade_lifecycle_v1.redemption_attempts, 0), COALESCE(excluded.redemption_attempts, 0)),
    redeemed_ts_ms=COALESCE(excluded.redeemed_ts_ms, trade_lifecycle_v1.redeemed_ts_ms),
    redeemed_notional_usd=COALESCE(excluded.redeemed_notional_usd, trade_lifecycle_v1.redeemed_notional_usd),
    verification_detail=COALESCE(excluded.verification_detail, trade_lifecycle_v1.verification_detail),
    updated_at_ms=MAX(trade_lifecycle_v1.updated_at_ms, excluded.updated_at_ms)
"#,
                params![
                    lifecycle_key,
                    normalized_strategy,
                    normalized_timeframe,
                    period_timestamp as i64,
                    condition_id,
                    normalized_token_id,
                    status,
                    redemption_tx_id,
                    redemption_attempts.map(|v| v as i64),
                    redeemed_ts_ms,
                    redeemed_notional_usd,
                    verification_detail,
                    now_ms,
                    now_ms
                ],
            )?;
            Ok(())
        })
    }

    pub fn record_wallet_cashflow_event(&self, row: &WalletCashflowEventRecord) -> Result<()> {
        let cashflow_key = row.cashflow_key.trim().to_string();
        if cashflow_key.is_empty() {
            return Err(anyhow!("wallet cashflow event requires cashflow_key"));
        }
        let strategy_id = normalize_strategy_id(row.strategy_id.as_str());
        let timeframe = row.timeframe.trim().to_ascii_lowercase();
        if timeframe.is_empty() {
            return Err(anyhow!("wallet cashflow event requires timeframe"));
        }
        let token_id = row
            .token_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string);
        let condition_id = row
            .condition_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string);
        let tx_id = row
            .tx_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string);
        let reason = row
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string);
        let source = row
            .source
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| "redemption_sweep".to_string());
        let event_type = row.event_type.trim().to_ascii_uppercase();
        if event_type.is_empty() {
            return Err(anyhow!("wallet cashflow event requires event_type"));
        }
        self.with_conn(|conn| {
            conn.execute(
                r#"
INSERT INTO wallet_cashflow_events_v1 (
    cashflow_key, ts_ms, strategy_id, timeframe, period_timestamp, condition_id, token_id,
    event_type, amount_usd, units, token_price, tx_id, reason, source, created_at_ms
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
ON CONFLICT(cashflow_key) DO NOTHING
"#,
                params![
                    cashflow_key,
                    row.ts_ms,
                    strategy_id,
                    timeframe,
                    row.period_timestamp as i64,
                    condition_id,
                    token_id,
                    event_type,
                    row.amount_usd,
                    row.units,
                    row.token_price,
                    tx_id,
                    reason,
                    source,
                    row.ts_ms
                ],
            )?;
            Ok(())
        })
    }

    pub fn upsert_mm_market_state(&self, row: &MmMarketStateUpsertRecord) -> Result<()> {
        let strategy_id = normalize_strategy_id(row.strategy_id.as_str());
        let condition_id = row.condition_id.trim().to_string();
        if condition_id.is_empty() {
            return Err(anyhow!("mm market state requires condition_id"));
        }
        let timeframe = row.timeframe.trim().to_ascii_lowercase();
        if timeframe.is_empty() {
            return Err(anyhow!("mm market state requires timeframe"));
        }
        let state = row.state.trim().to_ascii_lowercase();
        if state.is_empty() {
            return Err(anyhow!("mm market state requires state"));
        }
        let mode = row
            .mode
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_ascii_lowercase());
        let symbol = row
            .symbol
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_ascii_uppercase());
        let market_slug = row
            .market_slug
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string);
        let reason = row
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string);
        let up_token_id = row
            .up_token_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string);
        let down_token_id = row
            .down_token_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string);
        let imbalance_side = row
            .imbalance_side
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_ascii_lowercase());
        let market_mode_runtime = row
            .market_mode_runtime
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_ascii_lowercase());
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.with_conn(|conn| {
            conn.execute(
                r#"
INSERT INTO mm_market_states_v1 (
    strategy_id, condition_id, market_slug, timeframe, symbol, mode, period_timestamp,
    state, reason, reward_rate_hint, open_buy_orders, open_sell_orders,
    favor_inventory_shares, weak_inventory_shares,
    up_token_id, down_token_id, up_inventory_shares, down_inventory_shares,
    imbalance_shares, imbalance_side, avg_entry_up, avg_entry_down,
    up_best_bid, down_best_bid, mtm_pnl_exit_usd, time_in_imbalance_ms,
    max_imbalance_shares, last_rebalance_duration_ms, rebalance_fill_rate,
    exit_slippage_vs_bbo_bps, market_mode_runtime,
    ladder_top_price, ladder_bottom_price, last_event_ms, updated_at_ms, created_at_ms
) VALUES (
    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
    ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26,
    ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36
)
ON CONFLICT(strategy_id, condition_id) DO UPDATE SET
    market_slug=COALESCE(excluded.market_slug, mm_market_states_v1.market_slug),
    timeframe=excluded.timeframe,
    symbol=COALESCE(excluded.symbol, mm_market_states_v1.symbol),
    mode=COALESCE(excluded.mode, mm_market_states_v1.mode),
    period_timestamp=excluded.period_timestamp,
    state=excluded.state,
    reason=excluded.reason,
    reward_rate_hint=excluded.reward_rate_hint,
    open_buy_orders=excluded.open_buy_orders,
    open_sell_orders=excluded.open_sell_orders,
    favor_inventory_shares=excluded.favor_inventory_shares,
    weak_inventory_shares=excluded.weak_inventory_shares,
    up_token_id=COALESCE(excluded.up_token_id, mm_market_states_v1.up_token_id),
    down_token_id=COALESCE(excluded.down_token_id, mm_market_states_v1.down_token_id),
    up_inventory_shares=excluded.up_inventory_shares,
    down_inventory_shares=excluded.down_inventory_shares,
    imbalance_shares=excluded.imbalance_shares,
    imbalance_side=excluded.imbalance_side,
    avg_entry_up=excluded.avg_entry_up,
    avg_entry_down=excluded.avg_entry_down,
    up_best_bid=excluded.up_best_bid,
    down_best_bid=excluded.down_best_bid,
    mtm_pnl_exit_usd=excluded.mtm_pnl_exit_usd,
    time_in_imbalance_ms=excluded.time_in_imbalance_ms,
    max_imbalance_shares=excluded.max_imbalance_shares,
    last_rebalance_duration_ms=excluded.last_rebalance_duration_ms,
    rebalance_fill_rate=excluded.rebalance_fill_rate,
    exit_slippage_vs_bbo_bps=excluded.exit_slippage_vs_bbo_bps,
    market_mode_runtime=COALESCE(excluded.market_mode_runtime, mm_market_states_v1.market_mode_runtime),
    ladder_top_price=excluded.ladder_top_price,
    ladder_bottom_price=excluded.ladder_bottom_price,
    last_event_ms=excluded.last_event_ms,
    updated_at_ms=excluded.updated_at_ms
"#,
                params![
                    strategy_id,
                    condition_id,
                    market_slug,
                    timeframe,
                    symbol,
                    mode,
                    row.period_timestamp as i64,
                    state,
                    reason,
                    row.reward_rate_hint,
                    i64::try_from(row.open_buy_orders).unwrap_or(i64::MAX),
                    i64::try_from(row.open_sell_orders).unwrap_or(i64::MAX),
                    row.favor_inventory_shares,
                    row.weak_inventory_shares,
                    up_token_id,
                    down_token_id,
                    row.up_inventory_shares,
                    row.down_inventory_shares,
                    row.imbalance_shares,
                    imbalance_side,
                    row.avg_entry_up,
                    row.avg_entry_down,
                    row.up_best_bid,
                    row.down_best_bid,
                    row.mtm_pnl_exit_usd,
                    row.time_in_imbalance_ms,
                    row.max_imbalance_shares,
                    row.last_rebalance_duration_ms,
                    row.rebalance_fill_rate,
                    row.exit_slippage_vs_bbo_bps,
                    market_mode_runtime,
                    row.ladder_top_price,
                    row.ladder_bottom_price,
                    row.last_event_ms,
                    now_ms,
                    now_ms
                ],
            )?;
            Ok(())
        })
    }

    pub fn list_mm_market_states(&self, strategy_id: &str) -> Result<Vec<MmMarketStateRow>> {
        let strategy_id = normalize_strategy_id(strategy_id);
        self.with_conn_read(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT
    strategy_id, condition_id, market_slug, timeframe, symbol, mode, period_timestamp, state, reason,
    reward_rate_hint, open_buy_orders, open_sell_orders, favor_inventory_shares, weak_inventory_shares,
    up_token_id, down_token_id, up_inventory_shares, down_inventory_shares,
    imbalance_shares, imbalance_side, avg_entry_up, avg_entry_down,
    up_best_bid, down_best_bid, mtm_pnl_exit_usd, time_in_imbalance_ms,
    max_imbalance_shares, last_rebalance_duration_ms, rebalance_fill_rate,
    exit_slippage_vs_bbo_bps, market_mode_runtime,
    ladder_top_price, ladder_bottom_price, last_event_ms, updated_at_ms
FROM mm_market_states_v1
WHERE strategy_id=?1
ORDER BY updated_at_ms DESC, condition_id ASC
"#,
            )?;
            let rows = stmt.query_map(params![strategy_id], |row| {
                let period_ts: i64 = row.get(6)?;
                Ok(MmMarketStateRow {
                    strategy_id: row.get(0)?,
                    condition_id: row.get(1)?,
                    market_slug: row.get(2)?,
                    timeframe: row.get(3)?,
                    symbol: row.get(4)?,
                    mode: row.get(5)?,
                    period_timestamp: period_ts.max(0) as u64,
                    state: row.get(7)?,
                    reason: row.get(8)?,
                    reward_rate_hint: row.get(9)?,
                    open_buy_orders: row.get::<_, i64>(10)?.max(0) as u64,
                    open_sell_orders: row.get::<_, i64>(11)?.max(0) as u64,
                    favor_inventory_shares: row.get(12)?,
                    weak_inventory_shares: row.get(13)?,
                    up_token_id: row.get(14)?,
                    down_token_id: row.get(15)?,
                    up_inventory_shares: row.get(16)?,
                    down_inventory_shares: row.get(17)?,
                    imbalance_shares: row.get(18)?,
                    imbalance_side: row.get(19)?,
                    avg_entry_up: row.get(20)?,
                    avg_entry_down: row.get(21)?,
                    up_best_bid: row.get(22)?,
                    down_best_bid: row.get(23)?,
                    mtm_pnl_exit_usd: row.get(24)?,
                    time_in_imbalance_ms: row.get(25)?,
                    max_imbalance_shares: row.get(26)?,
                    last_rebalance_duration_ms: row.get(27)?,
                    rebalance_fill_rate: row.get(28)?,
                    exit_slippage_vs_bbo_bps: row.get(29)?,
                    market_mode_runtime: row.get(30)?,
                    ladder_top_price: row.get(31)?,
                    ladder_bottom_price: row.get(32)?,
                    last_event_ms: row.get(33)?,
                    updated_at_ms: row.get(34)?,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn get_mm_live_token_state(
        &self,
        strategy_id: &str,
        condition_id: &str,
        timeframe: &str,
        period_timestamp: u64,
        token_id: &str,
    ) -> Result<MmTokenLiveStateRow> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let condition_id = condition_id.trim().to_ascii_lowercase();
        let timeframe = timeframe.trim().to_ascii_lowercase();
        let token_id = token_id.trim().to_ascii_lowercase();
        if condition_id.is_empty() || timeframe.is_empty() || token_id.is_empty() {
            return Ok(MmTokenLiveStateRow::default());
        }
        let wallet_address = configured_wallet_address();
        let wallet_inventory_shares = self.with_conn_read(|conn| {
            if !table_exists(conn, "wallet_positions_live_latest_v1")? {
                return Ok(None);
            }
            let shares: f64 = conn.query_row(
                r#"
SELECT COALESCE(SUM(COALESCE(position_size, 0.0)), 0.0)
FROM wallet_positions_live_latest_v1
WHERE LOWER(TRIM(COALESCE(condition_id, ''))) = ?1
  AND LOWER(TRIM(COALESCE(token_id, ''))) = ?2
  AND COALESCE(position_size, 0.0) > 1e-9
  AND (?3 IS NULL OR LOWER(TRIM(COALESCE(wallet_address, ''))) = ?3)
"#,
                params![
                    condition_id.as_str(),
                    token_id.as_str(),
                    wallet_address.as_deref()
                ],
                |row| row.get(0),
            )?;
            Ok(Some(shares.max(0.0)))
        })?;
        let db_inventory_shares = self.with_conn_read(|conn| {
            let shares: f64 = conn.query_row(
                r#"
SELECT COALESCE(SUM(
    COALESCE(entry_units, 0.0)
    - COALESCE(exit_units, 0.0)
    - COALESCE(inventory_consumed_units, 0.0)
), 0.0)
FROM positions_v2
WHERE strategy_id = ?1
  AND LOWER(TRIM(COALESCE(condition_id, ''))) = ?2
  AND LOWER(TRIM(COALESCE(token_id, ''))) = ?3
  AND status = 'OPEN'
"#,
                params![
                    strategy_id.as_str(),
                    condition_id.as_str(),
                    token_id.as_str()
                ],
                |row| row.get(0),
            )?;
            Ok(Some(shares.max(0.0)))
        })?;
        let active_buy_orders = self
            .list_open_pending_orders_for_strategy_scope(
                timeframe.as_str(),
                "mm_rewards",
                strategy_id.as_str(),
                period_timestamp,
                token_id.as_str(),
                "BUY",
            )?
            .len() as u64;
        let active_sell_orders = self
            .list_open_pending_orders_for_strategy_scope(
                timeframe.as_str(),
                "mm_rewards",
                strategy_id.as_str(),
                period_timestamp,
                token_id.as_str(),
                "SELL",
            )?
            .len() as u64;
        Ok(MmTokenLiveStateRow {
            wallet_inventory_shares,
            db_inventory_shares,
            active_buy_orders,
            active_sell_orders,
        })
    }

    pub fn consume_inventory_units(
        &self,
        consume_key: &str,
        strategy_id: &str,
        timeframe: &str,
        period_timestamp: u64,
        condition_id: &str,
        token_id: &str,
        requested_units: f64,
        wallet_units_after: Option<f64>,
        source: &str,
        reason: Option<&str>,
    ) -> Result<InventoryConsumptionResult> {
        let _ = timeframe;
        let _ = period_timestamp;
        self.record_inventory_consumption(
            consume_key,
            strategy_id,
            condition_id,
            token_id,
            requested_units,
            wallet_units_after,
            source,
            reason,
            chrono::Utc::now().timestamp_millis(),
        )
    }

    pub fn consume_mm_merge_inventory(
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
    ) -> Result<Vec<InventoryConsumptionResult>> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let timeframe = if timeframe.trim().is_empty() {
            "mm".to_string()
        } else {
            timeframe.trim().to_ascii_lowercase()
        };
        let condition_id = condition_id.trim().to_string();
        let up_token_id = up_token_id.trim().to_string();
        let down_token_id = down_token_id.trim().to_string();
        if condition_id.is_empty() || up_token_id.is_empty() || down_token_id.is_empty() {
            return Err(anyhow!(
                "merge inventory consumption requires condition and both token ids"
            ));
        }
        let pair_shares = if pair_shares.is_finite() && pair_shares > 0.0 {
            pair_shares
        } else {
            return Ok(Vec::new());
        };
        let tx_component = tx_id
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("none");
        let source = source.trim().to_ascii_lowercase();
        let reason = reason
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("merge_inventory_consume");
        let requested_component = format!("{:.6}", pair_shares);
        let up_key = format!(
            "inventory_consume:{}:{}:{}:{}:{}:{}",
            strategy_id, condition_id, up_token_id, source, tx_component, requested_component
        );
        let down_key = format!(
            "inventory_consume:{}:{}:{}:{}:{}:{}",
            strategy_id, condition_id, down_token_id, source, tx_component, requested_component
        );
        let mut out = Vec::with_capacity(2);
        out.push(self.consume_inventory_units(
            up_key.as_str(),
            strategy_id.as_str(),
            timeframe.as_str(),
            period_timestamp,
            condition_id.as_str(),
            up_token_id.as_str(),
            pair_shares,
            None,
            source.as_str(),
            Some(reason),
        )?);
        out.push(self.consume_inventory_units(
            down_key.as_str(),
            strategy_id.as_str(),
            timeframe.as_str(),
            period_timestamp,
            condition_id.as_str(),
            down_token_id.as_str(),
            pair_shares,
            None,
            source.as_str(),
            Some(reason),
        )?);
        Ok(out)
    }

    pub fn get_mm_inventory_drift_snapshot(
        &self,
        strategy_id: &str,
        limit: usize,
    ) -> Result<MmInventoryDriftSnapshot> {
        let strategy_id = normalize_strategy_id(strategy_id);
        self.with_conn_read(|conn| {
            #[derive(Clone, Default)]
            struct ConditionMeta {
                market_slug: Option<String>,
                timeframe: Option<String>,
                symbol: Option<String>,
                mode: Option<String>,
                period_timestamp: Option<u64>,
            }

            let wallet_address = configured_wallet_address();
            let mut snapshot = MmInventoryDriftSnapshot {
                wallet_address: wallet_address.clone(),
                ..MmInventoryDriftSnapshot::default()
            };
            let mut condition_meta: HashMap<String, ConditionMeta> = HashMap::new();
            let mut rows: HashMap<(String, String), MmInventoryDriftRow> = HashMap::new();

            fn apply_meta<'a>(
                rows: &'a mut HashMap<(String, String), MmInventoryDriftRow>,
                condition_id: &str,
                token_id: &str,
                meta: Option<&ConditionMeta>,
            ) -> &'a mut MmInventoryDriftRow {
                let row = rows
                    .entry((condition_id.to_string(), token_id.to_string()))
                    .or_insert_with(|| MmInventoryDriftRow {
                        condition_id: condition_id.to_string(),
                        token_id: token_id.to_string(),
                        market_slug: None,
                        timeframe: None,
                        symbol: None,
                        mode: None,
                        period_timestamp: None,
                        db_open_positions: 0,
                        db_inventory_shares: 0.0,
                        db_remaining_cost_usd: 0.0,
                        wallet_inventory_shares: None,
                        wallet_avg_price: None,
                        wallet_cur_price: None,
                        wallet_current_value_usd: None,
                        wallet_best_bid: None,
                        wallet_mid_price: None,
                        wallet_inventory_value_bid_usd: None,
                        wallet_inventory_value_mid_usd: None,
                        wallet_unrealized_mid_pnl_usd: None,
                        wallet_snapshot_ts_ms: None,
                        mid_snapshot_ts_ms: None,
                        last_wallet_activity_ts_ms: None,
                        last_wallet_activity_type: None,
                        last_inventory_consumed_units: None,
                        inventory_drift_shares: None,
                    });
                if let Some(meta) = meta {
                    if row.market_slug.is_none() {
                        row.market_slug = meta.market_slug.clone();
                    }
                    if row.timeframe.is_none() {
                        row.timeframe = meta.timeframe.clone();
                    }
                    if row.symbol.is_none() {
                        row.symbol = meta.symbol.clone();
                    }
                    if row.mode.is_none() {
                        row.mode = meta.mode.clone();
                    }
                    if row.period_timestamp.is_none() {
                        row.period_timestamp = meta.period_timestamp;
                    }
                }
                row
            }

            {
                let mut stmt = conn.prepare(
                    r#"
SELECT
    condition_id,
    MAX(market_slug) AS market_slug,
    MAX(timeframe) AS timeframe,
    MAX(symbol) AS symbol,
    MAX(mode) AS mode,
    MAX(period_timestamp) AS period_timestamp,
    MAX(up_token_id) AS up_token_id,
    MAX(down_token_id) AS down_token_id
FROM mm_market_states_v1
WHERE strategy_id=?1
GROUP BY condition_id
"#,
                )?;
                let mut meta_rows = stmt.query(params![strategy_id.clone()])?;
                while let Some(row) = meta_rows.next()? {
                    let condition_id: String = row.get(0)?;
                    let period_ts = row.get::<_, Option<i64>>(5)?.map(|v| v.max(0) as u64);
                    let meta = ConditionMeta {
                        market_slug: row.get(1)?,
                        timeframe: row.get::<_, Option<String>>(2)?,
                        symbol: row.get::<_, Option<String>>(3)?,
                        mode: row.get::<_, Option<String>>(4)?,
                        period_timestamp: period_ts,
                    };
                    let up_token_id: Option<String> = row.get(6)?;
                    let down_token_id: Option<String> = row.get(7)?;
                    condition_meta.insert(condition_id.clone(), meta.clone());
                    if let Some(token_id) = up_token_id.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                        apply_meta(&mut rows, condition_id.as_str(), token_id, Some(&meta));
                    }
                    if let Some(token_id) =
                        down_token_id.as_deref().map(str::trim).filter(|v| !v.is_empty())
                    {
                        apply_meta(&mut rows, condition_id.as_str(), token_id, Some(&meta));
                    }
                }
            }

            {
                let mut stmt = conn.prepare(
                    r#"
SELECT
    condition_id,
    token_id,
    MAX(timeframe) AS timeframe,
    MAX(period_timestamp) AS period_timestamp,
    SUM(
        CASE
            WHEN (
                COALESCE(entry_units, 0.0)
                - COALESCE(exit_units, 0.0)
                - COALESCE(inventory_consumed_units, 0.0)
            ) > 0.0
                THEN (
                    COALESCE(entry_units, 0.0)
                    - COALESCE(exit_units, 0.0)
                    - COALESCE(inventory_consumed_units, 0.0)
                )
            ELSE 0.0
        END
    ) AS db_inventory_shares,
    SUM(
        CASE
            WHEN COALESCE(entry_units, 0.0) > 1e-9
                THEN (
                    COALESCE(entry_notional_usd, 0.0)
                    + COALESCE(entry_fee_usd, 0.0)
                ) * (
                    CASE
                        WHEN (
                            COALESCE(entry_units, 0.0)
                            - COALESCE(exit_units, 0.0)
                            - COALESCE(inventory_consumed_units, 0.0)
                        ) > 0.0
                            THEN (
                                COALESCE(entry_units, 0.0)
                                - COALESCE(exit_units, 0.0)
                                - COALESCE(inventory_consumed_units, 0.0)
                            )
                        ELSE 0.0
                    END
                ) / COALESCE(entry_units, 0.0)
            ELSE 0.0
        END
    ) AS db_remaining_cost_usd,
    SUM(
        CASE
            WHEN (
                COALESCE(entry_units, 0.0)
                - COALESCE(exit_units, 0.0)
                - COALESCE(inventory_consumed_units, 0.0)
            ) > 1e-9
                THEN 1
            ELSE 0
        END
    ) AS db_open_positions
FROM positions_v2
WHERE strategy_id=?1
  AND status <> 'SETTLED'
  AND condition_id IS NOT NULL
  AND TRIM(condition_id) <> ''
  AND token_id IS NOT NULL
  AND TRIM(token_id) <> ''
GROUP BY condition_id, token_id
"#,
                )?;
                let mut db_rows = stmt.query(params![strategy_id.clone()])?;
                while let Some(row) = db_rows.next()? {
                    let condition_id: String = row.get(0)?;
                    let token_id: String = row.get(1)?;
                    let meta = condition_meta.get(condition_id.as_str());
                    apply_meta(&mut rows, condition_id.as_str(), token_id.as_str(), meta);
                    if let Some(entry) = rows.get_mut(&(condition_id.clone(), token_id.clone())) {
                        entry.timeframe = entry
                            .timeframe
                            .clone()
                            .or_else(|| row.get::<_, Option<String>>(2).ok().flatten());
                        entry.period_timestamp = entry.period_timestamp.or_else(|| {
                            row.get::<_, Option<i64>>(3)
                                .ok()
                                .flatten()
                                .map(|v| v.max(0) as u64)
                        });
                        entry.db_inventory_shares = row.get(4)?;
                        entry.db_remaining_cost_usd = row.get(5)?;
                        entry.db_open_positions = row.get::<_, i64>(6)?.max(0) as u64;
                    }
                }
            }

            if table_exists(conn, "wallet_positions_live_latest_v1")? {
                let mut saw_wallet_positions = false;
                let sql = if wallet_address.is_some() {
                    r#"
SELECT
    condition_id,
    token_id,
    MAX(snapshot_ts_ms) AS snapshot_ts_ms,
    SUM(COALESCE(position_size, 0.0)) AS wallet_inventory_shares,
    AVG(CASE WHEN COALESCE(avg_price, 0.0) > 0.0 THEN avg_price END) AS avg_price,
    AVG(CASE WHEN COALESCE(cur_price, 0.0) > 0.0 THEN cur_price END) AS cur_price,
    SUM(COALESCE(current_value, 0.0)) AS current_value_usd
FROM wallet_positions_live_latest_v1
WHERE wallet_address=?1
  AND condition_id IS NOT NULL
  AND TRIM(condition_id) <> ''
  AND token_id IS NOT NULL
  AND TRIM(token_id) <> ''
GROUP BY condition_id, token_id
"#
                } else {
                    r#"
SELECT
    condition_id,
    token_id,
    MAX(snapshot_ts_ms) AS snapshot_ts_ms,
    SUM(COALESCE(position_size, 0.0)) AS wallet_inventory_shares,
    AVG(CASE WHEN COALESCE(avg_price, 0.0) > 0.0 THEN avg_price END) AS avg_price,
    AVG(CASE WHEN COALESCE(cur_price, 0.0) > 0.0 THEN cur_price END) AS cur_price,
    SUM(COALESCE(current_value, 0.0)) AS current_value_usd
FROM wallet_positions_live_latest_v1
WHERE condition_id IS NOT NULL
  AND TRIM(condition_id) <> ''
  AND token_id IS NOT NULL
  AND TRIM(token_id) <> ''
GROUP BY condition_id, token_id
"#
                };
                let mut stmt = conn.prepare(sql)?;
                let mut wallet_rows = if let Some(wallet) = wallet_address.as_deref() {
                    stmt.query(params![wallet])?
                } else {
                    stmt.query([])?
                };
                while let Some(row) = wallet_rows.next()? {
                    saw_wallet_positions = true;
                    let condition_id: String = row.get(0)?;
                    let token_id: String = row.get(1)?;
                    let meta = condition_meta.get(condition_id.as_str());
                    apply_meta(&mut rows, condition_id.as_str(), token_id.as_str(), meta);
                    if let Some(entry) = rows.get_mut(&(condition_id.clone(), token_id.clone())) {
                        entry.wallet_snapshot_ts_ms = row.get(2)?;
                        entry.wallet_inventory_shares = Some(row.get(3)?);
                        entry.wallet_avg_price = row.get(4)?;
                        entry.wallet_cur_price = row.get(5)?;
                        entry.wallet_current_value_usd = row.get(6)?;
                    }
                }
                snapshot.wallet_positions_available = saw_wallet_positions;
            }

            if table_exists(conn, "positions_unrealized_mid_latest_v1")? {
                let mut saw_mid_marks = false;
                let mut stmt = conn.prepare(
                    r#"
SELECT
    condition_id,
    token_id,
    MAX(snapshot_ts_ms) AS snapshot_ts_ms,
    AVG(CASE WHEN COALESCE(best_bid, 0.0) > 0.0 THEN best_bid END) AS best_bid,
    AVG(CASE WHEN COALESCE(mid_price, 0.0) > 0.0 THEN mid_price END) AS mid_price,
    SUM(COALESCE(unrealized_mid_pnl_usd, 0.0)) AS unrealized_mid_pnl_usd
FROM positions_unrealized_mid_latest_v1
WHERE strategy_id=?1
GROUP BY condition_id, token_id
"#,
                )?;
                let mut mark_rows = stmt.query(params![strategy_id.clone()])?;
                while let Some(row) = mark_rows.next()? {
                    saw_mid_marks = true;
                    let condition_id: String = row.get(0)?;
                    let token_id: String = row.get(1)?;
                    let meta = condition_meta.get(condition_id.as_str());
                    apply_meta(&mut rows, condition_id.as_str(), token_id.as_str(), meta);
                    if let Some(entry) = rows.get_mut(&(condition_id.clone(), token_id.clone())) {
                        entry.mid_snapshot_ts_ms = row.get(2)?;
                        entry.wallet_best_bid = row.get(3)?;
                        entry.wallet_mid_price = row.get(4)?;
                        entry.wallet_unrealized_mid_pnl_usd = row.get(5)?;
                    }
                }
                snapshot.wallet_mid_marks_available = saw_mid_marks;
            }

            if table_exists(conn, "wallet_activity_v1")? {
                let activity_consumed_expr = if table_has_column(
                    conn,
                    "wallet_activity_v1",
                    "inventory_consumed_units",
                )? {
                    "COALESCE(inventory_consumed_units, 0.0)"
                } else {
                    "CASE WHEN UPPER(COALESCE(activity_type, ''))='MERGE' THEN COALESCE(size, 0.0) ELSE 0.0 END"
                };
                let sql = if wallet_address.is_some() {
                    format!(
                        r#"
WITH ranked AS (
    SELECT
        condition_id,
        token_id,
        ts_ms,
        activity_type,
        {inventory_expr} AS inventory_consumed_units,
        ROW_NUMBER() OVER (
            PARTITION BY condition_id, token_id
            ORDER BY ts_ms DESC, id DESC
        ) AS rn
    FROM wallet_activity_v1
    WHERE wallet_address=?1
      AND condition_id IS NOT NULL
      AND TRIM(condition_id) <> ''
      AND token_id IS NOT NULL
      AND TRIM(token_id) <> ''
)
SELECT condition_id, token_id, ts_ms, activity_type, inventory_consumed_units
FROM ranked
WHERE rn=1
"#,
                        inventory_expr = activity_consumed_expr
                    )
                } else {
                    format!(
                        r#"
WITH ranked AS (
    SELECT
        condition_id,
        token_id,
        ts_ms,
        activity_type,
        {inventory_expr} AS inventory_consumed_units,
        ROW_NUMBER() OVER (
            PARTITION BY condition_id, token_id
            ORDER BY ts_ms DESC, id DESC
        ) AS rn
    FROM wallet_activity_v1
    WHERE condition_id IS NOT NULL
      AND TRIM(condition_id) <> ''
      AND token_id IS NOT NULL
      AND TRIM(token_id) <> ''
)
SELECT condition_id, token_id, ts_ms, activity_type, inventory_consumed_units
FROM ranked
WHERE rn=1
"#,
                        inventory_expr = activity_consumed_expr
                    )
                };
                let mut stmt = conn.prepare(sql.as_str())?;
                let mut activity_rows = if let Some(wallet) = wallet_address.as_deref() {
                    stmt.query(params![wallet])?
                } else {
                    stmt.query([])?
                };
                let mut saw_activity_rows = false;
                while let Some(row) = activity_rows.next()? {
                    saw_activity_rows = true;
                    let condition_id: String = row.get(0)?;
                    let token_id: String = row.get(1)?;
                    let meta = condition_meta.get(condition_id.as_str());
                    apply_meta(&mut rows, condition_id.as_str(), token_id.as_str(), meta);
                    if let Some(entry) = rows.get_mut(&(condition_id.clone(), token_id.clone())) {
                        entry.last_wallet_activity_ts_ms = row.get(2)?;
                        entry.last_wallet_activity_type = row.get(3)?;
                        entry.last_inventory_consumed_units = row.get(4)?;
                    }
                }
                snapshot.wallet_activity_available = saw_activity_rows;
            }

            for row in rows.values_mut() {
                if snapshot.wallet_positions_available {
                    let wallet_shares = row.wallet_inventory_shares.unwrap_or(0.0);
                    row.wallet_inventory_shares = Some(wallet_shares);
                    if row.wallet_current_value_usd.is_none() {
                        if let Some(cur_price) = row.wallet_cur_price {
                            row.wallet_current_value_usd = Some(wallet_shares * cur_price);
                        } else {
                            row.wallet_current_value_usd = Some(0.0);
                        }
                    }
                    row.inventory_drift_shares = Some(row.db_inventory_shares - wallet_shares);
                    row.wallet_inventory_value_bid_usd = row
                        .wallet_best_bid
                        .map(|bid| wallet_shares * bid)
                        .or_else(|| row.wallet_current_value_usd);
                    row.wallet_inventory_value_mid_usd = row
                        .wallet_mid_price
                        .map(|mid| wallet_shares * mid)
                        .or_else(|| row.wallet_current_value_usd);
                }
            }

            let mut out: Vec<MmInventoryDriftRow> = rows.into_values().collect();
            out.sort_by(|a, b| {
                let left = a
                    .inventory_drift_shares
                    .unwrap_or(a.db_inventory_shares)
                    .abs();
                let right = b
                    .inventory_drift_shares
                    .unwrap_or(b.db_inventory_shares)
                    .abs();
                right
                    .partial_cmp(&left)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.condition_id.cmp(&b.condition_id))
                    .then_with(|| a.token_id.cmp(&b.token_id))
            });
            let bounded_limit = limit.clamp(1, 10_000);
            if out.len() > bounded_limit {
                out.truncate(bounded_limit);
            }
            snapshot.rows = out;
            Ok(snapshot)
        })
    }

    pub fn reconcile_mm_inventory_drift_from_wallet_tables(
        &self,
        strategy_id: &str,
        min_drift_shares: f64,
        limit: usize,
    ) -> Result<Vec<MmInventoryReconcileRow>> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let min_drift_shares = if min_drift_shares.is_finite() && min_drift_shares > 0.0 {
            min_drift_shares
        } else {
            1.0
        };
        let snapshot = self.get_mm_inventory_drift_snapshot(strategy_id.as_str(), 10_000)?;
        if !snapshot.wallet_positions_available {
            return Err(anyhow!(
                "wallet_positions_live_latest_v1 snapshot unavailable for reconcile"
            ));
        }
        let mut candidates: Vec<MmInventoryDriftRow> = snapshot
            .rows
            .into_iter()
            .filter(|row| row.inventory_drift_shares.unwrap_or(0.0).abs() > min_drift_shares)
            .collect();
        candidates.sort_by(|a, b| {
            b.inventory_drift_shares
                .unwrap_or(0.0)
                .abs()
                .partial_cmp(&a.inventory_drift_shares.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.condition_id.cmp(&b.condition_id))
                .then_with(|| a.token_id.cmp(&b.token_id))
        });
        let bounded_limit = limit.clamp(1, 10_000);
        if candidates.len() > bounded_limit {
            candidates.truncate(bounded_limit);
        }

        let mut out = Vec::with_capacity(candidates.len());
        for row in candidates {
            let wallet_inventory_shares = row.wallet_inventory_shares.unwrap_or(0.0);
            let drift_shares = row.inventory_drift_shares.unwrap_or(0.0);
            if drift_shares.abs() <= min_drift_shares {
                continue;
            }
            let last_activity_component = row
                .last_wallet_activity_ts_ms
                .map(|v| v.to_string())
                .unwrap_or_else(|| "0".to_string());
            let wallet_component = format!("{:.6}", wallet_inventory_shares);
            let drift_component = format!("{:.6}", drift_shares.abs());
            let period_timestamp = row.period_timestamp.unwrap_or_else(|| {
                row.last_wallet_activity_ts_ms
                    .map(|ts| (ts.max(0) / 1_000) as u64)
                    .unwrap_or(0)
            });
            let timeframe = row.timeframe.clone().unwrap_or_else(|| "mm".to_string());
            if drift_shares > 0.0 {
                let consume_key = format!(
                    "inventory_reconcile:{}:{}:{}:{}:{}:{}",
                    strategy_id,
                    row.condition_id,
                    row.token_id,
                    last_activity_component,
                    wallet_component,
                    drift_component
                );
                let reason_json = json!({
                    "mode": "wallet_drift_reconcile_consume",
                    "db_inventory_shares_before": row.db_inventory_shares,
                    "wallet_inventory_shares": wallet_inventory_shares,
                    "inventory_drift_shares": drift_shares,
                    "last_wallet_activity_type": row.last_wallet_activity_type,
                    "last_wallet_activity_ts_ms": row.last_wallet_activity_ts_ms
                })
                .to_string();
                let result = self.consume_inventory_units(
                    consume_key.as_str(),
                    strategy_id.as_str(),
                    timeframe.as_str(),
                    period_timestamp,
                    row.condition_id.as_str(),
                    row.token_id.as_str(),
                    drift_shares,
                    Some(wallet_inventory_shares),
                    "mm_inventory_reconcile",
                    Some(reason_json.as_str()),
                )?;
                out.push(MmInventoryReconcileRow {
                    consume_key: result.consume_key,
                    action: "consume".to_string(),
                    condition_id: result.condition_id,
                    token_id: result.token_id,
                    market_slug: row.market_slug,
                    timeframe: row.timeframe,
                    symbol: row.symbol,
                    mode: row.mode,
                    db_inventory_shares_before: row.db_inventory_shares,
                    wallet_inventory_shares,
                    drift_shares_before: drift_shares,
                    requested_consume_shares: result.requested_units,
                    applied_consume_shares: result.applied_units,
                    requested_add_shares: 0.0,
                    applied_add_shares: 0.0,
                    db_inventory_shares_after: result.db_units_after,
                    applied_cost_usd: result.applied_cost_usd,
                    last_wallet_activity_ts_ms: row.last_wallet_activity_ts_ms,
                    last_wallet_activity_type: row.last_wallet_activity_type,
                });
            } else {
                let add_shares = (-drift_shares).max(0.0);
                if add_shares <= min_drift_shares {
                    continue;
                }
                let price_basis = row
                    .wallet_avg_price
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .or_else(|| {
                        row.wallet_current_value_usd.and_then(|value| {
                            (wallet_inventory_shares > 1e-9)
                                .then_some(value / wallet_inventory_shares)
                        })
                    })
                    .or(row.wallet_cur_price.filter(|v| v.is_finite() && *v > 0.0))
                    .or(row.wallet_mid_price.filter(|v| v.is_finite() && *v > 0.0))
                    .or(row.wallet_best_bid.filter(|v| v.is_finite() && *v > 0.0))
                    .unwrap_or(0.0);
                let notional_usd = if price_basis > 0.0 {
                    add_shares * price_basis
                } else {
                    0.0
                };
                let event_key = format!(
                    "inventory_reconcile_add:{}:{}:{}:{}:{}:{}",
                    strategy_id,
                    row.condition_id,
                    row.token_id,
                    last_activity_component,
                    wallet_component,
                    drift_component
                );
                let reason_json = json!({
                    "mode": "wallet_drift_reconcile_add",
                    "db_inventory_shares_before": row.db_inventory_shares,
                    "wallet_inventory_shares": wallet_inventory_shares,
                    "inventory_drift_shares": drift_shares,
                    "price_basis": price_basis,
                    "price_basis_source": if row.wallet_avg_price.filter(|v| v.is_finite() && *v > 0.0).is_some() {
                        "wallet_avg_price"
                    } else if row.wallet_current_value_usd.is_some() && wallet_inventory_shares > 1e-9 {
                        "wallet_current_value"
                    } else if row.wallet_cur_price.is_some() {
                        "wallet_cur_price"
                    } else if row.wallet_mid_price.is_some() {
                        "wallet_mid_price"
                    } else if row.wallet_best_bid.is_some() {
                        "wallet_best_bid"
                    } else {
                        "none"
                    },
                    "last_wallet_activity_type": row.last_wallet_activity_type,
                    "last_wallet_activity_ts_ms": row.last_wallet_activity_ts_ms
                })
                .to_string();
                let add_result = self.record_trade_event(&TradeEventRecord {
                    event_key: Some(event_key.clone()),
                    ts_ms: chrono::Utc::now().timestamp_millis(),
                    period_timestamp,
                    timeframe: timeframe.clone(),
                    strategy_id: strategy_id.to_string(),
                    asset_symbol: row.symbol.clone(),
                    condition_id: Some(row.condition_id.clone()),
                    token_id: Some(row.token_id.clone()),
                    token_type: None,
                    side: Some("BUY".to_string()),
                    event_type: "ENTRY_FILL_RECONCILED".to_string(),
                    price: (price_basis > 0.0).then_some(price_basis),
                    units: Some(add_shares),
                    notional_usd: Some(notional_usd),
                    pnl_usd: None,
                    reason: Some(reason_json),
                });
                let applied_add_shares = match add_result {
                    Ok(()) => add_shares,
                    Err(e)
                        if e.to_string()
                            .contains("duplicate trade_event ignored for key=") =>
                    {
                        0.0
                    }
                    Err(e) => return Err(e),
                };
                out.push(MmInventoryReconcileRow {
                    consume_key: event_key,
                    action: "add".to_string(),
                    condition_id: row.condition_id,
                    token_id: row.token_id,
                    market_slug: row.market_slug,
                    timeframe: row.timeframe,
                    symbol: row.symbol,
                    mode: row.mode,
                    db_inventory_shares_before: row.db_inventory_shares,
                    wallet_inventory_shares,
                    drift_shares_before: drift_shares,
                    requested_consume_shares: 0.0,
                    applied_consume_shares: 0.0,
                    requested_add_shares: add_shares,
                    applied_add_shares,
                    db_inventory_shares_after: row.db_inventory_shares + add_shares,
                    applied_cost_usd: notional_usd,
                    last_wallet_activity_ts_ms: row.last_wallet_activity_ts_ms,
                    last_wallet_activity_type: row.last_wallet_activity_type,
                });
            }
        }
        Ok(out)
    }

    pub fn summarize_strategy_wallet_checksum(
        &self,
        from_ts_ms: Option<i64>,
    ) -> Result<Vec<StrategyWalletChecksumRow>> {
        let from_ts_ms = from_ts_ms.unwrap_or(0);
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
WITH settled AS (
    SELECT
        strategy_id,
        COUNT(*) AS settled_positions,
        COALESCE(SUM(payout_usd), 0.0) AS settled_payout_usd,
        COALESCE(SUM(total_cost_basis_usd), 0.0) AS settled_cost_basis_usd,
        COALESCE(SUM(settled_pnl_usd), 0.0) AS settled_pnl_usd
    FROM settlements_v2
    WHERE ts_ms >= ?1
    GROUP BY strategy_id
),
redeemed AS (
    SELECT
        strategy_id,
        COUNT(*) AS redeem_events,
        COALESCE(SUM(COALESCE(amount_usd, 0.0)), 0.0) AS redeemed_cash_usd
    FROM wallet_cashflow_events_v1
    WHERE ts_ms >= ?1
      AND event_type IN ('REDEMPTION_SUCCESS', 'REDEMPTION_ALREADY_REDEEMED', 'REDEMPTION_CONFIRMED')
    GROUP BY strategy_id
),
strategies AS (
    SELECT strategy_id FROM settled
    UNION
    SELECT strategy_id FROM redeemed
)
SELECT
    strategies.strategy_id,
    COALESCE(settled.settled_positions, 0) AS settled_positions,
    COALESCE(settled.settled_payout_usd, 0.0) AS settled_payout_usd,
    COALESCE(settled.settled_cost_basis_usd, 0.0) AS settled_cost_basis_usd,
    COALESCE(settled.settled_pnl_usd, 0.0) AS settled_pnl_usd,
    COALESCE(redeemed.redeemed_cash_usd, 0.0) AS redeemed_cash_usd,
    COALESCE(redeemed.redeem_events, 0) AS redeem_events,
    COALESCE(settled.settled_payout_usd, 0.0) - COALESCE(redeemed.redeemed_cash_usd, 0.0) AS unresolved_redeem_usd
FROM strategies
LEFT JOIN settled ON settled.strategy_id = strategies.strategy_id
LEFT JOIN redeemed ON redeemed.strategy_id = strategies.strategy_id
ORDER BY strategies.strategy_id
"#,
            )?;
            let rows = stmt.query_map(params![from_ts_ms], |row| {
                Ok(StrategyWalletChecksumRow {
                    strategy_id: row.get(0)?,
                    settled_positions: row.get::<_, i64>(1)?.max(0) as u64,
                    settled_payout_usd: row.get(2)?,
                    settled_cost_basis_usd: row.get(3)?,
                    settled_pnl_usd: row.get(4)?,
                    redeemed_cash_usd: row.get(5)?,
                    redeem_events: row.get::<_, i64>(6)?.max(0) as u64,
                    unresolved_redeem_usd: row.get(7)?,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn summarize_wallet_cashflow_by_event(
        &self,
        strategy_id: &str,
        from_ts_ms: Option<i64>,
    ) -> Result<Vec<WalletCashflowSummaryRow>> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let from_ts_ms = from_ts_ms.unwrap_or(0);
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT
    event_type,
    COUNT(*) AS events,
    COALESCE(SUM(COALESCE(amount_usd, 0.0)), 0.0) AS total_amount_usd
FROM wallet_cashflow_events_v1
WHERE strategy_id=?1
  AND ts_ms>=?2
GROUP BY event_type
ORDER BY event_type ASC
"#,
            )?;
            let rows = stmt.query_map(params![strategy_id, from_ts_ms], |row| {
                Ok(WalletCashflowSummaryRow {
                    event_type: row.get(0)?,
                    events: row.get::<_, i64>(1)?.max(0) as u64,
                    total_amount_usd: row.get(2)?,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn summarize_wallet_cashflow_unattributed_by_event(
        &self,
        strategy_id: &str,
        from_ts_ms: Option<i64>,
    ) -> Result<Vec<WalletCashflowSummaryRow>> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let from_ts_ms = from_ts_ms.unwrap_or(0);
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT
    event_type,
    COUNT(*) AS events,
    COALESCE(SUM(COALESCE(amount_usd, 0.0)), 0.0) AS total_amount_usd
FROM wallet_cashflow_events_v1
WHERE strategy_id=?1
  AND ts_ms>=?2
  AND (condition_id IS NULL OR TRIM(condition_id)='')
GROUP BY event_type
ORDER BY event_type ASC
"#,
            )?;
            let rows = stmt.query_map(params![strategy_id, from_ts_ms], |row| {
                Ok(WalletCashflowSummaryRow {
                    event_type: row.get(0)?,
                    events: row.get::<_, i64>(1)?.max(0) as u64,
                    total_amount_usd: row.get(2)?,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn record_inventory_consumption(
        &self,
        consume_key: &str,
        strategy_id: &str,
        condition_id: &str,
        token_id: &str,
        requested_units: f64,
        wallet_units_after: Option<f64>,
        source: &str,
        reason: Option<&str>,
        ts_ms: i64,
    ) -> Result<InventoryConsumptionResult> {
        let consume_key = consume_key.trim().to_string();
        let strategy_id = normalize_strategy_id(strategy_id);
        let condition_id = condition_id.trim().to_string();
        let token_id = token_id.trim().to_string();
        let source = source.trim().to_string();
        let requested_units = requested_units.max(0.0);
        if consume_key.is_empty()
            || strategy_id.is_empty()
            || condition_id.is_empty()
            || token_id.is_empty()
            || source.is_empty()
            || !requested_units.is_finite()
            || requested_units <= 0.0
        {
            return Err(anyhow!("invalid inventory consumption request"));
        }
        self.with_conn_mut(|conn| {
            let tx = conn.transaction()?;
            if let Some(existing) = tx
                .query_row(
                    r#"
SELECT
    consume_key, strategy_id, timeframe, period_timestamp, condition_id, token_id,
    requested_units, applied_units, applied_cost_usd, db_units_before, db_units_after,
    wallet_units_after, source, reason
FROM inventory_consumption_events_v1
WHERE consume_key=?1
LIMIT 1
"#,
                    params![consume_key.as_str()],
                    |row| {
                        let period_ts: i64 = row.get(3)?;
                        Ok(InventoryConsumptionResult {
                            consume_key: row.get(0)?,
                            strategy_id: row.get(1)?,
                            timeframe: row.get(2)?,
                            period_timestamp: period_ts.max(0) as u64,
                            condition_id: row.get(4)?,
                            token_id: row.get(5)?,
                            requested_units: row.get(6)?,
                            applied_units: row.get(7)?,
                            applied_cost_usd: row.get(8)?,
                            db_units_before: row.get(9)?,
                            db_units_after: row.get(10)?,
                            wallet_units_after: row.get(11)?,
                            source: row.get(12)?,
                            reason: row.get(13)?,
                        })
                    },
                )
                .optional()?
            {
                tx.commit()?;
                return Ok(existing);
            }

            let db_units_before: f64 = tx.query_row(
                r#"
SELECT COALESCE(SUM(
    COALESCE(entry_units, 0.0)
    - COALESCE(exit_units, 0.0)
    - COALESCE(inventory_consumed_units, 0.0)
), 0.0)
FROM positions_v2
WHERE strategy_id=?1
  AND condition_id=?2
  AND token_id=?3
  AND status='OPEN'
"#,
                params![
                    strategy_id.as_str(),
                    condition_id.as_str(),
                    token_id.as_str()
                ],
                |row| row.get(0),
            )?;

            let mut applied_units = 0.0_f64;
            let mut applied_cost_usd = 0.0_f64;
            let mut first_timeframe = String::new();
            let mut first_period_timestamp = 0_u64;
            let mut allocation_seq = 0_i64;
            let mut remaining_to_consume = requested_units.min(db_units_before.max(0.0));
            let mut stmt = tx.prepare(
                r#"
SELECT
    position_key,
    timeframe,
    period_timestamp,
    COALESCE(entry_units, 0.0),
    COALESCE(entry_notional_usd, 0.0) + COALESCE(entry_fee_usd, 0.0),
    COALESCE(exit_units, 0.0),
    COALESCE(inventory_consumed_units, 0.0),
    opened_at_ms
FROM positions_v2
WHERE strategy_id=?1
  AND condition_id=?2
  AND token_id=?3
  AND status='OPEN'
ORDER BY opened_at_ms ASC, period_timestamp ASC, position_key ASC
"#,
            )?;
            let mut rows = stmt.query(params![
                strategy_id.as_str(),
                condition_id.as_str(),
                token_id.as_str()
            ])?;
            while let Some(row) = rows.next()? {
                if remaining_to_consume <= 1e-9 {
                    break;
                }
                let position_key: String = row.get(0)?;
                let timeframe: String = row.get(1)?;
                let period_ts_i64: i64 = row.get(2)?;
                let entry_units: f64 = row.get(3)?;
                let total_entry_cost_usd: f64 = row.get(4)?;
                let exit_units: f64 = row.get(5)?;
                let inventory_consumed_units: f64 = row.get(6)?;
                let remaining_units =
                    (entry_units - exit_units - inventory_consumed_units).max(0.0);
                if remaining_units <= 1e-9 {
                    continue;
                }
                let consume_units = remaining_to_consume.min(remaining_units);
                if consume_units <= 1e-9 {
                    continue;
                }
                let consume_cost_usd = if entry_units > 1e-9 && total_entry_cost_usd.is_finite() {
                    (total_entry_cost_usd * (consume_units / entry_units)).max(0.0)
                } else {
                    0.0
                };
                tx.execute(
                    r#"
UPDATE positions_v2
SET
    inventory_consumed_units=COALESCE(inventory_consumed_units, 0.0) + ?2,
    inventory_consumed_cost_usd=COALESCE(inventory_consumed_cost_usd, 0.0) + ?3,
    status=CASE
        WHEN (
            COALESCE(exit_units, 0.0)
            + COALESCE(inventory_consumed_units, 0.0)
            + ?2
        ) >= COALESCE(entry_units, 0.0) - 1e-9
             AND COALESCE(entry_units, 0.0) > 0.0
            THEN 'CLOSED'
        ELSE status
    END,
    updated_at_ms=?4
WHERE position_key=?1
"#,
                    params![
                        position_key.as_str(),
                        consume_units,
                        consume_cost_usd,
                        ts_ms
                    ],
                )?;
                allocation_seq += 1;
                tx.execute(
                    r#"
INSERT OR IGNORE INTO inventory_consumption_allocations_v1 (
    consume_key, allocation_seq, position_key, strategy_id, timeframe, period_timestamp,
    condition_id, token_id, consumed_units, consumed_cost_usd, created_at_ms
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
"#,
                    params![
                        consume_key.as_str(),
                        allocation_seq,
                        position_key.as_str(),
                        strategy_id.as_str(),
                        timeframe.as_str(),
                        period_ts_i64,
                        condition_id.as_str(),
                        token_id.as_str(),
                        consume_units,
                        consume_cost_usd,
                        ts_ms
                    ],
                )?;
                if first_timeframe.is_empty() {
                    first_timeframe = timeframe;
                    first_period_timestamp = period_ts_i64.max(0) as u64;
                }
                applied_units += consume_units;
                applied_cost_usd += consume_cost_usd;
                remaining_to_consume = (remaining_to_consume - consume_units).max(0.0);
            }
            drop(rows);
            drop(stmt);

            let db_units_after = (db_units_before - applied_units).max(0.0);
            let timeframe_out = if first_timeframe.is_empty() {
                "mm".to_string()
            } else {
                first_timeframe
            };
            tx.execute(
                r#"
INSERT INTO inventory_consumption_events_v1 (
    consume_key, ts_ms, strategy_id, timeframe, period_timestamp, condition_id, token_id,
    requested_units, applied_units, applied_cost_usd, wallet_units_after,
    db_units_before, db_units_after, source, reason, created_at_ms
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
"#,
                params![
                    consume_key.as_str(),
                    ts_ms,
                    strategy_id.as_str(),
                    timeframe_out.as_str(),
                    i64::try_from(first_period_timestamp).ok().unwrap_or(0),
                    condition_id.as_str(),
                    token_id.as_str(),
                    requested_units,
                    applied_units,
                    applied_cost_usd,
                    wallet_units_after,
                    db_units_before,
                    db_units_after,
                    source.as_str(),
                    reason,
                    ts_ms
                ],
            )?;
            tx.commit()?;
            Ok(InventoryConsumptionResult {
                consume_key,
                strategy_id,
                timeframe: timeframe_out,
                period_timestamp: first_period_timestamp,
                condition_id,
                token_id,
                requested_units,
                applied_units,
                applied_cost_usd,
                db_units_before,
                db_units_after,
                wallet_units_after,
                source,
                reason: reason.map(|v| v.to_string()),
            })
        })
    }

    pub fn list_mm_wallet_inventory_by_strategy(
        &self,
        strategy_id: &str,
        limit: usize,
    ) -> Result<Vec<MmWalletInventoryRow>> {
        let strategy_id = normalize_strategy_id(strategy_id);
        self.with_conn_read_profiled("wallet_inventory.list_by_strategy", Some(limit), |conn| {
            if !table_exists(conn, "wallet_positions_live_latest_v1")? {
                return Ok(Vec::new());
            }
            let mut stmt = conn.prepare(
                r#"
SELECT
    w.condition_id,
    w.token_id,
    COALESCE(MAX(s.market_slug), MAX(w.slug)) AS market_slug,
    MAX(s.timeframe) AS timeframe,
    MAX(s.symbol) AS symbol,
    MAX(s.mode) AS mode,
    COALESCE(SUM(COALESCE(w.position_size, 0.0)), 0.0) AS wallet_shares,
    CASE
        WHEN COALESCE(SUM(COALESCE(w.position_size, 0.0)), 0.0) > 0.0
        THEN COALESCE(SUM(COALESCE(w.avg_price, 0.0) * COALESCE(w.position_size, 0.0)), 0.0)
            / SUM(COALESCE(w.position_size, 0.0))
        ELSE NULL
    END AS wallet_avg_price,
    COALESCE(SUM(COALESCE(w.current_value, COALESCE(w.cur_price, 0.0) * COALESCE(w.position_size, 0.0))), 0.0) AS wallet_value_usd,
    MAX(w.snapshot_ts_ms) AS snapshot_ts_ms
FROM wallet_positions_live_latest_v1 w
LEFT JOIN mm_market_states_v1 s
  ON s.strategy_id=?1
 AND s.condition_id = w.condition_id
WHERE COALESCE(w.position_size, 0.0) > 1e-9
  AND w.condition_id IS NOT NULL
  AND TRIM(w.condition_id) <> ''
GROUP BY w.condition_id, w.token_id
ORDER BY ABS(wallet_shares) DESC, w.condition_id ASC, w.token_id ASC
LIMIT ?2
"#,
            )?;
            let rows = stmt.query_map(
                params![
                    strategy_id.as_str(),
                    i64::try_from(limit.clamp(1, 20_000)).ok().unwrap_or(2_000),
                ],
                |row| {
                    Ok(MmWalletInventoryRow {
                        condition_id: row.get(0)?,
                        token_id: row.get(1)?,
                        market_slug: row.get(2)?,
                        timeframe: row.get(3)?,
                        symbol: row.get(4)?,
                        mode: row.get(5)?,
                        wallet_shares: row.get(6)?,
                        wallet_avg_price: row.get(7)?,
                        wallet_value_usd: row.get(8)?,
                        snapshot_ts_ms: row.get(9)?,
                    })
                },
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn latest_wallet_positions_snapshot_ts_ms(&self) -> Result<Option<i64>> {
        self.with_conn_read(|conn| {
            if !table_exists(conn, "wallet_positions_live_latest_v1")? {
                return Ok(None);
            }
            let mut stmt =
                conn.prepare("SELECT MAX(snapshot_ts_ms) FROM wallet_positions_live_latest_v1")?;
            let ts_ms: Option<i64> = stmt.query_row([], |row| row.get(0))?;
            Ok(ts_ms.filter(|v| *v > 0))
        })
    }

    pub fn summarize_wallet_sync_health(
        &self,
        lookback_runs: usize,
    ) -> Result<Option<WalletSyncHealthSummary>> {
        self.with_conn_read(|conn| {
            if !table_exists(conn, "wallet_sync_runs_v1")? {
                return Ok(None);
            }
            let limit = i64::try_from(lookback_runs.clamp(1, 200))
                .ok()
                .unwrap_or(24);
            let mut stmt = conn.prepare(
                r#"
SELECT
    ts_ms,
    status,
    error,
    inventory_reconcile_error,
    dust_sell_error
FROM wallet_sync_runs_v1
ORDER BY id DESC
LIMIT ?1
"#,
            )?;
            let rows = stmt.query_map(params![limit], |row| {
                let ts_ms: i64 = row.get(0)?;
                let status: String = row.get(1)?;
                let error: Option<String> = row.get(2)?;
                let inventory_reconcile_error: Option<String> = row.get(3)?;
                let dust_sell_error: Option<String> = row.get(4)?;
                Ok((
                    ts_ms,
                    status,
                    error,
                    inventory_reconcile_error,
                    dust_sell_error,
                ))
            })?;
            let mut records = Vec::new();
            for row in rows {
                records.push(row?);
            }
            if records.is_empty() {
                return Ok(None);
            }
            let mut out = WalletSyncHealthSummary {
                latest_ts_ms: Some(records[0].0),
                latest_status: Some(records[0].1.clone()),
                latest_error: records[0]
                    .2
                    .clone()
                    .or(records[0].3.clone())
                    .or(records[0].4.clone()),
                ..WalletSyncHealthSummary::default()
            };
            let mut count_consecutive_non_ok = true;
            for (_ts_ms, status, error, reconcile_error, dust_sell_error) in records {
                out.recent_runs_checked = out.recent_runs_checked.saturating_add(1);
                let ok_status = status.trim().eq_ignore_ascii_case("ok");
                if ok_status {
                    count_consecutive_non_ok = false;
                } else {
                    out.recent_non_ok_runs = out.recent_non_ok_runs.saturating_add(1);
                    if count_consecutive_non_ok {
                        out.consecutive_non_ok_runs = out.consecutive_non_ok_runs.saturating_add(1);
                    }
                }

                let mut merged_error = String::new();
                if let Some(v) = error {
                    merged_error.push_str(v.as_str());
                    merged_error.push(' ');
                }
                if let Some(v) = reconcile_error {
                    merged_error.push_str(v.as_str());
                    merged_error.push(' ');
                }
                if let Some(v) = dust_sell_error {
                    merged_error.push_str(v.as_str());
                    merged_error.push(' ');
                }
                if merged_error.trim().is_empty() {
                    continue;
                }
                let lower = merged_error.to_ascii_lowercase();
                if lower.contains("database is locked") {
                    out.recent_db_locked_errors = out.recent_db_locked_errors.saturating_add(1);
                }
                if lower.contains("connection refused")
                    || lower.contains("max retries exceeded")
                    || lower.contains("failed to establish a new connection")
                    || lower.contains("timed out")
                    || lower.contains("timeout")
                {
                    out.recent_api_unavailable_errors =
                        out.recent_api_unavailable_errors.saturating_add(1);
                }
            }
            Ok(Some(out))
        })
    }

    pub fn list_wallet_mergeable_inventory(
        &self,
        limit: usize,
    ) -> Result<Vec<MmWalletInventoryRow>> {
        self.with_conn_read_profiled("wallet_inventory.list_mergeable", Some(limit), |conn| {
            if !table_exists(conn, "wallet_positions_live_latest_v1")? {
                return Ok(Vec::new());
            }
            let mut stmt = conn.prepare(
                r#"
SELECT
    w.condition_id,
    w.token_id,
    MAX(w.slug) AS market_slug,
    CAST(NULL AS TEXT) AS timeframe,
    CAST(NULL AS TEXT) AS symbol,
    CAST(NULL AS TEXT) AS mode,
    COALESCE(SUM(COALESCE(w.position_size, 0.0)), 0.0) AS wallet_shares,
    CASE
        WHEN COALESCE(SUM(COALESCE(w.position_size, 0.0)), 0.0) > 0.0
        THEN COALESCE(SUM(COALESCE(w.avg_price, 0.0) * COALESCE(w.position_size, 0.0)), 0.0)
            / SUM(COALESCE(w.position_size, 0.0))
        ELSE NULL
    END AS wallet_avg_price,
    COALESCE(SUM(COALESCE(w.current_value, COALESCE(w.cur_price, 0.0) * COALESCE(w.position_size, 0.0))), 0.0) AS wallet_value_usd,
    MAX(w.snapshot_ts_ms) AS snapshot_ts_ms
FROM wallet_positions_live_latest_v1 w
WHERE COALESCE(w.position_size, 0.0) > 1e-9
  AND COALESCE(w.mergeable, 0) = 1
  AND w.condition_id IS NOT NULL
  AND TRIM(w.condition_id) <> ''
GROUP BY w.condition_id, w.token_id
ORDER BY ABS(wallet_shares) DESC, w.condition_id ASC, w.token_id ASC
LIMIT ?1
"#,
            )?;
            let rows = stmt.query_map(
                params![i64::try_from(limit.clamp(1, 20_000)).ok().unwrap_or(2_000)],
                |row| {
                    Ok(MmWalletInventoryRow {
                        condition_id: row.get(0)?,
                        token_id: row.get(1)?,
                        market_slug: row.get(2)?,
                        timeframe: row.get(3)?,
                        symbol: row.get(4)?,
                        mode: row.get(5)?,
                        wallet_shares: row.get(6)?,
                        wallet_avg_price: row.get(7)?,
                        wallet_value_usd: row.get(8)?,
                        snapshot_ts_ms: row.get(9)?,
                    })
                },
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn snapshot_mm_inventory_drift(
        &self,
        strategy_id: &str,
        limit: usize,
    ) -> Result<MmInventoryDriftSnapshot> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let wallet_address = configured_wallet_address();
        self.with_conn_read_profiled("wallet_inventory.snapshot_drift", Some(limit), |conn| {
            let wallet_positions_available = table_exists(conn, "wallet_positions_live_latest_v1")?;
            let wallet_activity_available = table_exists(conn, "wallet_activity_v1")?;
            let wallet_mid_marks_available =
                table_exists(conn, "positions_unrealized_mid_latest_v1")?;
            let wallet_positions_cte = if wallet_positions_available {
                r#"
wallet_positions AS (
    SELECT
        condition_id,
        token_id,
        COALESCE(SUM(COALESCE(position_size, 0.0)), 0.0) AS wallet_inventory_shares,
        CASE
            WHEN COALESCE(SUM(COALESCE(position_size, 0.0)), 0.0) > 0.0
            THEN COALESCE(SUM(COALESCE(avg_price, 0.0) * COALESCE(position_size, 0.0)), 0.0)
                / SUM(COALESCE(position_size, 0.0))
            ELSE NULL
        END AS wallet_avg_price,
        CASE
            WHEN COALESCE(SUM(COALESCE(position_size, 0.0)), 0.0) > 0.0
            THEN COALESCE(SUM(COALESCE(cur_price, 0.0) * COALESCE(position_size, 0.0)), 0.0)
                / SUM(COALESCE(position_size, 0.0))
            ELSE NULL
        END AS wallet_cur_price,
        COALESCE(SUM(COALESCE(current_value, COALESCE(cur_price, 0.0) * COALESCE(position_size, 0.0))), 0.0) AS wallet_current_value_usd,
        MAX(snapshot_ts_ms) AS wallet_snapshot_ts_ms
    FROM wallet_positions_live_latest_v1
    WHERE condition_id IS NOT NULL
      AND token_id IS NOT NULL
      AND COALESCE(position_size, 0.0) > 1e-9
    GROUP BY condition_id, token_id
),
"#
            } else {
                r#"
wallet_positions AS (
    SELECT
        CAST(NULL AS TEXT) AS condition_id,
        CAST(NULL AS TEXT) AS token_id,
        CAST(NULL AS REAL) AS wallet_inventory_shares,
        CAST(NULL AS REAL) AS wallet_avg_price,
        CAST(NULL AS REAL) AS wallet_cur_price,
        CAST(NULL AS REAL) AS wallet_current_value_usd,
        CAST(NULL AS INTEGER) AS wallet_snapshot_ts_ms
    WHERE 0
),
"#
            };
            let wallet_mid_cte = if wallet_mid_marks_available {
                r#"
wallet_mid AS (
    SELECT
        condition_id,
        token_id,
        CASE
            WHEN COALESCE(SUM(COALESCE(remaining_units, 0.0)), 0.0) > 0.0
            THEN COALESCE(SUM(COALESCE(best_bid, 0.0) * COALESCE(remaining_units, 0.0)), 0.0)
                / SUM(COALESCE(remaining_units, 0.0))
            ELSE NULL
        END AS wallet_best_bid,
        CASE
            WHEN COALESCE(SUM(COALESCE(remaining_units, 0.0)), 0.0) > 0.0
            THEN COALESCE(SUM(COALESCE(mid_price, 0.0) * COALESCE(remaining_units, 0.0)), 0.0)
                / SUM(COALESCE(remaining_units, 0.0))
            ELSE NULL
        END AS wallet_mid_price,
        COALESCE(SUM(COALESCE(best_bid, 0.0) * COALESCE(remaining_units, 0.0)), 0.0) AS wallet_inventory_value_bid_usd,
        COALESCE(SUM(COALESCE(mid_price, 0.0) * COALESCE(remaining_units, 0.0)), 0.0) AS wallet_inventory_value_mid_usd,
        COALESCE(SUM(COALESCE(unrealized_mid_pnl_usd, 0.0)), 0.0) AS wallet_unrealized_mid_pnl_usd,
        MAX(snapshot_ts_ms) AS mid_snapshot_ts_ms
    FROM positions_unrealized_mid_latest_v1
    WHERE strategy_id=?1
    GROUP BY condition_id, token_id
),
"#
            } else {
                r#"
wallet_mid AS (
    SELECT
        CAST(NULL AS TEXT) AS condition_id,
        CAST(NULL AS TEXT) AS token_id,
        CAST(NULL AS REAL) AS wallet_best_bid,
        CAST(NULL AS REAL) AS wallet_mid_price,
        CAST(NULL AS REAL) AS wallet_inventory_value_bid_usd,
        CAST(NULL AS REAL) AS wallet_inventory_value_mid_usd,
        CAST(NULL AS REAL) AS wallet_unrealized_mid_pnl_usd,
        CAST(NULL AS INTEGER) AS mid_snapshot_ts_ms
    WHERE 0
),
"#
            };
            let wallet_activity_cte = if wallet_activity_available {
                r#"
wallet_activity_latest AS (
    SELECT
        condition_id,
        token_id,
        MAX(ts_ms) AS last_wallet_activity_ts_ms
    FROM wallet_activity_v1
    WHERE condition_id IS NOT NULL
      AND token_id IS NOT NULL
    GROUP BY condition_id, token_id
),
wallet_activity AS (
    SELECT
        wa.condition_id,
        wa.token_id,
        wa.ts_ms AS last_wallet_activity_ts_ms,
        wa.activity_type AS last_wallet_activity_type
    FROM wallet_activity_v1 wa
    JOIN wallet_activity_latest latest
      ON latest.condition_id = wa.condition_id
     AND latest.token_id = wa.token_id
     AND latest.last_wallet_activity_ts_ms = wa.ts_ms
),
"#
            } else {
                r#"
wallet_activity AS (
    SELECT
        CAST(NULL AS TEXT) AS condition_id,
        CAST(NULL AS TEXT) AS token_id,
        CAST(NULL AS INTEGER) AS last_wallet_activity_ts_ms,
        CAST(NULL AS TEXT) AS last_wallet_activity_type
    WHERE 0
),
"#
            };
            let sql = format!(
                r#"
WITH market_meta_raw AS (
    SELECT condition_id, up_token_id AS token_id, market_slug, timeframe, symbol, mode, period_timestamp
    FROM mm_market_states_v1
    WHERE strategy_id=?1
      AND up_token_id IS NOT NULL
      AND TRIM(up_token_id) <> ''
    UNION ALL
    SELECT condition_id, down_token_id AS token_id, market_slug, timeframe, symbol, mode, period_timestamp
    FROM mm_market_states_v1
    WHERE strategy_id=?1
      AND down_token_id IS NOT NULL
      AND TRIM(down_token_id) <> ''
),
market_meta AS (
    SELECT
        condition_id,
        token_id,
        MAX(market_slug) AS market_slug,
        MAX(timeframe) AS timeframe,
        MAX(symbol) AS symbol,
        MAX(mode) AS mode,
        MAX(period_timestamp) AS period_timestamp
    FROM market_meta_raw
    GROUP BY condition_id, token_id
),
db_inventory AS (
    SELECT
        condition_id,
        token_id,
        COUNT(*) AS db_open_positions,
        COALESCE(SUM(
            MAX(
                COALESCE(entry_units, 0.0)
                - COALESCE(exit_units, 0.0)
                - COALESCE(inventory_consumed_units, 0.0),
                0.0
            )
        ), 0.0) AS db_inventory_shares,
        COALESCE(SUM(
            CASE
                WHEN COALESCE(entry_units, 0.0) > 1e-9
                THEN (
                    COALESCE(entry_notional_usd, 0.0) + COALESCE(entry_fee_usd, 0.0)
                ) * (
                    MAX(
                        COALESCE(entry_units, 0.0)
                        - COALESCE(exit_units, 0.0)
                        - COALESCE(inventory_consumed_units, 0.0),
                        0.0
                    ) / COALESCE(entry_units, 1.0)
                )
                ELSE 0.0
            END
        ), 0.0) AS db_remaining_cost_usd
    FROM positions_v2
    WHERE strategy_id=?1
      AND status='OPEN'
      AND condition_id IS NOT NULL
      AND token_id IS NOT NULL
    GROUP BY condition_id, token_id
),
{wallet_positions_cte}
{wallet_mid_cte}
{wallet_activity_cte}
last_consumption AS (
    SELECT
        condition_id,
        token_id,
        MAX(ts_ms) AS last_consume_ts_ms,
        MAX(applied_units) AS last_inventory_consumed_units
    FROM inventory_consumption_events_v1
    WHERE strategy_id=?1
    GROUP BY condition_id, token_id
),
keys AS (
    SELECT condition_id, token_id FROM market_meta
    UNION
    SELECT condition_id, token_id FROM db_inventory
    UNION
    SELECT condition_id, token_id FROM wallet_positions
)
SELECT
    keys.condition_id,
    keys.token_id,
    market_meta.market_slug,
    market_meta.timeframe,
    market_meta.symbol,
    market_meta.mode,
    market_meta.period_timestamp,
    COALESCE(db_inventory.db_open_positions, 0) AS db_open_positions,
    COALESCE(db_inventory.db_inventory_shares, 0.0) AS db_inventory_shares,
    COALESCE(db_inventory.db_remaining_cost_usd, 0.0) AS db_remaining_cost_usd,
    wallet_positions.wallet_inventory_shares,
    wallet_positions.wallet_avg_price,
    wallet_positions.wallet_cur_price,
    wallet_positions.wallet_current_value_usd,
    wallet_mid.wallet_best_bid,
    wallet_mid.wallet_mid_price,
    wallet_mid.wallet_inventory_value_bid_usd,
    wallet_mid.wallet_inventory_value_mid_usd,
    wallet_mid.wallet_unrealized_mid_pnl_usd,
    wallet_positions.wallet_snapshot_ts_ms,
    wallet_mid.mid_snapshot_ts_ms,
    wallet_activity.last_wallet_activity_ts_ms,
    wallet_activity.last_wallet_activity_type,
    last_consumption.last_inventory_consumed_units,
    CASE
        WHEN wallet_positions.wallet_inventory_shares IS NOT NULL
        THEN wallet_positions.wallet_inventory_shares - COALESCE(db_inventory.db_inventory_shares, 0.0)
        ELSE NULL
    END AS inventory_drift_shares
FROM keys
LEFT JOIN market_meta
  ON market_meta.condition_id = keys.condition_id
 AND market_meta.token_id = keys.token_id
LEFT JOIN db_inventory
  ON db_inventory.condition_id = keys.condition_id
 AND db_inventory.token_id = keys.token_id
LEFT JOIN wallet_positions
  ON wallet_positions.condition_id = keys.condition_id
 AND wallet_positions.token_id = keys.token_id
LEFT JOIN wallet_mid
  ON wallet_mid.condition_id = keys.condition_id
 AND wallet_mid.token_id = keys.token_id
LEFT JOIN wallet_activity
  ON wallet_activity.condition_id = keys.condition_id
 AND wallet_activity.token_id = keys.token_id
LEFT JOIN last_consumption
  ON last_consumption.condition_id = keys.condition_id
 AND last_consumption.token_id = keys.token_id
ORDER BY ABS(COALESCE(inventory_drift_shares, 0.0)) DESC, keys.condition_id ASC, keys.token_id ASC
LIMIT ?2
"#
            );
            let mut stmt = conn.prepare(sql.as_str())?;
            let rows = stmt.query_map(
                params![
                    strategy_id.as_str(),
                    i64::try_from(limit.clamp(1, 20_000)).ok().unwrap_or(2_000),
                ],
                |row| {
                    let period_ts: Option<i64> = row.get(6)?;
                    Ok(MmInventoryDriftRow {
                        condition_id: row.get(0)?,
                        token_id: row.get(1)?,
                        market_slug: row.get(2)?,
                        timeframe: row.get(3)?,
                        symbol: row.get(4)?,
                        mode: row.get(5)?,
                        period_timestamp: period_ts.map(|v| v.max(0) as u64),
                        db_open_positions: row.get::<_, i64>(7)?.max(0) as u64,
                        db_inventory_shares: row.get(8)?,
                        db_remaining_cost_usd: row.get(9)?,
                        wallet_inventory_shares: row.get(10)?,
                        wallet_avg_price: row.get(11)?,
                        wallet_cur_price: row.get(12)?,
                        wallet_current_value_usd: row.get(13)?,
                        wallet_best_bid: row.get(14)?,
                        wallet_mid_price: row.get(15)?,
                        wallet_inventory_value_bid_usd: row.get(16)?,
                        wallet_inventory_value_mid_usd: row.get(17)?,
                        wallet_unrealized_mid_pnl_usd: row.get(18)?,
                        wallet_snapshot_ts_ms: row.get(19)?,
                        mid_snapshot_ts_ms: row.get(20)?,
                        last_wallet_activity_ts_ms: row.get(21)?,
                        last_wallet_activity_type: row.get(22)?,
                        last_inventory_consumed_units: row.get(23)?,
                        inventory_drift_shares: row.get(24)?,
                    })
                },
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(MmInventoryDriftSnapshot {
                wallet_address,
                wallet_positions_available,
                wallet_activity_available,
                wallet_mid_marks_available,
                rows: out,
            })
        })
    }

    pub fn reconcile_mm_inventory_with_wallet(
        &self,
        strategy_id: &str,
        tolerance_shares: f64,
        limit: usize,
    ) -> Result<Vec<MmInventoryReconcileRow>> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let tolerance_shares = tolerance_shares.max(0.0);
        let snapshot = self.snapshot_mm_inventory_drift(strategy_id.as_str(), limit)?;
        let ts_ms = chrono::Utc::now().timestamp_millis();
        let mut out = Vec::new();
        for row in snapshot.rows {
            let wallet_shares = row.wallet_inventory_shares.unwrap_or(0.0).max(0.0);
            let drift = row
                .inventory_drift_shares
                .unwrap_or(wallet_shares - row.db_inventory_shares);
            if !drift.is_finite() || drift.abs() <= tolerance_shares {
                continue;
            }
            if drift >= 0.0 {
                continue;
            }
            let requested_consume_shares = (-drift).max(0.0);
            if requested_consume_shares <= 1e-9 {
                continue;
            }
            let consume_key = format!(
                "wallet_reconcile:{}:{}:{}:{}",
                strategy_id, row.condition_id, row.token_id, ts_ms
            );
            let reason = json!({
                "reconcile": true,
                "wallet_inventory_shares": wallet_shares,
                "db_inventory_shares_before": row.db_inventory_shares,
                "inventory_drift_shares": drift,
                "last_wallet_activity_ts_ms": row.last_wallet_activity_ts_ms,
                "last_wallet_activity_type": row.last_wallet_activity_type
            })
            .to_string();
            let result = self.record_inventory_consumption(
                consume_key.as_str(),
                strategy_id.as_str(),
                row.condition_id.as_str(),
                row.token_id.as_str(),
                requested_consume_shares,
                Some(wallet_shares),
                "wallet_inventory_reconcile",
                Some(reason.as_str()),
                ts_ms,
            )?;
            out.push(MmInventoryReconcileRow {
                consume_key: result.consume_key,
                action: "consume".to_string(),
                condition_id: row.condition_id,
                token_id: row.token_id,
                market_slug: row.market_slug,
                timeframe: row.timeframe,
                symbol: row.symbol,
                mode: row.mode,
                db_inventory_shares_before: row.db_inventory_shares,
                wallet_inventory_shares: wallet_shares,
                drift_shares_before: drift,
                requested_consume_shares,
                applied_consume_shares: result.applied_units,
                requested_add_shares: 0.0,
                applied_add_shares: 0.0,
                db_inventory_shares_after: result.db_units_after,
                applied_cost_usd: result.applied_cost_usd,
                last_wallet_activity_ts_ms: row.last_wallet_activity_ts_ms,
                last_wallet_activity_type: row.last_wallet_activity_type,
            });
        }
        Ok(out)
    }

    pub fn summarize_mm_market_attribution(
        &self,
        strategy_id: &str,
        from_ts_ms: Option<i64>,
    ) -> Result<Vec<MmMarketAttributionRow>> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let from_ts_ms = from_ts_ms.unwrap_or(0);
        #[derive(Default, Clone)]
        struct ConditionWalletAgg {
            db_inventory_shares: f64,
            wallet_inventory_shares: Option<f64>,
            inventory_drift_shares: Option<f64>,
            wallet_inventory_value_live_usd: Option<f64>,
            wallet_inventory_value_bid_usd: Option<f64>,
            wallet_inventory_value_mid_usd: Option<f64>,
            last_wallet_activity_ts_ms: Option<i64>,
            last_wallet_activity_type: Option<String>,
        }

        let inventory_snapshot = self
            .get_mm_inventory_drift_snapshot(strategy_id.as_str(), 10_000)
            .unwrap_or_default();
        let mut wallet_by_condition: HashMap<String, ConditionWalletAgg> = HashMap::new();
        for row in inventory_snapshot.rows.iter() {
            let entry = wallet_by_condition
                .entry(row.condition_id.clone())
                .or_default();
            entry.db_inventory_shares += row.db_inventory_shares;
            sum_option_f64(
                &mut entry.wallet_inventory_shares,
                row.wallet_inventory_shares,
            );
            sum_option_f64(
                &mut entry.inventory_drift_shares,
                row.inventory_drift_shares,
            );
            sum_option_f64(
                &mut entry.wallet_inventory_value_live_usd,
                row.wallet_current_value_usd,
            );
            sum_option_f64(
                &mut entry.wallet_inventory_value_bid_usd,
                row.wallet_inventory_value_bid_usd,
            );
            sum_option_f64(
                &mut entry.wallet_inventory_value_mid_usd,
                row.wallet_inventory_value_mid_usd,
            );
            let should_replace_activity = match (
                entry.last_wallet_activity_ts_ms,
                row.last_wallet_activity_ts_ms,
            ) {
                (_, None) => false,
                (None, Some(_)) => true,
                (Some(current), Some(next)) => next >= current,
            };
            if should_replace_activity {
                entry.last_wallet_activity_ts_ms = row.last_wallet_activity_ts_ms;
                entry.last_wallet_activity_type = row.last_wallet_activity_type.clone();
            }
        }
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
WITH market_meta AS (
    SELECT
        condition_id,
        MAX(market_slug) AS market_slug,
        MAX(timeframe) AS timeframe,
        MAX(symbol) AS symbol,
        MAX(mode) AS mode
    FROM mm_market_states_v1
    WHERE strategy_id=?1
    GROUP BY condition_id
),
settled AS (
    SELECT
        condition_id,
        COUNT(*) AS settled_trades,
        COALESCE(SUM(settled_pnl_usd), 0.0) AS trade_realized_pnl_usd
    FROM settlements_v2
    WHERE strategy_id=?1
      AND ts_ms>=?2
      AND condition_id IS NOT NULL
      AND TRIM(condition_id) <> ''
    GROUP BY condition_id
),
cashflow AS (
    SELECT
        condition_id,
        SUM(CASE WHEN event_type='MERGE_SUCCESS' THEN 1 ELSE 0 END) AS merge_events,
        COALESCE(SUM(CASE WHEN event_type='MERGE_SUCCESS' THEN COALESCE(amount_usd, 0.0) ELSE 0.0 END), 0.0) AS merge_cashflow_usd,
        SUM(CASE WHEN event_type IN ('MAKER_REBATE', 'LIQUIDITY_REWARD', 'REWARD') THEN 1 ELSE 0 END) AS reward_events,
        COALESCE(SUM(CASE WHEN event_type IN ('MAKER_REBATE', 'LIQUIDITY_REWARD', 'REWARD') THEN COALESCE(amount_usd, 0.0) ELSE 0.0 END), 0.0) AS reward_cashflow_usd,
        SUM(CASE WHEN event_type IN ('REDEMPTION_SUCCESS', 'REDEMPTION_ALREADY_REDEEMED', 'REDEMPTION_CONFIRMED') THEN 1 ELSE 0 END) AS redemption_events,
        COALESCE(SUM(CASE WHEN event_type IN ('REDEMPTION_SUCCESS', 'REDEMPTION_ALREADY_REDEEMED', 'REDEMPTION_CONFIRMED') THEN COALESCE(amount_usd, 0.0) ELSE 0.0 END), 0.0) AS redemption_cashflow_usd
    FROM wallet_cashflow_events_v1
    WHERE strategy_id=?1
      AND ts_ms>=?2
      AND condition_id IS NOT NULL
      AND TRIM(condition_id) <> ''
    GROUP BY condition_id
),
open_positions AS (
    SELECT
        condition_id,
        SUM(CASE WHEN remaining_units > 1e-9 THEN 1 ELSE 0 END) AS open_positions,
        COALESCE(SUM(remaining_units), 0.0) AS open_entry_units,
        COALESCE(SUM(remaining_cost_usd), 0.0) AS open_entry_notional_usd,
        COALESCE(SUM(realized_pnl_usd), 0.0) AS open_realized_pnl_usd
    FROM (
        SELECT
            condition_id,
            CASE
                WHEN (
                    COALESCE(entry_units, 0.0)
                    - COALESCE(exit_units, 0.0)
                    - COALESCE(inventory_consumed_units, 0.0)
                ) > 0.0
                    THEN (
                        COALESCE(entry_units, 0.0)
                        - COALESCE(exit_units, 0.0)
                        - COALESCE(inventory_consumed_units, 0.0)
                    )
                ELSE 0.0
            END AS remaining_units,
            CASE
                WHEN COALESCE(entry_units, 0.0) > 1e-9
                    THEN (
                        COALESCE(entry_notional_usd, 0.0)
                        + COALESCE(entry_fee_usd, 0.0)
                    ) * (
                        CASE
                            WHEN (
                                COALESCE(entry_units, 0.0)
                                - COALESCE(exit_units, 0.0)
                                - COALESCE(inventory_consumed_units, 0.0)
                            ) > 0.0
                                THEN (
                                    COALESCE(entry_units, 0.0)
                                    - COALESCE(exit_units, 0.0)
                                    - COALESCE(inventory_consumed_units, 0.0)
                                )
                            ELSE 0.0
                        END
                    ) / COALESCE(entry_units, 0.0)
                ELSE 0.0
            END AS remaining_cost_usd,
            COALESCE(realized_pnl_usd, 0.0) AS realized_pnl_usd
        FROM positions_v2
        WHERE strategy_id=?1
          AND status <> 'SETTLED'
          AND condition_id IS NOT NULL
          AND TRIM(condition_id) <> ''
    ) pos
    GROUP BY condition_id
),
keys AS (
    SELECT condition_id FROM market_meta
    UNION
    SELECT condition_id FROM settled
    UNION
    SELECT condition_id FROM cashflow
    UNION
    SELECT condition_id FROM open_positions
)
SELECT
    keys.condition_id,
    market_meta.market_slug,
    market_meta.timeframe,
    market_meta.symbol,
    market_meta.mode,
    COALESCE(settled.settled_trades, 0) AS settled_trades,
    COALESCE(settled.trade_realized_pnl_usd, 0.0) AS trade_realized_pnl_usd,
    COALESCE(cashflow.merge_events, 0) AS merge_events,
    COALESCE(cashflow.merge_cashflow_usd, 0.0) AS merge_cashflow_usd,
    COALESCE(cashflow.reward_events, 0) AS reward_events,
    COALESCE(cashflow.reward_cashflow_usd, 0.0) AS reward_cashflow_usd,
    COALESCE(cashflow.redemption_events, 0) AS redemption_events,
    COALESCE(cashflow.redemption_cashflow_usd, 0.0) AS redemption_cashflow_usd,
    COALESCE(open_positions.open_positions, 0) AS open_positions,
    COALESCE(open_positions.open_entry_units, 0.0) AS open_entry_units,
    COALESCE(open_positions.open_entry_notional_usd, 0.0) AS open_entry_notional_usd,
    COALESCE(open_positions.open_realized_pnl_usd, 0.0) AS open_realized_pnl_usd,
    COALESCE(settled.trade_realized_pnl_usd, 0.0)
      + COALESCE(cashflow.merge_cashflow_usd, 0.0)
      + COALESCE(cashflow.reward_cashflow_usd, 0.0)
      + COALESCE(cashflow.redemption_cashflow_usd, 0.0) AS net_pnl_usd
FROM keys
LEFT JOIN market_meta ON market_meta.condition_id = keys.condition_id
LEFT JOIN settled ON settled.condition_id = keys.condition_id
LEFT JOIN cashflow ON cashflow.condition_id = keys.condition_id
LEFT JOIN open_positions ON open_positions.condition_id = keys.condition_id
ORDER BY net_pnl_usd DESC, keys.condition_id ASC
"#,
            )?;
            let rows = stmt.query_map(params![strategy_id, from_ts_ms], |row| {
                let condition_id: String = row.get(0)?;
                Ok((
                    condition_id,
                    MmMarketAttributionRow {
                        condition_id: row.get(0)?,
                        market_slug: row.get(1)?,
                        timeframe: row.get(2)?,
                        symbol: row.get(3)?,
                        mode: row.get(4)?,
                        settled_trades: row.get::<_, i64>(5)?.max(0) as u64,
                        trade_realized_pnl_usd: row.get(6)?,
                        merge_events: row.get::<_, i64>(7)?.max(0) as u64,
                        merge_cashflow_usd: row.get(8)?,
                        reward_events: row.get::<_, i64>(9)?.max(0) as u64,
                        reward_cashflow_usd: row.get(10)?,
                        redemption_events: row.get::<_, i64>(11)?.max(0) as u64,
                        redemption_cashflow_usd: row.get(12)?,
                        open_positions: row.get::<_, i64>(13)?.max(0) as u64,
                        open_entry_units: row.get(14)?,
                        open_entry_notional_usd: row.get(15)?,
                        open_realized_pnl_usd: row.get(16)?,
                        db_inventory_shares: 0.0,
                        wallet_inventory_shares: None,
                        inventory_drift_shares: None,
                        wallet_inventory_value_live_usd: None,
                        wallet_inventory_value_bid_usd: None,
                        wallet_inventory_value_mid_usd: None,
                        last_wallet_activity_ts_ms: None,
                        last_wallet_activity_type: None,
                        net_pnl_usd: row.get(17)?,
                    },
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (condition_id, mut row) = row?;
                if let Some(wallet) = wallet_by_condition.remove(condition_id.as_str()) {
                    row.db_inventory_shares = wallet.db_inventory_shares;
                    row.wallet_inventory_shares = wallet.wallet_inventory_shares;
                    row.inventory_drift_shares = wallet.inventory_drift_shares;
                    row.wallet_inventory_value_live_usd = wallet.wallet_inventory_value_live_usd;
                    row.wallet_inventory_value_bid_usd = wallet.wallet_inventory_value_bid_usd;
                    row.wallet_inventory_value_mid_usd = wallet.wallet_inventory_value_mid_usd;
                    row.last_wallet_activity_ts_ms = wallet.last_wallet_activity_ts_ms;
                    row.last_wallet_activity_type = wallet.last_wallet_activity_type;
                } else {
                    row.db_inventory_shares = row.open_entry_units;
                }
                out.push(row);
            }
            for (condition_id, wallet) in wallet_by_condition.into_iter() {
                out.push(MmMarketAttributionRow {
                    condition_id,
                    market_slug: None,
                    timeframe: None,
                    symbol: None,
                    mode: None,
                    settled_trades: 0,
                    trade_realized_pnl_usd: 0.0,
                    merge_events: 0,
                    merge_cashflow_usd: 0.0,
                    reward_events: 0,
                    reward_cashflow_usd: 0.0,
                    redemption_events: 0,
                    redemption_cashflow_usd: 0.0,
                    open_positions: 0,
                    open_entry_units: wallet.db_inventory_shares,
                    open_entry_notional_usd: 0.0,
                    open_realized_pnl_usd: 0.0,
                    db_inventory_shares: wallet.db_inventory_shares,
                    wallet_inventory_shares: wallet.wallet_inventory_shares,
                    inventory_drift_shares: wallet.inventory_drift_shares,
                    wallet_inventory_value_live_usd: wallet.wallet_inventory_value_live_usd,
                    wallet_inventory_value_bid_usd: wallet.wallet_inventory_value_bid_usd,
                    wallet_inventory_value_mid_usd: wallet.wallet_inventory_value_mid_usd,
                    last_wallet_activity_ts_ms: wallet.last_wallet_activity_ts_ms,
                    last_wallet_activity_type: wallet.last_wallet_activity_type,
                    net_pnl_usd: 0.0,
                });
            }
            out.sort_by(|a, b| {
                b.net_pnl_usd
                    .partial_cmp(&a.net_pnl_usd)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.condition_id.cmp(&b.condition_id))
            });
            Ok(out)
        })
    }

    pub fn upsert_strategy_feature_snapshot_intent(
        &self,
        row: &StrategyFeatureSnapshotIntentRecord,
    ) -> Result<String> {
        let strategy_id = normalize_strategy_id(row.strategy_id.as_str());
        let timeframe = row.timeframe.trim().to_ascii_lowercase();
        let token_id = row.token_id.trim().to_string();
        let impulse_id = row.impulse_id.trim().to_string();
        if timeframe.is_empty() {
            return Err(anyhow!("strategy feature snapshot timeframe is required"));
        }
        if token_id.is_empty() {
            return Err(anyhow!("strategy feature snapshot token_id is required"));
        }
        if impulse_id.is_empty() {
            return Err(anyhow!("strategy feature snapshot impulse_id is required"));
        }
        let market_key =
            normalize_market_key(row.market_key.as_deref(), row.asset_symbol.as_deref());
        let asset_symbol =
            normalize_asset_symbol(row.asset_symbol.as_deref(), None, Some(market_key.as_str()));
        let snapshot_id = make_strategy_feature_snapshot_id(
            strategy_id.as_str(),
            timeframe.as_str(),
            row.period_timestamp,
            market_key.as_str(),
            token_id.as_str(),
            impulse_id.as_str(),
            row.decision_id.as_deref(),
            row.rung_id.as_deref(),
            row.slice_id.as_deref(),
        );
        let decision_id = normalize_snapshot_optional_payload(row.decision_id.as_deref());
        let rung_id = normalize_snapshot_optional_payload(row.rung_id.as_deref());
        let slice_id = normalize_snapshot_optional_payload(row.slice_id.as_deref());
        let request_id = normalize_snapshot_optional_payload(row.request_id.as_deref());
        let order_id = normalize_snapshot_optional_payload(row.order_id.as_deref());
        let trade_key = normalize_snapshot_optional_payload(row.trade_key.as_deref());
        let position_key = normalize_snapshot_optional_payload(row.position_key.as_deref());
        let condition_id = normalize_snapshot_optional_payload(row.condition_id.as_deref());
        let feature_json = normalize_snapshot_optional_payload(row.feature_json.as_deref());
        let decision_context_json =
            normalize_snapshot_optional_payload(row.decision_context_json.as_deref());
        let execution_context_json =
            normalize_snapshot_optional_payload(row.execution_context_json.as_deref());
        let intent_status = normalize_snapshot_status(row.intent_status.as_deref(), "planned");
        let execution_status = normalize_snapshot_status(row.execution_status.as_deref(), "none");
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.with_conn(|conn| {
            let mut effective_intent_status = intent_status.clone();
            let mut effective_execution_status = execution_status.clone();
            let existing: Option<(String, String)> = conn
                .query_row(
                    "SELECT intent_status, execution_status FROM strategy_feature_snapshots_v1 WHERE snapshot_id=?1",
                    params![snapshot_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((current_intent_status, current_execution_status)) = existing {
                let next_intent_status = effective_intent_status.as_str();
                let next_execution_status = effective_execution_status.as_str();
                let intent_ok =
                    is_valid_intent_status_transition(current_intent_status.as_str(), next_intent_status);
                let execution_ok = is_valid_execution_status_transition(
                    current_execution_status.as_str(),
                    next_execution_status,
                );
                if !intent_ok
                    && is_stale_intent_downgrade(
                        current_intent_status.as_str(),
                        next_intent_status,
                    )
                {
                    effective_intent_status = current_intent_status.clone();
                }
                if !execution_ok
                    && is_stale_execution_downgrade(
                        current_execution_status.as_str(),
                        next_execution_status,
                    )
                {
                    effective_execution_status = current_execution_status.clone();
                }
                let final_intent_ok = is_valid_intent_status_transition(
                    current_intent_status.as_str(),
                    effective_intent_status.as_str(),
                );
                let final_execution_ok = is_valid_execution_status_transition(
                    current_execution_status.as_str(),
                    effective_execution_status.as_str(),
                );
                if !final_intent_ok || !final_execution_ok {
                    let reason = format!(
                        "invalid snapshot status transition intent:{}->{} execution:{}->{}",
                        current_intent_status,
                        effective_intent_status,
                        current_execution_status,
                        effective_execution_status
                    );
                    conn.execute(
                        r#"
INSERT INTO strategy_feature_snapshot_transition_rejections (
    snapshot_id, strategy_id, timeframe, period_timestamp,
    current_intent_status, next_intent_status,
    current_execution_status, next_execution_status,
    reason, rejected_at_ms
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
"#,
                        params![
                            snapshot_id.as_str(),
                            strategy_id.as_str(),
                            timeframe.as_str(),
                            row.period_timestamp as i64,
                            current_intent_status,
                            effective_intent_status.as_str(),
                            current_execution_status,
                            effective_execution_status.as_str(),
                            reason,
                            now_ms
                        ],
                    )?;
                    return Err(anyhow!(
                        "rejected strategy feature snapshot transition for {}",
                        snapshot_id
                    ));
                }
            }
            conn.execute(
                r#"
INSERT INTO strategy_feature_snapshots_v1 (
    snapshot_id,
    strategy_id,
    timeframe,
    period_timestamp,
    market_key,
    asset_symbol,
    condition_id,
    token_id,
    impulse_id,
    decision_id,
    rung_id,
    slice_id,
    request_id,
    order_id,
    trade_key,
    position_key,
    intent_status,
    execution_status,
    intent_ts_ms,
    submit_ts_ms,
    ack_ts_ms,
    fill_ts_ms,
    exit_ts_ms,
    settled_ts_ms,
    entry_notional_usd,
    exit_notional_usd,
    realized_pnl_usd,
    settled_pnl_usd,
    hold_ms,
    feature_json,
    decision_context_json,
    execution_context_json,
    created_at_ms,
    updated_at_ms
) VALUES (
    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
    ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
    ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
    ?31, ?32, ?33, ?34
)
ON CONFLICT(snapshot_id) DO UPDATE SET
    asset_symbol=excluded.asset_symbol,
    condition_id=COALESCE(excluded.condition_id, strategy_feature_snapshots_v1.condition_id),
    request_id=COALESCE(excluded.request_id, strategy_feature_snapshots_v1.request_id),
    order_id=COALESCE(excluded.order_id, strategy_feature_snapshots_v1.order_id),
    trade_key=COALESCE(excluded.trade_key, strategy_feature_snapshots_v1.trade_key),
    position_key=COALESCE(excluded.position_key, strategy_feature_snapshots_v1.position_key),
    intent_status=excluded.intent_status,
    execution_status=excluded.execution_status,
    intent_ts_ms=MIN(strategy_feature_snapshots_v1.intent_ts_ms, excluded.intent_ts_ms),
    submit_ts_ms=CASE
        WHEN excluded.submit_ts_ms IS NULL THEN strategy_feature_snapshots_v1.submit_ts_ms
        WHEN strategy_feature_snapshots_v1.submit_ts_ms IS NULL THEN excluded.submit_ts_ms
        ELSE MIN(strategy_feature_snapshots_v1.submit_ts_ms, excluded.submit_ts_ms)
    END,
    ack_ts_ms=CASE
        WHEN excluded.ack_ts_ms IS NULL THEN strategy_feature_snapshots_v1.ack_ts_ms
        WHEN strategy_feature_snapshots_v1.ack_ts_ms IS NULL THEN excluded.ack_ts_ms
        ELSE MIN(strategy_feature_snapshots_v1.ack_ts_ms, excluded.ack_ts_ms)
    END,
    fill_ts_ms=CASE
        WHEN excluded.fill_ts_ms IS NULL THEN strategy_feature_snapshots_v1.fill_ts_ms
        WHEN strategy_feature_snapshots_v1.fill_ts_ms IS NULL THEN excluded.fill_ts_ms
        ELSE MIN(strategy_feature_snapshots_v1.fill_ts_ms, excluded.fill_ts_ms)
    END,
    exit_ts_ms=CASE
        WHEN excluded.exit_ts_ms IS NULL THEN strategy_feature_snapshots_v1.exit_ts_ms
        WHEN strategy_feature_snapshots_v1.exit_ts_ms IS NULL THEN excluded.exit_ts_ms
        ELSE MIN(strategy_feature_snapshots_v1.exit_ts_ms, excluded.exit_ts_ms)
    END,
    settled_ts_ms=CASE
        WHEN excluded.settled_ts_ms IS NULL THEN strategy_feature_snapshots_v1.settled_ts_ms
        WHEN strategy_feature_snapshots_v1.settled_ts_ms IS NULL THEN excluded.settled_ts_ms
        ELSE MIN(strategy_feature_snapshots_v1.settled_ts_ms, excluded.settled_ts_ms)
    END,
    entry_notional_usd=COALESCE(excluded.entry_notional_usd, strategy_feature_snapshots_v1.entry_notional_usd),
    exit_notional_usd=COALESCE(excluded.exit_notional_usd, strategy_feature_snapshots_v1.exit_notional_usd),
    realized_pnl_usd=COALESCE(excluded.realized_pnl_usd, strategy_feature_snapshots_v1.realized_pnl_usd),
    settled_pnl_usd=COALESCE(excluded.settled_pnl_usd, strategy_feature_snapshots_v1.settled_pnl_usd),
    hold_ms=COALESCE(excluded.hold_ms, strategy_feature_snapshots_v1.hold_ms),
    feature_json=COALESCE(excluded.feature_json, strategy_feature_snapshots_v1.feature_json),
    decision_context_json=COALESCE(excluded.decision_context_json, strategy_feature_snapshots_v1.decision_context_json),
    execution_context_json=COALESCE(excluded.execution_context_json, strategy_feature_snapshots_v1.execution_context_json),
    updated_at_ms=excluded.updated_at_ms
"#,
                params![
                    snapshot_id,
                    strategy_id,
                    timeframe,
                    row.period_timestamp as i64,
                    market_key,
                    asset_symbol,
                    condition_id,
                    token_id,
                    impulse_id,
                    decision_id,
                    rung_id,
                    slice_id,
                    request_id,
                    order_id,
                    trade_key,
                    position_key,
                    effective_intent_status,
                    effective_execution_status,
                    row.intent_ts_ms,
                    row.submit_ts_ms,
                    row.ack_ts_ms,
                    row.fill_ts_ms,
                    row.exit_ts_ms,
                    row.settled_ts_ms,
                    row.entry_notional_usd,
                    row.exit_notional_usd,
                    row.realized_pnl_usd,
                    row.settled_pnl_usd,
                    row.hold_ms,
                    feature_json,
                    decision_context_json,
                    execution_context_json,
                    now_ms,
                    now_ms,
                ],
            )?;
            Ok(())
        })?;
        Ok(snapshot_id)
    }

    fn find_strategy_feature_snapshot_scope_for_event(
        &self,
        strategy_id: &str,
        timeframe: &str,
        period_timestamp: u64,
        token_id: &str,
        order_id: Option<&str>,
        trade_key: Option<&str>,
        request_id: Option<&str>,
    ) -> Result<Option<StrategyFeatureSnapshotScopeMatch>> {
        if order_id.is_none() && trade_key.is_none() && request_id.is_none() {
            return Ok(None);
        }
        self.with_conn(|conn| {
            conn.query_row(
                r#"
SELECT
    market_key,
    asset_symbol,
    condition_id,
    token_id,
    impulse_id,
    decision_id,
    rung_id,
    slice_id,
    request_id,
    order_id,
    trade_key,
    position_key
FROM strategy_feature_snapshots_v1
WHERE strategy_id=?1
  AND timeframe=?2
  AND period_timestamp=?3
  AND token_id=?4
  AND (
        (?5 IS NOT NULL AND order_id=?5)
     OR (?6 IS NOT NULL AND trade_key=?6)
     OR (?7 IS NOT NULL AND request_id=?7)
  )
ORDER BY
    CASE
        WHEN (?5 IS NOT NULL AND order_id=?5) THEN 0
        WHEN (?6 IS NOT NULL AND trade_key=?6) THEN 1
        WHEN (?7 IS NOT NULL AND request_id=?7) THEN 2
        ELSE 3
    END,
    updated_at_ms DESC
LIMIT 1
"#,
                params![
                    strategy_id,
                    timeframe,
                    period_timestamp as i64,
                    token_id,
                    order_id,
                    trade_key,
                    request_id
                ],
                |row| {
                    Ok(StrategyFeatureSnapshotScopeMatch {
                        market_key: row.get(0)?,
                        asset_symbol: row.get(1)?,
                        condition_id: row.get(2)?,
                        token_id: row.get(3)?,
                        impulse_id: row.get(4)?,
                        decision_id: row.get(5)?,
                        rung_id: row.get(6)?,
                        slice_id: row.get(7)?,
                        request_id: row.get(8)?,
                        order_id: row.get(9)?,
                        trade_key: row.get(10)?,
                        position_key: row.get(11)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
        })
    }

    pub fn upsert_strategy_feature_snapshot_from_trade_event(
        &self,
        event: &TradeEventRecord,
    ) -> Result<Option<String>> {
        let timeframe = event.timeframe.trim().to_ascii_lowercase();
        if timeframe.is_empty() {
            return Ok(None);
        }
        let token_id = event
            .token_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string);
        let Some(token_id) = token_id else {
            return Ok(None);
        };
        let strategy_id = normalize_strategy_id(event.strategy_id.as_str());
        let market_key = normalize_market_key(None, event.asset_symbol.as_deref());
        let asset_symbol = normalize_asset_symbol(
            event.asset_symbol.as_deref(),
            event.token_type.as_deref(),
            Some(market_key.as_str()),
        );
        let order_id = extract_order_id_from_trade_event(event);
        let trade_key = extract_trade_key_from_trade_event(event);
        let request_id = extract_request_id_from_trade_event(event);
        let event_type = event.event_type.trim().to_ascii_uppercase();
        if event_type == "ENTRY_FILL_RECONCILED" {
            let apply_reconciled_monetary =
                self.with_conn(|conn| should_apply_reconciled_fill_monetary(conn, event))?;
            if !apply_reconciled_monetary {
                // Canonical ENTRY_FILL already exists for this order; keep reconciled
                // row as audit-only and avoid duplicating snapshot notional/timestamps.
                return Ok(None);
            }
        }
        let scope_match = self.find_strategy_feature_snapshot_scope_for_event(
            strategy_id.as_str(),
            timeframe.as_str(),
            event.period_timestamp,
            token_id.as_str(),
            order_id.as_deref(),
            trade_key.as_deref(),
            request_id.as_deref(),
        )?;
        let snapback_requires_scope = false;
        if snapback_requires_scope && scope_match.is_none() {
            return Ok(None);
        }
        let market_key = scope_match
            .as_ref()
            .and_then(|scope| scope.market_key.clone())
            .unwrap_or(market_key);
        let asset_symbol = scope_match
            .as_ref()
            .and_then(|scope| scope.asset_symbol.clone())
            .unwrap_or(asset_symbol);
        let condition_id = event.condition_id.clone().or_else(|| {
            scope_match
                .as_ref()
                .and_then(|scope| scope.condition_id.clone())
        });
        let token_id = scope_match
            .as_ref()
            .map(|scope| scope.token_id.clone())
            .unwrap_or(token_id);
        let impulse_id = scope_match.as_ref().map_or_else(
            || {
                infer_impulse_id_from_trade_event(
                    strategy_id.as_str(),
                    timeframe.as_str(),
                    event.period_timestamp,
                    token_id.as_str(),
                    event,
                    order_id.as_deref(),
                    trade_key.as_deref(),
                )
            },
            |scope| scope.impulse_id.clone(),
        );
        let decision_id = scope_match
            .as_ref()
            .and_then(|scope| scope.decision_id.clone());
        let rung_id = scope_match.as_ref().and_then(|scope| scope.rung_id.clone());
        let slice_id = scope_match
            .as_ref()
            .and_then(|scope| scope.slice_id.clone());
        let merged_request_id = request_id.clone().or_else(|| {
            scope_match
                .as_ref()
                .and_then(|scope| scope.request_id.clone())
        });
        let merged_order_id = order_id.clone().or_else(|| {
            scope_match
                .as_ref()
                .and_then(|scope| scope.order_id.clone())
        });
        let merged_trade_key = trade_key.clone().or_else(|| {
            scope_match
                .as_ref()
                .and_then(|scope| scope.trade_key.clone())
        });
        let position_key = scope_match
            .as_ref()
            .and_then(|scope| scope.position_key.clone());
        let (intent_status, execution_status) = infer_snapshot_status_from_event(event);
        let is_market_close =
            event_type == "EXIT" && is_market_close_reason(event.reason.as_deref());
        let submitted_at_ms =
            parse_reason_json_i64_field(event.reason.as_deref(), "submitted_at_ms");
        let acked_at_ms = parse_reason_json_i64_field(event.reason.as_deref(), "acked_at_ms");
        let runtime_order_to_fill_latency_ms = parse_reason_nested_json_i64_field(
            event.reason.as_deref(),
            "reason",
            "order_to_fill_latency_ms",
        )
        .or_else(|| {
            parse_reason_json_i64_field(event.reason.as_deref(), "order_to_fill_latency_ms")
        })
        .and_then(|v| if v >= 0 { Some(v) } else { None });
        let submit_ts_ms = match event_type.as_str() {
            "ENTRY_SUBMIT" => Some(event.ts_ms),
            "ENTRY_ACK" => submitted_at_ms.or(Some(event.ts_ms)),
            "ENTRY_FILL" | "ENTRY_FILL_RECONCILED" => scope_match
                .is_none()
                .then(|| {
                    runtime_order_to_fill_latency_ms.map(|lat| event.ts_ms.saturating_sub(lat))
                })
                .flatten(),
            _ => None,
        };
        let ack_ts_ms = match event_type.as_str() {
            "ENTRY_ACK" => acked_at_ms.or(Some(event.ts_ms)),
            _ => None,
        };
        let fill_ts_ms = match event_type.as_str() {
            "ENTRY_FILL" | "ENTRY_FILL_RECONCILED" => Some(event.ts_ms),
            _ => None,
        };
        let exit_ts_ms = match event_type.as_str() {
            "EXIT" | "EXIT_SIGNAL" | "DISTRIBUTION_EXIT_FILLED" => Some(event.ts_ms),
            _ => None,
        };
        let settled_ts_ms = if is_market_close {
            Some(event.ts_ms)
        } else {
            None
        };
        let entry_notional_usd = match event_type.as_str() {
            "ENTRY_SUBMIT" | "ENTRY_ACK" | "ENTRY_FILL" | "ENTRY_FILL_RECONCILED" => {
                event.notional_usd
            }
            _ => None,
        };
        let exit_notional_usd = match event_type.as_str() {
            "EXIT" | "EXIT_SIGNAL" | "DISTRIBUTION_EXIT_FILLED" => event.notional_usd,
            _ => None,
        };
        let execution_context_json = serde_json::to_string(&json!({
            "event_type": event_type,
            "side": event.side,
            "token_type": event.token_type,
            "reason": event.reason
        }))
        .ok();
        let snapshot_id =
            self.upsert_strategy_feature_snapshot_intent(&StrategyFeatureSnapshotIntentRecord {
                strategy_id,
                timeframe,
                period_timestamp: event.period_timestamp,
                market_key: Some(market_key),
                asset_symbol: Some(asset_symbol),
                condition_id,
                token_id,
                impulse_id,
                decision_id,
                rung_id,
                slice_id,
                request_id: merged_request_id,
                order_id: merged_order_id,
                trade_key: merged_trade_key,
                position_key,
                intent_status: Some(intent_status.to_string()),
                execution_status: Some(execution_status.to_string()),
                intent_ts_ms: event.ts_ms,
                submit_ts_ms,
                ack_ts_ms,
                fill_ts_ms,
                exit_ts_ms,
                settled_ts_ms,
                entry_notional_usd,
                exit_notional_usd,
                realized_pnl_usd: event.pnl_usd,
                settled_pnl_usd: if is_market_close { event.pnl_usd } else { None },
                hold_ms: None,
                feature_json: None,
                decision_context_json: None,
                execution_context_json,
            })?;
        Ok(Some(snapshot_id))
    }

    pub fn upsert_snapback_feature_snapshot_intent(
        &self,
        row: &SnapbackFeatureSnapshotIntentRecord,
    ) -> Result<()> {
        let strategy_id = normalize_strategy_id(row.strategy_id.as_str());
        let timeframe = row.timeframe.trim().to_ascii_lowercase();
        let token_id = row.token_id.trim().to_string();
        let impulse_id = row.impulse_id.trim().to_string();
        let condition_id = row.condition_id.trim().to_string();
        if timeframe.is_empty() {
            return Err(anyhow!("snapback snapshot timeframe is required"));
        }
        if token_id.is_empty() {
            return Err(anyhow!("snapback snapshot token_id is required"));
        }
        if impulse_id.is_empty() {
            return Err(anyhow!("snapback snapshot impulse_id is required"));
        }
        if condition_id.is_empty() {
            return Err(anyhow!("snapback snapshot condition_id is required"));
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.with_conn(|conn| {
            conn.execute(
                r#"
INSERT INTO snapback_feature_snapshots_v1 (
    strategy_id, timeframe, period_timestamp, condition_id, token_id, impulse_id, position_key, entry_side,
    intent_ts_ms, request_id, order_id,
    signed_notional_30s, signed_notional_300s, signed_notional_900s,
    burst_zscore, side_imbalance_5m, impulse_strength,
    trigger_signed_flow, trigger_flow_source,
    fair_probability, reservation_price, max_price, limit_price, edge_bps, target_size_usd,
    bias_alignment, bias_confidence, created_at_ms, updated_at_ms
) VALUES (
    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
    ?9, ?10, ?11,
    ?12, ?13, ?14,
    ?15, ?16, ?17,
    ?18, ?19,
    ?20, ?21, ?22, ?23, ?24, ?25,
    ?26, ?27, ?28, ?29
)
ON CONFLICT(strategy_id, timeframe, period_timestamp, token_id, impulse_id) DO UPDATE SET
    condition_id=COALESCE(excluded.condition_id, snapback_feature_snapshots_v1.condition_id),
    position_key=COALESCE(excluded.position_key, snapback_feature_snapshots_v1.position_key),
    entry_side=COALESCE(excluded.entry_side, snapback_feature_snapshots_v1.entry_side),
    intent_ts_ms=MIN(snapback_feature_snapshots_v1.intent_ts_ms, excluded.intent_ts_ms),
    request_id=COALESCE(excluded.request_id, snapback_feature_snapshots_v1.request_id),
    order_id=COALESCE(excluded.order_id, snapback_feature_snapshots_v1.order_id),
    signed_notional_30s=COALESCE(excluded.signed_notional_30s, snapback_feature_snapshots_v1.signed_notional_30s),
    signed_notional_300s=COALESCE(excluded.signed_notional_300s, snapback_feature_snapshots_v1.signed_notional_300s),
    signed_notional_900s=COALESCE(excluded.signed_notional_900s, snapback_feature_snapshots_v1.signed_notional_900s),
    burst_zscore=COALESCE(excluded.burst_zscore, snapback_feature_snapshots_v1.burst_zscore),
    side_imbalance_5m=COALESCE(excluded.side_imbalance_5m, snapback_feature_snapshots_v1.side_imbalance_5m),
    impulse_strength=excluded.impulse_strength,
    trigger_signed_flow=COALESCE(excluded.trigger_signed_flow, snapback_feature_snapshots_v1.trigger_signed_flow),
    trigger_flow_source=COALESCE(excluded.trigger_flow_source, snapback_feature_snapshots_v1.trigger_flow_source),
    fair_probability=COALESCE(excluded.fair_probability, snapback_feature_snapshots_v1.fair_probability),
    reservation_price=COALESCE(excluded.reservation_price, snapback_feature_snapshots_v1.reservation_price),
    max_price=COALESCE(excluded.max_price, snapback_feature_snapshots_v1.max_price),
    limit_price=COALESCE(excluded.limit_price, snapback_feature_snapshots_v1.limit_price),
    edge_bps=COALESCE(excluded.edge_bps, snapback_feature_snapshots_v1.edge_bps),
    target_size_usd=COALESCE(excluded.target_size_usd, snapback_feature_snapshots_v1.target_size_usd),
    bias_alignment=COALESCE(excluded.bias_alignment, snapback_feature_snapshots_v1.bias_alignment),
    bias_confidence=COALESCE(excluded.bias_confidence, snapback_feature_snapshots_v1.bias_confidence),
    updated_at_ms=excluded.updated_at_ms
"#,
                params![
                    strategy_id,
                    timeframe,
                    row.period_timestamp as i64,
                    condition_id,
                    token_id,
                    impulse_id,
                    row.position_key,
                    row.entry_side,
                    row.intent_ts_ms,
                    row.request_id,
                    row.order_id,
                    row.signed_notional_30s,
                    row.signed_notional_300s,
                    row.signed_notional_900s,
                    row.burst_zscore,
                    row.side_imbalance_5m,
                    row.impulse_strength,
                    row.trigger_signed_flow,
                    row.trigger_flow_source,
                    row.fair_probability,
                    row.reservation_price,
                    row.max_price,
                    row.limit_price,
                    row.edge_bps,
                    row.target_size_usd,
                    row.bias_alignment,
                    row.bias_confidence,
                    now_ms,
                    now_ms,
                ],
            )?;
            Ok(())
        })?;

        let feature_json = serde_json::to_string(&json!({
            "signed_notional_30s": row.signed_notional_30s,
            "signed_notional_300s": row.signed_notional_300s,
            "signed_notional_900s": row.signed_notional_900s,
            "burst_zscore": row.burst_zscore,
            "side_imbalance_5m": row.side_imbalance_5m,
            "impulse_strength": row.impulse_strength,
            "impulse_mode": row.impulse_mode,
            "impulse_size_multiplier": row.impulse_size_multiplier,
            "direction_policy_split": row.direction_policy_split,
            "trigger_signed_flow": row.trigger_signed_flow,
            "trigger_flow_source": row.trigger_flow_source,
            "fair_probability": row.fair_probability,
            "reservation_price": row.reservation_price,
            "max_price": row.max_price,
            "limit_price": row.limit_price,
            "edge_bps": row.edge_bps,
            "target_size_usd": row.target_size_usd,
            "bias_alignment": row.bias_alignment,
            "bias_confidence": row.bias_confidence
        }))
        .ok();
        let execution_status =
            infer_execution_status_from_snapback_row(row.order_id.as_deref(), None, None, None);
        self.upsert_strategy_feature_snapshot_intent(&StrategyFeatureSnapshotIntentRecord {
            strategy_id: strategy_id.clone(),
            timeframe: timeframe.clone(),
            period_timestamp: row.period_timestamp,
            market_key: Some(DEFAULT_MARKET_KEY.to_string()),
            asset_symbol: Some(DEFAULT_ASSET_SYMBOL.to_string()),
            condition_id: Some(condition_id),
            token_id,
            impulse_id,
            decision_id: None,
            rung_id: None,
            slice_id: None,
            request_id: row.request_id.clone(),
            order_id: row.order_id.clone(),
            trade_key: None,
            position_key: row.position_key.clone(),
            intent_status: Some("submitted".to_string()),
            execution_status: Some(execution_status.to_string()),
            intent_ts_ms: row.intent_ts_ms,
            submit_ts_ms: None,
            ack_ts_ms: None,
            fill_ts_ms: None,
            exit_ts_ms: None,
            settled_ts_ms: None,
            entry_notional_usd: row.target_size_usd,
            exit_notional_usd: None,
            realized_pnl_usd: None,
            settled_pnl_usd: None,
            hold_ms: None,
            feature_json,
            decision_context_json: None,
            execution_context_json: None,
        })?;
        Ok(())
    }

    pub fn patch_snapback_feature_snapshot_outcome_by_key(
        &self,
        key: &SnapbackFeatureSnapshotKey,
        patch: &SnapbackFeatureSnapshotOutcomePatch,
    ) -> Result<bool> {
        let strategy_id = normalize_strategy_id(key.strategy_id.as_str());
        let timeframe = key.timeframe.trim().to_ascii_lowercase();
        let token_id = key.token_id.trim().to_string();
        let impulse_id = key.impulse_id.trim().to_string();
        if timeframe.is_empty() || token_id.is_empty() || impulse_id.is_empty() {
            return Ok(false);
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        let updated = self.with_conn(|conn| {
            let updated = conn.execute(
                r#"
UPDATE snapback_feature_snapshots_v1
SET
    condition_id=COALESCE(?1, condition_id),
    position_key=COALESCE(?2, position_key),
    request_id=COALESCE(?3, request_id),
    order_id=COALESCE(?4, order_id),
    fill_ts_ms=CASE
        WHEN ?5 IS NULL THEN fill_ts_ms
        WHEN fill_ts_ms IS NULL THEN ?5
        ELSE MIN(fill_ts_ms, ?5)
    END,
    exit_ts_ms=CASE
        WHEN ?6 IS NULL THEN exit_ts_ms
        WHEN exit_ts_ms IS NULL THEN ?6
        ELSE MIN(exit_ts_ms, ?6)
    END,
    settled_ts_ms=CASE
        WHEN ?7 IS NULL THEN settled_ts_ms
        WHEN settled_ts_ms IS NULL THEN ?7
        ELSE MIN(settled_ts_ms, ?7)
    END,
    exit_reason=COALESCE(?8, exit_reason),
    realized_pnl_usd=COALESCE(?9, realized_pnl_usd),
    settled_pnl_usd=COALESCE(?10, settled_pnl_usd),
    hold_ms=COALESCE(?11, hold_ms),
    updated_at_ms=?12
WHERE strategy_id=?13
  AND timeframe=?14
  AND period_timestamp=?15
  AND token_id=?16
  AND impulse_id=?17
"#,
                params![
                    patch.condition_id,
                    patch.position_key,
                    patch.request_id,
                    patch.order_id,
                    patch.fill_ts_ms,
                    patch.exit_ts_ms,
                    patch.settled_ts_ms,
                    patch.exit_reason,
                    patch.realized_pnl_usd,
                    patch.settled_pnl_usd,
                    patch.hold_ms,
                    now_ms,
                    strategy_id,
                    timeframe,
                    key.period_timestamp as i64,
                    token_id,
                    impulse_id,
                ],
            )?;
            Ok(updated > 0)
        })?;
        if !updated {
            return Ok(false);
        }

        let execution_status = infer_execution_status_from_snapback_row(
            patch.order_id.as_deref(),
            patch.fill_ts_ms,
            patch.exit_ts_ms,
            patch.settled_ts_ms,
        );
        self.upsert_strategy_feature_snapshot_intent(&StrategyFeatureSnapshotIntentRecord {
            strategy_id,
            timeframe,
            period_timestamp: key.period_timestamp,
            market_key: Some(DEFAULT_MARKET_KEY.to_string()),
            asset_symbol: Some(DEFAULT_ASSET_SYMBOL.to_string()),
            condition_id: patch.condition_id.clone(),
            token_id,
            impulse_id,
            decision_id: None,
            rung_id: None,
            slice_id: None,
            request_id: patch.request_id.clone(),
            order_id: patch.order_id.clone(),
            trade_key: None,
            position_key: patch.position_key.clone(),
            intent_status: Some("submitted".to_string()),
            execution_status: Some(execution_status.to_string()),
            intent_ts_ms: now_ms,
            submit_ts_ms: None,
            ack_ts_ms: None,
            fill_ts_ms: patch.fill_ts_ms,
            exit_ts_ms: patch.exit_ts_ms,
            settled_ts_ms: patch.settled_ts_ms,
            entry_notional_usd: None,
            exit_notional_usd: None,
            realized_pnl_usd: patch.realized_pnl_usd,
            settled_pnl_usd: patch.settled_pnl_usd,
            hold_ms: patch.hold_ms,
            feature_json: None,
            decision_context_json: None,
            execution_context_json: None,
        })?;
        Ok(true)
    }

    pub fn patch_latest_snapback_feature_snapshot_outcome_for_trade(
        &self,
        strategy_id: &str,
        timeframe: &str,
        period_timestamp: u64,
        token_id: &str,
        patch: &SnapbackFeatureSnapshotOutcomePatch,
    ) -> Result<bool> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let timeframe = timeframe.trim().to_ascii_lowercase();
        let token_id = token_id.trim().to_string();
        if timeframe.is_empty() || token_id.is_empty() {
            return Ok(false);
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        let (updated, impulse_id) = self.with_conn(|conn| {
            let selected: Option<(i64, String)> = conn
                .query_row(
                    r#"
SELECT id, impulse_id
FROM snapback_feature_snapshots_v1
WHERE strategy_id=?1
  AND timeframe=?2
  AND period_timestamp=?3
  AND token_id=?4
ORDER BY intent_ts_ms DESC, id DESC
LIMIT 1
"#,
                    params![strategy_id, timeframe, period_timestamp as i64, token_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((snapshot_row_id, impulse_id)) = selected else {
                return Ok((false, None));
            };
            let updated = conn.execute(
                r#"
UPDATE snapback_feature_snapshots_v1
SET
    condition_id=COALESCE(?1, condition_id),
    position_key=COALESCE(?2, position_key),
    request_id=COALESCE(?3, request_id),
    order_id=COALESCE(?4, order_id),
    fill_ts_ms=CASE
        WHEN ?5 IS NULL THEN fill_ts_ms
        WHEN fill_ts_ms IS NULL THEN ?5
        ELSE MIN(fill_ts_ms, ?5)
    END,
    exit_ts_ms=CASE
        WHEN ?6 IS NULL THEN exit_ts_ms
        WHEN exit_ts_ms IS NULL THEN ?6
        ELSE MIN(exit_ts_ms, ?6)
    END,
    settled_ts_ms=CASE
        WHEN ?7 IS NULL THEN settled_ts_ms
        WHEN settled_ts_ms IS NULL THEN ?7
        ELSE MIN(settled_ts_ms, ?7)
    END,
    exit_reason=COALESCE(?8, exit_reason),
    realized_pnl_usd=COALESCE(?9, realized_pnl_usd),
    settled_pnl_usd=COALESCE(?10, settled_pnl_usd),
    hold_ms=COALESCE(?11, hold_ms),
    updated_at_ms=?12
WHERE id=?13
"#,
                params![
                    patch.condition_id,
                    patch.position_key,
                    patch.request_id,
                    patch.order_id,
                    patch.fill_ts_ms,
                    patch.exit_ts_ms,
                    patch.settled_ts_ms,
                    patch.exit_reason,
                    patch.realized_pnl_usd,
                    patch.settled_pnl_usd,
                    patch.hold_ms,
                    now_ms,
                    snapshot_row_id,
                ],
            )?;
            Ok((updated > 0, Some(impulse_id)))
        })?;
        if !updated {
            return Ok(false);
        }

        let execution_status = infer_execution_status_from_snapback_row(
            patch.order_id.as_deref(),
            patch.fill_ts_ms,
            patch.exit_ts_ms,
            patch.settled_ts_ms,
        );
        self.upsert_strategy_feature_snapshot_intent(&StrategyFeatureSnapshotIntentRecord {
            strategy_id: strategy_id.clone(),
            timeframe: timeframe.clone(),
            period_timestamp,
            market_key: Some(DEFAULT_MARKET_KEY.to_string()),
            asset_symbol: Some(DEFAULT_ASSET_SYMBOL.to_string()),
            condition_id: patch.condition_id.clone(),
            token_id,
            impulse_id: impulse_id.unwrap_or_else(|| "unknown".to_string()),
            decision_id: None,
            rung_id: None,
            slice_id: None,
            request_id: patch.request_id.clone(),
            order_id: patch.order_id.clone(),
            trade_key: None,
            position_key: patch.position_key.clone(),
            intent_status: Some("submitted".to_string()),
            execution_status: Some(execution_status.to_string()),
            intent_ts_ms: now_ms,
            submit_ts_ms: None,
            ack_ts_ms: None,
            fill_ts_ms: patch.fill_ts_ms,
            exit_ts_ms: patch.exit_ts_ms,
            settled_ts_ms: patch.settled_ts_ms,
            entry_notional_usd: None,
            exit_notional_usd: None,
            realized_pnl_usd: patch.realized_pnl_usd,
            settled_pnl_usd: patch.settled_pnl_usd,
            hold_ms: patch.hold_ms,
            feature_json: None,
            decision_context_json: None,
            execution_context_json: None,
        })?;
        Ok(true)
    }

    pub fn record_strategy_decision(
        &self,
        period_timestamp: u64,
        decision: &StrategyDecision,
    ) -> Result<()> {
        let action = match decision.action {
            StrategyAction::Trade => "TRADE",
            StrategyAction::Skip => "SKIP",
        };
        let direction = decision
            .direction
            .as_ref()
            .map(|d| format!("{:?}", d).to_uppercase());
        let strategy_id = normalize_strategy_id(decision.strategy_id.as_str());
        let run_id = runtime_run_id();
        let strategy_cohort = runtime_strategy_cohort();
        let market_key = normalize_market_key(
            Some(decision.market_key.as_str()),
            Some(decision.asset_symbol.as_str()),
        );
        let asset_symbol = normalize_asset_symbol(
            Some(decision.asset_symbol.as_str()),
            None,
            Some(market_key.as_str()),
        );

        self.with_conn(|conn| {
            conn.execute(
                r#"
INSERT INTO strategy_decisions (
    period_timestamp, timeframe, strategy_id, run_id, strategy_cohort, market_key, asset_symbol, decided_at_ms,
    action, direction, confidence, source, mode, reasoning
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
ON CONFLICT(period_timestamp, timeframe, strategy_id, market_key) DO UPDATE SET
    run_id=excluded.run_id,
    strategy_cohort=excluded.strategy_cohort,
    asset_symbol=excluded.asset_symbol,
    decided_at_ms=excluded.decided_at_ms,
    action=excluded.action,
    direction=excluded.direction,
    confidence=excluded.confidence,
    source=excluded.source,
    mode=excluded.mode,
    reasoning=excluded.reasoning
WHERE excluded.decided_at_ms >= strategy_decisions.decided_at_ms
"#,
                params![
                    period_timestamp as i64,
                    decision.timeframe.as_str(),
                    strategy_id,
                    run_id,
                    strategy_cohort,
                    market_key,
                    asset_symbol,
                    decision.generated_at_ms,
                    action,
                    direction,
                    decision.confidence,
                    decision.source,
                    decision.mode,
                    decision.reasoning,
                ],
            )?;
            Ok(())
        })
    }

    pub fn record_strategy_decision_context(
        &self,
        period_timestamp: u64,
        decision: &StrategyDecision,
    ) -> Result<()> {
        let Some(trace) = decision.telemetry.as_ref() else {
            return Ok(());
        };

        let now_ms = chrono::Utc::now().timestamp_millis();
        let strategy_id = normalize_strategy_id(decision.strategy_id.as_str());
        let run_id = runtime_run_id();
        let strategy_cohort = runtime_strategy_cohort();
        let market_key = normalize_market_key(
            Some(decision.market_key.as_str()),
            Some(decision.asset_symbol.as_str()),
        );
        let asset_symbol = normalize_asset_symbol(
            Some(decision.asset_symbol.as_str()),
            None,
            Some(market_key.as_str()),
        );
        self.with_conn(|conn| {
            conn.execute(
                r#"
INSERT INTO strategy_decision_contexts (
    period_timestamp, timeframe, strategy_id, run_id, strategy_cohort, market_key, asset_symbol, decided_at_ms, source, mode,
    context_json, signal_digest_json, parse_error, created_at_ms
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
ON CONFLICT(period_timestamp, timeframe, strategy_id, market_key) DO UPDATE SET
    run_id=excluded.run_id,
    strategy_cohort=excluded.strategy_cohort,
    asset_symbol=excluded.asset_symbol,
    decided_at_ms=excluded.decided_at_ms,
    source=excluded.source,
    mode=excluded.mode,
    context_json=excluded.context_json,
    signal_digest_json=excluded.signal_digest_json,
    parse_error=excluded.parse_error,
    created_at_ms=excluded.created_at_ms
WHERE excluded.decided_at_ms >= strategy_decision_contexts.decided_at_ms
"#,
                params![
                    period_timestamp as i64,
                    decision.timeframe.as_str(),
                    strategy_id,
                    run_id,
                    strategy_cohort,
                    market_key,
                    asset_symbol,
                    decision.generated_at_ms,
                    decision.source.as_str(),
                    decision.mode.as_deref().unwrap_or(""),
                    trace.context_json.as_str(),
                    trace.signal_digest_json.as_str(),
                    trace.parse_error.as_deref(),
                    now_ms
                ],
            )?;
            Ok(())
        })
    }

    pub fn record_skip_market(
        &self,
        period_timestamp: u64,
        timeframe: Timeframe,
        strategy_id: &str,
        market_key: Option<&str>,
        asset_symbol: Option<&str>,
        reason: &str,
        source: &str,
        confidence: f64,
    ) -> Result<()> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let normalized_strategy_id = normalize_strategy_id(strategy_id);
        let run_id = runtime_run_id();
        let strategy_cohort = runtime_strategy_cohort();
        let normalized_market_key = normalize_market_key(market_key, asset_symbol);
        let normalized_asset_symbol =
            normalize_asset_symbol(asset_symbol, None, Some(normalized_market_key.as_str()));
        self.with_conn(|conn| {
            conn.execute(
                r#"
INSERT INTO skipped_markets (
    period_timestamp, timeframe, strategy_id, run_id, strategy_cohort, market_key, asset_symbol, decided_at_ms, reason, source, confidence
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
ON CONFLICT(period_timestamp, timeframe, strategy_id, market_key) DO UPDATE SET
    run_id=excluded.run_id,
    strategy_cohort=excluded.strategy_cohort,
    asset_symbol=excluded.asset_symbol,
    decided_at_ms=excluded.decided_at_ms,
    reason=excluded.reason,
    source=excluded.source,
    confidence=excluded.confidence
WHERE excluded.decided_at_ms >= skipped_markets.decided_at_ms
"#,
                params![
                    period_timestamp as i64,
                    timeframe.as_str(),
                    normalized_strategy_id,
                    run_id,
                    strategy_cohort,
                    normalized_market_key,
                    normalized_asset_symbol,
                    now_ms,
                    reason,
                    source,
                    confidence,
                ],
            )?;
            Ok(())
        })
    }

    pub fn summarize_timeframe_performance(&self) -> Result<Vec<TimeframePerformance>> {
        self.summarize_timeframe_performance_for_strategy(LEGACY_STRATEGY_ID)
    }

    pub fn summarize_timeframe_performance_for_strategy(
        &self,
        strategy_id: &str,
    ) -> Result<Vec<TimeframePerformance>> {
        self.summarize_timeframe_performance_for_strategy_with_source(
            strategy_id,
            OutcomeSource::from_env(),
        )
    }

    fn summarize_timeframe_performance_for_strategy_with_source(
        &self,
        strategy_id: &str,
        outcome_source: OutcomeSource,
    ) -> Result<Vec<TimeframePerformance>> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let prod_only = if reporting_prod_only() { 1_i64 } else { 0_i64 };
        let prod_filter = prod_event_filter_sql("");
        let prod_filter_te = prod_event_filter_sql("te");
        self.with_conn(|conn| {
            let query = match outcome_source {
                OutcomeSource::Settlement => format!(
                    r#"
SELECT
    s.timeframe,
    COUNT(*) AS trades,
    SUM(CASE WHEN settled_pnl_usd > 0 THEN 1 ELSE 0 END) AS wins,
    SUM(CASE WHEN settled_pnl_usd <= 0 THEN 1 ELSE 0 END) AS losses,
    COALESCE(SUM(settled_pnl_usd), 0.0) AS net_pnl_usd
FROM settlements_v2 s
LEFT JOIN trade_events te
  ON te.event_key = s.source_event_key
WHERE s.strategy_id = ?1
  AND (?2 = 0 OR te.id IS NULL OR ({}))
GROUP BY s.timeframe
ORDER BY s.timeframe
"#,
                    prod_filter_te
                ),
                OutcomeSource::Exit => format!(
                    r#"
SELECT
    timeframe,
    COUNT(*) AS trades,
    SUM(CASE WHEN pnl_usd > 0 THEN 1 ELSE 0 END) AS wins,
    SUM(CASE WHEN pnl_usd <= 0 THEN 1 ELSE 0 END) AS losses,
    COALESCE(SUM(pnl_usd), 0.0) AS net_pnl_usd
FROM trade_events
WHERE event_type = 'EXIT'
  AND strategy_id = ?1
  AND (?2 = 0 OR ({}))
GROUP BY timeframe
ORDER BY timeframe
"#,
                    prod_filter
                ),
                OutcomeSource::Hybrid => format!(
                    r#"
WITH outcomes AS (
    SELECT s.timeframe, COALESCE(s.settled_pnl_usd, 0.0) AS pnl_usd
    FROM settlements_v2 s
    LEFT JOIN trade_events te ON te.event_key = s.source_event_key
    WHERE s.strategy_id = ?1
      AND (?2 = 0 OR te.id IS NULL OR ({}))
    UNION ALL
    SELECT timeframe, COALESCE(pnl_usd, 0.0) AS pnl_usd
    FROM trade_events
    WHERE event_type = 'EXIT'
      AND strategy_id = ?1
      AND COALESCE(LOWER(reason), '') NOT LIKE '%market_close%'
      AND (?2 = 0 OR ({}))
)
SELECT
    timeframe,
    COUNT(*) AS trades,
    SUM(CASE WHEN pnl_usd > 0 THEN 1 ELSE 0 END) AS wins,
    SUM(CASE WHEN pnl_usd <= 0 THEN 1 ELSE 0 END) AS losses,
    COALESCE(SUM(pnl_usd), 0.0) AS net_pnl_usd
FROM outcomes
GROUP BY timeframe
ORDER BY timeframe
"#,
                    prod_filter_te, prod_filter
                ),
            };
            let mut stmt = conn.prepare(query.as_str())?;

            let rows = stmt.query_map(params![strategy_id, prod_only], |row| {
                Ok(TimeframePerformance {
                    timeframe: row.get::<_, String>(0)?,
                    trades: row.get::<_, i64>(1)?.max(0) as u64,
                    wins: row.get::<_, i64>(2)?.max(0) as u64,
                    losses: row.get::<_, i64>(3)?.max(0) as u64,
                    net_pnl_usd: row.get::<_, f64>(4)?,
                })
            })?;

            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn summarize_signal_accuracy(&self) -> Result<Vec<SignalAccuracyRow>> {
        self.summarize_signal_accuracy_for_strategy(LEGACY_STRATEGY_ID)
    }

    pub fn summarize_signal_accuracy_for_strategy(
        &self,
        strategy_id: &str,
    ) -> Result<Vec<SignalAccuracyRow>> {
        self.summarize_signal_accuracy_for_strategy_with_source(
            strategy_id,
            OutcomeSource::from_env(),
        )
    }

    fn summarize_signal_accuracy_for_strategy_with_source(
        &self,
        strategy_id: &str,
        outcome_source: OutcomeSource,
    ) -> Result<Vec<SignalAccuracyRow>> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let prod_only = if reporting_prod_only() { 1_i64 } else { 0_i64 };
        let prod_filter = prod_event_filter_sql("");
        let prod_filter_te = prod_event_filter_sql("te");
        self.with_conn(|conn| {
            let query = match outcome_source {
                OutcomeSource::Settlement => format!(
                    r#"
SELECT
    s.timeframe,
    'settlement_outcome' AS signal,
    COUNT(*) AS samples,
    SUM(CASE WHEN settled_pnl_usd > 0 THEN 1 ELSE 0 END) AS wins
FROM settlements_v2 s
LEFT JOIN trade_events te
  ON te.event_key = s.source_event_key
WHERE s.strategy_id = ?1
  AND (?2 = 0 OR te.id IS NULL OR ({}))
GROUP BY s.timeframe
HAVING samples > 0
ORDER BY s.timeframe, signal
"#,
                    prod_filter_te
                ),
                OutcomeSource::Exit => format!(
                    r#"
SELECT
    timeframe,
    COALESCE(reason, 'UNKNOWN') AS signal,
    COUNT(*) AS samples,
    SUM(CASE WHEN pnl_usd > 0 THEN 1 ELSE 0 END) AS wins
FROM trade_events
WHERE event_type IN ('EXIT', 'EXIT_SIGNAL')
  AND strategy_id = ?1
  AND (?2 = 0 OR ({}))
GROUP BY timeframe, COALESCE(reason, 'UNKNOWN')
HAVING samples > 0
ORDER BY timeframe, signal
"#,
                    prod_filter
                ),
                OutcomeSource::Hybrid => format!(
                    r#"
SELECT
    timeframe,
    signal,
    COUNT(*) AS samples,
    SUM(CASE WHEN pnl_usd > 0 THEN 1 ELSE 0 END) AS wins
FROM (
    SELECT timeframe, COALESCE(reason, 'UNKNOWN') AS signal, COALESCE(pnl_usd, 0.0) AS pnl_usd
    FROM trade_events
    WHERE event_type IN ('EXIT', 'EXIT_SIGNAL')
      AND strategy_id = ?1
      AND COALESCE(LOWER(reason), '') NOT LIKE '%market_close%'
      AND (?2 = 0 OR ({}))
    UNION ALL
    SELECT s.timeframe, 'settlement_outcome' AS signal, COALESCE(s.settled_pnl_usd, 0.0) AS pnl_usd
    FROM settlements_v2 s
    LEFT JOIN trade_events te ON te.event_key = s.source_event_key
    WHERE s.strategy_id = ?1
      AND (?2 = 0 OR te.id IS NULL OR ({}))
)
GROUP BY timeframe, signal
HAVING samples > 0
ORDER BY timeframe, signal
"#,
                    prod_filter, prod_filter_te
                ),
            };
            let mut stmt = conn.prepare(query.as_str())?;

            let rows = stmt.query_map(params![strategy_id, prod_only], |row| {
                let samples = row.get::<_, i64>(2)?.max(0) as u64;
                let wins = row.get::<_, i64>(3)?.max(0) as u64;
                let win_rate_pct = if samples == 0 {
                    0.0
                } else {
                    (wins as f64 / samples as f64) * 100.0
                };
                Ok(SignalAccuracyRow {
                    timeframe: row.get::<_, String>(0)?,
                    signal: row.get::<_, String>(1)?,
                    samples,
                    wins,
                    win_rate_pct,
                })
            })?;

            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn summarize_snapback_feature_bins_for_strategy(
        &self,
        strategy_id: &str,
    ) -> Result<Vec<SnapbackFeatureBinSummary>> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let prod_only = if reporting_prod_only() { 1_i64 } else { 0_i64 };
        let prod_filter_te = prod_event_filter_sql("te");
        self.with_conn(|conn| {
            let query = format!(
                r#"
SELECT
    s.timeframe,
    COALESCE(NULLIF(TRIM(s.entry_side), ''), 'UNKNOWN') AS direction,
    s.signed_notional_30s,
    s.signed_notional_300s,
    s.signed_notional_900s,
    s.burst_zscore,
    s.side_imbalance_5m,
    s.impulse_strength,
    s.realized_pnl_usd
FROM snapback_feature_snapshots_v1 s
WHERE s.strategy_id=?1
  AND s.realized_pnl_usd IS NOT NULL
  AND (
      ?2 = 0 OR NOT EXISTS (
          SELECT 1
          FROM trade_events te_any
          WHERE te_any.strategy_id = s.strategy_id
            AND te_any.timeframe = s.timeframe
            AND te_any.period_timestamp = s.period_timestamp
            AND te_any.token_id = s.token_id
            AND te_any.event_type IN ('ENTRY_FILL', 'EXIT')
      ) OR EXISTS (
          SELECT 1
          FROM trade_events te
          WHERE te.strategy_id = s.strategy_id
            AND te.timeframe = s.timeframe
            AND te.period_timestamp = s.period_timestamp
            AND te.token_id = s.token_id
            AND te.event_type IN ('ENTRY_FILL', 'EXIT')
            AND ({})
      )
  )
"#,
                prod_filter_te
            );
            let mut stmt = conn.prepare(query.as_str())?;
            let mut rows = stmt.query(params![strategy_id, prod_only])?;

            let mut groups: HashMap<(String, String, String, String), Vec<f64>> = HashMap::new();
            while let Some(row) = rows.next()? {
                let timeframe: String = row.get(0)?;
                let direction: String = row.get(1)?;
                let signed_notional_30s: Option<f64> = row.get(2)?;
                let signed_notional_300s: Option<f64> = row.get(3)?;
                let signed_notional_900s: Option<f64> = row.get(4)?;
                let burst_zscore: Option<f64> = row.get(5)?;
                let side_imbalance_5m: Option<f64> = row.get(6)?;
                let impulse_strength: f64 = row.get::<_, f64>(7).unwrap_or(0.0);
                let pnl: f64 = row.get::<_, f64>(8).unwrap_or(0.0);

                if let Some(value) = signed_notional_30s {
                    let key = (
                        timeframe.clone(),
                        "signed_notional_30s".to_string(),
                        bucket_signed_notional(value).to_string(),
                        direction.clone(),
                    );
                    groups.entry(key).or_default().push(pnl);
                }
                if let Some(value) = signed_notional_300s {
                    let key = (
                        timeframe.clone(),
                        "signed_notional_300s".to_string(),
                        bucket_signed_notional(value).to_string(),
                        direction.clone(),
                    );
                    groups.entry(key).or_default().push(pnl);
                }
                if let Some(value) = signed_notional_900s {
                    let key = (
                        timeframe.clone(),
                        "signed_notional_900s".to_string(),
                        bucket_signed_notional(value).to_string(),
                        direction.clone(),
                    );
                    groups.entry(key).or_default().push(pnl);
                }
                if let Some(value) = burst_zscore {
                    let key = (
                        timeframe.clone(),
                        "burst_zscore".to_string(),
                        bucket_burst_zscore(value).to_string(),
                        direction.clone(),
                    );
                    groups.entry(key).or_default().push(pnl);
                }
                if let Some(value) = side_imbalance_5m {
                    let key = (
                        timeframe.clone(),
                        "side_imbalance_5m".to_string(),
                        bucket_side_imbalance(value).to_string(),
                        direction.clone(),
                    );
                    groups.entry(key).or_default().push(pnl);
                }
                let key = (
                    timeframe,
                    "impulse_strength".to_string(),
                    bucket_impulse_strength(impulse_strength).to_string(),
                    direction,
                );
                groups.entry(key).or_default().push(pnl);
            }

            let mut out = Vec::new();
            for ((timeframe, feature, bucket, direction), values) in groups {
                if values.is_empty() {
                    continue;
                }
                let samples = values.len() as u64;
                let wins = values.iter().filter(|v| **v > 0.0).count() as u64;
                let net_pnl_usd = values.iter().sum::<f64>();
                let avg_pnl_usd = net_pnl_usd / samples as f64;
                let drawdown_proxy_usd = values
                    .iter()
                    .filter(|v| **v < 0.0)
                    .map(|v| v.abs())
                    .sum::<f64>();
                let mut median_values = values.clone();
                let median_pnl_usd = median(median_values.as_mut_slice());
                let win_rate_pct = if samples == 0 {
                    0.0
                } else {
                    (wins as f64 / samples as f64) * 100.0
                };
                out.push(SnapbackFeatureBinSummary {
                    timeframe,
                    feature,
                    bucket,
                    direction,
                    samples,
                    wins,
                    win_rate_pct,
                    net_pnl_usd,
                    avg_pnl_usd,
                    median_pnl_usd,
                    drawdown_proxy_usd,
                });
            }

            out.sort_by(|a, b| {
                a.timeframe
                    .cmp(&b.timeframe)
                    .then(a.feature.cmp(&b.feature))
                    .then(a.bucket.cmp(&b.bucket))
                    .then(a.direction.cmp(&b.direction))
            });
            Ok(out)
        })
    }

    pub fn summarize_strategy_feature_bins_for_strategy(
        &self,
        strategy_id: &str,
    ) -> Result<Vec<StrategyFeatureBinSummary>> {
        self.summarize_strategy_feature_bins_for_strategy_with_source(
            strategy_id,
            OutcomeSource::from_env(),
        )
    }

    pub fn summarize_strategy_feature_bins_for_strategy_with_source(
        &self,
        strategy_id: &str,
        outcome_source: OutcomeSource,
    ) -> Result<Vec<StrategyFeatureBinSummary>> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let prod_only = if reporting_prod_only() { 1_i64 } else { 0_i64 };
        let prod_filter_te = prod_event_filter_sql("te");
        self.with_conn(|conn| {
            let query = match outcome_source {
                OutcomeSource::Settlement => format!(
                    r#"
SELECT
    s.timeframe,
    s.feature_json,
    s.execution_context_json,
    s.settled_pnl_usd AS outcome_pnl
FROM strategy_feature_snapshots_v1 s
WHERE s.strategy_id=?1
  AND s.settled_pnl_usd IS NOT NULL
  AND (
      ?2 = 0 OR NOT EXISTS (
          SELECT 1
          FROM trade_events te_any
          WHERE te_any.strategy_id = s.strategy_id
            AND te_any.timeframe = s.timeframe
            AND te_any.period_timestamp = s.period_timestamp
            AND te_any.event_type = 'EXIT'
            AND COALESCE(LOWER(te_any.reason), '') LIKE '%market_close%'
      ) OR EXISTS (
          SELECT 1
          FROM trade_events te
          WHERE te.strategy_id = s.strategy_id
            AND te.timeframe = s.timeframe
            AND te.period_timestamp = s.period_timestamp
            AND te.event_type = 'EXIT'
            AND COALESCE(LOWER(te.reason), '') LIKE '%market_close%'
            AND ({})
      )
  )
"#,
                    prod_filter_te
                ),
                OutcomeSource::Exit => format!(
                    r#"
SELECT
    s.timeframe,
    s.feature_json,
    s.execution_context_json,
    s.realized_pnl_usd AS outcome_pnl
FROM strategy_feature_snapshots_v1 s
WHERE s.strategy_id=?1
  AND s.realized_pnl_usd IS NOT NULL
  AND (
      ?2 = 0 OR NOT EXISTS (
          SELECT 1
          FROM trade_events te_any
          WHERE te_any.strategy_id = s.strategy_id
            AND te_any.timeframe = s.timeframe
            AND te_any.period_timestamp = s.period_timestamp
            AND te_any.event_type IN ('EXIT', 'EXIT_SIGNAL')
      ) OR EXISTS (
          SELECT 1
          FROM trade_events te
          WHERE te.strategy_id = s.strategy_id
            AND te.timeframe = s.timeframe
            AND te.period_timestamp = s.period_timestamp
            AND te.event_type IN ('EXIT', 'EXIT_SIGNAL')
            AND ({})
      )
  )
"#,
                    prod_filter_te
                ),
                OutcomeSource::Hybrid => format!(
                    r#"
SELECT
    s.timeframe,
    s.feature_json,
    s.execution_context_json,
    COALESCE(s.settled_pnl_usd, s.realized_pnl_usd) AS outcome_pnl
FROM strategy_feature_snapshots_v1 s
WHERE s.strategy_id=?1
  AND (s.settled_pnl_usd IS NOT NULL OR s.realized_pnl_usd IS NOT NULL)
  AND (
      ?2 = 0 OR NOT EXISTS (
          SELECT 1
          FROM trade_events te_any
          WHERE te_any.strategy_id = s.strategy_id
            AND te_any.timeframe = s.timeframe
            AND te_any.period_timestamp = s.period_timestamp
            AND te_any.event_type IN ('ENTRY_FILL', 'EXIT', 'EXIT_SIGNAL')
      ) OR EXISTS (
          SELECT 1
          FROM trade_events te
          WHERE te.strategy_id = s.strategy_id
            AND te.timeframe = s.timeframe
            AND te.period_timestamp = s.period_timestamp
            AND te.event_type IN ('ENTRY_FILL', 'EXIT', 'EXIT_SIGNAL')
            AND ({})
      )
  )
"#,
                    prod_filter_te
                ),
            };
            let mut stmt = conn.prepare(query.as_str())?;
            let mut rows = stmt.query(params![strategy_id, prod_only])?;

            let mut groups: HashMap<(String, String, String), Vec<f64>> = HashMap::new();
            while let Some(row) = rows.next()? {
                let timeframe = row
                    .get::<_, String>(0)
                    .unwrap_or_else(|_| "unknown".to_string())
                    .trim()
                    .to_ascii_lowercase();
                let feature_json: Option<String> = row.get(1)?;
                let execution_context_json: Option<String> = row.get(2)?;
                let pnl = row.get::<_, f64>(3).unwrap_or(0.0);

                if let Some(raw) = feature_json {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
                        if let Some(object) = value.as_object() {
                            for (feature, feature_value) in object {
                                if let Some(bucket) =
                                    map_feature_bucket(feature.as_str(), feature_value)
                                {
                                    groups
                                        .entry((
                                            timeframe.clone(),
                                            feature.trim().to_ascii_lowercase(),
                                            bucket,
                                        ))
                                        .or_default()
                                        .push(pnl);
                                }
                            }
                        }
                    }
                }

                if let Some(raw) = execution_context_json {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
                        if let Some(event) = value.get("event") {
                            if let Some(bucket) = map_feature_bucket("event", event) {
                                groups
                                    .entry((timeframe.clone(), "event".to_string(), bucket))
                                    .or_default()
                                    .push(pnl);
                            }
                        }
                        if let Some(friction) = value.get("friction") {
                            if let Some(bucket) = map_feature_bucket("event", friction) {
                                groups
                                    .entry((timeframe.clone(), "friction".to_string(), bucket))
                                    .or_default()
                                    .push(pnl);
                            }
                        }
                    }
                }
            }

            let mut out = Vec::new();
            for ((timeframe, feature, bucket), values) in groups {
                if values.is_empty() {
                    continue;
                }
                let samples = values.len() as u64;
                let wins = values.iter().filter(|v| **v > 0.0).count() as u64;
                let net_pnl_usd = values.iter().sum::<f64>();
                let avg_pnl_usd = net_pnl_usd / samples as f64;
                let drawdown_proxy_usd = values
                    .iter()
                    .filter(|v| **v < 0.0)
                    .map(|v| v.abs())
                    .sum::<f64>();
                let mut median_values = values.clone();
                let median_pnl_usd = median(median_values.as_mut_slice());
                let win_rate_pct = if samples == 0 {
                    0.0
                } else {
                    (wins as f64 / samples as f64) * 100.0
                };
                out.push(StrategyFeatureBinSummary {
                    timeframe,
                    feature,
                    bucket,
                    samples,
                    wins,
                    win_rate_pct,
                    net_pnl_usd,
                    avg_pnl_usd,
                    median_pnl_usd,
                    drawdown_proxy_usd,
                });
            }
            out.sort_by(|a, b| {
                a.timeframe
                    .cmp(&b.timeframe)
                    .then(a.feature.cmp(&b.feature))
                    .then(a.bucket.cmp(&b.bucket))
            });
            Ok(out)
        })
    }

    pub fn summarize_settled_performance_v2(&self) -> Result<SettledPerformanceV2> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT
    COUNT(*) AS settled_trades,
    SUM(CASE WHEN settled_pnl_usd > 0 THEN 1 ELSE 0 END) AS wins,
    SUM(CASE WHEN settled_pnl_usd < 0 THEN 1 ELSE 0 END) AS losses,
    COALESCE(SUM(settled_pnl_usd), 0.0) AS net_settled_pnl_usd
FROM settlements_v2
"#,
            )?;
            let (settled_trades, wins, losses, net_settled_pnl_usd): (i64, i64, i64, f64) = stmt
                .query_row([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?;
            let settled_trades_u = settled_trades.max(0) as u64;
            let wins_u = wins.max(0) as u64;
            let losses_u = losses.max(0) as u64;
            let win_rate_pct = if settled_trades_u == 0 {
                0.0
            } else {
                (wins_u as f64 / settled_trades_u as f64) * 100.0
            };
            Ok(SettledPerformanceV2 {
                settled_trades: settled_trades_u,
                wins: wins_u,
                losses: losses_u,
                win_rate_pct,
                net_settled_pnl_usd,
            })
        })
    }

    pub fn summarize_open_mtm_v2(&self) -> Result<OpenMtmSummaryV2> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
WITH latest_marks AS (
    SELECT
        m.position_key,
        m.price,
        ROW_NUMBER() OVER (
            PARTITION BY m.position_key
            ORDER BY m.ts_ms DESC, m.id DESC
        ) AS rn
    FROM marks_v2 m
),
open_positions AS (
    SELECT
        p.position_key,
        MAX(
            p.entry_units
                - p.exit_units
                - COALESCE(p.inventory_consumed_units, 0.0),
            0.0
        ) AS net_units,
        CASE
            WHEN p.entry_units > 0 THEN p.entry_notional_usd / p.entry_units
            ELSE 0.0
        END AS avg_entry_price,
        COALESCE(lm.price, 0.0) AS mark_price
    FROM positions_v2 p
    LEFT JOIN latest_marks lm
      ON lm.position_key = p.position_key
     AND lm.rn = 1
    WHERE p.status = 'OPEN'
)
SELECT
    COUNT(*) AS open_positions,
    COALESCE(SUM(net_units), 0.0) AS net_units,
    COALESCE(SUM(net_units * avg_entry_price), 0.0) AS cost_basis_usd,
    COALESCE(SUM(net_units * mark_price), 0.0) AS mark_value_usd
FROM open_positions
"#,
            )?;
            let (open_positions, net_units, cost_basis_usd, mark_value_usd): (i64, f64, f64, f64) =
                stmt.query_row([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?;
            let open_positions_u = open_positions.max(0) as u64;
            Ok(OpenMtmSummaryV2 {
                open_positions: open_positions_u,
                net_units,
                cost_basis_usd,
                mark_value_usd,
                unrealized_pnl_usd: mark_value_usd - cost_basis_usd,
            })
        })
    }

    pub fn sum_settled_pnl_for_strategy(&self, strategy_id: &str) -> Result<f64> {
        let strategy_id = normalize_strategy_id(strategy_id);
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT COALESCE(SUM(settled_pnl_usd), 0.0)
FROM settlements_v2
WHERE strategy_id = ?1
"#,
            )?;
            let pnl = stmt.query_row(params![strategy_id], |row| row.get::<_, f64>(0))?;
            Ok(pnl)
        })
    }

    pub fn list_recent_settlement_outcomes_for_strategy_lane(
        &self,
        strategy_id: &str,
        timeframe: &str,
        asset_symbol: &str,
        limit: usize,
    ) -> Result<Vec<f64>> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let timeframe = timeframe.trim().to_ascii_lowercase();
        let normalized_symbol = normalize_asset_symbol(Some(asset_symbol), None, None);
        let token_prefix = format!("{} %", normalized_symbol);
        let limit = limit.max(1);
        let limit_i64 = i64::try_from(limit).unwrap_or(10_000).max(1);
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT settled_pnl_usd
FROM settlements_v2
WHERE strategy_id = ?1
  AND LOWER(timeframe) = ?2
  AND UPPER(COALESCE(token_type, '')) LIKE ?3
ORDER BY ts_ms DESC, id DESC
LIMIT ?4
"#,
            )?;
            let mut rows = stmt.query(params![strategy_id, timeframe, token_prefix, limit_i64])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let pnl: f64 = row.get(0)?;
                if pnl.is_finite() {
                    out.push(pnl);
                }
            }
            Ok(out)
        })
    }

    pub fn summarize_open_mtm_v2_for_strategy(
        &self,
        strategy_id: &str,
    ) -> Result<OpenMtmSummaryV2> {
        let strategy_id = normalize_strategy_id(strategy_id);
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
WITH latest_marks AS (
    SELECT
        m.position_key,
        m.price,
        ROW_NUMBER() OVER (
            PARTITION BY m.position_key
            ORDER BY m.ts_ms DESC, m.id DESC
        ) AS rn
    FROM marks_v2 m
),
open_positions AS (
    SELECT
        p.position_key,
        MAX(
            p.entry_units
                - p.exit_units
                - COALESCE(p.inventory_consumed_units, 0.0),
            0.0
        ) AS net_units,
        CASE
            WHEN p.entry_units > 0 THEN p.entry_notional_usd / p.entry_units
            ELSE 0.0
        END AS avg_entry_price,
        COALESCE(lm.price, 0.0) AS mark_price
    FROM positions_v2 p
    LEFT JOIN latest_marks lm
      ON lm.position_key = p.position_key
     AND lm.rn = 1
    WHERE p.status = 'OPEN'
      AND p.strategy_id = ?1
)
SELECT
    COUNT(*) AS open_positions,
    COALESCE(SUM(net_units), 0.0) AS net_units,
    COALESCE(SUM(net_units * avg_entry_price), 0.0) AS cost_basis_usd,
    COALESCE(SUM(net_units * mark_price), 0.0) AS mark_value_usd
FROM open_positions
"#,
            )?;
            let (open_positions, net_units, cost_basis_usd, mark_value_usd): (i64, f64, f64, f64) =
                stmt.query_row(params![strategy_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?;
            let open_positions_u = open_positions.max(0) as u64;
            Ok(OpenMtmSummaryV2 {
                open_positions: open_positions_u,
                net_units,
                cost_basis_usd,
                mark_value_usd,
                unrealized_pnl_usd: mark_value_usd - cost_basis_usd,
            })
        })
    }

    pub fn summarize_strategy_mtm_pnl_v2(&self, strategy_id: &str) -> Result<StrategyMtmSummaryV2> {
        let normalized_strategy = normalize_strategy_id(strategy_id);
        let settled_pnl_usd = self.sum_settled_pnl_for_strategy(normalized_strategy.as_str())?;
        let open = self.summarize_open_mtm_v2_for_strategy(normalized_strategy.as_str())?;
        let unrealized_pnl_usd = open.unrealized_pnl_usd;
        Ok(StrategyMtmSummaryV2 {
            strategy_id: normalized_strategy,
            settled_pnl_usd,
            open_positions: open.open_positions,
            net_units: open.net_units,
            cost_basis_usd: open.cost_basis_usd,
            mark_value_usd: open.mark_value_usd,
            unrealized_pnl_usd,
            mtm_pnl_usd: settled_pnl_usd + unrealized_pnl_usd,
        })
    }

    pub fn summarize_entry_fills_canonical(
        &self,
        strategy_id: Option<&str>,
        scope: Option<&ReportingScope>,
    ) -> Result<Vec<CanonicalFillSummary>> {
        let scope = Self::effective_reporting_scope(scope);
        let normalized_strategy = strategy_id.map(normalize_strategy_id);
        let include_legacy = if scope.include_legacy_default {
            1_i64
        } else {
            0_i64
        };
        let prod_only = if scope.prod_only { 1_i64 } else { 0_i64 };
        let prod_filter = prod_event_filter_sql("");
        self.with_conn(|conn| {
            let query = format!(
                r#"
SELECT
    strategy_id,
    COUNT(*) AS fills,
    COALESCE(SUM(notional_usd), 0.0) AS notional_usd
FROM trade_events
WHERE event_type='ENTRY_FILL'
  AND (?1 IS NULL OR ts_ms >= ?1)
  AND (?2 IS NULL OR ts_ms <= ?2)
  AND (?3 = 1 OR strategy_id != ?4)
  AND (?6 = 0 OR ({}))
  AND (?5 IS NULL OR strategy_id = ?5)
GROUP BY strategy_id
ORDER BY notional_usd DESC, fills DESC, strategy_id ASC
"#,
                prod_filter
            );
            let mut stmt = conn.prepare(query.as_str())?;

            let rows = stmt.query_map(
                params![
                    scope.from_ts_ms,
                    scope.to_ts_ms,
                    include_legacy,
                    LEGACY_STRATEGY_ID,
                    normalized_strategy,
                    prod_only
                ],
                |row| {
                    Ok(CanonicalFillSummary {
                        strategy_id: row.get::<_, String>(0)?,
                        fills: row.get::<_, i64>(1)?.max(0) as u64,
                        notional_usd: row.get::<_, f64>(2)?,
                    })
                },
            )?;

            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn summarize_strategy_activity_canonical(
        &self,
        scope: Option<&ReportingScope>,
    ) -> Result<Vec<StrategyActivitySummary>> {
        let scope = Self::effective_reporting_scope(scope);
        let include_legacy = if scope.include_legacy_default {
            1_i64
        } else {
            0_i64
        };
        let prod_only = if scope.prod_only { 1_i64 } else { 0_i64 };
        let prod_filter = prod_event_filter_sql("");
        self.with_conn(|conn| {
            let mut map: HashMap<String, StrategyActivitySummary> = HashMap::new();

            let event_query = format!(
                r#"
SELECT
    strategy_id,
    SUM(CASE WHEN event_type='ENTRY_SUBMIT' THEN 1 ELSE 0 END) AS entry_submits,
    SUM(CASE WHEN event_type='ENTRY_ACK' THEN 1 ELSE 0 END) AS entry_acks,
    SUM(CASE WHEN event_type='ENTRY_FILL' THEN 1 ELSE 0 END) AS entry_fills,
    COALESCE(SUM(CASE WHEN event_type='ENTRY_FILL' THEN COALESCE(notional_usd, 0.0) ELSE 0.0 END), 0.0) AS entry_fill_notional_usd
FROM trade_events
WHERE event_type IN ('ENTRY_SUBMIT','ENTRY_ACK','ENTRY_FILL')
  AND (?1 IS NULL OR ts_ms >= ?1)
  AND (?2 IS NULL OR ts_ms <= ?2)
  AND (?3 = 1 OR strategy_id != ?4)
  AND (?5 = 0 OR ({}))
GROUP BY strategy_id
"#,
                prod_filter
            );
            let mut event_stmt = conn.prepare(event_query.as_str())?;
            let event_rows = event_stmt.query_map(
                params![
                    scope.from_ts_ms,
                    scope.to_ts_ms,
                    include_legacy,
                    LEGACY_STRATEGY_ID,
                    prod_only
                ],
                |row| {
                    Ok(StrategyActivitySummary {
                        strategy_id: row.get::<_, String>(0)?,
                        entry_submits: row.get::<_, i64>(1)?.max(0) as u64,
                        entry_acks: row.get::<_, i64>(2)?.max(0) as u64,
                        entry_fills: row.get::<_, i64>(3)?.max(0) as u64,
                        entry_fill_notional_usd: row.get::<_, f64>(4)?,
                        open_pending_orders: 0,
                        open_pending_notional_usd: 0.0,
                    })
                },
            )?;

            for row in event_rows {
                let item = row?;
                map.insert(item.strategy_id.clone(), item);
            }

            let pending_query = format!(
                r#"
SELECT
    strategy_id,
    COUNT(*) AS open_pending_orders,
    COALESCE(SUM(size_usd), 0.0) AS open_pending_notional_usd
FROM pending_orders
WHERE status IN ({})
  AND (?1 IS NULL OR updated_at_ms >= ?1)
  AND (?2 IS NULL OR updated_at_ms <= ?2)
  AND (?3 = 1 OR strategy_id != ?4)
  AND (?5 = 0 OR ({}))
GROUP BY strategy_id
"#,
                ACTIVE_PENDING_ORDER_STATUSES_SQL,
                prod_filter
            );
            let mut pending_stmt = conn.prepare(pending_query.as_str())?;
            let pending_rows = pending_stmt.query_map(
                params![
                    scope.from_ts_ms,
                    scope.to_ts_ms,
                    include_legacy,
                    LEGACY_STRATEGY_ID,
                    prod_only
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?.max(0) as u64,
                        row.get::<_, f64>(2)?,
                    ))
                },
            )?;

            for row in pending_rows {
                let (strategy_id, open_count, open_notional) = row?;
                let entry = map
                    .entry(strategy_id.clone())
                    .or_insert_with(|| StrategyActivitySummary {
                        strategy_id,
                        entry_submits: 0,
                        entry_acks: 0,
                        entry_fills: 0,
                        entry_fill_notional_usd: 0.0,
                        open_pending_orders: 0,
                        open_pending_notional_usd: 0.0,
                    });
                entry.open_pending_orders = open_count;
                entry.open_pending_notional_usd = open_notional;
            }

            let mut out: Vec<StrategyActivitySummary> = map.into_values().collect();
            out.sort_by(|a, b| a.strategy_id.cmp(&b.strategy_id));
            Ok(out)
        })
    }

    pub fn summarize_ui_dashboard(&self) -> Result<UiDashboardDbSummary> {
        let reporting_scope = Self::effective_reporting_scope(Some(&ReportingScope::from_env()));
        let recent_window_start_ms = chrono::Utc::now()
            .timestamp_millis()
            .saturating_sub(24 * 3_600_000);
        let prod_only = if reporting_scope.prod_only { 1_i64 } else { 0_i64 };
        let prod_filter = prod_event_filter_sql("");
        self.with_conn(|conn| {
            let total_trades: u64 = conn
                .query_row("SELECT COUNT(*) FROM fills_v2", [], |row| row.get::<_, i64>(0))
                .unwrap_or(0)
                .max(0) as u64;
            let total_pnl: f64 = conn
                .query_row(
                    "SELECT COALESCE(SUM(realized_pnl_usd), 0.0) FROM positions_v2",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0.0);
            let (winning_trades, losing_trades): (u64, u64) = conn
                .query_row(
                    format!(
                        "SELECT \
                            COALESCE(SUM(CASE WHEN COALESCE(pnl_usd, 0.0) > 0 THEN 1 ELSE 0 END), 0), \
                            COALESCE(SUM(CASE WHEN COALESCE(pnl_usd, 0.0) < 0 THEN 1 ELSE 0 END), 0) \
                         FROM trade_events \
                         WHERE event_type='EXIT' \
                           AND (?1 = 0 OR ({}))",
                        prod_filter
                    )
                    .as_str(),
                    params![prod_only],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?.max(0) as u64,
                            row.get::<_, i64>(1)?.max(0) as u64,
                        ))
                    },
                )
                .unwrap_or((0, 0));
            let (ack_sample_count, avg_ack_latency_ms): (u64, Option<f64>) = conn
                .query_row(
                    "SELECT \
                        COALESCE(SUM(CASE WHEN ack_ts_ms IS NOT NULL AND submit_ts_ms IS NOT NULL AND ack_ts_ms >= submit_ts_ms THEN 1 ELSE 0 END), 0), \
                        AVG(CASE WHEN ack_ts_ms IS NOT NULL AND submit_ts_ms IS NOT NULL AND ack_ts_ms >= submit_ts_ms THEN CAST(ack_ts_ms - submit_ts_ms AS REAL) END) \
                     FROM strategy_feature_snapshots_v1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?.max(0) as u64,
                            row.get::<_, Option<f64>>(1)?,
                        ))
                    },
                )
                .unwrap_or((0, None));
            let open_positions_count: u64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM positions_v2 WHERE status='OPEN'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or(0)
                .max(0) as u64;
            let recent_orders_count: u64 = conn
                .query_row(
                    format!(
                        "SELECT COUNT(*) FROM trade_events \
                         WHERE event_type='ENTRY_SUBMIT' \
                           AND ts_ms >= ?1 \
                           AND (?2 = 0 OR ({}))",
                        prod_filter
                    )
                    .as_str(),
                    params![recent_window_start_ms, prod_only],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or(0)
                .max(0) as u64;
            let last_event = conn
                .query_row(
                    format!(
                        "SELECT \
                            ts_ms, \
                            COALESCE(NULLIF(TRIM(event_type), ''), 'UNKNOWN'), \
                            COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default'), \
                            pnl_usd \
                         FROM trade_events \
                         WHERE (?1 = 0 OR ({})) \
                         ORDER BY ts_ms DESC, id DESC \
                         LIMIT 1",
                        prod_filter
                    )
                    .as_str(),
                    params![prod_only],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            normalize_strategy_id(row.get::<_, String>(2)?.as_str()),
                            row.get::<_, Option<f64>>(3)?,
                        ))
                    },
                )
                .optional()?;
            let last_exit = conn
                .query_row(
                    format!(
                        "SELECT \
                            ts_ms, \
                            COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default'), \
                            pnl_usd \
                         FROM trade_events \
                         WHERE event_type='EXIT' \
                           AND (?1 = 0 OR ({})) \
                         ORDER BY ts_ms DESC, id DESC \
                         LIMIT 1",
                        prod_filter
                    )
                    .as_str(),
                    params![prod_only],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            normalize_strategy_id(row.get::<_, String>(1)?.as_str()),
                            row.get::<_, Option<f64>>(2)?,
                        ))
                    },
                )
                .optional()?;

            Ok(UiDashboardDbSummary {
                total_trades,
                total_pnl,
                winning_trades,
                losing_trades,
                avg_ack_latency_ms,
                ack_sample_count,
                open_positions_count,
                recent_orders_count,
                last_event_ts_ms: last_event.as_ref().map(|row| row.0),
                last_event_type: last_event.as_ref().map(|row| row.1.clone()),
                last_event_strategy_id: last_event.as_ref().map(|row| row.2.clone()),
                last_event_pnl_usd: last_event.as_ref().and_then(|row| row.3),
                last_exit_ts_ms: last_exit.as_ref().map(|row| row.0),
                last_exit_strategy_id: last_exit.as_ref().map(|row| row.1.clone()),
                last_exit_pnl_usd: last_exit.as_ref().and_then(|row| row.2),
            })
        })
    }

    pub fn latest_strategy_event(&self, strategy_id: &str) -> Result<Option<LatestStrategyEvent>> {
        let normalized_strategy = normalize_strategy_id(strategy_id);
        let reporting_scope = Self::effective_reporting_scope(Some(&ReportingScope::from_env()));
        let prod_only = if reporting_scope.prod_only { 1_i64 } else { 0_i64 };
        let prod_filter = prod_event_filter_sql("");
        self.with_conn(|conn| {
            let row = conn
                .query_row(
                format!(
                    r#"
SELECT
    COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default'),
    ts_ms,
    COALESCE(NULLIF(TRIM(event_type), ''), 'UNKNOWN'),
    pnl_usd,
    condition_id,
    token_id
FROM trade_events
WHERE COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default') = ?1
  AND (?2 = 0 OR ({}))
ORDER BY ts_ms DESC, id DESC
LIMIT 1
"#,
                    prod_filter
                )
                .as_str(),
                params![normalized_strategy, prod_only],
                |row| {
                    Ok(LatestStrategyEvent {
                        strategy_id: normalize_strategy_id(row.get::<_, String>(0)?.as_str()),
                        ts_ms: row.get(1)?,
                        event_type: row.get(2)?,
                        pnl_usd: row.get(3)?,
                        condition_id: row.get(4)?,
                        token_id: row.get(5)?,
                    })
                },
            )
                .optional()?;
            Ok(row)
        })
    }

    pub fn summarize_pending_filled_side_for_strategy(
        &self,
        strategy_id: &str,
        from_ts_ms: Option<i64>,
        to_ts_ms: Option<i64>,
    ) -> Result<PendingFilledSideSummary> {
        let strategy_id = normalize_strategy_id(strategy_id);
        self.with_conn_read(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT
    COALESCE(SUM(CASE WHEN UPPER(TRIM(COALESCE(side, '')))='BUY' THEN 1 ELSE 0 END), 0) AS buy_fills,
    COALESCE(SUM(CASE WHEN UPPER(TRIM(COALESCE(side, '')))='SELL' THEN 1 ELSE 0 END), 0) AS sell_fills,
    COALESCE(SUM(CASE WHEN UPPER(TRIM(COALESCE(side, '')))='BUY' THEN COALESCE(size_usd, 0.0) ELSE 0.0 END), 0.0) AS buy_notional_usd,
    COALESCE(SUM(CASE WHEN UPPER(TRIM(COALESCE(side, '')))='SELL' THEN COALESCE(size_usd, 0.0) ELSE 0.0 END), 0.0) AS sell_notional_usd
FROM pending_orders
WHERE strategy_id=?1
  AND UPPER(TRIM(COALESCE(status, '')))='FILLED'
  AND (?2 IS NULL OR COALESCE(filled_at_ms, updated_at_ms) >= ?2)
  AND (?3 IS NULL OR COALESCE(filled_at_ms, updated_at_ms) <= ?3)
"#,
            )?;
            let row = stmt.query_row(params![strategy_id, from_ts_ms, to_ts_ms], |row| {
                Ok(PendingFilledSideSummary {
                    buy_fills: row.get::<_, i64>(0)?.max(0) as u64,
                    sell_fills: row.get::<_, i64>(1)?.max(0) as u64,
                    buy_notional_usd: row.get::<_, f64>(2)?.max(0.0),
                    sell_notional_usd: row.get::<_, f64>(3)?.max(0.0),
                })
            })?;
            Ok(row)
        })
    }

    pub fn summarize_canonical_tracking_by_timeframe(
        &self,
        scope: Option<&ReportingScope>,
    ) -> Result<Vec<CanonicalTrackingSummary>> {
        let scope = Self::effective_reporting_scope(scope);
        let include_legacy = if scope.include_legacy_default {
            1_i64
        } else {
            0_i64
        };
        let prod_only = if scope.prod_only { 1_i64 } else { 0_i64 };
        let prod_filter = prod_event_filter_sql("");
        self.with_conn(|conn| {
            let query = format!(
                r#"
SELECT
    strategy_id,
    timeframe,
    SUM(CASE WHEN event_type='ENTRY_SUBMIT' THEN 1 ELSE 0 END) AS entry_submits,
    SUM(CASE WHEN event_type='ENTRY_ACK' THEN 1 ELSE 0 END) AS entry_acks,
    SUM(CASE WHEN event_type='ENTRY_FILL' THEN 1 ELSE 0 END) AS entry_fills,
    COALESCE(SUM(CASE WHEN event_type='ENTRY_FILL' THEN COALESCE(notional_usd, 0.0) ELSE 0.0 END), 0.0) AS entry_fill_notional_usd
FROM trade_events
WHERE event_type IN ('ENTRY_SUBMIT','ENTRY_ACK','ENTRY_FILL')
  AND (?1 IS NULL OR ts_ms >= ?1)
  AND (?2 IS NULL OR ts_ms <= ?2)
  AND (?3 = 1 OR strategy_id != ?4)
  AND (?5 = 0 OR ({}))
GROUP BY strategy_id, timeframe
ORDER BY strategy_id ASC, timeframe ASC
"#,
                prod_filter
            );
            let mut stmt = conn.prepare(query.as_str())?;

            let rows = stmt.query_map(
                params![
                    scope.from_ts_ms,
                    scope.to_ts_ms,
                    include_legacy,
                    LEGACY_STRATEGY_ID,
                    prod_only
                ],
                |row| {
                    Ok(CanonicalTrackingSummary {
                        strategy_id: row.get::<_, String>(0)?,
                        timeframe: row.get::<_, String>(1)?,
                        entry_submits: row.get::<_, i64>(2)?.max(0) as u64,
                        entry_acks: row.get::<_, i64>(3)?.max(0) as u64,
                        entry_fills: row.get::<_, i64>(4)?.max(0) as u64,
                        entry_fill_notional_usd: row.get::<_, f64>(5)?,
                    })
                },
            )?;

            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn summarize_parallel_overlap(
        &self,
        scope: Option<&ReportingScope>,
    ) -> Result<Vec<ParallelOverlapRow>> {
        let scope = Self::effective_reporting_scope(scope);
        let include_legacy = if scope.include_legacy_default {
            1_i64
        } else {
            0_i64
        };
        let prod_only = if scope.prod_only { 1_i64 } else { 0_i64 };
        let prod_filter = prod_event_filter_sql("");
        self.with_conn(|conn| {
            let query = format!(
                r#"
SELECT
    period_timestamp,
    token_id,
    side,
    COUNT(DISTINCT strategy_id) AS strategy_count,
    COUNT(*) AS submit_count
FROM trade_events
WHERE event_type='ENTRY_SUBMIT'
  AND token_id IS NOT NULL
  AND TRIM(token_id) != ''
  AND side IS NOT NULL
  AND TRIM(side) != ''
  AND (?1 IS NULL OR ts_ms >= ?1)
  AND (?2 IS NULL OR ts_ms <= ?2)
  AND (?3 = 1 OR strategy_id != ?4)
  AND (?5 = 0 OR ({}))
GROUP BY period_timestamp, token_id, side
HAVING COUNT(DISTINCT strategy_id) > 1
ORDER BY submit_count DESC, period_timestamp DESC
"#,
                prod_filter
            );
            let mut stmt = conn.prepare(query.as_str())?;

            let rows = stmt.query_map(
                params![
                    scope.from_ts_ms,
                    scope.to_ts_ms,
                    include_legacy,
                    LEGACY_STRATEGY_ID,
                    prod_only
                ],
                |row| {
                    let period_i64: i64 = row.get(0)?;
                    Ok(ParallelOverlapRow {
                        period_timestamp: period_i64.max(0) as u64,
                        token_id: row.get::<_, String>(1)?,
                        side: row.get::<_, String>(2)?,
                        strategy_count: row.get::<_, i64>(3)?.max(0) as u64,
                        submit_count: row.get::<_, i64>(4)?.max(0) as u64,
                    })
                },
            )?;

            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn summarize_rollout_strategy_gates_last_hours(
        &self,
        hours: u64,
        include_legacy_default: bool,
    ) -> Result<Vec<RolloutStrategyGateSummary>> {
        let window_ms = (hours.max(1) as i64).saturating_mul(3_600_000);
        let now_ms = chrono::Utc::now().timestamp_millis();
        let from_ts_ms = now_ms.saturating_sub(window_ms);
        let include_legacy = if include_legacy_default { 1_i64 } else { 0_i64 };
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT
    strategy_id,
    SUM(CASE WHEN event_type='ENTRY_SUBMIT' THEN 1 ELSE 0 END) AS entry_submits,
    SUM(CASE WHEN event_type='ENTRY_FAILED' THEN 1 ELSE 0 END) AS entry_failed
FROM trade_events
WHERE ts_ms >= ?1
  AND (?2 = 1 OR strategy_id != ?3)
GROUP BY strategy_id
ORDER BY strategy_id ASC
"#,
            )?;

            let base_rows = stmt.query_map(
                params![from_ts_ms, include_legacy, LEGACY_STRATEGY_ID],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?.max(0) as u64,
                        row.get::<_, i64>(2)?.max(0) as u64,
                    ))
                },
            )?;

            let mut out = Vec::new();
            for row in base_rows {
                let (strategy_id, entry_submits, entry_failed) = row?;
                let submit_storm_groups: u64 = conn
                    .query_row(
                        r#"
SELECT COALESCE(COUNT(*), 0) FROM (
  SELECT period_timestamp, token_id, side
  FROM trade_events
  WHERE ts_ms >= ?1
    AND strategy_id = ?2
    AND event_type = 'ENTRY_SUBMIT'
    AND token_id IS NOT NULL
    AND TRIM(token_id) != ''
    AND side IS NOT NULL
    AND TRIM(side) != ''
  GROUP BY period_timestamp, token_id, side
  HAVING COUNT(*) > 5
)
"#,
                        params![from_ts_ms, strategy_id.as_str()],
                        |row| row.get::<_, i64>(0),
                    )?
                    .max(0) as u64;

                let liqaccum_direction_lock_rejects: u64 = conn
                    .query_row(
                        r#"
SELECT COUNT(*)
FROM strategy_feature_snapshots_v1
WHERE strategy_id = ?1
  AND intent_ts_ms >= ?2
  AND impulse_id LIKE 'liqaccum_plan:%'
  AND execution_context_json LIKE '%"friction":"direction_lock_reject"%'
"#,
                        params![strategy_id.as_str(), from_ts_ms],
                        |row| row.get::<_, i64>(0),
                    )?
                    .max(0) as u64;
                let liqaccum_plan_rows: u64 = conn
                    .query_row(
                        r#"
SELECT COUNT(*)
FROM strategy_feature_snapshots_v1
WHERE strategy_id = ?1
  AND intent_ts_ms >= ?2
  AND intent_status = 'planned'
  AND rung_id IS NULL
  AND slice_id IS NULL
  AND impulse_id LIKE 'liqaccum_plan:%'
  AND (
    execution_context_json IS NULL
    OR execution_context_json NOT LIKE '%"friction":"direction_lock_reject"%'
  )
"#,
                        params![strategy_id.as_str(), from_ts_ms],
                        |row| row.get::<_, i64>(0),
                    )?
                    .max(0) as u64;

                let entry_failed_ratio_pct = if entry_submits > 0 {
                    (entry_failed as f64 / entry_submits as f64) * 100.0
                } else {
                    0.0
                };
                let liqaccum_direction_lock_reject_ratio_pct = if liqaccum_plan_rows > 0 {
                    (liqaccum_direction_lock_rejects as f64 / liqaccum_plan_rows as f64) * 100.0
                } else {
                    0.0
                };
                out.push(RolloutStrategyGateSummary {
                    strategy_id,
                    entry_submits,
                    entry_failed,
                    entry_failed_ratio_pct,
                    submit_storm_groups,
                    liqaccum_direction_lock_rejects,
                    liqaccum_plan_rows,
                    liqaccum_direction_lock_reject_ratio_pct,
                });
            }
            Ok(out)
        })
    }

    pub fn summarize_rollout_integrity_gates_last_hours(
        &self,
        hours: u64,
    ) -> Result<RolloutIntegrityGateSummary> {
        let window_ms = (hours.max(1) as i64).saturating_mul(3_600_000);
        let now_ms = chrono::Utc::now().timestamp_millis();
        let from_ts_ms = now_ms.saturating_sub(window_ms);
        self.with_conn(|conn| {
            let duplicate_snapshot_rows: u64 = conn
                .query_row(
                    r#"
SELECT COALESCE(COUNT(*), 0) FROM (
  SELECT snapshot_id
  FROM strategy_feature_snapshots_v1
  WHERE intent_ts_ms >= ?1
  GROUP BY snapshot_id
  HAVING COUNT(*) > 1
)
"#,
                    params![from_ts_ms],
                    |row| row.get::<_, i64>(0),
                )?
                .max(0) as u64;
            let invalid_transition_rejections: u64 = conn
                .query_row(
                    r#"
SELECT COUNT(*)
FROM strategy_feature_snapshot_transition_rejections
WHERE rejected_at_ms >= ?1
"#,
                    params![from_ts_ms],
                    |row| row.get::<_, i64>(0),
                )?
                .max(0) as u64;

            Ok(RolloutIntegrityGateSummary {
                duplicate_snapshot_rows,
                invalid_transition_rejections,
            })
        })
    }

    pub fn count_reconcile_backfills(&self, scope: Option<&ReportingScope>) -> Result<u64> {
        let scope = Self::effective_reporting_scope(scope);
        let include_legacy = if scope.include_legacy_default {
            1_i64
        } else {
            0_i64
        };
        let prod_only = if scope.prod_only { 1_i64 } else { 0_i64 };
        let prod_filter = prod_event_filter_sql("");
        self.with_conn(|conn| {
            let query = format!(
                r#"
SELECT COUNT(*)
FROM trade_events
WHERE event_type='ENTRY_FILL_RECONCILED'
  AND (?1 IS NULL OR ts_ms >= ?1)
  AND (?2 IS NULL OR ts_ms <= ?2)
  AND (?3 = 1 OR strategy_id != ?4)
  AND (?5 = 0 OR ({}))
"#,
                prod_filter
            );
            let count: i64 = conn.query_row(
                query.as_str(),
                params![
                    scope.from_ts_ms,
                    scope.to_ts_ms,
                    include_legacy,
                    LEGACY_STRATEGY_ID,
                    prod_only
                ],
                |row| row.get(0),
            )?;
            Ok(count.max(0) as u64)
        })
    }

    pub fn total_closed_trades(&self) -> Result<u64> {
        self.total_closed_trades_for_strategy(LEGACY_STRATEGY_ID)
    }

    pub fn total_closed_trades_for_strategy(&self, strategy_id: &str) -> Result<u64> {
        self.total_closed_trades_for_strategy_with_source(strategy_id, OutcomeSource::from_env())
    }

    fn total_closed_trades_for_strategy_with_source(
        &self,
        strategy_id: &str,
        outcome_source: OutcomeSource,
    ) -> Result<u64> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let prod_only = if reporting_prod_only() { 1_i64 } else { 0_i64 };
        let prod_filter = prod_event_filter_sql("");
        let prod_filter_te = prod_event_filter_sql("te");
        self.with_conn(|conn| {
            let query = match outcome_source {
                OutcomeSource::Settlement => format!(
                    r#"
SELECT COUNT(*)
FROM settlements_v2 s
LEFT JOIN trade_events te
  ON te.event_key = s.source_event_key
WHERE s.strategy_id=?1
  AND (?2 = 0 OR te.id IS NULL OR ({}))
"#,
                    prod_filter_te
                ),
                OutcomeSource::Exit => format!(
                    r#"
SELECT COUNT(*)
FROM trade_events
WHERE event_type='EXIT'
  AND strategy_id=?1
  AND (?2 = 0 OR ({}))
"#,
                    prod_filter
                ),
                OutcomeSource::Hybrid => format!(
                    r#"
SELECT (
    COALESCE((
        SELECT COUNT(*)
        FROM settlements_v2 s
        LEFT JOIN trade_events te ON te.event_key = s.source_event_key
        WHERE s.strategy_id=?1
          AND (?2 = 0 OR te.id IS NULL OR ({}))
    ), 0) +
    COALESCE((
        SELECT COUNT(*)
        FROM trade_events
        WHERE event_type='EXIT'
          AND strategy_id=?1
          AND COALESCE(LOWER(reason), '') NOT LIKE '%market_close%'
          AND (?2 = 0 OR ({}))
    ), 0)
)
"#,
                    prod_filter_te, prod_filter
                ),
            };
            let count: i64 =
                conn.query_row(query.as_str(), params![strategy_id, prod_only], |row| {
                    row.get(0)
                })?;
            Ok(count.max(0) as u64)
        })
    }

    pub fn last_reflection_trade_count(&self) -> Result<u64> {
        self.last_reflection_trade_count_for_strategy(LEGACY_STRATEGY_ID)
    }

    pub fn last_reflection_trade_count_for_strategy(&self, strategy_id: &str) -> Result<u64> {
        let strategy_id = normalize_strategy_id(strategy_id);
        self.with_conn(|conn| {
            let count = conn
                .query_row(
                    "SELECT closed_trade_count FROM outcome_reflections WHERE strategy_id=?1 ORDER BY id DESC LIMIT 1",
                    params![strategy_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .unwrap_or(0);
            Ok(count.max(0) as u64)
        })
    }

    pub fn record_outcome_reflection(
        &self,
        closed_trade_count: u64,
        source: &str,
        summary_json: &str,
        reflection_note: Option<&str>,
    ) -> Result<()> {
        self.record_outcome_reflection_for_strategy(
            LEGACY_STRATEGY_ID,
            closed_trade_count,
            source,
            summary_json,
            reflection_note,
        )
    }

    pub fn record_outcome_reflection_for_strategy(
        &self,
        strategy_id: &str,
        closed_trade_count: u64,
        source: &str,
        summary_json: &str,
        reflection_note: Option<&str>,
    ) -> Result<()> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.with_conn(|conn| {
            conn.execute(
                r#"
INSERT INTO outcome_reflections (
    strategy_id, closed_trade_count, created_at_ms, source, summary_json, reflection_note
) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
ON CONFLICT(strategy_id, closed_trade_count) DO UPDATE SET
    created_at_ms=excluded.created_at_ms,
    source=excluded.source,
    summary_json=excluded.summary_json,
    reflection_note=excluded.reflection_note
"#,
                params![
                    strategy_id,
                    closed_trade_count as i64,
                    now_ms,
                    source,
                    summary_json,
                    reflection_note
                ],
            )?;
            Ok(())
        })
    }

    pub fn prune_outcome_tables(
        &self,
        keep_reflections: usize,
        keep_suggestion_history_per_name: usize,
    ) -> Result<()> {
        self.prune_outcome_tables_for_strategy(
            LEGACY_STRATEGY_ID,
            keep_reflections,
            keep_suggestion_history_per_name,
        )
    }

    pub fn prune_outcome_tables_for_strategy(
        &self,
        strategy_id: &str,
        keep_reflections: usize,
        keep_suggestion_history_per_name: usize,
    ) -> Result<()> {
        let strategy_id = normalize_strategy_id(strategy_id);
        self.with_conn(|conn| {
            if keep_reflections > 0 {
                conn.execute(
                    r#"
DELETE FROM outcome_reflections
WHERE id IN (
    SELECT id
    FROM outcome_reflections
    WHERE strategy_id=?2
    ORDER BY created_at_ms DESC, id DESC
    LIMIT -1 OFFSET ?1
)
"#,
                    params![keep_reflections as i64, strategy_id],
                )?;
            }

            if keep_suggestion_history_per_name > 0 {
                conn.execute(
                    r#"
DELETE FROM parameter_suggestion_history
WHERE id IN (
    SELECT id FROM (
        SELECT
            id,
            ROW_NUMBER() OVER (
                PARTITION BY strategy_id, name
                ORDER BY archived_at_ms DESC, id DESC
            ) AS rn
        FROM parameter_suggestion_history
        WHERE strategy_id=?2
    ) ranked
    WHERE rn > ?1
)
"#,
                    params![keep_suggestion_history_per_name as i64, strategy_id],
                )?;
            }
            Ok(())
        })
    }

    pub fn prune_runtime_parameter_tables(&self, keep_events_per_name: usize) -> Result<()> {
        self.prune_runtime_parameter_tables_for_strategy(LEGACY_STRATEGY_ID, keep_events_per_name)
    }

    pub fn prune_runtime_parameter_tables_for_strategy(
        &self,
        strategy_id: &str,
        keep_events_per_name: usize,
    ) -> Result<()> {
        if keep_events_per_name == 0 {
            return Ok(());
        }
        let strategy_id = normalize_strategy_id(strategy_id);
        self.with_conn(|conn| {
            conn.execute(
                r#"
DELETE FROM runtime_parameter_events
WHERE id IN (
    SELECT id FROM (
        SELECT
            id,
            ROW_NUMBER() OVER (
                PARTITION BY strategy_id, name
                ORDER BY applied_at_ms DESC, id DESC
            ) AS rn
        FROM runtime_parameter_events
        WHERE strategy_id=?2
    ) ranked
    WHERE rn > ?1
)
"#,
                params![keep_events_per_name as i64, strategy_id],
            )?;
            Ok(())
        })
    }

    pub fn upsert_parameter_suggestion(&self, row: &ParameterSuggestionRecord) -> Result<()> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let strategy_id = normalize_strategy_id(row.strategy_id.as_str());
        self.with_conn_mut(|conn| {
            let tx = conn.transaction()?;
            tx.execute(
                r#"
INSERT INTO parameter_suggestion_history (
    suggestion_id, strategy_id, name, previous_status, current_value, suggested_value, min_value, max_value,
    confidence, reason, source, archived_at_ms
)
SELECT
    id, strategy_id, name, status, current_value, suggested_value, min_value, max_value,
    confidence, reason, source, ?2
FROM parameter_suggestions
WHERE strategy_id=?1 AND name=?3
"#,
                params![strategy_id, now_ms, row.name],
            )?;
            tx.execute(
                "DELETE FROM parameter_suggestions WHERE strategy_id=?1 AND name=?2",
                params![strategy_id, row.name],
            )?;
            tx.execute(
                r#"
INSERT INTO parameter_suggestions (
    strategy_id, name, current_value, suggested_value, min_value, max_value, confidence, reason, source, status,
    approved_by, approved_at_ms, applied_at_ms, rollback_at_ms, apply_error, created_at_ms, updated_at_ms
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, NULL, NULL, NULL, NULL, ?11, ?12)
"#,
                params![
                    strategy_id,
                    row.name,
                    row.current_value,
                    row.suggested_value,
                    row.min_value,
                    row.max_value,
                    row.confidence,
                    row.reason,
                    row.source,
                    row.status,
                    now_ms,
                    now_ms
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn list_pending_parameter_suggestions(&self) -> Result<Vec<ParameterSuggestionRecord>> {
        self.list_pending_parameter_suggestions_for_strategy(LEGACY_STRATEGY_ID)
    }

    pub fn list_pending_parameter_suggestions_for_strategy(
        &self,
        strategy_id: &str,
    ) -> Result<Vec<ParameterSuggestionRecord>> {
        self.list_parameter_suggestions_by_status_for_strategy(strategy_id, "pending", 200)
    }

    pub fn list_parameter_suggestions_by_status(
        &self,
        status: &str,
        limit: usize,
    ) -> Result<Vec<ParameterSuggestionRecord>> {
        self.list_parameter_suggestions_by_status_for_strategy(LEGACY_STRATEGY_ID, status, limit)
    }

    pub fn list_parameter_suggestions_by_status_for_strategy(
        &self,
        strategy_id: &str,
        status: &str,
        limit: usize,
    ) -> Result<Vec<ParameterSuggestionRecord>> {
        let strategy_id = normalize_strategy_id(strategy_id);
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT id, strategy_id, name, current_value, suggested_value, min_value, max_value, confidence, reason, source, status
FROM parameter_suggestions
WHERE strategy_id = ?1
  AND status = ?2
ORDER BY updated_at_ms DESC
LIMIT ?3
"#,
            )?;
            let rows = stmt.query_map(params![strategy_id, status, limit as i64], |row| {
                Ok(ParameterSuggestionRecord {
                    id: row.get(0)?,
                    strategy_id: row.get(1)?,
                    name: row.get(2)?,
                    current_value: row.get(3)?,
                    suggested_value: row.get(4)?,
                    min_value: row.get(5)?,
                    max_value: row.get(6)?,
                    confidence: row.get(7)?,
                    reason: row.get(8)?,
                    source: row.get(9)?,
                    status: row.get(10)?,
                })
            })?;

            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn update_parameter_suggestion_status(
        &self,
        suggestion_id: i64,
        status: &str,
        actor: &str,
        apply_error: Option<&str>,
    ) -> Result<()> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.with_conn(|conn| {
            match status {
                "approved" => {
                    conn.execute(
                        r#"
UPDATE parameter_suggestions
SET status='approved',
    approved_by=?2,
    approved_at_ms=?3,
    updated_at_ms=?3,
    apply_error=NULL
WHERE id=?1
"#,
                        params![suggestion_id, actor, now_ms],
                    )?;
                }
                "applied" => {
                    conn.execute(
                        r#"
UPDATE parameter_suggestions
SET status='applied',
    applied_at_ms=?2,
    updated_at_ms=?2,
    apply_error=NULL
WHERE id=?1
"#,
                        params![suggestion_id, now_ms],
                    )?;
                }
                "rolled_back" => {
                    conn.execute(
                        r#"
UPDATE parameter_suggestions
SET status='rolled_back',
    rollback_at_ms=?2,
    updated_at_ms=?2
WHERE id=?1
"#,
                        params![suggestion_id, now_ms],
                    )?;
                }
                "rejected" => {
                    conn.execute(
                        r#"
UPDATE parameter_suggestions
SET status='rejected',
    updated_at_ms=?2,
    apply_error=?3
WHERE id=?1
"#,
                        params![suggestion_id, now_ms, apply_error],
                    )?;
                }
                _ => {
                    conn.execute(
                        r#"
UPDATE parameter_suggestions
SET status=?2,
    updated_at_ms=?3
WHERE id=?1
"#,
                        params![suggestion_id, status, now_ms],
                    )?;
                }
            }
            Ok(())
        })
    }

    pub fn list_runtime_parameter_states(&self) -> Result<Vec<RuntimeParameterStateRecord>> {
        self.list_runtime_parameter_states_for_strategy(LEGACY_STRATEGY_ID)
    }

    pub fn list_runtime_parameter_states_for_strategy(
        &self,
        strategy_id: &str,
    ) -> Result<Vec<RuntimeParameterStateRecord>> {
        let strategy_id = normalize_strategy_id(strategy_id);
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT strategy_id, name, value_json, version, updated_at_ms, updated_by
FROM runtime_parameter_state
WHERE strategy_id=?1
ORDER BY name
"#,
            )?;
            let rows = stmt.query_map(params![strategy_id], |row| {
                Ok(RuntimeParameterStateRecord {
                    strategy_id: row.get(0)?,
                    name: row.get(1)?,
                    value_json: row.get(2)?,
                    version: row.get(3)?,
                    updated_at_ms: row.get(4)?,
                    updated_by: row.get(5)?,
                })
            })?;

            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn upsert_runtime_parameter_state(
        &self,
        name: &str,
        value_json: &str,
        updated_by: &str,
    ) -> Result<()> {
        self.upsert_runtime_parameter_state_for_strategy(
            LEGACY_STRATEGY_ID,
            name,
            value_json,
            updated_by,
        )
    }

    pub fn upsert_runtime_parameter_state_for_strategy(
        &self,
        strategy_id: &str,
        name: &str,
        value_json: &str,
        updated_by: &str,
    ) -> Result<()> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.with_conn(|conn| {
            conn.execute(
                r#"
INSERT INTO runtime_parameter_state (
    strategy_id, name, value_json, version, updated_at_ms, updated_by
) VALUES (?1, ?2, ?3, 1, ?4, ?5)
ON CONFLICT(strategy_id, name) DO UPDATE SET
    value_json=excluded.value_json,
    version=runtime_parameter_state.version + 1,
    updated_at_ms=excluded.updated_at_ms,
    updated_by=excluded.updated_by
"#,
                params![strategy_id, name, value_json, now_ms, updated_by],
            )?;
            Ok(())
        })
    }

    pub fn record_runtime_parameter_event(
        &self,
        name: &str,
        old_value_json: Option<&str>,
        new_value_json: &str,
        reason: &str,
        source_suggestion_id: Option<i64>,
        applied_by: &str,
    ) -> Result<()> {
        self.record_runtime_parameter_event_for_strategy(
            LEGACY_STRATEGY_ID,
            name,
            old_value_json,
            new_value_json,
            reason,
            source_suggestion_id,
            applied_by,
        )
    }

    pub fn record_runtime_parameter_event_for_strategy(
        &self,
        strategy_id: &str,
        name: &str,
        old_value_json: Option<&str>,
        new_value_json: &str,
        reason: &str,
        source_suggestion_id: Option<i64>,
        applied_by: &str,
    ) -> Result<()> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.with_conn(|conn| {
            conn.execute(
                r#"
INSERT INTO runtime_parameter_events (
    strategy_id, name, old_value_json, new_value_json, reason, source_suggestion_id, applied_at_ms, applied_by
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
"#,
                params![
                    strategy_id,
                    name,
                    old_value_json,
                    new_value_json,
                    reason,
                    source_suggestion_id,
                    now_ms,
                    applied_by
                ],
            )?;
            Ok(())
        })
    }

    pub fn apply_runtime_suggestion_atomic(
        &self,
        suggestion_id: i64,
        actor: &str,
        parameter_name: &str,
        old_value_json: Option<&str>,
        new_value_json: &str,
        reason: &str,
    ) -> Result<()> {
        self.apply_runtime_suggestion_atomic_for_strategy(
            LEGACY_STRATEGY_ID,
            suggestion_id,
            actor,
            parameter_name,
            old_value_json,
            new_value_json,
            reason,
        )
    }

    pub fn apply_runtime_suggestion_atomic_for_strategy(
        &self,
        strategy_id: &str,
        suggestion_id: i64,
        actor: &str,
        parameter_name: &str,
        old_value_json: Option<&str>,
        new_value_json: &str,
        reason: &str,
    ) -> Result<()> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.with_conn_mut(|conn| {
            let tx = conn.transaction()?;

            let approved_rows = tx.execute(
                r#"
UPDATE parameter_suggestions
SET status='approved',
    approved_by=?2,
    approved_at_ms=?3,
    updated_at_ms=?3,
    apply_error=NULL
WHERE id=?1 AND strategy_id=?4
"#,
                params![suggestion_id, actor, now_ms, strategy_id],
            )?;
            if approved_rows == 0 {
                return Err(anyhow!(
                    "runtime suggestion {} not found during atomic apply",
                    suggestion_id
                ));
            }

            tx.execute(
                r#"
INSERT INTO runtime_parameter_state (
    strategy_id, name, value_json, version, updated_at_ms, updated_by
) VALUES (?1, ?2, ?3, 1, ?4, ?5)
ON CONFLICT(strategy_id, name) DO UPDATE SET
    value_json=excluded.value_json,
    version=runtime_parameter_state.version + 1,
    updated_at_ms=excluded.updated_at_ms,
    updated_by=excluded.updated_by
"#,
                params![strategy_id, parameter_name, new_value_json, now_ms, actor],
            )?;

            tx.execute(
                r#"
INSERT INTO runtime_parameter_events (
    strategy_id, name, old_value_json, new_value_json, reason, source_suggestion_id, applied_at_ms, applied_by
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
"#,
                params![
                    strategy_id,
                    parameter_name,
                    old_value_json,
                    new_value_json,
                    reason,
                    suggestion_id,
                    now_ms,
                    actor
                ],
            )?;

            tx.execute(
                r#"
UPDATE parameter_suggestions
SET status='applied',
    applied_at_ms=?2,
    updated_at_ms=?2,
    apply_error=NULL
WHERE id=?1 AND strategy_id=?3
"#,
                params![suggestion_id, now_ms, strategy_id],
            )?;

            tx.commit()?;
            Ok(())
        })
    }

    pub fn upsert_pending_order(&self, row: &PendingOrderRecord) -> Result<()> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let normalized_side = normalize_side(row.side.as_str());
        let normalized_status = normalize_pending_status(row.status.as_str());
        let normalized_entry_mode = normalize_entry_mode(row.entry_mode.as_str());
        let normalized_strategy_id = normalize_strategy_id(row.strategy_id.as_str());
        let run_id = runtime_run_id();
        let strategy_cohort = runtime_strategy_cohort();
        let normalized_asset_symbol =
            normalize_asset_symbol(row.asset_symbol.as_deref(), None, None);
        let normalized_condition_id = row
            .condition_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string);
        let filled_at_ms = if normalized_status == "FILLED" {
            Some(now_ms)
        } else {
            None
        };
        self.with_conn(|conn| {
            conn.execute(
                r#"
INSERT INTO pending_orders (
    order_id, trade_key, token_id, condition_id, timeframe, strategy_id, run_id, strategy_cohort, asset_symbol, entry_mode,
    period_timestamp, price, size_usd, side, status, filled_at_ms, created_at_ms, updated_at_ms
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
ON CONFLICT(order_id) DO UPDATE SET
    trade_key=excluded.trade_key,
    token_id=excluded.token_id,
    condition_id=COALESCE(excluded.condition_id, pending_orders.condition_id),
    timeframe=excluded.timeframe,
    strategy_id=excluded.strategy_id,
    run_id=excluded.run_id,
    strategy_cohort=excluded.strategy_cohort,
    asset_symbol=excluded.asset_symbol,
    entry_mode=excluded.entry_mode,
    period_timestamp=excluded.period_timestamp,
    price=excluded.price,
    size_usd=excluded.size_usd,
    side=excluded.side,
    status=excluded.status,
    filled_at_ms=CASE
        WHEN excluded.status='FILLED' THEN COALESCE(pending_orders.filled_at_ms, excluded.filled_at_ms)
        ELSE pending_orders.filled_at_ms
    END,
    updated_at_ms=excluded.updated_at_ms
"#,
                params![
                    row.order_id,
                    row.trade_key,
                    row.token_id,
                    normalized_condition_id,
                    row.timeframe,
                    normalized_strategy_id,
                    run_id,
                    strategy_cohort,
                    normalized_asset_symbol,
                    normalized_entry_mode,
                    row.period_timestamp as i64,
                    row.price,
                    row.size_usd,
                    normalized_side,
                    normalized_status,
                    filled_at_ms,
                    now_ms,
                    now_ms
                ],
            )?;
            Ok(())
        })
    }

    pub fn update_pending_order_status(&self, order_id: &str, status: &str) -> Result<()> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let normalized_status = normalize_pending_status(status);
        self.with_conn(|conn| {
            conn.execute(
                r#"
UPDATE pending_orders
SET status=CASE
        WHEN status='FILLED' AND ?2!='FILLED' THEN status
        ELSE ?2
    END,
    filled_at_ms=CASE
        WHEN status='FILLED' OR ?2='FILLED' THEN COALESCE(filled_at_ms, ?3)
        ELSE filled_at_ms
    END,
    updated_at_ms=?3
WHERE order_id=?1
  AND (
      CASE
          WHEN status='FILLED' AND ?2!='FILLED' THEN status
          ELSE ?2
      END
  ) != status
"#,
                params![order_id, normalized_status, now_ms],
            )?;
            Ok(())
        })
    }

    pub fn list_reconcile_pending_orders_active(&self) -> Result<Vec<PendingOrderRecord>> {
        self.with_conn_read(|conn| {
            let query = format!(
                r#"
SELECT order_id, trade_key, token_id, condition_id, timeframe, strategy_id, asset_symbol, entry_mode, period_timestamp, price, size_usd, side, status
FROM pending_orders
WHERE status IN ({})
ORDER BY updated_at_ms DESC
"#,
                ACTIVE_PENDING_ORDER_STATUSES_SQL
            );
            let mut stmt = conn.prepare(query.as_str())?;
            let rows = stmt.query_map([], |row| {
                let period_ts: i64 = row.get(8)?;
                Ok(PendingOrderRecord {
                    order_id: row.get(0)?,
                    trade_key: row.get(1)?,
                    token_id: row.get(2)?,
                    condition_id: row.get(3)?,
                    timeframe: row.get(4)?,
                    strategy_id: row.get(5)?,
                    asset_symbol: row.get(6)?,
                    entry_mode: row.get(7)?,
                    period_timestamp: period_ts.max(0) as u64,
                    price: row.get(9)?,
                    size_usd: row.get(10)?,
                    side: row.get(11)?,
                    status: row.get(12)?,
                })
            })?;

            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn list_reconcile_pending_orders_retry(
        &self,
        max_rows: usize,
        min_retry_age_ms: i64,
        max_period_age_sec: i64,
    ) -> Result<Vec<PendingOrderRecord>> {
        let max_rows = i64::try_from(max_rows.max(1)).ok().unwrap_or(i64::MAX);
        let min_retry_age_ms = min_retry_age_ms.max(0);
        let max_period_age_sec = max_period_age_sec.max(0);
        let now_ms = chrono::Utc::now().timestamp_millis();
        let cutoff_updated_ms = now_ms.saturating_sub(min_retry_age_ms);
        let now_sec = chrono::Utc::now().timestamp();
        let cutoff_period_ts = now_sec.saturating_sub(max_period_age_sec).max(0);

        self.with_conn_read(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT order_id, trade_key, token_id, condition_id, timeframe, strategy_id, asset_symbol, entry_mode, period_timestamp, price, size_usd, side, status
FROM pending_orders
WHERE status IN ('STALE', 'RECONCILE_FAILED')
  AND updated_at_ms <= ?1
  AND period_timestamp >= ?2
ORDER BY updated_at_ms ASC
LIMIT ?3
"#,
            )?;
            let rows = stmt.query_map(
                params![cutoff_updated_ms, cutoff_period_ts, max_rows],
                |row| {
                    let period_ts: i64 = row.get(8)?;
                    Ok(PendingOrderRecord {
                        order_id: row.get(0)?,
                        trade_key: row.get(1)?,
                        token_id: row.get(2)?,
                        condition_id: row.get(3)?,
                        timeframe: row.get(4)?,
                        strategy_id: row.get(5)?,
                        asset_symbol: row.get(6)?,
                        entry_mode: row.get(7)?,
                        period_timestamp: period_ts.max(0) as u64,
                        price: row.get(9)?,
                        size_usd: row.get(10)?,
                        side: row.get(11)?,
                        status: row.get(12)?,
                    })
                },
            )?;

            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn get_pending_order_by_id(&self, order_id: &str) -> Result<Option<PendingOrderRecord>> {
        self.with_conn_read(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT order_id, trade_key, token_id, condition_id, timeframe, strategy_id, asset_symbol, entry_mode, period_timestamp, price, size_usd, side, status
FROM pending_orders
WHERE order_id=?1
LIMIT 1
"#,
            )?;
            let row = stmt
                .query_row(params![order_id], |row| {
                    let period_ts: i64 = row.get(8)?;
                    Ok(PendingOrderRecord {
                        order_id: row.get(0)?,
                        trade_key: row.get(1)?,
                        token_id: row.get(2)?,
                        condition_id: row.get(3)?,
                        timeframe: row.get(4)?,
                        strategy_id: row.get(5)?,
                        asset_symbol: row.get(6)?,
                        entry_mode: row.get(7)?,
                        period_timestamp: period_ts.max(0) as u64,
                        price: row.get(9)?,
                        size_usd: row.get(10)?,
                        side: row.get(11)?,
                        status: row.get(12)?,
                    })
                })
                .optional()?;
            Ok(row)
        })
    }

    pub fn has_trade_event_key(&self, event_key: &str) -> Result<bool> {
        self.with_conn_read(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT COUNT(1)
FROM trade_events
WHERE event_key=?1
"#,
            )?;
            let count: i64 = stmt.query_row(params![event_key], |row| row.get(0))?;
            Ok(count > 0)
        })
    }

    pub fn has_entry_fill_event_for_order_id(&self, order_id: &str) -> Result<bool> {
        self.with_conn_read(|conn| has_canonical_entry_fill_for_order(conn, order_id))
    }

    pub fn mark_pending_order_filled_with_event(
        &self,
        order_id: &str,
        event: &TradeEventRecord,
    ) -> Result<bool> {
        let strategy_id = normalize_strategy_id(event.strategy_id.as_str());
        let normalized_event_type = event.event_type.trim().to_ascii_uppercase();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let fill_ts_ms = if event.ts_ms > 0 { event.ts_ms } else { now_ms };
        let inserted = self.with_conn_mut(|conn| {
            let tx = conn.transaction()?;
            let pending_context: Option<(String, String, String, Option<String>)> = tx
                .query_row(
                    "SELECT asset_symbol, run_id, strategy_cohort, condition_id FROM pending_orders WHERE order_id=?1 LIMIT 1",
                    params![order_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            let pending_asset_symbol = pending_context
                .as_ref()
                .map(|(asset, _, _, _)| asset.as_str())
                .filter(|asset| !asset.trim().is_empty());
            let pending_condition_id = pending_context
                .as_ref()
                .and_then(|(_, _, _, condition_id)| condition_id.clone())
                .filter(|condition_id| !condition_id.trim().is_empty());
            let run_id = normalize_run_id(
                pending_context
                    .as_ref()
                    .map(|(_, run_id, _, _)| run_id.as_str())
                    .or(Some(runtime_run_id())),
            );
            let strategy_cohort = normalize_strategy_cohort(
                pending_context
                    .as_ref()
                    .map(|(_, _, cohort, _)| cohort.as_str())
                    .or(Some(runtime_strategy_cohort())),
            );
            let condition_id = event
                .condition_id
                .clone()
                .or(pending_condition_id)
                .filter(|condition_id| !condition_id.trim().is_empty());
            let asset_symbol = normalize_asset_symbol(
                event
                    .asset_symbol
                    .as_deref()
                    .or(pending_asset_symbol),
                event.token_type.as_deref(),
                None,
            );
            let inserted = tx.execute(
                r#"
INSERT INTO trade_events (
    event_key, ts_ms, period_timestamp, timeframe, strategy_id, run_id, strategy_cohort, asset_symbol,
    condition_id, token_id, token_type, side, event_type, price, units, notional_usd, pnl_usd, reason
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
ON CONFLICT(event_key) DO NOTHING
"#,
                params![
                    event.event_key,
                    event.ts_ms,
                    event.period_timestamp as i64,
                    event.timeframe,
                    strategy_id,
                    run_id,
                    strategy_cohort,
                    asset_symbol,
                    condition_id,
                    event.token_id,
                    event.token_type,
                    event.side,
                    normalized_event_type,
                    event.price,
                    event.units,
                    event.notional_usd,
                    event.pnl_usd,
                    event.reason,
                ],
            )?;
            if inserted > 0 {
                dual_write_trade_event_v2(&tx, event, strategy_id.as_str())?;
            }
            tx.execute(
                r#"
UPDATE pending_orders
SET status='FILLED',
    filled_at_ms=COALESCE(filled_at_ms, ?2),
    updated_at_ms=?3
WHERE order_id=?1
"#,
                params![order_id, fill_ts_ms, now_ms],
            )?;
            tx.commit()?;
            Ok(inserted > 0)
        })?;
        if inserted {
            if let Err(e) = self.upsert_strategy_feature_snapshot_from_trade_event(event) {
                warn!(
                    "Failed to upsert strategy feature snapshot from atomic fill event {}: {}",
                    order_id, e
                );
            }
        }
        Ok(inserted)
    }

    pub fn count_entry_submits_for_scope(
        &self,
        strategy_id: &str,
        timeframe: &str,
        period_timestamp: u64,
        token_id: &str,
        side: &str,
    ) -> Result<u64> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let timeframe = timeframe.trim().to_ascii_lowercase();
        let token_id = token_id.trim().to_string();
        let side = normalize_side(side);
        self.with_conn_read(|conn| {
            let count: i64 = conn.query_row(
                r#"
SELECT COUNT(*)
FROM trade_events
WHERE strategy_id=?1
  AND timeframe=?2
  AND period_timestamp=?3
  AND token_id=?4
  AND side=?5
  AND event_type='ENTRY_SUBMIT'
"#,
                params![
                    strategy_id,
                    timeframe,
                    period_timestamp as i64,
                    token_id,
                    side
                ],
                |row| row.get(0),
            )?;
            Ok(count.max(0) as u64)
        })
    }

    pub fn position_entry_basis_for_scope(
        &self,
        strategy_id: &str,
        timeframe: &str,
        period_timestamp: u64,
        token_id: &str,
    ) -> Result<Option<(f64, f64)>> {
        let key = position_key(strategy_id, timeframe, period_timestamp, token_id);
        self.with_conn_read(|conn| {
            conn.query_row(
                r#"
SELECT
    COALESCE(entry_units, 0.0),
    COALESCE(entry_notional_usd, 0.0) + COALESCE(entry_fee_usd, 0.0)
FROM positions_v2
WHERE position_key=?1
LIMIT 1
"#,
                params![key],
                |row| Ok((row.get::<_, f64>(0)?, row.get::<_, f64>(1)?)),
            )
            .optional()
            .map_err(Into::into)
        })
    }

    pub fn backfill_strategy_feature_snapshots_from_trade_events(
        &self,
        lookback_hours: i64,
        max_rows: usize,
    ) -> Result<u64> {
        let safe_hours = lookback_hours.max(1);
        let from_ts_ms = chrono::Utc::now()
            .timestamp_millis()
            .saturating_sub(safe_hours.saturating_mul(3_600_000));
        let rows = self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT
    event_key,
    ts_ms,
    period_timestamp,
    timeframe,
    strategy_id,
    asset_symbol,
    condition_id,
    token_id,
    token_type,
    side,
    event_type,
    price,
    units,
    notional_usd,
    pnl_usd,
    reason
FROM trade_events
WHERE ts_ms >= ?1
  AND strategy_id != 'legacy_default'
  AND token_id IS NOT NULL
  AND TRIM(token_id) != ''
ORDER BY ts_ms DESC
LIMIT ?2
"#,
            )?;
            let iter = stmt.query_map(params![from_ts_ms, max_rows as i64], |row| {
                let period_timestamp: i64 = row.get(2)?;
                Ok(TradeEventRecord {
                    event_key: row.get(0)?,
                    ts_ms: row.get(1)?,
                    period_timestamp: period_timestamp.max(0) as u64,
                    timeframe: row.get(3)?,
                    strategy_id: row.get(4)?,
                    asset_symbol: row.get(5)?,
                    condition_id: row.get(6)?,
                    token_id: row.get(7)?,
                    token_type: row.get(8)?,
                    side: row.get(9)?,
                    event_type: row.get(10)?,
                    price: row.get(11)?,
                    units: row.get(12)?,
                    notional_usd: row.get(13)?,
                    pnl_usd: row.get(14)?,
                    reason: row.get(15)?,
                })
            })?;
            let mut out = Vec::new();
            for row in iter {
                out.push(row?);
            }
            Ok(out)
        })?;

        let mut touched = 0_u64;
        let mut failed = 0_u64;
        for row in rows {
            match self.upsert_strategy_feature_snapshot_from_trade_event(&row) {
                Ok(Some(_)) => {
                    touched = touched.saturating_add(1);
                }
                Ok(None) => {}
                Err(_) => {
                    failed = failed.saturating_add(1);
                }
            }
        }
        if failed > 0 {
            warn!(
                "Feature snapshot backfill skipped {} trade-event row(s) due to transition/validation errors",
                failed
            );
        }
        Ok(touched)
    }

    pub fn backfill_missing_entry_fill_events(
        &self,
        lookback_hours: i64,
        max_rows: usize,
    ) -> Result<u64> {
        let safe_hours = lookback_hours.max(1);
        let from_ts_ms = chrono::Utc::now()
            .timestamp_millis()
            .saturating_sub(safe_hours.saturating_mul(3_600_000));
        #[derive(Debug)]
        struct MissingFillRow {
            order_id: String,
            trade_key: String,
            token_id: String,
            timeframe: String,
            strategy_id: String,
            condition_id: Option<String>,
            asset_symbol: Option<String>,
            period_timestamp: u64,
            price: f64,
            size_usd: f64,
            filled_at_ms: i64,
        }
        let rows = self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT
    p.order_id,
    p.trade_key,
    p.token_id,
    p.timeframe,
    p.strategy_id,
    p.condition_id,
    p.asset_symbol,
    p.period_timestamp,
    p.price,
    p.size_usd,
    COALESCE(p.filled_at_ms, p.updated_at_ms) AS fill_ts_ms
FROM pending_orders p
LEFT JOIN trade_events e
  ON e.event_key = ('entry_fill_order:' || p.order_id)
WHERE p.side='BUY'
  AND (p.status='FILLED' OR p.filled_at_ms IS NOT NULL)
  AND COALESCE(p.filled_at_ms, p.updated_at_ms) >= ?1
  AND p.strategy_id != 'legacy_default'
  AND UPPER(TRIM(COALESCE(p.entry_mode, 'LEGACY'))) != 'RESTORED'
  AND p.order_id NOT LIKE 'restored:%'
  AND p.order_id NOT LIKE 'local:%'
  AND e.id IS NULL
ORDER BY COALESCE(p.filled_at_ms, p.updated_at_ms) DESC
LIMIT ?2
"#,
            )?;
            let iter = stmt.query_map(params![from_ts_ms, max_rows as i64], |row| {
                let period_timestamp: i64 = row.get(7)?;
                Ok(MissingFillRow {
                    order_id: row.get(0)?,
                    trade_key: row.get(1)?,
                    token_id: row.get(2)?,
                    timeframe: row.get(3)?,
                    strategy_id: row.get(4)?,
                    condition_id: row.get(5)?,
                    asset_symbol: row.get(6)?,
                    period_timestamp: period_timestamp.max(0) as u64,
                    price: row.get(8)?,
                    size_usd: row.get(9)?,
                    filled_at_ms: row.get(10)?,
                })
            })?;
            let mut out = Vec::new();
            for row in iter {
                out.push(row?);
            }
            Ok(out)
        })?;

        let mut inserted = 0_u64;
        for row in rows {
            let order_id = row.order_id.trim().to_string();
            if order_id.is_empty() {
                continue;
            }
            let price = row.price;
            let units = if price > 0.0 {
                Some((row.size_usd / price).max(0.0))
            } else {
                None
            };
            let reason = serde_json::to_string(&json!({
                "mode": "startup_fill_backfill",
                "reason": "pending_order_filled_without_entry_fill",
                "order_id": order_id,
                "trade_key": row.trade_key
            }))
            .ok();
            let event = TradeEventRecord {
                event_key: Some(format!("entry_fill_order:{}", order_id)),
                ts_ms: row.filled_at_ms,
                period_timestamp: row.period_timestamp,
                timeframe: row.timeframe,
                strategy_id: row.strategy_id,
                asset_symbol: row.asset_symbol,
                condition_id: row.condition_id,
                token_id: Some(row.token_id),
                token_type: None,
                side: Some("BUY".to_string()),
                event_type: "ENTRY_FILL".to_string(),
                price: if price > 0.0 { Some(price) } else { None },
                units,
                notional_usd: Some(row.size_usd.max(0.0)),
                pnl_usd: None,
                reason,
            };
            if self.record_trade_event(&event).is_ok() {
                inserted = inserted.saturating_add(1);
            }
        }
        Ok(inserted)
    }

    pub fn reconcile_market_close_exit_pnl_from_settlements(
        &self,
        lookback_hours: i64,
    ) -> Result<u64> {
        let safe_hours = lookback_hours.max(1);
        let from_ts_ms = chrono::Utc::now()
            .timestamp_millis()
            .saturating_sub(safe_hours.saturating_mul(3_600_000));
        self.with_conn(|conn| {
            let changed = conn.execute(
                r#"
UPDATE trade_events
SET
    pnl_usd = (
        SELECT s.settled_pnl_usd
        FROM settlements_v2 s
        WHERE s.source_event_key = trade_events.event_key
        LIMIT 1
    ),
    notional_usd = (
        SELECT s.payout_usd
        FROM settlements_v2 s
        WHERE s.source_event_key = trade_events.event_key
        LIMIT 1
    ),
    units = (
        SELECT s.units
        FROM settlements_v2 s
        WHERE s.source_event_key = trade_events.event_key
        LIMIT 1
    )
WHERE event_type='EXIT'
  AND ts_ms >= ?1
  AND LOWER(COALESCE(reason, '')) LIKE '%market_close%'
  AND event_key IS NOT NULL
  AND EXISTS (
        SELECT 1
        FROM settlements_v2 s
        WHERE s.source_event_key = trade_events.event_key
          AND (
                ABS(COALESCE(trade_events.pnl_usd, 0.0) - COALESCE(s.settled_pnl_usd, 0.0)) > 1e-9
             OR ABS(COALESCE(trade_events.notional_usd, 0.0) - COALESCE(s.payout_usd, 0.0)) > 1e-9
             OR ABS(COALESCE(trade_events.units, 0.0) - COALESCE(s.units, 0.0)) > 1e-9
          )
  )
"#,
                params![from_ts_ms],
            )?;
            Ok(changed.max(0) as u64)
        })
    }

    pub fn prune_stale_replay_transition_rejections(&self) -> Result<u64> {
        self.with_conn(|conn| {
            let deleted = conn.execute(
                r#"
DELETE FROM strategy_feature_snapshot_transition_rejections
WHERE (
        current_execution_status='filled'
        AND next_execution_status IN ('none', 'ack', 'fill_partial')
      )
   OR (
        current_intent_status='submitted'
        AND next_intent_status='planned'
      )
"#,
                [],
            )?;
            Ok(deleted.max(0) as u64)
        })
    }

    pub fn remove_pending_order(&self, order_id: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM pending_orders WHERE order_id=?1",
                params![order_id],
            )?;
            Ok(())
        })
    }

    pub fn remove_active_pending_orders_for_strategy(&self, strategy_id: &str) -> Result<u64> {
        let strategy_id = normalize_strategy_id(strategy_id);
        self.with_conn(|conn| {
            let query = format!(
                "DELETE FROM pending_orders WHERE strategy_id=?1 AND status IN ({})",
                ACTIVE_PENDING_ORDER_STATUSES_SQL
            );
            let deleted = conn.execute(query.as_str(), params![strategy_id.as_str()])?;
            Ok(deleted.max(0) as u64)
        })
    }

    pub fn archive_and_prune_pending_orders(
        &self,
        older_than_ms: i64,
        limit: usize,
    ) -> Result<u64> {
        if limit == 0 {
            return Ok(0);
        }
        self.with_conn_mut_profiled("pending_orders.prune_terminal", Some(limit), |conn| {
            let tx = conn.transaction()?;
            let query = format!(
                r#"
DELETE FROM pending_orders
WHERE rowid IN (
    SELECT rowid
    FROM pending_orders
    WHERE status NOT IN ({})
      AND updated_at_ms > 0
      AND updated_at_ms < ?1
    ORDER BY updated_at_ms ASC
    LIMIT ?2
)
"#,
                ACTIVE_PENDING_ORDER_STATUSES_SQL
            );
            let deleted = tx.execute(
                query.as_str(),
                params![older_than_ms, i64::try_from(limit).ok().unwrap_or(i64::MAX)],
            )?;
            tx.commit()?;
            Ok(deleted.max(0) as u64)
        })
    }

    pub fn clear_mm_market_states_for_strategy(&self, strategy_id: &str) -> Result<u64> {
        let strategy_id = normalize_strategy_id(strategy_id);
        self.with_conn(|conn| {
            let deleted = conn.execute(
                "DELETE FROM mm_market_states_v1 WHERE strategy_id=?1",
                params![strategy_id.as_str()],
            )?;
            Ok(deleted.max(0) as u64)
        })
    }

    pub fn has_open_pending_order(
        &self,
        timeframe: &str,
        entry_mode: &str,
        period_timestamp: u64,
        token_id: &str,
        side: &str,
    ) -> Result<bool> {
        let normalized_side = normalize_side(side);
        let normalized_entry_mode = normalize_entry_mode(entry_mode);
        self.with_conn_read(|conn| {
            let query = format!(
                r#"
SELECT COUNT(1)
FROM pending_orders
WHERE timeframe=?1
  AND period_timestamp=?2
  AND token_id=?3
  AND side=?4
  AND (
        entry_mode=?5
        OR entry_mode IN ('LEGACY', 'UNKNOWN')
      )
  AND status IN ({})
"#,
                ACTIVE_PENDING_ORDER_STATUSES_SQL
            );
            let mut stmt = conn.prepare(query.as_str())?;
            let count: i64 = stmt.query_row(
                params![
                    timeframe,
                    period_timestamp as i64,
                    token_id,
                    normalized_side,
                    normalized_entry_mode
                ],
                |row| row.get(0),
            )?;
            Ok(count > 0)
        })
    }

    pub fn has_open_pending_order_for_strategy(
        &self,
        timeframe: &str,
        entry_mode: &str,
        strategy_id: &str,
        period_timestamp: u64,
        token_id: &str,
        side: &str,
    ) -> Result<bool> {
        let normalized_side = normalize_side(side);
        let normalized_entry_mode = normalize_entry_mode(entry_mode);
        let normalized_strategy_id = strategy_id.trim().to_ascii_lowercase();
        self.with_conn_read(|conn| {
            let query = format!(
                r#"
SELECT COUNT(1)
FROM pending_orders
WHERE timeframe=?1
  AND period_timestamp=?2
  AND token_id=?3
  AND side=?4
  AND entry_mode=?5
  AND strategy_id=?6
  AND status IN ({})
"#,
                ACTIVE_PENDING_ORDER_STATUSES_SQL
            );
            let mut stmt = conn.prepare(query.as_str())?;
            let count: i64 = stmt.query_row(
                params![
                    timeframe,
                    period_timestamp as i64,
                    token_id,
                    normalized_side,
                    normalized_entry_mode,
                    normalized_strategy_id
                ],
                |row| row.get(0),
            )?;
            Ok(count > 0)
        })
    }

    pub fn list_open_pending_orders_for_strategy_scope(
        &self,
        timeframe: &str,
        entry_mode: &str,
        strategy_id: &str,
        period_timestamp: u64,
        token_id: &str,
        side: &str,
    ) -> Result<Vec<PendingOrderRecord>> {
        let normalized_side = normalize_side(side);
        let normalized_entry_mode = normalize_entry_mode(entry_mode);
        let normalized_strategy_id = strategy_id.trim().to_ascii_lowercase();
        self.with_conn_read_profiled("pending_orders.scope_list", Some(64), |conn| {
            let query = format!(
                r#"
SELECT order_id, trade_key, token_id, condition_id, timeframe, strategy_id, asset_symbol, entry_mode, period_timestamp, price, size_usd, side, status
FROM pending_orders
WHERE timeframe=?1
  AND period_timestamp=?2
  AND token_id=?3
  AND side=?4
  AND entry_mode=?5
  AND strategy_id=?6
  AND status IN ({})
ORDER BY updated_at_ms DESC
"#,
                ACTIVE_PENDING_ORDER_STATUSES_SQL
            );
            let mut stmt = conn.prepare(query.as_str())?;
            let rows = stmt.query_map(
                params![
                    timeframe,
                    period_timestamp as i64,
                    token_id,
                    normalized_side,
                    normalized_entry_mode,
                    normalized_strategy_id
                ],
                |row| {
                    let period_ts: i64 = row.get(8)?;
                    Ok(PendingOrderRecord {
                        order_id: row.get(0)?,
                        trade_key: row.get(1)?,
                        token_id: row.get(2)?,
                        condition_id: row.get(3)?,
                        timeframe: row.get(4)?,
                        strategy_id: row.get(5)?,
                        asset_symbol: row.get(6)?,
                        entry_mode: row.get(7)?,
                        period_timestamp: period_ts.max(0) as u64,
                        price: row.get(9)?,
                        size_usd: row.get(10)?,
                        side: row.get(11)?,
                        status: row.get(12)?,
                    })
                },
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn list_open_pending_orders_for_market_scope(
        &self,
        timeframe: &str,
        entry_mode: &str,
        strategy_id: &str,
        period_timestamp: u64,
        condition_id: &str,
    ) -> Result<Vec<PendingOrderRecord>> {
        self.list_open_pending_orders_for_market_scope_labeled(
            timeframe,
            entry_mode,
            strategy_id,
            period_timestamp,
            condition_id,
            "pending_orders.market_scope_list",
        )
    }

    pub fn list_open_pending_orders_for_market_scope_labeled(
        &self,
        timeframe: &str,
        entry_mode: &str,
        strategy_id: &str,
        period_timestamp: u64,
        condition_id: &str,
        profile_scope: &'static str,
    ) -> Result<Vec<PendingOrderRecord>> {
        let normalized_entry_mode = normalize_entry_mode(entry_mode);
        let normalized_strategy_id = strategy_id.trim().to_ascii_lowercase();
        let normalized_condition_id = condition_id.trim();
        if normalized_condition_id.is_empty() {
            return Ok(Vec::new());
        }
        self.with_conn_read_profiled(profile_scope, Some(256), |conn| {
            let query = format!(
                r#"
SELECT order_id, trade_key, token_id, condition_id, timeframe, strategy_id, asset_symbol, entry_mode, period_timestamp, price, size_usd, side, status
FROM pending_orders
WHERE timeframe=?1
  AND period_timestamp=?2
  AND entry_mode=?3
  AND strategy_id=?4
  AND condition_id=?5
  AND status IN ({})
ORDER BY updated_at_ms DESC
"#,
                ACTIVE_PENDING_ORDER_STATUSES_SQL
            );
            let mut stmt = conn.prepare(query.as_str())?;
            let rows = stmt.query_map(
                params![
                    timeframe,
                    period_timestamp as i64,
                    normalized_entry_mode,
                    normalized_strategy_id,
                    normalized_condition_id
                ],
                |row| {
                    let period_ts: i64 = row.get(8)?;
                    Ok(PendingOrderRecord {
                        order_id: row.get(0)?,
                        trade_key: row.get(1)?,
                        token_id: row.get(2)?,
                        condition_id: row.get(3)?,
                        timeframe: row.get(4)?,
                        strategy_id: row.get(5)?,
                        asset_symbol: row.get(6)?,
                        entry_mode: row.get(7)?,
                        period_timestamp: period_ts.max(0) as u64,
                        price: row.get(9)?,
                        size_usd: row.get(10)?,
                        side: row.get(11)?,
                        status: row.get(12)?,
                    })
                },
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn update_pending_order_status_batch(
        &self,
        order_ids: &[String],
        status: &str,
    ) -> Result<u64> {
        if order_ids.is_empty() {
            return Ok(0);
        }
        let normalized_status = status.trim().to_ascii_uppercase();
        self.with_conn_mut_profiled(
            "pending_orders.status_batch",
            Some(order_ids.len()),
            |conn| {
                let tx = conn.transaction()?;
                let query = format!(
                    r#"
UPDATE pending_orders
SET status=?1,
    updated_at_ms=?2,
    filled_at_ms=CASE
        WHEN ?1='FILLED' AND filled_at_ms IS NULL THEN ?2
        ELSE filled_at_ms
    END
WHERE order_id=?3
  AND status IN ({})
"#,
                    ACTIVE_PENDING_ORDER_STATUSES_SQL
                );
                let mut stmt = tx.prepare(query.as_str())?;
                let now_ms = chrono::Utc::now().timestamp_millis();
                let mut changed = 0_u64;
                for order_id in order_ids {
                    let rows = stmt.execute(params![normalized_status, now_ms, order_id])?;
                    changed = changed.saturating_add(rows as u64);
                }
                drop(stmt);
                tx.commit()?;
                Ok(changed)
            },
        )
    }

    pub fn update_pending_order_status_batch_any(
        &self,
        order_ids: &[String],
        status: &str,
    ) -> Result<u64> {
        if order_ids.is_empty() {
            return Ok(0);
        }
        let normalized_status = status.trim().to_ascii_uppercase();
        self.with_conn_mut_profiled(
            "pending_orders.status_batch_any",
            Some(order_ids.len()),
            |conn| {
                let tx = conn.transaction()?;
                let query = r#"
UPDATE pending_orders
SET status=CASE
        WHEN status='FILLED' AND ?1!='FILLED' THEN status
        ELSE ?1
    END,
    updated_at_ms=?2,
    filled_at_ms=CASE
        WHEN status='FILLED' OR (?1='FILLED' AND filled_at_ms IS NULL) THEN COALESCE(filled_at_ms, ?2)
        ELSE filled_at_ms
    END
WHERE order_id=?3
  AND (
      CASE
          WHEN status='FILLED' AND ?1!='FILLED' THEN status
          ELSE ?1
      END
  ) != status
"#;
                let mut stmt = tx.prepare(query)?;
                let now_ms = chrono::Utc::now().timestamp_millis();
                let mut changed = 0_u64;
                for order_id in order_ids {
                    let rows = stmt.execute(params![normalized_status, now_ms, order_id])?;
                    changed = changed.saturating_add(rows as u64);
                }
                drop(stmt);
                tx.commit()?;
                Ok(changed)
            },
        )
    }

    pub fn list_open_pending_orders_for_strategy_condition(
        &self,
        strategy_id: &str,
        condition_id: &str,
    ) -> Result<Vec<PendingOrderRecord>> {
        let normalized_strategy_id = strategy_id.trim().to_ascii_lowercase();
        let normalized_condition_id = condition_id.trim().to_ascii_lowercase();
        self.with_conn_read(|conn| {
            let query = format!(
                r#"
SELECT order_id, trade_key, token_id, condition_id, timeframe, strategy_id, asset_symbol, entry_mode, period_timestamp, price, size_usd, side, status
FROM pending_orders
WHERE strategy_id=?1
  AND LOWER(TRIM(COALESCE(condition_id, '')))=?2
  AND status IN ({})
ORDER BY updated_at_ms DESC
"#,
                ACTIVE_PENDING_ORDER_STATUSES_SQL
            );
            let mut stmt = conn.prepare(query.as_str())?;
            let rows = stmt.query_map(
                params![normalized_strategy_id, normalized_condition_id],
                |row| {
                    let period_ts: i64 = row.get(8)?;
                    Ok(PendingOrderRecord {
                        order_id: row.get(0)?,
                        trade_key: row.get(1)?,
                        token_id: row.get(2)?,
                        condition_id: row.get(3)?,
                        timeframe: row.get(4)?,
                        strategy_id: row.get(5)?,
                        asset_symbol: row.get(6)?,
                        entry_mode: row.get(7)?,
                        period_timestamp: period_ts.max(0) as u64,
                        price: row.get(9)?,
                        size_usd: row.get(10)?,
                        side: row.get(11)?,
                        status: row.get(12)?,
                    })
                },
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn count_open_pending_orders_for_strategy_condition_side(
        &self,
        strategy_id: &str,
        condition_id: &str,
        side: &str,
    ) -> Result<u64> {
        let strategy_id = normalize_strategy_id(strategy_id);
        let condition_id = condition_id.trim().to_string();
        if condition_id.is_empty() {
            return Ok(0);
        }
        let side = normalize_side(side);
        self.with_conn_read(|conn| {
            let query = format!(
                r#"
SELECT COUNT(1)
FROM pending_orders
WHERE strategy_id=?1
  AND condition_id=?2
  AND side=?3
  AND status IN ({})
"#,
                ACTIVE_PENDING_ORDER_STATUSES_SQL
            );
            let mut stmt = conn.prepare(query.as_str())?;
            let count: i64 =
                stmt.query_row(params![strategy_id, condition_id, side], |row| row.get(0))?;
            Ok(count.max(0) as u64)
        })
    }

    pub fn has_open_pending_order_global(
        &self,
        period_timestamp: u64,
        token_id: &str,
        side: &str,
    ) -> Result<bool> {
        let normalized_side = normalize_side(side);
        self.with_conn_read(|conn| {
            let query = format!(
                r#"
SELECT COUNT(1)
FROM pending_orders
WHERE period_timestamp=?1
  AND token_id=?2
  AND side=?3
  AND status IN ({})
"#,
                ACTIVE_PENDING_ORDER_STATUSES_SQL
            );
            let mut stmt = conn.prepare(query.as_str())?;
            let count: i64 = stmt.query_row(
                params![period_timestamp as i64, token_id, normalized_side],
                |row| row.get(0),
            )?;
            Ok(count > 0)
        })
    }

    pub fn has_open_pending_order_for_trade_key(
        &self,
        trade_key: &str,
        side: &str,
    ) -> Result<bool> {
        let normalized_side = normalize_side(side);
        self.with_conn_read(|conn| {
            let query = format!(
                r#"
SELECT COUNT(1)
FROM pending_orders
WHERE trade_key=?1
  AND side=?2
  AND status IN ({})
"#,
                ACTIVE_PENDING_ORDER_STATUSES_SQL
            );
            let mut stmt = conn.prepare(query.as_str())?;
            let count: i64 =
                stmt.query_row(params![trade_key, normalized_side], |row| row.get(0))?;
            Ok(count > 0)
        })
    }

    pub fn sum_entry_fill_notional_for_request_prefix(
        &self,
        strategy_id: &str,
        timeframe: &str,
        period_timestamp: u64,
        token_id: &str,
        request_id_prefix: &str,
    ) -> Result<f64> {
        let strategy_id = strategy_id.trim().to_ascii_lowercase();
        let timeframe = timeframe.trim().to_ascii_lowercase();
        let request_id_prefix = request_id_prefix.trim().to_string();
        self.with_conn_read(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT COALESCE(SUM(COALESCE(f.notional_usd, 0.0)), 0.0)
FROM fills_v2 f
JOIN trade_events te
  ON te.event_key = f.event_key
WHERE f.strategy_id=?1
  AND f.timeframe=?2
  AND f.period_timestamp=?3
  AND f.token_id=?4
  AND CASE
        WHEN json_valid(te.reason) THEN
          COALESCE(json_extract(te.reason, '$.request_id'), '') LIKE (?5 || '%')
        ELSE 0
      END
"#,
            )?;
            let total: f64 = stmt.query_row(
                params![
                    strategy_id,
                    timeframe,
                    period_timestamp as i64,
                    token_id,
                    request_id_prefix
                ],
                |row| row.get(0),
            )?;
            Ok(total.max(0.0))
        })
    }

    pub fn count_entry_fills_for_strategy_condition(
        &self,
        strategy_id: &str,
        condition_id: &str,
    ) -> Result<u64> {
        let strategy_id = strategy_id.trim().to_ascii_lowercase();
        let condition_id = condition_id.trim().to_string();
        self.with_conn_read(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT COUNT(*)
FROM fills_v2
WHERE strategy_id=?1
  AND condition_id=?2
  AND UPPER(TRIM(COALESCE(side, 'BUY')))='BUY'
"#,
            )?;
            let count: i64 =
                stmt.query_row(params![strategy_id, condition_id], |row| row.get(0))?;
            Ok(count.max(0) as u64)
        })
    }

    pub fn get_entry_fill_avg_price_for_strategy_condition_token_with_last_buy_ts(
        &self,
        strategy_id: &str,
        condition_id: &str,
        token_id: &str,
    ) -> Result<Option<(f64, f64, i64)>> {
        let strategy_id = strategy_id.trim().to_ascii_lowercase();
        let condition_id = condition_id.trim().to_string();
        let token_id = token_id.trim().to_string();
        self.with_conn_read(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT
    COALESCE(SUM(COALESCE(units, 0.0)), 0.0) AS total_units,
    COALESCE(SUM(COALESCE(notional_usd, 0.0)), 0.0) AS total_notional,
    COALESCE(MAX(COALESCE(ts_ms, created_at_ms, 0)), 0) AS last_buy_ts_ms
FROM fills_v2
WHERE strategy_id=?1
  AND condition_id=?2
  AND token_id=?3
  AND UPPER(TRIM(COALESCE(side, 'BUY')))='BUY'
"#,
            )?;
            let (total_units, total_notional, last_buy_ts_ms): (f64, f64, i64) = stmt
                .query_row(params![strategy_id, condition_id, token_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;
            if total_units <= 0.0 || total_notional <= 0.0 {
                return Ok(None);
            }
            Ok(Some((
                total_notional / total_units,
                total_units,
                last_buy_ts_ms.max(0),
            )))
        })
    }

    pub fn get_entry_fill_avg_price_for_strategy_condition_token_since(
        &self,
        strategy_id: &str,
        condition_id: &str,
        token_id: &str,
        since_ts_ms: i64,
    ) -> Result<Option<(f64, f64, i64)>> {
        let strategy_id = strategy_id.trim().to_ascii_lowercase();
        let condition_id = condition_id.trim().to_string();
        let token_id = token_id.trim().to_string();
        let since_ts_ms = since_ts_ms.max(0);
        self.with_conn_read(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT
    COALESCE(SUM(COALESCE(units, 0.0)), 0.0) AS total_units,
    COALESCE(SUM(COALESCE(notional_usd, 0.0)), 0.0) AS total_notional,
    COALESCE(MAX(COALESCE(ts_ms, created_at_ms, 0)), 0) AS last_buy_ts_ms
FROM fills_v2
WHERE strategy_id=?1
  AND condition_id=?2
  AND token_id=?3
  AND UPPER(TRIM(COALESCE(side, 'BUY')))='BUY'
  AND COALESCE(ts_ms, created_at_ms, 0) >= ?4
"#,
            )?;
            let (total_units, total_notional, last_buy_ts_ms): (f64, f64, i64) = stmt.query_row(
                params![strategy_id, condition_id, token_id, since_ts_ms],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            if total_units <= 0.0 || total_notional <= 0.0 {
                return Ok(None);
            }
            Ok(Some((
                total_notional / total_units,
                total_units,
                last_buy_ts_ms.max(0),
            )))
        })
    }

    pub fn get_entry_fill_avg_price_for_strategy_condition_token(
        &self,
        strategy_id: &str,
        condition_id: &str,
        token_id: &str,
    ) -> Result<Option<(f64, f64)>> {
        Ok(self
            .get_entry_fill_avg_price_for_strategy_condition_token_with_last_buy_ts(
                strategy_id,
                condition_id,
                token_id,
            )?
            .map(|(price, units, _)| (price, units)))
    }

    pub fn get_entry_fill_avg_price_for_strategy_scope_condition_token_with_last_buy_ts(
        &self,
        strategy_id: &str,
        timeframe: &str,
        period_timestamp: u64,
        condition_id: &str,
        token_id: &str,
    ) -> Result<Option<(f64, f64, i64)>> {
        let strategy_id = strategy_id.trim().to_ascii_lowercase();
        let timeframe = timeframe.trim().to_ascii_lowercase();
        let condition_id = condition_id.trim().to_string();
        let token_id = token_id.trim().to_string();
        self.with_conn_read(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT
    COALESCE(SUM(COALESCE(units, 0.0)), 0.0) AS total_units,
    COALESCE(SUM(COALESCE(notional_usd, 0.0)), 0.0) AS total_notional,
    COALESCE(MAX(COALESCE(ts_ms, created_at_ms, 0)), 0) AS last_buy_ts_ms
FROM fills_v2
WHERE strategy_id=?1
  AND timeframe=?2
  AND period_timestamp=?3
  AND condition_id=?4
  AND token_id=?5
  AND UPPER(TRIM(COALESCE(side, 'BUY')))='BUY'
"#,
            )?;
            let (total_units, total_notional, last_buy_ts_ms): (f64, f64, i64) = stmt.query_row(
                params![
                    strategy_id,
                    timeframe,
                    period_timestamp as i64,
                    condition_id,
                    token_id
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            if total_units <= 0.0 || total_notional <= 0.0 {
                return Ok(None);
            }
            Ok(Some((
                total_notional / total_units,
                total_units,
                last_buy_ts_ms.max(0),
            )))
        })
    }

    pub fn get_entry_fill_avg_price_for_strategy_scope_condition_token_since(
        &self,
        strategy_id: &str,
        timeframe: &str,
        period_timestamp: u64,
        condition_id: &str,
        token_id: &str,
        since_ts_ms: i64,
    ) -> Result<Option<(f64, f64, i64)>> {
        let strategy_id = strategy_id.trim().to_ascii_lowercase();
        let timeframe = timeframe.trim().to_ascii_lowercase();
        let condition_id = condition_id.trim().to_string();
        let token_id = token_id.trim().to_string();
        let since_ts_ms = since_ts_ms.max(0);
        self.with_conn_read(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT
    COALESCE(SUM(COALESCE(units, 0.0)), 0.0) AS total_units,
    COALESCE(SUM(COALESCE(notional_usd, 0.0)), 0.0) AS total_notional,
    COALESCE(MAX(COALESCE(ts_ms, created_at_ms, 0)), 0) AS last_buy_ts_ms
FROM fills_v2
WHERE strategy_id=?1
  AND timeframe=?2
  AND period_timestamp=?3
  AND condition_id=?4
  AND token_id=?5
  AND UPPER(TRIM(COALESCE(side, 'BUY')))='BUY'
  AND COALESCE(ts_ms, created_at_ms, 0) >= ?6
"#,
            )?;
            let (total_units, total_notional, last_buy_ts_ms): (f64, f64, i64) = stmt.query_row(
                params![
                    strategy_id,
                    timeframe,
                    period_timestamp as i64,
                    condition_id,
                    token_id,
                    since_ts_ms
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            if total_units <= 0.0 || total_notional <= 0.0 {
                return Ok(None);
            }
            Ok(Some((
                total_notional / total_units,
                total_units,
                last_buy_ts_ms.max(0),
            )))
        })
    }

    pub fn get_entry_fill_avg_price_for_strategy_scope_condition_token(
        &self,
        strategy_id: &str,
        timeframe: &str,
        period_timestamp: u64,
        condition_id: &str,
        token_id: &str,
    ) -> Result<Option<(f64, f64)>> {
        Ok(self
            .get_entry_fill_avg_price_for_strategy_scope_condition_token_with_last_buy_ts(
                strategy_id,
                timeframe,
                period_timestamp,
                condition_id,
                token_id,
            )?
            .map(|(price, units, _)| (price, units)))
    }

    pub fn count_entry_fills_for_strategy_token_period(
        &self,
        strategy_id: &str,
        timeframe: &str,
        period_timestamp: u64,
        token_id: &str,
    ) -> Result<u64> {
        let strategy_id = strategy_id.trim().to_ascii_lowercase();
        let timeframe = timeframe.trim().to_ascii_lowercase();
        let token_id = token_id.trim().to_string();
        self.with_conn_read(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT COUNT(*)
FROM fills_v2
WHERE strategy_id=?1
  AND timeframe=?2
  AND period_timestamp=?3
  AND token_id=?4
  AND UPPER(TRIM(COALESCE(side, 'BUY')))='BUY'
"#,
            )?;
            let count: i64 = stmt.query_row(
                params![strategy_id, timeframe, period_timestamp as i64, token_id],
                |row| row.get(0),
            )?;
            Ok(count.max(0) as u64)
        })
    }

    pub fn count_entry_fills_for_strategy_symbol_period(
        &self,
        strategy_id: &str,
        timeframe: &str,
        period_timestamp: u64,
        asset_symbol: &str,
    ) -> Result<u64> {
        let strategy_id = strategy_id.trim().to_ascii_lowercase();
        let timeframe = timeframe.trim().to_ascii_lowercase();
        let asset_symbol = asset_symbol.trim().to_ascii_uppercase();
        self.with_conn_read(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT COUNT(*)
FROM fills_v2 f
JOIN trade_events te
  ON te.event_key = f.event_key
WHERE f.strategy_id=?1
  AND f.timeframe=?2
  AND f.period_timestamp=?3
  AND UPPER(COALESCE(te.asset_symbol, ''))=?4
  AND UPPER(TRIM(COALESCE(f.side, 'BUY')))='BUY'
"#,
            )?;
            let count: i64 = stmt.query_row(
                params![
                    strategy_id,
                    timeframe,
                    period_timestamp as i64,
                    asset_symbol
                ],
                |row| row.get(0),
            )?;
            Ok(count.max(0) as u64)
        })
    }

    pub fn sum_entry_fill_notional_for_trade_key_like(
        &self,
        strategy_id: &str,
        timeframe: &str,
        period_timestamp: u64,
        token_id: &str,
        trade_key_like: &str,
    ) -> Result<f64> {
        let strategy_id = strategy_id.trim().to_ascii_lowercase();
        let timeframe = timeframe.trim().to_ascii_lowercase();
        let trade_key_like = trade_key_like.trim().to_string();
        self.with_conn_read(|conn| {
            let mut stmt = conn.prepare(
                r#"
WITH scoped AS (
    SELECT
        id,
        event_key,
        reason,
        COALESCE(notional_usd, 0.0) AS notional_usd,
        CASE
            WHEN json_valid(reason)
                AND COALESCE(NULLIF(TRIM(COALESCE(json_extract(reason, '$.order_id'), '')), ''), '') != ''
                THEN LOWER(TRIM(COALESCE(json_extract(reason, '$.order_id'), '')))
            WHEN event_key LIKE 'entry_fill_order:%'
                THEN LOWER(TRIM(SUBSTR(event_key, LENGTH('entry_fill_order:') + 1)))
            WHEN event_key LIKE 'entry_fill_reconciled:%'
                THEN LOWER(TRIM(SUBSTR(event_key, LENGTH('entry_fill_reconciled:') + 1)))
            ELSE ''
        END AS normalized_order_id
    FROM trade_events
    WHERE strategy_id=?1
      AND timeframe=?2
      AND period_timestamp=?3
      AND token_id=?4
      AND event_type IN ('ENTRY_FILL','ENTRY_FILL_RECONCILED')
      AND CASE
            WHEN json_valid(reason) THEN
              COALESCE(json_extract(reason, '$.trade_key'), '') LIKE ?5
            ELSE 0
          END
)
SELECT COALESCE(SUM(d.notional_usd), 0.0)
FROM (
    SELECT
        CASE
            WHEN s.normalized_order_id != '' THEN ('order:' || s.normalized_order_id)
            WHEN s.event_key IS NOT NULL AND TRIM(s.event_key) != '' THEN ('event:' || TRIM(s.event_key))
            ELSE ('row:' || CAST(s.id AS TEXT))
        END AS dedupe_key,
        MAX(s.notional_usd) AS notional_usd
    FROM scoped s
    GROUP BY dedupe_key
) d
"#,
            )?;
            let total: f64 = stmt.query_row(
                params![
                    strategy_id,
                    timeframe,
                    period_timestamp as i64,
                    token_id,
                    trade_key_like
                ],
                |row| row.get(0),
            )?;
            Ok(total.max(0.0))
        })
    }

    pub fn list_active_pending_orders(&self) -> Result<Vec<PendingOrderRecord>> {
        self.with_conn_read(|conn| {
            let query = format!(
                r#"
SELECT order_id, trade_key, token_id, condition_id, timeframe, strategy_id, asset_symbol, entry_mode, period_timestamp, price, size_usd, side, status
FROM pending_orders
WHERE status IN ({})
ORDER BY updated_at_ms DESC
"#,
                ACTIVE_PENDING_ORDER_STATUSES_SQL
            );
            let mut stmt = conn.prepare(query.as_str())?;
            let rows = stmt.query_map([], |row| {
                let period_ts: i64 = row.get(8)?;
                Ok(PendingOrderRecord {
                    order_id: row.get(0)?,
                    trade_key: row.get(1)?,
                    token_id: row.get(2)?,
                    condition_id: row.get(3)?,
                    timeframe: row.get(4)?,
                    strategy_id: row.get(5)?,
                    asset_symbol: row.get(6)?,
                    entry_mode: row.get(7)?,
                    period_timestamp: period_ts.max(0) as u64,
                    price: row.get(9)?,
                    size_usd: row.get(10)?,
                    side: row.get(11)?,
                    status: row.get(12)?,
                })
            })?;

            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn list_reconcile_pending_orders(&self) -> Result<Vec<PendingOrderRecord>> {
        self.with_conn_read(|conn| {
            let query = format!(
                r#"
SELECT order_id, trade_key, token_id, condition_id, timeframe, strategy_id, asset_symbol, entry_mode, period_timestamp, price, size_usd, side, status
FROM pending_orders
WHERE status IN ({}, 'STALE', 'RECONCILE_FAILED')
ORDER BY updated_at_ms DESC
"#,
                ACTIVE_PENDING_ORDER_STATUSES_SQL
            );
            let mut stmt = conn.prepare(query.as_str())?;
            let rows = stmt.query_map([], |row| {
                let period_ts: i64 = row.get(8)?;
                Ok(PendingOrderRecord {
                    order_id: row.get(0)?,
                    trade_key: row.get(1)?,
                    token_id: row.get(2)?,
                    condition_id: row.get(3)?,
                    timeframe: row.get(4)?,
                    strategy_id: row.get(5)?,
                    asset_symbol: row.get(6)?,
                    entry_mode: row.get(7)?,
                    period_timestamp: period_ts.max(0) as u64,
                    price: row.get(9)?,
                    size_usd: row.get(10)?,
                    side: row.get(11)?,
                    status: row.get(12)?,
                })
            })?;

            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn list_latest_entry_fills(&self, limit: usize) -> Result<Vec<LatestEntryFillRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT
    t.period_timestamp,
    t.timeframe,
    t.strategy_id,
    t.asset_symbol,
    t.condition_id,
    t.token_id,
    t.token_type,
    t.price,
    t.notional_usd
FROM trade_events t
JOIN (
    SELECT condition_id, token_id, MAX(id) AS max_id
    FROM trade_events
    WHERE event_type = 'ENTRY_FILL'
      AND condition_id IS NOT NULL
      AND token_id IS NOT NULL
    GROUP BY condition_id, token_id
) latest ON latest.max_id = t.id
WHERE t.event_type = 'ENTRY_FILL'
ORDER BY t.ts_ms DESC
LIMIT ?1
"#,
            )?;

            let rows = stmt.query_map(params![limit as i64], |row| {
                let period_ts: i64 = row.get(0)?;
                Ok(LatestEntryFillRecord {
                    period_timestamp: period_ts.max(0) as u64,
                    timeframe: row.get(1)?,
                    strategy_id: normalize_strategy_id(row.get::<_, String>(2)?.as_str()),
                    asset_symbol: row.get(3)?,
                    condition_id: row.get(4)?,
                    token_id: row.get(5)?,
                    token_type: row.get(6)?,
                    price: row.get(7)?,
                    notional_usd: row.get(8)?,
                })
            })?;

            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn list_entry_fill_events_since(
        &self,
        strategy_id: &str,
        since_ts_ms: i64,
        limit: usize,
    ) -> Result<Vec<EntryFillEventRecord>> {
        let normalized_strategy = normalize_strategy_id(strategy_id);
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT
    id,
    ts_ms,
    COALESCE(event_key, printf('legacy_entry_fill:%d:%d', id, ts_ms)),
    period_timestamp,
    LOWER(TRIM(timeframe)),
    COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default'),
    condition_id,
    token_id,
    UPPER(COALESCE(NULLIF(TRIM(side), ''), 'BUY')),
    price,
    notional_usd
FROM trade_events
WHERE event_type='ENTRY_FILL'
  AND ts_ms >= ?1
  AND COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default') = ?2
  AND condition_id IS NOT NULL
  AND token_id IS NOT NULL
ORDER BY ts_ms ASC, id ASC
LIMIT ?3
"#,
            )?;
            let rows = stmt.query_map(
                params![
                    since_ts_ms,
                    normalized_strategy,
                    i64::try_from(limit.clamp(1, 10_000)).ok().unwrap_or(1_000),
                ],
                |row| {
                    let period_ts: i64 = row.get(3)?;
                    Ok(EntryFillEventRecord {
                        event_key: row.get(2)?,
                        ts_ms: row.get(1)?,
                        period_timestamp: period_ts.max(0) as u64,
                        timeframe: row.get(4)?,
                        strategy_id: normalize_strategy_id(row.get::<_, String>(5)?.as_str()),
                        condition_id: row.get(6)?,
                        token_id: row.get(7)?,
                        side: row.get(8)?,
                        price: row.get(9)?,
                        notional_usd: row.get(10)?,
                    })
                },
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn list_strategy_fill_events_since(
        &self,
        strategy_id: &str,
        since_ts_ms: i64,
        limit: usize,
    ) -> Result<Vec<StrategyFillEventRecord>> {
        let normalized_strategy = normalize_strategy_id(strategy_id);
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT
    id,
    ts_ms,
    COALESCE(event_key, printf('legacy_strategy_fill:%d:%d', id, ts_ms)),
    COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default'),
    condition_id,
    token_id,
    UPPER(COALESCE(NULLIF(TRIM(side), ''), 'BUY')),
    UPPER(COALESCE(NULLIF(TRIM(event_type), ''), 'ENTRY_FILL'))
FROM trade_events
WHERE ts_ms >= ?1
  AND COALESCE(NULLIF(TRIM(strategy_id), ''), 'legacy_default') = ?2
  AND event_type IN ('ENTRY_FILL', 'EXIT')
  AND condition_id IS NOT NULL
  AND token_id IS NOT NULL
ORDER BY ts_ms ASC, id ASC
LIMIT ?3
"#,
            )?;
            let rows = stmt.query_map(
                params![
                    since_ts_ms,
                    normalized_strategy,
                    i64::try_from(limit.clamp(1, 10_000)).ok().unwrap_or(1_000),
                ],
                |row| {
                    Ok(StrategyFillEventRecord {
                        event_key: row.get(2)?,
                        ts_ms: row.get(1)?,
                        strategy_id: normalize_strategy_id(row.get::<_, String>(3)?.as_str()),
                        condition_id: row.get(4)?,
                        token_id: row.get(5)?,
                        side: row.get(6)?,
                        event_type: row.get(7)?,
                    })
                },
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn list_open_positions_for_restore(
        &self,
        limit: usize,
    ) -> Result<Vec<LatestEntryFillRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
WITH latest_order_meta AS (
    SELECT condition_id, token_id, MAX(updated_at_ms) AS max_updated_at_ms
    FROM pending_orders
    WHERE condition_id IS NOT NULL
      AND token_id IS NOT NULL
    GROUP BY condition_id, token_id
),
order_meta AS (
    SELECT p.condition_id, p.token_id, p.asset_symbol
    FROM pending_orders p
    JOIN latest_order_meta lom
      ON lom.condition_id = p.condition_id
     AND lom.token_id = p.token_id
     AND lom.max_updated_at_ms = p.updated_at_ms
)
SELECT
    pos.period_timestamp,
    pos.timeframe,
    pos.strategy_id,
    om.asset_symbol,
    pos.condition_id,
    pos.token_id,
    pos.token_type,
    CASE
      WHEN COALESCE(pos.entry_units, 0.0) > 0.0
      THEN pos.entry_notional_usd / pos.entry_units
      ELSE NULL
    END AS entry_price,
    pos.entry_notional_usd
FROM positions_v2 pos
LEFT JOIN order_meta om
  ON om.condition_id = pos.condition_id
 AND om.token_id = pos.token_id
WHERE pos.status = 'OPEN'
  AND COALESCE(pos.entry_units, 0.0) >
      (COALESCE(pos.exit_units, 0.0) + COALESCE(pos.inventory_consumed_units, 0.0))
  AND pos.condition_id IS NOT NULL
  AND pos.token_id IS NOT NULL
ORDER BY pos.opened_at_ms ASC
LIMIT ?1
"#,
            )?;

            let rows = stmt.query_map(params![limit as i64], |row| {
                let period_ts: i64 = row.get(0)?;
                Ok(LatestEntryFillRecord {
                    period_timestamp: period_ts.max(0) as u64,
                    timeframe: row.get(1)?,
                    strategy_id: normalize_strategy_id(row.get::<_, String>(2)?.as_str()),
                    asset_symbol: row.get(3)?,
                    condition_id: row.get(4)?,
                    token_id: row.get(5)?,
                    token_type: row.get(6)?,
                    price: row.get(7)?,
                    notional_usd: row.get(8)?,
                })
            })?;

            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn list_open_position_inventory_by_strategy(
        &self,
        strategy_id: &str,
        limit: usize,
    ) -> Result<Vec<OpenPositionInventoryRow>> {
        let strategy_id = normalize_strategy_id(strategy_id);
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT
    strategy_id,
    condition_id,
    token_id,
    LOWER(TRIM(timeframe)) AS timeframe,
    MAX(period_timestamp) AS period_timestamp,
    SUM(COALESCE(entry_units, 0.0)) AS entry_units,
    SUM(COALESCE(exit_units, 0.0)) AS exit_units,
    SUM(
        COALESCE(entry_units, 0.0)
        - COALESCE(exit_units, 0.0)
        - COALESCE(inventory_consumed_units, 0.0)
    ) AS net_units
FROM positions_v2
WHERE strategy_id=?1
  AND status='OPEN'
  AND condition_id IS NOT NULL
  AND token_id IS NOT NULL
GROUP BY strategy_id, condition_id, token_id, LOWER(TRIM(timeframe))
HAVING SUM(
    COALESCE(entry_units, 0.0)
    - COALESCE(exit_units, 0.0)
    - COALESCE(inventory_consumed_units, 0.0)
) > 1e-9
ORDER BY ABS(net_units) DESC, condition_id ASC, token_id ASC
LIMIT ?2
"#,
            )?;
            let rows = stmt.query_map(
                params![
                    strategy_id,
                    i64::try_from(limit.clamp(1, 10_000)).ok().unwrap_or(1_000),
                ],
                |row| {
                    let period_ts: i64 = row.get(4)?;
                    Ok(OpenPositionInventoryRow {
                        strategy_id: normalize_strategy_id(row.get::<_, String>(0)?.as_str()),
                        condition_id: row.get(1)?,
                        token_id: row.get(2)?,
                        timeframe: row.get(3)?,
                        period_timestamp: period_ts.max(0) as u64,
                        entry_units: row.get(5)?,
                        exit_units: row.get(6)?,
                        net_units: row.get(7)?,
                    })
                },
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn list_open_position_strategies_for_condition_token(
        &self,
        condition_id: &str,
        token_id: &str,
        limit: usize,
    ) -> Result<Vec<String>> {
        let condition_id = condition_id.trim().to_string();
        let token_id = token_id.trim().to_string();
        if condition_id.is_empty() || token_id.is_empty() {
            return Ok(Vec::new());
        }
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT DISTINCT strategy_id
FROM positions_v2
WHERE condition_id=?1
  AND token_id=?2
  AND status='OPEN'
  AND COALESCE(entry_units, 0.0) >
      (COALESCE(exit_units, 0.0) + COALESCE(inventory_consumed_units, 0.0))
ORDER BY strategy_id ASC
LIMIT ?3
"#,
            )?;
            let rows = stmt.query_map(
                params![
                    condition_id,
                    token_id,
                    i64::try_from(limit.clamp(1, 1_000)).ok().unwrap_or(32),
                ],
                |row| Ok(normalize_strategy_id(row.get::<_, String>(0)?.as_str())),
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn list_redemption_candidates_for_restore(
        &self,
        limit: usize,
    ) -> Result<Vec<LatestEntryFillRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
WITH latest_event_meta AS (
    SELECT
        token_id,
        condition_id,
        MAX(id) AS max_id
    FROM trade_events
    WHERE token_id IS NOT NULL
      AND condition_id IS NOT NULL
    GROUP BY token_id, condition_id
)
SELECT
    l.period_timestamp,
    l.timeframe,
    l.strategy_id,
    te.asset_symbol,
    l.condition_id,
    l.token_id,
    COALESCE(NULLIF(TRIM(l.token_type), ''), te.token_type) AS token_type,
    CASE
      WHEN COALESCE(l.entry_units, 0.0) > 0.0
      THEN l.entry_notional_usd / l.entry_units
      ELSE NULL
    END AS entry_price,
    l.entry_notional_usd
FROM trade_lifecycle_v1 l
LEFT JOIN latest_event_meta lem
  ON lem.token_id = l.token_id
 AND lem.condition_id = l.condition_id
LEFT JOIN trade_events te
  ON te.id = lem.max_id
WHERE l.redemption_status IN ('pending','retrying','queued','paused')
  AND COALESCE(l.resolved_payout_usd, 0.0) > 0.0
  AND l.condition_id IS NOT NULL
  AND TRIM(l.condition_id) != ''
  AND l.token_id IS NOT NULL
  AND TRIM(l.token_id) != ''
ORDER BY COALESCE(l.resolved_ts_ms, l.period_timestamp * 1000) ASC, l.updated_at_ms ASC
LIMIT ?1
"#,
            )?;

            let rows = stmt.query_map(params![limit as i64], |row| {
                let period_ts: i64 = row.get(0)?;
                Ok(LatestEntryFillRecord {
                    period_timestamp: period_ts.max(0) as u64,
                    timeframe: row.get(1)?,
                    strategy_id: normalize_strategy_id(row.get::<_, String>(2)?.as_str()),
                    asset_symbol: row.get(3)?,
                    condition_id: row.get(4)?,
                    token_id: row.get(5)?,
                    token_type: row.get(6)?,
                    price: row.get(7)?,
                    notional_usd: row.get(8)?,
                })
            })?;

            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn upsert_evcurve_period_base(
        &self,
        symbol: &str,
        timeframe: &str,
        period_open_ts: i64,
        base_mid: f64,
        first_seen_ts_ms: i64,
    ) -> Result<()> {
        if symbol.trim().is_empty()
            || timeframe.trim().is_empty()
            || period_open_ts <= 0
            || !base_mid.is_finite()
            || base_mid <= 0.0
        {
            return Ok(());
        }
        let symbol_norm = symbol.trim().to_ascii_uppercase();
        let timeframe_norm = timeframe.trim().to_ascii_lowercase();
        self.with_conn(|conn| {
            conn.execute(
                r#"
INSERT OR IGNORE INTO evcurve_period_bases_v1 (
    symbol, timeframe, period_open_ts, base_mid, first_seen_ts_ms
)
VALUES (?1, ?2, ?3, ?4, ?5)
"#,
                params![
                    symbol_norm.as_str(),
                    timeframe_norm.as_str(),
                    period_open_ts,
                    base_mid,
                    first_seen_ts_ms
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_evcurve_period_base(
        &self,
        symbol: &str,
        timeframe: &str,
        period_open_ts: i64,
    ) -> Result<Option<f64>> {
        if symbol.trim().is_empty() || timeframe.trim().is_empty() || period_open_ts <= 0 {
            return Ok(None);
        }
        let symbol_norm = symbol.trim().to_ascii_uppercase();
        let timeframe_norm = timeframe.trim().to_ascii_lowercase();
        self.with_conn(|conn| {
            let value = conn
                .query_row(
                    r#"
SELECT base_mid
FROM evcurve_period_bases_v1
WHERE symbol = ?1
  AND timeframe = ?2
  AND period_open_ts = ?3
LIMIT 1
"#,
                    params![
                        symbol_norm.as_str(),
                        timeframe_norm.as_str(),
                        period_open_ts
                    ],
                    |row| row.get::<_, f64>(0),
                )
                .optional()?;
            Ok(value.filter(|v| v.is_finite() && *v > 0.0))
        })
    }

    pub fn get_evcurve_period_base_with_first_seen(
        &self,
        symbol: &str,
        timeframe: &str,
        period_open_ts: i64,
    ) -> Result<Option<(f64, i64)>> {
        if symbol.trim().is_empty() || timeframe.trim().is_empty() || period_open_ts <= 0 {
            return Ok(None);
        }
        let symbol_norm = symbol.trim().to_ascii_uppercase();
        let timeframe_norm = timeframe.trim().to_ascii_lowercase();
        self.with_conn(|conn| {
            let value = conn
                .query_row(
                    r#"
SELECT base_mid, first_seen_ts_ms
FROM evcurve_period_bases_v1
WHERE symbol = ?1
  AND timeframe = ?2
  AND period_open_ts = ?3
LIMIT 1
"#,
                    params![
                        symbol_norm.as_str(),
                        timeframe_norm.as_str(),
                        period_open_ts
                    ],
                    |row| {
                        let base_mid: f64 = row.get(0)?;
                        let first_seen_ts_ms: i64 = row.get(1)?;
                        Ok((base_mid, first_seen_ts_ms))
                    },
                )
                .optional()?;
            Ok(value.filter(|(v, _)| v.is_finite() && *v > 0.0))
        })
    }

    pub fn set_evcurve_period_base(
        &self,
        symbol: &str,
        timeframe: &str,
        period_open_ts: i64,
        base_mid: f64,
        first_seen_ts_ms: i64,
    ) -> Result<()> {
        if symbol.trim().is_empty()
            || timeframe.trim().is_empty()
            || period_open_ts <= 0
            || !base_mid.is_finite()
            || base_mid <= 0.0
        {
            return Ok(());
        }
        let symbol_norm = symbol.trim().to_ascii_uppercase();
        let timeframe_norm = timeframe.trim().to_ascii_lowercase();
        self.with_conn(|conn| {
            conn.execute(
                r#"
INSERT INTO evcurve_period_bases_v1 (
    symbol, timeframe, period_open_ts, base_mid, first_seen_ts_ms
)
VALUES (?1, ?2, ?3, ?4, ?5)
ON CONFLICT(symbol, timeframe, period_open_ts) DO UPDATE SET
    base_mid = excluded.base_mid,
    first_seen_ts_ms = excluded.first_seen_ts_ms
"#,
                params![
                    symbol_norm.as_str(),
                    timeframe_norm.as_str(),
                    period_open_ts,
                    base_mid,
                    first_seen_ts_ms
                ],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal_state::SignalState;
    use crate::strategy::{
        fallback_strategy, DecisionTelemetry, STRATEGY_ID_ENDGAME_SWEEP_V1, STRATEGY_ID_EVCURVE_V1,
        STRATEGY_ID_MM_REWARDS_V1, STRATEGY_ID_PREMARKET_V1,
    };
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_db_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("evpoly_tracking_{name}_{nonce}.db"))
    }

    fn cleanup_db(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
    }

    fn ensure_wallet_sidecar_tables(db: &TrackingDb) -> Result<()> {
        db.with_conn(|conn| {
            conn.execute_batch(
                r#"
CREATE TABLE IF NOT EXISTS wallet_positions_live_latest_v1 (
    wallet_address TEXT NOT NULL,
    condition_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    snapshot_ts_ms INTEGER NOT NULL,
    outcome TEXT,
    opposite_outcome TEXT,
    slug TEXT,
    event_slug TEXT,
    title TEXT,
    token_type TEXT,
    position_size REAL,
    avg_price REAL,
    cur_price REAL,
    initial_value REAL,
    current_value REAL,
    cash_pnl REAL,
    realized_pnl REAL,
    redeemable INTEGER,
    mergeable INTEGER,
    negative_risk INTEGER,
    end_date TEXT,
    raw_json TEXT,
    PRIMARY KEY (wallet_address, condition_id, token_id)
);
CREATE TABLE IF NOT EXISTS wallet_activity_v1 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    wallet_address TEXT NOT NULL,
    activity_key TEXT NOT NULL UNIQUE,
    ts_ms INTEGER NOT NULL,
    activity_type TEXT,
    condition_id TEXT,
    token_id TEXT,
    outcome TEXT,
    side TEXT,
    size REAL,
    usdc_size REAL,
    inventory_consumed_units REAL,
    price REAL,
    transaction_hash TEXT,
    slug TEXT,
    title TEXT,
    raw_json TEXT,
    synced_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS positions_unrealized_mid_latest_v1 (
    position_key TEXT PRIMARY KEY,
    snapshot_ts_ms INTEGER NOT NULL,
    strategy_id TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    period_timestamp INTEGER NOT NULL,
    condition_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    token_type TEXT,
    remaining_units REAL NOT NULL,
    remaining_cost_usd REAL NOT NULL,
    entry_avg_price REAL,
    best_bid REAL,
    best_ask REAL,
    mid_price REAL,
    unrealized_mid_pnl_usd REAL,
    mark_source TEXT NOT NULL
);
"#,
            )?;
            Ok(())
        })
    }

    #[test]
    fn strategy_context_uniqueness_scopes_by_strategy_id() -> Result<()> {
        let path = temp_db_path("context_unique");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_000_000_u64;
        let mut reactive = fallback_strategy(
            Timeframe::M5,
            period as i64,
            &SignalState::default(),
            "test",
        );
        reactive.strategy_id = STRATEGY_ID_TEST_PRIMARY.to_string();
        reactive.generated_at_ms = 1_000;
        reactive.source = "unit_test".to_string();
        reactive.mode = Some("auto".to_string());
        reactive.telemetry = Some(DecisionTelemetry::default());

        db.record_strategy_decision_context(period, &reactive)?;
        let mut reactive_newer = reactive.clone();
        reactive_newer.generated_at_ms = 2_000;
        db.record_strategy_decision_context(period, &reactive_newer)?;

        let mut premarket = reactive.clone();
        premarket.strategy_id = STRATEGY_ID_PREMARKET_V1.to_string();
        premarket.generated_at_ms = 1_500;
        db.record_strategy_decision_context(period, &premarket)?;

        let (row_count, reactive_ts): (i64, i64) = db.with_conn(|conn| {
            let row_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM strategy_decision_contexts WHERE period_timestamp=?1",
                params![period as i64],
                |row| row.get(0),
            )?;
            let reactive_ts: i64 = conn.query_row(
                "SELECT decided_at_ms FROM strategy_decision_contexts WHERE period_timestamp=?1 AND strategy_id=?2",
                params![period as i64, STRATEGY_ID_TEST_PRIMARY],
                |row| row.get(0),
            )?;
            Ok((row_count, reactive_ts))
        })?;
        assert_eq!(row_count, 2);
        assert_eq!(reactive_ts, 2_000);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn strategy_feature_snapshot_schema_bootstraps() -> Result<()> {
        let path = temp_db_path("strategy_feature_snapshot_schema");
        let db = TrackingDb::new(&path)?;

        let checks: (i64, i64, i64) = db.with_conn(|conn| {
            let table_exists: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='strategy_feature_snapshots_v1'",
                [],
                |row| row.get(0),
            )?;
            let snapshot_idx: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='uidx_strategy_feature_snapshots_snapshot_id'",
                [],
                |row| row.get(0),
            )?;
            let logical_idx: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='uidx_strategy_feature_snapshots_logical_scope'",
                [],
                |row| row.get(0),
            )?;
            Ok((table_exists, snapshot_idx, logical_idx))
        })?;

        assert_eq!(checks.0, 1);
        assert_eq!(checks.1, 1);
        assert_eq!(checks.2, 1);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn strategy_feature_snapshot_null_safe_uniqueness_collapses_duplicates() -> Result<()> {
        let path = temp_db_path("strategy_feature_snapshot_unique");
        let db = TrackingDb::new(&path)?;

        let row = StrategyFeatureSnapshotIntentRecord {
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            timeframe: "5m".to_string(),
            period_timestamp: 1_700_111_111_u64,
            market_key: Some("btc".to_string()),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-unique-1".to_string()),
            token_id: "token-unique-1".to_string(),
            impulse_id: "reactive:decision:abc".to_string(),
            decision_id: None,
            rung_id: None,
            slice_id: None,
            request_id: Some("req:1".to_string()),
            order_id: None,
            trade_key: None,
            position_key: None,
            intent_status: Some("planned".to_string()),
            execution_status: Some("none".to_string()),
            intent_ts_ms: 10_000,
            submit_ts_ms: None,
            ack_ts_ms: None,
            fill_ts_ms: None,
            exit_ts_ms: None,
            settled_ts_ms: None,
            entry_notional_usd: Some(25.0),
            exit_notional_usd: None,
            realized_pnl_usd: None,
            settled_pnl_usd: None,
            hold_ms: None,
            feature_json: Some("{\"score\":1.0}".to_string()),
            decision_context_json: None,
            execution_context_json: None,
        };
        let snapshot_id_1 = db.upsert_strategy_feature_snapshot_intent(&row)?;

        let mut duplicate = row.clone();
        duplicate.decision_id = Some("".to_string());
        duplicate.rung_id = Some("".to_string());
        duplicate.slice_id = Some("".to_string());
        duplicate.request_id = None;
        duplicate.feature_json = Some("{\"score\":1.1}".to_string());
        let snapshot_id_2 = db.upsert_strategy_feature_snapshot_intent(&duplicate)?;

        assert_eq!(snapshot_id_1, snapshot_id_2);
        let stats: (i64, String, String, String) = db.with_conn(|conn| {
            conn.query_row(
                r#"
SELECT
    COUNT(*),
    COALESCE(snapshot_id, ''),
    COALESCE(request_id, ''),
    COALESCE(feature_json, '')
FROM strategy_feature_snapshots_v1
WHERE strategy_id=?1 AND timeframe='5m' AND token_id='token-unique-1'
"#,
                params![STRATEGY_ID_TEST_PRIMARY],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(Into::into)
        })?;
        assert_eq!(stats.0, 1);
        assert_eq!(stats.1, snapshot_id_1);
        assert_eq!(stats.2, "req:1");
        assert_eq!(stats.3, "{\"score\":1.1}");

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn strategy_feature_snapshot_lifecycle_valid_transitions_apply() -> Result<()> {
        let path = temp_db_path("strategy_feature_snapshot_lifecycle_valid");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_111_222_u64;

        let mut row = StrategyFeatureSnapshotIntentRecord {
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            timeframe: "15m".to_string(),
            period_timestamp: period,
            market_key: Some("btc".to_string()),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-lifecycle-valid".to_string()),
            token_id: "token-lifecycle-valid".to_string(),
            impulse_id: "reactive:decision:lifecycle-valid".to_string(),
            decision_id: Some("decision-valid-1".to_string()),
            rung_id: None,
            slice_id: None,
            request_id: None,
            order_id: None,
            trade_key: None,
            position_key: None,
            intent_status: Some("planned".to_string()),
            execution_status: Some("none".to_string()),
            intent_ts_ms: 20_000,
            submit_ts_ms: None,
            ack_ts_ms: None,
            fill_ts_ms: None,
            exit_ts_ms: None,
            settled_ts_ms: None,
            entry_notional_usd: Some(30.0),
            exit_notional_usd: None,
            realized_pnl_usd: None,
            settled_pnl_usd: None,
            hold_ms: None,
            feature_json: None,
            decision_context_json: None,
            execution_context_json: None,
        };
        db.upsert_strategy_feature_snapshot_intent(&row)?;

        row.intent_status = Some("submitted".to_string());
        row.execution_status = Some("ack".to_string());
        row.submit_ts_ms = Some(20_100);
        row.ack_ts_ms = Some(20_120);
        db.upsert_strategy_feature_snapshot_intent(&row)?;

        row.execution_status = Some("filled".to_string());
        row.fill_ts_ms = Some(20_300);
        db.upsert_strategy_feature_snapshot_intent(&row)?;

        row.execution_status = Some("settled".to_string());
        row.settled_ts_ms = Some(25_000);
        row.realized_pnl_usd = Some(8.0);
        row.settled_pnl_usd = Some(8.0);
        db.upsert_strategy_feature_snapshot_intent(&row)?;

        let final_state: (String, String, i64) = db.with_conn(|conn| {
            conn.query_row(
                r#"
SELECT intent_status, execution_status, COALESCE(settled_ts_ms, 0)
FROM strategy_feature_snapshots_v1
WHERE strategy_id=?1 AND timeframe='15m' AND period_timestamp=?2 AND token_id='token-lifecycle-valid'
"#,
                params![STRATEGY_ID_TEST_PRIMARY, period as i64],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(Into::into)
        })?;
        assert_eq!(final_state.0, "submitted");
        assert_eq!(final_state.1, "settled");
        assert_eq!(final_state.2, 25_000);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn strategy_feature_snapshot_submit_ack_timestamps_keep_earliest_values() -> Result<()> {
        let path = temp_db_path("strategy_feature_snapshot_submit_ack_earliest");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_111_223_u64;

        let mut row = StrategyFeatureSnapshotIntentRecord {
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            timeframe: "5m".to_string(),
            period_timestamp: period,
            market_key: Some("btc".to_string()),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-submit-ack-earliest".to_string()),
            token_id: "token-submit-ack-earliest".to_string(),
            impulse_id: "reactive:decision:submit-ack-earliest".to_string(),
            decision_id: Some("decision-submit-ack-earliest".to_string()),
            rung_id: None,
            slice_id: None,
            request_id: Some("req-submit-ack-earliest".to_string()),
            order_id: Some("ord-submit-ack-earliest".to_string()),
            trade_key: Some("trade-submit-ack-earliest".to_string()),
            position_key: None,
            intent_status: Some("submitted".to_string()),
            execution_status: Some("ack".to_string()),
            intent_ts_ms: 50_000,
            submit_ts_ms: Some(50_100),
            ack_ts_ms: Some(50_150),
            fill_ts_ms: None,
            exit_ts_ms: None,
            settled_ts_ms: None,
            entry_notional_usd: Some(22.0),
            exit_notional_usd: None,
            realized_pnl_usd: None,
            settled_pnl_usd: None,
            hold_ms: None,
            feature_json: None,
            decision_context_json: None,
            execution_context_json: None,
        };
        db.upsert_strategy_feature_snapshot_intent(&row)?;

        row.execution_status = Some("filled".to_string());
        row.submit_ts_ms = Some(50_900);
        row.ack_ts_ms = Some(50_950);
        row.fill_ts_ms = Some(51_000);
        db.upsert_strategy_feature_snapshot_intent(&row)?;

        let details: (i64, i64, i64) = db.with_conn(|conn| {
            conn.query_row(
                r#"
SELECT
    COALESCE(submit_ts_ms, 0),
    COALESCE(ack_ts_ms, 0),
    COALESCE(fill_ts_ms, 0)
FROM strategy_feature_snapshots_v1
WHERE strategy_id=?1 AND timeframe='5m' AND period_timestamp=?2 AND token_id='token-submit-ack-earliest'
"#,
                params![STRATEGY_ID_TEST_PRIMARY, period as i64],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(Into::into)
        })?;
        assert_eq!(details.0, 50_100);
        assert_eq!(details.1, 50_150);
        assert_eq!(details.2, 51_000);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn strategy_feature_snapshot_lifecycle_invalid_transition_is_rejected() -> Result<()> {
        let path = temp_db_path("strategy_feature_snapshot_lifecycle_invalid");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_111_333_u64;

        let mut row = StrategyFeatureSnapshotIntentRecord {
            strategy_id: STRATEGY_ID_PREMARKET_V1.to_string(),
            timeframe: "5m".to_string(),
            period_timestamp: period,
            market_key: Some("btc".to_string()),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-lifecycle-invalid".to_string()),
            token_id: "token-lifecycle-invalid".to_string(),
            impulse_id: "premarket:r0".to_string(),
            decision_id: Some("decision-invalid-1".to_string()),
            rung_id: Some("r0".to_string()),
            slice_id: None,
            request_id: None,
            order_id: None,
            trade_key: None,
            position_key: None,
            intent_status: Some("planned".to_string()),
            execution_status: Some("none".to_string()),
            intent_ts_ms: 30_000,
            submit_ts_ms: None,
            ack_ts_ms: None,
            fill_ts_ms: None,
            exit_ts_ms: None,
            settled_ts_ms: None,
            entry_notional_usd: Some(5.0),
            exit_notional_usd: None,
            realized_pnl_usd: None,
            settled_pnl_usd: None,
            hold_ms: None,
            feature_json: None,
            decision_context_json: None,
            execution_context_json: None,
        };
        db.upsert_strategy_feature_snapshot_intent(&row)?;

        row.execution_status = Some("settled".to_string());
        row.settled_ts_ms = Some(31_000);
        let err = db
            .upsert_strategy_feature_snapshot_intent(&row)
            .expect_err("none -> settled transition must be rejected");
        assert!(err
            .to_string()
            .contains("rejected strategy feature snapshot transition"));

        let details: (String, String, i64) = db.with_conn(|conn| {
            let state: (String, String) = conn.query_row(
                r#"
SELECT intent_status, execution_status
FROM strategy_feature_snapshots_v1
WHERE strategy_id=?1 AND timeframe='5m' AND period_timestamp=?2 AND token_id='token-lifecycle-invalid'
"#,
                params![STRATEGY_ID_PREMARKET_V1, period as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let rejections: i64 = conn.query_row(
                "SELECT COUNT(*) FROM strategy_feature_snapshot_transition_rejections WHERE strategy_id=?1 AND period_timestamp=?2",
                params![STRATEGY_ID_PREMARKET_V1, period as i64],
                |row| row.get(0),
            )?;
            Ok((state.0, state.1, rejections))
        })?;
        assert_eq!(details.0, "planned");
        assert_eq!(details.1, "none");
        assert_eq!(details.2, 1);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn strategy_feature_snapshot_stale_replay_downgrade_is_ignored() -> Result<()> {
        let path = temp_db_path("strategy_feature_snapshot_stale_replay");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_111_334_u64;

        let mut row = StrategyFeatureSnapshotIntentRecord {
            strategy_id: STRATEGY_ID_TEST_TERTIARY.to_string(),
            timeframe: "5m".to_string(),
            period_timestamp: period,
            market_key: Some("btc".to_string()),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-stale-replay".to_string()),
            token_id: "token-stale-replay".to_string(),
            impulse_id: "order:stale-replay".to_string(),
            decision_id: Some("decision-stale-replay".to_string()),
            rung_id: None,
            slice_id: None,
            request_id: Some("req-stale-replay".to_string()),
            order_id: Some("ord-stale-replay".to_string()),
            trade_key: None,
            position_key: None,
            intent_status: Some("submitted".to_string()),
            execution_status: Some("filled".to_string()),
            intent_ts_ms: 40_000,
            submit_ts_ms: Some(40_000),
            ack_ts_ms: Some(40_010),
            fill_ts_ms: Some(40_020),
            exit_ts_ms: None,
            settled_ts_ms: None,
            entry_notional_usd: Some(12.0),
            exit_notional_usd: None,
            realized_pnl_usd: None,
            settled_pnl_usd: None,
            hold_ms: None,
            feature_json: None,
            decision_context_json: None,
            execution_context_json: None,
        };
        db.upsert_strategy_feature_snapshot_intent(&row)?;

        // Replay an older ack/submit style event for the same order scope; this should not
        // downgrade execution status, and should not create a rejection row.
        row.execution_status = Some("none".to_string());
        row.fill_ts_ms = None;
        row.ack_ts_ms = None;
        db.upsert_strategy_feature_snapshot_intent(&row)?;

        let details: (String, i64) = db.with_conn(|conn| {
            let execution_status: String = conn.query_row(
                r#"
SELECT execution_status
FROM strategy_feature_snapshots_v1
WHERE strategy_id=?1 AND timeframe='5m' AND period_timestamp=?2 AND token_id='token-stale-replay'
"#,
                params![STRATEGY_ID_TEST_TERTIARY, period as i64],
                |row| row.get(0),
            )?;
            let rejections: i64 = conn.query_row(
                "SELECT COUNT(*) FROM strategy_feature_snapshot_transition_rejections WHERE strategy_id=?1 AND period_timestamp=?2",
                params![STRATEGY_ID_TEST_TERTIARY, period as i64],
                |row| row.get(0),
            )?;
            Ok((execution_status, rejections))
        })?;
        assert_eq!(details.0, "filled");
        assert_eq!(details.1, 0);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn strategy_context_uniqueness_scopes_by_market_key() -> Result<()> {
        let path = temp_db_path("context_market_scope");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_000_050_u64;
        let mut btc = fallback_strategy(
            Timeframe::M5,
            period as i64,
            &SignalState::default(),
            "test-market",
        );
        btc.strategy_id = STRATEGY_ID_TEST_PRIMARY.to_string();
        btc.market_key = "btc".to_string();
        btc.asset_symbol = "BTC".to_string();
        btc.generated_at_ms = 1_000;
        btc.telemetry = Some(DecisionTelemetry::default());

        let mut eth = btc.clone();
        eth.market_key = "eth".to_string();
        eth.asset_symbol = "ETH".to_string();
        eth.generated_at_ms = 1_100;

        db.record_strategy_decision_context(period, &btc)?;
        db.record_strategy_decision_context(period, &eth)?;

        let row_count: i64 = db.with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM strategy_decision_contexts WHERE period_timestamp=?1 AND strategy_id=?2",
                params![period as i64, STRATEGY_ID_TEST_PRIMARY],
                |row| row.get(0),
            )
            .map_err(Into::into)
        })?;
        assert_eq!(row_count, 2);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn pending_orders_and_trade_events_persist_strategy_id() -> Result<()> {
        let path = temp_db_path("strategy_attr");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_000_100_u64;

        db.upsert_pending_order(&PendingOrderRecord {
            order_id: "order-1".to_string(),
            trade_key: "trade-1".to_string(),
            token_id: "token-1".to_string(),
            condition_id: Some("condition-1".to_string()),
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            entry_mode: "ladder".to_string(),
            period_timestamp: period,
            price: 0.41,
            size_usd: 25.0,
            side: "buy".to_string(),
            status: "open".to_string(),
        })?;
        assert!(db.has_open_pending_order_global(period, "token-1", "BUY")?);
        let active = db.list_active_pending_orders()?;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].strategy_id, STRATEGY_ID_TEST_PRIMARY);
        assert_eq!(active[0].asset_symbol.as_deref(), Some("BTC"));

        db.record_trade_event(&TradeEventRecord {
            event_key: Some("event-1".to_string()),
            ts_ms: 3_000,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_PREMARKET_V1.to_uppercase(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-1".to_string()),
            token_id: Some("token-1".to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_SUBMIT".to_string(),
            price: Some(0.41),
            units: Some(60.0),
            notional_usd: Some(24.6),
            pnl_usd: None,
            reason: None,
        })?;
        let stored: (String, String) = db.with_conn(|conn| {
            let value: (String, String) = conn.query_row(
                "SELECT strategy_id, asset_symbol FROM trade_events WHERE event_key='event-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            Ok(value)
        })?;
        assert_eq!(stored.0, STRATEGY_ID_PREMARKET_V1);
        assert_eq!(stored.1, "BTC");

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn pending_order_strategy_scope_isolated_from_legacy_unknown_modes() -> Result<()> {
        let path = temp_db_path("pending_scope_isolated");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_000_101_u64;

        db.upsert_pending_order(&PendingOrderRecord {
            order_id: "order-endgame-unknown".to_string(),
            trade_key: "trade-endgame-unknown".to_string(),
            token_id: "token-1".to_string(),
            condition_id: Some("condition-1".to_string()),
            timeframe: "1h".to_string(),
            strategy_id: STRATEGY_ID_ENDGAME_SWEEP_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            entry_mode: "unknown".to_string(),
            period_timestamp: period,
            price: 0.51,
            size_usd: 20.0,
            side: "BUY".to_string(),
            status: "OPEN".to_string(),
        })?;

        assert!(db.has_open_pending_order("1h", "evcurve", period, "token-1", "BUY")?);
        assert!(!db.has_open_pending_order_for_strategy(
            "1h",
            "evcurve",
            STRATEGY_ID_EVCURVE_V1,
            period,
            "token-1",
            "BUY",
        )?);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn sum_entry_fill_notional_trade_key_like_is_tick_scoped() -> Result<()> {
        let path = temp_db_path("evcurve_fill_tick_scope");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_000_102_u64;
        let token_id = "token-evcurve-1";

        db.record_trade_event(&TradeEventRecord {
            event_key: Some("fill-evcurve-tick0".to_string()),
            ts_ms: 10_000,
            period_timestamp: period,
            timeframe: "1h".to_string(),
            strategy_id: STRATEGY_ID_EVCURVE_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-1".to_string()),
            token_id: Some(token_id.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.98),
            units: Some(100.0),
            notional_usd: Some(98.0),
            pnl_usd: None,
            reason: Some(
                "{\"trade_key\":\"1700000102_token-evcurve-1_evcurve_98_1h_evcurve_v1_tick0\"}"
                    .to_string(),
            ),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some("fill-evcurve-tick1".to_string()),
            ts_ms: 10_100,
            period_timestamp: period,
            timeframe: "1h".to_string(),
            strategy_id: STRATEGY_ID_EVCURVE_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-1".to_string()),
            token_id: Some(token_id.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL_RECONCILED".to_string(),
            price: Some(0.97),
            units: Some(100.0),
            notional_usd: Some(97.0),
            pnl_usd: None,
            reason: Some(
                "{\"trade_key\":\"1700000102_token-evcurve-1_evcurve_97_1h_evcurve_v1_tick1\"}"
                    .to_string(),
            ),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some("fill-endgame-tick0".to_string()),
            ts_ms: 10_200,
            period_timestamp: period,
            timeframe: "1h".to_string(),
            strategy_id: STRATEGY_ID_ENDGAME_SWEEP_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-1".to_string()),
            token_id: Some(token_id.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.99),
            units: Some(100.0),
            notional_usd: Some(99.0),
            pnl_usd: None,
            reason: Some(
                "{\"trade_key\":\"1700000102_token-evcurve-1_endgame_99_1h_endgame_sweep_v1_tick0\"}"
                    .to_string(),
            ),
        })?;

        let tick0_sum = db.sum_entry_fill_notional_for_trade_key_like(
            STRATEGY_ID_EVCURVE_V1,
            "1h",
            period,
            token_id,
            "1700000102_token-evcurve-1_evcurve_%_1h_evcurve_v1_tick0",
        )?;
        let tick1_sum = db.sum_entry_fill_notional_for_trade_key_like(
            STRATEGY_ID_EVCURVE_V1,
            "1h",
            period,
            token_id,
            "1700000102_token-evcurve-1_evcurve_%_1h_evcurve_v1_tick1",
        )?;

        assert!((tick0_sum - 98.0).abs() < 1e-9);
        assert!((tick1_sum - 97.0).abs() < 1e-9);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn sum_entry_fill_notional_trade_key_like_dedupes_fill_and_reconciled_same_order() -> Result<()>
    {
        let path = temp_db_path("evcurve_fill_trade_key_dedupe");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_000_103_u64;
        let token_id = "token-evcurve-dedupe-1";
        let order_id = "ord-evcurve-dedupe-1";
        let trade_key = "1700000103_token-evcurve-dedupe-1_evcurve_95_1h_evcurve_v1_tick1";

        db.record_trade_event(&TradeEventRecord {
            event_key: Some(format!("entry_fill_order:{}", order_id)),
            ts_ms: 11_000,
            period_timestamp: period,
            timeframe: "1h".to_string(),
            strategy_id: STRATEGY_ID_EVCURVE_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-1".to_string()),
            token_id: Some(token_id.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.95),
            units: Some(100.0),
            notional_usd: Some(95.0),
            pnl_usd: None,
            reason: Some(format!(
                "{{\"trade_key\":\"{}\",\"order_id\":\"{}\"}}",
                trade_key, order_id
            )),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some(format!("entry_fill_reconciled:{}", order_id)),
            ts_ms: 11_100,
            period_timestamp: period,
            timeframe: "1h".to_string(),
            strategy_id: STRATEGY_ID_EVCURVE_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-1".to_string()),
            token_id: Some(token_id.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL_RECONCILED".to_string(),
            price: Some(0.95),
            units: Some(100.0),
            notional_usd: Some(95.0),
            pnl_usd: None,
            reason: Some(format!(
                "{{\"trade_key\":\"{}\",\"order_id\":\"{}\"}}",
                trade_key, order_id
            )),
        })?;

        let total = db.sum_entry_fill_notional_for_trade_key_like(
            STRATEGY_ID_EVCURVE_V1,
            "1h",
            period,
            token_id,
            "1700000103_token-evcurve-dedupe-1_evcurve_%_1h_evcurve_v1_tick1",
        )?;
        assert!((total - 95.0).abs() < 1e-9);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn entry_fill_count_helpers_use_deduped_fill_rows() -> Result<()> {
        let path = temp_db_path("entry_fill_count_deduped");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_000_104_u64;
        let strategy = STRATEGY_ID_EVCURVE_V1;
        let condition = "cond-fill-count";
        let token = "token-fill-count";
        let order_id = "ord-fill-count-1";

        db.record_trade_event(&TradeEventRecord {
            event_key: Some(format!("entry_fill_order:{}", order_id)),
            ts_ms: 12_000,
            period_timestamp: period,
            timeframe: "1h".to_string(),
            strategy_id: strategy.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some(condition.to_string()),
            token_id: Some(token.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.95),
            units: Some(100.0),
            notional_usd: Some(95.0),
            pnl_usd: None,
            reason: Some(format!("{{\"order_id\":\"{}\"}}", order_id)),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some(format!("entry_fill_reconciled:{}", order_id)),
            ts_ms: 12_100,
            period_timestamp: period,
            timeframe: "1h".to_string(),
            strategy_id: strategy.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some(condition.to_string()),
            token_id: Some(token.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL_RECONCILED".to_string(),
            price: Some(0.95),
            units: Some(100.0),
            notional_usd: Some(95.0),
            pnl_usd: None,
            reason: Some(format!("{{\"order_id\":\"{}\"}}", order_id)),
        })?;

        assert_eq!(
            db.count_entry_fills_for_strategy_condition(strategy, condition)?,
            1
        );
        assert_eq!(
            db.count_entry_fills_for_strategy_token_period(strategy, "1h", period, token)?,
            1
        );
        assert_eq!(
            db.count_entry_fills_for_strategy_symbol_period(strategy, "1h", period, "BTC")?,
            1
        );

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn entry_fill_avg_price_dedupes_canonical_and_reconciled_pair() -> Result<()> {
        let path = temp_db_path("entry_fill_avg_deduped");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_000_105_u64;
        let strategy = STRATEGY_ID_MM_REWARDS_V1;
        let condition = "cond-fill-avg-dedupe";
        let token = "token-fill-avg-dedupe";
        let order_id = "ord-fill-avg-dedupe-1";

        db.record_trade_event(&TradeEventRecord {
            event_key: Some(format!("entry_fill_order:{}", order_id)),
            ts_ms: 13_000,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: strategy.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some(condition.to_string()),
            token_id: Some(token.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.50),
            units: Some(100.0),
            notional_usd: Some(50.0),
            pnl_usd: None,
            reason: Some(format!("{{\"order_id\":\"{}\"}}", order_id)),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some(format!("entry_fill_reconciled:{}", order_id)),
            ts_ms: 13_100,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: strategy.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some(condition.to_string()),
            token_id: Some(token.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL_RECONCILED".to_string(),
            price: Some(0.50),
            units: Some(100.0),
            notional_usd: Some(50.0),
            pnl_usd: None,
            reason: Some(format!("{{\"order_id\":\"{}\"}}", order_id)),
        })?;

        let global = db
            .get_entry_fill_avg_price_for_strategy_condition_token(strategy, condition, token)?
            .unwrap();
        let scoped = db
            .get_entry_fill_avg_price_for_strategy_scope_condition_token(
                strategy, "5m", period, condition, token,
            )?
            .unwrap();

        assert!((global.0 - 0.50).abs() < 1e-9);
        assert!((global.1 - 100.0).abs() < 1e-9);
        assert!((scoped.0 - 0.50).abs() < 1e-9);
        assert!((scoped.1 - 100.0).abs() < 1e-9);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn canonical_fill_lookup_requires_exact_order_id_match() -> Result<()> {
        let path = temp_db_path("canonical_fill_exact_order_id");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_000_106_u64;

        db.record_trade_event(&TradeEventRecord {
            event_key: Some("entry-fill-exact-match".to_string()),
            ts_ms: 14_000,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_PREMARKET_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-exact".to_string()),
            token_id: Some("token-exact".to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.40),
            units: Some(50.0),
            notional_usd: Some(20.0),
            pnl_usd: None,
            reason: Some("{\"note\":\"contains order-like text ord-exact-1\"}".to_string()),
        })?;

        assert!(!db.has_entry_fill_event_for_order_id("ord-exact-1")?);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn ladder_pending_guard_is_rung_scoped() -> Result<()> {
        let path = temp_db_path("ladder_rung_scope");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_000_200_u64;

        db.upsert_pending_order(&PendingOrderRecord {
            order_id: "ladder-order-r0".to_string(),
            trade_key: "1700000200_token-1_ladder_40_15m_premarket_v1".to_string(),
            token_id: "token-1".to_string(),
            condition_id: Some("condition-ladder".to_string()),
            timeframe: "15m".to_string(),
            strategy_id: STRATEGY_ID_PREMARKET_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            entry_mode: "ladder".to_string(),
            period_timestamp: period,
            price: 0.40,
            size_usd: 20.0,
            side: "BUY".to_string(),
            status: "OPEN".to_string(),
        })?;
        db.upsert_pending_order(&PendingOrderRecord {
            order_id: "ladder-order-r1".to_string(),
            trade_key: "1700000200_token-1_ladder_30_15m_premarket_v1".to_string(),
            token_id: "token-1".to_string(),
            condition_id: Some("condition-ladder".to_string()),
            timeframe: "15m".to_string(),
            strategy_id: STRATEGY_ID_PREMARKET_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            entry_mode: "ladder".to_string(),
            period_timestamp: period,
            price: 0.30,
            size_usd: 20.0,
            side: "BUY".to_string(),
            status: "OPEN".to_string(),
        })?;

        assert!(db.has_open_pending_order_for_trade_key(
            "1700000200_token-1_ladder_40_15m_premarket_v1",
            "BUY"
        )?);
        assert!(db.has_open_pending_order_for_trade_key(
            "1700000200_token-1_ladder_30_15m_premarket_v1",
            "BUY"
        )?);
        assert!(!db.has_open_pending_order_for_trade_key(
            "1700000200_token-1_ladder_20_15m_premarket_v1",
            "BUY"
        )?);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn settlement_v2_created_and_math_uses_cost_basis() -> Result<()> {
        let path = temp_db_path("settlement_v2_math");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_000_300_u64;

        db.record_trade_event(&TradeEventRecord {
            event_key: Some("v2-entry-1".to_string()),
            ts_ms: 10_000,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-v2-1".to_string()),
            token_id: Some("token-v2-1".to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.40),
            units: Some(100.0),
            notional_usd: Some(40.0),
            pnl_usd: None,
            reason: Some("limit_buy_fill".to_string()),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some("v2-exit-1".to_string()),
            ts_ms: 20_000,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-v2-1".to_string()),
            token_id: Some("token-v2-1".to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("SELL".to_string()),
            event_type: "EXIT".to_string(),
            price: Some(1.0),
            units: Some(100.0),
            notional_usd: Some(100.0),
            pnl_usd: Some(60.0),
            reason: Some("production_market_close".to_string()),
        })?;

        let settled = db.summarize_settled_performance_v2()?;
        assert_eq!(settled.settled_trades, 1);
        assert_eq!(settled.wins, 1);
        assert_eq!(settled.losses, 0);
        assert!((settled.net_settled_pnl_usd - 60.0).abs() < 1e-9);
        assert!((settled.win_rate_pct - 100.0).abs() < 1e-9);

        let open = db.summarize_open_mtm_v2()?;
        assert_eq!(open.open_positions, 0);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn settled_win_rate_v2_excludes_open_positions() -> Result<()> {
        let path = temp_db_path("settled_excludes_open");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_000_400_u64;

        db.record_trade_event(&TradeEventRecord {
            event_key: Some("open-entry".to_string()),
            ts_ms: 10_000,
            period_timestamp: period,
            timeframe: "15m".to_string(),
            strategy_id: STRATEGY_ID_PREMARKET_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-open".to_string()),
            token_id: Some("token-open".to_string()),
            token_type: Some("BTC Down".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.30),
            units: Some(50.0),
            notional_usd: Some(15.0),
            pnl_usd: None,
            reason: Some("limit_buy_fill".to_string()),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some("settled-entry".to_string()),
            ts_ms: 11_000,
            period_timestamp: period,
            timeframe: "15m".to_string(),
            strategy_id: STRATEGY_ID_PREMARKET_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-settled".to_string()),
            token_id: Some("token-settled".to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.55),
            units: Some(20.0),
            notional_usd: Some(11.0),
            pnl_usd: None,
            reason: Some("limit_buy_fill".to_string()),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some("settled-exit".to_string()),
            ts_ms: 12_000,
            period_timestamp: period,
            timeframe: "15m".to_string(),
            strategy_id: STRATEGY_ID_PREMARKET_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-settled".to_string()),
            token_id: Some("token-settled".to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("SELL".to_string()),
            event_type: "EXIT".to_string(),
            price: Some(0.0),
            units: Some(20.0),
            notional_usd: Some(0.0),
            pnl_usd: Some(-11.0),
            reason: Some("production_market_close".to_string()),
        })?;

        let settled = db.summarize_settled_performance_v2()?;
        assert_eq!(settled.settled_trades, 1);
        assert_eq!(settled.wins, 0);
        assert_eq!(settled.losses, 1);
        assert!((settled.net_settled_pnl_usd + 11.0).abs() < 1e-9);

        let open = db.summarize_open_mtm_v2()?;
        assert_eq!(open.open_positions, 1);
        assert!(open.net_units > 0.0);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn outcome_source_modes_use_settlement_first_counts() -> Result<()> {
        let path = temp_db_path("outcome_source_modes");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_000_450_u64;

        db.record_trade_event(&TradeEventRecord {
            event_key: Some("outcome-mode-entry".to_string()),
            ts_ms: 10_000,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-outcome-mode".to_string()),
            token_id: Some("token-outcome-mode".to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.40),
            units: Some(100.0),
            notional_usd: Some(40.0),
            pnl_usd: None,
            reason: Some("limit_buy_fill".to_string()),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some("outcome-mode-exit-market-close".to_string()),
            ts_ms: 20_000,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-outcome-mode".to_string()),
            token_id: Some("token-outcome-mode".to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("SELL".to_string()),
            event_type: "EXIT".to_string(),
            price: Some(1.0),
            units: Some(100.0),
            notional_usd: Some(100.0),
            pnl_usd: Some(60.0),
            reason: Some("production_market_close".to_string()),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some("outcome-mode-exit-stop-loss".to_string()),
            ts_ms: 21_000,
            period_timestamp: period,
            timeframe: "15m".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-outcome-mode-2".to_string()),
            token_id: Some("token-outcome-mode-2".to_string()),
            token_type: Some("BTC Down".to_string()),
            side: Some("SELL".to_string()),
            event_type: "EXIT".to_string(),
            price: Some(0.0),
            units: Some(10.0),
            notional_usd: Some(0.0),
            pnl_usd: Some(-3.0),
            reason: Some("stop_loss".to_string()),
        })?;

        let closed_settlement = db.total_closed_trades_for_strategy_with_source(
            STRATEGY_ID_TEST_PRIMARY,
            OutcomeSource::Settlement,
        )?;
        let closed_exit = db.total_closed_trades_for_strategy_with_source(
            STRATEGY_ID_TEST_PRIMARY,
            OutcomeSource::Exit,
        )?;
        let closed_hybrid = db.total_closed_trades_for_strategy_with_source(
            STRATEGY_ID_TEST_PRIMARY,
            OutcomeSource::Hybrid,
        )?;
        assert_eq!(closed_settlement, 1);
        assert_eq!(closed_exit, 2);
        assert_eq!(closed_hybrid, 2);

        let perf_settlement = db.summarize_timeframe_performance_for_strategy_with_source(
            STRATEGY_ID_TEST_PRIMARY,
            OutcomeSource::Settlement,
        )?;
        assert_eq!(perf_settlement.len(), 1);
        assert_eq!(perf_settlement[0].timeframe, "5m");
        assert!((perf_settlement[0].net_pnl_usd - 60.0).abs() < 1e-9);

        let perf_hybrid = db.summarize_timeframe_performance_for_strategy_with_source(
            STRATEGY_ID_TEST_PRIMARY,
            OutcomeSource::Hybrid,
        )?;
        assert_eq!(perf_hybrid.len(), 2);
        let mut by_tf: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        for row in perf_hybrid {
            by_tf.insert(row.timeframe, row.net_pnl_usd);
        }
        assert!((by_tf.get("5m").copied().unwrap_or_default() - 60.0).abs() < 1e-9);
        assert!((by_tf.get("15m").copied().unwrap_or_default() + 3.0).abs() < 1e-9);

        let sig_settlement = db.summarize_signal_accuracy_for_strategy_with_source(
            STRATEGY_ID_TEST_PRIMARY,
            OutcomeSource::Settlement,
        )?;
        assert_eq!(sig_settlement.len(), 1);
        assert_eq!(sig_settlement[0].signal, "settlement_outcome");

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn outcome_source_parsing_defaults_to_settlement() {
        assert_eq!(
            OutcomeSource::from_raw("settlement"),
            OutcomeSource::Settlement
        );
        assert_eq!(OutcomeSource::from_raw("exit"), OutcomeSource::Exit);
        assert_eq!(OutcomeSource::from_raw("hybrid"), OutcomeSource::Hybrid);
        assert_eq!(
            OutcomeSource::from_raw("unknown"),
            OutcomeSource::Settlement
        );
    }

    #[test]
    fn dual_write_v2_smoke_tracks_entry_fill() -> Result<()> {
        let path = temp_db_path("dual_write_v2_smoke");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_000_500_u64;

        db.record_trade_event(&TradeEventRecord {
            event_key: Some("dual-entry-1".to_string()),
            ts_ms: 10_000,
            period_timestamp: period,
            timeframe: "1h".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-dual".to_string()),
            token_id: Some("token-dual".to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.25),
            units: Some(80.0),
            notional_usd: Some(20.0),
            pnl_usd: None,
            reason: Some("limit_buy_fill".to_string()),
        })?;

        let counts: (i64, i64, i64) = db.with_conn(|conn| {
            let trade_events: i64 = conn.query_row(
                "SELECT COUNT(*) FROM trade_events WHERE event_key='dual-entry-1'",
                [],
                |row| row.get(0),
            )?;
            let fills_v2: i64 = conn.query_row(
                "SELECT COUNT(*) FROM fills_v2 WHERE event_key='dual-entry-1'",
                [],
                |row| row.get(0),
            )?;
            let positions_v2: i64 = conn.query_row(
                "SELECT COUNT(*) FROM positions_v2 WHERE token_id='token-dual'",
                [],
                |row| row.get(0),
            )?;
            Ok((trade_events, fills_v2, positions_v2))
        })?;
        assert_eq!(counts.0, 1);
        assert_eq!(counts.1, 1);
        assert_eq!(counts.2, 1);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn fill_event_key_uniqueness_for_multiple_fills_same_token_period() -> Result<()> {
        let path = temp_db_path("fill_key_uniqueness");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_010_000_u64;
        let token_id = "token-same-period";

        db.record_trade_event(&TradeEventRecord {
            event_key: Some("entry_fill_order:ord-1".to_string()),
            ts_ms: 101_000,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-fill-1".to_string()),
            token_id: Some(token_id.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.40),
            units: Some(25.0),
            notional_usd: Some(10.0),
            pnl_usd: None,
            reason: Some("{\"order_id\":\"ord-1\"}".to_string()),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some("entry_fill_order:ord-2".to_string()),
            ts_ms: 102_000,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-fill-1".to_string()),
            token_id: Some(token_id.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.40),
            units: Some(30.0),
            notional_usd: Some(12.0),
            pnl_usd: None,
            reason: Some("{\"order_id\":\"ord-2\"}".to_string()),
        })?;

        let (count, notional): (i64, f64) = db.with_conn(|conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM trade_events WHERE event_type='ENTRY_FILL' AND period_timestamp=?1 AND token_id=?2",
                params![period as i64, token_id],
                |row| row.get(0),
            )?;
            let notional: f64 = conn.query_row(
                "SELECT COALESCE(SUM(notional_usd),0.0) FROM trade_events WHERE event_type='ENTRY_FILL' AND period_timestamp=?1 AND token_id=?2",
                params![period as i64, token_id],
                |row| row.get(0),
            )?;
            Ok((count, notional))
        })?;
        assert_eq!(count, 2);
        assert!((notional - 22.0).abs() < 1e-9);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn entry_fill_avg_price_scope_isolated_by_period() -> Result<()> {
        let path = temp_db_path("entry_fill_avg_scope_isolated");
        let db = TrackingDb::new(&path)?;
        let period_a = 1_700_020_000_u64;
        let period_b = 1_700_020_300_u64;
        let strategy = "mm_rewards_v1";
        let condition = "cond-mm-scope";
        let token = "token-mm-scope";

        db.record_trade_event(&TradeEventRecord {
            event_key: Some("mm-entry-a".to_string()),
            ts_ms: 201_000,
            period_timestamp: period_a,
            timeframe: "5m".to_string(),
            strategy_id: strategy.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some(condition.to_string()),
            token_id: Some(token.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.50),
            units: Some(100.0),
            notional_usd: Some(50.0),
            pnl_usd: None,
            reason: Some("scope-a".to_string()),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some("mm-entry-b".to_string()),
            ts_ms: 202_000,
            period_timestamp: period_b,
            timeframe: "5m".to_string(),
            strategy_id: strategy.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some(condition.to_string()),
            token_id: Some(token.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.80),
            units: Some(100.0),
            notional_usd: Some(80.0),
            pnl_usd: None,
            reason: Some("scope-b".to_string()),
        })?;

        let global_avg = db
            .get_entry_fill_avg_price_for_strategy_condition_token(strategy, condition, token)?
            .map(|(avg, _)| avg)
            .unwrap_or_default();
        let scoped_avg_a = db
            .get_entry_fill_avg_price_for_strategy_scope_condition_token(
                strategy, "5m", period_a, condition, token,
            )?
            .map(|(avg, _)| avg)
            .unwrap_or_default();
        let scoped_avg_b = db
            .get_entry_fill_avg_price_for_strategy_scope_condition_token(
                strategy, "5m", period_b, condition, token,
            )?
            .map(|(avg, _)| avg)
            .unwrap_or_default();

        assert!((global_avg - 0.65).abs() < 1e-9);
        assert!((scoped_avg_a - 0.50).abs() < 1e-9);
        assert!((scoped_avg_b - 0.80).abs() < 1e-9);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn mm_merge_inventory_consumption_reduces_positions_without_exit_events() -> Result<()> {
        let path = temp_db_path("mm_merge_inventory_consumption");
        let db = TrackingDb::new(&path)?;
        let condition_id = "cond-mm-merge-consume";
        let up_token_id = "token-mm-up-consume";
        let down_token_id = "token-mm-down-consume";
        let period_timestamp = 1_700_222_000_u64;

        for (event_key, token_id, token_type, price, notional) in [
            ("mm-merge-entry-up", up_token_id, "BTC Up", 0.55, 55.0),
            ("mm-merge-entry-down", down_token_id, "BTC Down", 0.45, 45.0),
        ] {
            db.record_trade_event(&TradeEventRecord {
                event_key: Some(event_key.to_string()),
                ts_ms: 101_000,
                period_timestamp,
                timeframe: "mm".to_string(),
                strategy_id: STRATEGY_ID_MM_REWARDS_V1.to_string(),
                asset_symbol: Some("BTC".to_string()),
                condition_id: Some(condition_id.to_string()),
                token_id: Some(token_id.to_string()),
                token_type: Some(token_type.to_string()),
                side: Some("BUY".to_string()),
                event_type: "ENTRY_FILL".to_string(),
                price: Some(price),
                units: Some(100.0),
                notional_usd: Some(notional),
                pnl_usd: None,
                reason: Some("unit_test_mm_merge".to_string()),
            })?;
        }

        let results = db.consume_mm_merge_inventory(
            STRATEGY_ID_MM_REWARDS_V1,
            "mm",
            period_timestamp,
            condition_id,
            up_token_id,
            down_token_id,
            40.0,
            Some("tx-mm-merge-consume"),
            Some("unit_test_merge"),
            "merge_sweep",
        )?;
        assert_eq!(results.len(), 2);
        for row in results {
            assert!((row.requested_units - 40.0).abs() < 1e-9);
            assert!((row.applied_units - 40.0).abs() < 1e-9);
            assert!((row.db_units_before - 100.0).abs() < 1e-9);
            assert!((row.db_units_after - 60.0).abs() < 1e-9);
        }

        let token_rows: Vec<(String, f64, f64, String)> = db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
SELECT token_id, exit_units, inventory_consumed_units, status
FROM positions_v2
WHERE strategy_id=?1
  AND condition_id=?2
ORDER BY token_id ASC
"#,
            )?;
            let rows = stmt.query_map(params![STRATEGY_ID_MM_REWARDS_V1, condition_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })?;
        assert_eq!(token_rows.len(), 2);
        for (_, exit_units, inventory_consumed_units, status) in token_rows {
            assert!(exit_units.abs() < 1e-9);
            assert!((inventory_consumed_units - 40.0).abs() < 1e-9);
            assert_eq!(status, "OPEN");
        }

        let inventory_rows =
            db.list_open_position_inventory_by_strategy(STRATEGY_ID_MM_REWARDS_V1, 10)?;
        assert_eq!(inventory_rows.len(), 2);
        for row in inventory_rows {
            assert!((row.net_units - 60.0).abs() < 1e-9);
        }

        let exit_events: i64 = db.with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM trade_events WHERE condition_id=?1 AND event_type='EXIT'",
                params![condition_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
        })?;
        assert_eq!(exit_events, 0);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn mm_inventory_reconcile_repairs_db_and_wallet_gaps() -> Result<()> {
        let path = temp_db_path("mm_inventory_reconcile_wallet_drift");
        let db = TrackingDb::new(&path)?;
        ensure_wallet_sidecar_tables(&db)?;
        let condition_id = "cond-mm-reconcile";
        let up_token_id = "token-mm-reconcile-up";
        let down_token_id = "token-mm-reconcile-down";
        let period_timestamp = 1_700_333_000_u64;

        db.upsert_mm_market_state(&MmMarketStateUpsertRecord {
            strategy_id: STRATEGY_ID_MM_REWARDS_V1.to_string(),
            condition_id: condition_id.to_string(),
            market_slug: Some("market-mm-reconcile".to_string()),
            timeframe: "mm".to_string(),
            symbol: Some("BTC".to_string()),
            mode: Some("target".to_string()),
            period_timestamp,
            state: "active".to_string(),
            reason: Some("unit_test".to_string()),
            reward_rate_hint: None,
            open_buy_orders: 0,
            open_sell_orders: 0,
            favor_inventory_shares: None,
            weak_inventory_shares: None,
            up_token_id: Some(up_token_id.to_string()),
            down_token_id: Some(down_token_id.to_string()),
            up_inventory_shares: None,
            down_inventory_shares: None,
            imbalance_shares: None,
            imbalance_side: None,
            avg_entry_up: None,
            avg_entry_down: None,
            up_best_bid: None,
            down_best_bid: None,
            mtm_pnl_exit_usd: None,
            time_in_imbalance_ms: None,
            max_imbalance_shares: None,
            last_rebalance_duration_ms: None,
            rebalance_fill_rate: None,
            exit_slippage_vs_bbo_bps: None,
            market_mode_runtime: None,
            ladder_top_price: None,
            ladder_bottom_price: None,
            last_event_ms: 111_000,
        })?;

        for (event_key, token_id, token_type, price, notional) in [
            ("mm-reconcile-entry-up", up_token_id, "BTC Up", 0.50, 50.0),
            (
                "mm-reconcile-entry-down",
                down_token_id,
                "BTC Down",
                0.50,
                50.0,
            ),
        ] {
            db.record_trade_event(&TradeEventRecord {
                event_key: Some(event_key.to_string()),
                ts_ms: 112_000,
                period_timestamp,
                timeframe: "mm".to_string(),
                strategy_id: STRATEGY_ID_MM_REWARDS_V1.to_string(),
                asset_symbol: Some("BTC".to_string()),
                condition_id: Some(condition_id.to_string()),
                token_id: Some(token_id.to_string()),
                token_type: Some(token_type.to_string()),
                side: Some("BUY".to_string()),
                event_type: "ENTRY_FILL".to_string(),
                price: Some(price),
                units: Some(100.0),
                notional_usd: Some(notional),
                pnl_usd: None,
                reason: Some("unit_test_mm_reconcile".to_string()),
            })?;
        }

        db.with_conn(|conn| {
            conn.execute(
                r#"
INSERT OR REPLACE INTO wallet_positions_live_latest_v1 (
    wallet_address, condition_id, token_id, snapshot_ts_ms, position_size, avg_price, cur_price, current_value
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
"#,
                params!["0xwallet", condition_id, up_token_id, 120_000_i64, 30.0_f64, 0.5_f64, 0.31_f64, 9.3_f64],
            )?;
            conn.execute(
                r#"
INSERT OR REPLACE INTO wallet_positions_live_latest_v1 (
    wallet_address, condition_id, token_id, snapshot_ts_ms, position_size, avg_price, cur_price, current_value
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
"#,
                params!["0xwallet", condition_id, down_token_id, 120_000_i64, 140.0_f64, 0.5_f64, 0.49_f64, 68.6_f64],
            )?;
            conn.execute(
                r#"
INSERT OR REPLACE INTO wallet_activity_v1 (
    wallet_address, activity_key, ts_ms, activity_type, condition_id, token_id,
    outcome, side, size, usdc_size, inventory_consumed_units, price, transaction_hash, slug, title, raw_json, synced_at_ms
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
"#,
                params![
                    "0xwallet",
                    "activity-up",
                    121_000_i64,
                    "MERGE",
                    condition_id,
                    up_token_id,
                    "Yes",
                    "SELL",
                    70.0_f64,
                    70.0_f64,
                    70.0_f64,
                    1.0_f64,
                    "0xtx-up",
                    "slug-up",
                    "title-up",
                    "{}",
                    121_000_i64
                ],
            )?;
            conn.execute(
                r#"
INSERT OR REPLACE INTO positions_unrealized_mid_latest_v1 (
    position_key, snapshot_ts_ms, strategy_id, timeframe, period_timestamp,
    condition_id, token_id, token_type, remaining_units, remaining_cost_usd,
    entry_avg_price, best_bid, best_ask, mid_price, unrealized_mid_pnl_usd, mark_source
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
"#,
                params![
                    format!("{}:{}:{}:{}", STRATEGY_ID_MM_REWARDS_V1, "mm", period_timestamp, up_token_id),
                    122_000_i64,
                    STRATEGY_ID_MM_REWARDS_V1,
                    "mm",
                    period_timestamp as i64,
                    condition_id,
                    up_token_id,
                    "BTC Up",
                    30.0_f64,
                    15.0_f64,
                    0.5_f64,
                    0.31_f64,
                    0.32_f64,
                    0.315_f64,
                    -5.55_f64,
                    "midpoint"
                ],
            )?;
            conn.execute(
                r#"
INSERT OR REPLACE INTO positions_unrealized_mid_latest_v1 (
    position_key, snapshot_ts_ms, strategy_id, timeframe, period_timestamp,
    condition_id, token_id, token_type, remaining_units, remaining_cost_usd,
    entry_avg_price, best_bid, best_ask, mid_price, unrealized_mid_pnl_usd, mark_source
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
"#,
                params![
                    format!("{}:{}:{}:{}", STRATEGY_ID_MM_REWARDS_V1, "mm", period_timestamp, down_token_id),
                    122_000_i64,
                    STRATEGY_ID_MM_REWARDS_V1,
                    "mm",
                    period_timestamp as i64,
                    condition_id,
                    down_token_id,
                    "BTC Down",
                    140.0_f64,
                    70.0_f64,
                    0.5_f64,
                    0.49_f64,
                    0.50_f64,
                    0.495_f64,
                    -0.7_f64,
                    "midpoint"
                ],
            )?;
            Ok(())
        })?;

        let repairs =
            db.reconcile_mm_inventory_drift_from_wallet_tables(STRATEGY_ID_MM_REWARDS_V1, 1.0, 10)?;
        assert_eq!(repairs.len(), 2);
        let consume = repairs
            .iter()
            .find(|row| row.token_id == up_token_id)
            .expect("consume repair missing");
        let add = repairs
            .iter()
            .find(|row| row.token_id == down_token_id)
            .expect("add repair missing");
        assert_eq!(consume.action, "consume");
        assert!((consume.db_inventory_shares_before - 100.0).abs() < 1e-9);
        assert!((consume.wallet_inventory_shares - 30.0).abs() < 1e-9);
        assert!((consume.drift_shares_before - 70.0).abs() < 1e-9);
        assert!((consume.applied_consume_shares - 70.0).abs() < 1e-9);
        assert!((consume.db_inventory_shares_after - 30.0).abs() < 1e-9);
        assert_eq!(add.action, "add");
        assert!((add.db_inventory_shares_before - 100.0).abs() < 1e-9);
        assert!((add.wallet_inventory_shares - 140.0).abs() < 1e-9);
        assert!((add.drift_shares_before + 40.0).abs() < 1e-9);
        assert!((add.applied_add_shares - 40.0).abs() < 1e-9);
        assert!((add.db_inventory_shares_after - 140.0).abs() < 1e-9);

        let per_token_inventory =
            db.list_open_position_inventory_by_strategy(STRATEGY_ID_MM_REWARDS_V1, 10)?;
        let up_row = per_token_inventory
            .iter()
            .find(|row| row.token_id == up_token_id)
            .expect("up inventory row missing");
        let down_row = per_token_inventory
            .iter()
            .find(|row| row.token_id == down_token_id)
            .expect("down inventory row missing");
        assert!((up_row.net_units - 30.0).abs() < 1e-9);
        assert!((down_row.net_units - 140.0).abs() < 1e-9);

        let attribution = db.summarize_mm_market_attribution(STRATEGY_ID_MM_REWARDS_V1, None)?;
        let row = attribution
            .iter()
            .find(|row| row.condition_id == condition_id)
            .expect("attribution row missing");
        assert!((row.db_inventory_shares - 170.0).abs() < 1e-9);
        assert_eq!(row.wallet_inventory_shares, Some(170.0));
        assert_eq!(row.inventory_drift_shares, Some(0.0));
        assert_eq!(row.last_wallet_activity_type.as_deref(), Some("MERGE"));
        assert_eq!(row.last_wallet_activity_ts_ms, Some(121_000));
        assert!((row.wallet_inventory_value_live_usd.unwrap_or_default() - 77.9).abs() < 1e-9);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn no_fill_status_without_trade_event() -> Result<()> {
        let path = temp_db_path("fill_status_requires_event");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_010_100_u64;
        let order_id = "order-fill-required";

        db.upsert_pending_order(&PendingOrderRecord {
            order_id: order_id.to_string(),
            trade_key: "trade-fill-required".to_string(),
            token_id: "token-fill-required".to_string(),
            condition_id: Some("cond-fill-required".to_string()),
            timeframe: "15m".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            entry_mode: "reactive".to_string(),
            period_timestamp: period,
            price: 0.42,
            size_usd: 21.0,
            side: "BUY".to_string(),
            status: "OPEN".to_string(),
        })?;

        let inserted = db.mark_pending_order_filled_with_event(
            order_id,
            &TradeEventRecord {
                event_key: Some(format!("entry_fill_order:{}", order_id)),
                ts_ms: 103_000,
                period_timestamp: period,
                timeframe: "15m".to_string(),
                strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
                asset_symbol: Some("BTC".to_string()),
                condition_id: Some("cond-fill-required".to_string()),
                token_id: Some("token-fill-required".to_string()),
                token_type: Some("BTC Down".to_string()),
                side: Some("BUY".to_string()),
                event_type: "ENTRY_FILL".to_string(),
                price: Some(0.42),
                units: Some(50.0),
                notional_usd: Some(21.0),
                pnl_usd: None,
                reason: Some(format!(
                    "{{\"mode\":\"unit\",\"order_id\":\"{}\"}}",
                    order_id
                )),
            },
        )?;
        assert!(inserted);

        let (status, fill_count): (String, i64) = db.with_conn(|conn| {
            let status: String = conn.query_row(
                "SELECT status FROM pending_orders WHERE order_id=?1",
                params![order_id],
                |row| row.get(0),
            )?;
            let fill_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM trade_events WHERE event_key=?1",
                params![format!("entry_fill_order:{}", order_id)],
                |row| row.get(0),
            )?;
            Ok((status, fill_count))
        })?;
        assert_eq!(status, "FILLED");
        assert_eq!(fill_count, 1);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn trade_event_snapshot_updates_existing_scope_by_order_id() -> Result<()> {
        let path = temp_db_path("trade_event_snapshot_scope_match");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_010_150_u64;

        db.upsert_strategy_feature_snapshot_intent(&StrategyFeatureSnapshotIntentRecord {
            strategy_id: STRATEGY_ID_PREMARKET_V1.to_string(),
            timeframe: "5m".to_string(),
            period_timestamp: period,
            market_key: Some("btc".to_string()),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-scope".to_string()),
            token_id: "token-scope".to_string(),
            impulse_id: "premarket_decision:scope".to_string(),
            decision_id: Some("decision-scope".to_string()),
            rung_id: Some("rung0".to_string()),
            slice_id: None,
            request_id: Some("req-scope".to_string()),
            order_id: Some("ord-scope".to_string()),
            trade_key: Some("trade-scope".to_string()),
            position_key: None,
            intent_status: Some("submitted".to_string()),
            execution_status: Some("ack".to_string()),
            intent_ts_ms: 100_000,
            submit_ts_ms: Some(100_000),
            ack_ts_ms: Some(100_100),
            fill_ts_ms: None,
            exit_ts_ms: None,
            settled_ts_ms: None,
            entry_notional_usd: Some(20.0),
            exit_notional_usd: None,
            realized_pnl_usd: None,
            settled_pnl_usd: None,
            hold_ms: None,
            feature_json: None,
            decision_context_json: None,
            execution_context_json: None,
        })?;

        let _snapshot = db
            .upsert_strategy_feature_snapshot_from_trade_event(&TradeEventRecord {
            event_key: Some("entry_fill_order:ord-scope".to_string()),
            ts_ms: 101_000,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_PREMARKET_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-scope".to_string()),
            token_id: Some("token-scope".to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.45),
            units: Some(44.0),
            notional_usd: Some(19.8),
            pnl_usd: None,
            reason: Some(
                r#"{"order_id":"ord-scope","request_id":"req-scope","trade_key":"trade-scope"}"#
                    .to_string(),
            ),
        })?;

        let (rows, impulse_id, rung_id, execution_status): (i64, String, Option<String>, String) =
            db.with_conn(|conn| {
                conn.query_row(
                    r#"
SELECT
    (SELECT COUNT(*) FROM strategy_feature_snapshots_v1 WHERE strategy_id=?1 AND timeframe='5m' AND period_timestamp=?2 AND token_id='token-scope') AS rows,
    impulse_id,
    rung_id,
    execution_status
FROM strategy_feature_snapshots_v1
WHERE strategy_id=?1
  AND timeframe='5m'
  AND period_timestamp=?2
  AND token_id='token-scope'
LIMIT 1
"#,
                    params![STRATEGY_ID_PREMARKET_V1, period as i64],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .map_err(Into::into)
            })?;
        assert_eq!(rows, 1);
        assert_eq!(impulse_id, "premarket_decision:scope");
        assert_eq!(rung_id.as_deref(), Some("rung0"));
        assert_eq!(execution_status, "filled");

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn snapback_trade_event_scope_matches_by_request_id_for_submit_and_ack() -> Result<()> {
        let path = temp_db_path("snapback_trade_event_scope_request_id");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_010_155_u64;

        db.upsert_snapback_feature_snapshot_intent(&SnapbackFeatureSnapshotIntentRecord {
            strategy_id: STRATEGY_ID_TEST_TERTIARY.to_string(),
            timeframe: "5m".to_string(),
            period_timestamp: period,
            condition_id: "cond-snapback-scope".to_string(),
            token_id: "token-snapback-scope".to_string(),
            impulse_id: "5m:scope:down:1:1".to_string(),
            position_key: None,
            entry_side: Some("UP".to_string()),
            intent_ts_ms: 200_000,
            request_id: Some("req-snapback-scope".to_string()),
            order_id: None,
            signed_notional_30s: Some(-200_000.0),
            signed_notional_300s: Some(-800_000.0),
            signed_notional_900s: Some(-1_500_000.0),
            burst_zscore: Some(2.0),
            side_imbalance_5m: Some(-0.4),
            impulse_strength: 0.8,
            impulse_mode: Some("follow".to_string()),
            impulse_size_multiplier: Some(1.0571428571428572),
            direction_policy_split: Some(0.60),
            trigger_signed_flow: Some(-800_000.0),
            trigger_flow_source: Some("binance".to_string()),
            fair_probability: Some(0.57),
            reservation_price: Some(0.50),
            max_price: Some(0.48),
            limit_price: Some(0.47),
            edge_bps: Some(20.0),
            target_size_usd: Some(35.0),
            bias_alignment: Some("aligned".to_string()),
            bias_confidence: Some(0.62),
        })?;

        db.upsert_strategy_feature_snapshot_from_trade_event(&TradeEventRecord {
            event_key: Some("entry_submit:snapback".to_string()),
            ts_ms: 200_100,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_TERTIARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-snapback-scope".to_string()),
            token_id: Some("token-snapback-scope".to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_SUBMIT".to_string(),
            price: Some(0.47),
            units: Some(74.0),
            notional_usd: Some(34.78),
            pnl_usd: None,
            reason: Some(
                r#"{"request_id":"req-snapback-scope","trade_key":"trade-snapback-scope","submitted_at_ms":200100}"#
                    .to_string(),
            ),
        })?;

        db.upsert_strategy_feature_snapshot_from_trade_event(&TradeEventRecord {
            event_key: Some("entry_ack:snapback".to_string()),
            ts_ms: 200_180,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_TERTIARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-snapback-scope".to_string()),
            token_id: Some("token-snapback-scope".to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_ACK".to_string(),
            price: Some(0.47),
            units: Some(74.0),
            notional_usd: Some(34.78),
            pnl_usd: None,
            reason: Some(
                r#"{"request_id":"req-snapback-scope","trade_key":"trade-snapback-scope","order_id":"ord-snapback-scope","submitted_at_ms":200100,"acked_at_ms":200180}"#
                    .to_string(),
            ),
        })?;

        let details: (i64, i64, i64, String, String, String) = db.with_conn(|conn| {
            conn.query_row(
                r#"
SELECT
    (SELECT COUNT(*) FROM strategy_feature_snapshots_v1 WHERE strategy_id=?1 AND timeframe='5m' AND period_timestamp=?2 AND token_id='token-snapback-scope'),
    COALESCE(submit_ts_ms, 0),
    COALESCE(ack_ts_ms, 0),
    COALESCE(request_id, ''),
    COALESCE(order_id, ''),
    COALESCE(impulse_id, '')
FROM strategy_feature_snapshots_v1
WHERE strategy_id=?1
  AND timeframe='5m'
  AND period_timestamp=?2
  AND token_id='token-snapback-scope'
  AND impulse_id='5m:scope:down:1:1'
"#,
                params![STRATEGY_ID_TEST_TERTIARY, period as i64],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .map_err(Into::into)
        })?;
        assert_eq!(details.0, 1);
        assert_eq!(details.1, 200_100);
        assert_eq!(details.2, 200_180);
        assert_eq!(details.3, "req-snapback-scope");
        assert_eq!(details.4, "ord-snapback-scope");
        assert_eq!(details.5, "5m:scope:down:1:1");

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn fill_reconcile_backfills_missing_entry_fill() -> Result<()> {
        let path = temp_db_path("fill_reconcile_backfill");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_010_200_u64;
        let order_id = "order-reconcile-backfill";

        db.upsert_pending_order(&PendingOrderRecord {
            order_id: order_id.to_string(),
            trade_key: "trade-reconcile-backfill".to_string(),
            token_id: "token-reconcile-backfill".to_string(),
            condition_id: Some("cond-reconcile-backfill".to_string()),
            timeframe: "1h".to_string(),
            strategy_id: STRATEGY_ID_TEST_SECONDARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            entry_mode: "ladder".to_string(),
            period_timestamp: period,
            price: 0.35,
            size_usd: 28.0,
            side: "BUY".to_string(),
            status: "OPEN".to_string(),
        })?;
        assert!(!db.has_entry_fill_event_for_order_id(order_id)?);

        let inserted = db.mark_pending_order_filled_with_event(
            order_id,
            &TradeEventRecord {
                event_key: Some(format!("entry_fill_order:{}", order_id)),
                ts_ms: 104_000,
                period_timestamp: period,
                timeframe: "1h".to_string(),
                strategy_id: STRATEGY_ID_TEST_SECONDARY.to_string(),
                asset_symbol: Some("BTC".to_string()),
                condition_id: None,
                token_id: Some("token-reconcile-backfill".to_string()),
                token_type: Some("BTC Up".to_string()),
                side: Some("BUY".to_string()),
                event_type: "ENTRY_FILL".to_string(),
                price: Some(0.35),
                units: Some(80.0),
                notional_usd: Some(28.0),
                pnl_usd: None,
                reason: Some(format!(
                    "{{\"mode\":\"reconcile\",\"order_id\":\"{}\"}}",
                    order_id
                )),
            },
        )?;
        assert!(inserted);
        assert!(db.has_entry_fill_event_for_order_id(order_id)?);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn entry_fill_reconciled_does_not_double_count_canonical_fill() -> Result<()> {
        let path = temp_db_path("fill_reconciled_no_double_count");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_010_250_u64;
        let order_id = "order-fill-reconciled-no-double";
        let token_id = "token-fill-reconciled-no-double";
        let condition_id = "cond-fill-reconciled-no-double";
        let notional = 12.34_f64;
        let units = 24.68_f64;

        db.record_trade_event(&TradeEventRecord {
            event_key: Some(format!("entry_fill_order:{}", order_id)),
            ts_ms: 104_100,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some(condition_id.to_string()),
            token_id: Some(token_id.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.50),
            units: Some(units),
            notional_usd: Some(notional),
            pnl_usd: None,
            reason: Some(format!("{{\"order_id\":\"{}\"}}", order_id)),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some(format!("entry_fill_reconciled:{}", order_id)),
            ts_ms: 104_200,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some(condition_id.to_string()),
            token_id: Some(token_id.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL_RECONCILED".to_string(),
            price: Some(0.50),
            units: Some(units),
            notional_usd: Some(notional),
            pnl_usd: None,
            reason: Some(format!("{{\"order_id\":\"{}\"}}", order_id)),
        })?;

        let (fills_v2_n, fills_v2_usd, fills_v2_reconciled_n): (i64, f64, i64) =
            db.with_conn(|conn| {
                conn.query_row(
                    r#"
SELECT
    COUNT(*),
    COALESCE(SUM(notional_usd), 0.0),
    SUM(CASE WHEN source_event_type='ENTRY_FILL_RECONCILED' THEN 1 ELSE 0 END)
FROM fills_v2
WHERE strategy_id=?1 AND timeframe='5m' AND period_timestamp=?2 AND token_id=?3
"#,
                    params![STRATEGY_ID_TEST_PRIMARY, period as i64, token_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(Into::into)
            })?;
        assert_eq!(fills_v2_n, 1);
        assert_eq!(fills_v2_reconciled_n, 0);
        assert!((fills_v2_usd - notional).abs() < 1e-9);

        let (entry_units, entry_notional): (f64, f64) = db.with_conn(|conn| {
            conn.query_row(
                r#"
SELECT entry_units, entry_notional_usd
FROM positions_v2
WHERE strategy_id=?1 AND timeframe='5m' AND period_timestamp=?2 AND token_id=?3
"#,
                params![STRATEGY_ID_TEST_PRIMARY, period as i64, token_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(Into::into)
        })?;
        assert!((entry_units - units).abs() < 1e-9);
        assert!((entry_notional - notional).abs() < 1e-9);

        let (lifecycle_units, lifecycle_notional): (f64, f64) = db.with_conn(|conn| {
            conn.query_row(
                r#"
SELECT entry_units, entry_notional_usd
FROM trade_lifecycle_v1
WHERE strategy_id=?1 AND timeframe='5m' AND period_timestamp=?2 AND token_id=?3
"#,
                params![STRATEGY_ID_TEST_PRIMARY, period as i64, token_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(Into::into)
        })?;
        assert!((lifecycle_units - units).abs() < 1e-9);
        assert!((lifecycle_notional - notional).abs() < 1e-9);

        let (snapshot_rows, snapshot_notional): (i64, f64) = db.with_conn(|conn| {
            conn.query_row(
                r#"
SELECT COUNT(*), COALESCE(SUM(entry_notional_usd), 0.0)
FROM strategy_feature_snapshots_v1
WHERE strategy_id=?1 AND timeframe='5m' AND period_timestamp=?2 AND token_id=?3
"#,
                params![STRATEGY_ID_TEST_PRIMARY, period as i64, token_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(Into::into)
        })?;
        assert_eq!(snapshot_rows, 1);
        assert!((snapshot_notional - notional).abs() < 1e-9);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn entry_fill_reconciled_only_counts_once_when_canonical_missing() -> Result<()> {
        let path = temp_db_path("fill_reconciled_only_once");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_010_275_u64;
        let order_id = "order-fill-reconciled-only";
        let token_id = "token-fill-reconciled-only";
        let condition_id = "cond-fill-reconciled-only";
        let notional = 9.99_f64;
        let units = 19.98_f64;

        db.record_trade_event(&TradeEventRecord {
            event_key: Some(format!("entry_fill_reconciled:{}", order_id)),
            ts_ms: 104_300,
            period_timestamp: period,
            timeframe: "15m".to_string(),
            strategy_id: STRATEGY_ID_PREMARKET_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some(condition_id.to_string()),
            token_id: Some(token_id.to_string()),
            token_type: Some("BTC Down".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL_RECONCILED".to_string(),
            price: Some(0.50),
            units: Some(units),
            notional_usd: Some(notional),
            pnl_usd: None,
            reason: Some(format!("{{\"order_id\":\"{}\"}}", order_id)),
        })?;

        let (fills_v2_n, fills_v2_usd, fills_v2_reconciled_n): (i64, f64, i64) =
            db.with_conn(|conn| {
                conn.query_row(
                    r#"
SELECT
    COUNT(*),
    COALESCE(SUM(notional_usd), 0.0),
    SUM(CASE WHEN source_event_type='ENTRY_FILL_RECONCILED' THEN 1 ELSE 0 END)
FROM fills_v2
WHERE strategy_id=?1 AND timeframe='15m' AND period_timestamp=?2 AND token_id=?3
"#,
                    params![STRATEGY_ID_PREMARKET_V1, period as i64, token_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(Into::into)
            })?;
        assert_eq!(fills_v2_n, 1);
        assert_eq!(fills_v2_reconciled_n, 1);
        assert!((fills_v2_usd - notional).abs() < 1e-9);

        let (entry_units, entry_notional): (f64, f64) = db.with_conn(|conn| {
            conn.query_row(
                r#"
SELECT entry_units, entry_notional_usd
FROM positions_v2
WHERE strategy_id=?1 AND timeframe='15m' AND period_timestamp=?2 AND token_id=?3
"#,
                params![STRATEGY_ID_PREMARKET_V1, period as i64, token_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(Into::into)
        })?;
        assert!((entry_units - units).abs() < 1e-9);
        assert!((entry_notional - notional).abs() < 1e-9);

        let (lifecycle_units, lifecycle_notional): (f64, f64) = db.with_conn(|conn| {
            conn.query_row(
                r#"
SELECT entry_units, entry_notional_usd
FROM trade_lifecycle_v1
WHERE strategy_id=?1 AND timeframe='15m' AND period_timestamp=?2 AND token_id=?3
"#,
                params![STRATEGY_ID_PREMARKET_V1, period as i64, token_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(Into::into)
        })?;
        assert!((lifecycle_units - units).abs() < 1e-9);
        assert!((lifecycle_notional - notional).abs() < 1e-9);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn entry_fill_strategy_id_matches_pending_order_strategy() -> Result<()> {
        let path = temp_db_path("fill_strategy_match");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_010_300_u64;
        let order_id = "order-fill-strategy-match";

        db.upsert_pending_order(&PendingOrderRecord {
            order_id: order_id.to_string(),
            trade_key: "trade-fill-strategy-match".to_string(),
            token_id: "token-fill-strategy-match".to_string(),
            condition_id: Some("cond-fill-strategy-match".to_string()),
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_PREMARKET_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            entry_mode: "ladder".to_string(),
            period_timestamp: period,
            price: 0.30,
            size_usd: 12.0,
            side: "BUY".to_string(),
            status: "OPEN".to_string(),
        })?;

        db.mark_pending_order_filled_with_event(
            order_id,
            &TradeEventRecord {
                event_key: Some(format!("entry_fill_order:{}", order_id)),
                ts_ms: 105_000,
                period_timestamp: period,
                timeframe: "5m".to_string(),
                strategy_id: STRATEGY_ID_PREMARKET_V1.to_string(),
                asset_symbol: Some("BTC".to_string()),
                condition_id: Some("cond-fill-strategy-match".to_string()),
                token_id: Some("token-fill-strategy-match".to_string()),
                token_type: Some("BTC Up".to_string()),
                side: Some("BUY".to_string()),
                event_type: "ENTRY_FILL".to_string(),
                price: Some(0.30),
                units: Some(40.0),
                notional_usd: Some(12.0),
                pnl_usd: None,
                reason: Some(format!("{{\"order_id\":\"{}\"}}", order_id)),
            },
        )?;

        let strategy_ids: (String, String) = db.with_conn(|conn| {
            let event_strategy: String = conn.query_row(
                "SELECT strategy_id FROM trade_events WHERE event_key=?1",
                params![format!("entry_fill_order:{}", order_id)],
                |row| row.get(0),
            )?;
            let pending_strategy: String = conn.query_row(
                "SELECT strategy_id FROM pending_orders WHERE order_id=?1",
                params![order_id],
                |row| row.get(0),
            )?;
            Ok((event_strategy, pending_strategy))
        })?;
        assert_eq!(strategy_ids.0, STRATEGY_ID_PREMARKET_V1);
        assert_eq!(strategy_ids.1, STRATEGY_ID_PREMARKET_V1);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn entry_fill_inherits_pending_run_and_cohort() -> Result<()> {
        let path = temp_db_path("fill_run_cohort_inherit");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_010_350_u64;
        let order_id = "order-fill-run-cohort";

        db.upsert_pending_order(&PendingOrderRecord {
            order_id: order_id.to_string(),
            trade_key: "trade-fill-run-cohort".to_string(),
            token_id: "token-fill-run-cohort".to_string(),
            condition_id: Some("cond-fill-run-cohort".to_string()),
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            entry_mode: "reactive".to_string(),
            period_timestamp: period,
            price: 0.44,
            size_usd: 11.0,
            side: "BUY".to_string(),
            status: "OPEN".to_string(),
        })?;
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE pending_orders SET run_id='run_test_123', strategy_cohort='cohort_test' WHERE order_id=?1",
                params![order_id],
            )?;
            Ok(())
        })?;

        db.mark_pending_order_filled_with_event(
            order_id,
            &TradeEventRecord {
                event_key: Some(format!("entry_fill_order:{}", order_id)),
                ts_ms: 105_500,
                period_timestamp: period,
                timeframe: "5m".to_string(),
                strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
                asset_symbol: Some("BTC".to_string()),
                condition_id: Some("cond-fill-run-cohort".to_string()),
                token_id: Some("token-fill-run-cohort".to_string()),
                token_type: Some("BTC Up".to_string()),
                side: Some("BUY".to_string()),
                event_type: "ENTRY_FILL".to_string(),
                price: Some(0.44),
                units: Some(25.0),
                notional_usd: Some(11.0),
                pnl_usd: None,
                reason: Some(format!("{{\"order_id\":\"{}\"}}", order_id)),
            },
        )?;

        let tags: (String, String) = db.with_conn(|conn| {
            conn.query_row(
                "SELECT run_id, strategy_cohort FROM trade_events WHERE event_key=?1",
                params![format!("entry_fill_order:{}", order_id)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(Into::into)
        })?;
        assert_eq!(tags.0, "run_test_123");
        assert_eq!(tags.1, "cohort_test");

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn latest_entry_fills_include_strategy_id() -> Result<()> {
        let path = temp_db_path("latest_entry_fill_strategy");
        let db = TrackingDb::new(&path)?;

        db.record_trade_event(&TradeEventRecord {
            event_key: Some("entry_fill_order:latest-entry-strategy".to_string()),
            ts_ms: 106_000,
            period_timestamp: 1_700_010_400,
            timeframe: "15m".to_string(),
            strategy_id: STRATEGY_ID_TEST_SECONDARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-latest-entry-strategy".to_string()),
            token_id: Some("token-latest-entry-strategy".to_string()),
            token_type: Some("BTC Down".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.25),
            units: Some(20.0),
            notional_usd: Some(5.0),
            pnl_usd: None,
            reason: Some("{\"mode\":\"runtime_fill\"}".to_string()),
        })?;

        let latest = db.list_latest_entry_fills(10)?;
        let row = latest
            .iter()
            .find(|r| r.token_id == "token-latest-entry-strategy")
            .cloned()
            .expect("latest fill row should exist");
        assert_eq!(row.strategy_id, STRATEGY_ID_TEST_SECONDARY);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn open_positions_for_restore_prefers_unsettled_open_inventory() -> Result<()> {
        let path = temp_db_path("open_positions_restore_candidates");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_010_500_u64;
        let condition_id = "cond-open-restore";
        let token_id = "token-open-restore";
        let event_key = "entry_fill_order:open-restore";

        db.record_trade_event(&TradeEventRecord {
            event_key: Some(event_key.to_string()),
            ts_ms: 106_500,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_PREMARKET_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some(condition_id.to_string()),
            token_id: Some(token_id.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.30),
            units: Some(40.0),
            notional_usd: Some(12.0),
            pnl_usd: None,
            reason: Some("{\"mode\":\"runtime_fill\"}".to_string()),
        })?;

        let restore_rows = db.list_open_positions_for_restore(20)?;
        let row = restore_rows
            .iter()
            .find(|r| r.token_id == token_id)
            .cloned()
            .expect("open position should be available for restore");
        assert_eq!(row.strategy_id, STRATEGY_ID_PREMARKET_V1);
        assert_eq!(row.condition_id, condition_id);
        assert_eq!(row.timeframe, "5m");

        // Once market-close EXIT is recorded, row should no longer be returned.
        db.record_trade_event(&TradeEventRecord {
            event_key: Some("exit_market_close:open-restore".to_string()),
            ts_ms: 107_000,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_PREMARKET_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some(condition_id.to_string()),
            token_id: Some(token_id.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("SELL".to_string()),
            event_type: "EXIT".to_string(),
            price: Some(1.0),
            units: Some(40.0),
            notional_usd: Some(40.0),
            pnl_usd: Some(28.0),
            reason: Some("production_market_close".to_string()),
        })?;

        let restore_rows_after_exit = db.list_open_positions_for_restore(20)?;
        assert!(
            restore_rows_after_exit
                .iter()
                .all(|r| r.token_id != token_id),
            "settled inventory must not be re-restored"
        );

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn redemption_candidates_for_restore_include_closed_claimable_rows() -> Result<()> {
        let path = temp_db_path("redemption_restore_candidates");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_010_550_u64;
        let condition_id = "cond-redemption-restore";
        let token_id = "token-redemption-restore";
        let strategy_id = STRATEGY_ID_TEST_PRIMARY;

        db.record_trade_event(&TradeEventRecord {
            event_key: Some("entry_fill_order:redemption-restore".to_string()),
            ts_ms: 107_500,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: strategy_id.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some(condition_id.to_string()),
            token_id: Some(token_id.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.40),
            units: Some(25.0),
            notional_usd: Some(10.0),
            pnl_usd: None,
            reason: Some("{\"mode\":\"runtime_fill\"}".to_string()),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some("exit_market_close:redemption-restore".to_string()),
            ts_ms: 107_900,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: strategy_id.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some(condition_id.to_string()),
            token_id: Some(token_id.to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("SELL".to_string()),
            event_type: "EXIT".to_string(),
            price: Some(1.0),
            units: Some(25.0),
            notional_usd: Some(25.0),
            pnl_usd: Some(15.0),
            reason: Some("production_market_close".to_string()),
        })?;
        db.update_trade_lifecycle_redemption_state(
            strategy_id,
            "5m",
            period,
            Some(condition_id),
            token_id,
            "pending",
            None,
            Some(0),
            None,
            Some("unit_test_pending_claimable"),
        )?;

        let open_rows = db.list_open_positions_for_restore(20)?;
        assert!(
            open_rows.iter().all(|row| row.token_id != token_id),
            "closed positions should not appear in open restore rows"
        );

        let redemption_rows = db.list_redemption_candidates_for_restore(20)?;
        let row = redemption_rows
            .iter()
            .find(|r| r.token_id == token_id)
            .cloned()
            .expect("claimable closed row should be returned for redemption restore");
        assert_eq!(row.strategy_id, strategy_id);
        assert_eq!(row.timeframe, "5m");
        assert_eq!(row.condition_id, condition_id);
        assert_eq!(row.token_type.as_deref(), Some("BTC Up"));

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn reporting_scope_prod_only_excludes_dryrun_events() -> Result<()> {
        let path = temp_db_path("reporting_scope_prod_only");
        let db = TrackingDb::new(&path)?;

        db.record_trade_event(&TradeEventRecord {
            event_key: Some("entry_fill_order:reporting-prod".to_string()),
            ts_ms: 108_000,
            period_timestamp: 1_700_010_600,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-reporting-prod".to_string()),
            token_id: Some("token-reporting-prod".to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.55),
            units: Some(10.0),
            notional_usd: Some(5.5),
            pnl_usd: None,
            reason: Some("runtime_fill".to_string()),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some("entry_fill_order:reporting-dryrun".to_string()),
            ts_ms: 108_100,
            period_timestamp: 1_700_010_605,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-reporting-dryrun".to_string()),
            token_id: Some("token-reporting-dryrun".to_string()),
            token_type: Some("BTC Down".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.45),
            units: Some(10.0),
            notional_usd: Some(4.5),
            pnl_usd: None,
            reason: Some("runtime_fill".to_string()),
        })?;
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE trade_events SET run_id='sim_test', strategy_cohort='dryrun' WHERE event_key='entry_fill_order:reporting-dryrun'",
                [],
            )?;
            Ok(())
        })?;

        let prod_scope = ReportingScope {
            from_ts_ms: None,
            to_ts_ms: None,
            include_legacy_default: true,
            prod_only: true,
        };
        let prod_rows =
            db.summarize_entry_fills_canonical(Some(STRATEGY_ID_TEST_PRIMARY), Some(&prod_scope))?;
        let prod_fills = prod_rows.iter().map(|row| row.fills).sum::<u64>();
        assert_eq!(prod_fills, 1);

        let all_scope = ReportingScope {
            from_ts_ms: None,
            to_ts_ms: None,
            include_legacy_default: true,
            prod_only: false,
        };
        let all_rows =
            db.summarize_entry_fills_canonical(Some(STRATEGY_ID_TEST_PRIMARY), Some(&all_scope))?;
        let all_fills = all_rows.iter().map(|row| row.fills).sum::<u64>();
        assert_eq!(all_fills, 2);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn outcome_closed_trade_count_respects_prod_only_filter() -> Result<()> {
        let path = temp_db_path("outcome_prod_only_closed_count");
        let db = TrackingDb::new(&path)?;
        let strategy = STRATEGY_ID_TEST_PRIMARY;

        db.record_trade_event(&TradeEventRecord {
            event_key: Some("exit:outcome-prod".to_string()),
            ts_ms: 109_000,
            period_timestamp: 1_700_010_700,
            timeframe: "5m".to_string(),
            strategy_id: strategy.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-outcome-prod".to_string()),
            token_id: Some("token-outcome-prod".to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("SELL".to_string()),
            event_type: "EXIT".to_string(),
            price: Some(1.0),
            units: Some(10.0),
            notional_usd: Some(10.0),
            pnl_usd: Some(2.0),
            reason: Some("manual_exit".to_string()),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some("exit:outcome-dryrun".to_string()),
            ts_ms: 109_100,
            period_timestamp: 1_700_010_705,
            timeframe: "5m".to_string(),
            strategy_id: strategy.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-outcome-dryrun".to_string()),
            token_id: Some("token-outcome-dryrun".to_string()),
            token_type: Some("BTC Down".to_string()),
            side: Some("SELL".to_string()),
            event_type: "EXIT".to_string(),
            price: Some(0.0),
            units: Some(10.0),
            notional_usd: Some(0.0),
            pnl_usd: Some(-4.0),
            reason: Some("manual_exit".to_string()),
        })?;
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE trade_events SET run_id='sim_test', strategy_cohort='dryrun' WHERE event_key='exit:outcome-dryrun'",
                [],
            )?;
            Ok(())
        })?;

        unsafe { std::env::set_var("EVPOLY_REPORTING_PROD_ONLY", "true") };
        let prod_only_count =
            db.total_closed_trades_for_strategy_with_source(strategy, OutcomeSource::Exit)?;
        unsafe { std::env::set_var("EVPOLY_REPORTING_PROD_ONLY", "false") };
        let all_count =
            db.total_closed_trades_for_strategy_with_source(strategy, OutcomeSource::Exit)?;
        unsafe { std::env::remove_var("EVPOLY_REPORTING_PROD_ONLY") };

        assert_eq!(prod_only_count, 1);
        assert_eq!(all_count, 2);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn snapback_feature_snapshot_intent_upsert_is_idempotent() -> Result<()> {
        let path = temp_db_path("snapback_snapshot_intent_upsert");
        let db = TrackingDb::new(&path)?;

        let key_period = 1_700_020_100_u64;
        let mut row = SnapbackFeatureSnapshotIntentRecord {
            strategy_id: STRATEGY_ID_TEST_TERTIARY.to_string(),
            timeframe: "15m".to_string(),
            period_timestamp: key_period,
            condition_id: "cond-snapback-1".to_string(),
            token_id: "token-snapback-1".to_string(),
            impulse_id: "15m:100:down:1:2".to_string(),
            position_key: None,
            entry_side: Some("UP".to_string()),
            intent_ts_ms: 200_000,
            request_id: Some("snapback:req:1".to_string()),
            order_id: None,
            signed_notional_30s: Some(-120_000.0),
            signed_notional_300s: Some(-820_000.0),
            signed_notional_900s: Some(-1_100_000.0),
            burst_zscore: Some(2.4),
            side_imbalance_5m: Some(-0.62),
            impulse_strength: 0.91,
            impulse_mode: Some("follow".to_string()),
            impulse_size_multiplier: Some(1.0885714285714285),
            direction_policy_split: Some(0.60),
            trigger_signed_flow: Some(-820_000.0),
            trigger_flow_source: Some("binance".to_string()),
            fair_probability: Some(0.58),
            reservation_price: Some(0.53),
            max_price: Some(0.51),
            limit_price: Some(0.50),
            edge_bps: Some(24.0),
            target_size_usd: Some(60.0),
            bias_alignment: Some("aligned".to_string()),
            bias_confidence: Some(0.73),
        };
        db.upsert_snapback_feature_snapshot_intent(&row)?;

        // Same unique key should update row, not insert a second one.
        row.impulse_strength = 1.07;
        row.limit_price = Some(0.49);
        row.request_id = None;
        db.upsert_snapback_feature_snapshot_intent(&row)?;

        let (count, impulse_strength, request_id, limit_price): (i64, f64, String, f64) =
            db.with_conn(|conn| {
                conn.query_row(
                    r#"
SELECT COUNT(*), COALESCE(impulse_strength, 0.0), COALESCE(request_id, ''), COALESCE(limit_price, 0.0)
FROM snapback_feature_snapshots_v1
WHERE strategy_id=?1 AND timeframe='15m' AND period_timestamp=?2 AND token_id='token-snapback-1' AND impulse_id='15m:100:down:1:2'
"#,
                    params![STRATEGY_ID_TEST_TERTIARY, key_period as i64],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .map_err(Into::into)
            })?;
        assert_eq!(count, 1);
        assert!((impulse_strength - 1.07).abs() < 1e-9);
        // COALESCE on upsert keeps original non-empty request_id when second write omits it.
        assert_eq!(request_id, "snapback:req:1");
        assert!((limit_price - 0.49).abs() < 1e-9);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn snapback_legacy_rows_backfill_into_unified_snapshot_on_open() -> Result<()> {
        let path = temp_db_path("snapback_backfill_unified");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_022_200_u64;

        db.with_conn(|conn| {
            conn.execute(
                r#"
INSERT INTO snapback_feature_snapshots_v1 (
    strategy_id, timeframe, period_timestamp, condition_id, token_id, impulse_id, position_key, entry_side,
    intent_ts_ms, request_id, order_id,
    signed_notional_30s, signed_notional_300s, signed_notional_900s,
    burst_zscore, side_imbalance_5m, impulse_strength,
    trigger_signed_flow, trigger_flow_source,
    fair_probability, reservation_price, max_price, limit_price, edge_bps, target_size_usd,
    bias_alignment, bias_confidence,
    fill_ts_ms, exit_ts_ms, settled_ts_ms, exit_reason, realized_pnl_usd, settled_pnl_usd, hold_ms,
    created_at_ms, updated_at_ms
) VALUES (
    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
    ?9, ?10, ?11,
    ?12, ?13, ?14,
    ?15, ?16, ?17,
    ?18, ?19,
    ?20, ?21, ?22, ?23, ?24, ?25,
    ?26, ?27,
    ?28, ?29, ?30, ?31, ?32, ?33, ?34,
    ?35, ?36
)
"#,
                params![
                    STRATEGY_ID_TEST_TERTIARY,
                    "15m",
                    period as i64,
                    "cond-snapback-backfill",
                    "token-snapback-backfill",
                    "15m:backfill:down:1:1",
                    Option::<String>::None,
                    Some("UP".to_string()),
                    600_000_i64,
                    Some("snapback:req:backfill".to_string()),
                    Some("ord-snapback-backfill".to_string()),
                    Some(-210_000.0_f64),
                    Some(-1_420_000.0_f64),
                    Some(-2_400_000.0_f64),
                    Some(3.1_f64),
                    Some(-0.71_f64),
                    1.18_f64,
                    Some(-1_420_000.0_f64),
                    Some("binance".to_string()),
                    Some(0.61_f64),
                    Some(0.56_f64),
                    Some(0.54_f64),
                    Some(0.53_f64),
                    Some(28.0_f64),
                    Some(70.0_f64),
                    Some("aligned".to_string()),
                    Some(0.72_f64),
                    Some(601_000_i64),
                    Some(620_000_i64),
                    Some(621_000_i64),
                    Some("production_market_close".to_string()),
                    Some(12.0_f64),
                    Some(12.0_f64),
                    Some(21_000_i64),
                    600_000_i64,
                    621_000_i64,
                ],
            )?;
            conn.execute(
                "DELETE FROM strategy_feature_snapshots_v1 WHERE strategy_id=?1 AND period_timestamp=?2 AND token_id=?3",
                params![STRATEGY_ID_TEST_TERTIARY, period as i64, "token-snapback-backfill"],
            )?;
            Ok(())
        })?;

        drop(db);
        let db = TrackingDb::new(&path)?;
        let row: (i64, String, String, i64) = db.with_conn(|conn| {
            conn.query_row(
                r#"
SELECT COUNT(*), COALESCE(intent_status,''), COALESCE(execution_status,''), COALESCE(settled_ts_ms,0)
FROM strategy_feature_snapshots_v1
WHERE strategy_id=?1
  AND timeframe='15m'
  AND period_timestamp=?2
  AND token_id='token-snapback-backfill'
  AND impulse_id='15m:backfill:down:1:1'
"#,
                params![STRATEGY_ID_TEST_TERTIARY, period as i64],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(Into::into)
        })?;
        assert_eq!(row.0, 1);
        assert_eq!(row.1, "submitted");
        assert_eq!(row.2, "settled");
        assert_eq!(row.3, 621_000);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn snapback_feature_snapshot_outcome_patch_updates_latest_for_trade() -> Result<()> {
        let path = temp_db_path("snapback_snapshot_patch_latest");
        let db = TrackingDb::new(&path)?;

        let period = 1_700_020_200_u64;
        let base = SnapbackFeatureSnapshotIntentRecord {
            strategy_id: STRATEGY_ID_TEST_TERTIARY.to_string(),
            timeframe: "5m".to_string(),
            period_timestamp: period,
            condition_id: "cond-snapback-2".to_string(),
            token_id: "token-snapback-2".to_string(),
            impulse_id: "5m:500:down:1:1".to_string(),
            position_key: None,
            entry_side: Some("UP".to_string()),
            intent_ts_ms: 300_000,
            request_id: Some("snapback:req:a".to_string()),
            order_id: None,
            signed_notional_30s: Some(-510_000.0),
            signed_notional_300s: Some(-600_000.0),
            signed_notional_900s: Some(-700_000.0),
            burst_zscore: Some(2.1),
            side_imbalance_5m: Some(-0.42),
            impulse_strength: 0.66,
            impulse_mode: Some("follow".to_string()),
            impulse_size_multiplier: Some(1.0171428571428571),
            direction_policy_split: Some(0.60),
            trigger_signed_flow: Some(-510_000.0),
            trigger_flow_source: Some("binance".to_string()),
            fair_probability: Some(0.56),
            reservation_price: Some(0.50),
            max_price: Some(0.48),
            limit_price: Some(0.47),
            edge_bps: Some(18.0),
            target_size_usd: Some(40.0),
            bias_alignment: Some("aligned".to_string()),
            bias_confidence: Some(0.61),
        };
        db.upsert_snapback_feature_snapshot_intent(&base)?;

        let mut newer = base.clone();
        newer.impulse_id = "5m:501:down:1:2".to_string();
        newer.intent_ts_ms = 301_000;
        newer.request_id = Some("snapback:req:b".to_string());
        db.upsert_snapback_feature_snapshot_intent(&newer)?;

        let updated = db.patch_latest_snapback_feature_snapshot_outcome_for_trade(
            STRATEGY_ID_TEST_TERTIARY,
            "5m",
            period,
            "token-snapback-2",
            &SnapbackFeatureSnapshotOutcomePatch {
                order_id: Some("ord-snapback-2".to_string()),
                fill_ts_ms: Some(302_000),
                ..Default::default()
            },
        )?;
        assert!(updated);

        let patched_rows: (i64, i64, i64, i64, i64) = db.with_conn(|conn| {
            conn.query_row(
                r#"
SELECT
    COALESCE((SELECT fill_ts_ms FROM snapback_feature_snapshots_v1 WHERE impulse_id='5m:500:down:1:1'), -1),
    COALESCE((SELECT fill_ts_ms FROM snapback_feature_snapshots_v1 WHERE impulse_id='5m:501:down:1:2'), -1),
    COALESCE((SELECT COUNT(*) FROM snapback_feature_snapshots_v1 WHERE order_id='ord-snapback-2'), 0),
    COALESCE((SELECT submit_ts_ms FROM strategy_feature_snapshots_v1 WHERE strategy_id=?1 AND timeframe='5m' AND period_timestamp=?2 AND token_id='token-snapback-2' AND impulse_id='5m:501:down:1:2'), -1),
    COALESCE((SELECT ack_ts_ms FROM strategy_feature_snapshots_v1 WHERE strategy_id=?1 AND timeframe='5m' AND period_timestamp=?2 AND token_id='token-snapback-2' AND impulse_id='5m:501:down:1:2'), -1)
"#,
                params![STRATEGY_ID_TEST_TERTIARY, period as i64],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .map_err(Into::into)
        })?;
        assert_eq!(patched_rows.0, -1);
        assert_eq!(patched_rows.1, 302_000);
        assert_eq!(patched_rows.2, 1);
        assert_eq!(patched_rows.3, -1);
        assert_eq!(patched_rows.4, -1);

        let updated_by_key = db.patch_snapback_feature_snapshot_outcome_by_key(
            &SnapbackFeatureSnapshotKey {
                strategy_id: STRATEGY_ID_TEST_TERTIARY.to_string(),
                timeframe: "5m".to_string(),
                period_timestamp: period,
                token_id: "token-snapback-2".to_string(),
                impulse_id: "5m:501:down:1:2".to_string(),
            },
            &SnapbackFeatureSnapshotOutcomePatch {
                exit_ts_ms: Some(320_000),
                settled_ts_ms: Some(321_000),
                exit_reason: Some("production_market_close".to_string()),
                realized_pnl_usd: Some(6.5),
                settled_pnl_usd: Some(6.5),
                hold_ms: Some(19_000),
                ..Default::default()
            },
        )?;
        assert!(updated_by_key);

        let final_row: (i64, i64, String, f64, f64, i64) = db.with_conn(|conn| {
            conn.query_row(
                r#"
SELECT
    COALESCE(exit_ts_ms, -1),
    COALESCE(settled_ts_ms, -1),
    COALESCE(exit_reason, ''),
    COALESCE(realized_pnl_usd, 0.0),
    COALESCE(settled_pnl_usd, 0.0),
    COALESCE(hold_ms, 0)
FROM snapback_feature_snapshots_v1
WHERE strategy_id=?1 AND timeframe='5m' AND period_timestamp=?2 AND token_id='token-snapback-2' AND impulse_id='5m:501:down:1:2'
"#,
                params![STRATEGY_ID_TEST_TERTIARY, period as i64],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .map_err(Into::into)
        })?;
        assert_eq!(final_row.0, 320_000);
        assert_eq!(final_row.1, 321_000);
        assert_eq!(final_row.2, "production_market_close");
        assert!((final_row.3 - 6.5).abs() < 1e-9);
        assert!((final_row.4 - 6.5).abs() < 1e-9);
        assert_eq!(final_row.5, 19_000);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn snapback_feature_bin_summary_aggregates_outcomes() -> Result<()> {
        let path = temp_db_path("snapback_feature_bins");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_020_300_u64;

        let mut row = SnapbackFeatureSnapshotIntentRecord {
            strategy_id: STRATEGY_ID_TEST_TERTIARY.to_string(),
            timeframe: "15m".to_string(),
            period_timestamp: period,
            condition_id: "cond-snapback-bins".to_string(),
            token_id: "token-snapback-bins".to_string(),
            impulse_id: "15m:900:down:2:1".to_string(),
            position_key: None,
            entry_side: Some("UP".to_string()),
            intent_ts_ms: 500_000,
            request_id: None,
            order_id: None,
            signed_notional_30s: Some(-120_000.0),
            signed_notional_300s: Some(-1_100_000.0),
            signed_notional_900s: Some(-2_200_000.0),
            burst_zscore: Some(3.2),
            side_imbalance_5m: Some(-0.72),
            impulse_strength: 1.12,
            impulse_mode: Some("follow".to_string()),
            impulse_size_multiplier: Some(1.1485714285714286),
            direction_policy_split: Some(0.60),
            trigger_signed_flow: Some(-1_100_000.0),
            trigger_flow_source: Some("binance".to_string()),
            fair_probability: Some(0.59),
            reservation_price: Some(0.54),
            max_price: Some(0.52),
            limit_price: Some(0.51),
            edge_bps: Some(26.0),
            target_size_usd: Some(55.0),
            bias_alignment: Some("aligned".to_string()),
            bias_confidence: Some(0.66),
        };
        db.upsert_snapback_feature_snapshot_intent(&row)?;
        db.patch_snapback_feature_snapshot_outcome_by_key(
            &SnapbackFeatureSnapshotKey {
                strategy_id: STRATEGY_ID_TEST_TERTIARY.to_string(),
                timeframe: "15m".to_string(),
                period_timestamp: period,
                token_id: "token-snapback-bins".to_string(),
                impulse_id: "15m:900:down:2:1".to_string(),
            },
            &SnapbackFeatureSnapshotOutcomePatch {
                exit_ts_ms: Some(520_000),
                settled_ts_ms: Some(521_000),
                realized_pnl_usd: Some(7.25),
                settled_pnl_usd: Some(7.25),
                ..Default::default()
            },
        )?;

        row.impulse_id = "15m:901:down:2:2".to_string();
        row.intent_ts_ms = 501_000;
        row.signed_notional_300s = Some(-900_000.0);
        row.burst_zscore = Some(2.2);
        row.side_imbalance_5m = Some(-0.35);
        row.impulse_strength = 0.62;
        db.upsert_snapback_feature_snapshot_intent(&row)?;
        db.patch_snapback_feature_snapshot_outcome_by_key(
            &SnapbackFeatureSnapshotKey {
                strategy_id: STRATEGY_ID_TEST_TERTIARY.to_string(),
                timeframe: "15m".to_string(),
                period_timestamp: period,
                token_id: "token-snapback-bins".to_string(),
                impulse_id: "15m:901:down:2:2".to_string(),
            },
            &SnapbackFeatureSnapshotOutcomePatch {
                exit_ts_ms: Some(522_000),
                settled_ts_ms: Some(523_000),
                realized_pnl_usd: Some(-3.0),
                settled_pnl_usd: Some(-3.0),
                ..Default::default()
            },
        )?;

        let bins = db.summarize_snapback_feature_bins_for_strategy(STRATEGY_ID_TEST_TERTIARY)?;
        assert!(!bins.is_empty());
        let impulse_bins: Vec<_> = bins
            .iter()
            .filter(|row| row.timeframe == "15m" && row.feature == "impulse_strength")
            .collect();
        assert!(!impulse_bins.is_empty());
        let total_samples = impulse_bins.iter().map(|row| row.samples).sum::<u64>();
        assert_eq!(total_samples, 2);
        let total_net = impulse_bins.iter().map(|row| row.net_pnl_usd).sum::<f64>();
        assert!((total_net - 4.25).abs() < 1e-9);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn strategy_feature_bin_summary_aggregates_unified_snapshot_rows() -> Result<()> {
        let path = temp_db_path("strategy_feature_bins_unified");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_020_400_u64;
        let strategy_id = STRATEGY_ID_TEST_PRIMARY.to_string();

        db.upsert_strategy_feature_snapshot_intent(&StrategyFeatureSnapshotIntentRecord {
            strategy_id: strategy_id.clone(),
            timeframe: "5m".to_string(),
            period_timestamp: period,
            market_key: Some("btc".to_string()),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-reactive-bins".to_string()),
            token_id: "token-reactive-bins".to_string(),
            impulse_id: "reactive:5m:bin:1".to_string(),
            decision_id: Some("decision-reactive-bins".to_string()),
            rung_id: None,
            slice_id: None,
            request_id: Some("req-reactive-bins-1".to_string()),
            order_id: Some("ord-reactive-bins-1".to_string()),
            trade_key: Some("trade-reactive-bins".to_string()),
            position_key: None,
            intent_status: Some("submitted".to_string()),
            execution_status: Some("settled".to_string()),
            intent_ts_ms: 100_000,
            submit_ts_ms: Some(100_000),
            ack_ts_ms: Some(100_100),
            fill_ts_ms: Some(100_200),
            exit_ts_ms: Some(100_500),
            settled_ts_ms: Some(100_600),
            entry_notional_usd: Some(25.0),
            exit_notional_usd: Some(27.0),
            realized_pnl_usd: Some(2.0),
            settled_pnl_usd: Some(2.0),
            hold_ms: Some(600),
            feature_json: Some(
                r#"{"expected_edge_bps":18.0,"expected_fill_prob":0.72,"decision_confidence":0.61}"#
                    .to_string(),
            ),
            decision_context_json: None,
            execution_context_json: Some(
                r#"{"friction":"direction_lock_reject","event":"premarket_open_plus_5_cancel"}"#
                    .to_string(),
            ),
        })?;

        db.upsert_strategy_feature_snapshot_intent(&StrategyFeatureSnapshotIntentRecord {
            strategy_id: strategy_id.clone(),
            timeframe: "5m".to_string(),
            period_timestamp: period,
            market_key: Some("btc".to_string()),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-reactive-bins".to_string()),
            token_id: "token-reactive-bins".to_string(),
            impulse_id: "reactive:5m:bin:2".to_string(),
            decision_id: Some("decision-reactive-bins".to_string()),
            rung_id: None,
            slice_id: None,
            request_id: Some("req-reactive-bins-2".to_string()),
            order_id: Some("ord-reactive-bins-2".to_string()),
            trade_key: Some("trade-reactive-bins".to_string()),
            position_key: None,
            intent_status: Some("submitted".to_string()),
            execution_status: Some("settled".to_string()),
            intent_ts_ms: 101_000,
            submit_ts_ms: Some(101_000),
            ack_ts_ms: Some(101_100),
            fill_ts_ms: Some(101_200),
            exit_ts_ms: Some(101_500),
            settled_ts_ms: Some(101_600),
            entry_notional_usd: Some(25.0),
            exit_notional_usd: Some(22.0),
            realized_pnl_usd: Some(-3.0),
            settled_pnl_usd: Some(-3.0),
            hold_ms: Some(600),
            feature_json: Some(
                r#"{"expected_edge_bps":6.0,"expected_fill_prob":0.28,"decision_confidence":0.31}"#
                    .to_string(),
            ),
            decision_context_json: None,
            execution_context_json: Some(r#"{"friction":"direction_lock_reject"}"#.to_string()),
        })?;

        let bins = db.summarize_strategy_feature_bins_for_strategy_with_source(
            strategy_id.as_str(),
            OutcomeSource::Settlement,
        )?;
        assert!(!bins.is_empty());
        assert!(bins
            .iter()
            .any(|row| row.timeframe == "5m" && row.feature == "expected_edge_bps"));
        assert!(bins
            .iter()
            .any(|row| row.timeframe == "5m" && row.feature == "friction"));

        let total_samples = bins
            .iter()
            .filter(|row| row.timeframe == "5m" && row.feature == "expected_edge_bps")
            .map(|row| row.samples)
            .sum::<u64>();
        assert_eq!(total_samples, 2);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn rollout_strategy_gate_summary_reports_failure_ratio_and_storm_groups() -> Result<()> {
        let path = temp_db_path("rollout_strategy_gate_summary");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_030_000_u64;
        let now_ms = chrono::Utc::now().timestamp_millis();

        for i in 0..6_u64 {
            db.record_trade_event(&TradeEventRecord {
                event_key: Some(format!("reactive_submit_{i}")),
                ts_ms: now_ms,
                period_timestamp: period,
                timeframe: "5m".to_string(),
                strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
                asset_symbol: Some("BTC".to_string()),
                condition_id: Some("cond-reactive".to_string()),
                token_id: Some("token-reactive".to_string()),
                token_type: Some("BTC_UP".to_string()),
                side: Some("BUY".to_string()),
                event_type: "ENTRY_SUBMIT".to_string(),
                price: Some(0.47),
                units: Some(10.0),
                notional_usd: Some(4.7),
                pnl_usd: None,
                reason: None,
            })?;
        }
        for i in 0..2_u64 {
            db.record_trade_event(&TradeEventRecord {
                event_key: Some(format!("reactive_fail_{i}")),
                ts_ms: now_ms,
                period_timestamp: period,
                timeframe: "5m".to_string(),
                strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
                asset_symbol: Some("BTC".to_string()),
                condition_id: Some("cond-reactive".to_string()),
                token_id: Some("token-reactive".to_string()),
                token_type: Some("BTC_UP".to_string()),
                side: Some("BUY".to_string()),
                event_type: "ENTRY_FAILED".to_string(),
                price: None,
                units: None,
                notional_usd: None,
                pnl_usd: None,
                reason: Some("simulated failure".to_string()),
            })?;
        }
        db.record_trade_event(&TradeEventRecord {
            event_key: Some("liqaccum_submit_1".to_string()),
            ts_ms: now_ms,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_SECONDARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-liqaccum".to_string()),
            token_id: Some("token-liqaccum".to_string()),
            token_type: Some("BTC_UP".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_SUBMIT".to_string(),
            price: Some(0.45),
            units: Some(11.0),
            notional_usd: Some(4.95),
            pnl_usd: None,
            reason: None,
        })?;

        for idx in 0..4_u64 {
            db.upsert_strategy_feature_snapshot_intent(&StrategyFeatureSnapshotIntentRecord {
                strategy_id: STRATEGY_ID_TEST_SECONDARY.to_string(),
                timeframe: "5m".to_string(),
                period_timestamp: period,
                market_key: Some("btc".to_string()),
                asset_symbol: Some("BTC".to_string()),
                condition_id: Some("cond-liqaccum".to_string()),
                token_id: "token-liqaccum".to_string(),
                impulse_id: format!("liqaccum_plan:5m:{period}:plan{idx}"),
                decision_id: None,
                rung_id: None,
                slice_id: None,
                request_id: None,
                order_id: None,
                trade_key: None,
                position_key: None,
                intent_status: Some("planned".to_string()),
                execution_status: Some("none".to_string()),
                intent_ts_ms: now_ms + i64::try_from(idx).unwrap_or_default(),
                submit_ts_ms: None,
                ack_ts_ms: None,
                fill_ts_ms: None,
                exit_ts_ms: None,
                settled_ts_ms: None,
                entry_notional_usd: Some(20.0 + idx as f64),
                exit_notional_usd: None,
                realized_pnl_usd: None,
                settled_pnl_usd: None,
                hold_ms: None,
                feature_json: None,
                decision_context_json: None,
                execution_context_json: None,
            })?;
        }
        for idx in 0..2_u64 {
            db.upsert_strategy_feature_snapshot_intent(&StrategyFeatureSnapshotIntentRecord {
                strategy_id: STRATEGY_ID_TEST_SECONDARY.to_string(),
                timeframe: "5m".to_string(),
                period_timestamp: period,
                market_key: Some("btc".to_string()),
                asset_symbol: Some("BTC".to_string()),
                condition_id: Some("cond-liqaccum".to_string()),
                token_id: "token-liqaccum".to_string(),
                impulse_id: format!("liqaccum_plan:5m:{period}:reject{idx}"),
                decision_id: None,
                rung_id: None,
                slice_id: None,
                request_id: None,
                order_id: None,
                trade_key: None,
                position_key: None,
                intent_status: Some("planned".to_string()),
                execution_status: Some("none".to_string()),
                intent_ts_ms: now_ms + 100 + i64::try_from(idx).unwrap_or_default(),
                submit_ts_ms: None,
                ack_ts_ms: None,
                fill_ts_ms: None,
                exit_ts_ms: None,
                settled_ts_ms: None,
                entry_notional_usd: Some(20.0),
                exit_notional_usd: None,
                realized_pnl_usd: None,
                settled_pnl_usd: None,
                hold_ms: None,
                feature_json: None,
                decision_context_json: None,
                execution_context_json: Some(
                    r#"{"friction":"direction_lock_reject","reason":"test"}"#.to_string(),
                ),
            })?;
        }

        let rows = db.summarize_rollout_strategy_gates_last_hours(1, false)?;
        let reactive = rows
            .iter()
            .find(|row| row.strategy_id == STRATEGY_ID_TEST_PRIMARY)
            .expect("reactive row missing");
        assert_eq!(reactive.entry_submits, 6);
        assert_eq!(reactive.entry_failed, 2);
        assert_eq!(reactive.submit_storm_groups, 1);
        assert!((reactive.entry_failed_ratio_pct - 33.3333).abs() < 0.1);

        let liqaccum = rows
            .iter()
            .find(|row| row.strategy_id == STRATEGY_ID_TEST_SECONDARY)
            .expect("liqaccum row missing");
        assert_eq!(liqaccum.entry_submits, 1);
        assert_eq!(liqaccum.liqaccum_plan_rows, 4);
        assert_eq!(liqaccum.liqaccum_direction_lock_rejects, 2);
        assert!((liqaccum.liqaccum_direction_lock_reject_ratio_pct - 50.0).abs() < 1e-9);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn rollout_integrity_gate_summary_reports_transition_rejections() -> Result<()> {
        let path = temp_db_path("rollout_integrity_gate_summary");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_040_000_u64;

        db.upsert_strategy_feature_snapshot_intent(&StrategyFeatureSnapshotIntentRecord {
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            timeframe: "15m".to_string(),
            period_timestamp: period,
            market_key: Some("btc".to_string()),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-integrity".to_string()),
            token_id: "token-integrity".to_string(),
            impulse_id: "reactive:integrity".to_string(),
            decision_id: Some("decision-integrity".to_string()),
            rung_id: None,
            slice_id: None,
            request_id: None,
            order_id: None,
            trade_key: None,
            position_key: None,
            intent_status: Some("planned".to_string()),
            execution_status: Some("none".to_string()),
            intent_ts_ms: 1_000,
            submit_ts_ms: None,
            ack_ts_ms: None,
            fill_ts_ms: None,
            exit_ts_ms: None,
            settled_ts_ms: None,
            entry_notional_usd: Some(25.0),
            exit_notional_usd: None,
            realized_pnl_usd: None,
            settled_pnl_usd: None,
            hold_ms: None,
            feature_json: None,
            decision_context_json: None,
            execution_context_json: None,
        })?;

        let rejected =
            db.upsert_strategy_feature_snapshot_intent(&StrategyFeatureSnapshotIntentRecord {
                strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
                timeframe: "15m".to_string(),
                period_timestamp: period,
                market_key: Some("btc".to_string()),
                asset_symbol: Some("BTC".to_string()),
                condition_id: Some("cond-integrity".to_string()),
                token_id: "token-integrity".to_string(),
                impulse_id: "reactive:integrity".to_string(),
                decision_id: Some("decision-integrity".to_string()),
                rung_id: None,
                slice_id: None,
                request_id: None,
                order_id: None,
                trade_key: None,
                position_key: None,
                intent_status: Some("planned".to_string()),
                execution_status: Some("settled".to_string()),
                intent_ts_ms: 1_001,
                submit_ts_ms: None,
                ack_ts_ms: None,
                fill_ts_ms: None,
                exit_ts_ms: None,
                settled_ts_ms: Some(1_001),
                entry_notional_usd: Some(25.0),
                exit_notional_usd: None,
                realized_pnl_usd: None,
                settled_pnl_usd: None,
                hold_ms: None,
                feature_json: None,
                decision_context_json: None,
                execution_context_json: None,
            });
        assert!(rejected.is_err());

        let summary = db.summarize_rollout_integrity_gates_last_hours(24)?;
        assert_eq!(summary.duplicate_snapshot_rows, 0);
        assert_eq!(summary.invalid_transition_rejections, 1);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn record_trade_event_auto_upserts_feature_snapshot() -> Result<()> {
        let path = temp_db_path("record_trade_event_auto_snapshot");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_050_000_u64;
        let ts_ms = chrono::Utc::now().timestamp_millis();

        db.record_trade_event(&TradeEventRecord {
            event_key: Some("auto-snapshot-entry-submit".to_string()),
            ts_ms,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-auto-snapshot".to_string()),
            token_id: Some("token-auto-snapshot".to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_SUBMIT".to_string(),
            price: Some(0.47),
            units: Some(10.0),
            notional_usd: Some(4.7),
            pnl_usd: None,
            reason: Some(r#"{"phase":"submit"}"#.to_string()),
        })?;

        let snapshot_rows: i64 = db.with_conn(|conn| {
            conn.query_row(
                r#"
SELECT COUNT(*)
FROM strategy_feature_snapshots_v1
WHERE strategy_id=?1
  AND timeframe='5m'
  AND period_timestamp=?2
  AND token_id='token-auto-snapshot'
  AND intent_status='submitted'
  AND submit_ts_ms IS NOT NULL
"#,
                params![STRATEGY_ID_TEST_PRIMARY, period as i64],
                |row| row.get(0),
            )
            .map_err(Into::into)
        })?;
        assert_eq!(snapshot_rows, 1);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn backfill_missing_entry_fill_events_inserts_rows() -> Result<()> {
        let path = temp_db_path("backfill_missing_entry_fill");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_060_000_u64;
        let order_id = "order-backfill-missing-fill";
        db.upsert_pending_order(&PendingOrderRecord {
            order_id: order_id.to_string(),
            trade_key: "trade-backfill-missing-fill".to_string(),
            token_id: "token-backfill-missing-fill".to_string(),
            condition_id: Some("cond-backfill-missing-fill".to_string()),
            timeframe: "15m".to_string(),
            strategy_id: STRATEGY_ID_PREMARKET_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            entry_mode: "ladder".to_string(),
            period_timestamp: period,
            price: 0.40,
            size_usd: 20.0,
            side: "BUY".to_string(),
            status: "FILLED".to_string(),
        })?;
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE pending_orders SET updated_at_ms=?2 WHERE order_id=?1",
                params![order_id, chrono::Utc::now().timestamp_millis()],
            )?;
            Ok(())
        })?;
        assert!(!db.has_entry_fill_event_for_order_id(order_id)?);

        let inserted = db.backfill_missing_entry_fill_events(24, 100)?;
        assert_eq!(inserted, 1);
        assert!(db.has_entry_fill_event_for_order_id(order_id)?);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn backfill_missing_entry_fill_events_skips_restored_orders() -> Result<()> {
        let path = temp_db_path("backfill_missing_entry_fill_skip_restored");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_060_001_u64;
        let order_id = "restored:1700060001:token-restored";
        db.upsert_pending_order(&PendingOrderRecord {
            order_id: order_id.to_string(),
            trade_key: "restored_1700060001_token-restored".to_string(),
            token_id: "token-restored".to_string(),
            condition_id: Some("cond-restored".to_string()),
            timeframe: "15m".to_string(),
            strategy_id: STRATEGY_ID_PREMARKET_V1.to_string(),
            asset_symbol: Some("BTC".to_string()),
            entry_mode: "restored".to_string(),
            period_timestamp: period,
            price: 0.40,
            size_usd: 20.0,
            side: "BUY".to_string(),
            status: "FILLED".to_string(),
        })?;
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE pending_orders SET updated_at_ms=?2 WHERE order_id=?1",
                params![order_id, chrono::Utc::now().timestamp_millis()],
            )?;
            Ok(())
        })?;
        assert!(!db.has_entry_fill_event_for_order_id(order_id)?);

        let inserted = db.backfill_missing_entry_fill_events(24, 100)?;
        assert_eq!(inserted, 0);
        assert!(!db.has_entry_fill_event_for_order_id(order_id)?);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn reconcile_market_close_exit_pnl_from_settlement_updates_event() -> Result<()> {
        let path = temp_db_path("reconcile_market_close_exit_pnl");
        let db = TrackingDb::new(&path)?;
        let period = 1_700_070_000_u64;

        db.record_trade_event(&TradeEventRecord {
            event_key: Some("reconcile-settle-entry".to_string()),
            ts_ms: 10_000,
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-reconcile-settle".to_string()),
            token_id: Some("token-reconcile-settle".to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.50),
            units: Some(40.0),
            notional_usd: Some(20.0),
            pnl_usd: None,
            reason: Some("limit_buy_fill".to_string()),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some("reconcile-settle-exit".to_string()),
            ts_ms: chrono::Utc::now().timestamp_millis(),
            period_timestamp: period,
            timeframe: "5m".to_string(),
            strategy_id: STRATEGY_ID_TEST_PRIMARY.to_string(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some("cond-reconcile-settle".to_string()),
            token_id: Some("token-reconcile-settle".to_string()),
            token_type: Some("BTC Up".to_string()),
            side: Some("SELL".to_string()),
            event_type: "EXIT".to_string(),
            price: Some(0.0),
            units: Some(40.0),
            notional_usd: Some(0.0),
            pnl_usd: Some(-10.0),
            reason: Some("production_market_close".to_string()),
        })?;

        let changed = db.reconcile_market_close_exit_pnl_from_settlements(24)?;
        assert!(changed >= 1);
        let reconciled_pnl: f64 = db.with_conn(|conn| {
            conn.query_row(
                "SELECT COALESCE(pnl_usd, 0.0) FROM trade_events WHERE event_key='reconcile-settle-exit'",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
        })?;
        assert!((reconciled_pnl + 20.0).abs() < 1e-9);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn lifecycle_and_wallet_cashflow_checksum_aligns_after_redemption() -> Result<()> {
        let path = temp_db_path("lifecycle_wallet_checksum");
        let db = TrackingDb::new(&path)?;
        let strategy_id = STRATEGY_ID_TEST_PRIMARY.to_string();
        let timeframe = "5m".to_string();
        let period = 1_700_080_000_u64;
        let condition_id = "cond-life-check";
        let token_id = "token-life-check";

        db.record_trade_event(&TradeEventRecord {
            event_key: Some("life-entry-fill".to_string()),
            ts_ms: 11_000,
            period_timestamp: period,
            timeframe: timeframe.clone(),
            strategy_id: strategy_id.clone(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some(condition_id.to_string()),
            token_id: Some(token_id.to_string()),
            token_type: Some("BTC Down".to_string()),
            side: Some("BUY".to_string()),
            event_type: "ENTRY_FILL".to_string(),
            price: Some(0.50),
            units: Some(20.0),
            notional_usd: Some(10.0),
            pnl_usd: None,
            reason: Some("limit_buy_fill".to_string()),
        })?;
        db.record_trade_event(&TradeEventRecord {
            event_key: Some("life-market-close-exit".to_string()),
            ts_ms: 12_000,
            period_timestamp: period,
            timeframe: timeframe.clone(),
            strategy_id: strategy_id.clone(),
            asset_symbol: Some("BTC".to_string()),
            condition_id: Some(condition_id.to_string()),
            token_id: Some(token_id.to_string()),
            token_type: Some("BTC Down".to_string()),
            side: Some("SELL".to_string()),
            event_type: "EXIT".to_string(),
            price: Some(1.0),
            units: Some(20.0),
            notional_usd: Some(20.0),
            pnl_usd: Some(10.0),
            reason: Some("production_market_close".to_string()),
        })?;

        db.update_trade_lifecycle_redemption_state(
            strategy_id.as_str(),
            timeframe.as_str(),
            period,
            Some(condition_id),
            token_id,
            "success",
            Some("tx-life-check"),
            Some(1),
            Some(20.0),
            Some("redemption confirmed"),
        )?;
        let cashflow = WalletCashflowEventRecord {
            cashflow_key: "cashflow-life-check".to_string(),
            ts_ms: 13_000,
            strategy_id: strategy_id.clone(),
            timeframe: timeframe.clone(),
            period_timestamp: period,
            condition_id: Some(condition_id.to_string()),
            token_id: Some(token_id.to_string()),
            event_type: "REDEMPTION_SUCCESS".to_string(),
            amount_usd: Some(20.0),
            units: Some(20.0),
            token_price: Some(1.0),
            tx_id: Some("tx-life-check".to_string()),
            reason: Some("unit-test".to_string()),
            source: Some("unit-test".to_string()),
        };
        db.record_wallet_cashflow_event(&cashflow)?;
        db.record_wallet_cashflow_event(&cashflow)?; // idempotent

        let lifecycle = db.with_conn(|conn| {
            conn.query_row(
                r#"
SELECT
    entry_units,
    entry_notional_usd,
    resolved_payout_usd,
    resolved_pnl_usd,
    redemption_status,
    redeemed_notional_usd
FROM trade_lifecycle_v1
WHERE lifecycle_key=?1
"#,
                params![format!(
                    "{}:{}:{}:{}",
                    strategy_id, timeframe, period, token_id
                )],
                |row| {
                    Ok((
                        row.get::<_, f64>(0)?,
                        row.get::<_, f64>(1)?,
                        row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                        row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                    ))
                },
            )
            .map_err(Into::into)
        })?;
        assert!((lifecycle.0 - 20.0).abs() < 1e-9);
        assert!((lifecycle.1 - 10.0).abs() < 1e-9);
        assert!((lifecycle.2 - 20.0).abs() < 1e-9);
        assert!((lifecycle.3 - 10.0).abs() < 1e-9);
        assert_eq!(lifecycle.4, "success");
        assert!((lifecycle.5 - 20.0).abs() < 1e-9);

        let cashflow_rows: i64 = db.with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM wallet_cashflow_events_v1 WHERE cashflow_key='cashflow-life-check'",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
        })?;
        assert_eq!(cashflow_rows, 1);

        let checksum = db.summarize_strategy_wallet_checksum(Some(0))?;
        let row = checksum
            .iter()
            .find(|r| r.strategy_id == strategy_id)
            .expect("checksum row missing");
        assert_eq!(row.settled_positions, 1);
        assert!((row.settled_payout_usd - 20.0).abs() < 1e-9);
        assert!((row.settled_cost_basis_usd - 10.0).abs() < 1e-9);
        assert!((row.settled_pnl_usd - 10.0).abs() < 1e-9);
        assert!((row.redeemed_cash_usd - 20.0).abs() < 1e-9);
        assert!((row.unresolved_redeem_usd).abs() < 1e-9);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn evcurve_period_base_persists_across_restart() -> Result<()> {
        let path = temp_db_path("evcurve_period_base_roundtrip");
        let period_open_ts = 1_771_915_200_i64;
        {
            let db = TrackingDb::new(&path)?;
            db.upsert_evcurve_period_base(
                "XRP",
                "15m",
                period_open_ts,
                1.33155,
                1_771_915_201_000,
            )?;
            let base = db.get_evcurve_period_base("xrp", "15M", period_open_ts)?;
            assert_eq!(base, Some(1.33155));
        }

        let db = TrackingDb::new(&path)?;
        let base = db.get_evcurve_period_base("XRP", "15m", period_open_ts)?;
        assert_eq!(base, Some(1.33155));

        drop(db);
        cleanup_db(&path);
        Ok(())
    }

    #[test]
    fn prune_terminal_pending_orders_respects_active_rows() -> Result<()> {
        let path = temp_db_path("pending_prune_terminal_rows");
        let db = TrackingDb::new(&path)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let old_ms = now_ms.saturating_sub(86_400_000);

        for (order_id, status) in [
            ("ord-old-terminal", "CANCELED"),
            ("ord-new-terminal", "FILLED"),
            ("ord-active-open", "OPEN"),
        ] {
            db.upsert_pending_order(&PendingOrderRecord {
                order_id: order_id.to_string(),
                trade_key: order_id.to_string(),
                token_id: "tok".to_string(),
                condition_id: Some("cond".to_string()),
                timeframe: "5m".to_string(),
                strategy_id: "mm_rewards_v1".to_string(),
                asset_symbol: Some("BTC".to_string()),
                entry_mode: "MM_REWARDS".to_string(),
                period_timestamp: 1_771_000_000,
                price: 0.5,
                size_usd: 10.0,
                side: "BUY".to_string(),
                status: status.to_string(),
            })?;
        }

        db.with_conn_mut(|conn| {
            conn.execute(
                "UPDATE pending_orders SET updated_at_ms=?1 WHERE order_id='ord-old-terminal'",
                params![old_ms],
            )?;
            conn.execute(
                "UPDATE pending_orders SET updated_at_ms=?1 WHERE order_id='ord-new-terminal'",
                params![now_ms],
            )?;
            conn.execute(
                "UPDATE pending_orders SET updated_at_ms=?1 WHERE order_id='ord-active-open'",
                params![old_ms],
            )?;
            Ok(())
        })?;

        let deleted = db.archive_and_prune_pending_orders(now_ms.saturating_sub(1_000), 100)?;
        assert_eq!(deleted, 1);

        let remaining: i64 = db.with_conn_read(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM pending_orders WHERE order_id='ord-old-terminal'",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
        })?;
        assert_eq!(remaining, 0);

        let active_remaining: i64 = db.with_conn_read(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM pending_orders WHERE order_id='ord-active-open'",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
        })?;
        assert_eq!(active_remaining, 1);

        drop(db);
        cleanup_db(&path);
        Ok(())
    }
}
