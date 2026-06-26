# Endgame Full Rollout Plan

## Context For Future Agents

This is the final rollout plan for upgrading local OSS `endgame_sweep_v1` in:

- Local OSS repo: `C:/Users/Administrator/Desktop/OSS/main`
- SaaS reference repo: `C:/Users/Administrator/Desktop/SaaS/EVPOLY-platform-runtime-local-first`

The goal is not to port Book99 wholesale. The goal is to upgrade Endgame using only the parts that solve verified Endgame problems:

- quote/readiness failures near the hit tick,
- data and connection warmup missing before the tick,
- submit lifecycle ambiguity after enqueue,
- CEX-depth risk awareness for close-window tail risk.

Current verified local Endgame facts:

- Live default Endgame uses the fast-submit GTC path. `EVPOLY_ENDGAME_FAST_SUBMIT_ENABLE` defaults true in `src/main.rs`; the worker calls `execute_endgame_buy_fast`; `src/trader.rs` builds `order_type: "GTC"` with `expiration_ts: None`.
- FAK / LIMIT+expiration is fallback-only when fast-submit is disabled.
- Default Endgame tick offsets are `[2_000, 1_000, 100]` ms in `src/config.rs`.
- Endgame quote/readiness skips currently consume ticks too aggressively. Transient data misses can insert `processed_tick_slots` directly or fall into the `best_candidate == None` processed-slot funnel.
- The retryable skip set is: registry not prewarmed, constraints missing/stale, fee model unavailable, quote missing/stale, and missing/non-finite `poly_mid`.
- Endgame still fetches the CLOB fee model on the due-tick path with `get_clob_fee_model(...).await`.
- `EndgameSubmitOutcome` and `SubmitOutcomeGuard` already exist, but Endgame producers pass `endgame_submit_outcome: None`, so submit results are computed and discarded.
- Endgame commits tick and notional after enqueue success, before the worker proves the order was submitted.
- The fast path bypasses the worker-level submit semaphore, but still hits the API-level `order_submit_semaphore` in `post_signed_order_local`.
- The local repo has no CEX-depth code. A local grep for `cex_depth`, `external_depth`, and `cex` in `src/` returns no local CEX-depth subsystem.
- SaaS Book99 has the CEX-depth cache/evaluator/hub/history code we need in `runtime/src/book99/risk_guard.rs`.
- SaaS Book99 has quote-cache code we can borrow outside CEX-depth only for freshness/timing mechanics in `runtime/src/book99/quote_cache.rs`.

Do not port Book99 price modes, alpha logic, cent/subcent ladders, external-depth size-increase behavior, close-window caps, or Book99-specific edge assumptions into Endgame.

## What We Borrow From Book99

### Borrow From CEX-Depth

Borrow and adapt these from `runtime/src/book99/risk_guard.rs`:

- external-depth cache structure around `Book99RiskGuardCache`,
- `upsert_external_depth`,
- history load/downsample/compact/persist helpers,
- `spawn_book99_external_depth_hub`,
- venue routing and fetch target logic,
- `evaluate_external_depth`,
- `evaluate_external_depth_venue`,
- `cost_to_buy_up_to`,
- `cost_to_buy_up_bps_buckets`,
- sell-down equivalent cost buckets,
- `external_depth_reduce_adjustment_for_cost`,
- stale/missing fail-open behavior,
- adaptive hot polling structure, adapted to Endgame T-minus offsets.

Borrow `runtime/src/bin/book99_cex_depth_archive.rs` only as a history-file reference if in-process compact history is not enough.

### Borrow Outside CEX-Depth

Outside CEX-depth, borrow only quote-cache freshness/timing mechanics from `runtime/src/book99/quote_cache.rs`:

- `ws_updated_ms_for_valid_update` logic,
- the `Book99TokenUpdate` / `Book99TokenUpdateKind` shape if we need update event telemetry,
- `upsert_from_orderbook_with_timing_and_kind` / `upsert_fields_with_timing_and_kind` update-source and timing pattern,
- `memory_stats` pattern only if Endgame quote cache needs cache health metrics.

No other Book99 function is in scope outside CEX-depth.

## Rollout Objective

Endgame should enter every close tick with all required data already warm, should not burn ticks on transient readiness failures, should submit through a bounded low-latency lane, and should apply live CEX-depth size reduction using a calculation tuned to Endgame T-minus timing.

The full rollout is:

1. Baseline and instrumentation.
2. Quote/readiness semantics.
3. Prewarm everything needed before the tick.
4. Submit reliability semantics.
5. Bounded Endgame submit lane.
6. Live CEX-depth size-reduction guard adapted to Endgame T-minus offsets.
7. Scheduler isolation if measurements still show scheduler contention.
8. Local active test, then release to all branches.

## Success Criteria

Endgame is ready when all of these are true in local testing:

- No due tick is consumed solely because registry, constraints, fee model, quote, or `poly_mid` was temporarily unavailable.
- The decisive tick has no required network fetch for token metadata, market constraints, CLOB fee model, collateral/allowance readiness, or expected orderbook quote.
- Endgame submit accounting is based on worker outcome, not enqueue success.
- POST timeout maps to `Unknown`, not `Failed`; `Unknown` holds tick and budget until reconciliation or expiry.
- CEX-depth data is warm before each configured Endgame T-minus offset.
- CEX-depth applies live size reduction from the local test build onward.
- CEX-depth never increases Endgame size in this rollout.
- Missing/stale CEX-depth data fails open with a metric.
- Submit-path collateral balance, allowance, builder-fee/fee-reserve, CLOB auth, and client handle readiness are warm or explicitly proven unnecessary for the Endgame fast path.
- Local active testing shows Endgame order count is not suppressed by readiness bugs, while duplicate/ambiguous submit risk remains bounded.

## Phase 0 - Baseline And Instrumentation

Add the metrics first so every later phase has before/after evidence.

Instrument Endgame per candidate and per close tick:

- market id, token id, symbol, timeframe, close/open timestamp, tau_ms,
- configured tick offset, tick index, safety stop,
- readiness state for registry, constraints, fee model, quote, `poly_mid`,
- quote source, quote age, `ws_updated_ms`, `rest_updated_ms`,
- fee-model age and whether it was prewarmed,
- time spent in decision, enqueue, worker receive, signing, API semaphore wait, CLOB POST, and response parse,
- submit outcome: Submitted, Failed, Unknown, Deduped, Skipped,
- budget state: free, reserved, pending_unknown, released, committed,
- CEX-depth multiplier, trigger group, venue, cost bucket, tau band, stale/fail-open reason, latency, and RSS impact.

Keep the metric names strategy-specific, for example:

- `endgame_readiness_skip`
- `endgame_retryable_readiness_skip`
- `endgame_due_tick_ready`
- `endgame_fee_model_prewarmed`
- `endgame_submit_outcome`
- `endgame_submit_unknown_reconciled`
- `endgame_cex_depth_active`
- `endgame_cex_depth_fail_open`
- `endgame_cex_depth_rollback`

Acceptance:

- A 10 minute local run shows all metrics without panics or log spam.
- A 24 hour baseline can group Endgame misses by readiness cause, submit outcome, and CEX-depth state.

## Phase 1 - Fix Quote And Readiness Semantics First

This phase changes tick-consumption rules before touching more risk logic.

Implement an explicit Endgame readiness decision enum:

- `Ready`
- `RetryableMissingRegistry`
- `RetryableMissingConstraints`
- `RetryableMissingFeeModel`
- `RetryableMissingQuote`
- `RetryableMissingPolyMid`
- `TerminalInvalidFair`
- `TerminalInvalidBand`
- `TerminalInvalidPrice`
- `TerminalNoEdge`
- `TerminalRiskGate`

Rules:

- Retryable readiness failures must not insert `processed_tick_slots`.
- Terminal strategy decisions may consume the tick, but only after the reason is explicitly classified.
- Retryable failures must throttle logs and evaluation. They should retry when fresh data arrives or on the next safety-poll interval, without flooding every 100 ms.
- Retryable failures in the final tiny window should record a missed-readiness metric rather than blocking the loop with a fresh network fetch.

Borrow from Book99 quote cache only for freshness/timing:

- Add `ws_updated_ms` to `EndgameQuoteSnapshot`.
- Add REST update timestamps separately from WS timestamps.
- Preserve update source priority, but do not assume WS-first. Local cache currently lets higher-priority REST replace WS at equal timestamp.
- Add update timing fields equivalent to Book99's `upsert_recv_ms` and `upsert_done_ms` only if they are used for metrics or stale detection.

Add REST quote rescue:

- Allow one bounded, timeout-controlled REST rescue only on non-final ticks.
- Do not perform a fresh REST fetch at the final 100 ms slot.
- Rescue writes to the same quote cache and records source/age.
- If rescue times out, keep the tick retryable unless the final window is already gone.

Acceptance:

- Unit tests cover all five retryable branches and prove they do not consume `processed_tick_slots`.
- Unit tests cover terminal branches and prove they consume consistently.
- A local run shows retryable skip counts without log spam.
- Quote rescue improves readiness without adding final-slot HTTP stalls.

## Phase 2 - Prewarm Everything Needed Before The Tick

This phase removes required hot-path network calls.

Extend the Endgame registry/prewarm worker so active Endgame markets have these ready before the first due offset:

- token metadata,
- market constraints,
- CLOB fee model,
- submit-path collateral balance and allowance snapshot,
- builder-fee / fee-reserve readiness for the GTC BUY path,
- CLOB auth/client handle readiness,
- latest quote with source/age,
- CEX-depth snapshot,
- CLOB client connection warmth.

Fee model:

- Move `get_clob_fee_model(...).await` out of the decisive candidate path.
- Store a cached fee model or fee-model readiness flag in the Endgame market context.
- If fee model is missing/stale at due time, classify as `RetryableMissingFeeModel` instead of consuming the tick.

CLOB connection:

- Measure first POST timing before changing behavior.
- Add explicit keepalive/pool settings to the vendored CLOB reqwest client or keep the connection warm from the prewarm worker.
- Instrument what the code can actually observe: client-handle lookup time, API semaphore wait, signing/build time, POST elapsed time, and response parse time.
- If deeper connection reuse visibility is needed, add explicit SDK/client instrumentation; do not claim "used existing connection" without a measurable signal.

Prewarm schedule:

- Prewarm active markets at least once before the first Endgame offset.
- Refresh quote and CEX-depth faster in the close window.
- Never let prewarm work block the final submit path.

Acceptance:

- The due-tick path no longer awaits fee-model HTTP.
- Metrics show fee model ready before due tick for active Endgame markets.
- Metrics show collateral/allowance, fee-reserve, CLOB auth, and client handle readiness before due tick, or the implementation proves those checks are bypassed for this Endgame path.
- CLOB POST timing metrics show whether connection warmup improved first POST latency.
- Missing prewarm data produces retryable readiness metrics, not silent tick burn.

## Phase 3 - Submit Reliability Semantics

This phase fixes the correctness bug where enqueue success is treated like order success.

Use the existing local structures:

- `EndgameSubmitOutcome`
- `SubmitOutcomeGuard`
- EVSnipe's live oneshot pattern as the local template.

Required changes:

- Add `Unknown` to `EndgameSubmitOutcome`.
- At the Endgame enqueue site, create a `oneshot::channel::<EndgameSubmitOutcome>()`.
- Pass `Some(sender)` in `endgame_submit_outcome`.
- Await the receiver only when enqueue returns `Sent`.
- Move `processed_tick_slots` and `submitted_notional_by_period` commit from enqueue success to submit outcome handling.
- Commit tick and notional on `Submitted`.
- Do not consume tick or budget on `Failed`, `Deduped`, `Skipped`, or enqueue error.
- On `Unknown`, consume the tick and hold notional in a pending/unknown budget bucket until reconciliation or expiry.

Reconciliation:

- Persist an Unknown identity record with: local request id, strategy, condition id, token id, side, price, share size, notional, symbol, timeframe, market open/close timestamp, tick slot, submit timestamp, timeout timestamp, signed-order fallback order id when available, and signed-order hash or equivalent deterministic identity.
- Add a lightweight Endgame reconciliation loop that checks open orders and recent trades for Unknown requests.
- Lookup sources are, in order: exact order id / fallback order id when available, open-order snapshot for matching token/side/price/size, recent trade/fill history for matching token/side/size/time, and local pending-order DB rows if the submit path wrote one.
- If the order is found open or filled, convert pending_unknown to committed.
- If the order is absent, keep pending_unknown until at least three consecutive reconciliation checks agree across open orders and recent trades, then release after the hard expiry window and record `unknown_released`.
- Unknown records must persist across restart so a desktop restart cannot free budget for a possibly-live GTC order.
- Do not auto-retry an Unknown order before reconciliation.

Acceptance:

- Tests prove Submitted commits.
- Tests prove Failed does not consume tick or budget.
- Tests prove Unknown holds budget and blocks immediate retry.
- Tests prove Unknown reconciles to committed or released.
- No deadlock occurs for Deduped/Skipped/Err paths.

## Phase 4 - Bounded Endgame Submit Lane

This phase reduces contention without creating an unbounded bypass.

Current verified issue:

- Endgame fast path bypasses the worker-level submit semaphore, but still waits on the API-level `order_submit_semaphore`.

Implement:

- A dedicated bounded Endgame submit lane at the API submit gate.
- Thread an Endgame-only submit option from `execute_endgame_buy_fast` through `place_order_with_timing_cached_metadata_only`, `place_order_with_timing_with_metadata_policy`, `place_order_with_handle`, and into `post_signed_order_local`.
- Generic strategy submits and batch submits keep the existing global `order_submit_semaphore`.
- Separate concurrency from the generic order lane.
- Nonblocking enqueue for Endgame, using the EVSnipe fast-lane shape where appropriate.
- A submit timeout that maps to `Unknown`.
- Metrics for queue wait, semaphore wait, signing time, POST time, and outcome.

Rules:

- The lane must be bounded.
- It must not starve generic orders.
- It must not submit duplicates for the same market/tick.
- Timeout is never treated as `Failed`.
- If 429/rate-limit responses rise after the lane is enabled, automatically fall back to the global semaphore and log `endgame_submit_lane_rate_limit_rollback`.

Borrow from Book99 only as semantics:

- Use Reserved/Acked/Released style state transitions.
- Do not port Book99's strategy-specific close-window caps or alpha policy.

Acceptance:

- Tests prove the Endgame lane has a hard concurrency bound.
- Tests prove a timed-out POST creates Unknown.
- Tests prove duplicate same-market/tick submits are rejected or deduped.
- Local metrics show Endgame submit wait no longer tracks generic order contention.

## Phase 5 - Port Book99 CEX-Depth Risk Guard To Live Endgame

This is live from the local test build. It must be size-reduction-only, fail-open on missing/stale data, and have an env/UI rollback switch.

Source code to borrow from SaaS:

- `runtime/src/book99/risk_guard.rs`
  - external-depth cache around `Book99RiskGuardCache`
  - `upsert_external_depth`
  - history load/downsample/compact/persist helpers
  - `spawn_book99_external_depth_hub`
  - venue routing/fetch targets
  - `evaluate_external_depth`
  - `evaluate_external_depth_venue`
  - `cost_to_buy_up_to`
  - `cost_to_buy_up_bps_buckets`
  - sell-down bucket equivalent
  - `external_depth_reduce_adjustment_for_cost`
  - stale/missing fail-open behavior
- `runtime/src/bin/book99_cex_depth_archive.rs`
  - history archive shape as a reference if in-process compact history is not enough.

Do not port:

- Book99 price modes,
- Book99 alpha decisions,
- cent/subcent ladder logic,
- Book99 external-depth size-increase path,
- Book99 close-window submit caps.

Local Endgame adaptation:

- Create an Endgame-owned CEX-depth module, not a Book99 namespace.
- Feed it only symbols/timeframes Endgame can trade.
- Use the same venue/fetch logic where it maps cleanly to crypto symbols.
- Persist compact history so the guard starts warm after restart.
- Evaluate CEX-depth before final sizing.
- Apply live size reduction immediately in local testing.
- Missing/stale depth fails open with a metric.
- Active rollback switch must be available from env/UI settings.

T-minus-specific calculation:

- Use Endgame's actual `tau_ms = market_close_ms - now_ms` and configured tick offset. Do not copy Book99's broad tau bands blindly.
- Default evaluation bands must align to Endgame's default offsets: T-2000 ms, T-1000 ms, and T-100 ms.
- Polling must go hot before the earliest configured Endgame offset and stay hot through the safety stop.
- The CEX-depth snapshot used for a tick must be fresh relative to that tick. A snapshot that is acceptable at T-2000 may be stale at T-100.
- Cost-to-move direction must match the Endgame settlement risk:
  - if Endgame buys Up, use the CEX cost to move the underlying down toward/through the relevant result boundary;
  - if Endgame buys Down, use the CEX cost to move the underlying up toward/through the relevant result boundary.
- The distance bucket must use the current distance from CEX mid to the Endgame result boundary/base, rounded using the Book99 bucket ladder only after adapting it to the actual T-minus tick.
- Thresholds must be keyed by symbol, venue, side, bucket, and Endgame T-minus band.
- At T-100, use stricter freshness and stronger reduction because there is little time to recover from a thin CEX book.
- At T-2000 and T-1000, allow smaller reductions for the same trigger so the guard does not kill normal profitable entries too early.
- If boundary/base price is unavailable or non-finite, CEX-depth fails open and logs `endgame_cex_depth_boundary_missing`.

Sizing composition:

- Compute Endgame base size from the existing fair-probability/VWAP model.
- Compute CEX-depth adjustment.
- Use the reduced CEX-depth size when triggered.
- Apply existing max shares/notional caps after the CEX-depth adjustment.

Live guardrails:

- CEX-depth is size-reduction-only.
- Missing/stale data fails open.
- Venue errors and 429s trigger backoff.
- If CEX-depth causes Endgame quote/order-count collapse, 429/error storm, or RSS growth above threshold, rollback to disabled and log `endgame_cex_depth_rollback`.
- Keep telemetry for every reduced order: original size, final size, multiplier, venue, cost bucket, tau_ms, tick offset, and fail-open status.

Acceptance:

- Tests cover CEX-depth routing by symbol/timeframe.
- Tests cover stale/missing fail-open.
- Tests cover active mode reducing submitted size.
- Tests prove CEX-depth never increases size.
- Tests cover T-2000/T-1000/T-100 band selection and freshness requirements.
- Tests cover cost direction for Up and Down Endgame buys.
- Tests cover history load/downsample/compact.
- Tests cover size composition with existing Endgame caps.
- Local active report shows trigger frequency, submitted-size delta, latency, RSS, 429/error rate, and rollback status.

## Phase 6 - Scheduler Jitter Isolation

Do this after Phase 0 metrics prove whether jitter remains material. This is still part of the full rollout path; the gate is based on measurement, not indecision.

Trigger condition:

- Endgame due-tick scheduler jitter remains high after quote/readiness, prewarm, submit-lane, and live CEX-depth fixes.

Implement if triggered:

- Move the close-loop scheduler and final submit handoff onto a dedicated runtime or pinned worker.
- Keep WS ingest, DB, admin, and unrelated strategies off the final close-loop path.
- Maintain existing lifecycle/shutdown semantics.

Acceptance:

- P95/P99 due-tick scheduling delay improves in local test.
- No shutdown deadlock or orphan task.
- Other strategies do not regress.

## Phase 7 - Tests And Verification Gates

Run these gates after each implementation slice:

- `cargo fmt --all -- --check`
- `cargo check --all-targets`
- `cargo test --all-targets --quiet`
- `./scripts/security_audit.sh`

Required focused tests:

- extract small pure helpers so tests do not need to drive the full Endgame loop: readiness classification, submit/budget transition, Unknown reconciliation state, CEX-depth T-minus classification, CEX-depth size composition, CEX-depth mode behavior, and rollout-default parsing,
- retryable readiness skips do not consume `processed_tick_slots`,
- terminal skips consume consistently,
- fee model is prewarmed before due tick,
- quote rescue is non-final-tick only,
- submit outcome Submitted commits,
- submit outcome Failed releases,
- submit outcome Unknown holds pending budget,
- Unknown reconciliation commits or releases,
- dedicated submit lane is bounded,
- duplicate market/tick submit is blocked,
- CEX-depth stale/missing fails open,
- CEX-depth active mode reduces size only,
- CEX-depth history warm-start works,
- combined sizing uses CEX-depth reduction and existing caps.

Required local runtime verification:

- Run bot locally with live CEX-depth active.
- Capture at least one full Endgame close cycle per active timeframe.
- Confirm no log spam from retryable readiness.
- Confirm socket and RSS remain stable.
- Confirm Endgame quote count/order count do not collapse due to CEX-depth.
- Confirm CEX-depth active metrics are present.
- Confirm rollback switch disables CEX-depth without rebuild.

## Phase 8 - Local Desktop Rollout

Implementation order for local test builds:

1. Baseline metrics.
2. Quote/readiness semantics.
3. Fee model and quote prewarm.
4. CLOB connection measurement/warmth.
5. Submit outcome lifecycle with Unknown.
6. Dedicated bounded Endgame lane.
7. CEX-depth module live active with rollback.
8. Scheduler isolation if Phase 0/6 metrics say it is needed.

For each local build:

- Commit locally with clear scope.
- Rebuild and reinstall desktop.
- Start bot from UI.
- Verify socket, RSS, quote/order counts, Endgame metrics, CEX-depth reductions, and submit outcomes.
- Compare against the Phase 0 baseline before moving to the next slice.

## Phase 9 - Release Rollout

After local active testing passes:

1. Merge to `main` through PR.
2. Release new runtime version on `main`.
3. Port/rebase to `desktop`.
4. Release desktop tag so the Windows update banner appears.
5. Port/rebase to `Linux`.
6. Release Linux tag so Linux users see the update.
7. Watch GitHub Actions for all release lines.
8. Install from banner locally and verify the installed version uses the new Endgame metrics and behavior.

Rollout defaults:

- `EVPOLY_ENDGAME_QUOTE_RESCUE_ENABLE=true`
- `EVPOLY_ENDGAME_QUOTE_RESCUE_TIMEOUT_MS=120`
- `EVPOLY_ENDGAME_FEE_PREWARM_ENABLE=true`
- `EVPOLY_ENDGAME_SUBMIT_COLLATERAL_PREWARM_ENABLE=true`
- `EVPOLY_ENDGAME_CLOB_KEEPALIVE_ENABLE=true`
- `EVPOLY_ENDGAME_SUBMIT_LANE_ENABLE=true`
- `EVPOLY_ENDGAME_SUBMIT_LANE_PERMITS=1`
- `EVPOLY_ENDGAME_SUBMIT_TIMEOUT_MS=1500`
- `EVPOLY_ENDGAME_UNKNOWN_RECONCILE_SEC=30`
- `EVPOLY_ENDGAME_UNKNOWN_HARD_EXPIRY_SEC=300`
- `EVPOLY_ENDGAME_CEX_DEPTH_ENABLE=true`
- `EVPOLY_ENDGAME_CEX_DEPTH_SIZE_INCREASE_ENABLE=false`
- `EVPOLY_ENDGAME_CEX_DEPTH_MAX_REDUCTION_PCT=50`
- `EVPOLY_ENDGAME_CEX_DEPTH_TICK_BANDS_MS=2000,1000,100`
- `EVPOLY_ENDGAME_CEX_DEPTH_T100_MAX_AGE_MS=250`
- `EVPOLY_ENDGAME_CEX_DEPTH_T1000_MAX_AGE_MS=500`
- `EVPOLY_ENDGAME_CEX_DEPTH_T2000_MAX_AGE_MS=750`

Required release hygiene:

- Update `.env.example` and `.env.full.example` for every new setting.
- Update user-visible docs for live CEX-depth mode and rollback.
- Update `strategy-changelog.md` because this changes strategy behavior, sizing, submit semantics, and defaults.
- Keep CEX-depth size-reduction-only for the first user release.

## Final Definition Of Done

The rollout is done only when:

- the final plan above is implemented in slices,
- all local gates pass,
- local desktop test confirms Endgame no longer burns ticks on retryable readiness misses,
- submit outcome metrics show no enqueue-success false commits,
- CEX-depth active metrics and size reductions are visible,
- CEX-depth calculation is keyed to Endgame T-minus tick bands,
- rollback switch works without rebuild,
- no socket/RSS degradation is observed,
- release builds for `main`, `desktop`, and `Linux` are green,
- update banner installs the release and local post-install behavior matches the test build.
