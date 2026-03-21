use crate::api::PolymarketApi;
use crate::event_log::log_event;
use crate::models::{OrderBook, OrderBookEntry, TokenPrice};

use futures_util::StreamExt;
use polymarket_client_sdk::clob::types::{OrderStatusType, Side};
use polymarket_client_sdk::clob::ws;
use polymarket_client_sdk::clob::ws::types::response::{
    BookUpdate, LastTradePrice, OrderMessage, OrderMessageType, TradeMessage, WsMessage,
};
use polymarket_client_sdk::types::{B256, U256};
use polymarket_client_sdk::ws::config::Config as WsConnectionConfig;
use rust_decimal::Decimal;
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Once;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::time::{sleep, Duration};

#[derive(Debug, Clone)]
pub struct PolymarketWsConfig {
    pub enabled: bool,
    pub endpoint: String,
    pub refresh_sec: u64,
    pub backoff_min_sec: u64,
    pub backoff_max_sec: u64,
    pub market_discovery_limit: u32,
    pub market_stale_ms: i64,
    pub order_stale_ms: i64,
    pub prune_after_ms: i64,
    pub market_lag_soft_errors: u32,
    pub market_lag_window_sec: u64,
    pub market_lag_hard_missed_messages: u32,
    pub market_lag_ignore_missed_messages: u32,
    pub market_lag_ignored_log_min_interval_ms: u64,
    pub market_lag_reconnect_cooldown_sec: u64,
    pub market_unstable_window_sec: u64,
    pub market_reconnect_jitter_ms: u64,
    pub target_change_debounce_scans: u32,
    pub target_change_min_hold_sec: u64,
    pub target_change_min_delta_bps: u32,
    pub reconnect_on_refresh: bool,
    pub market_shards: u32,
}

impl Default for PolymarketWsConfig {
    fn default() -> Self {
        let refresh_sec = env_u64("EVPOLY_PM_WS_REFRESH_SEC", 90).max(10);
        let backoff_min_sec = env_u64("EVPOLY_PM_WS_BACKOFF_MIN_SEC", 1).max(1);
        let backoff_max_sec = env_u64("EVPOLY_PM_WS_BACKOFF_MAX_SEC", 20).max(backoff_min_sec);
        let market_stale_ms = env_i64("EVPOLY_PM_WS_MARKET_STALE_MS", 600).max(250);
        let order_stale_ms = env_i64("EVPOLY_PM_WS_ORDER_STALE_MS", 5_000).max(500);
        Self {
            enabled: env_bool("EVPOLY_PM_WS_ENABLE", true),
            endpoint: std::env::var("EVPOLY_PM_WS_ENDPOINT")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "wss://ws-subscriptions-clob.polymarket.com".to_string()),
            refresh_sec,
            backoff_min_sec,
            backoff_max_sec,
            market_discovery_limit: env_u64("EVPOLY_PM_WS_MARKET_DISCOVERY_LIMIT", 250)
                .clamp(50, 5_000) as u32,
            market_stale_ms,
            order_stale_ms,
            prune_after_ms: env_i64("EVPOLY_PM_WS_PRUNE_AFTER_MS", 14_400_000).max(60_000),
            market_lag_soft_errors: env_u64("EVPOLY_PM_WS_MARKET_LAG_SOFT_ERRORS", 5).clamp(1, 100)
                as u32,
            market_lag_window_sec: env_u64("EVPOLY_PM_WS_MARKET_LAG_WINDOW_SEC", 20).clamp(5, 300),
            market_lag_hard_missed_messages: env_u64(
                "EVPOLY_PM_WS_MARKET_LAG_HARD_MISSED_MESSAGES",
                12_000,
            )
            .clamp(1_000, 200_000) as u32,
            market_lag_ignore_missed_messages: env_u64(
                "EVPOLY_PM_WS_MARKET_LAG_IGNORE_MISSED_MESSAGES",
                3_000,
            )
            .clamp(0, 100_000) as u32,
            market_lag_ignored_log_min_interval_ms: env_u64(
                "EVPOLY_PM_WS_MARKET_LAG_IGNORED_LOG_MIN_INTERVAL_MS",
                1_000,
            )
            .clamp(0, 30_000),
            market_lag_reconnect_cooldown_sec: env_u64(
                "EVPOLY_PM_WS_MARKET_LAG_RECONNECT_COOLDOWN_SEC",
                60,
            )
            .clamp(5, 900),
            market_unstable_window_sec: env_u64("EVPOLY_PM_WS_MARKET_UNSTABLE_WINDOW_SEC", 90)
                .clamp(10, 900),
            market_reconnect_jitter_ms: env_u64("EVPOLY_PM_WS_MARKET_RECONNECT_JITTER_MS", 250)
                .clamp(0, 5_000),
            target_change_debounce_scans: env_u64("EVPOLY_PM_WS_TARGET_CHANGE_DEBOUNCE_SCANS", 2)
                .clamp(1, 10) as u32,
            target_change_min_hold_sec: env_u64("EVPOLY_PM_WS_TARGET_CHANGE_MIN_HOLD_SEC", 90)
                .clamp(0, 900),
            target_change_min_delta_bps: env_u64("EVPOLY_PM_WS_TARGET_CHANGE_MIN_DELTA_BPS", 0)
                .clamp(0, 10_000) as u32,
            reconnect_on_refresh: env_bool("EVPOLY_PM_WS_RECONNECT_ON_REFRESH", false),
            market_shards: env_u64("EVPOLY_PM_WS_MARKET_SHARDS", 2).clamp(1, 12) as u32,
        }
    }
}

#[derive(Debug, Clone)]
struct PendingMarketTargetChange {
    asset_ids: Vec<U256>,
    market_ids: Vec<B256>,
    first_seen_ms: i64,
    confirmations: u32,
    delta_bps: u32,
}

#[derive(Debug, Clone)]
struct PendingUserTargetChange {
    market_ids: Vec<B256>,
    first_seen_ms: i64,
    confirmations: u32,
    delta_bps: u32,
}

#[derive(Debug, Clone)]
pub struct WsOrderbookSnapshot {
    pub orderbook: OrderBook,
    pub updated_ms: i64,
}

#[derive(Debug, Clone)]
pub struct WsTradeSnapshot {
    pub token_id: String,
    pub price: Decimal,
    pub size: Decimal,
    pub updated_ms: i64,
}

const WS_MARKET_TRADE_WINDOW_RETENTION_MS: i64 = 30_000;

#[derive(Debug, Clone)]
pub struct WsOrderStatusSnapshot {
    pub order_id: String,
    pub status: OrderStatusType,
    pub side: Side,
    pub price: Decimal,
    pub size_matched: Decimal,
    pub original_size: Decimal,
    pub market: B256,
    pub asset_id: U256,
    pub outcome: String,
    pub updated_ms: i64,
}

#[derive(Debug)]
struct PolymarketWsInner {
    orderbooks: tokio::sync::RwLock<HashMap<String, WsOrderbookSnapshot>>,
    trades: tokio::sync::RwLock<HashMap<String, WsTradeSnapshot>>,
    market_trades: tokio::sync::RwLock<HashMap<String, VecDeque<WsTradeSnapshot>>>,
    order_statuses: tokio::sync::RwLock<HashMap<String, WsOrderStatusSnapshot>>,
    subscription_scope_targets: StdMutex<HashMap<String, WsSubscriptionScopeTargets>>,
    market_update_notify: tokio::sync::Notify,
    user_update_notify: tokio::sync::Notify,
    market_connected_shards: StdMutex<HashMap<usize, bool>>,
    user_connected: AtomicBool,
    last_market_msg_ms: AtomicI64,
    last_user_msg_ms: AtomicI64,
}

#[derive(Clone, Debug)]
pub struct SharedPolymarketWsState {
    inner: Arc<PolymarketWsInner>,
}

#[derive(Clone, Debug, Default)]
struct WsSubscriptionScopeTargets {
    asset_ids: Vec<U256>,
    market_ids: Vec<B256>,
}

#[derive(Debug, Clone, Default)]
struct DegradedLogState {
    active: bool,
    since_ms: i64,
    last_heartbeat_ms: i64,
    event_count: u64,
    last_reason: String,
}

impl DegradedLogState {
    fn mark_degraded(
        &mut self,
        transition_event: &str,
        heartbeat_event: &str,
        reason: &str,
        mut payload: serde_json::Value,
    ) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.event_count = self.event_count.saturating_add(1);
        if !self.active {
            self.active = true;
            self.since_ms = now_ms;
            self.last_heartbeat_ms = now_ms;
            self.last_reason = reason.to_string();
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("reason".to_string(), json!(reason));
                obj.insert("degraded_since_ms".to_string(), json!(self.since_ms));
                obj.insert("event_count".to_string(), json!(self.event_count));
            }
            log_event(transition_event, payload);
            return;
        }

        self.last_reason = reason.to_string();
        if now_ms.saturating_sub(self.last_heartbeat_ms) >= 60_000 {
            self.last_heartbeat_ms = now_ms;
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("reason".to_string(), json!(self.last_reason));
                obj.insert("degraded_since_ms".to_string(), json!(self.since_ms));
                obj.insert(
                    "degraded_duration_ms".to_string(),
                    json!(now_ms.saturating_sub(self.since_ms)),
                );
                obj.insert("event_count".to_string(), json!(self.event_count));
            }
            log_event(heartbeat_event, payload);
        }
    }

    fn mark_recovered(&mut self, recovery_event: &str, mut payload: serde_json::Value) {
        if !self.active {
            return;
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("degraded_since_ms".to_string(), json!(self.since_ms));
            obj.insert(
                "degraded_duration_ms".to_string(),
                json!(now_ms.saturating_sub(self.since_ms)),
            );
            obj.insert("degraded_event_count".to_string(), json!(self.event_count));
            obj.insert("last_reason".to_string(), json!(self.last_reason.clone()));
        }
        log_event(recovery_event, payload);
        *self = Self::default();
    }
}

fn discovery_backoff_sec(streak: u32) -> u64 {
    match streak {
        0 | 1 => 90,
        2 => 180,
        _ => 300,
    }
}

pub fn new_shared_polymarket_ws_state() -> SharedPolymarketWsState {
    SharedPolymarketWsState {
        inner: Arc::new(PolymarketWsInner {
            orderbooks: tokio::sync::RwLock::new(HashMap::new()),
            trades: tokio::sync::RwLock::new(HashMap::new()),
            market_trades: tokio::sync::RwLock::new(HashMap::new()),
            order_statuses: tokio::sync::RwLock::new(HashMap::new()),
            subscription_scope_targets: StdMutex::new(HashMap::new()),
            market_update_notify: tokio::sync::Notify::new(),
            user_update_notify: tokio::sync::Notify::new(),
            market_connected_shards: StdMutex::new(HashMap::new()),
            user_connected: AtomicBool::new(false),
            last_market_msg_ms: AtomicI64::new(0),
            last_user_msg_ms: AtomicI64::new(0),
        }),
    }
}

impl SharedPolymarketWsState {
    pub fn set_subscription_scope_targets(
        &self,
        scope_id: &str,
        token_ids: &[String],
        condition_ids: &[String],
    ) {
        let scope_key = scope_id.trim().to_ascii_lowercase();
        if scope_key.is_empty() {
            return;
        }
        let mut asset_set: HashSet<U256> = HashSet::new();
        for token_id in token_ids {
            if let Some(asset_id) = parse_asset_id(token_id.as_str()) {
                asset_set.insert(asset_id);
            }
        }
        let mut market_set: HashSet<B256> = HashSet::new();
        for condition_id in condition_ids {
            if let Ok(market_id) = B256::from_str(condition_id.trim()) {
                market_set.insert(market_id);
            }
        }
        let mut asset_ids = asset_set.into_iter().collect::<Vec<_>>();
        asset_ids.sort();
        let mut market_ids = market_set.into_iter().collect::<Vec<_>>();
        market_ids.sort();

        let mut guard = self
            .inner
            .subscription_scope_targets
            .lock()
            .expect("polymarket ws subscription-scope mutex poisoned");
        if asset_ids.is_empty() && market_ids.is_empty() {
            guard.remove(scope_key.as_str());
        } else {
            guard.insert(
                scope_key,
                WsSubscriptionScopeTargets {
                    asset_ids,
                    market_ids,
                },
            );
        }
    }

    pub fn clear_subscription_scope_targets(&self, scope_id: &str) {
        let scope_key = scope_id.trim().to_ascii_lowercase();
        if scope_key.is_empty() {
            return;
        }
        let mut guard = self
            .inner
            .subscription_scope_targets
            .lock()
            .expect("polymarket ws subscription-scope mutex poisoned");
        guard.remove(scope_key.as_str());
    }

    pub fn subscription_scope_targets_snapshot(&self) -> (Vec<U256>, Vec<B256>) {
        let guard = self
            .inner
            .subscription_scope_targets
            .lock()
            .expect("polymarket ws subscription-scope mutex poisoned");
        let mut asset_set: HashSet<U256> = HashSet::new();
        let mut market_set: HashSet<B256> = HashSet::new();
        for scope in guard.values() {
            for asset_id in &scope.asset_ids {
                asset_set.insert(*asset_id);
            }
            for market_id in &scope.market_ids {
                market_set.insert(*market_id);
            }
        }
        let mut asset_ids = asset_set.into_iter().collect::<Vec<_>>();
        asset_ids.sort();
        let mut market_ids = market_set.into_iter().collect::<Vec<_>>();
        market_ids.sort();
        (asset_ids, market_ids)
    }

    pub async fn get_orderbook(&self, token_id: &str, max_age_ms: i64) -> Option<OrderBook> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let map = self.inner.orderbooks.read().await;
        let snapshot = map.get(token_id)?;
        if snapshot.updated_ms <= 0 || now_ms.saturating_sub(snapshot.updated_ms) > max_age_ms {
            return None;
        }
        Some(snapshot.orderbook.clone())
    }

    pub async fn get_best_price(&self, token_id: &str, max_age_ms: i64) -> Option<TokenPrice> {
        let orderbook = self.get_orderbook(token_id, max_age_ms).await?;
        let best_bid = orderbook
            .bids
            .iter()
            .filter(|v| v.price > Decimal::ZERO && v.size > Decimal::ZERO)
            .map(|v| v.price)
            .max();
        let best_ask = orderbook
            .asks
            .iter()
            .filter(|v| v.price > Decimal::ZERO && v.size > Decimal::ZERO)
            .map(|v| v.price)
            .min();
        if best_bid.is_none() && best_ask.is_none() {
            return None;
        }
        Some(TokenPrice {
            token_id: token_id.to_string(),
            bid: best_bid,
            ask: best_ask,
        })
    }

    pub async fn get_order_status(
        &self,
        order_id: &str,
        max_age_ms: i64,
    ) -> Option<WsOrderStatusSnapshot> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let map = self.inner.order_statuses.read().await;
        let snapshot = map.get(order_id)?;
        if snapshot.updated_ms <= 0 || now_ms.saturating_sub(snapshot.updated_ms) > max_age_ms {
            return None;
        }
        Some(snapshot.clone())
    }

    pub async fn get_last_trade(&self, token_id: &str, max_age_ms: i64) -> Option<WsTradeSnapshot> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let map = self.inner.trades.read().await;
        let snapshot = map.get(token_id)?;
        if snapshot.updated_ms <= 0 || now_ms.saturating_sub(snapshot.updated_ms) > max_age_ms {
            return None;
        }
        Some(snapshot.clone())
    }

    pub async fn sum_recent_market_trade_size_at_or_below_price(
        &self,
        token_id: &str,
        max_age_ms: i64,
        max_price: f64,
    ) -> f64 {
        if max_age_ms <= 0 || !max_price.is_finite() || max_price <= 0.0 {
            return 0.0;
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        let window_start_ms = now_ms.saturating_sub(max_age_ms);
        let map = self.inner.market_trades.read().await;
        let Some(window) = map.get(token_id) else {
            return 0.0;
        };
        window
            .iter()
            .filter(|trade| trade.updated_ms >= window_start_ms)
            .filter_map(|trade| {
                let price = f64::try_from(trade.price).ok()?;
                let size = f64::try_from(trade.size).ok()?;
                if !price.is_finite()
                    || !size.is_finite()
                    || price <= 0.0
                    || size <= 0.0
                    || price > max_price + 1e-9
                {
                    return None;
                }
                Some(size)
            })
            .sum::<f64>()
    }

    fn set_market_connected(&self, shard_idx: usize, connected: bool) {
        let mut guard = self
            .inner
            .market_connected_shards
            .lock()
            .expect("polymarket ws shard-state mutex poisoned");
        guard.insert(shard_idx, connected);
    }

    fn clear_market_connected(&self, shard_idx: usize) {
        let mut guard = self
            .inner
            .market_connected_shards
            .lock()
            .expect("polymarket ws shard-state mutex poisoned");
        guard.remove(&shard_idx);
    }

    fn set_user_connected(&self, connected: bool) {
        self.inner
            .user_connected
            .store(connected, Ordering::Relaxed);
    }

    pub fn market_connected(&self) -> bool {
        let guard = self
            .inner
            .market_connected_shards
            .lock()
            .expect("polymarket ws shard-state mutex poisoned");
        !guard.is_empty() && guard.values().all(|connected| *connected)
    }

    pub fn user_connected(&self) -> bool {
        self.inner.user_connected.load(Ordering::Relaxed)
    }

    pub fn last_market_msg_ms(&self) -> i64 {
        self.inner.last_market_msg_ms.load(Ordering::Relaxed)
    }

    pub fn last_user_msg_ms(&self) -> i64 {
        self.inner.last_user_msg_ms.load(Ordering::Relaxed)
    }

    pub async fn wait_for_user_update_after(&self, last_seen_ms: i64) -> i64 {
        loop {
            let current = self.last_user_msg_ms();
            if current > last_seen_ms {
                return current;
            }
            self.inner.user_update_notify.notified().await;
        }
    }

    pub async fn wait_for_order_update_after(&self, order_id: &str, last_seen_ms: i64) -> i64 {
        loop {
            let current = {
                let map = self.inner.order_statuses.read().await;
                map.get(order_id)
                    .map(|snapshot| snapshot.updated_ms)
                    .unwrap_or_else(|| self.last_user_msg_ms())
            };
            if current > last_seen_ms {
                return current;
            }
            self.inner.user_update_notify.notified().await;
        }
    }

    pub async fn wait_for_market_update_after(&self, last_seen_ms: i64) -> i64 {
        loop {
            let current = self.last_market_msg_ms();
            if current > last_seen_ms {
                return current;
            }
            self.inner.market_update_notify.notified().await;
        }
    }

    pub async fn wait_for_market_update_after_for_tokens(
        &self,
        last_seen_ms: i64,
        token_ids: &[String],
    ) -> i64 {
        if token_ids.is_empty() {
            return self.wait_for_market_update_after(last_seen_ms).await;
        }
        loop {
            let orderbook_current = {
                let map = self.inner.orderbooks.read().await;
                token_ids
                    .iter()
                    .filter_map(|token_id| map.get(token_id).map(|snap| snap.updated_ms))
                    .max()
                    .unwrap_or(0)
            };
            let trade_current = {
                let map = self.inner.market_trades.read().await;
                token_ids
                    .iter()
                    .filter_map(|token_id| map.get(token_id))
                    .filter_map(|window| window.back().map(|trade| trade.updated_ms))
                    .max()
                    .unwrap_or(0)
            };
            let current = orderbook_current.max(trade_current);
            if current > last_seen_ms {
                return current;
            }
            self.inner.market_update_notify.notified().await;
        }
    }

    async fn apply_book_update(&self, update: BookUpdate) {
        self.inner
            .last_market_msg_ms
            .store(update.timestamp, Ordering::Relaxed);
        let token_id = update.asset_id.to_string();
        let orderbook = OrderBook {
            bids: update
                .bids
                .into_iter()
                .map(|level| OrderBookEntry {
                    price: level.price,
                    size: level.size,
                })
                .collect(),
            asks: update
                .asks
                .into_iter()
                .map(|level| OrderBookEntry {
                    price: level.price,
                    size: level.size,
                })
                .collect(),
        };
        let snapshot = WsOrderbookSnapshot {
            orderbook,
            updated_ms: update.timestamp,
        };
        self.inner
            .orderbooks
            .write()
            .await
            .insert(token_id, snapshot);
        self.inner.market_update_notify.notify_waiters();
    }

    async fn apply_market_trade_update(&self, trade: LastTradePrice) {
        let updated_ms = trade.timestamp;
        self.inner
            .last_market_msg_ms
            .store(updated_ms, Ordering::Relaxed);

        let price = trade.price;
        let size = trade.size.unwrap_or(Decimal::ZERO);
        if price > Decimal::ZERO && size > Decimal::ZERO {
            let token_id = trade.asset_id.to_string();
            let snapshot = WsTradeSnapshot {
                token_id: token_id.clone(),
                price,
                size,
                updated_ms,
            };
            self.inner
                .trades
                .write()
                .await
                .insert(token_id.clone(), snapshot.clone());
            let mut windows = self.inner.market_trades.write().await;
            let window = windows.entry(token_id).or_default();
            window.push_back(snapshot);
            let min_keep_ms = updated_ms.saturating_sub(WS_MARKET_TRADE_WINDOW_RETENTION_MS);
            while window
                .front()
                .map(|entry| entry.updated_ms < min_keep_ms)
                .unwrap_or(false)
            {
                let _ = window.pop_front();
            }
        }

        self.inner.market_update_notify.notify_waiters();
    }

    async fn apply_order_update(&self, order: OrderMessage) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let updated_ms = order.timestamp.unwrap_or(now_ms);
        self.inner
            .last_user_msg_ms
            .store(updated_ms, Ordering::Relaxed);
        let status = status_from_order_message(&order);
        let size_matched = order.size_matched.unwrap_or(Decimal::ZERO);
        let original_size = order.original_size.unwrap_or(size_matched);
        let outcome = order.outcome.unwrap_or_default();
        let next = WsOrderStatusSnapshot {
            order_id: order.id.clone(),
            status,
            side: order.side,
            price: order.price,
            size_matched,
            original_size,
            market: order.market,
            asset_id: order.asset_id,
            outcome,
            updated_ms,
        };
        self.inner
            .order_statuses
            .write()
            .await
            .insert(order.id, next);
        self.inner.user_update_notify.notify_waiters();
    }

    async fn apply_trade_update(&self, trade: TradeMessage) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let updated_ms = trade
            .timestamp
            .or(trade.matchtime)
            .or(trade.last_update)
            .unwrap_or(now_ms);
        self.inner
            .last_user_msg_ms
            .store(updated_ms, Ordering::Relaxed);

        if let Some(order_id) = trade.taker_order_id.as_ref() {
            self.merge_trade_fill(
                order_id.as_str(),
                trade.market,
                trade.asset_id,
                trade.side,
                trade.price,
                trade.size,
                trade.outcome.clone().unwrap_or_default(),
                updated_ms,
            )
            .await;
        }
        for maker in &trade.maker_orders {
            self.merge_trade_fill(
                maker.order_id.as_str(),
                trade.market,
                maker.asset_id,
                trade.side,
                maker.price,
                maker.matched_amount,
                maker.outcome.clone(),
                updated_ms,
            )
            .await;
        }
    }

    async fn merge_trade_fill(
        &self,
        order_id: &str,
        market: B256,
        asset_id: U256,
        side: Side,
        price: Decimal,
        matched_size: Decimal,
        outcome: String,
        updated_ms: i64,
    ) {
        let mut map = self.inner.order_statuses.write().await;
        if let Some(existing) = map.get_mut(order_id) {
            existing.status = OrderStatusType::Matched;
            if price > Decimal::ZERO {
                existing.price = price;
            }
            if matched_size > existing.size_matched {
                existing.size_matched = matched_size;
            }
            if existing.original_size < existing.size_matched {
                existing.original_size = existing.size_matched;
            }
            existing.updated_ms = updated_ms;
        } else {
            map.insert(
                order_id.to_string(),
                WsOrderStatusSnapshot {
                    order_id: order_id.to_string(),
                    status: OrderStatusType::Matched,
                    side,
                    price,
                    size_matched: matched_size,
                    original_size: matched_size,
                    market,
                    asset_id,
                    outcome,
                    updated_ms,
                },
            );
        }
        drop(map);

        if price > Decimal::ZERO && matched_size > Decimal::ZERO {
            let token_id = asset_id.to_string();
            self.inner.trades.write().await.insert(
                token_id.clone(),
                WsTradeSnapshot {
                    token_id,
                    price,
                    size: matched_size,
                    updated_ms,
                },
            );
        }
        self.inner.user_update_notify.notify_waiters();
    }

    async fn prune_stale(&self, prune_after_ms: i64) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        {
            let mut books = self.inner.orderbooks.write().await;
            books.retain(|_, v| now_ms.saturating_sub(v.updated_ms) <= prune_after_ms);
        }
        {
            let mut statuses = self.inner.order_statuses.write().await;
            statuses.retain(|_, v| now_ms.saturating_sub(v.updated_ms) <= prune_after_ms);
        }
        {
            let mut trades = self.inner.trades.write().await;
            trades.retain(|_, v| now_ms.saturating_sub(v.updated_ms) <= prune_after_ms);
        }
        {
            let mut market_trades = self.inner.market_trades.write().await;
            market_trades.retain(|_, window| {
                window.retain(|entry| now_ms.saturating_sub(entry.updated_ms) <= prune_after_ms);
                !window.is_empty()
            });
        }
    }
}

pub fn spawn_polymarket_ws_bridge(
    api: Arc<PolymarketApi>,
    cfg: PolymarketWsConfig,
    shared_state: SharedPolymarketWsState,
) -> tokio::task::JoinHandle<()> {
    ensure_rustls_provider();
    tokio::spawn(async move {
        if !cfg.enabled {
            log_event(
                "polymarket_ws_disabled",
                json!({
                    "enabled": false
                }),
            );
            return;
        }

        log_event(
            "polymarket_ws_started",
            json!({
                "endpoint": cfg.endpoint,
                "refresh_sec": cfg.refresh_sec,
                "market_discovery_limit": cfg.market_discovery_limit,
                "market_stale_ms": cfg.market_stale_ms,
                "order_stale_ms": cfg.order_stale_ms
            }),
        );

        let market_shards = usize::try_from(cfg.market_shards).ok().unwrap_or(1).max(1);
        log_event(
            "polymarket_ws_market_shards_started",
            json!({
                "market_shards": market_shards
            }),
        );
        for shard_idx in 0..market_shards {
            let market_cfg = cfg.clone();
            let market_api = api.clone();
            let market_state = shared_state.clone();
            tokio::spawn(async move {
                run_market_loop(
                    market_api,
                    market_cfg,
                    market_state,
                    shard_idx,
                    market_shards,
                )
                .await
            });
        }

        let user_cfg = cfg.clone();
        let user_state = shared_state.clone();
        tokio::spawn(async move { run_user_loop(api, user_cfg, user_state).await });

        // Keep parent task alive; child loops are long-running.
        loop {
            sleep(Duration::from_secs(3_600)).await;
        }
    })
}

fn ensure_rustls_provider() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // SDK WS path uses rustls-tls-native-roots. Install a deterministic provider once
        // to avoid per-thread panic when both provider features are present in the graph.
        let before = rustls::crypto::CryptoProvider::get_default().is_some();
        let install_ok = rustls::crypto::ring::default_provider()
            .install_default()
            .is_ok();
        let after = rustls::crypto::CryptoProvider::get_default().is_some();
        log_event(
            "polymarket_ws_rustls_provider",
            json!({
                "before_installed": before,
                "install_ok": install_ok,
                "after_installed": after
            }),
        );
    });
}

async fn run_market_loop(
    api: Arc<PolymarketApi>,
    cfg: PolymarketWsConfig,
    state: SharedPolymarketWsState,
    shard_idx: usize,
    shard_count: usize,
) {
    let mut backoff_sec = cfg.backoff_min_sec;
    let mut last_lag_reconnect_ms = 0_i64;
    let mut last_discovered_targets: Option<(Vec<U256>, Vec<B256>, usize)> = None;
    let mut discovery_failure_streak = 0_u32;
    let mut discovery_retry_after_ms = 0_i64;
    let mut discovery_log_state = DegradedLogState::default();
    let mut stream_log_state = DegradedLogState::default();
    loop {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let (asset_ids_all, market_ids_all, tracked_markets) = if now_ms < discovery_retry_after_ms
        {
            if let Some(cached) = last_discovered_targets.clone() {
                cached
            } else {
                let wait_ms = discovery_retry_after_ms.saturating_sub(now_ms).max(1_000);
                sleep(Duration::from_millis(wait_ms as u64)).await;
                continue;
            }
        } else {
            match discover_subscription_targets(api.as_ref(), &state, cfg.market_discovery_limit)
                .await
            {
                Ok(v) => {
                    last_discovered_targets = Some(v.clone());
                    discovery_failure_streak = 0;
                    discovery_retry_after_ms = 0;
                    discovery_log_state.mark_recovered(
                        "polymarket_ws_market_discovery_recovered",
                        json!({
                            "shard_idx": shard_idx,
                            "shard_count": shard_count,
                            "asset_count": v.0.len(),
                            "market_count": v.1.len(),
                            "tracked_markets": v.2
                        }),
                    );
                    v
                }
                Err(e) => {
                    discovery_failure_streak = discovery_failure_streak.saturating_add(1);
                    let discovery_wait_sec = discovery_backoff_sec(discovery_failure_streak);
                    discovery_retry_after_ms = now_ms.saturating_add(
                        i64::try_from(discovery_wait_sec.saturating_mul(1_000))
                            .ok()
                            .unwrap_or(300_000),
                    );
                    if let Some((cached_assets, cached_markets, cached_tracked)) =
                        last_discovered_targets.clone()
                    {
                        discovery_log_state.mark_degraded(
                            "polymarket_ws_market_discovery_degraded",
                            "polymarket_ws_market_discovery_degraded_heartbeat",
                            "discovery_failed_using_cache",
                            json!({
                                "error": e.to_string(),
                                "shard_idx": shard_idx,
                                "shard_count": shard_count,
                                "cached_asset_count": cached_assets.len(),
                                "cached_market_count": cached_markets.len(),
                                "cached_tracked_markets": cached_tracked,
                                "discovery_failure_streak": discovery_failure_streak,
                                "next_discovery_wait_sec": discovery_wait_sec
                            }),
                        );
                        (cached_assets, cached_markets, cached_tracked)
                    } else {
                        state.set_market_connected(shard_idx, false);
                        discovery_log_state.mark_degraded(
                            "polymarket_ws_market_discovery_degraded",
                            "polymarket_ws_market_discovery_degraded_heartbeat",
                            "discovery_failed_no_cache",
                            json!({
                                "error": e.to_string(),
                                "shard_idx": shard_idx,
                                "shard_count": shard_count,
                                "discovery_failure_streak": discovery_failure_streak,
                                "next_discovery_wait_sec": discovery_wait_sec
                            }),
                        );
                        sleep(Duration::from_secs(discovery_wait_sec)).await;
                        continue;
                    }
                }
            }
        };
        let asset_ids = shard_vec(asset_ids_all.as_slice(), shard_idx, shard_count);
        let market_ids = shard_vec(market_ids_all.as_slice(), shard_idx, shard_count);

        if asset_ids.is_empty() {
            state.clear_market_connected(shard_idx);
            log_event(
                "polymarket_ws_market_shard_empty",
                json!({
                    "shard_idx": shard_idx,
                    "shard_count": shard_count,
                    "asset_count_all": asset_ids_all.len(),
                    "market_count_all": market_ids_all.len(),
                    "tracked_markets": tracked_markets
                }),
            );
            sleep(Duration::from_secs(cfg.refresh_sec)).await;
            continue;
        }

        let client = match ws::Client::new(cfg.endpoint.as_str(), WsConnectionConfig::default()) {
            Ok(v) => v,
            Err(e) => {
                state.set_market_connected(shard_idx, false);
                log_event(
                    "polymarket_ws_market_client_create_failed",
                    json!({
                        "error": e.to_string(),
                        "endpoint": cfg.endpoint
                    }),
                );
                sleep(Duration::from_secs(backoff_sec)).await;
                backoff_sec = (backoff_sec * 2).min(cfg.backoff_max_sec);
                continue;
            }
        };

        let book_stream = match client.subscribe_orderbook(asset_ids.clone()) {
            Ok(v) => v,
            Err(e) => {
                state.set_market_connected(shard_idx, false);
                log_event(
                    "polymarket_ws_market_subscribe_failed",
                    json!({
                        "error": e.to_string(),
                        "asset_count": asset_ids.len(),
                        "asset_count_all": asset_ids_all.len(),
                        "market_count": market_ids.len(),
                        "market_count_all": market_ids_all.len(),
                        "tracked_markets": tracked_markets
                        ,
                        "shard_idx": shard_idx,
                        "shard_count": shard_count
                    }),
                );
                sleep(Duration::from_secs(backoff_sec)).await;
                backoff_sec = (backoff_sec * 2).min(cfg.backoff_max_sec);
                continue;
            }
        };
        let trade_stream = match client.subscribe_last_trade_price(asset_ids.clone()) {
            Ok(v) => v,
            Err(e) => {
                state.set_market_connected(shard_idx, false);
                log_event(
                    "polymarket_ws_market_trade_subscribe_failed",
                    json!({
                        "error": e.to_string(),
                        "asset_count": asset_ids.len(),
                        "asset_count_all": asset_ids_all.len(),
                        "market_count": market_ids.len(),
                        "market_count_all": market_ids_all.len(),
                        "tracked_markets": tracked_markets
                        ,
                        "shard_idx": shard_idx,
                        "shard_count": shard_count
                    }),
                );
                sleep(Duration::from_secs(backoff_sec)).await;
                backoff_sec = (backoff_sec * 2).min(cfg.backoff_max_sec);
                continue;
            }
        };

        state.set_market_connected(shard_idx, true);
        stream_log_state.mark_recovered(
            "polymarket_ws_market_stream_recovered",
            json!({
                "shard_idx": shard_idx,
                "shard_count": shard_count,
                "asset_count": asset_ids.len(),
                "market_count": market_ids.len()
            }),
        );
        log_event(
            "polymarket_ws_market_subscribed",
            json!({
                "asset_count": asset_ids.len(),
                "asset_count_all": asset_ids_all.len(),
                "market_count": market_ids.len(),
                "market_count_all": market_ids_all.len(),
                "tracked_markets": tracked_markets
                ,
                "shard_idx": shard_idx,
                "shard_count": shard_count
            }),
        );

        let mut book_stream = Box::pin(book_stream);
        let mut trade_stream = Box::pin(trade_stream);
        let mut refresh_interval = tokio::time::interval(Duration::from_secs(cfg.refresh_sec));
        refresh_interval.tick().await;
        let mut refresh_reconnect = false;
        let mut pending_target_change: Option<PendingMarketTargetChange> = None;
        let stream_started_ms = chrono::Utc::now().timestamp_millis();
        let mut last_lag_event_ms: Option<i64> = None;
        let mut lag_window_start_ms = chrono::Utc::now().timestamp_millis();
        let mut lag_error_count = 0_u32;
        let mut lag_ignored_count = 0_u32;
        let mut lag_ignored_missed_max = 0_u32;
        let mut lag_ignored_last_log_ms = 0_i64;

        loop {
            tokio::select! {
                _ = refresh_interval.tick() => {
                    if cfg.reconnect_on_refresh {
                        refresh_reconnect = true;
                        log_event(
                            "polymarket_ws_market_refresh_reconnect",
                            json!({
                                "reason": "periodic_refresh",
                                "asset_count": asset_ids.len(),
                                "market_count": market_ids.len(),
                                "refresh_sec": cfg.refresh_sec
                            }),
                        );
                        break;
                    }
                    match discover_subscription_targets(
                        api.as_ref(),
                        &state,
                        cfg.market_discovery_limit,
                    )
                    .await
                    {
                        Ok((next_asset_ids_all, next_market_ids_all, next_tracked_markets)) => {
                            let next_asset_ids =
                                shard_vec(next_asset_ids_all.as_slice(), shard_idx, shard_count);
                            let next_market_ids =
                                shard_vec(next_market_ids_all.as_slice(), shard_idx, shard_count);
                            if next_asset_ids != asset_ids || next_market_ids != market_ids {
                                let now_ms = chrono::Utc::now().timestamp_millis();
                                let asset_delta_bps =
                                    symmetric_delta_bps(asset_ids.as_slice(), next_asset_ids.as_slice());
                                let market_delta_bps =
                                    symmetric_delta_bps(market_ids.as_slice(), next_market_ids.as_slice());
                                let delta_bps = asset_delta_bps.max(market_delta_bps);
                                if delta_bps < cfg.target_change_min_delta_bps {
                                    log_event(
                                        "polymarket_ws_market_target_change_ignored_small_delta",
                                        json!({
                                            "asset_delta_bps": asset_delta_bps,
                                            "market_delta_bps": market_delta_bps,
                                            "delta_bps": delta_bps,
                                            "min_delta_bps": cfg.target_change_min_delta_bps,
                                            "shard_idx": shard_idx,
                                            "shard_count": shard_count
                                        }),
                                    );
                                    pending_target_change = None;
                                    continue;
                                }
                                let mut confirmations = 1_u32;
                                let mut first_seen_ms = now_ms;
                                let mut max_delta_bps = delta_bps;
                                if let Some(existing) = pending_target_change.as_mut() {
                                    if existing.asset_ids == next_asset_ids
                                        && existing.market_ids == next_market_ids
                                    {
                                        existing.confirmations = existing.confirmations.saturating_add(1);
                                        existing.delta_bps = existing.delta_bps.max(delta_bps);
                                        confirmations = existing.confirmations;
                                        first_seen_ms = existing.first_seen_ms;
                                        max_delta_bps = existing.delta_bps;
                                    } else {
                                        *existing = PendingMarketTargetChange {
                                            asset_ids: next_asset_ids.clone(),
                                            market_ids: next_market_ids.clone(),
                                            first_seen_ms: now_ms,
                                            confirmations: 1,
                                            delta_bps,
                                        };
                                    }
                                } else {
                                    pending_target_change = Some(PendingMarketTargetChange {
                                        asset_ids: next_asset_ids.clone(),
                                        market_ids: next_market_ids.clone(),
                                        first_seen_ms: now_ms,
                                        confirmations: 1,
                                        delta_bps,
                                    });
                                }
                                let hold_elapsed_ms = now_ms.saturating_sub(first_seen_ms);
                                let hold_required_ms = i64::try_from(
                                    cfg.target_change_min_hold_sec.saturating_mul(1_000),
                                )
                                .ok()
                                .unwrap_or(0)
                                .max(0);
                                if confirmations < cfg.target_change_debounce_scans
                                    || hold_elapsed_ms < hold_required_ms
                                {
                                    log_event(
                                        "polymarket_ws_market_target_change_debounced",
                                        json!({
                                            "confirmations": confirmations,
                                            "required_confirmations": cfg.target_change_debounce_scans,
                                            "hold_elapsed_ms": hold_elapsed_ms,
                                            "hold_required_ms": hold_required_ms,
                                            "asset_delta_bps": asset_delta_bps,
                                            "market_delta_bps": market_delta_bps,
                                            "delta_bps": max_delta_bps,
                                            "shard_idx": shard_idx,
                                            "shard_count": shard_count
                                        }),
                                    );
                                    continue;
                                }
                                refresh_reconnect = true;
                                log_event(
                                    "polymarket_ws_market_refresh_reconnect",
                                    json!({
                                        "reason": "subscription_target_changed",
                                        "prev_asset_count": asset_ids.len(),
                                        "next_asset_count": next_asset_ids.len(),
                                        "prev_market_count": market_ids.len(),
                                        "next_market_count": next_market_ids.len(),
                                        "prev_tracked_markets": tracked_markets,
                                        "next_tracked_markets": next_tracked_markets,
                                        "confirmations": confirmations,
                                        "required_confirmations": cfg.target_change_debounce_scans,
                                        "hold_elapsed_ms": hold_elapsed_ms,
                                        "hold_required_ms": hold_required_ms,
                                        "delta_bps": max_delta_bps,
                                        "min_delta_bps": cfg.target_change_min_delta_bps,
                                        "shard_idx": shard_idx,
                                        "shard_count": shard_count
                                    }),
                                );
                                break;
                            } else if pending_target_change.is_some() {
                                pending_target_change = None;
                                log_event(
                                    "polymarket_ws_market_target_change_cleared",
                                    json!({
                                        "shard_idx": shard_idx,
                                        "shard_count": shard_count
                                    }),
                                );
                            }
                        }
                        Err(e) => {
                            log_event(
                                "polymarket_ws_market_refresh_discovery_failed",
                                json!({
                                        "error": e.to_string(),
                                        "asset_count": asset_ids.len(),
                                        "market_count": market_ids.len(),
                                        "shard_idx": shard_idx,
                                        "shard_count": shard_count
                                    }),
                                );
                            }
                    }
                }
                next_msg = book_stream.next() => {
                    match next_msg {
                        Some(Ok(book_update)) => {
                            state.apply_book_update(book_update).await;
                        }
                        Some(Err(e)) => {
                            let err_text = e.to_string();
                            let lower = err_text.to_ascii_lowercase();
                            if lower.contains("lagged") {
                                let missed_messages =
                                    parse_missed_messages_count(err_text.as_str()).unwrap_or(0);
                                let now_ms = chrono::Utc::now().timestamp_millis();
                                last_lag_event_ms = Some(now_ms);
                                if missed_messages > 0
                                    && missed_messages
                                        <= cfg.market_lag_ignore_missed_messages
                                {
                                    lag_ignored_count = lag_ignored_count.saturating_add(1);
                                    lag_ignored_missed_max = lag_ignored_missed_max.max(missed_messages);
                                    let log_interval_ms = i64::try_from(
                                        cfg.market_lag_ignored_log_min_interval_ms,
                                    )
                                    .ok()
                                    .unwrap_or(0)
                                    .max(0);
                                    if log_interval_ms == 0
                                        || now_ms.saturating_sub(lag_ignored_last_log_ms)
                                            >= log_interval_ms
                                    {
                                        log_event(
                                            "polymarket_ws_market_stream_lag_ignored",
                                            json!({
                                                "error": err_text,
                                                "asset_count": asset_ids.len(),
                                                "missed_messages": missed_messages,
                                                "missed_messages_max": lag_ignored_missed_max,
                                                "ignored_events": lag_ignored_count,
                                                "ignore_threshold": cfg.market_lag_ignore_missed_messages,
                                                "shard_idx": shard_idx,
                                                "shard_count": shard_count
                                            }),
                                        );
                                        lag_ignored_last_log_ms = now_ms;
                                        lag_ignored_count = 0;
                                        lag_ignored_missed_max = 0;
                                    }
                                    continue;
                                }
                                let lag_window_ms = i64::try_from(
                                    cfg.market_lag_window_sec.saturating_mul(1_000),
                                )
                                .ok()
                                .unwrap_or(20_000)
                                .max(1_000);
                                if now_ms.saturating_sub(lag_window_start_ms) >= lag_window_ms {
                                    lag_window_start_ms = now_ms;
                                    lag_error_count = 0;
                                }
                                lag_error_count = lag_error_count.saturating_add(1);
                                log_event(
                                    "polymarket_ws_market_stream_lag_soft",
                                    json!({
                                        "error": err_text,
                                        "asset_count": asset_ids.len(),
                                        "missed_messages": missed_messages,
                                        "hard_missed_threshold": cfg.market_lag_hard_missed_messages,
                                        "lag_errors_in_window": lag_error_count,
                                        "lag_error_window_sec": cfg.market_lag_window_sec,
                                        "lag_error_soft_limit": cfg.market_lag_soft_errors,
                                        "shard_idx": shard_idx,
                                        "shard_count": shard_count
                                    }),
                                );
                                let hard_missed = missed_messages >= cfg.market_lag_hard_missed_messages;
                                if lag_error_count < cfg.market_lag_soft_errors || !hard_missed {
                                    continue;
                                }
                                let reconnect_cooldown_ms = i64::try_from(
                                    cfg.market_lag_reconnect_cooldown_sec.saturating_mul(1_000),
                                )
                                .ok()
                                .unwrap_or(60_000)
                                .max(1_000);
                                if now_ms.saturating_sub(last_lag_reconnect_ms)
                                    < reconnect_cooldown_ms
                                {
                                    log_event(
                                        "polymarket_ws_market_lag_reconnect_suppressed",
                                        json!({
                                            "asset_count": asset_ids.len(),
                                            "missed_messages": missed_messages,
                                            "lag_errors_in_window": lag_error_count,
                                            "cooldown_ms": reconnect_cooldown_ms,
                                            "last_lag_reconnect_ms": last_lag_reconnect_ms,
                                            "shard_idx": shard_idx,
                                            "shard_count": shard_count
                                        }),
                                    );
                                    continue;
                                }
                                last_lag_reconnect_ms = now_ms;
                            }
                            stream_log_state.mark_degraded(
                                "polymarket_ws_market_stream_degraded",
                                "polymarket_ws_market_stream_degraded_heartbeat",
                                "book_stream_error",
                                json!({
                                    "error": err_text,
                                    "asset_count": asset_ids.len(),
                                    "shard_idx": shard_idx,
                                    "shard_count": shard_count
                                }),
                            );
                            break;
                        }
                        None => {
                            stream_log_state.mark_degraded(
                                "polymarket_ws_market_stream_degraded",
                                "polymarket_ws_market_stream_degraded_heartbeat",
                                "book_stream_closed",
                                json!({
                                    "asset_count": asset_ids.len(),
                                    "shard_idx": shard_idx,
                                    "shard_count": shard_count
                                }),
                            );
                            break;
                        }
                    }
                }
                next_trade = trade_stream.next() => {
                    match next_trade {
                        Some(Ok(trade)) => {
                            state.apply_market_trade_update(trade).await;
                        }
                        Some(Err(e)) => {
                            stream_log_state.mark_degraded(
                                "polymarket_ws_market_stream_degraded",
                                "polymarket_ws_market_stream_degraded_heartbeat",
                                "trade_stream_error",
                                json!({
                                    "error": e.to_string(),
                                    "asset_count": asset_ids.len(),
                                    "shard_idx": shard_idx,
                                    "shard_count": shard_count
                                }),
                            );
                            break;
                        }
                        None => {
                            stream_log_state.mark_degraded(
                                "polymarket_ws_market_stream_degraded",
                                "polymarket_ws_market_stream_degraded_heartbeat",
                                "trade_stream_closed",
                                json!({
                                    "asset_count": asset_ids.len(),
                                    "shard_idx": shard_idx,
                                    "shard_count": shard_count
                                }),
                            );
                            break;
                        }
                    }
                }
            }
        }

        if lag_ignored_count > 0 {
            log_event(
                "polymarket_ws_market_stream_lag_ignored",
                json!({
                    "asset_count": asset_ids.len(),
                    "missed_messages_max": lag_ignored_missed_max,
                    "ignored_events": lag_ignored_count,
                    "ignore_threshold": cfg.market_lag_ignore_missed_messages,
                    "shard_idx": shard_idx,
                    "shard_count": shard_count,
                    "flush": true
                }),
            );
        }
        state.set_market_connected(shard_idx, false);
        state.prune_stale(cfg.prune_after_ms).await;
        if !refresh_reconnect {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let unstable_window_ms = i64::try_from(cfg.market_unstable_window_sec)
                .ok()
                .and_then(|v| v.checked_mul(1_000))
                .unwrap_or(90_000)
                .max(1_000);
            let stream_uptime_ms = now_ms.saturating_sub(stream_started_ms);
            let lag_recent = last_lag_event_ms
                .map(|ts| now_ms.saturating_sub(ts) <= unstable_window_ms)
                .unwrap_or(false);

            if stream_uptime_ms >= unstable_window_ms && !lag_recent {
                backoff_sec = cfg.backoff_min_sec;
            } else {
                backoff_sec = (backoff_sec.max(cfg.backoff_min_sec) * 2).min(cfg.backoff_max_sec);
            }

            let jitter_ms = if cfg.market_reconnect_jitter_ms > 0 {
                let span = i64::try_from(cfg.market_reconnect_jitter_ms)
                    .ok()
                    .unwrap_or(0)
                    .max(0);
                if span == 0 {
                    0_u64
                } else {
                    let seed = now_ms
                        ^ i64::try_from(shard_idx)
                            .ok()
                            .and_then(|v| v.checked_mul(1_103_515_245))
                            .unwrap_or(0);
                    let offset = seed.unsigned_abs() % (u64::try_from(span).ok().unwrap_or(0) + 1);
                    offset
                }
            } else {
                0_u64
            };

            stream_log_state.mark_degraded(
                "polymarket_ws_market_stream_degraded",
                "polymarket_ws_market_stream_degraded_heartbeat",
                "reconnect_backoff",
                json!({
                    "shard_idx": shard_idx,
                    "shard_count": shard_count,
                    "stream_uptime_ms": stream_uptime_ms,
                    "lag_recent": lag_recent,
                    "backoff_sec": backoff_sec,
                    "jitter_ms": jitter_ms
                }),
            );
            sleep(Duration::from_secs(backoff_sec) + Duration::from_millis(jitter_ms)).await;
        }
    }
}

async fn run_user_loop(
    api: Arc<PolymarketApi>,
    cfg: PolymarketWsConfig,
    state: SharedPolymarketWsState,
) {
    let mut backoff_sec = cfg.backoff_min_sec;
    let mut last_market_targets: Option<(Vec<B256>, usize)> = None;
    let mut discovery_failure_streak = 0_u32;
    let mut discovery_retry_after_ms = 0_i64;
    let mut discovery_log_state = DegradedLogState::default();
    let mut user_stream_log_state = DegradedLogState::default();
    loop {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let (_, market_ids, tracked_markets) = if now_ms < discovery_retry_after_ms {
            if let Some((cached_market_ids, cached_tracked)) = last_market_targets.clone() {
                (Vec::new(), cached_market_ids, cached_tracked)
            } else {
                let wait_ms = discovery_retry_after_ms.saturating_sub(now_ms).max(1_000);
                sleep(Duration::from_millis(wait_ms as u64)).await;
                continue;
            }
        } else {
            match discover_subscription_targets(api.as_ref(), &state, cfg.market_discovery_limit)
                .await
            {
                Ok(v) => {
                    last_market_targets = Some((v.1.clone(), v.2));
                    discovery_failure_streak = 0;
                    discovery_retry_after_ms = 0;
                    discovery_log_state.mark_recovered(
                        "polymarket_ws_user_discovery_recovered",
                        json!({
                            "market_count": v.1.len(),
                            "tracked_markets": v.2
                        }),
                    );
                    v
                }
                Err(e) => {
                    discovery_failure_streak = discovery_failure_streak.saturating_add(1);
                    let discovery_wait_sec = discovery_backoff_sec(discovery_failure_streak);
                    discovery_retry_after_ms = now_ms.saturating_add(
                        i64::try_from(discovery_wait_sec.saturating_mul(1_000))
                            .ok()
                            .unwrap_or(300_000),
                    );
                    if let Some((cached_market_ids, cached_tracked)) = last_market_targets.clone() {
                        discovery_log_state.mark_degraded(
                            "polymarket_ws_user_discovery_degraded",
                            "polymarket_ws_user_discovery_degraded_heartbeat",
                            "discovery_failed_using_cache",
                            json!({
                                "error": e.to_string(),
                                "cached_market_count": cached_market_ids.len(),
                                "cached_tracked_markets": cached_tracked,
                                "discovery_failure_streak": discovery_failure_streak,
                                "next_discovery_wait_sec": discovery_wait_sec
                            }),
                        );
                        (Vec::new(), cached_market_ids, cached_tracked)
                    } else {
                        state.set_user_connected(false);
                        discovery_log_state.mark_degraded(
                            "polymarket_ws_user_discovery_degraded",
                            "polymarket_ws_user_discovery_degraded_heartbeat",
                            "discovery_failed_no_cache",
                            json!({
                                "error": e.to_string(),
                                "discovery_failure_streak": discovery_failure_streak,
                                "next_discovery_wait_sec": discovery_wait_sec
                            }),
                        );
                        sleep(Duration::from_secs(discovery_wait_sec)).await;
                        continue;
                    }
                }
            }
        };
        if market_ids.is_empty() {
            state.set_user_connected(false);
            sleep(Duration::from_secs(cfg.refresh_sec)).await;
            continue;
        }

        let (credentials, address) = match api.ws_auth_context().await {
            Ok(v) => v,
            Err(e) => {
                state.set_user_connected(false);
                log_event(
                    "polymarket_ws_user_auth_context_failed",
                    json!({
                        "error": e.to_string()
                    }),
                );
                sleep(Duration::from_secs(backoff_sec)).await;
                backoff_sec = (backoff_sec * 2).min(cfg.backoff_max_sec);
                continue;
            }
        };

        let unauth_client =
            match ws::Client::new(cfg.endpoint.as_str(), WsConnectionConfig::default()) {
                Ok(v) => v,
                Err(e) => {
                    state.set_user_connected(false);
                    log_event(
                        "polymarket_ws_user_client_create_failed",
                        json!({
                            "error": e.to_string(),
                            "endpoint": cfg.endpoint
                        }),
                    );
                    sleep(Duration::from_secs(backoff_sec)).await;
                    backoff_sec = (backoff_sec * 2).min(cfg.backoff_max_sec);
                    continue;
                }
            };

        let user_client = match unauth_client.authenticate(credentials, address) {
            Ok(v) => v,
            Err(e) => {
                state.set_user_connected(false);
                log_event(
                    "polymarket_ws_user_authenticate_failed",
                    json!({
                        "error": e.to_string()
                    }),
                );
                sleep(Duration::from_secs(backoff_sec)).await;
                backoff_sec = (backoff_sec * 2).min(cfg.backoff_max_sec);
                continue;
            }
        };

        let stream = match user_client.subscribe_user_events(market_ids.clone()) {
            Ok(v) => v,
            Err(e) => {
                state.set_user_connected(false);
                log_event(
                    "polymarket_ws_user_subscribe_failed",
                    json!({
                        "error": e.to_string(),
                        "market_count": market_ids.len(),
                        "tracked_markets": tracked_markets
                    }),
                );
                sleep(Duration::from_secs(backoff_sec)).await;
                backoff_sec = (backoff_sec * 2).min(cfg.backoff_max_sec);
                continue;
            }
        };

        backoff_sec = cfg.backoff_min_sec;
        state.set_user_connected(true);
        user_stream_log_state.mark_recovered(
            "polymarket_ws_user_stream_recovered",
            json!({
                "market_count": market_ids.len(),
                "tracked_markets": tracked_markets
            }),
        );
        log_event(
            "polymarket_ws_user_subscribed",
            json!({
                "market_count": market_ids.len(),
                "tracked_markets": tracked_markets
            }),
        );

        let mut stream = Box::pin(stream);
        let mut refresh_interval = tokio::time::interval(Duration::from_secs(cfg.refresh_sec));
        refresh_interval.tick().await;
        let mut refresh_reconnect = false;
        let mut pending_target_change: Option<PendingUserTargetChange> = None;
        loop {
            tokio::select! {
                _ = refresh_interval.tick() => {
                    if cfg.reconnect_on_refresh {
                        refresh_reconnect = true;
                        log_event(
                            "polymarket_ws_user_refresh_reconnect",
                            json!({
                                "reason": "periodic_refresh",
                                "market_count": market_ids.len(),
                                "refresh_sec": cfg.refresh_sec
                            }),
                        );
                        break;
                    }
                    match discover_subscription_targets(
                        api.as_ref(),
                        &state,
                        cfg.market_discovery_limit,
                    )
                    .await
                    {
                        Ok((_, next_market_ids, next_tracked_markets)) => {
                            if next_market_ids != market_ids {
                                let now_ms = chrono::Utc::now().timestamp_millis();
                                let delta_bps = symmetric_delta_bps(
                                    market_ids.as_slice(),
                                    next_market_ids.as_slice(),
                                );
                                if delta_bps < cfg.target_change_min_delta_bps {
                                    log_event(
                                        "polymarket_ws_user_target_change_ignored_small_delta",
                                        json!({
                                            "delta_bps": delta_bps,
                                            "min_delta_bps": cfg.target_change_min_delta_bps
                                        }),
                                    );
                                    pending_target_change = None;
                                    continue;
                                }
                                let mut confirmations = 1_u32;
                                let mut first_seen_ms = now_ms;
                                let mut max_delta_bps = delta_bps;
                                if let Some(existing) = pending_target_change.as_mut() {
                                    if existing.market_ids == next_market_ids {
                                        existing.confirmations = existing.confirmations.saturating_add(1);
                                        existing.delta_bps = existing.delta_bps.max(delta_bps);
                                        confirmations = existing.confirmations;
                                        first_seen_ms = existing.first_seen_ms;
                                        max_delta_bps = existing.delta_bps;
                                    } else {
                                        *existing = PendingUserTargetChange {
                                            market_ids: next_market_ids.clone(),
                                            first_seen_ms: now_ms,
                                            confirmations: 1,
                                            delta_bps,
                                        };
                                    }
                                } else {
                                    pending_target_change = Some(PendingUserTargetChange {
                                        market_ids: next_market_ids.clone(),
                                        first_seen_ms: now_ms,
                                        confirmations: 1,
                                        delta_bps,
                                    });
                                }
                                let hold_elapsed_ms = now_ms.saturating_sub(first_seen_ms);
                                let hold_required_ms = i64::try_from(
                                    cfg.target_change_min_hold_sec.saturating_mul(1_000),
                                )
                                .ok()
                                .unwrap_or(0)
                                .max(0);
                                if confirmations < cfg.target_change_debounce_scans
                                    || hold_elapsed_ms < hold_required_ms
                                {
                                    log_event(
                                        "polymarket_ws_user_target_change_debounced",
                                        json!({
                                            "confirmations": confirmations,
                                            "required_confirmations": cfg.target_change_debounce_scans,
                                            "hold_elapsed_ms": hold_elapsed_ms,
                                            "hold_required_ms": hold_required_ms,
                                            "delta_bps": max_delta_bps
                                        }),
                                    );
                                    continue;
                                }
                                refresh_reconnect = true;
                                log_event(
                                    "polymarket_ws_user_refresh_reconnect",
                                    json!({
                                        "reason": "subscription_target_changed",
                                        "prev_market_count": market_ids.len(),
                                        "next_market_count": next_market_ids.len(),
                                        "prev_tracked_markets": tracked_markets,
                                        "next_tracked_markets": next_tracked_markets,
                                        "confirmations": confirmations,
                                        "required_confirmations": cfg.target_change_debounce_scans,
                                        "hold_elapsed_ms": hold_elapsed_ms,
                                        "hold_required_ms": hold_required_ms,
                                        "delta_bps": max_delta_bps,
                                        "min_delta_bps": cfg.target_change_min_delta_bps
                                    }),
                                );
                                break;
                            } else if pending_target_change.is_some() {
                                pending_target_change = None;
                                log_event("polymarket_ws_user_target_change_cleared", json!({}));
                            }
                        }
                        Err(e) => {
                            log_event(
                                "polymarket_ws_user_refresh_discovery_failed",
                                json!({
                                    "error": e.to_string(),
                                    "market_count": market_ids.len()
                                }),
                            );
                        }
                    }
                }
                next_msg = stream.next() => {
                    match next_msg {
                        Some(Ok(WsMessage::Order(order))) => {
                            state.apply_order_update(order).await;
                        }
                        Some(Ok(WsMessage::Trade(trade))) => {
                            state.apply_trade_update(trade).await;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            user_stream_log_state.mark_degraded(
                                "polymarket_ws_user_stream_degraded",
                                "polymarket_ws_user_stream_degraded_heartbeat",
                                "stream_error",
                                json!({
                                    "error": e.to_string(),
                                    "market_count": market_ids.len()
                                }),
                            );
                            break;
                        }
                        None => {
                            user_stream_log_state.mark_degraded(
                                "polymarket_ws_user_stream_degraded",
                                "polymarket_ws_user_stream_degraded_heartbeat",
                                "stream_closed",
                                json!({
                                    "market_count": market_ids.len()
                                }),
                            );
                            break;
                        }
                    }
                }
            }
        }
        state.set_user_connected(false);
        state.prune_stale(cfg.prune_after_ms).await;
        if !refresh_reconnect {
            sleep(Duration::from_secs(backoff_sec)).await;
            backoff_sec = (backoff_sec * 2).min(cfg.backoff_max_sec);
        }
    }
}

fn status_from_order_message(order: &OrderMessage) -> OrderStatusType {
    if let Some(status) = order.status.clone() {
        return status;
    }
    match order.msg_type.clone() {
        Some(OrderMessageType::Cancellation) => OrderStatusType::Canceled,
        Some(OrderMessageType::Placement) => OrderStatusType::Live,
        Some(OrderMessageType::Update) => {
            if order
                .size_matched
                .map(|v| v > Decimal::ZERO)
                .unwrap_or(false)
            {
                OrderStatusType::Matched
            } else {
                OrderStatusType::Live
            }
        }
        Some(OrderMessageType::Unknown(raw)) => OrderStatusType::Unknown(raw),
        Some(_) => OrderStatusType::Live,
        None => {
            if order
                .size_matched
                .map(|v| v > Decimal::ZERO)
                .unwrap_or(false)
            {
                OrderStatusType::Matched
            } else {
                OrderStatusType::Live
            }
        }
    }
}

fn parse_missed_messages_count(err_text: &str) -> Option<u32> {
    let marker = "missed ";
    let start = err_text.to_ascii_lowercase().find(marker)?;
    let tail = &err_text[start + marker.len()..];
    let digits = tail
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u32>().ok()
}

fn symmetric_diff_count_sorted<T: Ord>(left: &[T], right: &[T]) -> usize {
    let mut i = 0usize;
    let mut j = 0usize;
    let mut diff = 0usize;
    while i < left.len() && j < right.len() {
        match left[i].cmp(&right[j]) {
            std::cmp::Ordering::Less => {
                diff = diff.saturating_add(1);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                diff = diff.saturating_add(1);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    diff = diff.saturating_add(left.len().saturating_sub(i));
    diff = diff.saturating_add(right.len().saturating_sub(j));
    diff
}

fn symmetric_delta_bps<T: Ord>(left: &[T], right: &[T]) -> u32 {
    let diff = symmetric_diff_count_sorted(left, right);
    let denom = left.len().max(right.len()).max(1);
    let bps =
        (u64::try_from(diff).ok().unwrap_or(0) * 10_000) / u64::try_from(denom).ok().unwrap_or(1);
    u32::try_from(bps).ok().unwrap_or(u32::MAX)
}

fn shard_vec<T: Clone>(items: &[T], shard_idx: usize, shard_count: usize) -> Vec<T> {
    if shard_count <= 1 {
        return items.to_vec();
    }
    items
        .iter()
        .cloned()
        .enumerate()
        .filter_map(|(idx, item)| {
            if idx % shard_count == shard_idx {
                Some(item)
            } else {
                None
            }
        })
        .collect()
}

async fn discover_subscription_targets(
    api: &PolymarketApi,
    ws_state: &SharedPolymarketWsState,
    limit: u32,
) -> anyhow::Result<(Vec<U256>, Vec<B256>, usize)> {
    let (extra_assets, extra_markets) = ws_state.subscription_scope_targets_snapshot();
    if !extra_assets.is_empty() && !extra_markets.is_empty() {
        let mut asset_vec = extra_assets;
        asset_vec.sort();
        asset_vec.dedup();
        let mut market_vec = extra_markets;
        market_vec.sort();
        market_vec.dedup();
        return Ok((asset_vec, market_vec.clone(), market_vec.len()));
    }

    let (fallback_assets, fallback_markets, fallback_tracked) =
        discover_updown_slug_targets(api).await;
    if !fallback_assets.is_empty() && !fallback_markets.is_empty() {
        return Ok((fallback_assets, fallback_markets, fallback_tracked));
    }

    let mut active_discovery_error: Option<anyhow::Error> = None;
    let markets = match api.get_all_active_markets(limit).await {
        Ok(markets) => markets,
        Err(e) => {
            active_discovery_error =
                Some(e.context("discover active markets for ws (primary feed)"));
            Vec::new()
        }
    };
    let mut asset_ids: HashSet<U256> = HashSet::new();
    let mut market_ids: HashSet<B256> = HashSet::new();
    let mut tracked_markets = 0usize;

    for market in markets {
        if !is_tracked_symbol_market(&market) {
            continue;
        }
        tracked_markets += 1;
        if let Ok(market_id) = B256::from_str(market.condition_id.trim()) {
            market_ids.insert(market_id);
        }

        if let Some(raw_ids) = market.clob_token_ids.as_ref() {
            if let Ok(ids) = serde_json::from_str::<Vec<String>>(raw_ids) {
                for token_id in ids {
                    if let Some(asset_id) = parse_asset_id(token_id.as_str()) {
                        asset_ids.insert(asset_id);
                    }
                }
                continue;
            }
        }
        if let Some(tokens) = market.tokens.as_ref() {
            for token in tokens {
                if let Some(asset_id) = parse_asset_id(token.token_id.as_str()) {
                    asset_ids.insert(asset_id);
                }
            }
        }
    }

    for asset in extra_assets {
        asset_ids.insert(asset);
    }
    for market in extra_markets {
        market_ids.insert(market);
    }

    let mut asset_vec = asset_ids.into_iter().collect::<Vec<_>>();
    asset_vec.sort();
    let mut market_vec = market_ids.into_iter().collect::<Vec<_>>();
    market_vec.sort();

    if !asset_vec.is_empty() && !market_vec.is_empty() {
        return Ok((asset_vec, market_vec, tracked_markets));
    }

    if let Some(err) = active_discovery_error {
        return Err(err);
    }
    if asset_vec.is_empty() || market_vec.is_empty() {
        anyhow::bail!("discovery returned empty targets after fallback + active scan");
    }
    Ok((asset_vec, market_vec, tracked_markets))
}

async fn discover_updown_slug_targets(api: &PolymarketApi) -> (Vec<U256>, Vec<B256>, usize) {
    let now_ts = chrono::Utc::now().timestamp();
    let symbols = ["btc", "eth", "sol", "xrp"];
    let windows = [
        ("5m", 300_i64),
        ("15m", 900_i64),
        ("1h", 3_600_i64),
        ("4h", 14_400_i64),
    ];
    let shifts = [-1_i64, 0_i64, 1_i64];

    let mut asset_ids: HashSet<U256> = HashSet::new();
    let mut market_ids: HashSet<B256> = HashSet::new();
    let mut tracked = 0usize;

    for symbol in symbols {
        for (tf_label, step_sec) in windows {
            let base_open = (now_ts.div_euclid(step_sec)) * step_sec;
            for shift in shifts {
                let open_ts = base_open.saturating_add(shift * step_sec);
                if open_ts <= 0 {
                    continue;
                }
                let slug = format!("{symbol}-updown-{tf_label}-{open_ts}");
                let Ok(market) = api.get_market_by_slug(slug.as_str()).await else {
                    continue;
                };
                tracked += 1;
                if let Ok(market_id) = B256::from_str(market.condition_id.trim()) {
                    market_ids.insert(market_id);
                }
                if let Some(raw_ids) = market.clob_token_ids.as_ref() {
                    if let Ok(ids) = serde_json::from_str::<Vec<String>>(raw_ids) {
                        for token_id in ids {
                            if let Some(asset_id) = parse_asset_id(token_id.as_str()) {
                                asset_ids.insert(asset_id);
                            }
                        }
                        continue;
                    }
                }
                if let Some(tokens) = market.tokens.as_ref() {
                    for token in tokens {
                        if let Some(asset_id) = parse_asset_id(token.token_id.as_str()) {
                            asset_ids.insert(asset_id);
                        }
                    }
                }
            }
        }
    }

    let mut asset_vec = asset_ids.into_iter().collect::<Vec<_>>();
    asset_vec.sort();
    let mut market_vec = market_ids.into_iter().collect::<Vec<_>>();
    market_vec.sort();
    (asset_vec, market_vec, tracked)
}

fn parse_asset_id(raw: &str) -> Option<U256> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    U256::from_str(trimmed).ok()
}

fn is_tracked_symbol_market(market: &crate::models::Market) -> bool {
    let slug = market.slug.to_ascii_lowercase();
    let question = market.question.to_ascii_lowercase();
    matches_market_text(slug.as_str()) || matches_market_text(question.as_str())
}

fn matches_market_text(value: &str) -> bool {
    value.contains("btc-updown")
        || value.contains("bitcoin-up-or-down")
        || value.contains("eth-updown")
        || value.contains("ethereum-up-or-down")
        || value.contains("sol-updown")
        || value.contains("solana-up-or-down")
        || value.contains("xrp-updown")
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn ws_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_ws_env<F: FnOnce()>(updates: &[(&str, Option<&str>)], f: F) {
        let _guard = ws_env_lock().lock().expect("ws env lock poisoned");
        let mut previous: Vec<(&str, Option<String>)> = Vec::with_capacity(updates.len());
        for (name, value) in updates {
            previous.push((*name, std::env::var(name).ok()));
            unsafe {
                match value {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
        }
        f();
        for (name, value) in previous {
            unsafe {
                match value {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn parse_asset_id_accepts_decimal_token_ids() {
        let sample =
            "71983878769646543569771747914086054960230109850913433882779852271882956401020";
        assert!(parse_asset_id(sample).is_some());
    }

    #[test]
    fn tracked_symbol_filter_matches_btc_updown_slug() {
        let market = crate::models::Market {
            condition_id: "0x0000000000000000000000000000000000000000000000000000000000000001"
                .to_string(),
            market_id: None,
            question: "BTC test".to_string(),
            slug: "btc-updown-5m-1771764000".to_string(),
            description: None,
            resolution_source: None,
            end_date: None,
            end_date_iso: None,
            end_date_iso_alt: None,
            game_start_time: None,
            start_date: None,
            active: true,
            closed: false,
            tokens: None,
            clob_token_ids: None,
            outcomes: None,
            competitive: None,
        };
        assert!(is_tracked_symbol_market(&market));
    }

    #[test]
    fn parse_missed_messages_count_extracts_value() {
        let sample = "WebSocket: Subscription lagged, missed 10291 messages";
        assert_eq!(parse_missed_messages_count(sample), Some(10291));
    }

    #[test]
    fn shard_vec_even_split_indices() {
        let items = vec![0, 1, 2, 3, 4, 5];
        assert_eq!(shard_vec(items.as_slice(), 0, 2), vec![0, 2, 4]);
        assert_eq!(shard_vec(items.as_slice(), 1, 2), vec![1, 3, 5]);
    }

    #[test]
    fn market_connected_tracks_registered_shards() {
        let state = new_shared_polymarket_ws_state();
        assert!(!state.market_connected());

        state.set_market_connected(0, true);
        assert!(state.market_connected());

        state.set_market_connected(1, false);
        assert!(!state.market_connected());

        state.clear_market_connected(1);
        assert!(state.market_connected());

        state.clear_market_connected(0);
        assert!(!state.market_connected());
    }

    #[test]
    fn ws_discovery_defaults_are_safe() {
        with_ws_env(
            &[
                ("EVPOLY_PM_WS_MARKET_DISCOVERY_LIMIT", None),
                ("EVPOLY_PM_WS_REFRESH_SEC", None),
            ],
            || {
                let cfg = PolymarketWsConfig::default();
                assert_eq!(cfg.market_discovery_limit, 250);
                assert_eq!(cfg.refresh_sec, 90);
            },
        );
    }

    #[test]
    fn discovery_backoff_schedule_is_90_180_300_capped() {
        assert_eq!(discovery_backoff_sec(0), 90);
        assert_eq!(discovery_backoff_sec(1), 90);
        assert_eq!(discovery_backoff_sec(2), 180);
        assert_eq!(discovery_backoff_sec(3), 300);
        assert_eq!(discovery_backoff_sec(99), 300);
    }

    #[test]
    fn discovery_uses_scope_targets_without_active_market_scan() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let state = new_shared_polymarket_ws_state();
            state.set_subscription_scope_targets(
                "test-scope",
                &[
                    "71983878769646543569771747914086054960230109850913433882779852271882956401020"
                        .to_string(),
                ],
                &[
                    "0x0000000000000000000000000000000000000000000000000000000000000001"
                        .to_string(),
                ],
            );
            let api = crate::api::PolymarketApi::new(
                "http://127.0.0.1:1".to_string(),
                "http://127.0.0.1:1".to_string(),
                None,
                None,
                None,
            );
            let (asset_ids, market_ids, tracked_markets) =
                discover_subscription_targets(&api, &state, 250)
                    .await
                    .expect("scope discovery");
            assert_eq!(asset_ids.len(), 1);
            assert_eq!(market_ids.len(), 1);
            assert_eq!(tracked_markets, 1);
        });
    }
}
