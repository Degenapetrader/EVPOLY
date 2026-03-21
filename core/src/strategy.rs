use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::signal_state::SignalState;

pub const STRATEGY_ID_PREMARKET_V1: &str = "premarket_v1";
pub const STRATEGY_ID_ENDGAME_SWEEP_V1: &str = "endgame_sweep_v1";
pub const STRATEGY_ID_EVCURVE_V1: &str = "evcurve_v1";
pub const STRATEGY_ID_SESSIONBAND_V1: &str = "sessionband_v1";
pub const STRATEGY_ID_EVSNIPE_V1: &str = "evsnipe_v1";
pub const STRATEGY_ID_MM_REWARDS_V1: &str = "mm_rewards_v1";
pub const STRATEGY_ID_MM_SPORT_V1: &str = "mm_sport_v1";
pub const STRATEGY_ID_LEGACY_DEFAULT: &str = "legacy_default";
pub const DEFAULT_MARKET_KEY: &str = "btc";
pub const DEFAULT_ASSET_SYMBOL: &str = "BTC";

pub fn default_strategy_id() -> String {
    STRATEGY_ID_LEGACY_DEFAULT.to_string()
}

pub fn default_market_key() -> String {
    DEFAULT_MARKET_KEY.to_string()
}

pub fn default_asset_symbol() -> String {
    DEFAULT_ASSET_SYMBOL.to_string()
}

fn normalize_market_key(market_key: &str) -> String {
    let normalized = market_key.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        default_market_key()
    } else {
        normalized
    }
}

fn normalize_asset_symbol(asset_symbol: &str, market_key: &str) -> String {
    let normalized = asset_symbol.trim().to_ascii_uppercase();
    if normalized.is_empty() {
        normalize_market_key(market_key).to_ascii_uppercase()
    } else {
        normalized
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Timeframe {
    M5,
    M15,
    H1,
    H4,
    D1,
}

impl Timeframe {
    pub fn all() -> [Timeframe; 3] {
        [Timeframe::M5, Timeframe::M15, Timeframe::H1]
    }

    pub fn all_with_h4() -> [Timeframe; 4] {
        [Timeframe::M5, Timeframe::M15, Timeframe::H1, Timeframe::H4]
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Timeframe::M5 => "5m",
            Timeframe::M15 => "15m",
            Timeframe::H1 => "1h",
            Timeframe::H4 => "4h",
            Timeframe::D1 => "1d",
        }
    }

    pub fn duration_seconds(self) -> i64 {
        match self {
            Timeframe::M5 => 5 * 60,
            Timeframe::M15 => 15 * 60,
            Timeframe::H1 => 60 * 60,
            Timeframe::H4 => 4 * 60 * 60,
            Timeframe::D1 => 24 * 60 * 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum StrategyAction {
    Trade,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Direction {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketPhase {
    PreOpen,
    Accumulation,
    MidMarket,
    Distribution,
}

impl MarketPhase {
    pub fn from_timing(time_elapsed_s: u64, period_duration_s: u64, is_preopen: bool) -> Self {
        if is_preopen {
            return Self::PreOpen;
        }
        if period_duration_s == 0 {
            return Self::MidMarket;
        }
        let pct = time_elapsed_s as f64 / period_duration_s as f64;
        if pct <= 0.20 {
            Self::Accumulation
        } else if pct >= 0.80 {
            Self::Distribution
        } else {
            Self::MidMarket
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LadderRung {
    pub price: f64,
    pub size_pct: f64,
    pub cancel_after_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LadderPlan {
    pub rungs: Vec<LadderRung>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccumulationPlan {
    pub max_price: f64,
    pub target_size_usd: f64,
    pub order_type: String,
    pub urgency: String,
    #[serde(default)]
    pub fair_probability: Option<f64>,
    #[serde(default)]
    pub uncertainty: Option<f64>,
    #[serde(default)]
    pub ev_floor_bps: Option<f64>,
    #[serde(default)]
    pub ladder: Option<LadderPlan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitPlan {
    pub take_profit_prices: Vec<f64>,
    pub take_profit_pcts: Vec<f64>,
    pub stop_loss_price: f64,
    pub whale_exit_enabled: bool,
    pub whale_exit_threshold_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DecisionTelemetry {
    pub context_json: String,
    pub signal_digest_json: String,
    pub parse_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyDecision {
    #[serde(default = "default_strategy_id")]
    pub strategy_id: String,
    #[serde(default = "default_market_key")]
    pub market_key: String,
    #[serde(default = "default_asset_symbol")]
    pub asset_symbol: String,
    pub timeframe: Timeframe,
    pub market_open_ts: i64,
    pub generated_at_ms: i64,
    pub source: String,
    pub action: StrategyAction,
    pub direction: Option<Direction>,
    pub mode: Option<String>,
    pub confidence: f64,
    pub accumulation: Option<AccumulationPlan>,
    pub exit_plan: Option<ExitPlan>,
    pub reasoning: String,
    #[serde(default)]
    pub telemetry: Option<DecisionTelemetry>,
}

#[derive(Debug, Default)]
pub struct StrategyBook {
    by_slot: HashMap<(Timeframe, String, String), StrategyDecision>,
}

impl StrategyBook {
    pub fn upsert(&mut self, decision: StrategyDecision) {
        let mut decision = decision;
        decision.market_key = normalize_market_key(decision.market_key.as_str());
        decision.asset_symbol =
            normalize_asset_symbol(decision.asset_symbol.as_str(), decision.market_key.as_str());
        let key = (
            decision.timeframe,
            decision.strategy_id.clone(),
            decision.market_key.clone(),
        );
        let should_replace = self
            .by_slot
            .get(&key)
            .map(|existing| decision.generated_at_ms >= existing.generated_at_ms)
            .unwrap_or(true);
        if should_replace {
            self.by_slot.insert(key, decision);
        }
    }

    pub fn latest_for_timeframe(&self, timeframe: Timeframe) -> Option<&StrategyDecision> {
        self.by_slot
            .values()
            .filter(|decision| decision.timeframe == timeframe)
            .max_by_key(|decision| decision.generated_at_ms)
    }

    pub fn latest_for_slot(
        &self,
        timeframe: Timeframe,
        strategy_id: &str,
    ) -> Option<&StrategyDecision> {
        self.by_slot
            .values()
            .filter(|decision| {
                decision.timeframe == timeframe
                    && decision.strategy_id.eq_ignore_ascii_case(strategy_id)
            })
            .max_by_key(|decision| decision.generated_at_ms)
    }

    pub fn latest_for_market_slot(
        &self,
        timeframe: Timeframe,
        strategy_id: &str,
        market_key: &str,
    ) -> Option<&StrategyDecision> {
        self.by_slot.get(&(
            timeframe,
            strategy_id.to_string(),
            normalize_market_key(market_key),
        ))
    }

    pub fn decisions_for_timeframe(&self, timeframe: Timeframe) -> Vec<StrategyDecision> {
        let mut out = self
            .by_slot
            .values()
            .filter(|decision| decision.timeframe == timeframe)
            .cloned()
            .collect::<Vec<_>>();
        out.sort_by(|a, b| {
            a.strategy_id
                .cmp(&b.strategy_id)
                .then(a.market_key.cmp(&b.market_key))
                .then(b.generated_at_ms.cmp(&a.generated_at_ms))
        });
        out
    }

    pub fn snapshot(&self) -> Vec<StrategyDecision> {
        let mut out = self.by_slot.values().cloned().collect::<Vec<_>>();
        out.sort_by(|a, b| {
            a.timeframe
                .as_str()
                .cmp(b.timeframe.as_str())
                .then(a.strategy_id.cmp(&b.strategy_id))
                .then(a.market_key.cmp(&b.market_key))
                .then(b.generated_at_ms.cmp(&a.generated_at_ms))
        });
        out
    }
}

pub type SharedStrategyBook = Arc<RwLock<StrategyBook>>;

pub fn new_shared_strategy_book() -> SharedStrategyBook {
    Arc::new(RwLock::new(StrategyBook::default()))
}

pub fn fallback_strategy(
    timeframe: Timeframe,
    market_open_ts: i64,
    signals: &SignalState,
    reason: &str,
) -> StrategyDecision {
    let adx = signals.trend.adx.unwrap_or(0.0);
    let trend_score = signals.trend.trend_score.unwrap_or(0.0);
    let plus = signals.trend.di_plus.unwrap_or(0.0);
    let minus = signals.trend.di_minus.unwrap_or(0.0);
    let volume_ratio = signals.binance_flow.volume_ratio_1m.unwrap_or(1.0);

    let weak_regime = adx < 18.0 || trend_score.abs() < 20.0 || volume_ratio < 0.7;
    if weak_regime {
        return StrategyDecision {
            strategy_id: default_strategy_id(),
            market_key: default_market_key(),
            asset_symbol: default_asset_symbol(),
            timeframe,
            market_open_ts,
            generated_at_ms: chrono::Utc::now().timestamp_millis(),
            source: "fallback".to_string(),
            action: StrategyAction::Skip,
            direction: None,
            mode: None,
            confidence: 0.0,
            accumulation: None,
            exit_plan: None,
            reasoning: format!("Fallback skip: weak regime ({})", reason),
            telemetry: None,
        };
    }

    let direction = if plus >= minus {
        Direction::Up
    } else {
        Direction::Down
    };

    let (max_price, target_size_usd) = match timeframe {
        Timeframe::M5 => (0.40, 15.0),
        Timeframe::M15 => (0.42, 25.0),
        Timeframe::H1 => (0.45, 40.0),
        Timeframe::H4 => (0.50, 60.0),
        Timeframe::D1 => (0.50, 60.0),
    };

    StrategyDecision {
        strategy_id: default_strategy_id(),
        market_key: default_market_key(),
        asset_symbol: default_asset_symbol(),
        timeframe,
        market_open_ts,
        generated_at_ms: chrono::Utc::now().timestamp_millis(),
        source: "fallback".to_string(),
        action: StrategyAction::Trade,
        direction: Some(direction),
        mode: Some("CONSERVATIVE_FALLBACK".to_string()),
        confidence: 0.45,
        accumulation: Some(AccumulationPlan {
            max_price,
            target_size_usd,
            order_type: "LIMIT".to_string(),
            urgency: "NORMAL".to_string(),
            fair_probability: Some(max_price),
            uncertainty: None,
            ev_floor_bps: None,
            ladder: Some(default_ladder(timeframe)),
        }),
        exit_plan: Some(ExitPlan {
            take_profit_prices: vec![0.60, 0.80],
            take_profit_pcts: vec![50.0, 50.0],
            stop_loss_price: 0.15,
            whale_exit_enabled: true,
            whale_exit_threshold_usd: 1_000_000.0,
        }),
        reasoning: format!(
            "Fallback trade: deterministic conservative mode ({})",
            reason
        ),
        telemetry: None,
    }
}

pub fn default_ladder(timeframe: Timeframe) -> LadderPlan {
    let (prices, sizes, cancels): (Vec<f64>, Vec<f64>, Vec<f64>) = match timeframe {
        Timeframe::M5 => (
            vec![0.40, 0.32, 0.24, 0.16],
            vec![0.50, 0.26, 0.16, 0.08],
            vec![0.22, 0.35, 0.50, 0.65],
        ),
        Timeframe::M15 => (
            vec![0.40, 0.30, 0.20, 0.10],
            vec![0.50, 0.26, 0.16, 0.08],
            vec![0.30, 0.45, 0.60, 0.75],
        ),
        Timeframe::H1 => (
            vec![0.45, 0.35, 0.25, 0.15],
            vec![0.45, 0.30, 0.17, 0.08],
            vec![0.35, 0.50, 0.70, 0.85],
        ),
        Timeframe::H4 => (
            vec![0.50, 0.40, 0.30, 0.20],
            vec![0.45, 0.30, 0.17, 0.08],
            vec![0.40, 0.55, 0.75, 0.90],
        ),
        Timeframe::D1 => (
            vec![0.50, 0.40, 0.30, 0.20],
            vec![0.45, 0.30, 0.17, 0.08],
            vec![0.40, 0.55, 0.75, 0.90],
        ),
    };

    let rungs = prices
        .into_iter()
        .zip(sizes.into_iter())
        .zip(cancels.into_iter())
        .map(|((price, size_pct), cancel_after_pct)| LadderRung {
            price,
            size_pct,
            cancel_after_pct,
        })
        .collect::<Vec<_>>();

    LadderPlan { rungs }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategy_book_keeps_separate_slots_per_strategy() {
        let mut book = StrategyBook::default();
        let mut a = fallback_strategy(Timeframe::M15, 1_700_000_000, &SignalState::default(), "a");
        a.strategy_id = STRATEGY_ID_PREMARKET_V1.to_string();
        a.generated_at_ms = 1_000;
        let mut b = a.clone();
        b.strategy_id = STRATEGY_ID_ENDGAME_SWEEP_V1.to_string();
        b.generated_at_ms = 1_100;

        book.upsert(a);
        book.upsert(b);

        assert_eq!(book.snapshot().len(), 2);
    }

    #[test]
    fn strategy_book_replaces_only_within_same_slot() {
        let mut book = StrategyBook::default();
        let mut old =
            fallback_strategy(Timeframe::M5, 1_700_000_000, &SignalState::default(), "old");
        old.strategy_id = STRATEGY_ID_PREMARKET_V1.to_string();
        old.generated_at_ms = 100;
        let mut newer = old.clone();
        newer.generated_at_ms = 200;
        let mut other_strategy = old.clone();
        other_strategy.strategy_id = STRATEGY_ID_ENDGAME_SWEEP_V1.to_string();
        other_strategy.generated_at_ms = 150;

        book.upsert(old);
        book.upsert(newer.clone());
        book.upsert(other_strategy);

        let snapshot = book.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert!(snapshot
            .iter()
            .any(|d| d.strategy_id == STRATEGY_ID_PREMARKET_V1 && d.generated_at_ms == 200));
        assert_eq!(
            book.latest_for_slot(Timeframe::M5, STRATEGY_ID_PREMARKET_V1)
                .unwrap()
                .generated_at_ms,
            200
        );
        assert_eq!(
            book.latest_for_slot(Timeframe::M5, STRATEGY_ID_ENDGAME_SWEEP_V1)
                .unwrap()
                .generated_at_ms,
            150
        );
        let per_tf = book.decisions_for_timeframe(Timeframe::M5);
        assert_eq!(per_tf.len(), 2);
        assert_eq!(per_tf[0].strategy_id, STRATEGY_ID_ENDGAME_SWEEP_V1);
        assert_eq!(per_tf[1].strategy_id, STRATEGY_ID_PREMARKET_V1);
    }

    #[test]
    fn strategy_book_keeps_separate_slots_per_market_key() {
        let mut book = StrategyBook::default();
        let mut btc =
            fallback_strategy(Timeframe::M5, 1_700_000_000, &SignalState::default(), "btc");
        btc.strategy_id = STRATEGY_ID_PREMARKET_V1.to_string();
        btc.market_key = "btc".to_string();
        btc.asset_symbol = "BTC".to_string();
        btc.generated_at_ms = 100;

        let mut eth = btc.clone();
        eth.market_key = "eth".to_string();
        eth.asset_symbol = "ETH".to_string();
        eth.generated_at_ms = 200;

        book.upsert(btc.clone());
        book.upsert(eth.clone());

        assert_eq!(book.snapshot().len(), 2);
        assert_eq!(
            book.latest_for_market_slot(Timeframe::M5, STRATEGY_ID_PREMARKET_V1, "btc")
                .unwrap()
                .asset_symbol,
            "BTC"
        );
        assert_eq!(
            book.latest_for_market_slot(Timeframe::M5, STRATEGY_ID_PREMARKET_V1, "eth")
                .unwrap()
                .asset_symbol,
            "ETH"
        );
    }
}
