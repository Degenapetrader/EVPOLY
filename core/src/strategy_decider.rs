use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;
use serde_json::json;
use tokio::sync::mpsc::Sender;

use crate::config::FeatureEngineV2Config;
use crate::event_log::log_event;
use crate::signal_state::SharedSignalState;
use crate::strategy::{
    default_asset_symbol, default_market_key, SharedStrategyBook, StrategyAction, StrategyDecision,
    Timeframe, STRATEGY_ID_PREMARKET_V1,
};

#[derive(Debug, Clone)]
pub struct StrategyDeciderConfig {
    pub features_v2: FeatureEngineV2Config,
    pub enable_premarket: bool,
}

impl Default for StrategyDeciderConfig {
    fn default() -> Self {
        Self {
            features_v2: FeatureEngineV2Config::from_env(),
            enable_premarket: env_flag_or("EVPOLY_STRATEGY_PREMARKET_ENABLE", true),
        }
    }
}

fn env_flag_or(name: &str, default_value: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default_value)
}

fn env_i64_or(name: &str, default_value: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default_value)
}

#[derive(Debug, Clone)]
pub struct PremarketIntent {
    pub decision_id: String,
    pub timeframe: Timeframe,
    pub market_open_ts: i64,
    pub decision: StrategyDecision,
}

pub fn spawn_pre_market_scheduler(
    cfg: StrategyDeciderConfig,
    _signal_state: SharedSignalState,
    _strategy_book: SharedStrategyBook,
    premarket_tx: Sender<PremarketIntent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        let mut premarket_slot_minute: HashMap<Timeframe, i64> = HashMap::new();
        let mut last_processed_slot: Option<i64> = None;
        let premarket_enqueue_guard_ms = env_i64_or("EVPOLY_PREMARKET_ENQUEUE_GUARD_MS", 15_000);
        let premarket_enqueue_guard_sec = ((premarket_enqueue_guard_ms.max(0) + 999) / 1_000) + 1;

        crate::log_println!(
            "🧭 Strategy scheduler started (premarket_enabled={})",
            cfg.enable_premarket
        );

        loop {
            interval.tick().await;
            let slot_id = Utc::now().timestamp() / 60;
            let default_start_slot = slot_id.saturating_sub(1);
            let mut start_slot = last_processed_slot
                .map(|last| last.saturating_add(1))
                .unwrap_or(default_start_slot);
            if slot_id.saturating_sub(start_slot) > 120 {
                let old_start = start_slot;
                start_slot = slot_id.saturating_sub(120);
                log_event(
                    "strategy_scheduler_catchup_truncated",
                    json!({
                        "from_slot": old_start,
                        "to_slot": start_slot,
                        "current_slot": slot_id
                    }),
                );
            }

            if cfg.enable_premarket {
                for slot in start_slot..=slot_id {
                    for timeframe in premarket_set_for_slot(slot) {
                        if premarket_slot_minute.get(&timeframe).copied() == Some(slot) {
                            continue;
                        }
                        premarket_slot_minute.insert(timeframe, slot);

                        let slot_ts = slot.saturating_mul(60);
                        let market_open_ts = next_market_open_ts(timeframe, slot_ts);
                        let decision =
                            deterministic_premarket_seed_decision(timeframe, market_open_ts);
                        let intent = PremarketIntent {
                            decision_id: format!(
                                "{}:{}:{}:{}",
                                decision.strategy_id,
                                decision.timeframe.as_str(),
                                decision.market_open_ts,
                                decision.generated_at_ms
                            ),
                            timeframe: decision.timeframe,
                            market_open_ts: decision.market_open_ts,
                            decision,
                        };

                        if Utc::now().timestamp()
                            >= market_open_ts.saturating_sub(premarket_enqueue_guard_sec)
                        {
                            log_event(
                                "strategy_premarket_skip_late_intent",
                                json!({
                                    "strategy_id": STRATEGY_ID_PREMARKET_V1,
                                    "timeframe": timeframe.as_str(),
                                    "market_open_ts": market_open_ts,
                                    "enqueue_guard_ms": premarket_enqueue_guard_ms,
                                    "enqueue_guard_sec": premarket_enqueue_guard_sec
                                }),
                            );
                            continue;
                        }

                        if let Err(e) = premarket_tx.send(intent).await {
                            crate::log_println!(
                                "❌ Premarket channel closed tf={} open_ts={}: {}",
                                timeframe.as_str(),
                                market_open_ts,
                                e
                            );
                            log_event(
                                "strategy_premarket_channel_closed",
                                json!({
                                    "timeframe": timeframe.as_str(),
                                    "market_open_ts": market_open_ts,
                                    "error": e.to_string()
                                }),
                            );
                            return;
                        }
                    }
                }
            }

            last_processed_slot = Some(slot_id);
        }
    })
}

fn premarket_set_for_slot(slot: i64) -> Vec<Timeframe> {
    let minute = slot.rem_euclid(60) as u32;
    let hour = slot.div_euclid(60).rem_euclid(24) as u32;

    if simulation_all_timeframes_every_5m_enabled() {
        if minute % 5 == 1 {
            return vec![Timeframe::M5, Timeframe::M15, Timeframe::H1, Timeframe::H4];
        }
        return Vec::new();
    }

    let mut set = Vec::new();
    if minute % 5 == 1 {
        set.push(Timeframe::M5);
    }
    if minute % 15 == 11 {
        set.push(Timeframe::M15);
    }
    if minute == 56 {
        set.push(Timeframe::H1);
        if hour % 4 == 3 {
            set.push(Timeframe::H4);
        }
    }
    set
}

fn next_market_open_ts(timeframe: Timeframe, now_ts: i64) -> i64 {
    let d = timeframe.duration_seconds();
    ((now_ts / d) + 1) * d
}

fn deterministic_premarket_seed_decision(
    timeframe: Timeframe,
    market_open_ts: i64,
) -> StrategyDecision {
    StrategyDecision {
        strategy_id: STRATEGY_ID_PREMARKET_V1.to_string(),
        market_key: default_market_key(),
        asset_symbol: default_asset_symbol(),
        timeframe,
        market_open_ts,
        generated_at_ms: chrono::Utc::now().timestamp_millis(),
        source: "premarket_deterministic_scheduler".to_string(),
        action: StrategyAction::Trade,
        direction: None,
        mode: Some("PREMARKET_DETERMINISTIC_DUAL_LADDER".to_string()),
        confidence: 0.50,
        accumulation: None,
        exit_plan: None,
        reasoning:
            "Deterministic premarket schedule (dual-side fixed ladder, no LLM direction gate)"
                .to_string(),
        telemetry: None,
    }
}

fn simulation_all_timeframes_every_5m_enabled() -> bool {
    env_flag_or("EVPOLY_SIMULATE_ALL_TIMEFRAMES_EVERY_5M", false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premarket_schedule_runs_four_minutes_before_open_and_includes_h4() {
        // 03:56 UTC slot => minute 56 + hour%4==3 => H1/H4 plus 5m/15m
        let slot = (3 * 60 + 56) as i64;
        let set = premarket_set_for_slot(slot);
        assert!(set.contains(&Timeframe::M5));
        assert!(set.contains(&Timeframe::M15));
        assert!(set.contains(&Timeframe::H1));
        assert!(set.contains(&Timeframe::H4));

        // 01:56 UTC => no H4
        let slot = (1 * 60 + 56) as i64;
        let set = premarket_set_for_slot(slot);
        assert!(set.contains(&Timeframe::H1));
        assert!(!set.contains(&Timeframe::H4));
    }

    #[test]
    fn deterministic_premarket_seed_is_trade_and_directionless() {
        let market_open_ts = 1_771_920_000_i64;
        let decision = deterministic_premarket_seed_decision(Timeframe::M5, market_open_ts);
        assert_eq!(decision.strategy_id, STRATEGY_ID_PREMARKET_V1);
        assert_eq!(decision.market_open_ts, market_open_ts);
        assert!(matches!(decision.action, StrategyAction::Trade));
        assert!(decision.direction.is_none());
        assert_eq!(decision.source, "premarket_deterministic_scheduler");
    }
}
