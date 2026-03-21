use std::collections::HashMap;

use crate::strategy::{
    Direction, Timeframe, STRATEGY_ID_ENDGAME_SWEEP_V1, STRATEGY_ID_EVCURVE_V1,
    STRATEGY_ID_EVSNIPE_V1, STRATEGY_ID_MM_REWARDS_V1, STRATEGY_ID_MM_SPORT_V1,
    STRATEGY_ID_PREMARKET_V1, STRATEGY_ID_SESSIONBAND_V1,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArbiterMode {
    Priority,
    Weighted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    PickBest,
    SkipBoth,
}

#[derive(Debug, Clone)]
pub struct ArbiterConfig {
    pub enabled: bool,
    pub mode: ArbiterMode,
    pub conflict_policy: ConflictPolicy,
    pub allow_parallel_strategies: bool,
    pub global_max_usd: f64,
    pub per_strategy_max_usd: HashMap<String, f64>,
    pub per_timeframe_max_usd: HashMap<Timeframe, f64>,
}

impl Default for ArbiterConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

impl ArbiterConfig {
    pub fn from_env() -> Self {
        let global_max_usd = env_f64("EVPOLY_ARB_GLOBAL_MAX_USD", 100_000.0).max(0.0);
        let mut per_strategy_max_usd = HashMap::new();
        per_strategy_max_usd.insert(
            STRATEGY_ID_PREMARKET_V1.to_string(),
            env_f64("EVPOLY_ARB_STRAT_PREMARKET_MAX_USD", global_max_usd).max(0.0),
        );
        per_strategy_max_usd.insert(
            STRATEGY_ID_ENDGAME_SWEEP_V1.to_string(),
            env_f64("EVPOLY_ARB_STRAT_ENDGAME_MAX_USD", global_max_usd).max(0.0),
        );
        per_strategy_max_usd.insert(
            STRATEGY_ID_EVCURVE_V1.to_string(),
            env_f64("EVPOLY_ARB_STRAT_EVCURVE_MAX_USD", global_max_usd).max(0.0),
        );
        per_strategy_max_usd.insert(
            STRATEGY_ID_SESSIONBAND_V1.to_string(),
            env_f64("EVPOLY_ARB_STRAT_SESSIONBAND_MAX_USD", global_max_usd).max(0.0),
        );
        per_strategy_max_usd.insert(
            STRATEGY_ID_EVSNIPE_V1.to_string(),
            env_f64("EVPOLY_ARB_STRAT_EVSNIPE_MAX_USD", global_max_usd).max(0.0),
        );
        per_strategy_max_usd.insert(
            STRATEGY_ID_MM_REWARDS_V1.to_string(),
            env_f64("EVPOLY_ARB_STRAT_MM_REWARDS_MAX_USD", global_max_usd).max(0.0),
        );
        per_strategy_max_usd.insert(
            STRATEGY_ID_MM_SPORT_V1.to_string(),
            env_f64("EVPOLY_ARB_STRAT_MM_SPORT_MAX_USD", global_max_usd).max(0.0),
        );

        let mut per_timeframe_max_usd = HashMap::new();
        if let Some(v) = env_f64_opt("EVPOLY_ARB_TF_5M_MAX_USD") {
            per_timeframe_max_usd.insert(Timeframe::M5, v.max(0.0));
        }
        if let Some(v) = env_f64_opt("EVPOLY_ARB_TF_15M_MAX_USD") {
            per_timeframe_max_usd.insert(Timeframe::M15, v.max(0.0));
        }
        if let Some(v) = env_f64_opt("EVPOLY_ARB_TF_1H_MAX_USD") {
            per_timeframe_max_usd.insert(Timeframe::H1, v.max(0.0));
        }
        if let Some(v) = env_f64_opt("EVPOLY_ARB_TF_4H_MAX_USD") {
            per_timeframe_max_usd.insert(Timeframe::H4, v.max(0.0));
        }

        let mode = match std::env::var("EVPOLY_ARB_MODE")
            .ok()
            .unwrap_or_else(|| "priority".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "weighted" => ArbiterMode::Weighted,
            _ => ArbiterMode::Priority,
        };

        let conflict_policy = match std::env::var("EVPOLY_ARB_CONFLICT_POLICY")
            .ok()
            .unwrap_or_else(|| "pick_best".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "skip_both" => ConflictPolicy::SkipBoth,
            _ => ConflictPolicy::PickBest,
        };

        Self {
            enabled: env_bool("EVPOLY_ARB_ENABLE", true),
            mode,
            conflict_policy,
            allow_parallel_strategies: env_bool("EVPOLY_ARB_ALLOW_PARALLEL_STRATEGIES", true),
            global_max_usd,
            per_strategy_max_usd,
            per_timeframe_max_usd,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StrategyIntent {
    pub strategy_id: String,
    pub timeframe: Timeframe,
    pub market_open_ts: i64,
    pub token_id: String,
    pub direction: Direction,
    pub max_price: f64,
    pub target_size_usd: f64,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArbiterDecisionStatus {
    Approved,
    Rejected,
}

#[derive(Debug, Clone)]
pub struct ArbiterIntentResult {
    pub intent: StrategyIntent,
    pub status: ArbiterDecisionStatus,
    pub allocated_size_usd: f64,
    pub reason: Option<String>,
}

impl ArbiterIntentResult {
    fn approve(intent: StrategyIntent) -> Self {
        let allocated_size_usd = intent.target_size_usd.max(0.0);
        Self {
            intent,
            status: ArbiterDecisionStatus::Approved,
            allocated_size_usd,
            reason: None,
        }
    }

    fn reject(intent: StrategyIntent, reason: impl Into<String>) -> Self {
        Self {
            intent,
            status: ArbiterDecisionStatus::Rejected,
            allocated_size_usd: 0.0,
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Arbiter {
    cfg: ArbiterConfig,
}

impl Arbiter {
    pub fn new(cfg: ArbiterConfig) -> Self {
        Self { cfg }
    }

    pub fn config(&self) -> &ArbiterConfig {
        &self.cfg
    }

    pub fn submit_intents(&self, intents: Vec<StrategyIntent>) -> Vec<ArbiterIntentResult> {
        if !self.cfg.enabled {
            return intents
                .into_iter()
                .map(|intent| ArbiterIntentResult::approve(intent))
                .collect();
        }

        let resolved = self.resolve_conflicts(intents);
        let budgeted = self.apply_budgets(resolved);
        self.approve_for_execution(budgeted)
    }

    pub fn resolve_conflicts(&self, intents: Vec<StrategyIntent>) -> Vec<ArbiterIntentResult> {
        let mut deduped: HashMap<(i64, String, String, String, String), (usize, StrategyIntent)> =
            HashMap::new();
        let mut results: Vec<Option<ArbiterIntentResult>> = vec![None; intents.len()];

        for (idx, intent) in intents.into_iter().enumerate() {
            let key = (
                intent.market_open_ts,
                intent.token_id.clone(),
                direction_label(&intent.direction).to_string(),
                intent.strategy_id.clone(),
                dedupe_variant_key(&intent),
            );
            if let Some((prev_idx, existing)) = deduped.get(&key) {
                if self.is_better(&intent, existing) {
                    if let Some((replaced_idx, prev)) = deduped.insert(key, (idx, intent.clone())) {
                        results[replaced_idx] =
                            Some(ArbiterIntentResult::reject(prev, "dedupe_exact_key_lost"));
                    }
                } else {
                    let _ = prev_idx;
                    results[idx] =
                        Some(ArbiterIntentResult::reject(intent, "dedupe_exact_key_lost"));
                }
            } else {
                deduped.insert(key, (idx, intent));
            }
        }
        if self.cfg.allow_parallel_strategies {
            for (_, (idx, intent)) in deduped {
                results[idx] = Some(ArbiterIntentResult::approve(intent));
            }
            return results
                .into_iter()
                .map(|item| item.expect("arbiter result missing for deduped intent"))
                .collect();
        }

        let mut by_market: HashMap<(i64, String), Vec<(usize, StrategyIntent)>> = HashMap::new();
        for (_, (idx, intent)) in deduped {
            by_market
                .entry((intent.market_open_ts, intent.token_id.clone()))
                .or_default()
                .push((idx, intent));
        }

        for mut candidates in by_market.into_values() {
            if candidates.len() <= 1 {
                if let Some((idx, only)) = candidates.pop() {
                    results[idx] = Some(ArbiterIntentResult::approve(only));
                }
                continue;
            }

            match self.cfg.conflict_policy {
                ConflictPolicy::SkipBoth => {
                    for (idx, intent) in candidates {
                        results[idx] = Some(ArbiterIntentResult::reject(
                            intent,
                            "conflict_opposite_side_skip_both",
                        ));
                    }
                }
                ConflictPolicy::PickBest => {
                    let mut best_idx = 0usize;
                    for (idx, (_, intent)) in candidates.iter().enumerate().skip(1) {
                        if self.is_better(intent, &candidates[best_idx].1) {
                            best_idx = idx;
                        }
                    }

                    for (idx, (result_idx, intent)) in candidates.into_iter().enumerate() {
                        if idx == best_idx {
                            results[result_idx] = Some(ArbiterIntentResult::approve(intent));
                        } else {
                            results[result_idx] = Some(ArbiterIntentResult::reject(
                                intent,
                                "conflict_opposite_side_lost",
                            ));
                        }
                    }
                }
            }
        }

        results
            .into_iter()
            .map(|item| item.expect("arbiter result missing after conflict resolution"))
            .collect()
    }

    pub fn apply_budgets(&self, intents: Vec<ArbiterIntentResult>) -> Vec<ArbiterIntentResult> {
        let mut used_global = 0.0_f64;
        let mut used_by_strategy: HashMap<String, f64> = HashMap::new();
        let mut used_by_timeframe: HashMap<Timeframe, f64> = HashMap::new();

        let mut out = Vec::with_capacity(intents.len());
        for mut item in intents {
            if item.status != ArbiterDecisionStatus::Approved {
                out.push(item);
                continue;
            }

            let strategy_cap = self
                .cfg
                .per_strategy_max_usd
                .get(&item.intent.strategy_id)
                .copied()
                .unwrap_or(self.cfg.global_max_usd)
                .max(0.0);
            let timeframe_cap = self
                .cfg
                .per_timeframe_max_usd
                .get(&item.intent.timeframe)
                .copied()
                .unwrap_or(f64::INFINITY)
                .max(0.0);

            let global_remaining = (self.cfg.global_max_usd - used_global).max(0.0);
            let strategy_used = used_by_strategy
                .get(&item.intent.strategy_id)
                .copied()
                .unwrap_or(0.0);
            let strategy_remaining = (strategy_cap - strategy_used).max(0.0);
            let timeframe_used = used_by_timeframe
                .get(&item.intent.timeframe)
                .copied()
                .unwrap_or(0.0);
            let timeframe_remaining = (timeframe_cap - timeframe_used).max(0.0);

            let alloc = item
                .allocated_size_usd
                .min(global_remaining)
                .min(strategy_remaining)
                .min(timeframe_remaining)
                .max(0.0);

            if alloc <= 0.0 {
                item.status = ArbiterDecisionStatus::Rejected;
                item.allocated_size_usd = 0.0;
                item.reason = Some("budget_exhausted".to_string());
                out.push(item);
                continue;
            }

            item.allocated_size_usd = alloc;
            used_global += alloc;
            *used_by_strategy
                .entry(item.intent.strategy_id.clone())
                .or_insert(0.0) += alloc;
            *used_by_timeframe
                .entry(item.intent.timeframe)
                .or_insert(0.0) += alloc;
            out.push(item);
        }

        out
    }

    pub fn approve_for_execution(
        &self,
        intents: Vec<ArbiterIntentResult>,
    ) -> Vec<ArbiterIntentResult> {
        intents
            .into_iter()
            .map(|mut item| {
                if item.status != ArbiterDecisionStatus::Approved {
                    return item;
                }

                if !(0.0..=1.0).contains(&item.intent.max_price)
                    || item.intent.max_price <= 0.0
                    || item.allocated_size_usd <= 0.0
                {
                    item.status = ArbiterDecisionStatus::Rejected;
                    item.allocated_size_usd = 0.0;
                    item.reason = Some("invalid_execution_params".to_string());
                }
                item
            })
            .collect()
    }

    fn is_better(&self, candidate: &StrategyIntent, current: &StrategyIntent) -> bool {
        match self.cfg.mode {
            ArbiterMode::Priority => {
                let c_rank = strategy_priority(candidate.strategy_id.as_str());
                let cur_rank = strategy_priority(current.strategy_id.as_str());
                c_rank < cur_rank || (c_rank == cur_rank && candidate.score > current.score)
            }
            ArbiterMode::Weighted => candidate.score > current.score,
        }
    }
}

fn dedupe_variant_key(intent: &StrategyIntent) -> String {
    if intent.strategy_id == STRATEGY_ID_PREMARKET_V1 {
        // Premarket ladder submits many rungs for the same token/side in one batch.
        // Keep rung-level uniqueness by including quantized price in the exact-key dedupe tuple.
        return format!("{:.6}", intent.max_price);
    }
    "_".to_string()
}

fn strategy_priority(strategy_id: &str) -> u8 {
    match strategy_id {
        STRATEGY_ID_PREMARKET_V1 => 0,
        STRATEGY_ID_ENDGAME_SWEEP_V1 => 1,
        STRATEGY_ID_SESSIONBAND_V1 => 2,
        STRATEGY_ID_EVCURVE_V1 => 3,
        STRATEGY_ID_EVSNIPE_V1 => 4,
        STRATEGY_ID_MM_REWARDS_V1 => 5,
        STRATEGY_ID_MM_SPORT_V1 => 6,
        _ => 9,
    }
}

fn direction_label(direction: &Direction) -> &'static str {
    match direction {
        Direction::Up => "UP",
        Direction::Down => "DOWN",
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn env_f64_opt(key: &str) -> Option<f64> {
    std::env::var(key).ok().and_then(|v| v.parse::<f64>().ok())
}

fn env_f64(key: &str, default: f64) -> f64 {
    env_f64_opt(key).unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::{
        STRATEGY_ID_ENDGAME_SWEEP_V1, STRATEGY_ID_PREMARKET_V1, STRATEGY_ID_SESSIONBAND_V1,
    };
    use std::collections::HashMap;

    fn intent(
        strategy_id: &str,
        direction: Direction,
        score: f64,
        size_usd: f64,
    ) -> StrategyIntent {
        StrategyIntent {
            strategy_id: strategy_id.to_string(),
            timeframe: Timeframe::M15,
            market_open_ts: 1_700_000_000,
            token_id: "token-1".to_string(),
            direction,
            max_price: 0.42,
            target_size_usd: size_usd,
            score,
        }
    }

    #[test]
    fn resolve_conflict_prefers_priority_strategy() {
        let cfg = ArbiterConfig {
            enabled: true,
            mode: ArbiterMode::Priority,
            conflict_policy: ConflictPolicy::PickBest,
            allow_parallel_strategies: false,
            global_max_usd: 200.0,
            per_strategy_max_usd: HashMap::new(),
            per_timeframe_max_usd: HashMap::new(),
        };
        let arbiter = Arbiter::new(cfg);

        let results = arbiter.submit_intents(vec![
            intent(STRATEGY_ID_ENDGAME_SWEEP_V1, Direction::Up, 0.9, 30.0),
            intent(STRATEGY_ID_PREMARKET_V1, Direction::Down, 0.2, 30.0),
        ]);

        let approved = results
            .iter()
            .filter(|r| r.status == ArbiterDecisionStatus::Approved)
            .collect::<Vec<_>>();
        assert_eq!(approved.len(), 1);
        assert_eq!(approved[0].intent.strategy_id, STRATEGY_ID_PREMARKET_V1);
    }

    #[test]
    fn budgets_clip_by_global_cap() {
        let cfg = ArbiterConfig {
            enabled: true,
            mode: ArbiterMode::Weighted,
            conflict_policy: ConflictPolicy::PickBest,
            allow_parallel_strategies: true,
            global_max_usd: 50.0,
            per_strategy_max_usd: HashMap::new(),
            per_timeframe_max_usd: HashMap::new(),
        };
        let arbiter = Arbiter::new(cfg);

        let results = arbiter.submit_intents(vec![
            intent(STRATEGY_ID_PREMARKET_V1, Direction::Up, 0.9, 40.0),
            StrategyIntent {
                token_id: "token-2".to_string(),
                ..intent(STRATEGY_ID_ENDGAME_SWEEP_V1, Direction::Up, 0.8, 40.0)
            },
        ]);
        let approved = results
            .iter()
            .filter(|r| r.status == ArbiterDecisionStatus::Approved)
            .collect::<Vec<_>>();
        assert_eq!(approved.len(), 2);
        let total_alloc = approved.iter().map(|r| r.allocated_size_usd).sum::<f64>();
        assert!((total_alloc - 50.0).abs() < 1e-6);
    }

    #[test]
    fn submit_intents_preserves_input_order() {
        let cfg = ArbiterConfig {
            enabled: true,
            mode: ArbiterMode::Weighted,
            conflict_policy: ConflictPolicy::PickBest,
            allow_parallel_strategies: true,
            global_max_usd: 100.0,
            per_strategy_max_usd: HashMap::new(),
            per_timeframe_max_usd: HashMap::new(),
        };
        let arbiter = Arbiter::new(cfg);

        let results = arbiter.submit_intents(vec![
            StrategyIntent {
                token_id: "token-a".to_string(),
                ..intent(STRATEGY_ID_PREMARKET_V1, Direction::Up, 0.9, 20.0)
            },
            StrategyIntent {
                token_id: "token-b".to_string(),
                ..intent(STRATEGY_ID_ENDGAME_SWEEP_V1, Direction::Down, 0.8, 10.0)
            },
        ]);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].intent.token_id, "token-a");
        assert_eq!(results[1].intent.token_id, "token-b");
    }

    #[test]
    fn premarket_exact_dedupe_keeps_distinct_rungs() {
        let cfg = ArbiterConfig {
            enabled: true,
            mode: ArbiterMode::Weighted,
            conflict_policy: ConflictPolicy::PickBest,
            allow_parallel_strategies: true,
            global_max_usd: 1_000.0,
            per_strategy_max_usd: HashMap::new(),
            per_timeframe_max_usd: HashMap::new(),
        };
        let arbiter = Arbiter::new(cfg);

        let base = intent(STRATEGY_ID_PREMARKET_V1, Direction::Up, 0.5, 25.0);
        let results = arbiter.submit_intents(vec![
            StrategyIntent {
                max_price: 0.49,
                ..base.clone()
            },
            StrategyIntent {
                max_price: 0.48,
                ..base
            },
        ]);

        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|r| r.status == ArbiterDecisionStatus::Approved));
    }

    #[test]
    fn non_premarket_exact_dedupe_still_blocks_duplicates() {
        let cfg = ArbiterConfig {
            enabled: true,
            mode: ArbiterMode::Weighted,
            conflict_policy: ConflictPolicy::PickBest,
            allow_parallel_strategies: true,
            global_max_usd: 1_000.0,
            per_strategy_max_usd: HashMap::new(),
            per_timeframe_max_usd: HashMap::new(),
        };
        let arbiter = Arbiter::new(cfg);

        let dupe = intent(STRATEGY_ID_SESSIONBAND_V1, Direction::Up, 0.5, 25.0);
        let results = arbiter.submit_intents(vec![dupe.clone(), dupe]);
        let rejected = results
            .iter()
            .filter(|r| r.status == ArbiterDecisionStatus::Rejected)
            .count();
        assert_eq!(rejected, 1);
    }
}
