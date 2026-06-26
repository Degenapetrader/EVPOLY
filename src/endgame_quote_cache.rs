use crate::endgame_sweep::BookAskLevel;
use crate::models::OrderBook;
use rust_decimal::prelude::ToPrimitive;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndgameQuoteSource {
    WsBook,
    RestBatchBook,
    RestExactBook,
}

impl EndgameQuoteSource {
    pub fn as_str(self) -> &'static str {
        match self {
            EndgameQuoteSource::WsBook => "polymarket_ws_orderbook",
            EndgameQuoteSource::RestBatchBook => "polymarket_rest_orderbook_batch",
            EndgameQuoteSource::RestExactBook => "polymarket_rest_orderbook",
        }
    }

    fn priority(self) -> u8 {
        match self {
            EndgameQuoteSource::WsBook => 1,
            EndgameQuoteSource::RestBatchBook => 2,
            EndgameQuoteSource::RestExactBook => 3,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EndgameQuoteSnapshot {
    pub token_id: String,
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
    pub ask_levels: Vec<BookAskLevel>,
    pub asks_within_limit: usize,
    pub ask_level_count: usize,
    pub poly_mid: Option<f64>,
    pub updated_ms: i64,
    pub ws_updated_ms: Option<i64>,
    pub rest_updated_ms: Option<i64>,
    pub source: EndgameQuoteSource,
    pub seq: u64,
    pub valid: bool,
    pub invalid_reason: Option<String>,
}

impl EndgameQuoteSnapshot {
    pub fn age_ms(&self, now_ms: i64) -> i64 {
        now_ms.saturating_sub(self.updated_ms).max(0)
    }
}

#[derive(Debug, Clone)]
pub struct EndgameQuoteFields {
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
    pub ask_levels: Vec<BookAskLevel>,
    pub asks_within_limit: usize,
    pub ask_level_count: usize,
    pub poly_mid: Option<f64>,
}

#[derive(Debug)]
struct EndgameQuoteCacheInner {
    seq: u64,
    quotes: HashMap<String, EndgameQuoteSnapshot>,
}

#[derive(Debug, Clone)]
pub struct EndgameQuoteCache {
    inner: Arc<StdMutex<EndgameQuoteCacheInner>>,
}

impl EndgameQuoteCache {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(StdMutex::new(EndgameQuoteCacheInner {
                seq: 0,
                quotes: HashMap::new(),
            })),
        }
    }

    pub fn latest(&self, token_id: &str) -> Option<EndgameQuoteSnapshot> {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.quotes.get(token_id).cloned())
    }

    pub fn upsert_from_orderbook(
        &self,
        token_id: &str,
        orderbook: &OrderBook,
        updated_ms: i64,
        source: EndgameQuoteSource,
    ) -> Option<EndgameQuoteSnapshot> {
        match endgame_quote_fields_from_book(orderbook) {
            Some(fields) => self.upsert_fields(token_id, fields, updated_ms, source),
            None => self.invalidate(token_id, updated_ms, source, "invalid_or_empty_book"),
        }
    }

    pub fn upsert_fields(
        &self,
        token_id: &str,
        fields: EndgameQuoteFields,
        updated_ms: i64,
        source: EndgameQuoteSource,
    ) -> Option<EndgameQuoteSnapshot> {
        if token_id.trim().is_empty() || updated_ms <= 0 {
            return None;
        }
        let token_id = token_id.trim().to_string();
        let mut inner = self.inner.lock().ok()?;
        let (previous_ws_updated_ms, previous_rest_updated_ms) = inner
            .quotes
            .get(token_id.as_str())
            .map(|old| (old.ws_updated_ms, old.rest_updated_ms))
            .unwrap_or((None, None));
        if let Some(old) = inner.quotes.get(token_id.as_str()) {
            if updated_ms < old.updated_ms
                || (updated_ms == old.updated_ms && source.priority() < old.source.priority())
            {
                return None;
            }
        }
        inner.seq = inner.seq.saturating_add(1);
        let snapshot = EndgameQuoteSnapshot {
            token_id: token_id.clone(),
            best_bid: fields.best_bid,
            best_ask: fields.best_ask,
            ask_levels: fields.ask_levels,
            asks_within_limit: fields.asks_within_limit,
            ask_level_count: fields.ask_level_count,
            poly_mid: fields.poly_mid,
            updated_ms,
            ws_updated_ms: quote_ws_updated_ms(source, updated_ms, previous_ws_updated_ms),
            rest_updated_ms: quote_rest_updated_ms(source, updated_ms, previous_rest_updated_ms),
            source,
            seq: inner.seq,
            valid: true,
            invalid_reason: None,
        };
        inner.quotes.insert(token_id, snapshot.clone());
        Some(snapshot)
    }

    pub fn invalidate(
        &self,
        token_id: &str,
        updated_ms: i64,
        source: EndgameQuoteSource,
        reason: &str,
    ) -> Option<EndgameQuoteSnapshot> {
        if token_id.trim().is_empty() || updated_ms <= 0 {
            return None;
        }
        let token_id = token_id.trim().to_string();
        let mut inner = self.inner.lock().ok()?;
        let (previous_ws_updated_ms, previous_rest_updated_ms) = inner
            .quotes
            .get(token_id.as_str())
            .map(|old| (old.ws_updated_ms, old.rest_updated_ms))
            .unwrap_or((None, None));
        if let Some(old) = inner.quotes.get(token_id.as_str()) {
            if updated_ms < old.updated_ms
                || (updated_ms == old.updated_ms && source.priority() < old.source.priority())
            {
                return None;
            }
        }
        inner.seq = inner.seq.saturating_add(1);
        let snapshot = EndgameQuoteSnapshot {
            token_id: token_id.clone(),
            best_bid: None,
            best_ask: None,
            ask_levels: Vec::new(),
            asks_within_limit: 0,
            ask_level_count: 0,
            poly_mid: None,
            updated_ms,
            ws_updated_ms: quote_ws_updated_ms(source, updated_ms, previous_ws_updated_ms),
            rest_updated_ms: quote_rest_updated_ms(source, updated_ms, previous_rest_updated_ms),
            source,
            seq: inner.seq,
            valid: false,
            invalid_reason: Some(reason.to_string()),
        };
        inner.quotes.insert(token_id, snapshot.clone());
        Some(snapshot)
    }
}

fn quote_ws_updated_ms(
    source: EndgameQuoteSource,
    updated_ms: i64,
    previous: Option<i64>,
) -> Option<i64> {
    match source {
        EndgameQuoteSource::WsBook => Some(updated_ms),
        EndgameQuoteSource::RestBatchBook | EndgameQuoteSource::RestExactBook => previous,
    }
}

fn quote_rest_updated_ms(
    source: EndgameQuoteSource,
    updated_ms: i64,
    previous: Option<i64>,
) -> Option<i64> {
    match source {
        EndgameQuoteSource::WsBook => previous,
        EndgameQuoteSource::RestBatchBook | EndgameQuoteSource::RestExactBook => Some(updated_ms),
    }
}

impl Default for EndgameQuoteCache {
    fn default() -> Self {
        Self::new()
    }
}

pub fn endgame_quote_fields_from_book(orderbook: &OrderBook) -> Option<EndgameQuoteFields> {
    let limit_price = std::env::var("EVPOLY_ENDGAME_FIXED_LIMIT_PRICE")
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .unwrap_or(0.99)
        .clamp(0.01, 0.99);
    let max_ask_levels = std::env::var("EVPOLY_ENDGAME_QUOTE_MAX_ASK_LEVELS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(20)
        .clamp(1, 100);

    let mut best_bid: Option<f64> = None;
    for level in &orderbook.bids {
        let Some(price) = level.price.to_f64() else {
            continue;
        };
        let Some(size) = level.size.to_f64() else {
            continue;
        };
        if !price.is_finite() || !size.is_finite() || price <= 0.0 || size <= 0.0 {
            continue;
        }
        best_bid = Some(best_bid.map(|old| old.max(price)).unwrap_or(price));
    }

    let mut ask_levels = Vec::new();
    let mut ask_level_count = 0usize;
    let mut asks_within_limit = 0usize;
    for level in &orderbook.asks {
        let Some(price) = level.price.to_f64() else {
            continue;
        };
        let Some(size) = level.size.to_f64() else {
            continue;
        };
        if !price.is_finite() || !size.is_finite() || price <= 0.0 || size <= 0.0 {
            continue;
        }
        ask_level_count = ask_level_count.saturating_add(1);
        if price <= limit_price {
            asks_within_limit = asks_within_limit.saturating_add(1);
        }
        ask_levels.push(BookAskLevel { price, size });
    }
    ask_levels.sort_by(|left, right| {
        left.price
            .partial_cmp(&right.price)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut retained_asks = Vec::with_capacity(ask_levels.len().min(max_ask_levels));
    let mut retained_above_limit = 0usize;
    for level in ask_levels {
        if level.price <= limit_price || retained_above_limit < max_ask_levels {
            if level.price > limit_price {
                retained_above_limit = retained_above_limit.saturating_add(1);
            }
            retained_asks.push(level);
        }
    }
    let best_ask = retained_asks.first().map(|level| level.price);
    let poly_mid = match (best_bid, best_ask) {
        (Some(bid), Some(ask)) if ask > bid => Some((bid + ask) / 2.0),
        _ => best_ask.or(best_bid),
    };

    if best_bid.is_none() && best_ask.is_none() {
        return None;
    }

    Some(EndgameQuoteFields {
        best_bid,
        best_ask,
        ask_levels: retained_asks,
        asks_within_limit,
        ask_level_count,
        poly_mid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::OrderBookEntry;
    use rust_decimal::Decimal;

    fn level(price: &str, size: &str) -> OrderBookEntry {
        OrderBookEntry {
            price: price.parse::<Decimal>().unwrap(),
            size: size.parse::<Decimal>().unwrap(),
        }
    }

    #[test]
    fn endgame_quote_cache_extracts_best_prices_and_ask_levels() {
        let book = OrderBook {
            bids: vec![level("0.92", "10"), level("0.94", "5")],
            asks: vec![level("0.99", "3"), level("0.97", "2"), level("1.00", "1")],
        };

        let fields = endgame_quote_fields_from_book(&book).expect("valid book");

        assert_eq!(fields.best_bid, Some(0.94));
        assert_eq!(fields.best_ask, Some(0.97));
        assert_eq!(fields.ask_levels[0].price, 0.97);
        assert_eq!(fields.asks_within_limit, 2);
        assert_eq!(fields.ask_level_count, 3);
        assert_eq!(fields.poly_mid, Some((0.94 + 0.97) / 2.0));
    }
}
