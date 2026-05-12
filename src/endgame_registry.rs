use crate::api::MarketConstraintSnapshot;
use crate::models::Market;
use crate::strategy::Timeframe;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock as StdRwLock};

#[derive(Debug, Clone)]
pub struct EndgameConstraintCtx {
    pub snapshot: MarketConstraintSnapshot,
    pub fetched_ms: i64,
}

#[derive(Debug, Clone)]
pub struct EndgameMarketCtx {
    pub symbol: String,
    pub timeframe: Timeframe,
    pub market_open_ts: i64,
    pub market_close_ts: i64,
    pub matched_open_ts: u64,
    pub market: Market,
    pub up_token_id: String,
    pub down_token_id: String,
    pub constraints: Option<EndgameConstraintCtx>,
    pub updated_ms: i64,
}

type RegistryKey = (String, Timeframe, i64);

#[derive(Debug, Default)]
struct EndgameRegistryInner {
    markets: HashMap<RegistryKey, EndgameMarketCtx>,
}

#[derive(Debug, Clone, Default)]
pub struct EndgameRegistry {
    inner: Arc<StdRwLock<EndgameRegistryInner>>,
}

impl EndgameRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        market_open_ts: i64,
    ) -> Option<EndgameMarketCtx> {
        let symbol_key = normalize_symbol_key(symbol);
        self.inner.read().ok().and_then(|inner| {
            inner
                .markets
                .get(&(symbol_key, timeframe, market_open_ts))
                .cloned()
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_market(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        market_open_ts: i64,
        market_close_ts: i64,
        matched_open_ts: u64,
        market: Market,
        up_token_id: String,
        down_token_id: String,
        updated_ms: i64,
    ) -> Option<EndgameMarketCtx> {
        if market_open_ts <= 0 || market_close_ts <= market_open_ts {
            return None;
        }
        let symbol_key = normalize_symbol_key(symbol);
        if symbol_key.is_empty()
            || market.condition_id.trim().is_empty()
            || up_token_id.trim().is_empty()
            || down_token_id.trim().is_empty()
        {
            return None;
        }

        let key = (symbol_key.clone(), timeframe, market_open_ts);
        let mut inner = self.inner.write().ok()?;
        let constraints = inner.markets.get(&key).and_then(|ctx| {
            (ctx.market.condition_id == market.condition_id)
                .then(|| ctx.constraints.clone())
                .flatten()
        });
        let ctx = EndgameMarketCtx {
            symbol: symbol_key,
            timeframe,
            market_open_ts,
            market_close_ts,
            matched_open_ts,
            market,
            up_token_id: up_token_id.trim().to_string(),
            down_token_id: down_token_id.trim().to_string(),
            constraints,
            updated_ms,
        };
        inner.markets.insert(key, ctx.clone());
        Some(ctx)
    }

    pub fn upsert_constraints(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        market_open_ts: i64,
        snapshot: MarketConstraintSnapshot,
        fetched_ms: i64,
    ) -> Option<EndgameMarketCtx> {
        let symbol_key = normalize_symbol_key(symbol);
        let key = (symbol_key, timeframe, market_open_ts);
        let mut inner = self.inner.write().ok()?;
        let ctx = inner.markets.get_mut(&key)?;
        ctx.constraints = Some(EndgameConstraintCtx {
            snapshot,
            fetched_ms,
        });
        Some(ctx.clone())
    }

    pub fn active_token_ids(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        if let Ok(inner) = self.inner.read() {
            for ctx in inner.markets.values() {
                for token_id in [&ctx.up_token_id, &ctx.down_token_id] {
                    if !token_id.trim().is_empty() && seen.insert(token_id.clone()) {
                        out.push(token_id.clone());
                    }
                }
            }
        }
        out
    }

    pub fn active_scope(&self) -> (Vec<String>, Vec<String>) {
        let mut token_seen = HashSet::new();
        let mut condition_seen = HashSet::new();
        let mut token_ids = Vec::new();
        let mut condition_ids = Vec::new();
        if let Ok(inner) = self.inner.read() {
            for ctx in inner.markets.values() {
                if !ctx.market.condition_id.trim().is_empty()
                    && condition_seen.insert(ctx.market.condition_id.clone())
                {
                    condition_ids.push(ctx.market.condition_id.clone());
                }
                for token_id in [&ctx.up_token_id, &ctx.down_token_id] {
                    if !token_id.trim().is_empty() && token_seen.insert(token_id.clone()) {
                        token_ids.push(token_id.clone());
                    }
                }
            }
        }
        (token_ids, condition_ids)
    }

    pub fn prune_old(&self, now_ts: i64) {
        if let Ok(mut inner) = self.inner.write() {
            inner.markets.retain(|_, ctx| {
                ctx.market_open_ts
                    .saturating_add(ctx.timeframe.duration_seconds() * 2)
                    >= now_ts
            });
        }
    }
}

fn normalize_symbol_key(symbol: &str) -> String {
    symbol.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    fn market(condition_id: &str) -> Market {
        Market {
            condition_id: condition_id.to_string(),
            market_id: None,
            question: "test".to_string(),
            slug: "btc-updown-5m-test".to_string(),
            description: None,
            resolution_source: None,
            end_date: None,
            end_date_iso: None,
            end_date_iso_alt: None,
            game_start_time: None,
            start_date: None,
            sports_market_type: None,
            active: true,
            closed: false,
            tokens: None,
            clob_token_ids: None,
            outcomes: None,
            competitive: None,
            events: None,
        }
    }

    fn constraints() -> MarketConstraintSnapshot {
        MarketConstraintSnapshot {
            accepting_orders: true,
            active: true,
            closed: false,
            minimum_order_size: Decimal::new(5, 0),
            minimum_tick_size: Decimal::new(1, 2),
            min_size_shares: Decimal::ZERO,
        }
    }

    #[test]
    fn endgame_registry_returns_market_and_scope() {
        let registry = EndgameRegistry::new();
        let ctx = registry
            .upsert_market(
                "BTC",
                Timeframe::M5,
                1_000,
                1_300,
                1_000,
                market("cond"),
                "up".to_string(),
                "down".to_string(),
                10,
            )
            .expect("market inserted");

        assert_eq!(ctx.symbol, "btc");
        assert!(registry.get("btc", Timeframe::M5, 1_000).is_some());
        let (tokens, conditions) = registry.active_scope();
        assert_eq!(tokens.len(), 2);
        assert!(tokens.contains(&"up".to_string()));
        assert!(tokens.contains(&"down".to_string()));
        assert_eq!(conditions, vec!["cond".to_string()]);
    }

    #[test]
    fn endgame_registry_preserves_constraints_for_same_condition() {
        let registry = EndgameRegistry::new();
        registry.upsert_market(
            "BTC",
            Timeframe::M5,
            1_000,
            1_300,
            1_000,
            market("cond"),
            "up".to_string(),
            "down".to_string(),
            10,
        );
        registry.upsert_constraints("BTC", Timeframe::M5, 1_000, constraints(), 20);
        let updated = registry
            .upsert_market(
                "BTC",
                Timeframe::M5,
                1_000,
                1_300,
                1_000,
                market("cond"),
                "up2".to_string(),
                "down2".to_string(),
                30,
            )
            .expect("market updated");

        assert_eq!(updated.constraints.expect("constraints").fetched_ms, 20);
        assert_eq!(registry.active_token_ids().len(), 2);
    }
}
