use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::event_log::log_event;

const PRICE_SCALE: f64 = 10_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CoinbaseFeedState {
    Invalid,
    Resyncing,
    Warmup,
    Healthy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoinbaseBookLevel {
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoinbaseTradeSample {
    pub seq: u64,
    pub ts_ms: i64,
    pub price: f64,
    pub notional_usd: f64,
    pub exchange_seq: Option<u64>,
    pub trade_id: Option<u64>,
    pub seq_gap_after_prev: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoinbasePeriodAnchorSample {
    pub period_open_ts: i64,
    pub anchor_price: f64,
    pub source_trade_ts_ms: i64,
    pub source_exchange_seq: Option<u64>,
    pub seq_gap_detected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoinbaseBookState {
    pub feed_state: CoinbaseFeedState,
    pub product_id: String,
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
    pub mid_price: Option<f64>,
    pub spread: Option<f64>,
    pub top_bid_depth: Option<f64>,
    pub top_ask_depth: Option<f64>,
    pub imbalance: Option<f64>,
    pub book_update_rate_5s: Option<f64>,
    pub trade_notional_p95_1s: Option<f64>,
    pub last_trade_notional_usd: Option<f64>,
    pub last_trade_ts_ms: Option<i64>,
    pub last_qualifying_trade_notional_usd: Option<f64>,
    pub last_qualifying_trade_ts_ms: Option<i64>,
    pub last_qualifying_trade_seq: Option<u64>,
    pub max_trade_notional_15s: Option<f64>,
    pub max_trade_notional_15s_ts_ms: Option<i64>,
    pub trade_update_rate_5s: Option<f64>,
    pub recent_trades: Vec<CoinbaseTradeSample>,
    pub period_anchors_15m: Vec<CoinbasePeriodAnchorSample>,
    pub bid_levels: Vec<CoinbaseBookLevel>,
    pub ask_levels: Vec<CoinbaseBookLevel>,
    pub last_update_ms: i64,
    pub last_state_transition_ms: i64,
    pub state_reason: Option<String>,
}

impl Default for CoinbaseBookState {
    fn default() -> Self {
        Self {
            feed_state: CoinbaseFeedState::Invalid,
            product_id: "BTC-USD".to_string(),
            best_bid: None,
            best_ask: None,
            mid_price: None,
            spread: None,
            top_bid_depth: None,
            top_ask_depth: None,
            imbalance: None,
            book_update_rate_5s: None,
            trade_notional_p95_1s: None,
            last_trade_notional_usd: None,
            last_trade_ts_ms: None,
            last_qualifying_trade_notional_usd: None,
            last_qualifying_trade_ts_ms: None,
            last_qualifying_trade_seq: None,
            max_trade_notional_15s: None,
            max_trade_notional_15s_ts_ms: None,
            trade_update_rate_5s: None,
            recent_trades: Vec::new(),
            period_anchors_15m: Vec::new(),
            bid_levels: Vec::new(),
            ask_levels: Vec::new(),
            last_update_ms: 0,
            last_state_transition_ms: 0,
            state_reason: Some("init".to_string()),
        }
    }
}

pub type SharedCoinbaseBookState = Arc<tokio::sync::RwLock<CoinbaseBookState>>;
pub type SharedCoinbaseTradeNotify = Arc<tokio::sync::Notify>;

pub fn new_shared_coinbase_book_state() -> SharedCoinbaseBookState {
    Arc::new(tokio::sync::RwLock::new(CoinbaseBookState::default()))
}

pub fn new_shared_coinbase_trade_notify() -> SharedCoinbaseTradeNotify {
    Arc::new(tokio::sync::Notify::new())
}

#[derive(Debug, Clone)]
pub struct CoinbaseWsConfig {
    pub stream_url: String,
    pub product_id: String,
    pub channel: String,
    pub reconnect_min_sec: u64,
    pub reconnect_max_sec: u64,
    pub inactivity_timeout_sec: u64,
    pub warmup_sec: u64,
    pub depth_levels: usize,
}

impl Default for CoinbaseWsConfig {
    fn default() -> Self {
        Self {
            stream_url: std::env::var("EVPOLY_COINBASE_WS_URL")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "wss://ws-feed.exchange.coinbase.com".to_string()),
            product_id: std::env::var("EVPOLY_COINBASE_PRODUCT_ID")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "BTC-USD".to_string()),
            channel: std::env::var("EVPOLY_COINBASE_CHANNEL")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                // Coinbase Exchange: level2 now requires auth; level2_batch (aka level2_50) is public.
                .unwrap_or_else(|| "level2_batch".to_string()),
            reconnect_min_sec: env_u64("EVPOLY_COINBASE_RECONNECT_MIN_SEC", 1).max(1),
            reconnect_max_sec: env_u64("EVPOLY_COINBASE_RECONNECT_MAX_SEC", 20).max(2),
            inactivity_timeout_sec: env_u64("EVPOLY_COINBASE_INACTIVITY_TIMEOUT_SEC", 20).max(5),
            warmup_sec: env_u64("EVPOLY_COINBASE_WARMUP_SEC", 8).max(1),
            depth_levels: env_u64("EVPOLY_COINBASE_DEPTH_LEVELS", 10).max(1) as usize,
        }
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

#[derive(Debug, Deserialize)]
struct CoinbaseEnvelope {
    #[serde(rename = "type")]
    msg_type: String,
}

#[derive(Debug, Deserialize)]
struct CoinbaseSnapshotMsg {
    product_id: String,
    bids: Vec<Vec<String>>,
    asks: Vec<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct CoinbaseL2UpdateMsg {
    product_id: String,
    changes: Vec<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct CoinbaseMatchMsg {
    product_id: String,
    price: String,
    size: String,
    #[serde(default)]
    time: Option<String>,
    #[serde(default)]
    sequence: Option<u64>,
    #[serde(default)]
    trade_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CoinbaseErrorMsg {
    message: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Default)]
struct CoinbaseBookRuntime {
    bids: BTreeMap<i64, f64>,
    asks: BTreeMap<i64, f64>,
    snapshot_received: bool,
    warmup_started_ms: Option<i64>,
    update_times_ms: VecDeque<i64>,
    trade_times_ms: VecDeque<i64>,
    trade_notional_1s: VecDeque<(i64, f64)>,
    trade_notional_15s: VecDeque<(i64, f64)>,
    last_trade_notional_usd: Option<f64>,
    last_trade_ts_ms: Option<i64>,
    trade_seq: u64,
    last_qualifying_trade_notional_usd: Option<f64>,
    last_qualifying_trade_ts_ms: Option<i64>,
    last_qualifying_trade_seq: Option<u64>,
    recent_trades: VecDeque<CoinbaseTradeSample>,
    period_anchors_15m: VecDeque<CoinbasePeriodAnchorSample>,
    last_exchange_seq: Option<u64>,
    last_trade_price: Option<f64>,
    last_trade_exchange_seq: Option<u64>,
    gap_since_last_15m_anchor: bool,
}

impl CoinbaseBookRuntime {
    fn clear(&mut self) {
        self.bids.clear();
        self.asks.clear();
        self.snapshot_received = false;
        self.warmup_started_ms = None;
        self.update_times_ms.clear();
        self.trade_times_ms.clear();
        self.trade_notional_1s.clear();
        self.trade_notional_15s.clear();
        self.last_trade_notional_usd = None;
        self.last_trade_ts_ms = None;
        self.trade_seq = 0;
        self.last_qualifying_trade_notional_usd = None;
        self.last_qualifying_trade_ts_ms = None;
        self.last_qualifying_trade_seq = None;
        self.recent_trades.clear();
        self.period_anchors_15m.clear();
        self.last_exchange_seq = None;
        self.last_trade_price = None;
        self.last_trade_exchange_seq = None;
        self.gap_since_last_15m_anchor = false;
    }

    fn apply_snapshot(&mut self, snapshot: &CoinbaseSnapshotMsg) {
        self.bids.clear();
        self.asks.clear();
        for level in &snapshot.bids {
            if let Some((price_key, size)) = parse_level(level) {
                if size > 0.0 {
                    self.bids.insert(price_key, size);
                }
            }
        }
        for level in &snapshot.asks {
            if let Some((price_key, size)) = parse_level(level) {
                if size > 0.0 {
                    self.asks.insert(price_key, size);
                }
            }
        }
        self.snapshot_received = true;
    }

    fn apply_l2_update(&mut self, update: &CoinbaseL2UpdateMsg) {
        for change in &update.changes {
            if change.len() < 3 {
                continue;
            }
            let side = change[0].trim().to_ascii_lowercase();
            let Some((price_key, size)) = parse_price_size(change[1].as_str(), change[2].as_str())
            else {
                continue;
            };
            let book = if side == "buy" {
                &mut self.bids
            } else if side == "sell" {
                &mut self.asks
            } else {
                continue;
            };
            if size <= 0.0 {
                book.remove(&price_key);
            } else {
                book.insert(price_key, size);
            }
        }
    }

    fn top_of_book(&self) -> Option<(f64, f64)> {
        let best_bid = self
            .bids
            .iter()
            .next_back()
            .map(|(price_key, _)| decode_price(*price_key))?;
        let best_ask = self
            .asks
            .iter()
            .next()
            .map(|(price_key, _)| decode_price(*price_key))?;
        Some((best_bid, best_ask))
    }

    fn is_crossed(&self) -> bool {
        self.top_of_book()
            .map(|(bid, ask)| bid >= ask)
            .unwrap_or(true)
    }

    fn depth_totals(&self, levels: usize) -> (f64, f64) {
        let bid_depth = self
            .bids
            .iter()
            .rev()
            .take(levels)
            .map(|(_, size)| *size)
            .sum::<f64>();
        let ask_depth = self
            .asks
            .iter()
            .take(levels)
            .map(|(_, size)| *size)
            .sum::<f64>();
        (bid_depth, ask_depth)
    }

    fn top_levels(&self, levels: usize) -> (Vec<CoinbaseBookLevel>, Vec<CoinbaseBookLevel>) {
        let bids = self
            .bids
            .iter()
            .rev()
            .take(levels)
            .map(|(price_key, size)| CoinbaseBookLevel {
                price: decode_price(*price_key),
                size: *size,
            })
            .collect::<Vec<_>>();
        let asks = self
            .asks
            .iter()
            .take(levels)
            .map(|(price_key, size)| CoinbaseBookLevel {
                price: decode_price(*price_key),
                size: *size,
            })
            .collect::<Vec<_>>();
        (bids, asks)
    }

    fn push_update_time(&mut self, now_ms: i64) {
        self.update_times_ms.push_back(now_ms);
        let cutoff_ms = now_ms.saturating_sub(5_000);
        while self
            .update_times_ms
            .front()
            .map(|ts| *ts < cutoff_ms)
            .unwrap_or(false)
        {
            self.update_times_ms.pop_front();
        }
    }

    fn update_rate_5s(&self) -> f64 {
        self.update_times_ms.len() as f64 / 5.0
    }

    fn push_trade_time(&mut self, now_ms: i64) {
        self.trade_times_ms.push_back(now_ms);
        let cutoff_ms = now_ms.saturating_sub(5_000);
        while self
            .trade_times_ms
            .front()
            .map(|ts| *ts < cutoff_ms)
            .unwrap_or(false)
        {
            self.trade_times_ms.pop_front();
        }
    }

    fn trade_rate_5s(&self) -> f64 {
        self.trade_times_ms.len() as f64 / 5.0
    }

    fn push_trade_notional_1s(&mut self, now_ms: i64, notional_usd: f64) {
        if !notional_usd.is_finite() || notional_usd <= 0.0 {
            return;
        }
        let bucket_sec = now_ms.div_euclid(1_000);
        if let Some((last_bucket, last_notional)) = self.trade_notional_1s.back_mut() {
            if *last_bucket == bucket_sec {
                *last_notional += notional_usd;
            } else {
                self.trade_notional_1s.push_back((bucket_sec, notional_usd));
            }
        } else {
            self.trade_notional_1s.push_back((bucket_sec, notional_usd));
        }
        let cutoff_sec = bucket_sec.saturating_sub(1_800);
        while self
            .trade_notional_1s
            .front()
            .map(|(sec, _)| *sec < cutoff_sec)
            .unwrap_or(false)
        {
            self.trade_notional_1s.pop_front();
        }

        self.trade_notional_15s.push_back((now_ms, notional_usd));
        let cutoff_15s = now_ms.saturating_sub(15_000);
        while self
            .trade_notional_15s
            .front()
            .map(|(ts, _)| *ts < cutoff_15s)
            .unwrap_or(false)
        {
            self.trade_notional_15s.pop_front();
        }
        self.last_trade_notional_usd = Some(notional_usd);
        self.last_trade_ts_ms = Some(now_ms);
    }

    fn trade_notional_p95_1s(&self) -> Option<f64> {
        if self.trade_notional_1s.len() < 5 {
            return None;
        }
        let mut values = self
            .trade_notional_1s
            .iter()
            .map(|(_, value)| *value)
            .filter(|v| v.is_finite() && *v > 0.0)
            .collect::<Vec<_>>();
        if values.len() < 5 {
            return None;
        }
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((values.len() - 1) as f64 * 0.95).round() as usize;
        values.get(idx).copied()
    }

    fn max_trade_notional_15s(&self) -> Option<f64> {
        self.trade_notional_15s
            .iter()
            .map(|(_, notional)| *notional)
            .filter(|v| v.is_finite() && *v > 0.0)
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    }

    fn max_trade_notional_15s_with_ts(&self) -> Option<(i64, f64)> {
        self.trade_notional_15s
            .iter()
            .filter(|(_, notional)| notional.is_finite() && *notional > 0.0)
            .copied()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    }

    fn apply_match(&mut self, trade: &CoinbaseMatchMsg, now_ms: i64) {
        let price = trade.price.trim().parse::<f64>().ok();
        let size = trade.size.trim().parse::<f64>().ok();
        let Some(price) = price.filter(|v| v.is_finite() && *v > 0.0) else {
            return;
        };
        let Some(size) = size.filter(|v| v.is_finite() && *v > 0.0) else {
            return;
        };
        let exchange_ts_ms = trade
            .time
            .as_deref()
            .and_then(parse_iso8601_to_ms)
            .unwrap_or(now_ms);
        let exchange_seq = trade.sequence.or(trade.trade_id);
        let seq_gap_after_prev =
            if let (Some(prev), Some(curr)) = (self.last_exchange_seq, exchange_seq) {
                curr <= prev
            } else {
                false
            };
        if seq_gap_after_prev {
            self.gap_since_last_15m_anchor = true;
        }
        if let Some(curr) = exchange_seq {
            self.last_exchange_seq = Some(curr);
        }
        if let (Some(prev_trade_ts_ms), Some(prev_trade_price)) =
            (self.last_trade_ts_ms, self.last_trade_price)
        {
            let prev_period_open_ts = prev_trade_ts_ms
                .div_euclid(900_000)
                .saturating_mul(900_000)
                .div_euclid(1_000);
            let curr_period_open_ts = exchange_ts_ms
                .div_euclid(900_000)
                .saturating_mul(900_000)
                .div_euclid(1_000);
            if curr_period_open_ts > prev_period_open_ts && prev_trade_ts_ms > 0 {
                self.period_anchors_15m
                    .push_back(CoinbasePeriodAnchorSample {
                        period_open_ts: curr_period_open_ts,
                        anchor_price: prev_trade_price,
                        source_trade_ts_ms: prev_trade_ts_ms,
                        source_exchange_seq: self.last_trade_exchange_seq,
                        seq_gap_detected: self.gap_since_last_15m_anchor || seq_gap_after_prev,
                    });
                while self.period_anchors_15m.len() > 64 {
                    self.period_anchors_15m.pop_front();
                }
                self.gap_since_last_15m_anchor = false;
            }
        }
        let notional_usd = price * size;
        self.trade_seq = self.trade_seq.saturating_add(1);
        self.push_trade_notional_1s(exchange_ts_ms, notional_usd);
        self.push_trade_time(exchange_ts_ms);
        self.recent_trades.push_back(CoinbaseTradeSample {
            seq: self.trade_seq,
            ts_ms: exchange_ts_ms,
            price,
            notional_usd,
            exchange_seq,
            trade_id: trade.trade_id,
            seq_gap_after_prev,
        });
        let cutoff_ms = exchange_ts_ms.saturating_sub(60_000);
        while self
            .recent_trades
            .front()
            .map(|trade| trade.ts_ms < cutoff_ms)
            .unwrap_or(false)
        {
            self.recent_trades.pop_front();
        }
        while self.recent_trades.len() > 512 {
            self.recent_trades.pop_front();
        }
        if notional_usd >= 10_000.0 {
            self.last_qualifying_trade_notional_usd = Some(notional_usd);
            self.last_qualifying_trade_ts_ms = Some(exchange_ts_ms);
            self.last_qualifying_trade_seq = Some(self.trade_seq);
        }
        self.last_trade_notional_usd = Some(notional_usd);
        self.last_trade_ts_ms = Some(exchange_ts_ms);
        self.last_trade_price = Some(price);
        self.last_trade_exchange_seq = exchange_seq;
    }
}

fn parse_iso8601_to_ms(value: &str) -> Option<i64> {
    let raw = value.trim();
    if raw.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn parse_level(level: &[String]) -> Option<(i64, f64)> {
    if level.len() < 2 {
        return None;
    }
    parse_price_size(level[0].as_str(), level[1].as_str())
}

fn parse_price_size(price_raw: &str, size_raw: &str) -> Option<(i64, f64)> {
    let price = price_raw.trim().parse::<f64>().ok()?;
    let size = size_raw.trim().parse::<f64>().ok()?;
    if !price.is_finite() || !size.is_finite() || price <= 0.0 {
        return None;
    }
    Some((encode_price(price), size.max(0.0)))
}

fn encode_price(price: f64) -> i64 {
    (price * PRICE_SCALE).round() as i64
}

fn decode_price(price_key: i64) -> f64 {
    (price_key as f64) / PRICE_SCALE
}

fn next_state_after_book_update(
    current: CoinbaseFeedState,
    snapshot_received: bool,
    warmup_started_ms: Option<i64>,
    now_ms: i64,
    warmup_sec: u64,
    crossed_book: bool,
) -> (CoinbaseFeedState, Option<i64>) {
    if !snapshot_received || crossed_book {
        return (CoinbaseFeedState::Invalid, None);
    }
    if current == CoinbaseFeedState::Healthy {
        return (CoinbaseFeedState::Healthy, warmup_started_ms);
    }

    let started_at = warmup_started_ms.unwrap_or(now_ms);
    let warmup_ms = i64::try_from(warmup_sec.saturating_mul(1000)).unwrap_or(i64::MAX);
    if now_ms.saturating_sub(started_at) >= warmup_ms {
        (CoinbaseFeedState::Healthy, Some(started_at))
    } else {
        (CoinbaseFeedState::Warmup, Some(started_at))
    }
}

async fn transition_state(
    shared_state: &SharedCoinbaseBookState,
    next_state: CoinbaseFeedState,
    reason: &str,
    now_ms: i64,
) {
    let mut guard = shared_state.write().await;
    if guard.feed_state == next_state
        && guard
            .state_reason
            .as_deref()
            .map(|v| v == reason)
            .unwrap_or(false)
    {
        return;
    }
    let prev_state = guard.feed_state;
    guard.feed_state = next_state;
    guard.state_reason = Some(reason.to_string());
    guard.last_state_transition_ms = now_ms;
    log_event(
        "coinbase_state_transition",
        json!({
            "product_id": guard.product_id,
            "from": format!("{:?}", prev_state).to_uppercase(),
            "to": format!("{:?}", next_state).to_uppercase(),
            "reason": reason,
            "ts_ms": now_ms
        }),
    );
}

async fn publish_metrics(
    cfg: &CoinbaseWsConfig,
    runtime: &CoinbaseBookRuntime,
    shared_state: &SharedCoinbaseBookState,
    now_ms: i64,
) {
    let mut guard = shared_state.write().await;
    guard.product_id = cfg.product_id.clone();
    guard.last_update_ms = now_ms;
    if let Some((best_bid, best_ask)) = runtime.top_of_book() {
        guard.best_bid = Some(best_bid);
        guard.best_ask = Some(best_ask);
        guard.mid_price = Some((best_bid + best_ask) / 2.0);
        guard.spread = Some((best_ask - best_bid).max(0.0));
    } else {
        guard.best_bid = None;
        guard.best_ask = None;
        guard.mid_price = None;
        guard.spread = None;
    }
    let (bid_depth, ask_depth) = runtime.depth_totals(cfg.depth_levels);
    let (bid_levels, ask_levels) = runtime.top_levels(cfg.depth_levels);
    let total_depth = bid_depth + ask_depth;
    guard.top_bid_depth = Some(bid_depth);
    guard.top_ask_depth = Some(ask_depth);
    guard.bid_levels = bid_levels;
    guard.ask_levels = ask_levels;
    guard.imbalance = if total_depth > 0.0 {
        Some(((bid_depth - ask_depth) / total_depth).clamp(-1.0, 1.0))
    } else {
        None
    };
    guard.book_update_rate_5s = Some(runtime.update_rate_5s());
    guard.trade_notional_p95_1s = runtime.trade_notional_p95_1s();
    guard.last_trade_notional_usd = runtime.last_trade_notional_usd;
    guard.last_trade_ts_ms = runtime.last_trade_ts_ms;
    guard.last_qualifying_trade_notional_usd = runtime.last_qualifying_trade_notional_usd;
    guard.last_qualifying_trade_ts_ms = runtime.last_qualifying_trade_ts_ms;
    guard.last_qualifying_trade_seq = runtime.last_qualifying_trade_seq;
    let (max_trade_notional_15s_ts_ms, max_trade_notional_15s) = runtime
        .max_trade_notional_15s_with_ts()
        .map(|(ts, notional)| (Some(ts), Some(notional)))
        .unwrap_or((None, None));
    guard.max_trade_notional_15s =
        max_trade_notional_15s.or_else(|| runtime.max_trade_notional_15s());
    guard.max_trade_notional_15s_ts_ms = max_trade_notional_15s_ts_ms;
    guard.trade_update_rate_5s = Some(runtime.trade_rate_5s());
    guard.recent_trades = runtime.recent_trades.iter().cloned().collect();
    guard.period_anchors_15m = runtime.period_anchors_15m.iter().cloned().collect();
}

fn configured_channels(raw: &str) -> Vec<String> {
    let mut channels = raw
        .split(',')
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .collect::<Vec<_>>();
    if channels.is_empty() {
        channels.push("level2_batch".to_string());
    }
    if !channels.iter().any(|v| v.eq_ignore_ascii_case("matches")) {
        channels.push("matches".to_string());
    }
    channels
}

pub fn spawn_coinbase_level2_feed(
    cfg: CoinbaseWsConfig,
    shared_state: SharedCoinbaseBookState,
    trade_notify: Option<SharedCoinbaseTradeNotify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = cfg.reconnect_min_sec.max(1);
        let mut runtime = CoinbaseBookRuntime::default();

        loop {
            match connect_async(&cfg.stream_url).await {
                Ok((mut ws_stream, _)) => {
                    log_event(
                        "coinbase_connected",
                        json!({
                            "stream_url": cfg.stream_url,
                            "product_id": cfg.product_id,
                            "channel": cfg.channel,
                            "backoff_sec": backoff
                        }),
                    );
                    runtime.clear();
                    backoff = cfg.reconnect_min_sec.max(1);
                    transition_state(
                        &shared_state,
                        CoinbaseFeedState::Resyncing,
                        "connected",
                        chrono::Utc::now().timestamp_millis(),
                    )
                    .await;

                    let subscribe_channels = configured_channels(cfg.channel.as_str());
                    let subscribe = json!({
                        "type": "subscribe",
                        "product_ids": [cfg.product_id],
                        "channels": subscribe_channels
                    });
                    if let Err(e) = ws_stream.send(Message::Text(subscribe.to_string())).await {
                        log_event(
                            "coinbase_subscribe_failed",
                            json!({
                                "error": e.to_string(),
                                "channel": cfg.channel,
                                "product_id": cfg.product_id
                            }),
                        );
                        transition_state(
                            &shared_state,
                            CoinbaseFeedState::Invalid,
                            "subscribe_failed",
                            chrono::Utc::now().timestamp_millis(),
                        )
                        .await;
                        continue;
                    }

                    loop {
                        let next_message = tokio::time::timeout(
                            Duration::from_secs(cfg.inactivity_timeout_sec.max(5)),
                            ws_stream.next(),
                        )
                        .await;
                        let Some(msg) = (match next_message {
                            Ok(v) => v,
                            Err(_) => {
                                transition_state(
                                    &shared_state,
                                    CoinbaseFeedState::Invalid,
                                    "inactivity_timeout",
                                    chrono::Utc::now().timestamp_millis(),
                                )
                                .await;
                                break;
                            }
                        }) else {
                            transition_state(
                                &shared_state,
                                CoinbaseFeedState::Invalid,
                                "socket_ended",
                                chrono::Utc::now().timestamp_millis(),
                            )
                            .await;
                            break;
                        };

                        match msg {
                            Ok(Message::Text(text)) => {
                                let now_ms = chrono::Utc::now().timestamp_millis();
                                let trade_message = match process_coinbase_text(
                                    &cfg,
                                    &mut runtime,
                                    text.as_str(),
                                    now_ms,
                                ) {
                                    Ok(trade_message) => trade_message,
                                    Err(reason) => {
                                        log_event(
                                            "coinbase_parse_or_contract_error",
                                            json!({
                                                "reason": reason,
                                                "product_id": cfg.product_id
                                            }),
                                        );
                                        transition_state(
                                            &shared_state,
                                            CoinbaseFeedState::Invalid,
                                            reason.as_str(),
                                            chrono::Utc::now().timestamp_millis(),
                                        )
                                        .await;
                                        break;
                                    }
                                };

                                runtime.push_update_time(now_ms);
                                publish_metrics(&cfg, &runtime, &shared_state, now_ms).await;
                                if trade_message {
                                    if let Some(notify) = trade_notify.as_ref() {
                                        notify.notify_waiters();
                                    }
                                }

                                let current = { shared_state.read().await.feed_state };
                                let (next_state, warmup_started_ms) = next_state_after_book_update(
                                    current,
                                    runtime.snapshot_received,
                                    runtime.warmup_started_ms,
                                    now_ms,
                                    cfg.warmup_sec,
                                    runtime.is_crossed(),
                                );
                                runtime.warmup_started_ms = warmup_started_ms;
                                if next_state != current {
                                    let reason = if next_state == CoinbaseFeedState::Warmup {
                                        "warmup"
                                    } else if next_state == CoinbaseFeedState::Healthy {
                                        "healthy_after_warmup"
                                    } else {
                                        "invalid_book"
                                    };
                                    transition_state(&shared_state, next_state, reason, now_ms)
                                        .await;
                                }
                            }
                            Ok(Message::Ping(_))
                            | Ok(Message::Pong(_))
                            | Ok(Message::Binary(_)) => {}
                            Ok(Message::Close(_)) => {
                                transition_state(
                                    &shared_state,
                                    CoinbaseFeedState::Invalid,
                                    "socket_closed",
                                    chrono::Utc::now().timestamp_millis(),
                                )
                                .await;
                                break;
                            }
                            Ok(_) => {}
                            Err(e) => {
                                log_event(
                                    "coinbase_read_error",
                                    json!({
                                        "error": e.to_string(),
                                        "product_id": cfg.product_id
                                    }),
                                );
                                transition_state(
                                    &shared_state,
                                    CoinbaseFeedState::Invalid,
                                    "read_error",
                                    chrono::Utc::now().timestamp_millis(),
                                )
                                .await;
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    log_event(
                        "coinbase_connect_failed",
                        json!({
                            "error": e.to_string(),
                            "retry_in_sec": backoff,
                            "product_id": cfg.product_id
                        }),
                    );
                    transition_state(
                        &shared_state,
                        CoinbaseFeedState::Invalid,
                        "connect_failed",
                        chrono::Utc::now().timestamp_millis(),
                    )
                    .await;
                }
            }

            sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(cfg.reconnect_max_sec.max(backoff));
        }
    })
}

fn process_coinbase_text(
    cfg: &CoinbaseWsConfig,
    runtime: &mut CoinbaseBookRuntime,
    text: &str,
    now_ms: i64,
) -> Result<bool, String> {
    let env = serde_json::from_str::<CoinbaseEnvelope>(text)
        .map_err(|_| "coinbase_json_parse_failed".to_string())?;
    match env.msg_type.as_str() {
        "snapshot" => {
            let msg = serde_json::from_str::<CoinbaseSnapshotMsg>(text)
                .map_err(|_| "coinbase_snapshot_parse_failed".to_string())?;
            if !msg.product_id.eq_ignore_ascii_case(cfg.product_id.as_str()) {
                return Ok(false);
            }
            runtime.apply_snapshot(&msg);
            if runtime.is_crossed() {
                return Err("coinbase_crossed_after_snapshot".to_string());
            }
            Ok(false)
        }
        "l2update" => {
            let msg = serde_json::from_str::<CoinbaseL2UpdateMsg>(text)
                .map_err(|_| "coinbase_l2update_parse_failed".to_string())?;
            if !msg.product_id.eq_ignore_ascii_case(cfg.product_id.as_str()) {
                return Ok(false);
            }
            if !runtime.snapshot_received {
                return Err("coinbase_update_before_snapshot".to_string());
            }
            runtime.apply_l2_update(&msg);
            if runtime.is_crossed() {
                return Err("coinbase_crossed_book".to_string());
            }
            Ok(false)
        }
        "match" | "last_match" => {
            let msg = serde_json::from_str::<CoinbaseMatchMsg>(text)
                .map_err(|_| "coinbase_match_parse_failed".to_string())?;
            if !msg.product_id.eq_ignore_ascii_case(cfg.product_id.as_str()) {
                return Ok(false);
            }
            runtime.apply_match(&msg, now_ms);
            Ok(true)
        }
        "subscriptions" | "heartbeat" => Ok(false),
        "error" => {
            let msg = serde_json::from_str::<CoinbaseErrorMsg>(text).ok();
            let reason = msg
                .as_ref()
                .and_then(|m| m.reason.clone())
                .unwrap_or_else(|| "unknown_reason".to_string());
            let message = msg
                .as_ref()
                .and_then(|m| m.message.clone())
                .unwrap_or_else(|| "unknown_message".to_string());
            Err(format!(
                "coinbase_error_message:{}:{}",
                reason.replace(':', "_"),
                message.replace(':', "_")
            ))
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coinbase_state_machine_promotes_to_healthy_after_warmup() {
        let (state, started) =
            next_state_after_book_update(CoinbaseFeedState::Resyncing, true, None, 1_000, 5, false);
        assert_eq!(state, CoinbaseFeedState::Warmup);
        assert_eq!(started, Some(1_000));

        let (state, started) =
            next_state_after_book_update(CoinbaseFeedState::Warmup, true, started, 7_000, 5, false);
        assert_eq!(state, CoinbaseFeedState::Healthy);
        assert_eq!(started, Some(1_000));
    }

    #[test]
    fn coinbase_contract_rejects_update_before_snapshot() {
        let cfg = CoinbaseWsConfig::default();
        let mut runtime = CoinbaseBookRuntime::default();
        let update = json!({
            "type": "l2update",
            "product_id": cfg.product_id,
            "changes": [["buy", "100000.00", "0.5"]]
        });
        let err = process_coinbase_text(&cfg, &mut runtime, update.to_string().as_str(), 1_000)
            .expect_err("update before snapshot should fail");
        assert_eq!(err, "coinbase_update_before_snapshot");
    }

    #[test]
    fn coinbase_default_channel_prefers_public_batch() {
        let cfg = CoinbaseWsConfig::default();
        assert_eq!(cfg.channel, "level2_batch");
        let channels = configured_channels(cfg.channel.as_str());
        assert!(channels
            .iter()
            .any(|v| v.eq_ignore_ascii_case("level2_batch")));
        assert!(channels.iter().any(|v| v.eq_ignore_ascii_case("matches")));
    }

    #[test]
    fn coinbase_error_message_surfaces_reason_text() {
        let cfg = CoinbaseWsConfig::default();
        let mut runtime = CoinbaseBookRuntime::default();
        let err_msg = json!({
            "type": "error",
            "message": "Failed to subscribe",
            "reason": "level2, level3, and full channels now require authentication."
        });
        let err = process_coinbase_text(&cfg, &mut runtime, err_msg.to_string().as_str(), 1_000)
            .expect_err("error messages should bubble reason");
        assert!(err.contains("coinbase_error_message"));
        assert!(err.contains("require authentication"));
    }

    #[test]
    fn coinbase_match_updates_trade_notional_p95() {
        let cfg = CoinbaseWsConfig::default();
        let mut runtime = CoinbaseBookRuntime::default();
        for i in 0_i64..12_i64 {
            let msg = json!({
                "type": "match",
                "product_id": cfg.product_id,
                "price": "68000.00",
                "size": format!("{:.4}", 0.01 + (i as f64) * 0.001)
            });
            process_coinbase_text(
                &cfg,
                &mut runtime,
                msg.to_string().as_str(),
                1_000 + i * 1_000,
            )
            .expect("match parse should succeed");
        }
        let p95 = runtime.trade_notional_p95_1s().unwrap_or(0.0);
        assert!(p95 > 0.0);
        assert!(runtime.trade_rate_5s() >= 0.0);
    }
}
