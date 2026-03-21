use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntryLogicalKey {
    pub strategy_id: String,
    pub timeframe: String,
    pub period_timestamp: u64,
    pub token_id: String,
    pub side: String,
    pub rung_id: Option<String>,
}

impl EntryLogicalKey {
    pub fn new(
        strategy_id: impl AsRef<str>,
        timeframe: impl AsRef<str>,
        period_timestamp: u64,
        token_id: impl AsRef<str>,
        side: impl AsRef<str>,
        rung_id: Option<&str>,
    ) -> Self {
        Self {
            strategy_id: strategy_id.as_ref().trim().to_ascii_lowercase(),
            timeframe: timeframe.as_ref().trim().to_ascii_lowercase(),
            period_timestamp,
            token_id: token_id.as_ref().trim().to_string(),
            side: side.as_ref().trim().to_ascii_uppercase(),
            rung_id: rung_id
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty()),
        }
    }

    pub fn as_compact_string(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}",
            self.strategy_id,
            self.timeframe,
            self.period_timestamp,
            self.token_id,
            self.side,
            self.rung_id.as_deref().unwrap_or("-")
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntryScopeKey {
    pub strategy_id: String,
    pub period_timestamp: u64,
    pub timeframe: String,
    pub entry_mode: String,
}

impl EntryScopeKey {
    pub fn new(
        strategy_id: impl AsRef<str>,
        period_timestamp: u64,
        timeframe: impl AsRef<str>,
        entry_mode: impl AsRef<str>,
    ) -> Self {
        Self {
            strategy_id: strategy_id.as_ref().trim().to_ascii_lowercase(),
            period_timestamp,
            timeframe: timeframe.as_ref().trim().to_ascii_lowercase(),
            entry_mode: entry_mode.as_ref().trim().to_ascii_lowercase(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EntryIdempotencyConfig {
    pub enqueue_dedupe_cooldown_ms: i64,
    pub recent_done_ttl_ms: i64,
    pub recent_done_ttl_mm_rewards_ms: i64,
    pub failure_cooldown_ms: i64,
    pub retryable_failure_cooldown_ms: i64,
    pub parameter_invalid_cooldown_ms: i64,
    pub market_not_ready_cooldown_ms: i64,
    pub market_not_ready_jitter_ms: i64,
    pub retry_window_ms: i64,
    pub max_failures_per_window: usize,
    pub allowance_backoff_ms: i64,
    pub warning_cooldown_ms: i64,
}

impl Default for EntryIdempotencyConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

impl EntryIdempotencyConfig {
    pub fn from_env() -> Self {
        Self {
            enqueue_dedupe_cooldown_ms: env_i64("EVPOLY_ENTRY_DEDUPE_COOLDOWN_MS", 1_500, 100),
            recent_done_ttl_ms: env_i64("EVPOLY_ENTRY_RECENT_DONE_TTL_MS", 10_000, 250),
            recent_done_ttl_mm_rewards_ms: env_i64(
                "EVPOLY_ENTRY_RECENT_DONE_TTL_MM_REWARDS_MS",
                750,
                100,
            ),
            failure_cooldown_ms: env_i64("EVPOLY_ENTRY_FAILURE_COOLDOWN_MS", 8_000, 250),
            retryable_failure_cooldown_ms: env_i64(
                "EVPOLY_ENTRY_RETRYABLE_COOLDOWN_MS",
                3_000,
                100,
            ),
            parameter_invalid_cooldown_ms: env_i64(
                "EVPOLY_ENTRY_PARAMETER_INVALID_COOLDOWN_MS",
                30_000,
                500,
            ),
            market_not_ready_cooldown_ms: env_i64(
                "EVPOLY_ENTRY_MARKET_NOT_READY_COOLDOWN_MS",
                2_000,
                100,
            ),
            market_not_ready_jitter_ms: env_i64("EVPOLY_ENTRY_MARKET_NOT_READY_JITTER_MS", 500, 0),
            retry_window_ms: env_i64("EVPOLY_ENTRY_RETRY_WINDOW_MS", 60_000, 1_000),
            max_failures_per_window: env_usize("EVPOLY_ENTRY_MAX_FAILURES_PER_WINDOW", 4, 1),
            allowance_backoff_ms: env_i64("EVPOLY_ENTRY_ALLOWANCE_BACKOFF_MS", 60_000, 1_000),
            warning_cooldown_ms: env_i64("EVPOLY_ENTRY_WARNING_COOLDOWN_MS", 8_000, 500),
        }
    }
}

fn env_i64(key: &str, default: i64, min: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(default)
        .max(min)
}

fn env_usize(key: &str, default: usize, min: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
        .max(min)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueBlockReason {
    Cooldown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnqueueDecision {
    pub forwarded: bool,
    pub reason: Option<EnqueueBlockReason>,
    pub retry_at_ms: Option<i64>,
}

pub struct EnqueueDedupe {
    cooldown_ms: i64,
    cooldown_evcurve_ms: i64,
    seen_until_ms: HashMap<EntryLogicalKey, i64>,
}

impl EnqueueDedupe {
    pub fn new(cooldown_ms: i64) -> Self {
        let cooldown_ms = cooldown_ms.max(0);
        let cooldown_evcurve_ms = std::env::var("EVPOLY_ENTRY_DEDUPE_COOLDOWN_EVCURVE_MS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(cooldown_ms.max(5_000))
            .max(0);
        Self {
            cooldown_ms,
            cooldown_evcurve_ms,
            seen_until_ms: HashMap::new(),
        }
    }

    fn cooldown_for_key_ms(&self, key: &EntryLogicalKey) -> i64 {
        if key.strategy_id == "evcurve_v1" {
            self.cooldown_evcurve_ms
        } else {
            self.cooldown_ms
        }
    }

    pub fn should_forward(&mut self, key: &EntryLogicalKey, now_ms: i64) -> EnqueueDecision {
        self.prune(now_ms);
        if let Some(until_ms) = self.seen_until_ms.get(key) {
            if *until_ms > now_ms {
                return EnqueueDecision {
                    forwarded: false,
                    reason: Some(EnqueueBlockReason::Cooldown),
                    retry_at_ms: Some(*until_ms),
                };
            }
        }

        let until_ms = now_ms.saturating_add(self.cooldown_for_key_ms(key));
        self.seen_until_ms.insert(key.clone(), until_ms);
        EnqueueDecision {
            forwarded: true,
            reason: None,
            retry_at_ms: None,
        }
    }

    fn prune(&mut self, now_ms: i64) {
        self.seen_until_ms.retain(|_, until_ms| *until_ms > now_ms);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerDropReason {
    InFlight,
    RecentDone,
    Backoff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerBeginDecision {
    pub execute: bool,
    pub reason: Option<WorkerDropReason>,
    pub retry_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionErrorClass {
    BalanceAllowance,
    MarketNotReady,
    Retryable,
    ParameterInvalid,
    Permanent,
}

pub fn classify_entry_error(message: &str) -> ExecutionErrorClass {
    let msg = message.to_ascii_lowercase();

    let parameter_invalid = msg.contains("invalid_order_min_size")
        || msg.contains("invalid_order_min_tick_size")
        || msg.contains("minimum_order_size")
        || msg.contains("minimum tick size")
        || msg.contains("minimum_tick_size")
        || msg.contains("min tick")
        || msg.contains("min_size");
    if parameter_invalid {
        return ExecutionErrorClass::ParameterInvalid;
    }

    let balance_or_allowance = msg.contains("not enough balance / allowance")
        || msg.contains("insufficient usdc balance")
        || msg.contains("insufficient usdc allowance")
        || (msg.contains("insufficient") && msg.contains("allowance"))
        || (msg.contains("insufficient") && msg.contains("balance"))
        || msg.contains("not enough balance");
    if balance_or_allowance {
        return ExecutionErrorClass::BalanceAllowance;
    }

    let market_not_ready = msg.contains("market_not_ready")
        || msg.contains("market not ready")
        || msg.contains("market is not ready")
        || msg.contains("not accepting orders")
        || msg.contains("accepting_orders=false")
        || msg.contains("trading is not active");
    if market_not_ready {
        return ExecutionErrorClass::MarketNotReady;
    }

    let retryable = msg.contains("429")
        || msg.contains("rate limit")
        || msg.contains("too many requests")
        || msg.contains("timeout")
        || msg.contains("timed out")
        || msg.contains("status: 500")
        || msg.contains("status 500")
        || msg.contains("status: 502")
        || msg.contains("status 502")
        || msg.contains("status: 503")
        || msg.contains("status 503")
        || msg.contains("status: 504")
        || msg.contains("status 504")
        || msg.contains("temporar")
        || msg.contains("service unavailable")
        || msg.contains("bad gateway")
        || msg.contains("gateway timeout")
        || msg.contains("connection reset")
        || msg.contains("connection aborted")
        || msg.contains("connection refused");
    if retryable {
        return ExecutionErrorClass::Retryable;
    }

    ExecutionErrorClass::Permanent
}

pub struct WorkerIdempotency {
    cfg: EntryIdempotencyConfig,
    in_flight: HashSet<EntryLogicalKey>,
    in_flight_by_scope: HashMap<EntryScopeKey, usize>,
    recent_done_until_ms: HashMap<EntryLogicalKey, i64>,
    backoff_until_ms: HashMap<EntryLogicalKey, i64>,
    failure_windows: HashMap<EntryLogicalKey, RetryWindowState>,
    warning_suppression_until_ms: HashMap<(EntryLogicalKey, String), i64>,
}

#[derive(Debug, Clone, Copy)]
struct RetryWindowState {
    window_start_ms: i64,
    failures: usize,
}

impl WorkerIdempotency {
    pub fn new(cfg: EntryIdempotencyConfig) -> Self {
        Self {
            cfg,
            in_flight: HashSet::new(),
            in_flight_by_scope: HashMap::new(),
            recent_done_until_ms: HashMap::new(),
            backoff_until_ms: HashMap::new(),
            failure_windows: HashMap::new(),
            warning_suppression_until_ms: HashMap::new(),
        }
    }

    pub fn begin(
        &mut self,
        key: &EntryLogicalKey,
        scope: &EntryScopeKey,
        now_ms: i64,
    ) -> WorkerBeginDecision {
        self.prune(now_ms);

        if let Some(until_ms) = self.backoff_until_ms.get(key) {
            if *until_ms > now_ms {
                return WorkerBeginDecision {
                    execute: false,
                    reason: Some(WorkerDropReason::Backoff),
                    retry_at_ms: Some(*until_ms),
                };
            }
        }

        if let Some(until_ms) = self.recent_done_until_ms.get(key) {
            if *until_ms > now_ms {
                return WorkerBeginDecision {
                    execute: false,
                    reason: Some(WorkerDropReason::RecentDone),
                    retry_at_ms: Some(*until_ms),
                };
            }
        }

        if self.in_flight.contains(key) {
            return WorkerBeginDecision {
                execute: false,
                reason: Some(WorkerDropReason::InFlight),
                retry_at_ms: None,
            };
        }

        self.in_flight.insert(key.clone());
        *self.in_flight_by_scope.entry(scope.clone()).or_insert(0) += 1;

        WorkerBeginDecision {
            execute: true,
            reason: None,
            retry_at_ms: None,
        }
    }

    pub fn finish_success(
        &mut self,
        key: &EntryLogicalKey,
        scope: &EntryScopeKey,
        now_ms: i64,
    ) -> i64 {
        self.release_in_flight(key, scope);
        let until_ms = now_ms.saturating_add(self.recent_done_ttl_for_key(key));
        self.recent_done_until_ms.insert(key.clone(), until_ms);
        until_ms
    }

    pub fn finish_failure(
        &mut self,
        key: &EntryLogicalKey,
        scope: &EntryScopeKey,
        now_ms: i64,
        class: ExecutionErrorClass,
    ) -> i64 {
        self.release_in_flight(key, scope);

        let market_not_ready_jitter_ms = match class {
            ExecutionErrorClass::MarketNotReady => {
                jitter_ms_for_key(key, self.cfg.market_not_ready_jitter_ms)
            }
            _ => 0,
        };
        let cooldown_ms = match class {
            ExecutionErrorClass::BalanceAllowance => self.cfg.allowance_backoff_ms,
            ExecutionErrorClass::MarketNotReady => self
                .cfg
                .market_not_ready_cooldown_ms
                .saturating_add(market_not_ready_jitter_ms),
            ExecutionErrorClass::Retryable => self.cfg.retryable_failure_cooldown_ms,
            ExecutionErrorClass::ParameterInvalid => self.cfg.parameter_invalid_cooldown_ms,
            ExecutionErrorClass::Permanent => self.cfg.failure_cooldown_ms,
        }
        .max(0);

        let mut until_ms = now_ms.saturating_add(cooldown_ms);
        if cooldown_ms > 0 {
            self.recent_done_until_ms.insert(key.clone(), until_ms);
        }

        if matches!(
            class,
            ExecutionErrorClass::BalanceAllowance
                | ExecutionErrorClass::MarketNotReady
                | ExecutionErrorClass::ParameterInvalid
        ) && cooldown_ms > 0
        {
            self.backoff_until_ms.insert(key.clone(), until_ms);
        }

        if self.cfg.retry_window_ms > 0 && self.cfg.max_failures_per_window > 0 {
            let state = self
                .failure_windows
                .entry(key.clone())
                .or_insert(RetryWindowState {
                    window_start_ms: now_ms,
                    failures: 0,
                });
            if now_ms.saturating_sub(state.window_start_ms) >= self.cfg.retry_window_ms {
                state.window_start_ms = now_ms;
                state.failures = 0;
            }
            state.failures = state.failures.saturating_add(1);
            if state.failures >= self.cfg.max_failures_per_window {
                let window_until_ms = state
                    .window_start_ms
                    .saturating_add(self.cfg.retry_window_ms);
                until_ms = until_ms.max(window_until_ms);
                self.backoff_until_ms.insert(key.clone(), until_ms);
                self.recent_done_until_ms.insert(key.clone(), until_ms);
            }
        }

        until_ms
    }

    pub fn in_flight_count_for_scope(&self, scope: &EntryScopeKey) -> usize {
        self.in_flight_by_scope.get(scope).copied().unwrap_or(0)
    }

    pub fn should_emit_warning(
        &mut self,
        key: &EntryLogicalKey,
        reason: &str,
        now_ms: i64,
    ) -> bool {
        self.prune(now_ms);
        let reason_key = reason.to_ascii_lowercase();
        let map_key = (key.clone(), reason_key);
        match self.warning_suppression_until_ms.get(&map_key).copied() {
            Some(until_ms) if until_ms > now_ms => false,
            _ => {
                let until_ms = now_ms.saturating_add(self.cfg.warning_cooldown_ms);
                self.warning_suppression_until_ms.insert(map_key, until_ms);
                true
            }
        }
    }

    fn release_in_flight(&mut self, key: &EntryLogicalKey, scope: &EntryScopeKey) {
        if self.in_flight.remove(key) {
            if let Some(count) = self.in_flight_by_scope.get_mut(scope) {
                if *count > 1 {
                    *count -= 1;
                } else {
                    self.in_flight_by_scope.remove(scope);
                }
            }
        }
    }

    fn recent_done_ttl_for_key(&self, key: &EntryLogicalKey) -> i64 {
        if key.strategy_id == "mm_rewards_v1" {
            self.cfg.recent_done_ttl_mm_rewards_ms
        } else {
            self.cfg.recent_done_ttl_ms
        }
    }

    fn prune(&mut self, now_ms: i64) {
        self.recent_done_until_ms
            .retain(|_, until_ms| *until_ms > now_ms);
        self.backoff_until_ms
            .retain(|_, until_ms| *until_ms > now_ms);
        self.failure_windows.retain(|_, state| {
            now_ms.saturating_sub(state.window_start_ms) < self.cfg.retry_window_ms
        });
        self.warning_suppression_until_ms
            .retain(|_, until_ms| *until_ms > now_ms);
    }
}

fn jitter_ms_for_key(key: &EntryLogicalKey, max_jitter_ms: i64) -> i64 {
    if max_jitter_ms <= 0 {
        return 0;
    }
    let mut hash: u64 = 1469598103934665603;
    for b in key.as_compact_string().bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    (hash % (max_jitter_ms as u64 + 1)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> EntryLogicalKey {
        EntryLogicalKey::new(
            "premarket_v1",
            "1h",
            1_771_430_400,
            "token-123",
            "BUY",
            None,
        )
    }

    fn ladder_key(rung_id: &str) -> EntryLogicalKey {
        EntryLogicalKey::new(
            "premarket_v1",
            "15m",
            1_771_430_700,
            "token-abc",
            "BUY",
            Some(rung_id),
        )
    }

    fn scope() -> EntryScopeKey {
        EntryScopeKey::new("premarket_v1", 1_771_430_400, "1h", "ladder")
    }

    #[test]
    fn enqueue_dedupe_burst_forwards_once() {
        let mut dedupe = EnqueueDedupe::new(1_000);
        let key = key();

        let first = dedupe.should_forward(&key, 10_000);
        assert!(first.forwarded);

        for _ in 0..10 {
            let blocked = dedupe.should_forward(&key, 10_100);
            assert!(!blocked.forwarded);
            assert_eq!(blocked.reason, Some(EnqueueBlockReason::Cooldown));
        }

        let after = dedupe.should_forward(&key, 11_001);
        assert!(after.forwarded);
    }

    #[test]
    fn worker_idempotency_blocks_inflight_and_recent_done_duplicates() {
        let cfg = EntryIdempotencyConfig {
            enqueue_dedupe_cooldown_ms: 100,
            recent_done_ttl_ms: 500,
            recent_done_ttl_mm_rewards_ms: 500,
            failure_cooldown_ms: 250,
            retryable_failure_cooldown_ms: 100,
            parameter_invalid_cooldown_ms: 250,
            market_not_ready_cooldown_ms: 200,
            market_not_ready_jitter_ms: 0,
            retry_window_ms: 5_000,
            max_failures_per_window: 5,
            allowance_backoff_ms: 1_000,
            warning_cooldown_ms: 200,
        };
        let mut worker = WorkerIdempotency::new(cfg);
        let key = key();
        let scope = scope();

        let begin = worker.begin(&key, &scope, 1_000);
        assert!(begin.execute);

        let blocked_inflight = worker.begin(&key, &scope, 1_001);
        assert!(!blocked_inflight.execute);
        assert_eq!(blocked_inflight.reason, Some(WorkerDropReason::InFlight));

        worker.finish_success(&key, &scope, 1_100);

        let blocked_recent = worker.begin(&key, &scope, 1_200);
        assert!(!blocked_recent.execute);
        assert_eq!(blocked_recent.reason, Some(WorkerDropReason::RecentDone));

        let allowed_after_ttl = worker.begin(&key, &scope, 1_601);
        assert!(allowed_after_ttl.execute);
    }

    #[test]
    fn allowance_backoff_suppresses_immediate_retries() {
        let cfg = EntryIdempotencyConfig {
            enqueue_dedupe_cooldown_ms: 100,
            recent_done_ttl_ms: 500,
            recent_done_ttl_mm_rewards_ms: 500,
            failure_cooldown_ms: 250,
            retryable_failure_cooldown_ms: 100,
            parameter_invalid_cooldown_ms: 250,
            market_not_ready_cooldown_ms: 200,
            market_not_ready_jitter_ms: 0,
            retry_window_ms: 5_000,
            max_failures_per_window: 5,
            allowance_backoff_ms: 2_000,
            warning_cooldown_ms: 200,
        };
        let mut worker = WorkerIdempotency::new(cfg);
        let key = key();
        let scope = scope();

        assert!(worker.begin(&key, &scope, 1_000).execute);
        let until_ms =
            worker.finish_failure(&key, &scope, 1_010, ExecutionErrorClass::BalanceAllowance);
        assert_eq!(until_ms, 3_010);

        let blocked = worker.begin(&key, &scope, 1_500);
        assert!(!blocked.execute);
        assert_eq!(blocked.reason, Some(WorkerDropReason::Backoff));

        let still_blocked = worker.begin(&key, &scope, 3_009);
        assert!(!still_blocked.execute);

        let allowed = worker.begin(&key, &scope, 3_011);
        assert!(allowed.execute);
    }

    #[test]
    fn incident_style_reactive_burst_is_bounded() {
        let cfg = EntryIdempotencyConfig {
            enqueue_dedupe_cooldown_ms: 1_500,
            recent_done_ttl_ms: 10_000,
            recent_done_ttl_mm_rewards_ms: 10_000,
            failure_cooldown_ms: 8_000,
            retryable_failure_cooldown_ms: 3_000,
            parameter_invalid_cooldown_ms: 20_000,
            market_not_ready_cooldown_ms: 2_000,
            market_not_ready_jitter_ms: 0,
            retry_window_ms: 60_000,
            max_failures_per_window: 5,
            allowance_backoff_ms: 60_000,
            warning_cooldown_ms: 8_000,
        };

        let mut enqueue = EnqueueDedupe::new(cfg.enqueue_dedupe_cooldown_ms);
        let mut worker = WorkerIdempotency::new(cfg);
        let key = key();
        let scope = scope();

        let mut submit_attempts = 0usize;
        for i in 0..104 {
            let now_ms = 10_000 + (i as i64 * 10);
            let enqueue_decision = enqueue.should_forward(&key, now_ms);
            if !enqueue_decision.forwarded {
                continue;
            }

            let begin = worker.begin(&key, &scope, now_ms);
            if !begin.execute {
                continue;
            }
            submit_attempts += 1;

            worker.finish_failure(
                &key,
                &scope,
                now_ms + 1,
                ExecutionErrorClass::BalanceAllowance,
            );
        }

        assert!(submit_attempts <= 1, "submit_attempts={}", submit_attempts);
    }

    #[test]
    fn classify_entry_error_detects_balance_allowance_signature() {
        let class = classify_entry_error("not enough balance / allowance for BUY");
        assert_eq!(class, ExecutionErrorClass::BalanceAllowance);
    }

    #[test]
    fn classify_entry_error_detects_parameter_invalid_signatures() {
        let class = classify_entry_error("invalid_order_min_size: notional 0.42 below min 1.00");
        assert_eq!(class, ExecutionErrorClass::ParameterInvalid);
        let class =
            classify_entry_error("reject: minimum tick size is 0.01 but received non-aligned");
        assert_eq!(class, ExecutionErrorClass::ParameterInvalid);
    }

    #[test]
    fn classify_entry_error_detects_market_not_ready_signatures() {
        let class = classify_entry_error("market_not_ready: accepting_orders=false");
        assert_eq!(class, ExecutionErrorClass::MarketNotReady);
    }

    #[test]
    fn premarket_distinct_rungs_not_deduped() {
        let mut enqueue = EnqueueDedupe::new(1_500);
        let key_r0 = ladder_key("r0");
        let key_r1 = ladder_key("r1");

        let first = enqueue.should_forward(&key_r0, 10_000);
        let second = enqueue.should_forward(&key_r1, 10_001);
        assert!(first.forwarded);
        assert!(second.forwarded);

        let cfg = EntryIdempotencyConfig {
            enqueue_dedupe_cooldown_ms: 1_500,
            recent_done_ttl_ms: 10_000,
            recent_done_ttl_mm_rewards_ms: 10_000,
            failure_cooldown_ms: 8_000,
            retryable_failure_cooldown_ms: 3_000,
            parameter_invalid_cooldown_ms: 20_000,
            market_not_ready_cooldown_ms: 2_000,
            market_not_ready_jitter_ms: 0,
            retry_window_ms: 60_000,
            max_failures_per_window: 5,
            allowance_backoff_ms: 60_000,
            warning_cooldown_ms: 8_000,
        };
        let mut worker = WorkerIdempotency::new(cfg);
        let scope = EntryScopeKey::new("premarket_v1", 1_771_430_700, "15m", "ladder");
        assert!(worker.begin(&key_r0, &scope, 10_010).execute);
        assert!(worker.begin(&key_r1, &scope, 10_011).execute);
    }

    #[test]
    fn premarket_duplicate_same_rung_deduped() {
        let mut enqueue = EnqueueDedupe::new(1_500);
        let key_r0 = ladder_key("r0");
        let first = enqueue.should_forward(&key_r0, 20_000);
        let second = enqueue.should_forward(&key_r0, 20_001);
        assert!(first.forwarded);
        assert!(!second.forwarded);
        assert_eq!(second.reason, Some(EnqueueBlockReason::Cooldown));
    }

    #[test]
    fn failure_window_caps_retries_per_key() {
        let cfg = EntryIdempotencyConfig {
            enqueue_dedupe_cooldown_ms: 100,
            recent_done_ttl_ms: 10,
            recent_done_ttl_mm_rewards_ms: 10,
            failure_cooldown_ms: 10,
            retryable_failure_cooldown_ms: 10,
            parameter_invalid_cooldown_ms: 10,
            market_not_ready_cooldown_ms: 10,
            market_not_ready_jitter_ms: 0,
            retry_window_ms: 1_000,
            max_failures_per_window: 3,
            allowance_backoff_ms: 100,
            warning_cooldown_ms: 100,
        };
        let mut worker = WorkerIdempotency::new(cfg);
        let key = key();
        let scope = scope();

        assert!(worker.begin(&key, &scope, 1_000).execute);
        worker.finish_failure(&key, &scope, 1_001, ExecutionErrorClass::Retryable);

        assert!(worker.begin(&key, &scope, 1_020).execute);
        worker.finish_failure(&key, &scope, 1_021, ExecutionErrorClass::Retryable);

        assert!(worker.begin(&key, &scope, 1_040).execute);
        let until = worker.finish_failure(&key, &scope, 1_041, ExecutionErrorClass::Retryable);
        assert_eq!(until, 2_001);

        let blocked = worker.begin(&key, &scope, 1_500);
        assert!(!blocked.execute);
        assert_eq!(blocked.reason, Some(WorkerDropReason::Backoff));

        let allowed = worker.begin(&key, &scope, 2_002);
        assert!(allowed.execute);
    }
}
