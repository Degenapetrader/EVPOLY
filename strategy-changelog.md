# Strategy Changelog

This file documents EVPoly strategy architecture, decision flow, and change history.
When strategy logic/config is changed, add a new dated entry under `## Change Log`.

Current operator-facing behavior is summarized in `README.md` and `docs/*.md`.
This file is append-heavy history plus high-level architecture notes.
Older entries may reference env keys that were removed in later commits.

## Change Log

### 2026-04-26
- `evcurve_v1`: aligned EVcurve decision thresholds with UPDATE/main_v2 (`src/evcurve.rs`, `.env.full.example`, `docs/evcurve_v1.md`).
  - max `p_flip` is now fixed in code at `0.11`, and `EVPOLY_EVCURVE_MAX_FLIP_PROB` env overrides are ignored.
  - D1 EV-gap threshold is now fixed in code at `0.12`, and `EVPOLY_EVCURVE_D1_EV_GAP` env overrides are ignored.
  - alpha owns the EVcurve decision path, so alpha service binaries must be rebuilt/deployed for this threshold change to take effect remotely.
- `evcurve_v1`: clarified and cleaned the remote-alpha-only decision path (`src/main.rs`, `.env.example`, `.env.full.example`, `docs/evcurve_v1.md`).
  - runtime no longer carries placeholder local Plan3/PlanDaily table handles through EVcurve decision calls; non-D1 and D1 candidates are explicitly fetched from `/v1/alpha/evcurve`.
  - alpha owns Plan3/PlanDaily lookup, `p_flip`, hold-side selection, D1 sub-strategy selection, and max-buy decisioning; checkpoint scheduling, base anchoring, market discovery, sizing/caps, and chase/FAK execution remain local.
  - env templates now expose `EVPOLY_REMOTE_EVCURVE_ALPHA_URL` and `EVPOLY_REMOTE_EVCURVE_ALPHA_TOKEN` with blank-token fallback to `EVPOLY_ALPHA_KEY`.
- `endgame_sweep_v1`: moved the active `t0/t1/t2` checkpoint policy fully to remote alpha timing (`src/main.rs`, `src/config.rs`, `src/bin/alpha_service.rs`, `.env.example`, `.env.full.example`, `docs/endgame_sweep_v1.md`).
  - runtime now requests `/v1/alpha/endgame/policy` at `T-3m` before current period close / next period open; market discovery, proxy feeds, direction logic, sizing, caps, and order placement remain local.
  - alpha defaults to base offsets `2000,1000,100` ms and randomizes each returned offset up to `25%` closer to `T` only, never farther from close.
  - local `EVPOLY_ENDGAME_TICK_OFFSETS_MS` is now a label/capacity default of `2000,1000,100`, and `EVPOLY_ENDGAME_SAFETY_STOP_SEC` now defaults to `0` so the `t-100ms` alpha tick can fire.
- `premarket_v1`: replaced the remote `should_trade` alpha gate with remote alpha ladder selection (`src/main.rs`, `src/bin/alpha_service.rs`, `.env.example`, `.env.full.example`, `docs/premarket_v1.md`).
  - scheduler, market discovery, sizing, caps, cancel scheduling, and order placement remain local.
  - at the scheduled pre-open intent (`T-4m` path), runtime sends base ladder prices to `/v1/alpha/premarket/ladder`.
  - alpha returns one aligned random price shift across all rungs, bounded to about `+/-10%`; unavailable or invalid alpha ladder now fail-closed skips that asset intent.
- `premarket_v1`, `endgame_sweep_v1`, `evcurve_v1`, `sessionband_v1`, `evsnipe_v1`, `mm_rewards_v1`, `mm_sport_v1`: added self-serve EVPOLY alpha access and official builder-attribution gating (`src/main.rs`, `src/api.rs`, `src/bin/alpha_service.rs`, `src/builder_attribution.rs`, `.env.example`, `.env.full.example`).
  - official `POLY_BUILDER_CODE` now has a compiled fallback for normal CLOB V2 order posting.
  - runtime auto-registers `EVPOLY_ALPHA_KEY` from wallet + official builder code when the key is blank.
  - remote alpha/discovery requests include builder attribution and can use `EVPOLY_ALPHA_KEY` when per-endpoint alpha tokens are blank.
  - alpha server accepts backwards-compatible shared tokens while enforcing official builder code for auto-generated alpha keys.
- `premarket_v1`, `mm_sport_v1`, and all strategies writing `pending_orders`: strengthened terminal pending-order retention so local desktop/UI SQLite scans do not accumulate millions of closed rows (`src/main.rs`, `src/tracking_db.rs`, `.env.example`, `.env.full.example`).
  - active rows (`OPEN`, `PENDING`, `PLACED`, `LIVE`) remain preserved.
  - terminal rows default to 60-minute retention, 50k-row chunks, and a bounded startup burst via `EVPOLY_PENDING_ORDER_PRUNE_*` env keys.

### 2026-04-05
- `mm_sport_v1`: passive quote depth now follows the real two-level passive band instead of top-of-book only.
  - BUY depth/ratio checks now aggregate visible bid depth from `best bid` through the passive quote price (`best bid - 1 tick`).
  - passive SELL exits now record the same cumulative ask-side context from `best ask` through the passive exit price (`best ask + 1 tick`).
  - hard thin-depth safety floor lowered from `$2,000` to `$1,000`.
  - default configurable `EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD` lowered from `$100,000` to `$10,000` in env examples.

### 2026-04-01
- `premarket_v1`: overlapping deterministic schedule slots now use timeframe priority instead of additive fanout.
  - Priority order: `4h > 1h > 15m > 5m`
  - This prevents stacked premarket runs on overlap minutes such as `:11` and `:56`.
- `1h` discovery: accept both year-bearing and legacy no-year ET event slug formats.
  - Discovery/mismatch guards now recognize:
    - `ethereum-up-or-down-february-20-2026-3am-et`
    - `ethereum-up-or-down-february-20-3am-et`
  - This prevents false 1h discovery rejects when Polymarket serves either slug style.

## Common Architecture

- Market discovery: strategy loops resolve active Polymarket markets by `symbol + timeframe + period_open_ts`.
- Proxy feeds:
  - Coinbase mid for `5m/15m/4h`
  - Binance trade/spot for `1h` (and EVcurve D1 internals where applicable)
- Base anchoring: each strategy stores/reloads period base price in `tracking.db` for restart safety.
- Decision -> Arbiter -> Trader:
  - Strategy emits `ArbiterExecutionRequest`
  - Arbiter applies dedupe / caps / invariants
  - Trader submits `LIMIT` / `FAK` (strategy-specific)
- Tracking:
  - Events in `trade_events`
  - Position lifecycle in `positions_v2`
  - Settlement in `settlements_v2`

## Strategy Flows

### `premarket_v1`
- Places pre-open ladder orders per timeframe/rung schedule.
- Uses configured price ladder + weight ladder + expiry/cancel schedules.
- Uses remote premarket alpha gate with local 50/50 fallback on transport-class alpha failures.
- Includes delayed TP worker for `15m/1h/4h` fills.

### `endgame_sweep_v1`
- Late-window sweep strategy near close with alpha-owned tick policy (`tick_offsets_ms`, submit stale guard).
- Uses proxy-vs-base direction + guard stack + FAK/limit routing as applicable.
- Holds to settlement (`no sell ladder`).

### `evcurve_v1`
- Uses plan3/plan-daily stats (`p_flip`) vs market ask.
- Core gate: `p_flip` cap, min samples, curve-derived `max_buy`, min-buy floor.
- Supports `15m/1h/4h/1d` (`5m` removed from EVcurve runtime path).
- 1d uses dedicated zero-flip and EV-gap sub-rules.

### `sessionband_v1`
- Uses plan4b session watch-starts + lead% price bands.
- Uses two late checkpoints (`tau=2`, `tau=1`) with remote alpha decisioning.
- Single-attempt-per-period option + fixed symbol sizing.

### `evsnipe_v1`
- Binance price-rule strategy for hit markets (`High >= strike` / `Low <= strike`) only.
- Uses Binance trade stream for hit triggers.
- Discovery is remote-first with local fallback.
- Executes fast FAK cross-spread buy on the winning token side.

### `mm_rewards_v1`
- Liquidity-rewards market-making strategy with four modes:
  - `quiet_stack`
  - `tail_biased`
  - `spike`
  - `sports_pregame`
- Uses local market discovery/ranking and remote alpha selection.
- Uses local preflight gate before quote submission.
- Uses quote refresh + scoped cancel/replace + refill-on-fill loop through arbiter/trader.

### `mm_sport_v1`
- Sports rewards market-making strategy (default disabled).
- Discovers pregame sports match markets from rewards API + CLOB market details.
- Quotes top-of-book BUY on both outcomes (maker-only, post-only), no ladder.
- Uses reward min-size floor with multiplier and a top-share ratio gate before quote placement.
- Uses literal SELL only for inventory exit (prestart window or post-fill inventory unwind mode).
- Pauses a market for 120 minutes after fills, then runs inventory unwind before resuming two-sided BUY quoting.

## Change Log

### 2026-03-30

- `evsnipe_v1` internal strategy-cap tuning was removed from operator-facing config (`src/evsnipe.rs`, `src/main.rs`, `src/bot_admin.rs`, `.env.full.example`, `docs/evsnipe_v1.md`):
  - runtime now hardcodes the in-loop EVSnipe cap to a large internal backstop (`1_000_000_000_000`) and ignores any stale `EVPOLY_EVSNIPE_STRATEGY_CAP_USD` env override.
  - bot-admin/settings and docs/templates no longer expose `EVPOLY_EVSNIPE_STRATEGY_CAP_USD` as a supported knob.
  - EVSnipe startup/submit telemetry no longer advertises a user-tunable strategy-cap field.

### 2026-03-24

- Premarket/non-MM minimum-size policy cleanup (`src/main.rs`, `src/trader.rs`, `docs/premarket_v1.md`):
  - `premarket_v1` now uses a flat `$5` minimum per rung/order and no longer applies reward `min_size` as a floor.
  - submit-time reward-share floor stripping now applies to `premarket_v1`, `endgame_sweep_v1`, `evcurve_v1`, `sessionband_v1`, and `evsnipe_v1`.
  - reward `min_size` remains active only for `mm_rewards_v1` and `mm_sport_v1`.

- `evsnipe_v1` hit trigger now submits immediately from the Binance signal without waiting on a PM orderbook precheck (`src/main.rs`):
  - removed blocking `get_orderbook + select_cross_ask_price` gating in the hit path.
  - hit submit telemetry now records `book_precheck_bypassed=true` with neutral ask hints.
  - effect: EVSnipe hit legs no longer lose time waiting on a PM book fetch/shape check before enqueue.

- `evsnipe_v1` env/docs now explicitly describe hit-only behavior (`.env.full.example`, `docs/evsnipe_v1.md`, `strategy-changelog.md`):
  - EVSnipe is documented as hit-market-only.
  - close/range triggers remain disabled in runtime behavior.

### 2026-03-22

- `mm_sport_v1` prestart/exit execution hardening for live inventory unwind (`src/main.rs`, `src/mm/mod.rs`, `.env.example`, `.env.full.example`):
  - near-start force-exit window changed from `T-5m` to `T-30m` (`MM_SPORT_FORCE_EXIT_WINDOW_MS` now `30 * 60 * 1000`), while keeping the broader prestart exit mode behavior.
  - force-exit SELL orders now use normal crossing `LIMIT` (`GTD`) with `post_only=false` so they can take bid liquidity immediately and leave remainder resting on book; non-force MM Sport quoting remains maker-style post-only.
  - MM Sport terminal fill persistence now records both sides:
    - BUY terminal reconciles keep writing `ENTRY_FILL` (`entry_fill_order:<order_id>`),
    - SELL terminal reconciles now also write `EXIT` (`exit_fill_order:<order_id>`),
    enabling complete MM Sport entry/exit fill audit trail in `trade_events`.
  - added immediate inventory fallback market hydration for conditions with live MM Sport inventory that are missing from current discovery set, reducing prestart gaps where no exit order is live due transient discovery misses.
  - default MM Sport randomized quote expiry range changed to `180s..300s`:
    - `EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC=180`
    - `EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC=300`
  - affected scope: `mm_sport_v1` across discovered sports match markets (pregame inventory exit path and BUY quote rotation), all symbols/timeframes used by MM Sport.

- Order signer timeout tuning for live order submit path (`src/api.rs`):
  - changed primary signer timeout threshold from `500ms` to `1000ms` (`ORDER_SIGNER_PRIMARY_FALLBACK_TIMEOUT_MS`).
  - this affects order-submit behavior for all strategies that place CLOB orders (`premarket_v1`, `endgame_sweep_v1`, `evcurve_v1`, `sessionband_v1`, `evsnipe_v1`, `mm_rewards_v1`, `mm_sport_v1`) across all symbols/timeframes.
  - key path touched: primary signer timeout/fallback switch in builder post flow (`builder_client_primary.post_order` timeout and AWS fallback handoff).

### 2026-03-21

- MM rewards inventory-scope attribution guard hardening (`src/tracking_db.rs`, runtime impact in MM rewards janitor path in `src/main.rs`):
  - `list_mm_wallet_inventory_by_strategy` now returns wallet rows only when the same `(condition_id, token_id)` has strategy-attributed `OPEN` inventory in `positions_v2`.
  - MM drift reconcile now skips wallet-only rows that have no strategy open attribution (`db_open_positions == 0` and zero DB inventory), preventing synthetic cross-strategy ownership adds.
  - this prevents `mm_rewards_v1` from inheriting inventory owned by other strategies (e.g., `evsnipe_v1`) on overlapping conditions and reduces false `scope_violation_cross_strategy` blocks.
  - added regression test: `mm_wallet_inventory_and_reconcile_require_strategy_open_scope`.

- MM inventory-drift reconcile scope guard hotfix (`src/tracking_db.rs`, runtime impact via `src/main.rs` MM Sport reconcile trigger):
  - `reconcile_mm_inventory_drift_from_wallet_tables` now enriches/reconciles only strategy-scoped rows seeded by that strategy’s own MM state/position records.
  - wallet/mid/activity snapshots no longer create new reconcile candidates for unrelated wallet tokens/markets.
  - prevents cross-strategy ownership contamination (notably `mm_sport_v1` claiming non-sport/MM-rewards markets via `ENTRY_FILL_RECONCILED` drift-add events).
  - affects MM inventory reconcile behavior for `mm_sport_v1` and `mm_rewards_v1` across all symbols/timeframes by keeping ownership attribution strategy-scoped.
  - added regression test: `mm_inventory_reconcile_skips_wallet_rows_outside_strategy_scope`.

- `mm_sport_v1` depth-ratio pair-atomic funding update (`src/main.rs`):
  - changed depth-ratio USDC cap multiplier from `50%` to `40%` (`MM_SPORT_DEPTH_RATIO_USDC_CAP_MULT=0.4`).
  - added pair-atomic funding check for `EVPOLY_MM_SPORT_QUOTE_SIZE_MODE=depth_ratio`:
    - computes both-side planned BUY notionals before any side placement,
    - if pair notional exceeds depth-ratio cap budget, scales both sides down by one shared factor,
    - keeps pair submission all-or-nothing at planning stage (if scaled result fails pair min-size checks, no side is planned/placed).
  - this update is per-market/pair only; no cross-market global budget bucket is used.
  - new telemetry: `mm_sport_depth_ratio_pair_budget_scaled`.

- `mm_sport_v1` urgent depth-ratio BUY-cap fallback hotfix (`src/main.rs`):
  - fixed depth-ratio cap behavior so `allowance_usd <= 0` no longer hard-zeroes BUY quoting when `balance_usd > 0`; effective available USDC now falls back to balance in this specific stale/invalid allowance case, while preserving the existing 50% cap.
  - added throttled fallback telemetry: `mm_sport_depth_ratio_allowance_zero_fallback` (`strategy_id`, `balance_usd`, `allowance_usd`, `chosen_available_usdc`).
  - added explicit zero-size skip telemetry for depth-ratio BUY path: `mm_sport_depth_ratio_zero_bid_skip` before skip/continue.
  - affects `mm_sport_v1` only, in `EVPOLY_MM_SPORT_QUOTE_SIZE_MODE=depth_ratio`, across all active sports conditions/timeframes/symbols discovered by MM Sport; `mm_rewards_v1` logic unchanged.

- Shared symbol ownership + discovery/log hardening sweep (`src/symbol_ownership.rs`, `src/main.rs`, `src/evcurve.rs`, `src/sessionband.rs`, `src/polymarket_ws.rs`, `src/api.rs`, `src/trader.rs`, `.env.example`, `.env.full.example`):
  - symbol ownership is now centralized: `DOGE/BNB/HYPE` are restricted to `endgame_sweep_v1` and `evsnipe_v1`; `evcurve_v1` and `sessionband_v1` bootstraps now filter those symbols out even if provided via env.
  - Coinbase routing hard rule enforced: `coinbase_product_id_for_market_symbol("DOGE")` now returns no mapping; no `DOGE-USD` Coinbase feed spawn path remains.
  - EVcurve default symbol universe changed to `BTC,ETH,SOL,XRP` (`EVPOLY_EVCURVE_SYMBOLS` default updated accordingly).
  - Polymarket WS market discovery defaults tightened to reduce crash pressure:
    - `EVPOLY_PM_WS_MARKET_DISCOVERY_LIMIT` default `250` (from `1500`)
    - `EVPOLY_PM_WS_REFRESH_SEC` default `90` (from `30`)
  - discovery now prefers strategy scope targets, reuses last-good cache on failures, and applies discovery-specific backoff (`90s -> 180s -> 300s` capped).
  - active-market JSON handling is now bounded + typed in API discovery path (`get_all_active_markets_page_tagged`) to fail cleanly on malformed/oversized payloads.
  - arbiter/trader/MM logging pressure reductions:
    - `arbiter_dispatch_rejected` dedupe reason is aggregated to `arbiter_dispatch_rejected_dedupe_summary` (30s window),
    - batch success spam replaced by `batch_place_submit_summary` (30s window),
    - `entry_ack_empty_order_id` now emits immediate first-seen + 60s summaries (`entry_ack_empty_order_id_summary`) per `(strategy_id, token_id, response_status, signer_route)`,
    - `entry_execution_timing` now defaults to selective emit unless `EVPOLY_TRACE_ENTRY_EXECUTION_TIMING=true`.
  - MM rewards hot-loop telemetry is now change/transition based:
    - `mm_rewards_quote_plan` (fingerprint change or 60s heartbeat),
    - `mm_rewards_quote_skew_applied` (cooldown + material-change gate),
    - `mm_rewards_safe_buffer_applied` (cooldown + effective-change gate),
    - `mm_rewards_floor_size_upscaled` (aggregated per `(condition_id, token_id, side)` per loop),
    - `mm_rewards_inventory_janitor_snapshot` (count-change or 300s heartbeat),
    - `mm_rewards_state_action_mismatch` (transition or 30s heartbeat).
  - Polymarket WS unstable-feed logs moved to degraded/recovered + heartbeat model (market and user loops), replacing per-loop reconnect/error floods.

- `mm_sport_v1` depth-ratio quote sizing now hard-clamps each BUY quote to 50% of live available USDC notional (`src/main.rs`):
  - applies only when `EVPOLY_MM_SPORT_QUOTE_SIZE_MODE=depth_ratio`.
  - available notional is computed from live `min(USDC balance, USDC allowance)` (cached refresh in MM sport loop).
  - per-side target shares are clamped before ratio/min-size/place checks so oversized depth-ratio quotes cannot exceed half of available USDC.
  - adds throttled telemetry on balance-check failures: `mm_sport_depth_ratio_balance_check_failed`.

- `mm_sport_v1` added a second quote sizing mode `depth_ratio` (as an alternative to multiplier sizing) with asymmetric per-side depth-based BUY sizing (`src/mm/mod.rs`, `src/main.rs`, `.env.example`, `.env.full.example`):
  - new mode key: `EVPOLY_MM_SPORT_QUOTE_SIZE_MODE` with values:
    - `multiple` (existing behavior)
    - `depth_ratio` (new behavior)
  - `depth_ratio` sizing logic:
    - computes quote size per side from each side’s own external top-bid depth (no forced equal side size),
    - uses base quote ratio = `50%` of `max_share_ratio` at/below `100k` top-bid shares,
    - for depth above `100k`, increases on a capped log curve up to `75%` of `max_share_ratio`,
    - applies a floor clamp to the default multiplier floor (`1.2x reward min size`) in this mode.
  - `EVPOLY_MM_SPORT_MAX_SHARE_RATIO` runtime default changed from `0.02` to `0.05`.
  - existing ratio watchdog/pause/cancel guards remain the hard cap enforcement path; this change affects quote sizing input, not the max-ratio safety gate itself.

### 2026-03-20

- `mm_rewards_v1` dust cleanup now runs automatically on the hourly redemption scheduler slot (default minute `22`) with expanded run telemetry (`src/trader.rs`):
  - dust cleanup is now piggybacked on redemption `scheduled_interval` passes (same hourly slot) instead of being manual-only.
  - runtime now emits scheduled sweep events:
    - `mm_dust_sell_scheduled_completed`
    - `mm_dust_sell_scheduled_failed`
  - dust cleanup now emits richer per-run summary telemetry:
    - `mm_dust_sell_summary` includes counts, reason buckets, and sampled row outcomes for robust dust-position auditability.
  - no new env knobs were introduced for dust scheduling in this change; schedule timing follows existing redemption slot configuration.

- Core startup market discovery is now skipped in MM-only runtime mode (`src/main.rs`, `src/market_discovery.rs`, `src/monitor.rs`):
  - when all core strategies are disabled (`premarket_v1`, `endgame_sweep_v1`, `evcurve_v1`, `sessionband_v1`, `evsnipe_v1`) and at least one MM strategy is enabled (`mm_rewards_v1` and/or `mm_sport_v1`), bot now skips startup BTC/ETH/SOL/XRP discovery and logs `core_discovery_skipped_mm_only`.
  - MM-only mode now initializes monitor markets with explicit fallback placeholders (`dummy_*_fallback`, including BTC) and skips the legacy 15-minute core period-discovery worker.
  - startup/current-market metadata prewarm loops are also skipped in MM-only mode so MM runtimes do not keep emitting core crypto discovery noise.
  - affects MM-only deployments where operator intentionally runs MM strategies without core crypto strategies.

- `mm_sport_v1` quote GTD expiry defaults and per-market sampling were updated for pregame sports MM quoting (`src/main.rs`, `src/mm/mod.rs`, `.env.example`, `.env.full.example`):
  - default randomized quote expiry window changed from `65s..125s` to `300s..600s`:
    - `EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC=300`
    - `EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC=600`
  - quote expiry randomization is now condition-scoped (market-scoped), so paired outcomes in the same market share the same sampled TTL window per quote cycle.
  - affects `mm_sport_v1` pregame sports markets across all discovered symbols/conditions.

- Tick-metadata rate-limit handling was hardened across order submit + prewarm loops (`src/api.rs`, `src/main.rs`):
  - API prewarm now checks cache before backoff gate and marks tick-metadata backoff on prewarm-side rate limits (`tick_size` / `fee_rate` / `neg_risk`) instead of repeatedly falling through.
  - tick-metadata backoff now also has a short global guard (in addition to per-token) so cross-token 429 bursts pause metadata fetches platform-wide briefly instead of immediately thrashing new token ids.
  - main order submit now fails fast when prewarm is explicitly blocked by tick-metadata backoff/rate-limit, avoiding immediate on-demand metadata retries.
  - runtime loops now parse `remaining_ms=` from metadata errors and defer retries accordingly:
    - `mm_sport_v1` BUY quote submit and inventory-exit SELL submit paths apply per-token-side backoff (`mm_sport_buy_backoff_tick_metadata`, `mm_sport_inventory_exit_backoff_tick_metadata`);
    - `mm_rewards_v1` selected-token prewarm retry and `evsnipe_v1` selected-token prewarm retry now use parsed remaining backoff windows instead of fixed short retries.
  - `mm_sport_v1` inventory-exit SELL failure handling now applies explicit capacity backoff (`mm_sport_inventory_exit_backoff_balance_allowance`) and periodically triggers local wallet-table drift reconcile (`mm_sport_inventory_reconcile_triggered`) on repeated `not enough balance / allowance` errors to clear stale strategy inventory attribution.
  - `mm_sport_v1` now runs an orphan-condition cancel sweep (`mm_sport_orphan_condition_cancel_sweep`): any MM Sport `OPEN` pending rows whose condition drops out of active discovery/fallback scope are explicitly canceled with retry cooldown, preventing lingering past-expiration duplicate quotes on the exchange UI.
  - MM Sport also rearms aged `RECONCILE_FAILED` rows with direct cancel-first handling (`mm_sport_reconcile_failed_rearmed`): bot now attempts exchange cancel by order-id, marks `CANCELED` on confirmation, and only falls back to `OPEN` rearm when cancel is unconfirmed.
  - affects MM Sport pregame quoting/exits, MM Rewards selected-market prewarm cycles (all enabled modes/timeframes), and EVSnipe refresh prewarm on discovered symbols.

- MM inventory drift reconcile now supports both `mm_rewards_v1` and `mm_sport_v1` via admin + wallet-sync path (`src/trader.rs`, `src/main.rs`, `scripts/wallet_history_sync.py`):
  - admin endpoint `POST /admin/mm/reconcile/inventory` now accepts `strategy_id` (`mm_rewards_v1` default; `mm_sport_v1` supported) and reconciles per strategy against `wallet_positions_live_latest_v1`.
  - wallet sync worker now calls reconcile for both MM strategies each run and aggregates repaired shares/status so strategy inventory attribution is repaired from live wallet snapshots.
  - affects MM inventory tracking/repair across all MM rewards and MM sport markets (all symbols/timeframes where those strategies hold inventory).

- `mm_sport_v1` now applies a simple WS bust-flow pause and randomized quote expiry cycle for pregame sports markets (`src/main.rs`, `src/mm/mod.rs`, `src/polymarket_ws.rs`, `.env.example`, `.env.full.example`):
  - market WS bridge now consumes public `last_trade_price` events and keeps a short per-token market-trade window used by MM Sport bust checks.
  - MM Sport bust trigger is now explicit (`bust_shares_1s >= 10000` by default over a `1s` window): if triggered at/under bid guard price, bot cancels MM Sport condition quotes and pauses quoting for a random `60s..300s`, then resumes normal ratio/depth re-evaluation.
  - MM Sport quote submission is now always post-only `GTD` with randomized expiration `65s..125s`; live orders are tracked with expiry and replaced after expiry via normal cancel/requote flow.
  - new env knobs:
    - `EVPOLY_MM_SPORT_BUST_WINDOW_MS`
    - `EVPOLY_MM_SPORT_BUST_SHARES_1S`
    - `EVPOLY_MM_SPORT_BUST_PAUSE_MIN_SEC`
    - `EVPOLY_MM_SPORT_BUST_PAUSE_MAX_SEC`
    - `EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC`
    - `EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC`

- `mm_sport_v1` terminal fill persistence is now written atomically during MM Sport-owned reconciliation paths (`src/main.rs`):
  - when MM Sport sees terminal BUY fills from WS status updates or cancel/get-order reconciliation, it now writes canonical `ENTRY_FILL` (`entry_fill_order:<order_id>`) via `mark_pending_order_filled_with_event` instead of only flipping `pending_orders.status`.
  - canceled-with-partial-match BUY orders now persist matched units/notional into `trade_events`/`fills_v2`/`positions_v2` through the same atomic fill transition path.
  - prevents wallet-live MM Sport inventory from drifting out of strategy attribution after terminal reconciliations, so inventory exit logic can see real MM Sport exposure.

- `mm_sport_v1` cancel-state reconciliation was hardened to prevent exchange/live vs local/canceled drift (`src/main.rs`, `src/api.rs`):
  - MM Sport cancel path now requires cancel confirmation from CLOB response (`canceled` contains the specific `order_id`) before writing local `CANCELED`.
  - when cancel is unconfirmed, MM Sport now falls back to `get_order` terminal reconciliation (`FILLED`/`CANCELED`/`STALE`) and keeps rows active if exchange still reports live.
  - shared API single-order cancel transport now routes through CLOB batch-cancel endpoint (`cancel_orders([order_id])`) to avoid ambiguous single-cancel responses that could return empty-ID `not_canceled` payloads.
  - affects MM Sport quote/cancel/requote behavior across sports markets by preventing false local cancels that lead to duplicate live quotes.

- `mm_sport_v1` requote duplicate-order guard + pending-write fail-safe (`src/main.rs`):
  - normal BUY requote and inventory-exit SELL requote now block repost when cancel attempts are unresolved and the old order still remains active in `pending_orders` (new telemetry: `mm_sport_requote_blocked_unresolved_cancel`).
  - MM Sport order placement now fails closed if `pending_orders` upsert fails: runtime immediately attempts exchange cancel for the just-posted order and returns an error instead of continuing with untracked live exposure.
  - affects `mm_sport_v1` live quoting/exit paths across discovered sports markets by preventing same-side duplicate reposts under cancel/DB race conditions.

- `mm_rewards_v1` restart safety + auto-selection resilience + config-surface alignment (`src/main.rs`, `src/mm/mod.rs`, `.env.example`, `.env.full.example`):
  - startup cancel-all skip flag is now runtime-configurable and defaults safe for MM rewards restarts:
    - `EVPOLY_STARTUP_CANCEL_ALL_SKIP_IF_MM_REWARDS` now defaults to `true` in runtime (was effectively hardcoded off).
    - when MM rewards is enabled and skip flag remains true, startup global cancel-all is skipped so weak-exit SELL continuity can survive restart.
  - auto/hybrid remote selection no longer fail-closes to empty on remote-alpha errors:
    - selection path now holds last-good active set for that cycle (`mm_rewards_auto_selection_hold_last_good`) instead of deselecting/canceling all active MM rewards conditions.
  - several MM rewards runtime knobs are now truly env-backed (previously hardcoded in runtime):
    - `EVPOLY_MM_RUNTIME_MODE`
    - `EVPOLY_MM_HARD_DISABLE`
    - `EVPOLY_MM_POLL_MS`
    - `EVPOLY_MM_REPRICE_MIN_INTERVAL_MS`
    - `EVPOLY_MM_WEAK_EXIT_STALE_FALLBACK_ENABLE`
    - `EVPOLY_MM_COMPETITION_HIGH_FREEZE_ENABLE`
    - `EVPOLY_MM_SCORING_SPREAD_EXTEND_CENTS`
  - affects `mm_rewards_v1` across all non-sports MM-selected markets/timeframes/symbols (sports flow remains owned by `mm_sport_v1`).

- `mm_sport_v1` pause semantics were split into entry-fill pause and continuous ratio-block mode (`src/main.rs`, `src/mm/mod.rs`, `.env.example`, `.env.full.example`):
  - pause-on-fill now arms only for inventory-increasing BUY entry fills (DB event path + WS matched-size path).
  - SELL/unwind fills no longer re-arm pause, so inventory exit is not interrupted by the same pause machine.
  - removed timer-based ratio pause (`EVPOLY_MM_SPORT_RATIO_BREACH_PAUSE_SEC` removed).
  - ratio breaches now enter continuous `ratio_blocked` state with cancel cooldown (`EVPOLY_MM_SPORT_RATIO_BREACH_CANCEL_COOLDOWN_MS`) and hysteresis-based recovery (`0.9 * max_share_ratio`, 2 consecutive healthy checks).
  - MM Sport loop defaults were tightened for faster event reaction:
    - `EVPOLY_MM_SPORT_POLL_MS` default `250`
    - `EVPOLY_MM_SPORT_EVENT_FALLBACK_POLL_MS` default `200`

- `mm_sport_v1` game-start handling and inventory-exit market retention were corrected (`src/main.rs`, `src/models.rs`):
  - pregame timers now use real match-start fields (`gameStartTime` / `startDate`) and no longer fall back to market end timestamps.
  - discovery now emits explicit missing-start filtering telemetry (`skipped_missing_start_time`) when pregame-only is enabled.
  - fallback inventory-market refresh keeps inventory-bearing conditions active until flat (not only inside prestart window).
  - pregame stop gate now allows inventory-exit flow to continue for inventory-bearing conditions.

- `mm_sport_v1` now has a dedicated per-cycle ratio/depth watchdog for currently quoted markets (`src/main.rs`):
  - checks active MM Sport BUY conditions (including one-sided drift) every loop.
  - uses WS orderbook first, then token-scoped polling fallback.
  - on breach (ratio/depth/one-sided), cancels the full condition pair, enters `ratio_blocked`, and applies short cancel cooldown to reduce churn.
  - added queue-ahead/front-ratio guard for live BUY orders:
    - per-order guard tracks `live_shares` and monotonic `ahead_shares` at price level;
    - runtime breach now includes front-ratio (`live / (ahead + live)`) not just visible top-depth ratio.
  - WS subscription scope now refreshes from active markets plus active MM Sport orders so event-driven checks stay scoped to live exposure.

- `mm_sport_v1` inventory pause/exit, queue guard, and cancel reconciliation were tightened for sports pregame markets across all discovered symbols/timeframes (`src/main.rs`, `src/mm/mod.rs`, `src/api.rs`, `src/polymarket_ws.rs`, `.env.example`, `.env.full.example`):
  - fill pause now gates inventory-only unwind until pause expiry; prestart windows (`T-60m`/`T-5m`) still allow forced exit behavior.
  - queue-ahead tracking is now strictly non-increasing for a live order; once `ahead_shares` reaches zero it is not reset upward unless the order context changes.
  - MM Sport cancel helper now marks local pending rows terminal only after successful exchange cancel or terminal reconciliation (`FILLED`/`CANCELED`/`STALE`), avoiding false local safety on cancel errors.
  - exposure freshness defaults were tightened:
    - `EVPOLY_PM_WS_MARKET_STALE_MS`: runtime default `2500 -> 600`, env full example `1000 -> 600`.
    - `EVPOLY_MM_SPORT_WS_STALE_MS`: `2500 -> 600`.
    - `EVPOLY_MM_SPORT_RATIO_BREACH_CANCEL_COOLDOWN_MS`: `2000 -> 200`.
  - hardcoded pair baseline gate sizing (`1.2x`) is now env-configurable with `EVPOLY_MM_SPORT_PAIR_BASELINE_QUOTE_SIZE_MULT` (default `1.2`).

- Pending-order reconciliation hardening for MM strategies (`src/tracking_db.rs`):
  - `update_pending_order_status` and `update_pending_order_status_batch_any` no longer downgrade `FILLED` rows to non-filled terminal states.
  - missing-entry-fill backfill now also considers rows with non-null `filled_at_ms` even if status changed from `FILLED`.
  - affects `mm_sport_v1`/`mm_rewards_v1` runtime safety against stale cancel-vs-fill races.

- `mm_rewards_v1` entry-cap default was changed to unlimited in the arbiter worker admission path (`src/main.rs`, `.env.full.example`):
  - `EVPOLY_MAX_MM_REWARDS_ENTRIES_PER_TIMEFRAME_PERIOD` now defaults to no cap (`usize::MAX`) when unset.
  - Env override remains supported: setting `EVPOLY_MAX_MM_REWARDS_ENTRIES_PER_TIMEFRAME_PERIOD=<integer>` enforces an explicit per-timeframe/period cap.
  - Affects MM Rewards across all symbols/timeframes (notably `1d` BTC buckets that previously hit default cap `20` and emitted `entry_invariant_blocked`).

### 2026-03-19

- `mm_sport_v1` discovery loop was hardened against CLOB rate-limit starvation (`src/main.rs`):
  - discovery no longer re-hammers every ~1s when no markets are selected; it now uses exponential backoff (`5s` to `5m`) for empty/error refreshes and tracks `next_discovery_attempt_ms`.
  - when CLOB market-detail fetch is rate-limited, strategy now falls back to Gamma slug lookup (`get_market_by_slug`) to recover token ids + game time and keep discovery alive.
  - added explicit rate-limit telemetry via `mm_sport_discovery_rate_limited` plus richer `mm_sport_discovery_refresh` counters (`clob_detail_rate_limited`, fallback counts, cache usage, backoff timing).
  - discovery keeps and reuses last-good market set during transient detail-fetch failures to avoid dropping active scope to zero.

- `mm_sport_v1` internal budget caps were removed from live quoting (`src/main.rs`):
  - removed MM Sport balance/allowance-derived BUY budget accounting and all budget-driven quote scaling/fallback branches.
  - quote size is now governed by configured strategy size plus existing depth/ratio/min-size gates only.
  - retained runtime safety for real exchange capacity failures (`mm_sport_buy_backoff_balance_allowance` + pair cancel on insufficient balance/allowance errors).
  - affected scope: MM Sport pregame rewards markets across all discovered sports match conditions.

- `mm_sport_v1` budget gating and collateral allowance parsing were hardened to prevent false zero-budget starvation (`src/main.rs`, `src/api.rs`):
  - MM Sport now reserves budget against its own active BUY notional only (instead of all strategies), so `mm_rewards_v1` open BUYs do not zero out MM Sport quote capacity.
  - USDC allowance lookup now falls back to the max allowance reported by CLOB when the configured exchange key resolves to zero/missing.
  - MM Sport budget now falls back to balance-based sizing when collateral allowance resolves to zero, preventing hard-stop when balance is available but allowance telemetry is stale/incomplete.
  - impact: MM Sport can keep quoting eligible markets when wallet collateral is available, while still enforcing strategy-local budget caps.

- `mm_sport_v1` quoting/exposure control was rewritten to remove ratio-based size shrinking and enforce strict pair-level watch/cancel behavior (`src/main.rs`, `src/mm/mod.rs`, `.env.example`, `.env.full.example`):
  - removed per-side “shrink to fit ratio” path; normal quoting now uses strict ratio feasibility only.
  - if either side breaches `max_share_ratio`, MM Sport cancels both sides for that market and stays in watch mode until depth recovers.
  - buy sizing now applies a collateral budget precheck using `0.8 * min(USDC balance, USDC allowance)` with open-buy notional reservation; pair quotes are scaled or skipped when budget is insufficient.
  - on buy capacity errors (`not enough balance / allowance`), MM Sport cancels the market pair to prevent persistent one-sided quoting and enters cooldown.
  - `EVPOLY_MM_SPORT_MAX_SHARE_RATIO` default remains `0.02` (runtime override supported).

- `mm_sport_v1` fill handling now uses a longer pause and post-pause inventory unwind flow (`src/main.rs`, `src/mm/mod.rs`, `.env.example`, `.env.full.example`):
  - pause after fill default changed from `900s` to `7200s` (`EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC`).
  - after pause, if inventory remains, market enters inventory-exit mode (BUY quoting off, SELL unwind on) until flat.
  - once flat, strategy resumes normal two-sided BUY quoting.

- `mm_sport_v1` normal quoting now enforces a hard pair-level baseline feasibility gate before any side quote (`src/main.rs`):
  - hardcoded low-depth floor raised from `30,000` to `50,000` USD (`MM_SPORT_LOW_DEPTH_FLOOR_USD`).
  - new baseline gate requires both outcomes to support at least `1.2x` reward-min shares under `max_share_ratio` (default 2%): required external top shares per side = `baseline * (1-r)/r`.
  - if either side fails that baseline ratio gate, bot cancels both sides for the market and waits; quoting resumes only after both sides become feasible again.
  - BUY placement failures from balance/allowance errors now trigger a per-token BUY cooldown backoff (`30s`) before reattempt (`mm_sport_buy_backoff_balance_allowance`), reducing rapid retry spam while preserving normal quoting when capacity recovers.

- `mm_rewards_v1` + `mm_sport_v1` pending-order reconciliation now treats exchange unknown terminal states (notably `INVALID`) as terminal instead of `OPEN` (`src/main.rs`, `src/trader.rs`):
  - shared terminal helpers now classify unknown status strings into `FILLED` / `CANCELED` / `STALE`; `INVALID` maps to `STALE`.
  - MM stale-check paths now consider `Unmatched` and terminal unknown statuses as terminal for cleanup.
  - fixes stale `OPEN` rows that could block quote refresh and create apparent one-sided quoting even when exchange order is already invalid.
- `mm_sport_v1` websocket order-state loop now writes terminal WS statuses back to `pending_orders` immediately (`src/main.rs`):
  - on WS terminal status (`Matched`/`Canceled`/`Unmatched`/terminal unknown), DB status is updated in-loop (no wait for slower periodic reconcile).
  - reduces stale-open lag and prevents phantom “kept” rows from suppressing replacement orders on one side.

- Shared sizing-policy defaults are now env-overridable while keeping the same code defaults (`src/main.rs`, `src/size_policy.rs`, `.env.example`, `.env.full.example`):
  - `premarket_v1` fixed ladder can now be overridden via:
    - `EVPOLY_PREMARKET_FIXED_LADDER_PRICES` (default `0.40,0.30,0.24,0.21,0.15,0.12`)
    - `EVPOLY_PREMARKET_FIXED_LADDER_WEIGHTS` (default `0.23,0.23,0.17,0.14,0.12,0.11`)
  - `endgame_sweep_v1` per-tick split can now be overridden via:
    - `EVPOLY_ENDGAME_TICK0_MULTIPLIER` (default `0.20`)
    - `EVPOLY_ENDGAME_TICK1_MULTIPLIER` (default `0.40`)
    - `EVPOLY_ENDGAME_TICK2_MULTIPLIER` (default `0.40`)
  - `sessionband_v1` tau multipliers can now be overridden via:
    - `EVPOLY_SESSIONBAND_TAU2_MULTIPLIER` (default `0.30`)
    - `EVPOLY_SESSIONBAND_TAU1_MULTIPLIER` (default `0.70`)
  - invalid env values fail closed to existing code defaults (no behavior change unless valid override is set).

- `evcurve_v1` fill-state tracking now has an explicit WSS-driven reconcile loop with polling fallback (`src/main.rs`, `src/trader.rs`, `.env.example`, `.env.full.example`):
  - when Polymarket WS is enabled, runtime now wakes on user-channel events and reconciles EVcurve pending orders via `api.get_order` (WS snapshot first, API fallback).
  - polling fallback is controlled by `EVPOLY_EVCURVE_FILL_FALLBACK_POLL_MS` (default `1500ms`) so fills still reconcile without fresh WS events.
  - existing global periodic reconcile path remains unchanged as a safety backstop.

- `mm_sport_v1` prestart inventory-exit reliability hardening (`src/main.rs`):
  - added tolerant `game_start_time` parsing fallback formats so MM Sport can resolve start timestamps even when feeds return non-strict RFC3339 datetime strings.
  - added inventory-condition fallback refresh (`60s`) so open MM Sport positions can still enter prestart exit handling even if a condition temporarily falls out of normal reward discovery scope.
  - prestart SELL exits now bypass reward `min_size` floor and enforce exchange min-order floor only, preventing stuck sub-reward inventory during the `T-60m` to start exit window.
  - added forced `T-5m` exit mode: if inventory remains, MM Sport places one non-post-only SELL limit targeting level-2 bid (fallback level-1 when needed) to prioritize immediate flattening.
  - affects `mm_sport_v1` sports pregame inventory-exit path across all discovered symbols/markets; normal two-outcome BUY quoting logic is unchanged.

- `mm_sport_v1` budget handling now preserves valid live quotes and avoids collateral-starvation churn (`src/main.rs`):
  - pair-level budget gating now uses `effective_remaining_budget_usd = remaining_budget_usd + existing_market_buy_notional_usd` so already-posted MM Sport BUYs are treated as reusable budget during requote checks.
  - when configured high multiplier (for example `EVPOLY_MM_SPORT_QUOTE_SIZE_MULT=10`) exceeds current effective budget, quoting falls back to baseline `1.2x` reward min-size before hard scaling to zero.
  - token-level budget clamps now also credit existing same-token BUY notional, preventing unnecessary cancel/repost loops caused by treating current live orders as fresh budget demand.
  - affects `mm_sport_v1` normal two-outcome BUY quoting only; fill-pause and inventory-exit behavior are unchanged.

- `mm_sport_v1` ratio-breach handling now hard-pauses affected markets for 15 minutes to stop cancel churn (`src/main.rs`, `.env`):
  - when pair ratio checks fail (`mm_sport_skip_pair_ratio_limit_exceeded` or runtime `..._runtime`), MM Sport now cancels current orders for that condition and sets a 15-minute condition pause before re-checking.
  - added explicit `ratio_pause_until_ms` telemetry field on both ratio-breach events.
  - live runtime override updated to `EVPOLY_MM_SPORT_MAX_SHARE_RATIO=0.05` in `.env` (previously `0.01`).
  - affects `mm_sport_v1` sports normal quoting path; fill-trigger pause (`EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC`) is unchanged.

### 2026-03-18

- `mm_sport_v1` normal quoting now uses pair-depth regimes to avoid one-sided high-mult quotes when one outcome book is thinner (`src/main.rs`):
  - computes external top-bid depth on both outcomes and uses pair minimum depth for regime selection.
  - if pair depth `< 30,000 USD`: cancels both sides and skips quoting that market (keeps monitoring).
  - if pair depth is `>= 30,000` and `< EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD` (default `100,000`): forces quote multiplier to hardcoded `1.2x` (code default).
  - if pair depth is `>= EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD`: uses normal configured multiplier (`EVPOLY_MM_SPORT_QUOTE_SIZE_MULT`).
  - normal per-side `100k` depth skip path was removed; depth gating is now pair-level before per-token ratio clamp.

- Docs/env surface alignment for 7-symbol rollout (`.env.full.example`, `docs/endgame_sweep_v1.md`, `docs/evcurve_v1.md`, `docs/evsnipe_v1.md`, `README.md`, `AGENTS.md`):
  - `.env.full.example` now sets `EVPOLY_ENTRY_WORKER_COUNT_ENDGAME=8` to match runtime code default.
  - strategy guides now reflect 7-symbol defaults for endgame/evcurve/evsnipe (`BTC,ETH,SOL,XRP,DOGE,BNB,HYPE`) and updated symbol sizing notes.
  - operator/AI runtime docs now explicitly require `./ev` for live start/restart/status/log lifecycle.

- `mm_sport_v1` discovery refresh cadence is now hardcoded to `300s` with no env override (`src/mm/mod.rs`, `.env.example`, `.env.full.example`):
  - removed runtime env path `EVPOLY_MM_SPORT_DISCOVERY_REFRESH_SEC`.
  - MM Sport discovery now always refreshes every 5 minutes regardless of env.

- `mm_sport_v1` timeout fallback wake cadence is now hardcoded to `1000ms` with no env override (`src/mm/mod.rs`, `.env.example`, `.env.full.example`):
  - removed runtime env path `EVPOLY_MM_SPORT_EVENT_FALLBACK_POLL_MS`.
  - event-driven MM Sport still prefers websocket updates first, but its backup poll now always wakes once per second.

- `mm_sport_v1` now uses websocket-first eventing and side-clamped top-depth gating for quote size control (`src/main.rs`, `src/polymarket_ws.rs`, `src/mm/mod.rs`, `.env.example`, `.env.full.example`):
  - mm sport discovered token/condition scopes are now injected into polymarket WS subscription targets, enabling websocket-first orderbook and user-order/fill updates for sports markets.
  - loop wake path is now event-driven (`market` + `user` WS) with timeout fallback to polling (`EVPOLY_MM_SPORT_EVENT_FALLBACK_POLL_MS`).
  - added WS-assisted fill pause path (`mm_sport_market_paused_on_fill_ws`) that reacts to user-channel matched size deltas and triggers condition pause/cancel immediately.
  - max share ratio default tightened to `0.02` via `EVPOLY_MM_SPORT_MAX_SHARE_RATIO` (from `0.10`).
  - added top-level external depth floor `EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD` (default `100000`): side is skipped if external top depth is below floor.
  - normal BUY size is now clamped per side to satisfy ratio cap instead of all-or-none rejection; opposite side can continue quoting independently.

- `mm_sport_v1` quoting/execution behavior updated to reduce one-sided SELL failures and align post-fill sizing (`src/main.rs`):
  - normal quoting path now uses BUY-only on both outcomes (no literal SELL in normal mode).
  - post-fill resume sizing now boosts the opposite outcome BUY size by held inventory (`desired_buy = base_quote + opposite_inventory`), e.g. if A fills, B quote grows after pause.
  - normal-mode invalid side handling is now strategy-scoped per token side (stale/non-target rows canceled on that side) while preserving independent quoting on the opposite side.
  - literal SELL remains in the prestart inventory-exit window only (60 minutes before game start), preserving exit behavior while eliminating normal-mode SELL spam.

- `mm_rewards_v1` CBB priority/only paths were removed from runtime (`src/main.rs`, `src/mm/mod.rs`, `src/mm/reward_scanner.rs`, `src/bin/alpha_service.rs`, `.env.example`, `.env.full.example`, `docs/mm_rewards_v1.md`):
  - removed `EVPOLY_MM_CBB_*` env surface and CBB-only/piority selection hooks.
  - removed CBB candidate metadata plumbing (`is_cbb`) from MM rewards discovery/selection payloads.
  - MM rewards now runs without CBB-specific bypass/selection behavior.

- Added new `mm_sport_v1` strategy runtime (`src/main.rs`, `src/mm/mod.rs`, `src/strategy.rs`, `src/tracking_db.rs`, `src/trader.rs`, `.env.example`, `.env.full.example`, `docs/strategy_combos.md`):
  - new strategy toggle `EVPOLY_STRATEGY_MM_SPORT_ENABLE` (default `false`).
  - new MM Sport env surface (`EVPOLY_MM_SPORT_*`) for poll cadence, discovery refresh, reward/day threshold, quote size multiplier, max top-share ratio, fill-pause duration, and market filters.
  - discovery behavior: scans rewards markets, keeps sports pregame match markets only, requires reward-eligible params by default, and keeps rate/day `>= 300` by default.
  - quoting behavior: top bid + top ask only (per token), maker-only (`post_only=true` hard-enforced), no ladder, with strict ratio gate `our_size / (existing_top + our_size) < max_share_ratio` for both sides; if either side fails gate, both sides are canceled/skipped.
  - fill behavior: any `ENTRY_FILL` or `EXIT` event for `mm_sport_v1` pauses that market for `EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC` (default 900s), cancels active orders for that condition, and resumes after pause with inventory-adjusted ask sizing.
  - tracking additions: `tracking_db.list_strategy_fill_events_since(...)` now exposes strategy-scoped fill stream (`ENTRY_FILL` + `EXIT`) used by MM Sport pause logic.

- `mm_sport_v1` sports discovery + pregame inventory-exit behavior updated (`src/main.rs`):
  - discovery now classifies sports markets using CLOB market tags/category (with slug fallback) instead of requiring `sports/...` slug prefixes, which restores CBB and other sports reward market discovery.
  - execution now enters inventory-exit mode exactly 60 minutes before `game_start_time`: BUY quoting is canceled/stopped, SELL exits are managed post-only on top ask, and pause-on-fill is bypassed during this prestart exit window.
  - exit submissions enforce MM-rewards-style minimum exit floor (`max(exchange_min_shares, reward_min_shares)`), and undersized inventory is skipped with explicit telemetry.
  - affects `mm_sport_v1` only (all sports symbols/timeframes discovered by rewards feed); non-MM-sport strategies unchanged.

- `mm_rewards_v1` now hard-excludes sports markets so MM Sport owns sports flow (`src/mm/mod.rs`, `src/mm/reward_scanner.rs`, `src/main.rs`):
  - hard-disabled `sports_pregame` mode in runtime defaults (`mode_enable_sports_pregame=false`) with no env override path.
  - auto candidate scan now rejects sports markets at source using market tags + slug classification before ranking/selection.
  - added runtime safety guard: if any sports market still reaches the MM rewards execution loop, MM rewards skips it and strategy-scoped cancels `mm_rewards_v1` pending orders for that condition.
  - affects `mm_rewards_v1` only; cancellation path is strategy-scoped and does not touch `mm_sport_v1` orders.

- `endgame_sweep_v1` arbiter worker defaults and fastlane submit dispatch were adjusted for 7-symbol concurrency (`src/main.rs`):
  - default `EVPOLY_ENTRY_WORKER_COUNT_ENDGAME` fallback changed from `2` to `8` when env is unset.
  - endgame fastlane submit path no longer blocks worker loop on `submit_task.await`; endgame requests now use spawned submit tasks like non-fastlane paths.
  - affects endgame execution queueing/submit concurrency across enabled endgame symbols/timeframes by reducing per-worker head-of-line blocking under bursty tick windows.

- `endgame_sweep_v1` proxy-source routing was hardcoded for the 7-symbol default universe (`src/main.rs`, `src/hyperliquid_wss.rs`, `src/lib.rs`):
  - `BNB` now uses Binance spot trade feed (`bnbusdt@trade`) for all enabled endgame timeframes.
  - `HYPE` now uses Hyperliquid spot trade feed (`HYPEUSDC`, hardcoded coin `@107`) for `5m/15m/4h`, and Binance trade feed for `1h`.
  - synthetic proxy snapshots are generated from Binance/Hyperliquid trade prints to preserve existing endgame planner interfaces (`CoinbaseBookState` path) with minimal strategy logic changes.
  - affects endgame proxy input path only; strategy toggles and endgame symbol default env (`EVPOLY_ENDGAME_SYMBOLS`) remain unchanged.

- `endgame_sweep_v1` DOGE path was aligned to XRP-style Coinbase routing (`src/main.rs`):
  - added `DOGE <-> DOGE-USD` Coinbase product mapping in shared market-symbol helper functions used by endgame feed bootstrap.
  - DOGE now uses the same Coinbase level2 proxy path as BTC/ETH/SOL/XRP in endgame.

- `endgame_sweep_v1` DOGE proxy-source default was hard-switched to Binance to mirror BNB (`src/main.rs`):
  - `endgame_proxy_source_for_symbol_timeframe(...)` now routes `DOGE` to Binance (`dogeusdt@trade`) for all enabled endgame timeframes.
  - removes Coinbase as the endgame proxy feed source for DOGE while preserving existing synthetic snapshot/planner interfaces.

- Endgame/trader symbol attribution was corrected for `DOGE/BNB/HYPE` submit telemetry (`src/main.rs`, `src/trader.rs`):
  - `entry_execution_timing.asset_symbol` now prefers symbol extraction from `request_id` (e.g. `...:doge:...`) instead of falling back to token-type family (`BTC/ETH/SOL/XRP` only).
  - `trade_events.asset_symbol` now uses `reason.request_id` symbol fallback when token-type label cannot represent extended symbols, so endgame `ENTRY_SUBMIT` rows keep `DOGE/BNB/HYPE` attribution.

- Shared timeframe slug helpers were extended for `DOGE/BNB/HYPE` (`src/main.rs` tests + helpers):
  - added normalize/slug-prefix/H1-D1 asset prefix handling so discovery validation accepts these symbols across `5m/15m/1h/4h` naming paths.
  - touched code paths: `normalize_market_symbol`, `market_symbol_slug_prefixes`, `h1_symbol_from_market_slug`, `h1_market_slug_matches_target_open_ts` fallback symbol set, and `h1_event_slug_asset_prefix`.

- `endgame_sweep_v1` submit stale allowance now applies a hardcoded symbol multiplier for the new 3 symbols (`src/main.rs`):
  - runtime now multiplies remote alpha `submit_proxy_max_age_ms` by `3x` for `DOGE`, `BNB`, and `HYPE` before enqueueing endgame requests.
  - the effective value is now used in arbiter request proxy-age override and endgame execution telemetry payloads (`alpha_submit_proxy_max_age_ms`).
  - affects endgame proxy-stale gating across enabled endgame timeframes for `DOGE/BNB/HYPE` only; other symbols keep the unscaled remote alpha value.

- `sessionband_v1` fastlane submit dispatch now uses nonblocking worker submit scheduling (`src/main.rs`):
  - arbiter fastlane worker path now spawns `sessionband_v1` submit tasks instead of awaiting each submit inline.
  - aligns sessionband with endgame fastlane behavior to reduce worker head-of-line blocking during bursty periods.
  - affects sessionband execution queueing/submit concurrency across enabled sessionband symbols/timeframes.

- `evsnipe_v1` arbiter worker default pool size fallback increased (`src/main.rs`):
  - default `EVPOLY_ENTRY_WORKER_COUNT_EVSNIPE` fallback changed from `2` to `4` when env is unset.
  - affects EVSnipe arbiter execution parallelism defaults for EVSnipe entry submits (all EVSnipe symbols/timeframes).

- Shared FAK/FOK BUY precision alignment now probes taker scale from token tick-size instead of price scale fallback only (`src/api.rs`):
  - submit path now fetches token `minimum_tick_size` and uses `minimum_tick_size` decimal scale + `2` as the taker probe scale for cent-step USDC notional alignment, with fallback to the previous `price.scale() + 2` behavior if tick-size fetch fails.
  - affects immediate BUY submit precision gating across strategies that use shared `place_order` FAK/FOK BUY path (`endgame_sweep_v1`, `sessionband_v1`, `evsnipe_v1`, and other FAK/FOK BUY callers) for all enabled symbols/timeframes.
  - objective is to reduce exchange precision rejects (`maker <= 2`, `taker <= 4`) on markets with finer token tick sizes (for example `0.001`).

### 2026-03-17

- Shared size policy symbol multiplier defaults expanded to include `DOGE/BNB/HYPE` at `0.5` (`src/size_policy.rs`, `.env.full.example`):
  - `symbol_size_multiplier(...)` now returns `0.5` for `DOGE`, `BNB`, and `HYPE` (same as `SOL/XRP`), instead of falling back to `1.0`.
  - affects default notional sizing in strategies that use shared base-size policy (`premarket_v1`, `endgame_sweep_v1`, `evcurve_v1`, `sessionband_v1`) when those symbols are enabled.
  - added unit-test coverage for `DOGE/BNB/HYPE` multiplier behavior.

- Strategy default symbol universe expanded to full 7-symbol set for endgame/evcurve/evsnipe (`src/config.rs`, `src/evcurve.rs`, `src/bot_admin.rs`, `.env.full.example`, `.env`):
  - `endgame_sweep_v1` env/code fallback default now uses `BTC,ETH,SOL,XRP,DOGE,BNB,HYPE` when `EVPOLY_ENDGAME_SYMBOLS` is unset.
  - `evcurve_v1` env/code fallback default now uses `BTC,ETH,SOL,XRP,DOGE,BNB,HYPE` when `EVPOLY_EVCURVE_SYMBOLS` is unset.
  - `evsnipe_v1` templates/runtime env defaults are aligned to explicit `BTC,ETH,SOL,XRP,DOGE,BNB,HYPE` (`EVPOLY_EVSNIPE_SYMBOLS`).
  - affects symbol fanout defaults across configured timeframes for these three strategy paths.
- `mm_rewards_v1` now enforces reward minimum share size on SELL/exit order paths when reward-min enforcement is enabled and the market side is reward-eligible (`src/main.rs`):
  - weak-exit sell replacement and inventory-reduction sell paths now compute `min_exit_shares` as `max(exchange_min_shares, reward_min_shares_with_policy)` instead of exchange floor only.
  - this keeps MM-reward exit quotes at reward-qualifying share size (including inventory unwind/rebalance ladders), avoiding sub-min-size SELL quotes that forfeit rewards.
  - affected knobs/code paths: `reward_min_size_enforce`, `mm_reward_min_shares_with_policy`, and SELL `min_exit_shares` calculation in MM rewards loop.
### 2026-03-16

- `mm_rewards_v1` weak-exit stale balance fallback is now hard-disabled in runtime (`src/mm/mod.rs`):
  - `weak_exit_stale_fallback_enable` is hardcoded to `false` (no env toggle path).
  - `EVPOLY_MM_WEAK_EXIT_STALE_FALLBACK_ENABLE` is now ignored by runtime config.
  - affects all MM rewards modes (`quiet_stack`, `tail_biased`, `spike`, `sports_pregame`) across configured symbols/timeframes by preventing cached-stale-balance weak-exit submits.

- `premarket_v1` now has delayed take-profit limit placement via a dedicated non-blocking TP worker queue (`src/trader.rs`, `.env.example`, `.env.full.example`):
  - applies only to `15m/1h/4h` premarket positions (`5m` excluded).
  - TP scheduling starts at `T+5m` from market launch (`period_timestamp + 300s`).
  - worker retries every `30s` until API position basis is available (`avg entry` + `position size`), then places SELL LIMIT TP.
  - TP price rule: `max(entry*2, top_ask, 0.60)` with tick alignment before submit.
  - queue/enqueue path is `try_send` with bounded capacity and per-trade inflight/retry state, so TP API calls/timeouts do not block pending-trade loop or other strategies.
  - runtime toggle added: `EVPOLY_PREMARKET_TP_ENABLE` (default `true`).

- `mm_rewards_v1` preflight gate moved back to local runtime evaluation while keeping alpha parity for scoring/full-ladder logic (`src/main.rs`, `src/mm/scoring_guard.rs`, `docs/mm_rewards_v1.md`, `.env.example`, `.env.full.example`, `scripts/remote_onboard.py`, `README.md`):
  - MM remote alpha is now selection-only (`EVPOLY_REMOTE_MM_REWARDS_SELECTION_ALPHA_URL` + `EVPOLY_REMOTE_MM_REWARDS_ALPHA_TOKEN`).
  - bot-side preflight now uses the same scoring/Q-min bypass/full-ladder rules as alpha service preflight handler, but evaluated in-process before rung submission.
  - removed runtime/env onboarding dependency on `EVPOLY_REMOTE_MM_REWARDS_PREFLIGHT_ALPHA_URL`.

- Manual close chase-limit replace flow now actively reprices stale working orders even when order lookup succeeds (`src/bin/manual_bot.rs`):
  - in `/manual/close` chase mode, inflight non-terminal orders older than settle window are now marked stale and canceled/reconciled before next quote pass, instead of waiting on lookup-error path only.
  - stale-cancel reconcile now records terminal state from post-cancel status check rather than forcing terminal on any successful reconcile read.
  - affects manual close execution (`/manual/close`, `ChaseLimit`) for all symbols/timeframes by restoring continuous cancel/requote behavior on drifting prices.

- `evsnipe_v1` market discovery was realigned to Poly-builder behavior for both bot fallback and remote alpha output (`src/evsnipe.rs`, `src/bin/alpha_service.rs`):
  - local discovery now uses Gamma crypto-tag paging (`tag_slug=crypto`) with event-offset stepping (`offset += page_limit`), matching Poly-builder event-pagination semantics and avoiding market-window skips caused by flattened-market offset stepping.
  - fallback/local page-0 retry remains, but now retries the same crypto-tag path at smaller page size (`200`) for rate-limit resilience.
  - alpha service EVSnipe cache prewarm now calls the local-only EVSnipe discovery path directly, so `/v1/discovery/evsnipe` serves the same discovery set as Poly-builder logic instead of depending on upstream remote EVSnipe discovery config.
  - alpha service EVSnipe cache now also applies strike-distance anchor filtering before publishing specs (same `abs(strike-anchor)/anchor <= EVPOLY_EVSNIPE_STRIKE_WINDOW_PCT` behavior used by bot runtime), reducing remote discovery payload size and aligning remote served set with bot tradable watchlist.
  - affects EVSnipe hit/close market discovery coverage across configured symbols (`BTC,ETH,SOL,XRP`) and expiry-window filtering.

- `mm_rewards_v1` remote alpha defaults were normalized to public HTTPS domains (no localhost fallback profile for remote MM endpoints) (`src/main.rs`, `.env`):
  - runtime default URL fallback keys `EVPOLY_REMOTE_MM_REWARDS_SELECTION_ALPHA_URL` and `EVPOLY_REMOTE_MM_REWARDS_PREFLIGHT_ALPHA_URL` now default to `https://alpha.evplus.ai/...`.
  - repository `.env` MM remote alpha URL values were updated from `127.0.0.1` to public `alpha.evplus.ai` endpoints.

- Shared submit metadata prewarm + MM/EVSnipe selective prewarm were hardened against tick-size rate-limit storms (`src/api.rs`, `src/main.rs`, `src/mm/mod.rs`, `.env.full.example`):
  - shared order build path now applies token-level rate-limit backoff for tick metadata (`base=1000ms`, `max=30000ms`) and skips immediate metadata retry when first build failure is rate-limited (`429/1015`), while preserving existing retry path for non-rate-limit failures.
  - MM loop now prewarms only currently-selected market token IDs (UP/DOWN), with bounded per-loop token budget + cooldown, instead of broad metadata churn; EVSnipe refresh loop now does the same for discovered hotset tokens.
  - MM constraint/cache cooldown knobs are now hardcoded in runtime (`constraints cache=30000ms`, constraints failure cooldown `60000ms`, market-details failure cooldown `60000ms`) and removed from env template surface.
  - affects submit-path resilience and metadata call pressure across strategy submits (including `premarket_v1`, `endgame_sweep_v1`, `evcurve_v1`, `sessionband_v1`, `evsnipe_v1`, `mm_rewards_v1`), with direct behavioral impact on `mm_rewards_v1` and `evsnipe_v1` prewarm cadence.

- Redeem path now includes neg-risk adapter handling plus post-submit effect verification (`src/api.rs`, `src/trader.rs`):
  - redeem/merge closeout flow now attempts neg-risk adapter redeem (`redeemPositions(bytes32,uint256[])`) for neg-risk markets and falls back to CTF redeem path when adapter payload is unavailable or fails.
  - redemption success state now requires verified effect (condition disappears from live redeemable set); relayer-confirmed submits without observed effect are kept in retry/submitted state instead of final success.
  - affects settlement/closeout behavior for all strategies that rely on redemption lifecycle (`premarket_v1`, `endgame_sweep_v1`, `evcurve_v1`, `sessionband_v1`, `evsnipe_v1`, `mm_rewards_v1`).

- Remote alpha/discovery timeout surface cleanup + EVSnipe hard timeout (`src/main.rs`, `src/evsnipe.rs`, `.env`, `.env.example`, `.env.full.example`):
  - premarket/endgame/evcurve/sessionband remote alpha timeouts remain hardcoded at `1000ms` in runtime (no env control path).
  - EVSnipe remote discovery timeout is now hardcoded at `2000ms` (removed `EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TIMEOUT_MS` env path).
  - runtime env templates and local `.env` removed stale timeout keys for those strategy alpha/discovery timeout values.
  - local runtime `.env` alpha/discovery URLs were switched back to public `https://alpha.evplus.ai/...` endpoints.
  - affects remote alpha/discovery request behavior for `premarket_v1`, `endgame_sweep_v1`, `evcurve_v1`, `sessionband_v1`, and `evsnipe_v1`.

### 2026-03-15

- `premarket_v1` submit cap guard was hardcoded to `48` per token-side (`src/main.rs`, `docs/premarket_v1.md`, `.env.full.example`):
  - removed env override path `EVPOLY_PREMARKET_SUBMIT_CAP_PER_TOKEN`.
  - reason: prevent SOL/XRP premarket ladders from getting blocked when BTC/ETH consume inflight submit budget in the same timeframe window.

- Redeem/merge relayer submit priority was flipped to local-key primary with remote signer fallback (`src/api.rs`, `scripts/remote_onboard.py`, `.env.example`, `.env.full.example`, `README.md`):
  - for redeem/merge submit paths, `RELAYER_API_KEY` + `RELAYER_API_KEY_ADDRESS` is now attempted first when configured.
  - if local relayer-key submit fails (or is missing/misconfigured), bot falls back to remote signer submit path.
  - onboarding now prints a clear action note to fetch relayer keys from:
    `https://polymarket.com/settings?tab=api-keys`
    and set them in `.env` (or ask AI to fill).

- Runtime submission/discovery/balance pressure controls were hardened with hardcoded values and reduced env surface (`src/api.rs`, `src/trader.rs`, `src/main.rs`, `src/bot_admin.rs`, `.env`, `.env.example`, `.env.full.example`, `scripts/remote_onboard.py`):
  - USDC balance+allowance cache windows increased to `hit=60s` and `fallback=180s`, plus singleflight gating so only one in-flight collateral `/balance-allowance` fetch runs at a time.
  - USDC balance+allowance fetch no longer performs immediate second attempt when first failure is rate-limit (`429/1015`); it now falls back to cached snapshot if valid or returns error.
  - redemption/merge available-ratio auto-trigger checks now reuse a hardcoded cached USDC snapshot window (`300s`) instead of forcing a fresh balance call on every closure scheduler pass.
  - remote timeframe market discovery timeout is hardcoded to `2000ms` (no env override).
  - latency-critical order submit retry defaults are hardcoded low (`order_api_attempts=1`, signer rate-limit retries `=1`), while non-critical maintenance/reconcile retry paths remain unchanged.
  - removed now-unused order submit retry env knobs from templates (`EVPOLY_ORDER_API_RETRY_ATTEMPTS`, `EVPOLY_ORDER_API_RETRY_BASE_MS`).

- `premarket_v1` ladder and dispatch defaults were tightened and hardcoded (`src/main.rs`, `src/bin/manual_bot.rs`, `.env`, `.env.full.example`, `docs/premarket_v1.md`):
  - fixed ladder reduced from `10x10` to `6x6` per side with hardcoded prices:
    `0.40,0.30,0.24,0.21,0.15,0.12`
    and normalized hardcoded weights:
    `0.23,0.23,0.17,0.14,0.12,0.11`.
  - premarket worker/scope-lane controls are now hardcoded and no longer env-driven:
    - `entry worker count = 4`
    - `scope lane max = 48`
    - `scope lane queue cap = 32`
  - removed obsolete premarket discovery env knobs (`EVPOLY_PREMARKET_DISCOVERY_RETRIES`, `EVPOLY_PREMARKET_DISCOVERY_DEADLINE_SEC`) and removed now-dead remote discovery timeout/fallback env onboarding writes.

- `evsnipe_v1` discovery now runs remote-first with automatic local fallback instead of fail-closed remote-only (`src/evsnipe.rs`, `src/bin/alpha_service.rs`, `.env.example`, `.env.full.example`, `docs/evsnipe_v1.md`, `AGENTS.md`):
  - `refresh_hit_market_specs(...)` now attempts remote EVSnipe discovery first and falls back to local Gamma paging/market-spec parsing when remote transport/status/data is unavailable.
  - restored local EVSnipe market-spec extraction/parsing helpers (symbol/rule/strike/token/end-ts parsing) used only for fallback path.
  - alpha service EVSnipe discovery prewarm default cadence changed from `30s` to `60s` (`DEFAULT_EVSNIPE_REFRESH_SEC`) to reduce refresh pressure.
  - affects `evsnipe_v1` discovery across default symbols (`BTC,ETH,SOL,XRP`) and all EVSnipe market families (hit + close/range) by keeping discovery available during remote alpha interruptions.

- Core balance-poll pressure controls were tightened by hardcoding slower intervals and longer USDC cache hit TTL (`src/config.rs`, `src/main.rs`, `src/api.rs`, `.env.full.example`):
  - removed env override surface for `POLY_MARKET_CLOSURE_CHECK_INTERVAL_SECONDS` and hardcoded closure worker interval to `60s` regardless of `config.json`/env values.
  - increased shared pending-trade loop interval in main runtime from `500ms` to `15s` (path that calls `check_pending_trades()` and balance-only checks).
  - increased `USDC_BALANCE_CACHE_HIT_TTL_MS` from `2000ms` to `20000ms` to reduce repeated `/balance-allowance` hits.
  - affects shared post-entry/closure polling behavior across strategy flows that rely on trader pending/closure workers (`premarket_v1`, `endgame_sweep_v1`, `evcurve_v1`, `sessionband_v1`, `evsnipe_v1`, `mm_rewards_v1`).

- Remote alpha/discovery URL defaults are now hardcoded in runtime config resolution (`src/main.rs`, `src/evsnipe.rs`, `src/bot_admin.rs`):
  - when env URL keys are blank/missing, bot now falls back to built-in defaults:
    - Premarket: `https://alpha.evplus.ai/v1/alpha/premarket/should-trade`
    - Endgame: `https://alpha.evplus.ai/v1/alpha/endgame/policy`
    - EVcurve: `https://alpha.evplus.ai/v1/alpha/evcurve`
    - SessionBand: `https://alpha.evplus.ai/v1/alpha/sessionband`
    - MM selection/preflight: `http://alpha.evplus.ai/v1/alpha/mm-rewards/{selection|preflight}`
    - Market discovery: `https://alpha.evplus.ai/v1/discovery/timeframe`
    - EVSnipe discovery: `https://alpha.evplus.ai/v1/discovery/evsnipe`
  - startup/runtime missing-config warnings now treat those URL keys as defaulted (token checks remain unchanged).
  - affects remote decision/discovery routing defaults for `premarket_v1`, `endgame_sweep_v1`, `evcurve_v1`, `sessionband_v1`, `evsnipe_v1`, and `mm_rewards_v1`.

- Removed global exposure cap gating from runtime entry flow (`src/trader.rs`, `src/config.rs`, `src/main.rs`, `src/bot_admin.rs`, `.env`, `.env.full.example`, `config.json`):
  - deleted both hard and soft cap checks previously tied to `POLY_MAX_TOTAL_EXPOSURE_USD` (`filled` and `filled+working` gate paths), including exposure reject cooldown logic.
  - removed config/env/admin setting surface for `POLY_MAX_TOTAL_EXPOSURE_USD` so cap behavior cannot be toggled back on via runtime settings.
  - affects entry gating across strategy execution modes by eliminating global exposure-based rejects (`risk_reject` hard/soft exposure paths).

- `mm_rewards_v1` merge execution now performs immediate per-condition fallback when batch merge submit fails (`src/trader.rs`):
  - merge sweep still submits batched merge requests first, but when batch submit errors or returns `success=false`, it now retries each condition immediately via single-condition `merge_complete_sets` (which includes adapter -> CTF target fallback logic).
  - manual condition merge path (`/manual/merge` with `condition_id`) now uses single-condition merge submit directly instead of one-item batch submit, so it gets the same immediate target fallback behavior.
  - affects MM merge/redeem inventory settlement flow only (`merge_sweep` / `merge_manual` paths); no quote policy or entry sizing changes.

- `premarket_v1` now applies adaptive DB-contention load shedding in hot submit paths (`src/main.rs`, `src/tracking_db.rs`):
  - arbiter premarket caps auto-scale when DB lock wait p95 rises, covering active-cap, submit-cap, and ladder rungs per side.
  - added DB contention telemetry/alert loop and throttle events (`premarket_db_contention_throttle`, `db_contention_alert`) plus late-guard burst alerting.
  - key env surface: `EVPOLY_DB_CONTENTION_*`, `EVPOLY_PREMARKET_DB_THROTTLE_*`, `EVPOLY_LATE_GUARD_ALERT_*`.
  - affects `premarket_v1` across configured symbols/timeframes by reducing submit pressure during lock contention.

- Pending-order reconcile status writes now support batched flush semantics to cut DB write churn (`src/trader.rs`, `src/tracking_db.rs`):
  - trader reconcile/startup flows aggregate status transitions and flush grouped updates.
  - new tracking-db helper `update_pending_order_status_batch_any` batches updates with fallback to per-row on error.
  - affects shared reconcile paths used by strategy pending-order lifecycle handling (including premarket/MM inventory-reconcile loops).

- Remote alpha/discovery HTTP calls now auto-failover from `alpha.evplus.ai` to `alpha2.evplus.ai` on transient primary failures (`src/main.rs`, `src/evsnipe.rs`, `.env.example`, `.env.full.example`):
  - for remote endpoints whose configured URL host is `alpha.evplus.ai`, runtime retries once against same path/query on `alpha2.evplus.ai`.
  - failover triggers on transport timeout/connect/request errors and primary HTTP `408`, `429`, or `5xx`.
  - non-transient client errors (`4xx` other than `408`/`429`) remain fail-closed with no backup retry.
  - affected flows: timeframe discovery, EVSnipe discovery, premarket alpha, endgame alpha, EVcurve alpha, SessionBand alpha, MM rewards selection/preflight alpha.

- `mm_rewards_v1` added a local reward-min-size market filter gate to skip quoting when market `rewards.min_size` exceeds an operator-defined ceiling (`src/mm/mod.rs`, `src/main.rs`, `.env.full.example`):
  - new env: `EVPOLY_MM_MAX_REWARD_MIN_SIZE` (default `0` = disabled).
  - gate compares local market details `min_size` against configured max and emits `mm_rewards_skip_reward_min_size_filtered` before orderbook/scoring/quote flow.
  - unit follows existing `EVPOLY_MM_REWARD_MIN_SIZE_UNIT` (`shares`/`usd`), so threshold semantics align with current MM reward-min policy settings.
  - affects `mm_rewards_v1` across all MM modes (`quiet_stack`, `tail_biased`, `spike`, `sports_pregame`) for all selected symbols/timeframes.

### 2026-03-14

- `mm_rewards_v1` now runs fail-closed on remote alpha for auto-selection + preflight gates, removing local alpha decision fallback paths (`src/main.rs`, `src/bin/alpha_service.rs`, `scripts/remote_onboard.py`, `.env.example`, `.env.full.example`, `docs/mm_rewards_v1.md`, `README.md`):
  - bot-side MM auto mode assignment/selection now calls remote endpoints (`/v1/alpha/mm-rewards/selection`) and no longer executes local `select_top_candidates_by_reward` / local force-include decision logic as fallback.
  - bot-side MM preflight scoring/full-ladder gating now calls remote endpoint (`/v1/alpha/mm-rewards/preflight`) and blocks quoting when remote MM alpha is missing/unavailable (`remote_alpha_not_configured` / `remote_alpha_unavailable`).
  - added MM remote alpha env surface:
    - `EVPOLY_REMOTE_MM_REWARDS_SELECTION_ALPHA_URL`
    - `EVPOLY_REMOTE_MM_REWARDS_PREFLIGHT_ALPHA_URL`
    - `EVPOLY_REMOTE_MM_REWARDS_ALPHA_TOKEN`
    - `EVPOLY_REMOTE_MM_REWARDS_ALPHA_TIMEOUT_MS` (default `1000`)
  - onboarding helper now writes MM remote alpha URLs/token alongside existing strategy alpha keys.
  - affects `mm_rewards_v1` across all MM-enabled symbols/timeframes/modes by making quote-policy alpha decisions remote-authoritative.

- `mm_rewards_v1` auto market discovery/rotation refresh cadence default was increased from `60s` to `900s` (`15m`) (`src/mm/mod.rs`, `src/main.rs`, `.env.full.example`, local `.env`):
  - changed default env fallback for `EVPOLY_MM_AUTO_REFRESH_SEC` to `900` in MM config parsing.
  - changed MM runtime fallback values tied to `auto_refresh_sec` from `60` to `900` in background refresh + auto-selection refresh paths.
  - affects MM rewards auto market selection and competition snapshot refresh timing across MM-selected markets/timeframes/symbols (`mm_rewards_v1`).

- `mm_rewards_v1` MM precheck notional hard cap now has a floor clamp of `$20.00` in trader pre-submit validation (`src/trader.rs`):
  - hard cap formula changed from `max(target*1.05, target+0.50)` to `max(target*1.05, target+0.50, 20.0)`.
  - affects all MM rewards symbols/timeframes when exchange min-order floors upsize small target notionals.

- `mm_rewards_v1` competition-level filter code default is now narrowed to `low,medium` when `EVPOLY_MM_COMPETITION_LEVEL_ALLOWLIST` is unset/empty (`src/mm/mod.rs`, `.env.full.example`):
  - env override still fully controls behavior (supports explicit `low/medium/high/unknown` or `all/*`).
  - invalid/unknown-only env values now safely fall back to `low,medium` instead of broad allow-all.

- `mm_rewards_v1` market-details refresh now applies a per-condition cooldown after rate-limit failures when stale reward snapshots are already cached (`src/main.rs`, `.env.full.example`):
  - on `429` / `Too Many Requests`, MM records a short retry-after window (`EVPOLY_MM_MARKET_DETAILS_FAILURE_COOLDOWN_MS`, default `30000` ms) and reuses cached market details during cooldown instead of repeatedly refetching the same market metadata.
  - when stale details are already cached, MM now skips extra same-cycle 429 retries and immediately falls back to cached details + cooldown, reducing avoidable request bursts under heavy MM dispatch.
  - this targets MM auto-scan + quote refresh loops across all modes/timeframes/symbols, reducing rate-limit pressure without changing order placement logic or reward-floor sizing.

- `mm_rewards_v1` submit-path metadata prewarm now uses token-level singleflight protection to avoid duplicate concurrent metadata fetches for the same token under parallel MM dispatch (`src/api.rs`):
  - when multiple workers try to prewarm the same token at once, only one request fanout (`tick_size`/`fee_rate`/`neg_risk`) is allowed; other workers wait briefly and reuse the warmed cache.
  - this reduces CLOB metadata endpoint burst pressure and cuts `Failed to build order ... /tick-size ... 429` submit errors without changing order sizing/price policy.

- `mm_rewards_v1` constraint handling was hardened to avoid transient-read failures turning into entry failures under high dispatch load (`src/main.rs`, `src/trader.rs`):
  - MM loop now primes trader-side market-constraint cache from already-fetched MM constraint snapshots before entry submits (`prime_market_constraints_snapshot` path).
  - MM loop now allows transient stale-constraint fallback when fresh constraint refresh fails with read-transient signatures (`429` / timeout / gateway), using recent cached constraints for a bounded window (`~max(120s, 6x EVPOLY_MM_CONSTRAINTS_CACHE_MS)` in loop logic).
  - trader-side `market_order_constraints` now supports bounded stale-cache fallback on transient constraint fetch errors via `EVPOLY_CONSTRAINT_CACHE_STALE_FALLBACK_MS` (default `900000` ms), instead of failing immediately with `market_not_ready` when recent cached constraints exist.
  - `mm_rewards_notional_floor_exceeds_budget` is now treated as an explicit MM skip path (`mm_rewards_skip_notional_floor_budget`) rather than a submit error when exchange floor upsize exceeds per-order MM budget policy (`src/trader.rs`).
  - order metadata prewarm (`tick_size`/`fee_rate`/`neg_risk`) now fails open during submit path: transient prewarm failures no longer hard-fail order submits and instead continue with on-demand metadata fetch (`src/api.rs`).
  - affected scope: `mm_rewards_v1` across all MM-selected symbols/timeframes/modes (`quiet_stack`, `tail_biased`, `spike`, `sports_pregame`) where entries pass through shared trader constraint precheck.

### 2026-03-13

- Strategy enable defaults were aligned to run five core strategies by default and keep MM rewards off by default (`.env.full.example`, `.env.example`, `src/config.rs`, `src/evcurve.rs`, `src/sessionband.rs`, `src/evsnipe.rs`, `src/bot_admin.rs`):
  - default-on: `premarket_v1`, `endgame_sweep_v1`, `evcurve_v1`, `sessionband_v1`, `evsnipe_v1`.
  - default-off: `mm_rewards_v1`.
  - `.env.full.example` strategy toggles now match this profile and `.env.example` now includes the same strategy toggle block.
  - runtime fallback defaults (when env is unset) were changed to `true` for endgame/evcurve/sessionband/evsnipe while MM remains `false`; this affects default startup behavior across default symbols/timeframes (`BTC,ETH,SOL,XRP` with `5m/15m/1h/4h`, plus EVcurve `1d` where configured).

- `mm_rewards_v1` env surface was simplified to avoid confusion between base quote size and reward floor multiplier (`.env`, `.env.full.example`, `src/mm/mod.rs`):
  - removed `EVPOLY_MM_QUOTE_SHARES_PER_SIDE` from local/runtime env and full example template so MM now uses code default when unset.
  - effective default base quote size for MM remains `5.0` shares from code (`MmRewardsConfig::from_env`).
  - reward-floor scaling remains controlled by `EVPOLY_MM_REWARD_MIN_TARGET_MULT` (not by `EVPOLY_MM_QUOTE_SHARES_PER_SIDE`).
  - affects `mm_rewards_v1` quote sizing across all MM-selected symbols/timeframes/modes.

- `manual_bot` manual execution API was synced to `Poly-builder` endpoint/run-control behavior while preserving poly-oss signing/auth flow (`src/bin/manual_bot.rs`):
  - route surface now includes explicit open/close controls and run views: `/manual/open`, `/manual/close`, `/manual/open/stop`, `/manual/close/stop`, `/manual/open/runs*`, `/manual/close/runs*` (plus existing health/redeem/merge/positions/balance/pnl/audit/doctor paths).
  - semantics tightened:
    - `/manual/open` is BUY path,
    - `/manual/close` is reduce-only SELL path,
    - `tp_price` is accepted for open-only and rejected on close (`400`).
  - close-run safety includes duplicate scope conflict guard (`action+condition+token`), overlap prevention, cancel confirmation + post-cancel fill reconciliation, and stronger terminal status/stop_reason transitions for stop/timeout/cancel outcomes.
  - affects manual execution flows (`manual_open*`, `manual_close*`) without changing strategy runtime loops.

- Strategy/arbiter cap defaults were raised to new baseline values (`src/arbiter.rs`, `src/config.rs`, `src/sessionband.rs`, `src/evcurve.rs`, `src/evsnipe.rs`, `src/mm/mod.rs`, `.env.full.example`):
  - arbiter global default changed to `EVPOLY_ARB_GLOBAL_MAX_USD=100000` in code; `.env.full.example` arbiter guidance now reflects `100000`.
  - arbiter per-strategy `.env.full.example` caps raised for `sessionband_v1` and `evsnipe_v1`:
    - `EVPOLY_ARB_STRAT_SESSIONBAND_MAX_USD=10000`
    - `EVPOLY_ARB_STRAT_EVSNIPE_MAX_USD=10000`
  - strategy cap defaults changed in code:
    - `endgame_sweep_v1`: `EVPOLY_ENDGAME_PER_PERIOD_CAP_USD` default `250 -> 10000`
    - `sessionband_v1`: `EVPOLY_SESSIONBAND_STRATEGY_CAP_USD` default `2000 -> 10000`
    - `evcurve_v1` D1: `EVPOLY_EVCURVE_D1_STRATEGY_CAP_USD` default `5000 -> 10000`
    - `evsnipe_v1`: `EVPOLY_EVSNIPE_STRATEGY_CAP_USD` default `2000 -> 10000`
    - `mm_rewards_v1`: `EVPOLY_MM_AUTO_RANK_BUDGET_USD` default `2000 -> 5000`
  - `.env.full.example` was aligned to these defaults (endgame per-period cap, EVcurve D1 cap, SessionBand cap, EVSnipe cap, MM auto rank budget).

- `sessionband_v1` checkpoint schedule is now hardcoded to two alpha trigger points only (`src/main.rs`):
  - strategy evaluates checkpoint eligibility only for `tau_sec=2` and `tau_sec=1` (all other second-level checkpoints are ignored).
  - remote alpha request timing is now aligned to pre-trigger offsets before close: `2050ms` (`tau=2`) and `1050ms` (`tau=1`), with a small hardcoded grace window to tolerate poll jitter.
  - checkpoint dedupe now uses per-period checkpoint IDs (`tau=2` / `tau=1`) instead of generic per-second dedupe.
  - affects `sessionband_v1` across configured symbols/timeframes (`5m/15m/1h/4h`) by reducing decision calls to exactly two late-window checkpoints per period.

- `evcurve_v1` + `sessionband_v1` local plan markdown dependency removed from runtime and repository (`src/main.rs`, `src/plan3_tables.rs`, `docs/`):
  - EVcurve startup no longer loads `docs/plan3.md` or `docs/plandaily.md`; it now boots with `Plan3Tables::empty_for_remote()` and a disabled local D1 table path, keeping decisioning remote-alpha driven.
  - SessionBand startup no longer loads/derives local watch-starts from `docs/plan4b.md`; SessionBand loop now runs without local plan-table initialization and uses remote alpha decisions directly.
  - deleted strategy plan docs: `docs/plan3.md`, `docs/plandaily.md`, `docs/plan4b.md`.
  - affects EVcurve/SessionBand across their configured symbol/timeframe universes by removing local-plan startup requirements.

- `premarket_v1` market discovery is now aligned to shared remote-first discovery flow (`src/main.rs`):
  - premarket now resolves markets via `discover_market_for_timeframe_once(...)` (same path as `endgame_sweep_v1`) instead of the dedicated retry loop in `discover_market_for_timeframe(...)`.
  - `EVPOLY_REMOTE_MARKET_DISCOVERY_ALLOW_LOCAL_FALLBACK` env parsing was removed from runtime config and fallback is now hardcoded `true` for shared timeframe discovery.
  - affects premarket timeframe discovery behavior across configured symbols/timeframes (`5m/15m/1h/4h`).

- `evsnipe_v1` discovery is now strictly remote-only with no local fallback branch (`src/evsnipe.rs`, `.env.full.example`):
  - removed runtime fallback gating/env usage (`EVPOLY_REMOTE_EVSNIPE_DISCOVERY_ALLOW_LOCAL_FALLBACK`) and removed local Gamma paging fallback from `refresh_hit_market_specs(...)`.
  - EVSnipe now fails closed when remote discovery is not configured or returns no data.
  - `.env.full.example` removed both fallback keys:
    - `EVPOLY_REMOTE_MARKET_DISCOVERY_ALLOW_LOCAL_FALLBACK`
    - `EVPOLY_REMOTE_EVSNIPE_DISCOVERY_ALLOW_LOCAL_FALLBACK`

- `mm_rewards_v1` market mode code default changed from `target` to `auto` (`src/mm/mod.rs`):
  - `MmMarketMode::from_env(None)` now resolves to `Auto`.
  - `target`, unknown, and unset values now all resolve to `auto` (`auto` / `hybrid` remain explicit).
  - affects MM rewards market discovery routing when `EVPOLY_MM_MARKET_MODE` is unset.

- `mm_rewards_v1` market discovery path was simplified to remove symbol/timeframe target routing (`src/main.rs`, `src/mm/mod.rs`, `.env.full.example`, `.env.example`):
  - removed MM mode target env/config parsing and runtime usage (`EVPOLY_MM_SINGLE_MARKET_TARGET`, `EVPOLY_MM_TARGET_QUIET_STACK`, `EVPOLY_MM_TARGET_TAIL_BIASED`, `EVPOLY_MM_TARGET_SPIKE`, `EVPOLY_MM_TARGET_SPORTS_PREGAME`).
  - MM no longer uses shared remote timeframe discovery in its work loop; MM discovery is now local rewards auto-scan only.
  - `EVPOLY_MM_MARKET_MODE=auto` now means local auto rewards discovery only; `EVPOLY_MM_MARKET_MODE=hybrid` adds only `EVPOLY_MM_SINGLE_MARKET_SLUGS` selectors.
  - affects `mm_rewards_v1` across enabled modes (`quiet_stack`, `tail_biased`, `spike`, `sports_pregame`) for all symbols/timeframes inferred from selected reward markets.

- `endgame_sweep_v1` hard guards/defaults tightened for alpha-dependent runtime (`src/main.rs`, `src/config.rs`, `.env.example`, `.env.full.example`):
  - near-base proxy skip gate is now mandatory in endgame (no env switch); `endgame_skip_near_base_proxy_bps` always applies when distance is below `EVPOLY_NEAR_BASE_SKIP_BPS`.
  - thin-flip guard path is now default-off with no env surface (`thin_flip_guard_enable=false`, `guard_v3_enable=false` hardcoded in config parser), and thin-flip env keys were removed from env templates.
  - remote alpha client timeout is now fixed at `1000ms` for all alpha decision/policy callers (`premarket_v1`, `endgame_sweep_v1`, `evcurve_v1`, `sessionband_v1`); per-strategy alpha-timeout env keys were removed from env templates where present.

- Global symbol enable defaults now include `BTC/ETH/SOL/XRP` unless env overrides (`src/config.rs`, `.env.full.example`):
  - changed `Config::default().trading.enable_solana_trading` and `enable_xrp_trading` from `false` to `true`.
  - keeps env override behavior unchanged (`POLY_ENABLE_ETH_TRADING`, `POLY_ENABLE_SOLANA_TRADING`, `POLY_ENABLE_XRP_TRADING` still take precedence when set).
  - affects all strategy paths that use global trading symbol enables, including `premarket_v1` asset fanout and shared market discovery used by multi-strategy runtime loops.

- `evcurve_v1` timeframe/checkpoint surface tightened (`src/evcurve.rs`, `src/main.rs`, `.env.full.example`):
  - removed `5m` from EVcurve timeframe parsing (no env path can enable EVcurve `5m` now).
  - EVcurve default timeframes are now `15m,1h,4h,1d` (symbols remain `BTC,ETH,SOL,XRP` by default; env can override symbol/timeframe list, except `5m` stays unsupported).
  - hardcoded 15m checkpoints are now exactly `300,240,180` seconds (`T-5/T-4/T-3`); removed the old `T-120` toggle/env control (`EVPOLY_EVCURVE_15M_DISABLE_T3`).

- `evcurve_v1` 4h/1d base-anchor hardening ported from `Poly-builder@417e003` (`src/main.rs`, `src/api.rs`, `.env.full.example`):
  - expanded `ws_mid_carry_forward` anchor fallback from `15m`-only to non-`15m` EVcurve timeframes (notably `4h`/`1d`) when a prior-period carry value is available at period open.
  - replaced one-shot REST exact-open anchor failure behavior with retry scheduling (`evcurve_base_anchor_rest_retry`) using `EVPOLY_EVCURVE_BASE_ANCHOR_REST_RETRY_MS` (default `2000`ms), so missing REST anchor doesn’t permanently skip the period immediately.
  - enabled Coinbase open/close composition for `1d` timeframe by mapping daily proxy fetch granularity to `1h` buckets in `fetch_coinbase_open_close(...)`.

- Unified bot-side size policy across `premarket_v1`, `endgame_sweep_v1`, `evcurve_v1`, and `sessionband_v1` (excluding `evsnipe_v1` and `mm_rewards_v1`) (`src/size_policy.rs`, `src/main.rs`, `src/endgame_sweep.rs`, `src/evcurve.rs`, `src/sessionband.rs`, `.env.full.example`):
  - added per-strategy base-size env keys with blank-default behavior (`100` USD fallback): `EVPOLY_PREMARKET_BASE_SIZE_USD`, `EVPOLY_ENDGAME_BASE_SIZE_USD`, `EVPOLY_EVCURVE_BASE_SIZE_USD`, `EVPOLY_SESSIONBAND_BASE_SIZE_USD`.
  - symbol sizing now follows shared policy: `BTC=1.0`, `ETH=0.8`, `SOL/XRP=0.5`.
  - `premarket_v1` timeframe sizing now hardcodes `5m=0.75`, `15m=1.0`, `1h/4h=1.25` (applies across default symbols `BTC/ETH/SOL/XRP`, env universe overrides still apply).
  - `evcurve_v1` default sizing now uses policy multipliers `15m=0.75`, `1h=1.0`, `4h/1d=1.25`; removed fixed D1 size path from runtime sizing.
  - `endgame_sweep_v1` sizing now uses hardcoded tick split on alpha tick index: `tick0(t-3000ms)=20%`, `tick1(t-1000ms)=40%`, `tick2(t-100ms)=40%` of symbol-scaled base size.
  - `sessionband_v1` sizing now uses hardcoded tau split: `tau=2 -> 30%`, `tau=1 -> 70%`; non-`tau(2|1)` checkpoints do not produce a size.
  - SessionBand remote alpha contract no longer carries sizing (`target_size_usd` removed from alpha response payload); bot sizing remains local-authoritative.

### 2026-03-13

- `premarket_v1` now supports remote alpha yes/no gating before market discovery (`src/main.rs`):
  - added remote premarket alpha client path (`EVPOLY_REMOTE_PREMARKET_ALPHA_URL`, `EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN`, `EVPOLY_REMOTE_PREMARKET_ALPHA_TIMEOUT_MS`) as a mandatory gate before discovery/ladder build.
  - each scheduled premarket asset intent now always calls alpha first; `should_trade=false` records explicit skip (`premarket_alpha_no_trade`) and logs `remote_premarket_alpha_hit` / `premarket_alpha_skip`.
  - remote connection/timeout failures now hard-fallback to local 50/50 gate (`premarket_alpha_fallback_local`) with no env switch; non-network alpha failures remain fail-closed (`premarket_alpha_unavailable`).
  - affects `5m/15m/1h/4h` premarket flows across enabled symbols (`BTC/ETH/SOL/XRP`).

- `endgame_sweep_v1` timing/stale edge is now alpha-policy driven (`src/main.rs`):
  - added remote endgame policy client path (`EVPOLY_REMOTE_ENDGAME_ALPHA_URL`, `EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN`, `EVPOLY_REMOTE_ENDGAME_ALPHA_TIMEOUT_MS`) with required-mode control (`EVPOLY_ENDGAME_ALPHA_REQUIRED`, default true).
  - strategy now fetches one alpha policy at `t-60s` per `(symbol,timeframe,period)`; policy provides `tick_offsets_ms` and per-period `submit_proxy_max_age_ms`.
  - endgame tick deadlines now use alpha offsets (local jitter removed from scheduler path), and arbiter submit-time stale guard now uses per-request override from alpha policy.
  - added telemetry: `endgame_alpha_policy_requested`, `endgame_alpha_policy_received`, `endgame_alpha_policy_missing`, `endgame_alpha_tick_executed`, `endgame_alpha_stale_guard_blocked`.

- `endgame_sweep_v1` default universe widened in config parser/runtime defaults (`src/config.rs`, `.env.full.example`):
  - default `EVPOLY_ENDGAME_SYMBOLS` now `BTC,ETH,SOL,XRP`.
  - default `EVPOLY_ENDGAME_TIMEFRAMES` (and invalid-input fallback) now `5m,15m,1h,4h`.

- Alpha service now exposes premarket/endgame alpha endpoints with shared-token + proxy-wallet gating (`src/bin/alpha_service.rs`):
  - new routes: `POST /v1/alpha/premarket/should-trade`, `POST /v1/alpha/endgame/policy`.
  - premarket returns random gate decisions with configurable yes-probability range (`ALPHA_PREMARKET_YES_MIN`, `ALPHA_PREMARKET_YES_MAX`).
  - endgame returns randomized policy around base offsets/stale guard (`ALPHA_ENDGAME_BASE_OFFSETS_MS`, `ALPHA_ENDGAME_OFFSET_JITTER_MS`, `ALPHA_ENDGAME_SUBMIT_PROXY_MAX_AGE_BASE_MS`, `ALPHA_ENDGAME_SUBMIT_PROXY_MAX_AGE_JITTER_MS`).
  - optional proxy-wallet allowlist added via `ALPHA_ALLOWED_PROXY_WALLETS`; request wallet/header mismatch is rejected.

### 2026-03-12

- `evcurve_v1` + `sessionband_v1` alpha dependency is now strictly remote-only (`src/main.rs`):
  - removed local alpha fallback behavior for EVcurve checkpoint decisions (including chase/recheck path) and SessionBand decision evaluation; when remote alpha is missing or unavailable, strategy emits explicit remote-skip outcomes instead of local model decisions.
  - removed `EVPOLY_REMOTE_EVCURVE_ALPHA_ALLOW_LOCAL_FALLBACK` / `EVPOLY_REMOTE_SESSIONBAND_ALPHA_ALLOW_LOCAL_FALLBACK` runtime usage from remote alpha config path.
  - affects all configured symbols/timeframes for both strategies (`5m/15m/1h/4h`, plus EVcurve `1d`) by making signal generation dependent on remote alpha responses.

- `mm_rewards_v1` market selection filters extended with competition gate + keyword blacklist (`src/mm/mod.rs`, `src/main.rs`, `.env.full.example`):
  - added `EVPOLY_MM_COMPETITION_LEVEL_ALLOWLIST` (`low,medium,high,unknown`, plus `all/*`) to skip markets whose competition bucket is not allowed.
  - added `EVPOLY_MM_MARKET_BLACKLIST_KEYWORDS` (comma-separated, case-insensitive) to skip markets when slug/question/description matches blocked terms.
  - affects MM rewards candidate admission across all runtime modes (`auto`, `single`, `target`, `hybrid`) before quoting/entry.

- `remote market discovery` local fallback default changed to enabled (`src/main.rs`, `.env`, `.env.example`, `.env.full.example`, `scripts/remote_onboard.py`, `README.md`):
  - runtime code default for `EVPOLY_REMOTE_MARKET_DISCOVERY_ALLOW_LOCAL_FALLBACK` remains/now enforced as `true`.
  - onboarding helper now writes market and EVSnipe fallback flags independently, with market fallback defaulting to `true` unless API runtime config overrides it.
  - affects timeframe market discovery used by shared strategy loops (`premarket_v1`, `endgame_sweep_v1`, and `mm_rewards_v1` target/hybrid discovery path) by defaulting to local discovery fallback when remote timeframe discovery is configured but unavailable.

- `premarket_v1` active cap default increased for multi-asset ladder throughput (`.env`, `.env.full.example`):
  - `EVPOLY_PREMARKET_ACTIVE_CAP_PER_ASSET` changed from `24` to `100`.
  - Affects premarket worker admission gate across `5m/15m/1h/4h` by allowing larger concurrent per-asset ladder batches before `entry_invariant_blocked` suppression.

- `poly-oss` parity sync with `poly-remote` for live strategy behavior (`src/main.rs`, `src/mm/reward_scanner.rs`, `src/api.rs`, `src/bin/alpha_service.rs`, `src/bot_admin.rs`, `src/lib.rs`):
  - aligned remote decision wrappers for `evcurve_v1` and `sessionband_v1` so decision payloads and remote alpha response handling match poly-remote execution flow.
  - aligned MM rewards discovery path to rewards-API adaptive candidate pooling (`get_rewards_markets_api` + scanner-side shortlist enrichment), reducing heavy full-universe scans while keeping ranking parity behavior.
  - aligned order submit behavior for primary signer path with post-time retry/backoff and FAK/FOK notional precision guard on BUY market orders, affecting shared entry submit reliability across `premarket_v1`, `endgame_sweep_v1`, `evcurve_v1`, `sessionband_v1`, `evsnipe_v1`, and `mm_rewards_v1`.
  - aligned alpha service discovery to scheduler-driven cached snapshots (timeframe + evsnipe + MM rewards snapshots), making discovery endpoints cache-first instead of request-inline scans.
  - enabled `/bot/*` API surface in OSS runtime by including `bot_admin` module wiring in `lib.rs` and route handlers in `main.rs`.

- `order attribution` runtime signer endpoint is now env-driven with HTTPS default (`src/api.rs`, `.env.example`, `.env.full.example`):
  - primary `/order` signer default changed from hardcoded `http://45.76.35.46:8788/sign/order` to `https://signer.evplus.ai/sign/order`.
  - new env overrides:
    - `EVPOLY_ORDER_SIGNER_PRIMARY_URL` (default `https://signer.evplus.ai/sign/order`)
    - `EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN` (optional bearer token for primary signer only)
    - `EVPOLY_ORDER_SIGNER_FALLBACK_URL` (optional, default existing AWS `/sign/order`)
    - `EVPOLY_SUBMIT_SIGNER_URL` (optional, default existing AWS `/sign/submit`)
  - affects entry submit path behavior for all strategies that route through shared order posting (`premarket_v1`, `endgame_sweep_v1`, `evcurve_v1`, `sessionband_v1`, `evsnipe_v1`, `mm_rewards_v1`) by switching default primary signer transport to HTTPS and enabling optional primary signer auth without changing strategy decision logic.

- `remote discovery` onboarding/runtime defaults updated for VPS-hosted alpha endpoints (`scripts/remote_onboard.py`, `.env.example`, `.env.full.example`, `README.md`, `aws/lambda/evpoly-builder-signer/lambda_function.py`):
  - onboarding helper now auto-writes signer + discovery runtime keys (URLs/timeouts/fallback flags) into env files after `/onboard/finish`.
  - default discovery URLs now target `https://alpha.evplus.ai/v1/discovery/timeframe` and `https://alpha.evplus.ai/v1/discovery/evsnipe`.
  - default remote discovery mode now sets `EVPOLY_REMOTE_*_DISCOVERY_ALLOW_LOCAL_FALLBACK=false` in templates/onboard writes, making discovery VPS-dependent by default unless explicitly overridden.
  - onboarding API now supports optional runtime bootstrap payload (order signer URL, alpha base/discovery URLs, optional shared discovery/signer tokens) so users can onboard with fewer manual env edits.

### 2026-03-11

- strategy surface cleanup removed legacy strategy/runtime paths:
  - removed strategy implementations/modules and runtime loops for `reactive_v1`, `liqaccum_v1`, `snapback_v1_1`, and `endgame_revert_v1`.
  - removed associated entry execution modes, worker lanes, and strategy-specific submit/cancel/reporting branches in `src/main.rs`, `src/trader.rs`, `src/config.rs`, and `src/arbiter.rs`.
  - detached `endgame_sweep_v1` from reactive-bias dependency so it runs on deterministic retained inputs only.
  - removed learning/adaptive runtime loop wiring and retired removed-strategy env/config surfaces from `.env` and `.env.full.example`.
  - introduced neutral entry tuning keys (`EVPOLY_ENTRY_*`) for detector entry gates/buffers in place of removed reactive-prefixed keys.

- `redemption` `/positions` pre-scan reliability hardening (`src/api.rs`, `src/trader.rs`):
  - wallet redeemable-position fetch now retries with bounded exponential backoff before failing (`EVPOLY_WALLET_POSITIONS_RETRY_ATTEMPTS`, `EVPOLY_WALLET_POSITIONS_RETRY_BASE_MS`).
  - `/positions` response handling now reads bytes and includes bounded body preview in error context to preserve parse/transport diagnostics.
  - redemption pre-scan warning now logs the full anyhow chain and applies cooldown suppression (`EVPOLY_REDEMPTION_POSITIONS_WARNING_COOLDOWN_SEC`) to reduce repeated noise during transient upstream/API failures.
  - affects global redemption sweep maintenance (all strategy inventories routed through trader redemption logic).

- `manual_premarket_v1` + `manual_chase_limit_v1` runtime entry modules added via admin API (`src/main.rs`, `src/trader.rs`):
  - new admin endpoints:
    - `POST /admin/manual/premarket`
    - `POST /admin/manual/chase-limit`
  - both paths submit through `Trader::execute_limit_buy(...)` (same live order post path as other strategies), preserving remote signer builder attribution on order posts.

- `manual_premarket_v1` pre-open ladder behavior (manual trigger) now uses fixed `5m` cancel policy regardless of market domain (`src/main.rs`):
  - manual premarket resolves market by `condition_id` or `market_slug` and supports explicit token IDs for non-crypto binaries.
  - ladder submits use deterministic fixed-rung sizing (`build_premarket_fixed_rungs(...)`) and schedule staged cancels from `premarket_cancel_stages_for_timeframe(Timeframe::M5)`.
  - staged cancel now uses token-scope filtered cancel (`cancel_pending_orders_for_scope_filtered`) with optional price filters to avoid broad symbol-family cancels.

- `manual_chase_limit_v1` chase-limit runtime loop added (`src/main.rs`):
  - request requires `min_price`, `max_price`, `duration_min`, and `size_usd`.
  - chase repricing cadence defaults from global chase env (`AdaptiveChaseConfig::from_env()`), using `fallback_tick_ms` / `min_replace_interval_ms` unless `poll_ms` is provided.
  - default submit mode is post-only (`manual_chase_limit_v1`); normal maker-off mode is available via `manual_chase_limit_taker_v1`.

- post-only defaults for manual strategy IDs added in trader submit policy (`src/trader.rs`):
  - strategy IDs:
    - `manual_premarket_v1`, `manual_premarket_taker_v1`
    - `manual_chase_limit_v1`, `manual_chase_limit_taker_v1`
  - new strategy-scoped env aliases:
    - `EVPOLY_ENTRY_POST_ONLY_MANUAL_PREMARKET`
    - `EVPOLY_ENTRY_POST_ONLY_MANUAL_PREMARKET_TAKER`
    - `EVPOLY_ENTRY_POST_ONLY_MANUAL_CHASE`
    - `EVPOLY_ENTRY_POST_ONLY_MANUAL_CHASE_TAKER`

- `entry precheck` market-constraints and submit-scope load hardening (`src/api.rs`, `src/main.rs`, `src/tracking_db.rs`, `.env`, `.env.full.example`):
  - market-constraint fetch (`/markets/{condition_id}`) now uses readonly proxy-fallback path plus bounded retry/backoff (`EVPOLY_MARKET_CONSTRAINTS_RETRY_ATTEMPTS`, `EVPOLY_MARKET_CONSTRAINTS_RETRY_BASE_MS`) to reduce direct CLOB 429/noisy precheck failures.
  - arbiter submit-scope historical count DB lookup is now skipped when the submit cap is effectively disabled (`usize::MAX`, e.g. `mm_rewards_v1`), removing unnecessary per-request DB reads without changing cap behavior.
  - added targeted hot-path indexes for submit-scope and pending-order existence/lookups to reduce lock hold/wait under MM burst load.
  - affects entry execution/precheck stability across strategies, with largest runtime impact on `mm_rewards_v1`.

### 2026-03-10

- `market discovery` can now be routed to VPS-hosted remote endpoints with optional hard dependency gates (`src/main.rs`, `src/evsnipe.rs`, `.env.example`, `.env.full.example`):
  - timeframe market lookup path (`discover_market_for_timeframe_once`) now supports remote discovery for shared strategy loops that consume this path (`premarket_v1`, `endgame_sweep_v1`, `liqaccum_v1`, `mm_rewards_v1` helper lookups), with strict slug/open-ts validation before acceptance.
  - EVSnipe market-spec refresh now supports a remote spec source before local Gamma scanning, with the same optional local-fallback policy.
  - new env controls:
    - `EVPOLY_REMOTE_MARKET_DISCOVERY_URL`, `EVPOLY_REMOTE_MARKET_DISCOVERY_TOKEN`, `EVPOLY_REMOTE_MARKET_DISCOVERY_TIMEOUT_MS`, `EVPOLY_REMOTE_MARKET_DISCOVERY_ALLOW_LOCAL_FALLBACK`
    - `EVPOLY_REMOTE_EVSNIPE_DISCOVERY_URL`, `EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TOKEN`, `EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TIMEOUT_MS`, `EVPOLY_REMOTE_EVSNIPE_DISCOVERY_ALLOW_LOCAL_FALLBACK`
  - when `*_ALLOW_LOCAL_FALLBACK=false`, discovery becomes VPS-dependent and local fallback is blocked.

- `order attribution` signer routing now falls back to AWS only on timeout-class failures (`src/api.rs`):
  - single-order and batch-order post paths now keep primary errors local unless they are timeout-like.
  - market-order submit path now switches to AWS fallback only for timeout-class primary signer errors.
  - hardcoded signer endpoints are unchanged; this changes entry execution behavior for all live strategies by preventing non-timeout primary errors (for example 4xx no-fill) from auto-rerouting to AWS.

- `order attribution` fallback classifier broadened to transient infra errors while keeping rate limits on primary (`src/api.rs`):
  - AWS fallback now triggers on timeout/network/server-transient classes (timeouts, connection failures, 5xx).
  - `429`/rate-limit errors explicitly do not trigger AWS fallback; callers stay on primary and use existing backoff/retry flow.
  - affects all order submit paths (single, batch, market-order) and keeps business/validation 4xx local to primary.

- `redemption` adaptive trigger default is now enabled by default (`src/trader.rs`, `.env`, `.env.full.example`):
  - `EVPOLY_REDEMPTION_ADAPTIVE_ENABLE` fallback changed from `false` to `true` in code.
  - runtime/example env now explicitly set `EVPOLY_REDEMPTION_ADAPTIVE_ENABLE=true`.
  - affects global redemption sweep behavior across all strategies/timeframes by allowing balance-ratio and pending-count adaptive triggering without requiring explicit env opt-in.

### 2026-03-09

- `redemption` claim sweep tightened to strict API-only candidate gating (`src/trader.rs`):
  - DB restore path is now explicitly bypassed for redemption submit eligibility, even if restore env is set.
  - adaptive pending trigger now uses live API redeemable-condition count only.
  - pending-trade winner rows are redeemed only when their condition is currently present in API redeemable set; otherwise they stay pending with `waiting_api_redeemable_winner`.
  - losing rows remain `not_required` (no loser redeem submit).
  - affects all strategies routed through trader redemption maintenance.

- `redemption` API-direct submits now batch by condition (default chunk 10) to reduce relayer credit consumption (`src/trader.rs`, existing env):
  - API-direct winner claims now use `redeem_tokens_batch(...)` when `EVPOLY_REDEMPTION_BATCH_SUBMIT_ENABLE=true`.
  - batch chunk size is controlled by `EVPOLY_REDEMPTION_BATCH_CONDITIONS_PER_REQUEST` and capped by per-sweep max.
  - one-by-one direct redeem remains as fallback when batch submit is disabled.
  - telemetry now tags API-direct submits with `batch_submit` and `batch_size`.

- `redemption` + `merge` submit mode hard-pinned to batch (2026-03-09, `src/trader.rs`, `src/api.rs`):
  - redemption submit path is now batch-only (no one-by-one fallback) for both API-direct winners and pending winner queues.
  - redemption batch chunk is hardcoded to `10` conditions per relayer request.
  - added `merge_complete_sets_batch(...)` in API and switched merge sweeps/manual merge submit to batch-call path.
  - merge sweep now sends chunked relayer batch calls (hardcoded `10` conditions per request) and logs `batch_submit` / `batch_size`.

- `redemption` + `merge` sweep trigger policy updated to adaptive-first mode (`src/trader.rs`, `.env`):
  - added explicit schedule toggles:
    - `EVPOLY_REDEMPTION_SCHEDULE_ENABLE`
    - `EVPOLY_MERGE_SCHEDULE_ENABLE`
  - added adaptive trigger gates for both sweep paths:
    - pending-count threshold OR low available-balance ratio (`available_usdc / estimated_portfolio`)
    - cooldown-enforced run slots (`EVPOLY_*_ADAPTIVE_COOLDOWN_SEC`, set to 15m in runtime env).
  - runtime env now enables adaptive sweeps and disables hourly scheduled intervals by default.
  - merge adaptive trigger has a hard gate: it runs only when `EVPOLY_STRATEGY_MM_REWARDS_ENABLE=true`; manual/scheduled merge behavior remains unchanged.
  - affects global post-settlement ops flow (all symbols/timeframes) via trader redemption/merge maintenance loop.

- `redemption` claim path switched to API-first verification and winner-only submit focus (`src/trader.rs`, `.env`):
  - adaptive redemption pending trigger now uses in-memory unsold count (no DB candidate query dependency).
  - optional DB restore for redemption candidates is now gated by `EVPOLY_REDEMPTION_RESTORE_PENDING_FROM_DB_ENABLE` (default runtime `false`).
  - added API direct redeemable scan (`/positions`) pass before pending-trade loop; condition-level redeem submits now come from live `redeemable=true` wallet positions (winner-only path).
  - per-candidate flow now checks live API token balance first; zero-balance rows are cleared as `already_redeemed` before market-result/redeem logic.
  - submit path remains winner-only (`token_winner`); losing tokens are marked `not_required` and no redeem submit is attempted.
  - affects all strategy positions routed through trader redemption maintenance.

- `mm_rewards_v1` anti-pickoff upgrade for pump/dump conditions (`src/mm/fair_mid.rs`, `src/mm/metrics.rs`, `src/main.rs`, `src/mm/mod.rs`, `.env`, `.env.full.example`):
  - added new fair-mid module using microprice-style side reference and asymmetric speed limiter (`up_step_bps` slow, `down_step_bps` fast) to avoid over-chasing upward spikes.
  - wired fair-mid cap into normal ladder quoting so top BUY rung is clipped to a fair-mid-derived ceiling (`EVPOLY_MM_FAIR_MID_*`).
  - extended toxicity trigger with thin-depth detection (`latest top depth vs rolling p20`) and preserved existing cancel+pause+reentry flow.
  - added forced inventory-exit BUY floor gate: in forced counter-ladder mode, BUY quoting is blocked (and existing forced BUYs canceled) when top reference price is `<= EVPOLY_MM_FORCE_BUY_MIN_PRICE` (default `0.10`).
  - affects `mm_rewards_v1` across all MM symbols/timeframes using the shared MM runtime loop; `competition_high_freeze` behavior unchanged (still env-controlled and currently disabled in runtime config).

- `premarket_v1` + `mm_rewards_v1` now use true Polymarket `POST /orders` batch placement on the live submit path (`src/trader.rs`, `src/api.rs`):
  - added a trader-side micro-batcher that aggregates eligible order submits for a short flush window and submits them via one API batch call.
  - route scope:
    - `premarket_v1` ladder entries (`strategy_id=premarket_v1`, `entry_mode=ladder`)
    - `mm_rewards_v1` entries (`strategy_id=mm_rewards_v1`, `entry_mode=mm_rewards/ladder`)
  - non-eligible strategies keep the existing single-order submit path unchanged.
  - batch worker includes safe single-order fallback if batch call fails at transport level.
  - per-order success/failure from batch response is preserved and propagated back to execution logic.
  - new env controls:
    - `EVPOLY_BATCH_PLACE_ENABLE` (default `true`)
    - `EVPOLY_BATCH_PLACE_MAX_ORDERS` (default `10`, capped to API max `15`)
    - `EVPOLY_BATCH_PLACE_FLUSH_MS` (default `8`)
    - `EVPOLY_BATCH_PLACE_QUEUE_CAP` (default `2048`)

### 2026-03-08

- `evcurve_v1` + `sessionband_v1` 15m Coinbase base-anchor source changed to trade carry-forward with sequence-gap guard (`src/coinbase_ws.rs`, `src/main.rs`):
  - Coinbase WS trade parsing now captures exchange-time/sequence fields and stores rolling 15m boundary anchors (`period_anchors_15m`) derived as carry-forward base (`last trade <= period open`).
  - 15m anchor selection in both strategy loops now prefers this trade carry-forward anchor (`ws_trade_carry_forward`) before falling back to previous WS-mid exact-open path.
  - Added WS mid carry-forward fallback (`ws_mid_carry_forward`): the latest healthy mid observed before period boundary is carried to the next 15m open when trade-carry anchor is unavailable.
  - If a sequence gap is detected around boundary crossing, 15m anchor is hard-skipped with `skip_reason=sequence_gap_around_open` to avoid polluted base.
  - When WS carry-forward/exact anchor is unavailable, 15m now allows existing REST exact-open fallback path (`rest_exact`) instead of immediate hard-skip.
  - Affects: all four symbols on `15m` for `evcurve_v1` and `sessionband_v1`; improves exact-open robustness during Coinbase WS lag/reconnect bursts.

- `evcurve_v1` + `sessionband_v1` exact 15m base-anchor hardening:
  - Updated `src/main.rs` period base capture path to keep the first proxy tick seen after period open in-memory per `(symbol,timeframe,period_open_ts)`.
  - Added DB `upsert_evcurve_period_base(...)` writes from first-seen WS exact anchors (within `EVPOLY_PROXY_BASE_EXACT_MAX_DELAY_MS`) so later loops/restarts can reuse exact anchors.
  - For `15m`, removed REST candle fallback for base anchoring: if exact DB/WS anchor is unavailable, strategy now skips with `exact_proxy_base_unavailable_ws_or_db` instead of accepting delayed/empty REST.
  - Affects: `evcurve_v1` and `sessionband_v1` 15m checkpoint eligibility and anchor correctness; key path `src/main.rs` base-anchor init blocks in each loop.

- H1 period-mismatch guard fixed for non-BTC symbols:
  - Updated `src/main.rs` `h1_market_slug_matches_target_open_ts(...)` to handle symbol-aware hour slugs (`bitcoin/ethereum/solana/xrp`) instead of BTC-only expected slug logic.
  - Added compatibility for timestamp-style 1h slugs (`*-updown-1h-<open_ts>`) when present.
  - Added/updated tests in `src/main.rs` (`h1_mismatch_blocks_submit_and_emits_event`) to assert ETH + timestamp-style 1h slugs pass when period matches.
  - Affects: all strategies using `h1_market_period_mismatch_for_trade(...)` (`evcurve_v1`, `endgame_sweep_v1`, `sessionband_v1`, and other H1 discovery-gated paths).
- `mm_rewards_v1` weak-exit loss-floor basis TTL refresh (`src/main.rs`, `src/mm/mod.rs`, `src/tracking_db.rs`, `.env`, `.env.full.example`):
  - added `EVPOLY_MM_WEAK_EXIT_ENTRY_BASIS_TTL_SEC` (default `7200`) to age out stale entry basis used by weak-exit loss-floor pricing.
  - weak-exit now resolves entry basis in this order:
    - all-time BUY-fill basis while age <= TTL,
    - recent BUY-fill basis inside TTL window when stale,
    - live mark fallback (`best_bid/best_ask`) when stale and no recent fills.
  - affects `mm_rewards_v1` inventory-exit/weak-exit SELL pricing across all symbols/timeframes/modes; prevents old high-cost basis from pinning exit ladders far above current market for long-held inventory.
  - added runtime telemetry fields on weak-exit events: `entry_basis_source`, `entry_basis_age_ms`, `entry_basis_ttl_ms`, `entry_basis_ttl_expired`.
- `mm_rewards_v1` weak-exit basis rolling-window precedence fix (`src/main.rs`):
  - basis resolution now prefers rolling-window BUY-fill basis (last TTL window) before all-time basis.
  - prevents fresh fills from re-pinning exits to stale high all-time basis when recent-window basis exists.
  - keeps stale all-time fallback behavior unchanged (`mark_fallback_after_ttl` / `stale_all_time_basis`) when no rolling-window basis is available.
  - affects weak-exit SELL pricing in long-held inventory markets.
- `premarket_v1` dispatch and expiry safety update (`src/main.rs`, `src/trader.rs`, `.env`):
  - premarket arbiter dispatch now uses true per-market scope lanes (`timeframe + period + condition`) with dedicated channel queues and lifecycle GC (`EVPOLY_PREMARKET_SCOPE_LANE_ENABLE`, `EVPOLY_PREMARKET_MAX_SCOPE_LANES`, `EVPOLY_PREMARKET_SCOPE_LANE_IDLE_TTL_SEC`, `EVPOLY_PREMARKET_SCOPE_LANE_QUEUE_CAP`).
  - each scope lane forwards to execution workers independently, so one bursty premarket market no longer blocks the enqueue path for other premarket markets.
  - premarket limit `expiration_ts` is now capped by launch window (`period_open_ts + EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_*`) so late submits cannot remain live far after market open.
  - premarket cancel stage scheduling is now env-driven for post-open offsets and pre-open trims (`EVPOLY_PREMARKET_CANCEL_AFTER_OPEN_*`, `EVPOLY_PREMARKET_CANCEL_PREOPEN_TRIM_*_OFFSET_SEC`); `15m` post-open cancel window changed to `+40s`.
  - non-premarket ladder/reacive paths are unchanged by this expiry cap.
- `evsnipe_v1` hit execution upgraded to two-leg trigger flow (`src/main.rs`, `src/evsnipe.rs`, `.env`):
  - pre-hit leg: if Binance trade enters `EVPOLY_EVSNIPE_PRE_TRIGGER_BPS` band (default `1bps`) before strike, submit `EVPOLY_EVSNIPE_SIZE_USD * EVPOLY_EVSNIPE_PRE_LEG_RATIO` (default `30%`).
  - confirm leg: on actual hit-cross, submit remaining signal size (`total - already reserved`) so one signal totals `EVPOLY_EVSNIPE_SIZE_USD` (default now `200 USD`).
  - leg dedupe is now per condition+leg (`pre`/`confirm`) so one market can do at most one pre-leg and one confirm-leg.
  - submit cap remains hardcoded `0.99` for hit path, with existing cross-ask level selection.
  - close/range EVSnipe path remains unchanged.
- `evsnipe_v1` execution isolation + enqueue behavior update (`src/main.rs`):
  - EVSnipe now has its own arbiter execution lane/worker pool (`arbiter_exec_tx_evsnipe`, `EVPOLY_ENTRY_WORKER_COUNT_EVSNIPE`) instead of sharing the normal lane.
  - EVSnipe enqueue is now single-shot (no retry/backoff loop) for both hit and close/range submits, reducing avoidable enqueue latency under burst conditions.
- Runtime memory safety cleanup for `evsnipe_v1` + `mm_rewards_v1` (`src/main.rs`):
  - `evsnipe_v1`: added bounded runtime-state pruning with expiry tracking for condition/leg dedupe sets (`fired_conditions`, `fired_hit_legs`, `reserved_usd_by_condition`) plus periodic telemetry (`evsnipe_state_pruned`, `evsnipe_state_sizes`).
  - `mm_rewards_v1`: fill dedupe keys are now TTL/cap bounded (`mm_fill_seen_event_keys`), rolling action/cancel/fill maps now prune empty keys, and periodic map-size telemetry was added (`mm_rewards_state_sizes`).
  - Removed hard-dead runtime branches by replacing `if false` loop blocks with explicit legacy feature flags:
    - `EVPOLY_MM_LEGACY_ACTIVITY_SYNC_IN_LOOP`
    - `EVPOLY_MM_LEGACY_COMP_REFRESH_IN_LOOP`
  - Affects runtime stability/observability only; no intentional change to strategy entry/exit/sizing decisions.
- `mm_rewards_v1` arbiter budget leak fix for request-id collisions (`src/main.rs`):
  - MM quote `request_id` now includes `condition_id`, `token_id`, and an atomic nonce, eliminating same-millisecond cross-market ID reuse for identical mode/symbol/timeframe/period/side/rung tuples.
  - `ArbiterBudgetLedger::reserve` now detects duplicate `request_id` keys and releases the prior reservation before applying the new one, with `arbiter_budget_request_id_collision_replaced` telemetry.
  - This prevents reservation overwrite/leak behavior that could pin effective arbiter remaining budget near zero and cascade into `mm_floor_min_not_met` BUY starvation.
- `mm_rewards_v1` reward-min target multiplier for true floor scaling (`src/mm/mod.rs`, `src/main.rs`, `src/mm/reward_scanner.rs`, `.env.full.example`):
  - added `EVPOLY_MM_REWARD_MIN_TARGET_MULT` (default `1.0`) to scale computed reward-min shares targets after floor policy (`per_rung`/`reference_price`/`hybrid`) and before cap.
  - MM rung sizing and local scoring min-size checks now apply this multiplier consistently, so setting `EVPOLY_MM_REWARD_MIN_TARGET_MULT=2.0` targets 2x reward-min sizing across MM rung computations.
  - MM auto candidate reward-floor share estimates now use the same multiplier, keeping market ranking expectations aligned with live rung sizing.
  - startup MM config telemetry now prints `reward_min_target_mult` for runtime verification.
  - affects `mm_rewards_v1` across all enabled modes/symbols/timeframes (`quiet_stack`, `tail_biased`, `spike`, `sports_pregame`).

### 2026-03-07
- `evcurve_v1` + `sessionband_v1` proxy-base anchoring is now strict exact-open with REST fallback (`src/main.rs`, `src/api.rs`, `src/tracking_db.rs`):
  - period base is accepted only when timestamp is exact-open (delay `<= EVPOLY_PROXY_BASE_EXACT_MAX_DELAY_MS`, default `0ms`).
  - late DB anchors are rejected; late WS anchors are rejected.
  - when exact WS/DB anchor is unavailable, strategy now fetches exact-open from proxy REST:
    - Binance for `1h`
    - Coinbase for other active tfs in these strategies.
  - if REST cannot provide exact-open base, that period is hard-skipped (`*_base_anchor_exact_skip`), no late proxy fallback trading.
  - base persistence now uses overwrite (`set_evcurve_period_base`) so exact REST anchor can replace previously polluted late rows.
  - Coinbase REST exact-open fallback now sends explicit `User-Agent` header to satisfy Coinbase candle endpoint requirements.
- `mm_rewards_v1` forced-inventory deep-bid inside offset (`src/main.rs`, `src/mm/mod.rs`, `.env`):
  - forced inventory two-way deep bid anchor now applies an inside-border offset (`EVPOLY_MM_FORCED_EXIT_DEEP_BID_INSIDE_OFFSET_CENTS`, default `1.0`) so bids sit 1c inside the rewards spread border instead of exactly on the outer edge.
  - anchor formula changed from `best_bid - reward_range_width` to `best_bid - reward_range_width + inside_offset`, still clamped below best bid by at least one tick.
  - added telemetry fields in `mm_rewards_forced_exit_opposite_deep_ladder`: `inside_offset_cents` and `inside_offset_width`.
  - affects `mm_rewards_v1` inventory-exit two-way quote path for inventory-heavy markets (e.g., `MISC/1d` exit ladders).
- Order reconcile + PM WS resilience update (`src/trader.rs`, `src/tracking_db.rs`, `src/polymarket_ws.rs`, `.env`):
  - periodic exchange reconcile now prioritizes active pending orders and retries only a bounded stale/reconcile-failed subset with age/backoff gates:
    - `EVPOLY_ORDER_RECONCILE_RETRY_MAX_ROWS`
    - `EVPOLY_ORDER_RECONCILE_RETRY_MIN_AGE_MS`
    - `EVPOLY_ORDER_RECONCILE_RETRY_MAX_PERIOD_AGE_SEC`
  - repeated status rewrites are suppressed (`pending_orders` status update now no-ops when status is unchanged), reducing DB churn and repeated reconcile loops.
  - known order-lookup misses in periodic/startup/stale-cancel reconciles were downgraded from warn-level to debug-level and keep terminal vs transient classification.
  - PM market WS reconnect now uses unstable-session aware backoff + jitter (`EVPOLY_PM_WS_MARKET_UNSTABLE_WINDOW_SEC`, `EVPOLY_PM_WS_MARKET_RECONNECT_JITTER_MS`) to reduce shard reconnect churn.
  - runtime defaults updated in `.env` (including `EVPOLY_PM_WS_MARKET_SHARDS=4`) for lower per-shard WS lag pressure.
- `mm_rewards_v1` inventory two-way precedence + auto-deselect protection (`src/main.rs`):
  - inventory-forced conditions now take execution precedence over auto/target work items in the per-loop work queue, so inventory-exit behavior is not silently overridden by auto selection.
  - auto-deselect cleanup now skips `cancel_pending_orders_for_strategy_condition` for conditions currently tracked by the inventory janitor, preventing accidental cancellation of active inventory-exit ladders.
  - any market with live inventory and active imbalance now forces inventory-exit mode (even when sourced from auto), keeping two-way inventory exit logic active instead of falling back to heavy-side BUY cancellation.
  - affected path: `mm_rewards_v1` loop orchestration for inventory-bearing markets (notably `MISC/1d` auto + inventory overlap cases).
- `mm_rewards_v1` hourly dust inventory auto-sell (`src/trader.rs`, `src/main.rs`, `scripts/wallet_history_sync.py`, `.env`):
  - added new admin endpoint `POST /admin/mm/dust-sell` that scans MM wallet inventory and submits `SELL` `FAK` market orders for dust-sized positions.
  - dust sweep now forces a fresh CLOB auth before submitting market orders to avoid silent stale-session submit failures during background hourly runs.
  - dust candidates are constrained by wallet size + notional caps and still respect exchange min-order constraints before submit; unsellable below-exchange-min dust is skipped with explicit reasons.
  - wallet sync worker now triggers dust sell once per run (hourly tmux loop), and records dust sell status/candidate/submitted metrics in `wallet_sync_runs_v1`.
  - wallet sync mark persistence now computes book marks first and writes unrealized rows in batched DB statements (instead of per-row write during network I/O), reducing SQLite writer-lock hold time and lock contention with live trading writes.
  - key env controls: `EVPOLY_MM_DUST_SELL_ENABLE`, `EVPOLY_MM_DUST_SELL_MAX_NOTIONAL_USD`, `EVPOLY_MM_DUST_SELL_MAX_SHARES`, `EVPOLY_MM_DUST_SELL_SCAN_LIMIT`, `EVPOLY_MM_DUST_SELL_MAX_ORDERS`, `EVPOLY_MM_DUST_SELL_TIMEOUT_SEC`.
  - runtime env defaults enabled in `.env`: `ENABLE=true`, `MAX_NOTIONAL_USD=30`, `MAX_SHARES=250`, `SCAN_LIMIT=4000`, `MAX_ORDERS=20`, `TIMEOUT_SEC=45`.
- `premarket_v1` arbiter dedupe now preserves multi-rung ladder submits (`src/arbiter.rs`):
  - exact-key dedupe in `Arbiter::resolve_conflicts` now includes a premarket-only variant key derived from rung price (`max_price`), so same token/side rungs do not collapse into one intent.
  - non-premarket strategies keep prior exact-key dedupe behavior unchanged.
  - added tests:
    - `premarket_exact_dedupe_keeps_distinct_rungs`
    - `non_premarket_exact_dedupe_still_blocks_duplicates`
  - impact: prevents `arbiter_dispatch_rejected reason=dedupe_exact_key_lost` from dropping most premarket ladder rungs in a batch.
- `mm_rewards_v1` arbiter floor-aware reservation clamp + MM budget uplift (`src/main.rs`, `.env`):
  - raised `EVPOLY_ARB_GLOBAL_MAX_USD` from `14400` to `100000`.
  - arbiter dispatch now treats MM rung `target_size_usd` as a minimum reservation floor and attempts reservation with `max(arbiter_alloc, mm_floor)`.
  - if that floor cannot be reserved, dispatch now drops with `arbiter_dispatch_budget_exhausted` (`reason=mm_floor_min_not_met`) instead of forwarding an undersized MM order.
  - affects `mm_rewards_v1` auto-mode quote flow (primarily `MISC/1d`) by preventing repeated `ENTRY_FAILED precheck:mm_rewards_notional_floor_exceeds_budget` loops from under-allocated intents.
- `mm_rewards_v1` active-sell spread-capture + inventory-exit hardening (`src/main.rs`, `src/mm/mod.rs`, `src/tracking_db.rs`, `src/trader.rs`):
  - added force-exit stale-balance fallback controls (`EVPOLY_MM_WEAK_EXIT_STALE_FALLBACK_*`) so weak-exit SELLs can continue using capped/haircut cached balance instead of hard-stopping on transient live-balance misses.
  - forced-exit ladder now keeps minimum live SELL coverage (`EVPOLY_MM_WEAK_EXIT_FORCE_MIN_ACTIVE_RUNGS`) and avoids canceling below that floor to reduce cancel-flap and no-order gaps.
  - spread-capture mode now anchors forced weak-exit ladder at/above top ask by default (`EVPOLY_MM_WEAK_EXIT_SPREAD_CAPTURE_ENABLE`), then ladders upward by ticks.
  - added inventory-only mode activation for dead-capital scopes (forced inventory scope or low-reward + inventory), blocking new BUYs and running exit-only flow until inventory is reduced.
  - `/admin/mm/status` now exposes new live KPIs: `sell_to_buy_fill_usd_pct_24h`, `markets_with_open_sells_when_weak_inventory_gt0`, and weak-exit stale-skip rates (`3h`/`24h`) from event telemetry.
- `mm_rewards_v1` weak-exit floor fix for dead-capital drains (`src/main.rs`):
  - weak-exit SELL min-size checks now use exchange minimum-order notional converted to shares (`minimum_order_size/price`) instead of reward `rewards.min_size` shares.
  - this prevents forced inventory exits from stalling on reward floor values (for example 100-200 share reward targets) when inventory is below that threshold.
  - applied across competition-high exits, dynamic imbalance ladders, and normal weak-side exits; added extra telemetry context (`reward_min_size_shares_hint`) to verify the floor source at runtime.
  - force-exit balance sourcing now treats zero/empty live sellable+balance reads as stale when cached inventory is positive, so fallback sizing can still post exit SELLs (`mm_rewards_weak_exit_live_zero_override`) instead of repeatedly skipping with `effective_weak_inventory_shares=0`.
  - weak-exit source selection now filters non-positive live values before prioritization, so `live_sellable=0` cannot shadow a positive live/cached inventory path.
  - in force-exit mode, weak-exit now selects the maximum positive candidate among `live_sellable`, `live_balance`, and `cached_haircut` (instead of first-hit precedence), preventing tiny/zero live reads from suppressing valid exit sizing.
- `endgame_sweep_v1` and `sessionband_v1` now enforce submit-time proxy freshness in arbiter execution (`src/main.rs`):
  - added request-carried proxy timestamp (`proxy_update_ts_ms`) and decision-age telemetry.
  - submit is hard-blocked when proxy age at submit exceeds configured max (defaults `500ms`):
    - `EVPOLY_ENDGAME_SUBMIT_PROXY_MAX_AGE_MS`
    - `EVPOLY_SESSIONBAND_SUBMIT_PROXY_MAX_AGE_MS`
  - blocked submits emit `entry_submit_proxy_stale_blocked` and do not call trader POST.
- `sessionband_v1` now enforces decision-to-submit timing gap guard (`src/main.rs`):
  - submit is hard-blocked when `decision_to_submit_ms` exceeds default `500ms` via `EVPOLY_SESSIONBAND_MAX_DECISION_TO_SUBMIT_MS`.
  - blocked submits emit `sessionband_timing_gap_blocked`.
- timing telemetry for all entry paths now includes proxy fields (`src/main.rs`):
  - `proxy_update_ts_ms`, `proxy_age_at_decision_ms`, `proxy_age_at_submit_ms` in `entry_execution_timing`.
- execution fast-lane update for `endgame_sweep_v1` + `sessionband_v1` (`src/main.rs`, `.env`):
  - submit path for these two strategies now executes inline in worker (no spawned handoff), and bypasses submit semaphore waiting entirely.
  - worker hot-path DB checks were removed for these two strategies: active-trade count and submit-scope DB cap check are skipped in fast lane.
  - `sessionband_v1` enqueue retry loop (150ms backoff retry) was removed; enqueue is now single-shot fire-and-forget.
  - added per-pool submit concurrency runtime settings:
    - `EVPOLY_ENTRY_SUBMIT_CONCURRENCY_ENDGAME=16`
    - `EVPOLY_ENTRY_SUBMIT_CONCURRENCY_SESSIONBAND=16`

### 2026-03-06
- `evsnipe_v1` execution cap behavior changed to fixed submit limit `0.99` for both hit and close/range paths (`src/main.rs`, `src/evsnipe.rs`):
  - removed ask<=max-price gating from EVSnipe ask-level selection; EVSnipe now always selects a cross level from live asks when orderbook is present.
  - EVSnipe requests now submit with hardcoded `max_price=0.99` and `bid_price=0.99` (no dynamic max-buy cap from ask selection).
  - skip reason on empty/invalid ask books is now `evsnipe_skip_book_empty` instead of `evsnipe_skip_ask_above_cap`.
  - affects: `evsnipe_v1` reaction tracking and submission behavior for all supported market families.

### 2026-03-06
- `mm_rewards_v1` startup stale-order reset + runtime stale-order hardening (`src/main.rs`, `src/trader.rs`, `src/tracking_db.rs`, `src/mm/mod.rs`):
  - startup `cancel_all_orders()` is now hard-forced for MM restarts instead of honoring `EVPOLY_STARTUP_CANCEL_ALL_SKIP_IF_MM_REWARDS`.
  - after a successful startup cancel-all, MM now purges local active `pending_orders` rows for `mm_rewards_v1` and clears stale `mm_market_states_v1` rows before startup restore/reconcile trusts local state.
  - generic exchange pending-order reconcile now retries rows previously marked `RECONCILE_FAILED` and converts terminal-like order lookup failures (`404` / `not found` / unknown order) into `STALE` instead of leaving them stuck.
  - MM runtime stale-order full sweep is now enabled by default (`EVPOLY_MM_STALE_PERIODIC_FULL_SWEEP_SEC=60`) with a larger default lookup cap (`EVPOLY_MM_STALE_PERIODIC_FULL_SWEEP_MAX_LOOKUPS=2000`).
  - wallet-backed inventory janitor can now bootstrap `market_slug` from `wallet_positions_live_latest_v1` and defaults wallet-only forced markets to `MISC/1d` when old MM state rows were intentionally purged on startup.
  - MM single-market selector discovery now resolves `event_slug/market_slug` selectors by direct `market_slug` lookup first, avoiding current Gamma event-slug `422` failures from blocking restart recovery.
  - startup tracking DB maintenance passes (`strategy_feature_snapshots` backfill, replay prune, missing `ENTRY_FILL` backfill, settlement PnL reconcile) now run in a background task after `bot_start` and market discovery, instead of blocking MM startup.
  - `auto` mode now sends a fast bootstrap candidate snapshot from rewards-page / Gamma rows before waiting on the slow ranked scan, so restart recovery does not sit empty waiting on the full ranking pass.
  - weak-exit SELL sizing now uses live sellable shares (`min(balance, allowance)`) instead of balance alone, and still refuses to stack a new normal weak exit on top of existing open SELL rows for the same token/scope.
  - merge sweep candidate discovery now seeds directly from raw wallet `mergeable=1` positions in `wallet_positions_live_latest_v1`, not only the MM-scoped wallet inventory view.
  - affects: `mm_rewards_v1` startup stale-order cleanup, stale market-state carryover across restarts, and runtime stale-order convergence while the bot is running.

### 2026-03-06
- `evsnipe_v1` expanded from hit-only to hit + close/range execution (`src/evsnipe.rs`, `src/main.rs`):
  - added rule parser support for `close_above`, `close_below`, and `close_between` markets (e.g. `ethereum-above-*`, `ethereum-price-*` families) while keeping existing hit-rule support.
  - added Binance 1m kline-close stream ingest and dedicated close-trigger submit path.
  - close/range path now selects YES/NO token based on resolved close condition and submits through existing arbiter/trader FAK execution.
  - added boundary-ambiguity and missing-NO-token skip telemetry for close/range cases.
  - EVSnipe max buy is now fixed at `0.99` in code for all EVSnipe market families (same value used by hit path).
  - affects: `evsnipe_v1` for BTC/ETH/SOL/XRP D1-style price-rule markets.

### 2026-03-06
- `mm_rewards_v1` live status truth + preflight/watchdog/exit hardening (`src/tracking_db.rs`, `src/trader.rs`, `src/mm/scoring_guard.rs`, `src/main.rs`, `scripts/mm_balance_merge_guard.sh`):
  - `/admin/mm/status` now derives active order counts and side inventory from live wallet/pending-order reads instead of trusting `mm_market_states_v1` snapshot counts as truth.
  - MM scoring guard now uses same-side BBO anchors in the second-stage per-rung validator, matching the earlier ladder-capacity filter.
  - sparse markets can keep partial active ladders with a single valid level when reward-eligible, instead of requiring the full pair-depth minimum before staying active.
  - preflight blockers now keep their real reason (`non_scoring`, `full_ladder_required`, `shares_over_cap`, `below_order_floor`) instead of collapsing everything into `non_scoring_rebalance_only`.
  - watchdog now moves empty-ladder markets into `watchdog` state instead of leaving them as `quoting`.
  - weak-exit SELL sizing now requires fresh live balance, uses DB open SELL rows for reserved inventory, and refreshes sell allowance proactively before submit.
  - tracked low-USDC merge guard default in `scripts/mm_balance_merge_guard.sh` is now `300` instead of relying on a local ignored env override.
  - affects: `mm_rewards_v1` runtime market-state telemetry, ladder persistence policy, weak-exit safety, and local merge-guard ops behavior.

### 2026-03-06
- `mm_rewards_v1` wallet sync worker exact-snapshot + auto-reconcile hardening (`scripts/wallet_history_sync.py`):
  - `wallet_positions_live_latest_v1` is now replaced per sync run for the configured wallet, so positions that disappeared from the live wallet are pruned instead of lingering as stale latest rows.
  - after each successful wallet snapshot/activity/mark refresh, the same worker now immediately calls `/admin/mm/reconcile/inventory` and records reconcile status, repaired row count, repaired shares, and reconcile errors in `wallet_sync_runs_v1`.
  - effect: the tmux wallet sync worker now performs actual live-wallet-to-DB convergence for `mm_rewards_v1` inventory instead of only refreshing staging tables.

### 2026-03-06
- `mm_rewards_v1` merge-accounting, wallet-backed reporting, and toxic re-entry implementation (`src/tracking_db.rs`, `src/trader.rs`, `src/main.rs`, `src/mm/mod.rs`, `scripts/wallet_history_sync.py`, `scripts/mm_inventory_reconcile_worker.sh`, `README.md`, `docs/mm-runtime-runbook.md`):
  - merge success now consumes MM inventory in persistent accounting instead of leaving `positions_v2` stale after successful merges.
  - `positions_v2` open-inventory readers now subtract `inventory_consumed_units`, and wallet-sync unrealized marks honor that same net-unit definition.
  - added inventory-consumption audit tables / drift snapshot plumbing and exposed wallet-backed MM attribution plus per-token drift in `/admin/mm/status`.
  - added `/admin/mm/reconcile/inventory` plus a separate reconcile worker script for periodic wallet-vs-DB repair in both directions:
    - stale excess DB inventory is consumed,
    - missing DB inventory is added back through synthetic `ENTRY_FILL_RECONCILED` accounting.
  - inventory janitor now prefers wallet-backed MM inventory when wallet snapshot tables are available, with DB fallback retained for safety.
  - MM quote loop gained toxic re-entry controls:
    - side-toxic and adverse-BBO triggers now arm hold/probe re-entry state,
    - probe mode deepens/reduces the ladder instead of immediately rejoining old top levels,
    - quote submits now tag `reward_stub` vs `capture` rung buckets,
    - PM last-trade snapshots are consumed as an additional toxic sweep signal.
  - added env/config defaults for toxic re-entry, reward stub, and fee-enabled dynamic checks in `MmRewardsConfig`.
  - runtime sizing change in `.env`:
    - `EVPOLY_MM_QUOTE_SHARES_PER_SIDE` changed from `200` to `1`
    - `EVPOLY_MM_REFILL_SHARES_PER_FILL` changed from `200` to `1`
    - effect: active MM ladder sizing now falls through to the live market reward-min floor instead of using a fixed `200` share base.
  - startup/runtime ops hardening:
    - admin API startup was moved earlier so local MM reconcile tooling can reach the bot during heavy startup bootstrap instead of waiting for later restore phases,
    - reconcile worker now logs explicit `admin_unavailable` warnings on startup races,
    - wallet-under-DB reconcile add-path is now idempotent on repeated runs.
  - merge sweep behavior change:
    - merge candidate discovery now unions in-memory MM pending inventory with wallet-backed inventory rows, so sweep can see mergeable complete sets beyond the current pending-trade set.
    - `EVPOLY_MERGE_MAX_CONDITIONS_PER_SWEEP=0` is now treated as unlimited, and the code default was changed from `10` to `0`.
    - effect: one sweep pass can submit all discovered mergeable conditions instead of truncating to the top 10 by pair size.
  - local ops env change in `.env`:
    - `EVPOLY_BALANCE_GUARD_MIN_USDC` reduced from `1000` to `300`.
    - effect: low-USDC merge-guard manual sweeps now trigger only when available USDC drops below `300`.
  - MM competition fallback hardening (`src/main.rs`):
    - when auto-candidate competition and rewards-page competition caches are both missing, MM now falls back to `market.competitive` from the live market payload instead of leaving competition as `unknown`.
    - effect: forced/janitor markets are less likely to quote with `competition_level=unknown`.
  - MM scoring anchor change (`src/main.rs`):
    - local scoring ladder filtering now uses live same-side top of book (`best_bid`, fallback `best_ask`) as the score anchor before falling back to midpoint geometry.
    - effect: a side priced at `79c` with reward spread `3c` and extend `2c` is treated as scoreable down to about `74c`, instead of being forced through midpoint-equivalent spread logic that can over-reject deep-but-still-valid same-side quotes.

### 2026-03-06
- `evcurve_v1` default chase anchor-stop hardening (`src/main.rs`):
  - EVcurve chase now records a fixed PM anchor price at trigger start and cancels all open BUY chase orders if the PM price falls `0.20` below that initial anchor.
  - This is default code behavior, not env-gated.
  - Effect example: if chase starts from `0.72`, the chase is hard-stopped at `0.52` or lower instead of continuing to reprice/cancel indefinitely.

### 2026-03-06
- `mm_rewards_v1` toxic-flow fair-reentry design note recorded in `mm-update.md` after rereading merged `Poly-builder` code (`0ac75c4`):
  - documented target behavior for LP-reward farming + spread capture under toxic flow:
    - immediate toxic cancel,
    - hold/recovery window,
    - deeper fair-value re-entry ladder instead of instant top-of-book rejoin,
    - split `reward_stub` vs deeper `capture_ladder`.
  - documented dynamic fee-enabled detection requirement from live market metadata / `/fee-rate` instead of hardcoded timeframe assumptions.
  - identified intended runtime touchpoints for later implementation:
    - side-toxic cancel branch,
    - ladder deepen / size-shaping path,
    - selective ladder reprice / offboard cooldown path in `src/main.rs`.
  - scope: `mm_rewards_v1` across reward markets/timeframes; documentation-only strategy-plan update, no runtime code change yet.

### 2026-03-06
- Shared execution/risk plumbing hardening across live entry strategies (`src/main.rs`, `src/arbiter.rs`, `src/tracking_db.rs`, `src/monitor.rs`, `scripts/rollout_gate.py`):
  - arbiter conflict resolution now preserves input order, which allows dispatcher-side batch arbitration to map results back to the original concurrent requests safely.
  - arbiter dispatcher now batches ready requests and applies admission before worker fanout, instead of re-arbitrating each request in isolation inside workers.
  - added shared in-memory arbiter budget reservations across the live dispatch stream so global / per-strategy / per-timeframe caps are enforced across concurrently running strategies, not only within a single one-request arbiter call.
  - worker execution now consumes dispatcher-approved allocation directly and releases reserved arbiter budget on exit.
  - premarket dispatcher drop-on-full is now explicit (`EVPOLY_PREMARKET_DISPATCH_DROP_IF_FULL`, default `false`) instead of default behavior.
  - monitor callback drop-on-backpressure is now explicit (`EVPOLY_MONITOR_DROP_CALLBACK_ON_BACKPRESSURE`, default `false`) instead of default snapshot loss.
  - affects all runtime-routed entry strategies using `ArbiterExecutionRequest`, including `premarket_v1`, `reactive_v1`, `liqaccum_v1`, `snapback_v1_1`, `endgame_sweep_v1`, `endgame_revert_v1`, `evcurve_v1`, `sessionband_v1`, `evsnipe_v1`, and `mm_rewards_v1`.
- Tracking/risk-gate fixes for fill-aware strategies (`src/tracking_db.rs`, `src/main.rs`, `scripts/rollout_gate.py`):
  - fill-count helpers now read deduped entry-fill state from `fills_v2` instead of broken `trade_events.order_id` SQL.
  - EVcurve and MM fill-cap guards no longer fail open on fill-count query errors.
  - avg entry-price helpers now use deduped fill rows, preventing canonical + reconciled double counting.
  - `pending_orders` now carries `filled_at_ms`, and rollout fill parity prefers `filled_at_ms` plus `fills_v2` when present instead of treating `updated_at_ms` as fill time.
  - active pending-order status handling is normalized to include both `PLACED` and legacy `LIVE`.
- Follow-up runtime/ops hardening for local dirty stack promotion (`ev`, `src/evsnipe.rs`, `src/polymarket_ws.rs`, `src/entry_idempotency.rs`, `src/strategy_decider.rs`, `src/mm/mod.rs`):
  - wallet sync sidecar management was wired into `./ev`, but live-mode scoping is now explicit so `./ev start dry` does not auto-start the wallet sync worker.
  - `evsnipe_v1` paged discovery fallback no longer skips the `200..499` range after a first-page small-limit retry.
  - `evsnipe_v1` market expiry parsing now accepts Gamma `endDate` timestamps and date-only `endDateIso` payloads, restoring discovery of active weekly/monthly crypto hit markets.
  - `evsnipe_v1` discovery now scopes Gamma `/events` paging to `tag_slug=crypto`, which brings the current weekly/monthly crypto hit markets into the scan window without raising the global event scan limit.
  - `evsnipe_v1` watchlist discovery is now narrowed by symbol anchor distance:
    - keep only strikes within `EVPOLY_EVSNIPE_STRIKE_WINDOW_PCT` (default `10%`) of the current anchor spot,
    - refresh the anchor every `EVPOLY_EVSNIPE_ANCHOR_REFRESH_SEC` (default `4h`),
    - or earlier when spot drifts by `EVPOLY_EVSNIPE_ANCHOR_DRIFT_REFRESH_PCT` (default `3%`).
  - Polymarket market WS sharding now tracks market connectivity per shard instead of flipping one shared global flag from each shard loop.
  - Polymarket slug fallback discovery now includes `4h` markets so fallback coverage matches active strategy timeframes.
  - EVcurve-specific enqueue dedupe cooldown remains configurable via `EVPOLY_ENTRY_DEDUPE_COOLDOWN_EVCURVE_MS`.
  - `premarket_v1` scheduler-side late-intent guard remains env-backed by `EVPOLY_PREMARKET_ENQUEUE_GUARD_MS`.
  - removed unused MM config surface `max_active_markets_per_mode` from `mm_rewards_v1` config parsing.

### 2026-03-05
- `mm_rewards_v1` weak-exit balance guard hardening (`src/main.rs`):
  - force-rebalance weak-exit now requires a fresh live token balance sample before posting new SELL exits (prevents stale cached balance from oversizing exit posts).
  - dynamic and single-order weak-exit submits now refresh token allowance cache + re-read live token balance on `not enough balance/allowance` errors, then back off.
  - weak-exit ladder sizing now uses `weak_balance_shares_for_exit` (live when present) and emits balance-source telemetry fields.
- discovery refresh resiliency for strategy feeds:
  - WS subscription target discovery now falls back to slug-based targets when the active-markets feed is unavailable (`src/polymarket_ws.rs`).
  - `evsnipe_v1` paged market discovery now retries first page with smaller page size and truncates later pages safely on transient fetch failures (`src/evsnipe.rs`).

### 2026-03-05
- `mm_rewards_v1` reward gating + rebalance execution hardening (`src/main.rs`):
  - competition/reward snapshot refresh now merges **rewards page + gamma rewards feed** in one pass, and only swaps to new maps when snapshot has usable rows (keeps last-good snapshot on empty/error refresh).
  - added daily reward hint caches (`condition/slug`) and wired gate fallback: if total reward hint is missing/zero, MM now uses daily-rate hint as secondary reward check.
  - `reward_below_min_total` became a **soft gate** for quoting (still logged), while near-expiry remains the hard exit/offboard gate.
- `mm_rewards_v1` market-details rate-limit resilience (`src/main.rs`):
  - `get_market(...)` detail fetch now retries 429/Too-Many-Requests with backoff and uses stale cached detail snapshot when available instead of hard-failing immediately.
- `mm_rewards_v1` imbalance behavior (`src/main.rs`):
  - rebalance-only mode no longer blindly cancels BUYs on both sides; it cancels heavy/blocked side and can keep light-side BUY quoting during active imbalance (except near-expiry exit window).
  - stale market-feed pause no longer hard-stops rebalance cycles; rebalance/exit can continue in stale windows.
- `mm_rewards_v1` stale/feed handling (`src/main.rs`):
  - successful orderbook fetch now refreshes MM market-data freshness timestamp, preventing false stale-pause/offboard loops when WS is quiet but books are fresh.

### 2026-03-04
- `mm_rewards_v1` reward discovery/ranking and market-feed scope updates (`src/api.rs`, `src/mm/reward_scanner.rs`, `src/polymarket_ws.rs`):
  - Gamma reward parsing now captures total-reward/date fields (with aliases) and computes `reward_total_hint(...)` in API layer.
  - MM auto scanner now ranks/filters by total-reward horizon (not raw daily rate), applies near-expiry exit-window filtering, and threads spread-extend into scoring-feasibility estimates.
  - Added token-scoped market-update wait path in Polymarket WS state so MM event-driven wakeups can be scoped to relevant tokens.
  - Expiry/prune sync now includes token-level market timestamp cache cleanup.
  - Net effect: auto MM market selection is less noisy near expiry and better aligned to remaining reward opportunity.

### 2026-03-04
- `mm_rewards_v1` entry-basis scoping hardening for weak-exit logic (`src/mm/mod.rs`, `src/main.rs`, `src/tracking_db.rs`):
  - added config flags:
    - `EVPOLY_MM_ENTRY_BASIS_PERIOD_SCOPED` (default `false`)
    - `EVPOLY_MM_WEAK_EXIT_REQUIRE_ENTRY_BASIS` (default `false`)
  - added new DB accessor:
    - `get_entry_fill_avg_price_for_strategy_scope_condition_token(...)`
    - computes BUY entry average by `strategy + timeframe + period_timestamp + condition_id + token_id`.
  - MM weak-exit paths now use a shared avg-entry lookup that can be period-scoped when enabled.
  - when `WEAK_EXIT_REQUIRE_ENTRY_BASIS=true`, weak-exit submits are skipped (with `mm_rewards_weak_exit_skip_missing_entry_basis`) instead of using fallback basis prices.
  - MM MTM/heartbeat entry basis now uses the same scoped lookup path for consistency with weak-exit decisions.

### 2026-03-04
- `mm_rewards_v1` stale/reprice cleanup hardening (`src/mm/mod.rs`, `src/main.rs`):
  - added stale reconcile controls:
    - `EVPOLY_MM_STALE_PERIODIC_FULL_SWEEP_SEC`
    - `EVPOLY_MM_STALE_PERIODIC_FULL_SWEEP_MAX_LOOKUPS`
    - `EVPOLY_MM_STALE_RECONCILE_CONCURRENCY`
    - `EVPOLY_MM_STALE_RECONCILE_API_BUDGET_PER_MIN`
  - stale reconcile now supports periodic full-sweep passes (bounded by max lookups + per-minute API budget) with telemetry:
    - `mm_rewards_stale_reconcile_summary`
    - `mm_rewards_stale_reconcile_full_sweep_truncated`
    - `mm_rewards_stale_reconcile_budget_exhausted`
  - added off-target cancel policy controls:
    - `EVPOLY_MM_OFF_TARGET_CANCEL_MODE=disabled|safe|aggressive`
    - `EVPOLY_MM_OFF_TARGET_CANCEL_MAX_PER_PASS`
  - ladder reprice now applies off-target cancellation by policy mode with per-pass cap, while preserving legacy behavior when mode is `disabled`.
  - heartbeat now includes stale local-row and stale exchange-cancel counters for clearer stale-state attribution.

### 2026-03-04
- `mm_rewards_v1` submit backpressure safety controls (`src/mm/mod.rs`, `src/main.rs`):
  - added MM-only enqueue policy:
    - `EVPOLY_MM_ENQUEUE_MODE=await|bounded|try_send`
    - `EVPOLY_MM_ENQUEUE_TIMEOUT_MS`
    - `EVPOLY_MM_QUEUE_PRESSURE_TELEMETRY`
  - MM submit path now uses `enqueue_arbiter_request_mm(...)`:
    - keeps dedupe behavior,
    - can skip rung submit on queue pressure (bounded timeout / try_send full) instead of blocking.
  - added `entry_enqueue_backpressure_skip` telemetry with queue capacity fields and enqueue mode.
  - defaults preserve prior behavior (`await` mode) until explicitly changed.

### 2026-03-04
- `mm_rewards_v1` reward min-size floor safety controls (`src/mm/mod.rs`, `src/main.rs`):
  - added configurable reward-floor policy:
    - `EVPOLY_MM_REWARD_MIN_FLOOR_MODE=per_rung|reference_price|hybrid`
    - `EVPOLY_MM_REWARD_MIN_FLOOR_MAX_MULT` (hybrid clamp vs reference floor)
    - `EVPOLY_MM_REWARD_MIN_SHARES_CAP` (hard cap, `0` disables cap).
  - MM rung sizing and local scoring min-size checks now use the same policy helper (`mm_reward_min_shares_with_policy`), including cap/floor mode.
  - startup telemetry now prints reward-floor mode/mult/cap.
  - effect: prevents deep-rung floor-share blowups on low prices and keeps scoring-vs-submit floor math consistent.

### 2026-03-04
- `mm_rewards_v1` ladder/BBO sizing hardening for top-of-book depth (`src/main.rs`):
  - `best_bid_ask_sizes_from_orderbook(...)` now aggregates size across all rows at the best bid/ask price level instead of using only the first row.
  - affects BBO1 footprint checks used by MM ladder anchoring/deepen decisions.
  - effect: reduces false “BBO1 too thin” triggers and stabilizes ladder placement when orderbook has multiple rows at the same top price.

### 2026-03-04
- `mm_rewards_v1` scoring-guard correctness hardening (`src/main.rs`, `src/mm/scoring_guard.rs`):
  - fixed min-size scoring unit mismatch by converting scoring min size to shares per side/price using `reward_min_size_unit` before local scoring checks.
  - local scoring now validates midpoint/price/size inputs explicitly (non-finite or invalid values fail closed).
  - enriched scoring telemetry context with:
    - side min-size shares,
    - spread-cap used,
    - spread/size pass flags,
    - structured `scoring_fail_reasons`.
  - effect: fewer false scoring passes in `mm_rewards_v1` when reward min-size is USD-based and when counterpart quote inputs are invalid/stale.

### 2026-03-04
- `mm_rewards_v1` stale-reconcile existing-key suppression hardening (`src/main.rs`):
  - fixed a stale-throttle edge where `existing_keys` used all open pending rows even when only a capped subset was reconciled, which could let unchecked stale rows block valid ladder reposts.
  - added per-scope verified-open cache from recent order-status checks; when open rows exceed stale lookup cap, existing-key dedupe now prefers:
    - top-window rows (recent),
    - freshly checked non-stale rows this pass,
    - recently verified rows still inside TTL.
  - stale rows removed from verified cache immediately; cache clears when scope has no open rows.
  - effect: reduces false `already-covered` suppression from deep stale rows without changing non-MM strategy paths.

- `mm_rewards_v1` event-driven wait narrowed to MM token scope to reduce wake noise from unrelated markets (`src/polymarket_ws.rs`, `src/main.rs`, `src/mm/mod.rs`, `.env`):
  - Added WS token-scoped waiter path: `wait_for_market_update_after_for_tokens(...)`.
  - MM loop now caches active MM token scope and, when enabled, waits only for updates in that scope.
  - Added config/env: `EVPOLY_MM_EVENT_SCOPE_FILTER_ENABLE=true`.
  - Effect: lower MM loop churn under high global WS traffic without changing MM quote logic.

### 2026-03-04
- `premarket_v1` enqueue path changed to true batch enqueue chunks through arbiter dedupe/send (`src/main.rs`, `.env`):
  - Replaced per-request parallel task fanout with chunked batch enqueue (`enqueue_arbiter_requests_batch`).
  - Batch path dedupes by shard in bulk, then sends enqueued requests with batch contention telemetry.
  - Added config/env: `EVPOLY_PREMARKET_BATCH_ENQUEUE_CHUNK=24`.
  - Runtime tuning: raised `EVPOLY_ENTRY_WORKER_COUNT_PREMARKET` from `4` -> `8` in `.env`.
  - Effect: reduce enqueue overhead and queue burst pressure for premarket ladders near open.

### 2026-03-04
- `mm_rewards_v1` reward metric switched from daily-rate to total-reward hint across MM selection/gating/ranking (`src/api.rs`, `src/mm/reward_scanner.rs`, `src/main.rs`):
  - Added reward-config parsing fields: `total_rewards`, `start_date`, `end_date` (+ aliases for Gamma/Next payloads).
  - Added `RewardsMarketEntry::reward_total_hint(now_ms, market_end_ts_ms)`:
    - uses explicit total when available,
    - otherwise estimates total as `daily_rate * time_to_market_end_days`.
  - Auto-scan + force-include + min-reward filters now use total-reward hint (including `EVPOLY_MM_AUTO_FORCE_INCLUDE_REWARD_MIN` and `EVPOLY_MM_MIN_REWARD_RATE_HINT` semantics).
  - MM detail snapshot now stores:
    - `reward_daily_hint`
    - `reward_total_hint` (and legacy `reward_rate_hint` now mirrors total for backward-compatible plumbing/log fields).

### 2026-03-04
- `mm_rewards_v1` scoring spread extension for ladder/scoring gates (`src/mm/mod.rs`, `src/mm/reward_scanner.rs`, `src/main.rs`, `.env`):
  - Added config/env: `EVPOLY_MM_SCORING_SPREAD_EXTEND_CENTS` (set to `2.0` in `.env`).
  - MM scoring checks now use `effective_max_spread = market_max_spread + spread_extend` (with proper cents/probability handling) for:
    - auto candidate feasible-level estimation,
    - runtime ladder scoring filter,
    - local q-min scoring guard (`q_min_ready`) preflight.
  - Added telemetry fields in scoring events:
    - `max_spread_effective`
    - `scoring_spread_extend_cents`
  - Affects: `mm_rewards_v1` market selection + scoring-gated quoting behavior across all MM modes/symbols/timeframes.

### 2026-03-04
- `mm_rewards_v1` auto-market override + competition-aware normal buffer (`src/main.rs`, `src/mm/mod.rs`, `src/mm/reward_scanner.rs`):
  - Added high-reward force-include path above mode cap:
    - `EVPOLY_MM_AUTO_FORCE_INCLUDE_REWARD_MIN` (default `500`).
  - Force-included markets now run through auto mode with existing min-stay/cooldown/dedupe safety; they are tagged as `market_source=auto_force_reward`.
  - Added competition-level safe buffer ticks for normal/add BUY ladders:
    - low=`EVPOLY_MM_NORMAL_SAFE_BUFFER_TICKS_LOW` (default `1`)
    - medium=`EVPOLY_MM_NORMAL_SAFE_BUFFER_TICKS_MEDIUM` (default `2`)
    - high=`EVPOLY_MM_NORMAL_SAFE_BUFFER_TICKS_HIGH` (default `3`)
  - Buffer is applied only on normal quote path (not rebalance/imbalance unwind path).
  - Added telemetry for force-include reason and effective buffer ticks in auto selection, safe-buffer, and submit context logs.

### 2026-03-04
- `mm_rewards_v1` normal BUY quote safe-buffer/reprice controls (`src/main.rs`, `src/mm/mod.rs`):
  - Added normal-path safe buffer so new/add BUY ladders do not quote at BBO top:
    - `EVPOLY_MM_NORMAL_SAFE_BUFFER_ENABLE` (default `true`)
    - `EVPOLY_MM_NORMAL_SAFE_BUFFER_TICKS` (default `1`)
  - Safe buffer applies only to normal entry/add quoting path (BUY ladder), not rebalance/imbalance exit paths.
  - Added forced top-hit reprice behavior for normal BUY orders:
    - if live BUY row sits at top bid, mark for cancel/repost to buffered ladder.
    - new controls:
      - `EVPOLY_MM_NORMAL_TOP_REPRICE_MIN_REST_MS` (default `700`)
      - `EVPOLY_MM_NORMAL_TOP_REPRICE_COOLDOWN_MS` (default `1500`)
  - Added telemetry:
    - `mm_rewards_safe_buffer_applied`
    - enriched `mm_rewards_ladder_reprice_cancel` with `normal_top_hit_rows` and safe-buffer config context.

### 2026-03-04
- `mm_rewards_v1` weak-exit execution reliability hardening (`src/main.rs`):
  - Imbalance ladder exits now size only the **remaining** target (`target_exit_total - open_exit_reserved_shares`) to avoid duplicate oversubmits that caused `not enough balance / allowance`.
  - Added settle-wait behavior after weak-exit reconcile cancels (`mm_rewards_weak_exit_reconcile_settle_wait`) so the same loop does not cancel+repost before cancel state settles.
  - Added post-only sell floor enforcement for weak exits (`sell_price >= best_bid + 2*tick_size`) in both dynamic imbalance ladder and single-order weak-exit paths to reduce `invalid post-only order: order crosses book` rejects.
  - Added weak-exit failure backoff in dynamic ladder path for balance/allowance/insufficient errors, and aborts remaining rung submits on balance/cross-book rejects to prevent burst-fail spam.
- Polymarket WS market-loop lag handling (`src/polymarket_ws.rs`):
  - Added soft lag-error tolerance window (`EVPOLY_PM_WS_MARKET_LAG_SOFT_ERRORS`, `EVPOLY_PM_WS_MARKET_LAG_WINDOW_SEC`) so transient `lagged by ... messages` stream errors do not force immediate reconnect storms.
  - New telemetry `polymarket_ws_market_stream_lag_soft` for lag-rate visibility before hard reconnect.

### 2026-03-04
- `mm_rewards_v1` Phase-5 market routing/state-priority hardening (`src/main.rs`):
  - Added explicit MM dominant-state model with precedence:
    - `stale_pause > rebalance_emergency > rebalance_hard > rebalance_soft > normal_quote`.
  - Rebalance-only state now carries adaptive band context in reason/logs:
    - `imbalance_rebalance_only_{soft|hard|emergency}`.
  - Added dominant-state transition telemetry:
    - `mm_rewards_dominant_state_transition`.
  - Added emergency auto offboard path for `auto` mode:
    - new runtime map `mm_auto_force_offboard_until_ms_by_condition`,
    - bypasses min-stay stickiness when market is force-offboarded.
  - Auto cooldown/rotation now also triggers for non-viable routing cases:
    - `not_reward_eligible`,
    - non-scoring rebalance-only (including full-ladder-required path),
    - stale market-feed pause.
  - Auto scan snapshot now reports:
    - `forced_offboard_skipped_count`,
    - `force_offboard_active_conditions`.
  - MM market runtime state persistence now includes dominant-state tag in `market_mode_runtime` (`source:mode:dominant_state`).
  - Affects `mm_rewards_v1` across all enabled MM modes (`quiet_stack`, `tail_biased`, `spike`, `sports_pregame`) and all symbols/timeframes selected by target/auto/hybrid routing.

### 2026-03-04
- `mm_rewards_v1` Phase-4 event-driven control hardening (`src/main.rs`, `src/mm/mod.rs`, `src/mm/metrics.rs`):
  - Added two-step stale gate per MM scope (`condition:tf:period:token`):
    - stale hold on early strikes,
    - stale pause + ladder cancel on repeated/prolonged stale,
    - recovery requires configurable healthy streak before resume.
  - Added feed-stale integration (`last_market_event_ms`) into stale strikes/pause behavior.
  - Added side toxicity guard (PM-only microstructure):
    - uses short-horizon returns (250ms/1s), micro-skew (BBO size imbalance), and update-burst,
    - pauses threatened side and cancels in-scope BUY quotes during toxicity pause window.
  - Added action-budget mode (`normal/conserve/survival`) per condition from rolling action pressure:
    - actions/min, cancel/fill ratio, and stale-strike pressure.
    - mode now modulates ladder levels, size multiplier, and deepen ticks.
  - Added diff-state hysteresis upgrades:
    - pending-cancel settle window before repost,
    - separate anchor vs outer min rest cooldowns for reprice.
  - New config/env knobs added for stale gate, toxicity, action-budget, and reprice hysteresis controls.
  - Added MM telemetry events:
    - `mm_rewards_action_mode_transition`,
    - `mm_rewards_stale_hold`,
    - `mm_rewards_toxic_side_pause`,
    - enriched reprice logs with action-mode/pending-cancel context.

### 2026-03-04
- `mm_rewards_v1` Phase-2 cleanup + Phase-3 escalation update (`src/main.rs`, `src/mm/mod.rs`):
  - Fixed imbalance exit sizing bug:
    - dynamic rebalance target now uses `weak_balance_shares - safety_buffer` (no longer adds open SELL size and over-targets ladder notional).
  - Competition-freeze exit sizing now supports dynamic sizing + BBO ask liquidity clip (instead of always fixed max-order cap behavior).
  - Added adaptive rebalance bands (`soft/hard/emergency`) using:
    - imbalance ratio,
    - time in imbalance,
    - 1s markout EWMA.
  - Added new MM weak-exit band env controls:
    - hard/emergency thresholds (ratio/time/markout),
    - per-band loss floors,
    - per-band extra deepen ticks,
    - per-band reprice multipliers.
  - Rebalance exit ladder now uses band-driven aggressiveness:
    - adaptive allowed loss floor,
    - deeper/lower sell anchors in hard/emergency,
    - faster reprice cadence in hard/emergency.
  - Extended weak-exit telemetry with band context:
    - `rebalance_band`, `allowed_loss_pct`, `time_in_imbalance_ms`,
      `markout_ewma_1s_bps`, `reprice_mult`, `force_touch_exit`.

### 2026-03-04
- `mm_rewards_v1` Phase-2 (imbalance-first exit core) update (`src/main.rs`, `src/mm/mod.rs`):
  - Added dynamic imbalance weak-exit controls:
    - `EVPOLY_MM_WEAK_EXIT_DYNAMIC_IMBALANCE_ENABLE`
    - `EVPOLY_MM_WEAK_EXIT_IMBALANCE_LADDER_LEVELS`
    - `EVPOLY_MM_WEAK_EXIT_IMBALANCE_SAFETY_BUFFER_SHARES`
    - `EVPOLY_MM_WEAK_EXIT_IMBALANCE_MAX_PCT_OF_BBO_ASK_SIZE`
  - In rebalance imbalance mode, weak-exit now plans a multi-rung SELL ladder from anchor ask, distributes target size across levels, and maintains/cancels/reposts by live open SELL rows in-scope.
  - Exit target size in imbalance mode now tracks live inventory (`wallet + open exit rows`) minus safety buffer, instead of fixed `weak_exit_max_order_shares` clipping.
  - Added liquidity clipping by BBO ask size fraction for each weak-exit rung to avoid oversizing against thin top-of-book.
  - Added telemetry event `mm_rewards_rebalance_exit_ladder_plan` with ladder plan, imbalance ratio, anchor price, and liquidity cap context.

### 2026-03-04
- `mm_rewards_v1` Phase-1 inventory/PnL state foundation (`src/main.rs`, `src/tracking_db.rs`):
  - Extended `mm_market_states_v1` with live MM inventory and MTM fields:
    - token IDs, per-side inventory, imbalance side/size, avg entry prices, best bids,
      `mtm_pnl_exit_usd`, `time_in_imbalance_ms`, `max_imbalance_shares`,
      `last_rebalance_duration_ms`, `rebalance_fill_rate`, `exit_slippage_vs_bbo_bps`,
      and `market_mode_runtime`.
  - MM loop now tracks live rebalance lifecycle per condition:
    - starts imbalance timer on threshold breach,
    - records max imbalance during active cycle,
    - emits `mm_rewards_rebalance_cycle_closed` with duration + peak imbalance on recovery.
  - Weak-exit telemetry counters are now tracked in runtime maps and persisted through state snapshots:
    - exit submit/fill counts (for fill-rate),
    - EWMA of exit slippage vs BBO (bps).
  - Final per-market state upsert now writes live inventory/MTM context each loop in live mode.

### 2026-03-04
- `mm_rewards_v1` Phase-0 hotfix pack (`src/main.rs`, `src/mm/reward_scanner.rs`, `src/mm/metrics.rs`):
  - Fixed duplicate-rung suppression source:
    - `existing_keys` is now built from full open BUY rows (not stale-reconcile subset), so deep stale rows cannot silently block/duplicate ladder submits.
  - Fixed q-min deadlock during skew/risk block:
    - when opposite side is intentionally blocked by skew (`opposite_blocked_by_skew`), scoring preflight now allows side-only scoring for that cycle and emits `mm_rewards_scoring_qmin_bypass`.
  - Wired stale transition telemetry:
    - added stale pause state tracking per MM scope, with transition events:
      - `mm_rewards_stale_pause_entered`
      - `mm_rewards_stale_pause_recovered`
    - `MmMetrics.stale_pauses` now increments on each stale transition via `on_stale_transition`.
  - Added weak-exit SELL TTL maintenance:
    - active MM weak-side SELL exits now enforce TTL (`quote_ttl_ms`) and trigger `mm_rewards_weak_side_exit_ttl_cancel` before replacement.
    - keeps single-live-order behavior with settle grace + replace budget checks.
  - Auto selector fairness + underfill guard:
    - mode selection is now slot-interleaved (round-robin by slot) to avoid mode-order starvation.
    - effective auto pool now enforces a minimum of `enabled_modes * max_active_markets_per_mode` (`auto_top_n_effective`).
  - Config/no-op + data hygiene:
    - explicit startup note that `EVPOLY_MM_PREFILL_DRIFT_RECOVER_BPS` is currently informational-only.
    - removed duplicate `"ncaa"` token in sports slug classifier.

### 2026-03-04
- `premarket_v1` strict late-submit guard + parallel execution isolation (`src/main.rs`, `src/trader.rs`):
  - Added strict premarket late guard in arbiter worker path:
    - blocks any `premarket_v1` ladder request when `now >= period_timestamp` before execution (`premarket_late_guard_blocked` event).
  - Added strict premarket late guard in trader path (`execute_limit_buy`) as fail-safe:
    - rejects premarket ladder submits after market open even if a stale request reaches trader.
  - Added dedicated premarket arbiter queue/pool:
    - new channel + worker pool label `premarket`,
    - new env override `EVPOLY_ENTRY_WORKER_COUNT_PREMARKET` (default `4`),
    - premarket requests no longer share the normal arbiter queue with `mm_rewards_v1/evcurve_v1`.
  - Affects `premarket_v1` across all enabled symbols/timeframes (`5m/15m/1h/4h/1d` ladder mode).

### 2026-03-04
- Strategy execution pool split for true parallel lanes (`src/main.rs`, `.env`):
  - Added dedicated arbiter queues/pools for:
    - `endgame_sweep_v1` (`pool=endgame`)
    - `endgame_revert_v1` (`pool=endgame_revert`)
    - `sessionband_v1` (`pool=sessionband`)
    - `evcurve_v1` (`pool=evcurve`)
  - These strategies no longer share `normal`/`critical` execution lanes with each other.
  - Added worker count envs:
    - `EVPOLY_ENTRY_WORKER_COUNT_ENDGAME`
    - `EVPOLY_ENTRY_WORKER_COUNT_ENDGAME_REVERT`
    - `EVPOLY_ENTRY_WORKER_COUNT_SESSIONBAND`
    - `EVPOLY_ENTRY_WORKER_COUNT_EVCURVE`
  - Updated runtime env defaults in `.env` to `4` workers for each new dedicated pool.

### 2026-03-04
- `mm_rewards_v1` risk-cap + merge scheduler reliability update (`.env`, `src/trader.rs`):
  - Raised MM/live exposure caps to avoid ladder submit hard-rejects at `10k`:
    - `POLY_MAX_TOTAL_EXPOSURE_USD: 10000.0 -> 100000.0`
    - `EVPOLY_ARB_STRAT_MM_REWARDS_MAX_USD: 10000 -> 100000`
  - Fixed hourly merge scheduler trigger behavior:
    - `try_claim_merge_scheduled_slot` now allows run at/after configured minute in the hour (not exact minute match only).
    - Prevents merge sweeps from being skipped when merge scheduling is chained from redemption scheduler at a later minute.
  - Affects `mm_rewards_v1` inventory/risk handling and hourly merge execution for all MM symbols/timeframes.

### 2026-03-03
- `mm_rewards_v1` full-ladder blocker softening (`src/main.rs`):
  - Added partial-keep path for `full_ladder_required` preflight failures when coverage already meets the configured minimum floor (`min_ladder_levels`).
  - New telemetry event: `mm_rewards_full_ladder_partial_keep`.
  - Behavior change: MM keeps valid covered ladder levels instead of forcing rebalance-only cancel-all when only deep rungs are missing.
  - Affects all `mm_rewards_v1` modes and symbols/timeframes.

### 2026-03-03
- `mm_rewards_v1` quote lifetime extended for ladder continuity (`.env`):
  - Updated `EVPOLY_MM_QUOTE_TTL_MS` from `12000` (12s) to `300000` (300s).
  - Affects all `mm_rewards_v1` modes (`quiet_stack`, `tail_biased`, `spike`, `sports_pregame`) across all selected symbols/timeframes.
  - Purpose: reduce ladder dropouts caused by short TTL relative to per-condition refresh cadence.

### 2026-03-03
- `mm_rewards_v1` adaptive ladder-fit for tight reward spreads (`src/mm/mod.rs`, `src/mm/reward_scanner.rs`, `src/main.rs`, `.env`):
  - Added auto-scan feasible-depth gating/ranking:
    - new env `EVPOLY_MM_AUTO_MIN_FEASIBLE_LEVELS` (set `2`).
    - scanner now estimates `tick_size`, builds estimated ladders, computes scoring-feasible levels (`up/down/pair`), skips candidates below feasible minimum, and boosts score by feasible-depth ratio.
  - Added auto-scan telemetry fields:
    - `tick_size_est`, `feasible_levels_up_est`, `feasible_levels_down_est`, `feasible_pair_levels_est`.
  - Added runtime ladder adaptation for live MM quoting:
    - when full-ladder mode is active and scoring guard is enabled, MM trims both sides to scoring-feasible depth instead of hard-failing at fixed 5 levels.
    - required ladder depth now uses market-adaptive `required_ladder_levels_market` (e.g., 5 -> 3/2) with minimum floor from `EVPOLY_MM_AUTO_MIN_FEASIBLE_LEVELS`.
  - Added runtime capacity telemetry event `mm_rewards_scoring_capacity` and enriched non-scoring logs with feasible-depth fields.

### 2026-03-03
- `mm_rewards_v1` non-scoring churn reduction + strict scoring clip (`src/mm/reward_scanner.rs`, `src/main.rs`):
  - Changed ladder scoring-fit checks to strict spread-cap comparison (removed half-tick tolerance drift), so scanner/runtime fit logic matches preflight scoring exactly.
  - Added side-level re-clip after skew/conserve deepen in runtime path:
    - effective ladder is clipped again to scoring-valid prices before preflight/cancel/submit.
  - Added partial-keep behavior for `non_scoring` preflight:
    - if covered levels already meet minimum ladder floor, MM keeps active ladder and skips hard cancel-all.
    - new event: `mm_rewards_non_scoring_partial_keep`.
  - Goal: prevent repeated cancel/repost loops that drop active ladder to zero when only deep rungs fail scoring.

### 2026-03-03
- `mm_rewards_v1` scoring midpoint source correction (`src/main.rs`, `src/mm/reward_scanner.rs`, `src/mm/scoring_guard.rs`):
  - Runtime scoring midpoint now prioritizes live orderbook-derived midpoint (`best bid/ask`) over market-details midpoint.
  - Auto scanner midpoint selection also prioritizes live books over static details midpoint.
  - Added tiny floating tolerance in local scoring guard (`1e-9`) to avoid false non-scoring on exact boundary equality.
  - Impact: removes repeated 0.5c boundary misses caused by stale midpoint source and float precision edge cases.

### 2026-03-03
- `mm_rewards_v1` auto-market stability + de-selection cancel hardening (`src/main.rs`, `src/mm/mod.rs`, `src/trader.rs`, `src/tracking_db.rs`, `.env`):
  - Added sticky auto selection state with minimum market lease:
    - new env `EVPOLY_MM_AUTO_MIN_STAY_SEC` (set to `7200`), so an auto-selected market stays at least 2h before replacement.
  - Added controlled rotation budget per refresh:
    - new env `EVPOLY_MM_AUTO_ROTATION_BUDGET_PER_REFRESH` (set to `1`), limiting churn to one replacement per refresh cycle.
  - Added hard de-selection cancel for auto markets:
    - when a condition leaves active auto set, MM cancels all open MM orders for that condition immediately (BUY+SELL path).
  - Added trader/db support for condition-wide MM cancel:
    - `Trader::cancel_pending_orders_for_strategy_condition(...)`
    - `TrackingDb::list_open_pending_orders_for_strategy_condition(...)`.
  - Effect: snapshot churn no longer rotates the full book; dropped auto markets are actively removed from book instead of lingering until TTL.
  - Increased auto ranking pool in live env:
    - `EVPOLY_MM_AUTO_TOP_N: 8 -> 80` to reduce truncation of high-reward candidates before per-mode selection.
  - Added top-reward inclusion guard in auto selector:
    - new env `EVPOLY_MM_AUTO_FORCE_TOP_REWARD=true`.
    - if the highest reward-rate candidate is missing from per-mode selection, it is injected into the first enabled mode for that refresh.

### 2026-03-03
- `mm_rewards_v1` weak-exit/unwind min-size guard hardening (`src/main.rs`):
  - Added explicit minimum-share checks (`order_floor.min_size_shares`) before submitting MM SELL exits.
  - Applies to both `competition_high_freeze` exits and regular weak-side rebalance exits.
  - New skip events: `mm_rewards_competition_high_exit_skip_min_size`, `mm_rewards_weak_exit_skip_min_size`.
  - Prevents repeated 400 rejects like `Size (...) lower than the minimum`.

### 2026-03-03
- `mm_rewards_v1` full-ladder auto selection unblocked (`src/main.rs`, `.env`):
  - Fixed ladder deepen behavior when full ladder is required:
    - deepening now shifts rung prices down while preserving rung count, instead of dropping a rung (`covered_levels` no longer forced from 5 -> 4).
  - Auto-market replacement cooldown is no longer set for internal `full_ladder_required` preflight failures.
  - Raised BBO participation threshold to reduce unnecessary deepen triggers:
    - `EVPOLY_MM_BBO1_MAX_SIZE_FRACTION: 0.25 -> 0.40`.
  - Affects `mm_rewards_v1` auto mode across all enabled MM modes.

### 2026-03-03
- `mm_rewards_v1` stub-book reward scan + ladder anchor fix (`src/mm/reward_scanner.rs`, `src/main.rs`):
  - Auto scanner now treats extreme stub books (`~0.01/0.99` or very wide spread) specially by using token price hints for reward EV estimation instead of raw top bid.
  - Fixed reward midpoint inference to use YES/UP token price (or `1-NO`) instead of averaging token prices to `0.5`.
  - MM ladder builder now supports per-token price hint and uses it as anchor when book is stub/wide, instead of anchoring at dead `0.01` bids.
  - Effect: high-reward markets with thin/dead top-of-book are no longer incorrectly scored as zero by construction.

### 2026-03-03
- `mm_rewards_v1` auto-scan source switched to full CLOB universe first (`src/api.rs`, `src/mm/reward_scanner.rs`):
  - Added `get_all_clob_markets()` pagination path on CLOB `/markets` and scanner now loads full market set first.
  - CLOB scan now starts near the tail window (recent offsets) instead of oldest pages, so auto scan sees current/open markets.
  - Auto scanner now filters out inactive/closed/expired markets before ranking and applies reward filters from market `rewards` metadata.
  - Reward feed rows (`/rewards` page) are now optional overlay for competition/reward overrides, not the primary candidate source.
  - Added rewards-feed detail-resolve supplement/fallback path so current reward markets are injected even when CLOB window scan is sparse/old.
  - Candidate pool is sorted by reward rate from full-market scan, then trimmed to scanner budget (`auto_scan_limit * 4`).
  - Auto selection now fully respects `EVPOLY_MM_MIN_REWARD_RATE_HINT` as the reward-rate floor.

### 2026-03-03
- `mm_rewards_v1` imbalance-rebalance execution update (`src/main.rs`, `src/mm/mod.rs`, `.env`):
  - Added imbalance loss-allowed weak-exit policy with explicit env controls:
    - `EVPOLY_MM_WEAK_EXIT_ALLOW_LOSS_ON_IMBALANCE`
    - `EVPOLY_MM_WEAK_EXIT_IMBALANCE_MAX_LOSS_PCT`
    - `EVPOLY_MM_WEAK_EXIT_IMBALANCE_FORCE_TOUCH_RATIO`
  - Weak-exit pricing now supports imbalance-first de-risking:
    - when imbalance is active, exit pricing can step to touch (maker ask) and go below avg entry within configured max-loss floor.
    - normal balanced flow remains profit-target based.
  - Weak-exit cancel/reprice churn reduced:
    - size-only reprices now require both absolute and percentage size delta (`requote_min_size_delta_pct`) instead of any `>=1` share delta.
  - Auto-market replacement cooldown added:
    - auto candidates can be put on temporary cooldown after `competition_high_freeze` or `non_scoring_rebalance_only` outcomes.
    - scanner excludes cooled conditions until expiry and forces early refresh for replacement rotation.
    - env control: `EVPOLY_MM_REPLACEMENT_COOLDOWN_SEC`.

### 2026-03-03
- `mm_rewards_v1` reward-safety + execution wiring hardening (`src/main.rs`, `src/mm/mod.rs`, `src/mm/reward_scanner.rs`, `src/mm/scoring_guard.rs`, `.env`):
  - Added strict reward-eligibility controls and turned them on in env:
    - `EVPOLY_MM_REQUIRE_REWARD_ELIGIBLE=true`
    - `EVPOLY_MM_ALLOW_UNWIND_ONLY_WHEN_NOT_REWARD_ELIGIBLE=true`
    - Non-eligible markets now emit `mm_rewards_skip_not_reward_eligible`, BUY ladders are canceled/blocked, and unwind-only path is allowed when configured.
  - Added auto fail-closed option for rewards feed and enabled it:
    - `EVPOLY_MM_AUTO_REQUIRE_REWARDS_FEED=true`
    - auto scanner now returns no candidates (instead of full active-market fallback) when rewards feed is unavailable.
  - Enforced strict `auto_top_n` cap in runtime selection (removed implicit widening pool behavior).
  - Wired local scoring guard into MM rung submit path and enabled it:
    - `EVPOLY_MM_SCORING_GUARD_ENABLE=true`
    - `EVPOLY_MM_SCORING_GUARD_REQUIRE_QMIN=true`
    - New skip event: `mm_rewards_skip_non_scoring_rung`.
    - Scoring guard math corrected to rewards units (`max_spread` cents -> probability delta) and DOWN-side yes-equivalent pricing.
  - Reduced stale-order reconciliation fanout with throttling controls (enabled in env):
    - `EVPOLY_MM_STALE_RECONCILE_INTERVAL_MS=2000`
    - `EVPOLY_MM_STALE_RECONCILE_MAX_LOOKUPS=8`
    - Prevents per-loop `get_order` storms on every open row.
  - Tightened market-state order counts to execution scope:
    - `open_buy_orders` / `open_sell_orders` now computed by timeframe+period+token scope (same scope as MM execution), reducing state drift.
  - Added mode-cap enforcement in runtime work queue:
    - `EVPOLY_MM_MAX_ACTIVE_MARKETS_PER_MODE` now actively limits non-forced work items per mode.
  - Added mode-override handling for inventory-janitor forced items when original mode is disabled, with telemetry (`mm_rewards_inventory_janitor_mode_override`).
  - Auto rank score now uses configured alpha/beta:
    - score = `rank_alpha_reward * expected_reward_day_est - rank_beta_risk * inventory_risk`.
    - `spike_reward_rate_min` is now enforced for spike-mode auto candidates (`mm_rewards_auto_skip_spike_below_reward_min`).

### 2026-03-03
- `mm_rewards_v1` inventory skew trigger + competition-freeze controls updated (`src/main.rs`, `src/mm/mod.rs`, `src/mm/reward_scanner.rs`, `.env`):
  - Skew guard now triggers on absolute YES/NO inventory imbalance, not only favor/weak ratio context:
    - new threshold env `EVPOLY_MM_INVENTORY_IMBALANCE_TRIGGER_SHARES` (set to `200` shares).
    - when imbalance is above threshold, BUY quoting is blocked/canceled on the overloaded token side.
  - Added competition-level monitoring from rewards feed snapshot + auto-candidate metadata:
    - markets entering `high` competition are put into `competition_high_freeze`.
    - while frozen, MM cancels BUY ladders for the market and pauses new quoting until competition returns to `medium`/`low`.
    - freeze lifecycle events added:
      - `mm_rewards_competition_high_freeze_enter`
      - `mm_rewards_competition_high_freeze_active`
      - `mm_rewards_competition_high_freeze_exit`
  - Added high-competition inventory exit attempts:
    - while market is frozen and inventory exists, bot posts maker `SELL` exits (post-only GTC) per side with current best-ask / avg-entry guard, tracked with existing MM exit state.
    - new events:
      - `mm_rewards_competition_high_exit_submitted`
      - `mm_rewards_competition_high_exit_closed`
      - `mm_rewards_competition_high_exit_failed`
  - New env gate:
    - `EVPOLY_MM_COMPETITION_HIGH_FREEZE_ENABLE=true`.
  - Scope: all `mm_rewards_v1` markets (target and auto modes), all symbols/timeframes currently selected by MM.

### 2026-03-02
- `mm_rewards_v1` auto-mode selection and favor-side behavior updated (`src/main.rs`, `src/mm/reward_scanner.rs`, `.env`):
  - Hybrid auto selection now picks one market per enabled MM mode (`quiet_stack`, `tail_biased`, `spike`, `sports_pregame`) using mode-fit scoring and per-mode dedupe by `condition_id`.
  - Auto selection pool widened from strict `auto_top_n` slicing to a larger ranked pool (`max(auto_top_n, enabled_modes*8)`) before per-mode top-1 pick.
  - Added mode-aware auto selector helper: `select_top_candidate_per_mode(...)` with explicit scoring heuristics and sports/tail detection.
  - Favor-side logic is now dynamic when `EVPOLY_MM_FAVOR_SIDE=none`:
    - if UP reference price (`mid/bbo`) >= `0.85` and dominant -> favor UP,
    - else if DOWN reference price >= `0.85` and dominant -> favor DOWN,
    - else stay neutral (`none`).
  - `mm_rewards_quote_plan` telemetry now logs:
    - `favor_side_configured`, `favor_side_effective`,
    - `favor_side_threshold_price`,
    - `up_reference_price`, `down_reference_price`.
  - MM env defaults aligned for requested behavior:
    - `EVPOLY_MM_FAVOR_SIDE=none`
    - all four mode flags enabled.

- `mm_rewards_v1` hybrid scheduler fix: target + auto now run together (`src/main.rs`):
  - Reworked MM work-item scheduling so `hybrid` mode processes both configured target selectors and auto-selected candidates in the same loop.
  - Fixed prior behavior where presence of `single_market_selectors` implicitly disabled auto execution.
  - Added per-loop condition dedupe (`condition_id`) to prevent double-processing when target and auto overlap.
  - Added explicit MM skip telemetry for previously silent paths:
    - `mm_rewards_skip_missing_token_id`
    - `mm_rewards_skip_orderbook_fetch`
    - `mm_rewards_skip_ladder_empty`
    - `mm_rewards_skip_period_elapsed`
    - `mm_rewards_skip_duplicate_work_item`
  - Added `source=target|auto` tagging in `mm_rewards_quote_plan` for attribution/debug.
  - Also adjusted single-market fill-cap short-circuit so it does not block auto processing in hybrid.
  - Runtime config aligned for validation with both sources enabled together:
    - `EVPOLY_MM_MARKET_MODE=hybrid` with non-empty `EVPOLY_MM_SINGLE_MARKET_SLUGS`.
  - Added auto-market enrichment for token IDs/outcomes (`src/main.rs`):
    - If auto candidate payload is missing directional token mapping, MM now fetches full market by slug and caches it (10m) before quote processing.
    - New diagnostics:
      - `mm_rewards_auto_market_enrich_failed`
      - `mm_rewards_auto_market_enrich_mismatch`
  - Hardened directional token selection fallback (`src/main.rs`):
    - `select_token_id_for_direction` now falls back to deterministic binary mapping (`ids[0]` for UP, `ids[1]` for DOWN) when outcomes are missing/unparseable.
    - This prevents repeated `mm_rewards_skip_missing_token_id` on binary auto markets with incomplete outcome payloads.
  - Market-slug discovery hardening for MM auto enrichment (`src/api.rs`):
    - `get_market_by_slug` now attempts direct Gamma `/markets?slug=...` lookup first, then falls back to event-slug discovery.
    - Added market JSON normalization before deserialize:
      - array `clobTokenIds`/`outcomes` -> JSON-string format expected by `Market`
      - `endDate` fallback into `endDateISO`
      - default `active/closed` if missing
    - Reduced `mm_rewards_auto_market_enrich_failed` on market-slug inputs.

- `mm_rewards_v1` target-market pin restored for Fed/Warsh set (`.env`):
  - Switched MM rewards market mode from `hybrid` back to `target`:
    - `EVPOLY_MM_MARKET_MODE=target`
  - Restored explicit single-market selector rotation across the 5 intended markets:
    - `fed-decision-in-april / 25 bps decrease`
    - `fed-decision-in-april / no change`
    - `fed-decision-in-march-885 / 25 bps decrease`
    - `fed-decision-in-march-885 / no change`
    - `who-will-trump-nominate-as-fed-chair / kevin-warsh`
    - via `EVPOLY_MM_SINGLE_MARKET_SLUGS=...`
  - Effect: MM rewards loop now prioritizes these configured markets instead of auto-selected non-target markets.

- `mm_rewards_v1` auto-ranker refinement for reward floor + competition estimate (`src/mm/reward_scanner.rs`, `src/main.rs`):
  - Auto-scan competition baseline now uses reward-min-size in normalized shares (`shares` mode uses raw min-size; `usd` mode converts min-size by reference price before scoring).
  - This keeps rank estimates consistent with runtime reward-min-size enforcement and avoids overestimating score when `min_size` is not share-denominated.
  - Added API competition signal from Gamma market payload (`market.competitive` in range `[0,1]`) into scanner candidate state.
    - Stored/logged as:
      - `competition_score_api`
      - `competition_level_api` (`low` `<0.33`, `medium` `<0.66`, else `high`)
    - Ranking risk now includes this competition signal (higher competition lowers effective rank score).
  - Auto-scan telemetry now emits scoring internals in both snapshot and selection logs:
    - `expected_reward_day_est`
    - `reward_efficiency_est`
    - `capital_locked_est`
    - `our_share_est`
    - `our_qmin_est`
    - `competition_qmin_est`
    - `competition_score_api`
    - `competition_level_api`
  - Affects: `mm_rewards_v1` auto/hybrid market selection only.

- `mm_rewards_v1` auto-ranker source/objective upgrade (`src/api.rs`, `src/mm/reward_scanner.rs`, `src/main.rs`):
  - Auto candidate universe now prioritizes rewards-page market feed (`/rewards` dehydrated payload) instead of first-page Gamma events.
    - Fallback remains paginated Gamma active markets when rewards payload fetch fails.
  - Added rewards-feed competition signal ingestion:
    - raw field: `market_competitiveness`
    - normalized risk score: log-scaled into `[0,1]`
    - API bucket level emitted as `low|medium|high`.
  - Ranking objective changed to prioritize absolute expected reward capture:
    - primary: `expected_reward_day_est`
    - secondary: reward efficiency
    - both risk-adjusted via existing alpha/beta controls.
  - Added telemetry fields:
    - `competition_raw_api`
    - `competition_source_api` (`rewards_market_competitiveness|gamma_market_competitive|none`)
  - Affects: `mm_rewards_v1` auto/hybrid market discovery, ranking, and selection quality.

- `mm_rewards_v1` ranking objective switched to budgeted reward/day + hybrid rollout (`src/mm/mod.rs`, `src/mm/reward_scanner.rs`, `src/main.rs`, `.env`):
  - Auto ranking now uses **estimated reward/day** as primary score (instead of efficiency-heavy score).
  - Ranking share model now uses a budgeted capital model:
    - `shares_for_rank = auto_rank_budget_usd / (best_bid_up + best_bid_down)`
    - with reward min-size floor still enforced.
  - Added config:
    - `EVPOLY_MM_AUTO_RANK_BUDGET_USD` (default `2000`, matching 200 USD x 10 ladder levels total).
  - Runtime env rollout updates:
    - `EVPOLY_MM_MARKET_MODE=hybrid`
    - `EVPOLY_MM_AUTO_TOP_N=5`
    - `EVPOLY_MM_AUTO_RANK_BUDGET_USD=2000`
    - cleared single-market selector overrides to allow auto/hybrid selection:
      - `EVPOLY_MM_SINGLE_MARKET_SLUGS=`
      - `EVPOLY_MM_SINGLE_MARKET_SLUG=`
  - Startup and auto-scan telemetry now include `auto_rank_budget_usd`.
  - Affects: `mm_rewards_v1` auto market selection/ranking only; execution path unchanged.

- `mm_rewards_v1` market-details parser hardening for auto-scan (`src/models.rs`):
  - `MarketDetails.end_date_iso` now deserializes `null` as empty string instead of hard-failing parse.
  - Fixes repeated CLOB market parse failures in MM auto universe scans on markets where `end_date_iso` is null.
  - Affects: `mm_rewards_v1` auto/hybrid candidate fetch reliability.

- `mm_rewards_v1` reward min-size enforcement in ladder sizing (`src/mm/mod.rs`, `src/main.rs`):
  - Added reward-floor config controls:
    - `EVPOLY_MM_REWARD_MIN_SIZE_ENFORCE` (default `true`)
    - `EVPOLY_MM_REWARD_MIN_SIZE_UNIT=shares|usd` (default `shares`)
  - MM BUY ladder sizing now enforces reward min-size before submit:
    - side size = `max(adaptive_size, reward_min_floor_for_price)`,
    - applied per rung (price-aware conversion when `unit=usd`).
  - BBO1 concentration/deepen checks now use reward-floor-adjusted effective size.
  - Added telemetry fields in quote-plan/submit/skip logs for reward floor:
    - `reward_min_size_enforce`, `reward_min_size_unit`, `reward_min_size_raw`, `reward_min_shares_for_rung`.
  - Added unit tests for reward min-size conversion helper in `src/main.rs` tests.
  - Affects: `mm_rewards_v1` only.

- `mm_rewards_v1` Chunk-5 attribution + rollout controls (`src/tracking_db.rs`, `src/trader.rs`, `src/main.rs`, `src/mm/mod.rs`):
  - Added per-market MM attribution query + structs in `src/tracking_db.rs`:
    - `summarize_mm_market_attribution(strategy_id, from_ts_ms)` with:
      - trade realized PnL (`settlements_v2`)
      - merge/reward/redemption cashflow (`wallet_cashflow_events_v1`)
      - open carry snapshot (`positions_v2` open units/notional)
      - net per market.
    - `summarize_wallet_cashflow_unattributed_by_event(...)` for condition-less cashflow visibility.
  - Extended MM admin status payload in `src/trader.rs` (`mm_rewards_status`):
    - `attribution_24h`, `attribution_all`
    - `attribution_totals_24h`, `attribution_totals_all`
    - `cashflow_unattributed_24h`, `cashflow_unattributed_all`
    - `rollout_flags`.
  - Added admin reporting endpoint in `src/main.rs`:
    - `GET /admin/mm/attribution?window=24h|all`
    - returns rows + totals + unattributed cashflow for selected window.
  - Added staged rollout action toggles in `src/mm/mod.rs` and wired in `src/main.rs`:
    - `EVPOLY_MM_CONSERVE_ACTIONS_ENABLE`
    - `EVPOLY_MM_UNWIND_ACTIONS_ENABLE`
    - conserve/unwind telemetry remains visible even when actions are disabled.
  - Affects: `mm_rewards_v1` only; other strategies unchanged.

- `mm_rewards_v1` Chunk-4 adaptive quoting controls + markout pipeline (`src/mm/metrics.rs`, `src/mm/mod.rs`, `src/main.rs`, `src/tracking_db.rs`):
  - Added markout runtime pipeline (100ms/1s/5s) with prefill-drift/pickoff tracking:
    - new tracker types in `src/mm/metrics.rs` store BBO snapshots, submit anchors, fill samples, EWMA stats, and per-condition state.
    - new DB query in `src/tracking_db.rs`:
      - `list_entry_fill_events_since(strategy_id, since_ts_ms, limit)` for MM fill ingestion.
    - new telemetry:
      - `mm_rewards_markout_sample`
      - `mm_rewards_markout_sample_invalid`
      - `mm_rewards_markout_scan_error`
      - `mm_rewards_conserve_on` / `mm_rewards_conserve_off`
      - `mm_rewards_unwind_gate_triggered` / `mm_rewards_unwind_gate_cleared`
  - Added adaptive requote hysteresis/churn controls in `src/main.rs`:
    - reprice-cancel now requires meaningful drift by price ticks or ladder-level gap.
    - new envs:
      - `EVPOLY_MM_REQUOTE_MIN_PRICE_DELTA_TICKS`
      - `EVPOLY_MM_REQUOTE_MIN_SIZE_DELTA_PCT`
  - Added continuous inventory-skew sizing/deepening controls in `src/main.rs`:
    - side size multiplier and ladder deepening now respond to side inventory ratio + favor/weak skew.
    - new envs:
      - `EVPOLY_MM_SKEW_RATIO_SOFT`
      - `EVPOLY_MM_SKEW_RATIO_HARD`
      - `EVPOLY_MM_SKEW_SIZE_MULT_MIN`
      - `EVPOLY_MM_SKEW_DEEPEN_LEVELS_MAX`
  - Added conserve/unwind gates driven by markout + inventory in `src/main.rs`:
    - conserve mode reduces levels/size and deepens ladder during toxic windows.
    - unwind gate cancels favored-side BUY ladder pressure when inventory is high and markout is sufficiently negative.
  - Added/normalized MM markout env controls in `src/mm/mod.rs`:
    - `EVPOLY_MM_MARKOUT_ENABLE`, `EVPOLY_MM_MARKOUT_GATE_BPS`, `EVPOLY_MM_MARKOUT_RECOVER_BPS`, `EVPOLY_MM_MARKOUT_MIN_SAMPLES`, `EVPOLY_MM_MARKOUT_MAX_BOOK_AGE_MS`, `EVPOLY_MM_MARKOUT_WINSOR_BPS`, `EVPOLY_MM_MARKOUT_EWMA_HALFLIFE_FILLS`
    - `EVPOLY_MM_PREFILL_DRIFT_*`, `EVPOLY_MM_PICKOFF_ENABLE`
    - `EVPOLY_MM_CONSERVE_*`, `EVPOLY_MM_UNWIND_*`
  - Affects: `mm_rewards_v1` only; other strategies unchanged.

- `mm_rewards_v1` Chunk-3 auto selector + RV-aware ranking (`src/mm/mod.rs`, `src/mm/reward_scanner.rs`, `src/main.rs`):
  - Added auto/hybrid market-scanning controls:
    - `EVPOLY_MM_AUTO_SCAN_LIMIT`
    - `EVPOLY_MM_MIN_REWARD_RATE_HINT`
    - `EVPOLY_MM_RANK_ALPHA_REWARD`
    - `EVPOLY_MM_RANK_BETA_RISK`
    - `EVPOLY_MM_RV_ENABLE`
    - `EVPOLY_MM_RV_WINDOW_SHORT_SEC`
    - `EVPOLY_MM_RV_WINDOW_MID_SEC`
    - `EVPOLY_MM_RV_WINDOW_LONG_SEC`
    - `EVPOLY_MM_RV_BLOCK_THRESHOLD`
  - Implemented reward scanner/ranker in `src/mm/reward_scanner.rs`:
    - scans active markets via Gamma events,
    - loads market details + UP/DOWN orderbooks,
    - computes reward snapshot, spread quality, midpoint history RV (`short/mid/long`), and risk-adjusted score,
    - filters low-reward and high-RV markets (when RV gate is enabled),
    - returns ranked candidates for runtime selection.
  - Runtime integration in `src/main.rs`:
    - periodic auto refresh (`auto_refresh_sec`) with structured telemetry:
      - `mm_rewards_auto_scan_snapshot`
      - `mm_rewards_auto_scan_error`
      - `mm_rewards_auto_market_selected`
    - `market_mode=auto` now actively selects and trades ranked markets (instead of scaffold-only idle) when candidates exist.
    - `market_mode=hybrid` now uses auto-ranked market per mode when available, with target-mode fallback when auto set is empty.
    - heartbeat now includes `auto_selected_count` and effective market-mode context.
  - Affects: `mm_rewards_v1` market selection/discovery path for all MM modes (`quiet_stack`, `tail_biased`, `spike`, `sports_pregame`).

- `mm_rewards_v1` weak-side maker-exit stability hardening (`src/main.rs`, `src/mm/mod.rs`):
  - Added reserved-aware weak inventory handling for exit cancel decisions:
    - effective weak inventory now uses `available_balance + reserved_open_exit_shares` instead of raw available balance only.
  - Added no-inventory cancel suppression to prevent submit/cancel flapping:
    - settle grace gate before cancel: `EVPOLY_MM_WEAK_EXIT_SETTLE_GRACE_MS` (default `3000`).
    - consecutive low-inventory confirmation loops before cancel:
      - `EVPOLY_MM_WEAK_EXIT_NO_INVENTORY_CONFIRM_LOOPS` (default `3`).
  - Added weak-exit cancel/replace anti-flap budget:
    - `EVPOLY_MM_WEAK_EXIT_MAX_REPLACES_PER_MIN` (default `20`).
    - emits `mm_rewards_weak_exit_flap_detected` when budget exhausted.
  - Enforced strict single-live-order behavior on weak-side reprice:
    - reprice path now cancels and waits settle grace before replacement submit (no same-loop cancel+replace).
  - Added new MM weak-exit telemetry:
    - `mm_rewards_weak_exit_suppressed_no_inventory_grace`
    - `mm_rewards_weak_exit_flap_detected`
  - Affects: `mm_rewards_v1` weak-side maker exit behavior across all configured MM markets.

- `mm_rewards_v1` Chunk-1 foundation/config cleanup (`src/mm/mod.rs`, `src/main.rs`):
  - Added market-mode scaffolding for MM operation:
    - new enum/config: `target | auto | hybrid` via `EVPOLY_MM_MARKET_MODE`.
    - added auto placeholders for upcoming selector rollout:
      - `EVPOLY_MM_AUTO_TOP_N`
      - `EVPOLY_MM_AUTO_REFRESH_SEC`
  - Runtime behavior kept backward-compatible for current target flow:
    - existing target/single-market path remains unchanged.
    - `auto` mode is scaffold-only in this chunk (explicit telemetry + heartbeat context, no auto selector execution yet).
  - Added MM mode telemetry context in startup and heartbeat:
    - `market_mode_requested`
    - `market_mode_effective`
  - Unified dynamic-markout env parsing (cleanup for upcoming MM controls):
    - canonical key: `EVPOLY_MM_MARKOUT_DYNAMIC_ENABLE`
    - canonical gate key: `EVPOLY_MM_MARKOUT_DYNAMIC_K_GATE`
    - legacy alias fallback retained:
      - `EVPOLY_MM_MARKOUT_DYNAMIC_GATE_ENABLE`
      - `EVPOLY_MM_MARKOUT_DYNAMIC_GATE_K`
    - recover key: `EVPOLY_MM_MARKOUT_DYNAMIC_K_RECOVER`
  - Affects: `mm_rewards_v1` config/runtime metadata across all MM-targeted symbols/timeframes.

- `mm_rewards_v1` tracking/audit hardening (`src/main.rs`, `src/tracking_db.rs`, `src/trader.rs`, `src/api.rs`):
  - Added MM market-state persistence table `mm_market_states_v1` and runtime upserts per active MM condition (state/reason/open buy+sell counts/inventory/ladder bounds/last event time).
  - Added admin status endpoint `GET /admin/mm/status` returning live MM market states + wallet cashflow summaries (`24h` and all-time) for `mm_rewards_v1`.
  - Added Polymarket Data API activity sync for MM cashflows:
    - new activity fetch path in API client (`get_user_activity`),
    - runtime ingest loop (default enabled) writes `REDEMPTION_CONFIRMED`, `MERGE_SUCCESS`, `MAKER_REBATE`-style rows into `wallet_cashflow_events_v1` with idempotent `cashflow_key`.
    - new env controls:
      - `EVPOLY_MM_ACTIVITY_SYNC_ENABLE`
      - `EVPOLY_MM_ACTIVITY_USER`
      - `EVPOLY_MM_ACTIVITY_SYNC_SEC`
      - `EVPOLY_MM_ACTIVITY_SYNC_LIMIT`
      - `EVPOLY_MM_WATCHDOG_NO_LADDER_SEC`
  - Added no-active-ladder watchdog log path (`mm_rewards_watchdog_no_ladder`) driven by persisted per-condition last-active timestamps.
  - Affects: `mm_rewards_v1` all symbols/timeframes currently selected via MM market selectors.

- Fill-accounting correction for MM sell fills (`src/trader.rs`):
  - `on_order_filled` is now side-aware:
    - BUY fills remain `ENTRY_FILL`,
    - SELL fills are recorded as `EXIT` with realized PnL estimate vs tracked average entry price.
  - Prevents MM weak-side sell fills from being incorrectly booked as entry fills.
  - This impacts lifecycle accounting quality for any strategy path that can fill SELL through pending-order reconcile; primary target is `mm_rewards_v1` weak-side maker exits.

- Merge cashflow tracking extension (`src/trader.rs`):
  - Successful merge submit paths now emit `MERGE_SUBMITTED` cashflow rows (manual condition merge + scheduled/manual merge sweep success paths), improving realized cash tracking beyond settlement-only views.

- Merge/redeem ops reliability + MM runtime tuning (`src/trader.rs`, `.env`):
  - Merge sweep candidate detection is now token-type agnostic:
    - switched from crypto-only `TokenType::{Up,Down}` pairing to per-condition token-id pairing from live pending inventory.
    - merge scan now also ingests configured MM selector markets (`EVPOLY_MM_SINGLE_MARKET_SLUGS` / `EVPOLY_MM_SINGLE_MARKET_SLUG`) and pulls condition token ids directly from Gamma event/market slug resolution.
    - token-id extraction supports both `tokens[]` and `clobTokenIds` payload formats from Gamma responses.
    - this allows hourly/manual merge sweeps to detect and merge non-crypto/MM-rewards binary markets (e.g., Fed markets) instead of silently finding zero candidates.
    - per-condition UP/DOWN balance probes now run concurrently (join) to reduce merge sweep latency.
  - MM capital/risk headroom raised:
    - `POLY_MAX_TOTAL_EXPOSURE_USD`: `6000 -> 10000` (soft exposure guard rises accordingly).
    - `EVPOLY_MM_MAX_FAVOR_INVENTORY_SHARES`: `2000 -> 10000`.
    - `EVPOLY_MM_MAX_WEAK_INVENTORY_SHARES`: `2000 -> 10000`.
  - MM reaction speed increased (event-driven fallback and reprice cadence):
    - `EVPOLY_MM_POLL_MS`: `250 -> 200`
    - `EVPOLY_MM_EVENT_FALLBACK_POLL_MS`: `750 -> 250`
    - `EVPOLY_MM_REPRICE_MIN_INTERVAL_MS`: `300 -> 120`
    - `EVPOLY_MM_QUOTE_TTL_MS`: `20000 -> 12000`
    - `EVPOLY_MM_CONSTRAINTS_CACHE_MS`: `30000 -> 10000`
  - Affects: `mm_rewards_v1` runtime behavior and global merge sweep execution.

- Execution-layer reliability update across strategy order paths (`src/api.rs`):
  - Added builder-attribution fail-open behavior:
    - if builder `/order` post returns auth error (`401/403`), runtime disables builder posting for the current run and continues with standard CLOB posting path on retries.
    - prevents builder credential issues from hard-blocking `premarket_v1`, `endgame_sweep_v1`, `evcurve_v1`, `sessionband_v1`, `evsnipe_v1`, and `mm_rewards_v1`.
  - Reduced auth-cache churn in balance probes:
    - `check_balance_only` and `check_balance_allowance` now refresh auth client only on auth errors.
    - non-auth balance-call failures use a same-client transient retry instead of forcing global client reset.
  - Effect:
    - fewer forced re-auth cycles during high-frequency loops (especially `mm_rewards_v1` weak-side inventory checks),
    - lower probability of cascading `ENTRY_FAILED` bursts caused by repeated auth rebuilds.
- `mm_rewards_v1` multi-market single-slug expansion (`src/mm/mod.rs`, `src/main.rs`, `.env`):
  - Added multi-selector env:
    - `EVPOLY_MM_SINGLE_MARKET_SLUGS` (comma-separated list; supports full Polymarket URLs and `event_slug/market_slug` selectors).
  - Runtime single-market mode now:
    - iterates configured selectors every poll cycle (using repeated quiet-stack passes; no longer one hardcoded slug only),
    - applies the same quote/ladder/reprice/favor-side/weak-exit logic per selected market.
  - Fill-cap safety changed from global stop to per-condition stop:
    - `EVPOLY_MM_SINGLE_MARKET_MAX_ENTRY_FILLS` now halts only the market that reached cap, while other configured markets continue.
  - Updated live env selector list to include Fed + requested additional markets:
    - March no-change,
    - March -25bps cut,
    - April no-change,
    - April -25bps cut,
    - Trump/Kevin Warsh Fed Chair nomination market.
  - Runtime cap update in `.env`:
    - `EVPOLY_MM_SINGLE_MARKET_MAX_ENTRY_FILLS` set from `5` -> `0` (unlimited; no per-market auto-stop on fill count).

### 2026-03-01
- `evcurve_v1` chase overfill hardening in `src/main.rs` + `.env`:
  - Added first-fill stop for EVcurve chase loops:
    - `EVPOLY_EVCURVE_CHASE_STOP_ON_FIRST_FILL` (default enabled in env)
    - On first detected fill notional in a chase cycle, bot cancels scope BUY orders and exits chase.
  - Added stricter single-live-order handling inside chase:
    - if multiple open pending rows are detected for EVcurve scope, cancel-all and pause re-place.
    - added pending-action gate with cancel-settle wait and action-timeout guard to prevent rapid cancel/place overlap.
  - Added EVcurve-specific entry cap:
    - `EVPOLY_MAX_EVCURVE_ENTRIES_PER_TIMEFRAME_PERIOD=1`
    - worker cap routing now uses this for `evcurve_v1` instead of global reactive default.
  - New EVcurve chase env controls:
    - `EVPOLY_EVCURVE_CHASE_CANCEL_SETTLE_MS`
    - `EVPOLY_EVCURVE_CHASE_ACTION_TIMEOUT_MS`
  - Affects EVcurve chase execution across enabled symbols/timeframes (not MM reward flow).
  - Runtime adjustment:
    - `EVPOLY_EVCURVE_CHASE_STOP_ON_FIRST_FILL` set to `false` in `.env` so chase can continue filling remainder toward target size (instead of stopping after first partial fill).
  - Checkpoint-late tolerance runtime override:
    - set `EVPOLY_EVCURVE_MAX_LATE_MS=60000` in `.env` (effective late-window widening without changing code default in `src/evcurve.rs`).

- `evcurve_v1` size scale-up in `.env` (2x sizing rollout):
  - Doubled non-D1 and D1 strategy caps:
    - `EVPOLY_EVCURVE_STRATEGY_CAP_USD`: `10000 -> 20000`
    - `EVPOLY_EVCURVE_D1_STRATEGY_CAP_USD`: `6000 -> 12000`
  - Doubled D1 fixed size:
    - `EVPOLY_EVCURVE_D1_SIZE_USD`: `500 -> 1000`
  - Doubled per-scope caps and explicitly set 1h/4h caps (to ensure true 2x vs default fallbacks):
    - `15m`: BTC/ETH `500 -> 1000`, SOL/XRP `200 -> 400`
    - `1h`: BTC/ETH `600`, SOL/XRP `300` (new explicit overrides)
    - `4h`: BTC/ETH `800`, SOL/XRP `400` (new explicit overrides)
    - `1d`: all symbols `1000 -> 2000`
  - Affects `BTC/ETH/SOL/XRP` across `15m/1h/4h/1d`.

- `premarket_v1` size scale-up in `.env` (2x side budgets):
  - `EVPOLY_PREMARKET_SIDE_BUDGET_5M_USD`: `200 -> 400`
  - `EVPOLY_PREMARKET_SIDE_BUDGET_15M_USD`: `400 -> 800`
  - `EVPOLY_PREMARKET_SIDE_BUDGET_1H_USD`: `600 -> 1200`
  - `EVPOLY_PREMARKET_SIDE_BUDGET_4H_USD`: `800 -> 1600`
  - Symbol multipliers unchanged (`BTC 1.0`, `ETH/SOL/XRP 0.5`), so effective per-symbol budget doubles.
  - Affects `BTC/ETH/SOL/XRP` across `5m/15m/1h/4h`.

- `liqaccum_v1` multi-asset rollout backport from `liqaccum-normal-size` into `Poly-builder`:
  - Strategy loop now runs across `BTC/ETH/SOL/XRP` (using `premarket_assets`) instead of single-symbol behavior.
  - Added per-timeframe enable parsing in config:
    - `EVPOLY_LIQACCUM_TIMEFRAMES` (supports `5m,15m,1h,4h` and optional `1d` parsing fallback).
  - Added per-asset sizing multipliers in config:
    - `EVPOLY_LIQACCUM_ASSET_MULT_BTC`
    - `EVPOLY_LIQACCUM_ASSET_MULT_ETH`
    - `EVPOLY_LIQACCUM_ASSET_MULT_SOL`
    - `EVPOLY_LIQACCUM_ASSET_MULT_XRP`
  - Added `4h`-specific LiqAccum sizing/buffer/slice config paths in `src/strategy_liqaccum.rs`:
    - `EVPOLY_LIQACCUM_BUFFER_BPS_4H`
    - `EVPOLY_LIQACCUM_BASE_USD_4H`
    - `EVPOLY_LIQACCUM_MAX_USD_4H`
    - `EVPOLY_LIQACCUM_SLICE_COUNT_4H`
  - LiqAccum discovery switched to per `(timeframe, asset)` cache/backoff discovery loop in `src/main.rs` (no longer dependent on single `reactive_market_cache` slot per timeframe).
  - Direction locks/reject counters are now scoped by `(timeframe, period, market_key)` to prevent cross-symbol lock collisions.

- `liqaccum_v1` adaptive reprice upgrade aligned to EVcurve proxy math, with down-only replace policy:
  - Reprice now uses EVcurve proxy source selection via `read_evcurve_proxy_snapshot`:
    - Coinbase proxy for `5m/15m/4h`
    - Binance proxy for `1h`
  - Reprice live cap now computed with shared adaptive chase math (`compute_live_fair_state`) using proxy-vs-PM dislocation and buffers.
  - Added LiqAccum proxy/PM staleness env:
    - `EVPOLY_LIQACCUM_PROXY_STALE_MS`
    - `EVPOLY_LIQACCUM_PM_QUOTE_FRESH_MS`
  - Reprice remains explicitly **down-only**:
    - only cancels/replaces when new target bid is lower than working bid by min tick delta,
    - never chases up.
  - Added/extended LiqAccum reprice telemetry payload with proxy/dislocation fields in `src/main.rs`.

- `mm_rewards_v1` single-market canary retarget + sizing model update:
  - Retargeted MM single-market test in `.env` to:
    - `https://polymarket.com/event/fed-decision-in-march-885/will-there-be-no-change-in-fed-interest-rates-after-the-march-2026-meeting`
  - Kept one-market fill stop guard unchanged:
    - `EVPOLY_MM_SINGLE_MARKET_MAX_ENTRY_FILLS=5`
- `mm_rewards_v1` size semantics changed from **USD notional** to **share count**:
  - Replaced config keys in `src/mm/mod.rs` + `.env`:
    - `EVPOLY_MM_QUOTE_USD_PER_SIDE` -> `EVPOLY_MM_QUOTE_SHARES_PER_SIDE`
    - `EVPOLY_MM_REFILL_USD_PER_FILL` -> `EVPOLY_MM_REFILL_SHARES_PER_FILL`
  - Quote planner now carries `size_shares` (`src/mm/quote_manager.rs`).
  - Runtime converts shares to notional at submit-time (`size_usd = shares * submit_price`) so existing arbiter/trader pipeline remains compatible (`src/main.rs` MM loop).
  - Added MM telemetry fields for shares:
    - `size_shares`, `target_size_shares`.
- `mm_rewards_v1` submit-cap removal for per-token period:
  - Removed MM-specific submit cap env usage from worker gating in `src/main.rs`.
  - `EVPOLY_MAX_MM_REWARDS_SUBMITS_PER_TOKEN_PERIOD` is no longer used.
  - MM submit scope is now bounded by other guards (entry cap, arbiter caps, fill-stop, quote logic), not by submit-count cap.

### 2026-02-28
- `mm_rewards_v1` emergency safety + single-market canary mode (live control hardening):
  - Added single-market runtime override in `src/mm/mod.rs` + `src/main.rs`:
    - `EVPOLY_MM_SINGLE_MARKET_SLUG`
    - `EVPOLY_MM_SINGLE_MARKET_TARGET`
    - `EVPOLY_MM_SINGLE_MARKET_MAX_ENTRY_FILLS`
  - `EVPOLY_MM_SINGLE_MARKET_SLUG` now accepts:
    - plain slug,
    - `event_slug/market_slug`,
    - full Polymarket event URL (`.../event/<event_slug>/<market_slug>`).
    - When two slugs are provided, MM resolves by event slug and enforces exact market slug match (prevents selecting wrong market in multi-market events).
  - Behavior:
    - when `EVPOLY_MM_SINGLE_MARKET_SLUG` is set, MM ignores 4-mode discovery and trades only that slug.
    - after `N` entry fills on that condition (`ENTRY_FILL` distinct order IDs), MM cancels remaining working quotes and fully stops quoting for that run.
  - Added YES/NO token mapping support in `select_token_id_for_direction` so MM can quote non-UP/DOWN binary events safely.
  - Added strict MM notional guard in `src/trader.rs`:
    - `mm_rewards` entries are now rejected if Polymarket min-size enforcement would upsize notional materially above requested budget (prevents accidental large notional fills from order-floor mismatch).
  - Follow-up fix in `src/trader.rs`:
    - MM notional guard now uses the actual request notional (`size_override * price`) rather than global fixed trade amount, so normal `$20` MM orders pass while oversized auto-upsize attempts are blocked.
  - Updated `.env` canary config:
    - only one MM market enabled,
    - quote/refill size raised to `$20` per side,
    - single target slug set to `us-strikes-iraq-by-march-7`,
    - hard stop after `5` MM entry fills,
    - raised MM arbiter cap headroom to avoid cap-side false blocks during canary.

- `mm_rewards_v1` initial rollout (canary-capable execution path):
  - Added new MM module namespace:
    - `src/mm/mod.rs`
    - `src/mm/reward_scanner.rs`
    - `src/mm/quote_manager.rs`
    - `src/mm/scoring_guard.rs`
    - `src/mm/risk_supervisor.rs`
    - `src/mm/inventory_engine.rs`
    - `src/mm/ws_router.rs`
    - `src/mm/metrics.rs`
  - Added strategy ID and runtime integration:
    - `STRATEGY_ID_MM_REWARDS_V1 = "mm_rewards_v1"` in `src/strategy.rs`.
    - Exported MM module from `src/lib.rs`.
    - Added MM strategy cap env support in `src/arbiter.rs`:
      - `EVPOLY_ARB_STRAT_MM_REWARDS_MAX_USD`.
  - Added new execution mode in `src/trader.rs`:
    - `EntryExecutionMode::MmRewards` (`entry_mode="mm_rewards"`).
    - Added dedicated TTL cancel maintenance path keyed by:
      - `EVPOLY_MM_QUOTE_TTL_MS`.
    - Added post-only alias env support:
      - `EVPOLY_ENTRY_POST_ONLY_MM_REWARDS`.
  - Main runtime loop integration in `src/main.rs`:
    - New startup config + boot summary via env-based MM config (`mm::MmRewardsConfig`).
    - New loop discovers configured market per mode, builds paired UP/DOWN maker quote plan, submits via arbiter/trader, and reprices by cancel+replace when quote drifts.
    - Refill behavior supported by re-submitting fresh side size when no open pending order is present.
    - Emits MM telemetry events:
      - `mm_rewards_quote_plan`
      - `mm_rewards_quote_submitted`
      - `mm_rewards_skip_*`
      - `mm_rewards_heartbeat`
  - Added MM-specific entry invariant caps in `src/main.rs` worker path to avoid timeframe-cap starvation against two-sided quoting:
    - `EVPOLY_MAX_MM_REWARDS_ENTRIES_PER_TIMEFRAME_PERIOD` (default `20`)
    - `EVPOLY_MAX_MM_REWARDS_SUBMITS_PER_TOKEN_PERIOD` (default `entries*6`)
  - Canary safety refinements:
    - Quote manager now places maker bids one tick below top bid (or two ticks below ask fallback) to reduce post-only cross rejects (`src/mm/quote_manager.rs`).
    - Added explicit ask-aware clamp (`price <= best_ask - 2*ticks`) in maker quote construction for MM bids to further reduce post-only crosses during fast book shifts.
    - MM live submit path now enforces order-floor gate before submit:
      - skip when configured canary size is below required market minimum notional/shares.
      - emits `mm_rewards_skip_below_order_floor` with required floor values.
  - New MM env surface (defaults in code, override in `.env`):
    - `EVPOLY_STRATEGY_MM_REWARDS_ENABLE`
    - `EVPOLY_MM_HARD_DISABLE`
    - `EVPOLY_MM_MODE` (`shadow|live`)
    - `EVPOLY_MM_POLL_MS`
    - `EVPOLY_MM_QUOTE_TTL_MS`
    - `EVPOLY_MM_MAX_ACTIVE_MARKETS_PER_MODE`
    - `EVPOLY_MM_QUOTE_USD_PER_SIDE`
    - `EVPOLY_MM_REFILL_USD_PER_FILL`
    - `EVPOLY_MM_MIN_PRICE`
    - `EVPOLY_MM_MAX_PRICE`
    - `EVPOLY_MM_MIN_TICK_MOVE`
    - `EVPOLY_MM_ENABLE_QUIET_STACK`
    - `EVPOLY_MM_ENABLE_TAIL_BIASED`
    - `EVPOLY_MM_ENABLE_SPIKE`
    - `EVPOLY_MM_ENABLE_SPORTS_PREGAME`
    - `EVPOLY_MM_TARGET_QUIET_STACK`
    - `EVPOLY_MM_TARGET_TAIL_BIASED`
    - `EVPOLY_MM_TARGET_SPIKE`
    - `EVPOLY_MM_TARGET_SPORTS_PREGAME`

- `liqaccum_v1` execution update: adaptive **down-only** repricing for resting BUY orders:
  - Added runtime reprice loop inside LiqAccum strategy scheduler (`src/main.rs`) that checks open `liqaccum` pending BUY orders per scope (`timeframe + period + token`) and reprices only when market moves against resting bid.
  - Behavior:
    - fetches live PM top (`best_bid`/`best_ask`) for token,
    - computes `live_cap` from current plan `reservation_price`,
    - if `working_price` is above `target_bid=min(book, live_cap)` by at least configured ticks, cancels scope orders and submits replacement lower,
    - **no chase-up** (down-only).
  - Added anti-churn guards:
    - cooldown per scope,
    - duplicate target-price suppression.
  - New env/config in `src/strategy_liqaccum.rs`:
    - `EVPOLY_LIQACCUM_ADAPTIVE_REPRICE_ENABLE` (default `true`)
    - `EVPOLY_LIQACCUM_ADAPTIVE_REPRICE_COOLDOWN_MS` (default `1000`)
    - `EVPOLY_LIQACCUM_ADAPTIVE_REPRICE_MIN_TICKS` (default `1`)
  - Added telemetry event:
    - `liqaccum_reprice_submitted`
  - Touched code paths:
    - `src/main.rs`
    - `src/strategy_liqaccum.rs`

- `evcurve_v1` adaptive chase hardening pass (plan items 2/5/6/8/9/10) in `src/main.rs` + `src/adaptive_chase.rs`:
  - Added explicit re-anchor policy wiring for adaptive chase anchors:
    - interval-based (`EVPOLY_CHASE_REANCHOR_INTERVAL_MS`)
    - proxy move threshold (`EVPOLY_CHASE_REANCHOR_PROXY_BPS`)
    - dislocation move threshold (`EVPOLY_CHASE_REANCHOR_DISLOC_BPS`)
  - Added race-safety and duplicate suppression in replace/place path:
    - duplicate target suppress window (`EVPOLY_CHASE_DUPLICATE_REPLACE_SUPPRESS_MS`)
    - post-cancel fill re-read and remainder recompute before re-submit
    - nonce-tagged cancel/place telemetry fields for in-flight action tracing
  - Enforced single-writer ownership per EVcurve chase scope (`strategy/symbol/tf/period/token/sub_strategy`) so only one worker can mutate one chase intent scope at a time.
  - Replaced hard-fail behavior for live decision compute issues with safe fallback pause:
    - cancel stale remainder, pause/retry, and only hard-stop after capped failures (`EVPOLY_CHASE_FALLBACK_FAIL_LIMIT`)
  - Added per-intent replace budget (`EVPOLY_CHASE_MAX_REPLACES_TOTAL`) in addition to per-second replace breaker.
  - Added cutoff precedence handling (`EVPOLY_CHASE_CUTOFF_GUARD_MS`):
    - if cutoff is reached during/after replace/place path, scope cancel is forced and chase exits immediately.

- `evcurve_v1` execution-layer upgrade: **Adaptive Chase-Limit v1** (BUY chase path, non-D1 phases):
  - Replaced static 10s chase submit loop with event-driven adaptive repricing in `src/main.rs` EVcurve chase worker.
  - Chase now reacts to:
    - Coinbase proxy trade notify (or fallback timer),
    - PM top-of-book change detection,
    - fallback safety tick.
  - Added live cap computation pipeline (`max_buy_live`) using:
    - strategy model cap (`max_buy_curve_live` from EVcurve decision),
    - proxy-vs-PM dislocation fair shift,
    - edge/vol/stale/expiry buffers,
    - hard-stop cancel when live cap drops below min buy.
  - Added remainder-aware replace behavior:
    - preserves filled notional,
    - cancels scoped open EVcurve pending BUY orders for replace,
    - re-submits only remainder with updated target bid,
    - per-second replace breaker + min replace interval.
  - Phase-3 FAK now uses adaptive `max_buy_live` guard instead of static initial cap.
  - Added structured telemetry events:
    - `evcurve_chase_live_update`
    - `evcurve_chase_replace_cancel`
    - `evcurve_chase_state`
    - `evcurve_chase_hard_stop`
  - Added new env/config surface (execution layer):
    - `EVPOLY_CHASE_KAPPA_5M`
    - `EVPOLY_CHASE_KAPPA_15M`
    - `EVPOLY_CHASE_KAPPA_1H`
    - `EVPOLY_CHASE_KAPPA_4H`
    - `EVPOLY_CHASE_EDGE_BUFFER`
    - `EVPOLY_CHASE_VOL_A`
    - `EVPOLY_CHASE_VOL_B`
    - `EVPOLY_CHASE_MIN_REPLACE_MS`
    - `EVPOLY_CHASE_MIN_PRICE_IMPROVE_TICKS`
    - `EVPOLY_CHASE_FALLBACK_TICK_MS`
    - `EVPOLY_CHASE_MAX_REPLACES_PER_SEC`
    - `EVPOLY_CHASE_HEALTHY_RESUME_MS`
    - `EVPOLY_CHASE_STALE_BUFFER_MULT`
    - `EVPOLY_CHASE_EXPIRY_BUFFER_MAX`
  - Code paths touched:
    - `src/adaptive_chase.rs` (new pure computation/state module + unit tests)
    - `src/main.rs` (EVcurve chase worker integration + telemetry)
    - `src/trader.rs` (`cancel_pending_orders_for_scope`)
    - `src/tracking_db.rs` (`list_open_pending_orders_for_strategy_scope`)
    - `src/lib.rs` (module export)

- `evcurve_v1` chase fill-accounting hard fix:
  - Fixed EVcurve chase filled-notional tracking key in `src/main.rs` to match trader trade-key format.
  - `trade_key LIKE` now uses `..._evcurve_%_<tf>_<strategy>_tickN` (removed sub-strategy suffix mismatch).
  - Impact: prevents over-submission in chase phases when fills are present but were previously missed by the matcher.
  - Code path touched:
    - `src/main.rs` (`evcurve_trade_key_like` in EVcurve worker task).

- `sessionband_v1` sizing update (live env):
  - Per-symbol fixed size changed in `.env`:
    - `BTC=600`, `ETH=600`, `SOL=300`, `XRP=125` USD.
  - Per-scope caps aligned to new size map:
    - `EVPOLY_SESSIONBAND_SCOPE_CAP_<SYMBOL>_<TF>` updated for `5m/15m/1h/4h`.

- `evcurve_v1` 15m sizing update (live env):
  - Added explicit 15m scope-cap overrides in `.env`:
    - `EVPOLY_EVCURVE_SCOPE_CAP_BTC_15M=500`
    - `EVPOLY_EVCURVE_SCOPE_CAP_ETH_15M=500`
    - `EVPOLY_EVCURVE_SCOPE_CAP_SOL_15M=200`
    - `EVPOLY_EVCURVE_SCOPE_CAP_XRP_15M=200`
  - Affects EVcurve classic checkpoints on `15m` (requested size uses scope cap in `src/main.rs`).

- `evcurve_v1` 15m checkpoint/routing updates:
  - Disabled 15m `tick3` (`T-120s`) via new env-gated checkpoint skip:
    - `EVPOLY_EVCURVE_15M_DISABLE_T3=true`
  - Added dedicated 15m `tick2` (`T-180s`) max-buy curve override:
    - `EVPOLY_EVCURVE_15M_T2_MAXBUY_CURVE=0:0.99,0.01:0.98,0.02:0.96,0.03:0.94,0.04:0.92,0.05:0.9,0.06:0.88,0.07:0.85,0.08:0.75,0.09:0.72,0.1:0.69,0.11:0.67,0.12:0.65,0.13:0.64,0.14:0.63,0.15:0.62`
  - Added 15m `tick2` execution gate to disable phase-3 FAK fallback (chase-only for remaining size):
    - `EVPOLY_EVCURVE_15M_T2_NO_FAK=true`
  - New event logs:
    - `evcurve_t2_curve_override`
    - `evcurve_phase3_fak_disabled`
  - Code paths touched:
    - `src/evcurve.rs` config/env additions.
    - `src/main.rs` checkpoint skip (15m t3), t2 curve override gating, and t2 chase-only stop.

- `endgame_sweep_v1` tick schedule (live env):
  - Removed `t0` (`T-6000ms`) from active endgame ticks.
  - Updated `.env`:
    - `EVPOLY_ENDGAME_TICK_OFFSETS_MS=3000,1000,100`
    - `EVPOLY_ENDGAME_TICK_OFFSETS_SEC=3,1` (fallback only when ms list is unset)
    - `EVPOLY_ENDGAME_PER_TICK_USD_BY_OFFSET=3000:600,1000:1000,100:1000`
    - `EVPOLY_ENDGAME_PER_TICK_USD_BY_SYMBOL` remapped to 3 tick slots.
- `sessionband_v1` tau gating:
  - Added optional tau allowlist env parsing in `src/sessionband.rs`:
    - `EVPOLY_SESSIONBAND_ALLOWED_TAU_SEC` (comma-separated seconds, e.g. `1,2`).
  - Added runtime gate in `src/main.rs` SessionBand loop:
    - Skip and log `sessionband_skip_tau_not_allowed` when current `tau_sec` is not allowed.
  - Updated `.env` default live setting:
    - `EVPOLY_SESSIONBAND_ALLOWED_TAU_SEC=1,2`.

### 2026-02-27
- Execution accounting / fill attribution:
  - Fixed limit-fill attribution path in `src/trader.rs` to skip token-balance delta fill detection for real exchange orders.
  - Exchange order fills are now treated as canonical only via order-level reconcile (`get_order` / pending-order status), avoiding multi-order over-count when several ladder orders share the same token.
  - Impact: affects fill accounting and PnL tracking quality across limit-based strategies (`premarket_v1`, `sessionband_v1`, `evcurve_v1`, `endgame_sweep_v1` when applicable), no strategy signal/risk logic change.
- EVcurve D1:
  - Lowered EV gap gate from `0.15` to `0.08` in `.env`.
  - Env changed: `EVPOLY_EVCURVE_D1_EV_GAP=0.08`.
  - Affects strategy path `evcurve_v1` daily (`1d`) `d1_ev` sub-rule for `BTC/ETH/SOL/XRP`.
- EVcurve D1 execution flow:
  - Removed D1 phase-3 `FAK` fallback; D1 now uses minute re-eval + limit chase only.
  - Added D1 cancel/unlock rule: if current best bid exceeds `max_buy_hold`, stop active signal and allow later hourly D1 signal for same symbol/day.
  - Added D1 single-active-signal lane (`symbol + day`) to prevent parallel workers.
  - Added D1 post-fill block rule: if a signal reaches `>=50%` of target notional fill, block further D1 signals for that symbol/day.
  - New event logs: `evcurve_d1_signal_started`, `evcurve_d1_recheck_pass`, `evcurve_d1_signal_canceled`, `evcurve_d1_signal_blocked_after_fill`, `evcurve_d1_signal_unlocked`.
  - Code path touched: `src/main.rs` EVcurve D1 worker/gating state.

### 2026-02-26
- Redemption lifecycle:
  - Changed scheduled redemption sweep cadence from hourly to interval-based with env control in `src/trader.rs`.
  - Added new env: `EVPOLY_REDEMPTION_SWEEP_INTERVAL_HOURS` (default `1`, clamped `1..24`).
  - `.env` now sets `EVPOLY_REDEMPTION_SWEEP_INTERVAL_HOURS=4` (run every 4h at configured minute).
  - Scheduler trigger label updated from `hourly_schedule` to `scheduled_interval`.
  - Retry log text updated to “next scheduled sweep” to match non-hourly cadence.
- EVcurve:
  - Removed `5m` from active EVcurve timeframes in `.env`:
    - `EVPOLY_EVCURVE_TIMEFRAMES=15m,1h,4h,1d`
  - Removed `T-1m` checkpoints from classic EVcurve:
    - `15m`: now `T-5/T-4/T-3/T-2`
    - `1h`: now `T-15/T-10/T-5`
  - Added symbol-specific max-buy curve overrides:
    - `EVPOLY_EVCURVE_MAXBUY_CURVE_SOL`
    - `EVPOLY_EVCURVE_MAXBUY_CURVE_XRP`
  - SOL/XRP override curve:
    - `0%->0.99, 1%->0.98, 2%->0.96, 3%->0.94, 4%->0.90, 5%->0.87, 6%->0.84, 7%->0.79, 8%->0.76, 9%->0.73, 10%->0.70`
- Near-base skip threshold:
  - Updated global near-base threshold to `1.0 bps` in `.env`:
    - `EVPOLY_NEAR_BASE_SKIP_BPS=1.0`
  - Applies to strategies enabled for near-base gating (`endgame`, `evcurve`, `sessionband`).

### 2026-02-28
- Entry execution parallel lanes (arbiter workers):
  - Split arbiter submit queue into two independent worker pools in `src/main.rs`:
    - `critical` lane for `endgame_sweep_v1`, `endgame_revert_v1`, `sessionband_v1`.
    - `normal` lane for other entry strategies (`evcurve_v1`, `premarket_v1`, `evsnipe_v1`, `mm_rewards_v1`, reactive/ladder/liqaccum paths).
  - Purpose: allow close-time strategies (`endgame` + `sessionband`) to enqueue/execute concurrently without waiting behind non-critical traffic.
  - New env knobs:
    - `EVPOLY_ENTRY_WORKER_COUNT_CRITICAL` (default `2`)
    - `EVPOLY_ENTRY_WORKER_COUNT_NORMAL` (fallback to legacy `EVPOLY_ENTRY_WORKER_COUNT`, default `4`)
  - Worker telemetry now includes `worker_pool` field on key drop/backoff/cap events for lane visibility.

### 2026-03-01
- `evcurve_v1` overfill guard hardening (execution-layer):
  - Fixed cancel-status handling in `src/trader.rs` (`apply_cancel_candidates` and stale-ladder cancel reconcile path):
    - A cancel ACK is no longer treated as terminal `CANCELED`.
    - When `get_order` is still non-terminal (or temporarily fetch-fails after cancel ACK), status is kept `OPEN` conservatively.
  - Impact: prevents false local close of still-live orders, which could allow a new EVcurve chase submit while the previous order can still fill.

- `evcurve_v1` single-live-order guard (memory + DB):
  - Added `Trader::has_unconfirmed_buy_order_for_scope(...)` in `src/trader.rs`.
  - EVcurve chase loop in `src/main.rs` now requires BOTH checks before placing a new order:
    - DB open pending rows (`pending_orders`), and
    - in-memory unconfirmed pending trade guard.
  - New telemetry event: `evcurve_chase_pending_guard_mem_only` when memory sees an open order but DB has not reflected it yet.
  - Affects EVcurve chase submits across symbols/timeframes; goal is one live working order per EVcurve lifecycle scope at a time.

- `evcurve_v1` submit-cap + fill-sum guard tightening:
  - Added EVcurve-specific submit cap env and routing in `src/main.rs`:
    - New env: `EVPOLY_MAX_EVCURVE_SUBMITS_PER_TOKEN_PERIOD` (set in `.env`).
    - Arbiter submit-cap check now uses this EVcurve-specific cap for `evcurve_v1` instead of the generic submit cap.
  - Hardened EVcurve chase filled-notional recompute in `src/tracking_db.rs`:
    - `sum_entry_fill_notional_for_trade_key_like(...)` now dedupes `ENTRY_FILL` + `ENTRY_FILL_RECONCILED` by normalized order identity before summing.
    - Prevents duplicate counted notional when both canonical and reconciled fill events exist for the same order.
  - Added regression test:
    - `sum_entry_fill_notional_trade_key_like_dedupes_fill_and_reconciled_same_order`.
  - Affects EVcurve execution sizing/chase remainder logic across timeframes/symbols.

- `evcurve_v1` adaptive chase upgraded to ladder-style repricing (BUY side):
  - Added EVcurve ladder chase mode in `src/main.rs`:
    - Maintains 10-rung equal-size BUY ladder (1-cent spacing) per active chase scope.
    - Reprices by canceling stale rungs and reposting missing rungs as fair/max-buy shifts.
    - Supports both downshift and upshift repricing while respecting live `max_buy_live`.
  - Ladder path remains event-driven (`proxy_tick`, `pm_top_change`, fallback tick), with live `max_buy_curve` recompute from EVcurve decision logic already in loop.
  - Added T-minus cutoff stop for chase ladder across EVcurve timeframes:
    - Cancel remaining open ladder orders at `close_ts - cutoff_sec` (default 60s).
  - Added selective cancel helper in `src/trader.rs`:
    - `cancel_pending_orders_for_scope_order_ids(...)` for scoped partial cancel/reprice.
  - New EVcurve env knobs consumed in runtime (`.env`):
    - `EVPOLY_EVCURVE_CHASE_LADDER_ENABLE`
    - `EVPOLY_EVCURVE_CHASE_LADDER_RUNGS`
    - `EVPOLY_EVCURVE_CHASE_LADDER_STEP_CENTS`
    - `EVPOLY_EVCURVE_CHASE_LADDER_CUTOFF_SEC`
  - Affects `evcurve_v1` chase execution path for all enabled EVcurve timeframes/symbols.

- `evcurve_v1` daily (`1d`) re-entry guard tightened after first fill:
  - Added DB-backed guard in `src/main.rs` before launching a D1 signal:
    - Skip new D1 signals when `count_entry_fills_for_strategy_condition(strategy_id, condition_id) > 0`.
    - New event log: `evcurve_d1_signal_skip_existing_fill`.
  - Updated in-worker D1 block rule:
    - `blocked_after_fill` now latches on **any** positive `final_filled_notional` (was `fill_ratio >= 0.5`).
  - Effect: once EVcurve gets any D1 entry fill for the day/condition, later D1 checkpoints for that symbol/day are blocked (including after restart).

- `evcurve_v1` daily (`1d`) first-fill hard stop fixed (minute chase):
  - Added explicit D1 in-loop stop-on-first-fill in `src/main.rs`:
    - As soon as `filled_notional > 0`, cancel remaining pending scope orders and stop the D1 chase loop.
    - New event log: `evcurve_d1_stop_on_first_fill`.
  - Added stronger pre-launch D1 fill gate in `src/main.rs`:
    - Existing condition-wide guard kept.
    - New token+period and symbol+period guards added via:
      - `count_entry_fills_for_strategy_token_period(...)`
      - `count_entry_fills_for_strategy_symbol_period(...)`
    - Prevents reopening D1 chase for same symbol/day after any prior fill, even if token side changes.
  - Added runtime D1 safety stop in chase loop:
    - If symbol+day already has any entry fill, cancel pending D1 scope orders and stop worker.
    - New event log: `evcurve_d1_stop_existing_symbol_fill`.
  - Added new tracking DB helper in `src/tracking_db.rs`:
    - `count_entry_fills_for_strategy_token_period(strategy_id, timeframe, period_timestamp, token_id)`.
    - `count_entry_fills_for_strategy_symbol_period(strategy_id, timeframe, period_timestamp, asset_symbol)`.
  - Affects: `evcurve_v1` timeframe `1d`, all enabled symbols (`BTC/ETH/SOL/XRP`).

- `mm_rewards_v1` upgraded to ladder+inventory execution for single-market reward test:
  - Replaced single-quote per side with fixed ladder quoting (`BBO1..BBO5`) using equal per-level size (`EVPOLY_MM_QUOTE_SHARES_PER_SIDE`) and `EVPOLY_MM_LADDER_LEVELS`.
  - Added BBO participation guard: if per-level size exceeds `EVPOLY_MM_BBO1_MAX_SIZE_FRACTION` of top bid size, ladder shifts down one level (`BBO2..BBO6`) for that side.
  - Added PM-trade-driven toxic-flow handling (via Polymarket WS trade snapshots):
    - when a fresh trade prints through/at BBO1, bot cancels both BUY ladders and pauses repricing for `EVPOLY_MM_BBO_HIT_COOLDOWN_SEC`.
  - Added inventory skew control + maker weak-side exit:
    - uses live token balances; when weak-side inventory is larger than favored side, weak-side BUY ladder is paused/canceled.
    - weak-side inventory posts maker `SELL` GTC exits above average entry by `EVPOLY_MM_WEAK_EXIT_PROFIT_MIN` (no merge path).
    - favored side remains build/hold biased via `EVPOLY_MM_FAVOR_SIDE`.
  - Added Polymarket WS trade snapshot plumbing (`src/polymarket_ws.rs`, `src/api.rs`) and DB helper for average entry lookup (`src/tracking_db.rs`).
  - Updated MM env surface in `.env`:
    - `EVPOLY_MM_LADDER_LEVELS=5`
    - `EVPOLY_MM_QUOTE_SHARES_PER_SIDE=200` (=> 1,000 shares per side total ladder)
    - `EVPOLY_MM_BBO_HIT_COOLDOWN_SEC=10`
    - `EVPOLY_MM_BBO1_MAX_SIZE_FRACTION=0.25`
    - `EVPOLY_MM_FAVOR_SIDE=UP`
    - `EVPOLY_MM_WEAK_EXIT_ENABLE=true`
    - `EVPOLY_MM_WEAK_EXIT_PROFIT_MIN=0.01`
    - `EVPOLY_MM_WEAK_EXIT_MAX_ORDER_SHARES=200`

- `mm_rewards_v1` ladder precision + weak-exit churn hardening:
  - Fixed ladder dedupe to use tick-key matching instead of cent-key matching in `src/main.rs`:
    - prevents `0.001` tick ladders from collapsing (e.g. `BBO1` + `BBO5` only).
    - BUY open-order matching now uses `price_key_tick(price, tick_size)` for target/existing ladder reconciliation.
  - Hardened MM order-terminal classification in `src/main.rs`:
    - MM stale-open and weak-exit tracking now treat only `Matched`/`Canceled` as terminal.
    - `Unmatched` no longer auto-forces stale/replace in MM ladder logic.
  - Added safer order-lookup error handling for MM:
    - transient `get_order` errors no longer auto-mark BUY rows as `STALE`.
    - terminal lookup errors are restricted to explicit missing/not-found patterns.
  - Added weak-side maker-exit submit throttle in `src/main.rs`:
    - tracks last submit per token and enforces minimum resubmit interval (derived from poll interval, floor 1s).
    - prevents hot-loop re-submits of the same weak-side `SELL` order.
    - adds failure backoff cooldown on weak-side exit submit errors (e.g. balance/allowance), with longer retry delay for balance-related failures.
  - Effect:
    - cleaner `BBO1..BBO5` ladder behavior on fine-tick books,
    - lower MM cancel/repost churn,
    - reduced pressure that contributed to WS lag bursts.

- `mm_rewards_v1` ladder stability + replenish timing fix:
  - Reduced MM rung dead-time caused by worker idempotency recent-done blocking:
    - added strategy-specific cooldown override for MM logical keys in `src/entry_idempotency.rs`.
    - new env override: `EVPOLY_ENTRY_RECENT_DONE_TTL_MM_REWARDS_MS` (set to `750` in `.env`).
  - Reworked MM BUY ladder reprice cancellation logic in `src/main.rs`:
    - no longer cancels whole scope whenever any row is off-target.
    - now cancels only duplicate same-price rows in active BUY ladder management (off-target rows are left to TTL/cooldown paths).
    - uses selective cancel API `cancel_pending_orders_for_scope_order_ids(...)` for less churn.
    - added per-token reprice cooldown (>=10s) before another BUY ladder reprice cancel pass can run.
  - Relaxed weak-side skew pause threshold in `src/main.rs`:
    - weak-side BUY pause now triggers only when weak inventory exceeds favored side by more than one full ladder size (`quote_shares_per_side * ladder_levels`), instead of one rung size.
  - Effect:
    - ladder levels persist longer,
    - missing rung replenishment happens faster,
    - fewer rapid cancel/repost cycles and fewer `RecentDone` rung lockouts.
  - Increased MM rewards arbiter strategy cap in `.env`:
    - `EVPOLY_ARB_STRAT_MM_REWARDS_MAX_USD` changed `1000 -> 10000` to avoid cap-throttling ladder replenishment while holding favored-side inventory.

- `mm_rewards_v1` ladder key + cooldown hit-gate fix (2026-03-01):
  - Fixed MM trade-key collision in `src/trader.rs`:
    - `EntryExecutionMode::MmRewards` key now uses 0.001 precision (`price_mills`) instead of 0.01 (`price_cents`).
    - key now also appends ladder suffix from request id (`l1..l5`) when available.
    - effect: rung-level pending/open dedupe no longer collapses multiple ladder levels into 1-2 keys.
  - Reduced over-aggressive cancel loops in `src/main.rs`:
    - BBO1 toxic-flow cancel is now ignored while cooldown is already active (no cooldown extension spam).
    - BBO1 toxic-flow detection only applies on sides actually quoting BBO1 (`start_offset == 0`), avoiding false full-ladder cancel when side is intentionally shifted to BBO2+.
  - Affects: `mm_rewards_v1` ladder placement/cancel behavior for all symbols/timeframes using MM rewards path.

- `evcurve_v1` size allocation reduced 50% (2026-03-01):
  - Updated EVcurve per-scope caps in `.env` (all symbols/timeframes):
    - `15m`: BTC/ETH `1000 -> 500`, SOL/XRP `400 -> 200`
    - `1h`: BTC/ETH `600 -> 300`, SOL/XRP `300 -> 150`
    - `4h`: BTC/ETH `800 -> 400`, SOL/XRP `400 -> 200`
    - `1d`: all symbols `2000 -> 1000`
  - Updated EVcurve daily fixed size in `.env`:
    - `EVPOLY_EVCURVE_D1_SIZE_USD: 1000 -> 500`
  - Updated EVcurve strategy caps in `.env`:
    - `EVPOLY_EVCURVE_STRATEGY_CAP_USD: 20000 -> 10000`
    - `EVPOLY_EVCURVE_D1_STRATEGY_CAP_USD: 12000 -> 6000`
  - Affects: `evcurve_v1` across `15m/1h/4h/1d` for `BTC/ETH/SOL/XRP`.

- Ops hardening: redemption visibility + DB/live cancel sync + merge sweep (2026-03-02):
  - Redemption sweep observability:
    - `src/trader.rs`: every redemption lifecycle update now emits structured `redemption_attempt_status` and stores recent attempt history (status/attempt/tx/error/rate-limit pause hint) for admin debugging.
    - `GET /admin/redemption/status` now includes `recent_attempts`.
    - New env: `EVPOLY_REDEMPTION_RECENT_ATTEMPTS_LIMIT` (`.env`).
  - MM and strategy cancel path DB/live sync hardening:
    - `src/trader.rs`: `cancel_pending_orders_for_scope*` now also reconciles/cancels DB-tracked OPEN rows that are missing in in-memory `pending_trades` (db-only rows), reducing stuck-open mismatches.
    - Affects all strategies using scope cancel paths, including `mm_rewards_v1`, `evcurve_v1`, `sessionband_v1`, and `endgame_sweep_v1`.
  - Merge complete-sets automation:
    - `src/trader.rs`: added scheduled/manual merge sweep (`merge_complete_sets`) that scans pending inventory per condition, checks live UP/DOWN balances, and merges when `min(UP,DOWN)` exceeds threshold.
    - `src/main.rs`: new admin endpoints:
      - `POST /admin/merge/sweep`
      - `GET /admin/merge/status`
      - `POST /admin/merge/condition?condition_id=0x...` (force single-condition merge test with pre-check output)
    - `.env`: added merge controls:
      - `EVPOLY_MERGE_SWEEP_ENABLE=true`
      - `EVPOLY_MERGE_SWEEP_MINUTE=40`
      - `EVPOLY_MERGE_SWEEP_INTERVAL_HOURS=1`
      - `EVPOLY_MERGE_MIN_PAIRS_SHARES=1`
      - `EVPOLY_MERGE_MAX_CONDITIONS_PER_SWEEP=10`
      - `EVPOLY_MERGE_RECENT_LOG_LIMIT=100`

- `mm_rewards_v1` event-driven execution + anti-pickoff/inventory controls (2026-03-02):
  - Converted MM quote loop in `src/main.rs` to event-driven wakeups from Polymarket WSS book updates:
    - new wait path uses `SharedPolymarketWsState::wait_for_market_update_after(...)` from `src/polymarket_ws.rs`.
    - fallback polling remains enabled via timeout only.
    - new envs in `.env` + `src/mm/mod.rs`:
      - `EVPOLY_MM_EVENT_DRIVEN_ENABLE`
      - `EVPOLY_MM_EVENT_FALLBACK_POLL_MS`
  - Improved reprice/cancel timeliness in `src/main.rs`:
    - fixed ladder reconciliation to cancel BUY orders not in current target ladder (not only duplicates).
    - reduced reprice gating from long static windows to configurable low-latency gate:
      - `EVPOLY_MM_REPRICE_MIN_INTERVAL_MS`
    - added `mm_rewards_ladder_reprice_cancel` telemetry events.
  - Replaced broken BBO1 trade-hit detection (user-trade dependent) with live book adverse-move trigger in `src/main.rs`:
    - detects top-bid down-tick while we had a BBO1 quote and starts 10s cooldown with scope cancel-all.
    - new env:
      - `EVPOLY_MM_ADVERSE_TICK_TRIGGER_TICKS`
    - emits `mm_rewards_bbo1_hit_detected` + `mm_rewards_ladder_cooldown_started`.
  - Added MM constraints cache to avoid per-wake constraints refetch:
    - `EVPOLY_MM_CONSTRAINTS_CACHE_MS`
  - Added inventory caps and side throttles in `src/main.rs`:
    - favored/weak side BUYs now hard-stop when side inventory exceeds cap.
    - new envs:
      - `EVPOLY_MM_MAX_FAVOR_INVENTORY_SHARES`
      - `EVPOLY_MM_MAX_WEAK_INVENTORY_SHARES`
    - emits `mm_rewards_skip_side_inventory_cap`.
  - Strengthened weak-side maker-exit lifecycle telemetry in `src/main.rs`:
    - logs cancel due to no inventory, reprice-cancel, and terminal close with matched shares/estimated edge.
    - events:
      - `mm_rewards_weak_side_exit_canceled_no_inventory`
      - `mm_rewards_weak_side_exit_reprice_cancel`
      - `mm_rewards_weak_side_exit_closed`

- `mm_rewards_v1` robust inventory tracking + strategy-scope isolation (2026-03-03):
  - Added inventory janitor pass in `src/main.rs`:
    - periodically scans MM open inventory from `positions_v2` and injects forced per-market maintenance work items even when auto/target selection rotates away.
    - updates state via `mm_rewards_inventory_janitor_snapshot` telemetry and blocks mixed-scope conditions.
    - new env (optional): `EVPOLY_MM_INVENTORY_JANITOR_SEC` (default 15s in code).
  - Added cross-strategy scope guard in `src/main.rs`:
    - MM now checks open positions by `(condition_id, token_id)` and blocks quoting when non-MM strategy inventory overlaps.
    - emits `mm_rewards_scope_violation_blocked` and writes MM state reason `scope_violation_cross_strategy`.
  - Added DB APIs in `src/tracking_db.rs` for robust inventory reads:
    - `list_open_position_inventory_by_strategy`
    - `list_open_position_strategies_for_condition_token`
  - Improved MM exit SELL order tracking (competition-freeze exits + weak-side exits) in `src/main.rs`:
    - direct SELL submits are now mirrored into `pending_orders` with strategy scope.
    - terminal/cancel transitions update pending status (`FILLED` / `CANCELED` / `STALE`) so DB and live order state stay in sync.

- `mm_rewards_v1` imbalance rebalance + ladder churn fixes (2026-03-03):
  - Rebalance exit now works even when `favor_side_effective=none` in `src/main.rs`:
    - weak-exit path now also targets the currently blocked heavy side when inventory imbalance gate triggers.
    - this enables maker `SELL` reductions on the over-held side instead of only blocking new buys.
    - added `weak_exit_source` telemetry to distinguish `configured_weak_side` vs `imbalance_blocked_side`.
  - Reduced unnecessary MM cancel/reprice churn in `src/main.rs`:
    - reprice decision now keys off top anchor levels only (not bottom rung drift).
    - new config/env in `src/mm/mod.rs`: `EVPOLY_MM_REQUOTE_ANCHOR_LEVELS` (default `2`).
    - prevents full ladder churn from deep-level changes like `0.45 -> 0.44` while top ladder is unchanged.
  - Ladder generation updated in `src/main.rs` (`bid_ladder_prices_from_orderbook`):
    - now builds contiguous 1-tick ladders from an anchor level instead of sparse raw bid-level jumps.
    - removes large rung gaps (e.g. `bb1` then `bb5`) on thin books.
  - Rebalance-side quoting behavior improved in `src/main.rs`:
    - when imbalance is active, opposite-side rebalance quotes no longer auto-deepen due BBO1 max-size guard.
    - helps keep rebalance quotes nearer top of book during inventory repair.

- `mm_rewards_v1` inventory rebalance + requote stability fixes (2026-03-03):
  - Reworked bid ladder generation in `src/main.rs` (`bid_ladder_prices_from_orderbook`):
    - ladder now builds contiguous tick steps from an anchor level instead of sparse raw book jumps.
    - prevents wide rung gaps like `bb1 -> bb5` behavior on thin books.
  - Added anchor-level requote gating for MM BUY ladder reprices in `src/main.rs`:
    - reprice now keys off top-N anchor ladder levels (new `EVPOLY_MM_REQUOTE_ANCHOR_LEVELS`, default `2`) rather than bottom-rung drift.
    - reduces cancel/reprice churn caused by deep ladder-only changes.
  - Added imbalance-aware behavior in side quoting in `src/main.rs`:
    - when imbalance block is active, opposite-side rebalance quotes are allowed to stay closer to top-of-book (less forced deepening from BBO1-size guard).
  - Extended maker SELL exit logic in `src/main.rs`:
    - weak-exit path now also runs for imbalance-blocked token when no explicit favor side is active, so heavy inventory can be reduced via maker exits at profit targets.
    - adds `weak_exit_source` telemetry (`configured_weak_side` vs `imbalance_blocked_side`).
  - Config touched:
    - `src/mm/mod.rs`: new `requote_anchor_levels` with env `EVPOLY_MM_REQUOTE_ANCHOR_LEVELS`.

- `mm_rewards_v1` mode-cap/scoring/stale-suppression hardening (2026-03-03):
  - Fixed per-mode active-cap slot burn in `src/main.rs`:
    - `max_active_markets_per_mode` is now acquired only after side-level gates pass (not at candidate enqueue), so failed candidates do not consume mode slots.
    - affects all MM reward symbols/timeframes/modes.
  - Strengthened stale-order suppression path in `src/main.rs`:
    - stale reconcile now uses rotating cursor (`mm_stale_reconcile_cursor_by_scope`) plus capped-keying behavior.
    - when stale checks are throttled, submit dedupe keys are built from the checked/fresh slice instead of all historical rows, reducing false submit suppression by stale DB rows.
  - Corrected scoring guard q-min inputs in `src/main.rs`:
    - `check_local_scoring(...)` now receives real counterpart quote inputs (live opposite BUY quote when present, otherwise planned opposite ladder quote + size floor), instead of mirrored synthetic side sizes.
    - `q_min_ready` gating now reflects actual UP/DOWN quoteability for MM ladder submits.

- `mm_rewards_v1` imbalance-priority rebalance fix (2026-03-03):
  - Updated `src/main.rs` rebalancing precedence:
    - favor-side logic now applies only when inventory imbalance flag is not active.
    - when imbalance is active, bot selects rebalance mode by live BBO liquidity:
      - `reduce_heavy` (sell heavy side), or
      - `increase_light` (buy light side).
  - Updated weak-exit target selection:
    - weak-exit SELL now only targets imbalance-blocked heavy side during `reduce_heavy`.
    - weak-exit is skipped during `increase_light`.
  - Added rebalance-only gate:
    - when `reduce_heavy` is chosen, light-side rebalance BUY quoting is skipped for that cycle (`mm_rewards_skip_rebalance_reduce_heavy_only`).
  - Updated weak-exit pricing + size behavior in `src/main.rs`:
    - weak-exit profit is now adaptive to live spread ticks + imbalance severity (severe imbalance can flatten at touch).
    - fixed-size `+0.01` behavior is no longer the only driver.
    - weak-exit size now uses effective inventory (`wallet + reserved active exit`) to avoid `200 -> 1` churn from transient balance reads.
    - weak-exit can now run even when `avg_entry_price` is missing in DB by falling back to live BBO reference; this prevents imbalance-repair from stalling on legacy/manual inventory.
  - Added balance fallback cache in MM loop:
    - token balance reads now use last-good cached balance when API timeout/errors occur, reducing false zero-balance states and cancel/repost loops.
  - Added transient zero-balance guard in `src/main.rs`:
    - MM now requires 3 consecutive `0` balance reads before replacing a positive cached balance with `0`.
    - this prevents single-read balance flickers from flipping heavy/light side detection and causing wrong-side rebalance/cancel churn.
    - new telemetry event: `mm_rewards_balance_zero_guard`.
  - Added unconfirmed-zero imbalance skip in `src/main.rs`:
    - if one side reads `0` while the opposite side is positive and the zero streak is not confirmed (`<3`), MM skips imbalance-driven rebalance/weak-exit for that cycle.
    - prevents accidental wrong-side exits (e.g., selling YES when NO is actually heavy) during transient balance read glitches.
    - new telemetry event: `mm_rewards_skip_unconfirmed_zero_imbalance`.
  - Updated imbalance weak-exit arming in `src/main.rs`:
    - when imbalance is active, heavy-side weak-exit SELL is always armed on the blocked token (instead of only when `reduce_heavy` branch is selected).
    - this prevents rebalance stalls when light-side rebalance BUYs are blocked by scoring/q-min gates.
  - Affects: `mm_rewards_v1` (all symbols/timeframes/markets under MM loop).

- `mm_rewards_v1` startup weak-exit persistence guard (2026-03-03):
  - Updated startup cleanup flow in `src/main.rs`:
    - added `EVPOLY_STARTUP_CANCEL_ALL_ORDERS` (default `true`) and `EVPOLY_STARTUP_CANCEL_ALL_SKIP_IF_MM_REWARDS` (default `true`).
    - when MM rewards strategy is enabled and skip flag is on, startup `cancel_all_orders()` is skipped to avoid wiping active MM weak-exit SELL orders created for imbalance repair.
    - startup now logs explicit skip reason fields (`enabled`, `mm_rewards_enabled`, `skip_if_mm_rewards`) for auditability.
  - Affects: `mm_rewards_v1` inventory-repair exits across all enabled MM markets at bot restart.

- `mm_rewards_v1` ladder gating + rebalance-only routing hardening (2026-03-03):
  - Updated MM loop in `src/main.rs`:
    - inventory-imbalance markets now route to `rebalance_only` BUY behavior (`mm_rewards_rebalance_only_imbalance`): BUY ladders are canceled/skipped and weak-exit SELL logic handles balancing.
    - added ladder preflight planning (all-or-none): before any submit, each rung is validated for order floor + scoring.
    - if preflight fails (`non_scoring`, `below_order_floor`, or `full_ladder_required`), side is moved to `non_scoring_rebalance_only` and emits a single throttled event (`mm_rewards_rebalance_only_non_scoring`) instead of per-rung skip spam.
    - for normal markets, ladder coverage must satisfy full configured levels before posting new ladder orders.
  - Affects: `mm_rewards_v1` quote placement flow across all MM modes/markets.

- `mm_rewards_v1` auto-discovery coverage + per-mode multiplicity upgrade (2026-03-03):
  - Updated reward discovery in `src/api.rs`:
    - added `get_rewards_markets_gamma(...)` to pull reward-eligible markets from Gamma `/markets` (`order=volumeNum`, `ascending=false`, `active=true`, `closed=false`, `acceptingOrders=true`).
    - parses reward fields from Gamma payload (`clobRewards.rewardsDailyRate`, `rewardsMaxSpread`, `rewardsMinSize`, competitiveness).
  - Updated scanner in `src/mm/reward_scanner.rs`:
    - merged rewards-page feed + Gamma reward feed into one deduped map by condition/slug.
    - improved matching with case-insensitive condition/slug lookup.
    - keeps reward-feed fallback enrichment via `get_market(condition_id)` so high-reward markets outside rewards-page top rows can still enter candidates.
    - replaced single-pick selector with `select_top_candidates_per_mode(..., per_mode)` to return multiple auto markets per mode.
  - Updated MM runtime integration in `src/main.rs`:
    - auto mode now consumes multiple candidates per mode (up to `EVPOLY_MM_MAX_ACTIVE_MARKETS_PER_MODE`) instead of one.
    - auto work-items are queued before target work-items in hybrid mode, so auto slots are not starved by target mode.
    - auto snapshot/heartbeat logs now report total selected markets (sum across all mode slots), and include per-slot index in snapshot rows.
  - Affects: `mm_rewards_v1` market selection/rotation for all symbols/timeframes/modes.

- `mm_rewards_v1` competition-freeze gate toggled OFF in live env (2026-03-03):
  - Updated `.env`:
    - `EVPOLY_MM_COMPETITION_HIGH_FREEZE_ENABLE=false`
  - Effect:
    - disables high-competition freeze blocking so selected auto MM markets can keep quoting instead of entering `competition_high_freeze` pause.
  - Affects: `mm_rewards_v1` all modes/markets in current runtime configuration.

- `mm_rewards_v1` near-expiry + low-reward exit mode gates (2026-03-04):
  - Updated `src/mm/reward_scanner.rs`:
    - auto-candidate scanner now filters markets that are inside the configured near-expiry window (`EVPOLY_MM_NEAR_EXPIRY_EXIT_WINDOW_SEC`, default 24h), in addition to already-expired markets.
  - Updated `src/main.rs` MM runtime loop:
    - introduced effective reward-total tracking from rewards feed + market details.
    - if market enters near-expiry window or effective total reward drops below configured floor (`EVPOLY_MM_MIN_TOTAL_REWARD_QUOTE`, default 25), MM BUY quoting is disabled and market enters forced inventory-exit mode.
    - forced exit mode cancels BUY ladders, keeps weak-exit SELL logic active, and offboards auto-selected markets for replacement while inventory is unwound.
  - Updated `.env`:
    - `EVPOLY_MM_MIN_TOTAL_REWARD_QUOTE=25`
    - `EVPOLY_MM_NEAR_EXPIRY_EXIT_WINDOW_SEC=86400`
  - Affects: `mm_rewards_v1` all symbols/timeframes/modes (auto + target/hybrid runtime gating).

- `mm_rewards_v1` daily-reward-only gating + proxy-fed read path (2026-03-05):
  - Updated reward metric usage to `rewardsDailyRate` only (removed total reward estimation/horizon math and ignored `rewardsAmount` in MM decision paths).
  - Updated `src/api.rs`:
    - removed total-reward aggregation logic from rewards parsing/scoring paths.
    - rewards feed ranking now uses daily rate only.
    - added non-trading read proxy pool support via `POLY_MM_PROXIES` with round-robin + fallback for MM scanner/read flows (`get_all_clob_markets`, `get_rewards_markets_page`, `get_rewards_markets_gamma`, and `get_market_via_proxy_pool`).
    - kept direct path for trading-critical market discovery methods (`get_market_by_slug`, `get_market_from_event_slug`, `get_market`).
    - added MM read telemetry counters (`proxy/direct attempts, successes, 429s, errors`) exposed by `mm_read_telemetry_snapshot`.
  - Updated `src/mm/reward_scanner.rs`:
    - `MarketRewardSnapshot` now tracks `reward_daily_rate` only.
    - scanner filters/ranking/force-include comparison now use daily rate only.
  - Updated `src/main.rs` MM runtime:
    - competition snapshot caches now store daily reward maps only.
    - reward eligibility and low-reward gate now evaluate daily reward only.
    - removed legacy total-reward telemetry fields in MM skip/exit events; logs now emit daily metric fields.
    - MM heartbeat includes proxy-vs-direct read telemetry counters.
  - Updated thresholds/defaults:
    - `EVPOLY_MM_AUTO_FORCE_INCLUDE_REWARD_MIN=300`
    - `EVPOLY_MM_MIN_REWARD_RATE_HINT=15`
    - `EVPOLY_MM_MIN_TOTAL_REWARD_QUOTE=15` (now treated as daily gate metric while env name is kept).
  - Affects: `mm_rewards_v1` all symbols/timeframes/modes (auto + target/hybrid reward selection and gating).

- `mm_rewards_v1` ladder floor autosize + background refresh + direct-first read fallback (2026-03-05):
  - Updated ladder preflight in `src/main.rs`:
    - allows weak-market degrade to 1 rung (`EVPOLY_MM_AUTO_MIN_FEASIBLE_LEVELS=1`) with telemetry `mm_rewards_ladder_degraded_to_one`.
    - replaced hard `below_order_floor` skip with rung autosizing to exchange floor (`required_notional_usd / price`) before submit.
    - added per-order guard `EVPOLY_MM_MAX_SHARES_PER_ORDER` and telemetry `mm_rewards_floor_size_upscaled`.
  - Updated MM refresh execution in `src/main.rs`:
    - added background worker for competition snapshot + auto ranked-candidate refresh.
    - hot MM quote loop now consumes background snapshot cache instead of running heavy scan calls inline.
    - moved activity sync to dedicated background worker (`mm_rewards_activity_sync` source=`background_worker`).
  - Updated read routing in `src/api.rs`:
    - non-trading MM reads are now direct-first with limited proxy fallback (`EVPOLY_MM_PROXY_FALLBACK_MAX`, default 2), replacing proxy-first chain behavior.
  - Updated env in `.env`:
    - `EVPOLY_MM_AUTO_MIN_FEASIBLE_LEVELS=1`
    - `EVPOLY_MM_MAX_SHARES_PER_ORDER=10000`
    - `EVPOLY_MM_PROXY_FALLBACK_MAX=2`
  - Affects: `mm_rewards_v1` all MM modes and selected markets.

- `mm_rewards_v1` no-ladder watchdog fix for sell-only/rebalance states (2026-03-05):
  - Updated `src/main.rs` watchdog activity detection:
    - changed ladder-active check from `open_buy_orders > 0` to `open_buy_orders + open_sell_orders > 0`.
    - this prevents false `no_active_ladder_watchdog` tagging when MM is actively running sell-side/rebalance orders (`0/N` states).
  - No env default changes.
  - Affects: `mm_rewards_v1` all modes/markets where sell-only inventory balancing is active.

- `mm_rewards_v1` weak-exit side-flip cleanup for stale SELL ladders (2026-03-05):
  - Updated `src/main.rs` weak-exit dynamic rebalance path:
    - when weak-exit active token flips token, bot now cancels stale open `SELL` orders on the opposite token for the same scope (`strategy_id`, timeframe, period, condition).
    - adds cooldown + in-memory cleanup for canceled opposite-side exits to avoid immediate flap re-submit.
    - emits `mm_rewards_weak_exit_side_flip_cleanup` telemetry with active/canceled token IDs and canceled row count.
  - No env default changes.
  - Affects: `mm_rewards_v1` dynamic imbalance rebalance (inventory exit ladder).

- `premarket_v1` enqueue late-guard + backpressure mode for pre-open reliability (2026-03-05):
  - Updated `src/main.rs` premarket enqueue path:
    - added pre-enqueue time guard (`EVPOLY_PREMARKET_ENQUEUE_GUARD_MS`, default `2500`) to stop building/flushing new ladder requests too close to market open.
    - added configurable premarket enqueue mode (`EVPOLY_PREMARKET_ENQUEUE_MODE=await|bounded|try_send`, default `bounded`) with timeout (`EVPOLY_PREMARKET_ENQUEUE_TIMEOUT_MS`, default `120`) for batch enqueue.
    - emits `premarket_enqueue_guard_blocked` and includes enqueue mode/timeout in batch telemetry.
  - Affects: `premarket_v1` batch ladder request enqueue behavior under queue pressure near open.

- `evsnipe_v1` burst-tick ingest hardening (2026-03-05):
  - Updated `src/main.rs` + `src/evsnipe.rs`:
    - EVSnipe tick channel capacity now configurable via `EVPOLY_EVSNIPE_TICK_CHANNEL_CAP` (default `65536`).
    - Binance trade stream spillway timeout now configurable via `EVPOLY_EVSNIPE_TICK_SEND_TIMEOUT_MS` (default `60ms`).
  - Affects: `evsnipe_v1` trade-tick buffering behavior during burst load (no strategy decision-model change).

- Arbiter dispatcher fast fanout (2026-03-05):
  - Updated `src/main.rs` worker dispatcher:
    - dispatch now tries non-blocking `try_send` fanout across workers before falling back to awaited send.
    - reduces head-of-line blocking when a single worker queue is full.
  - Affects: all strategy request execution pools (`normal`, `premarket`, `endgame`, `endgame_revert`, `sessionband`, `evcurve`, `critical`) at dispatch layer only.

- `evcurve_v1` enqueue dedupe hardening (2026-03-05):
  - Updated `src/entry_idempotency.rs` `EnqueueDedupe` to support EVcurve-specific enqueue cooldown override.
  - New env: `EVPOLY_ENTRY_DEDUPE_COOLDOWN_EVCURVE_MS` (defaults to `max(EVPOLY_ENTRY_DEDUPE_COOLDOWN_MS, 5000)` when unset).
  - Affects: `evcurve_v1` producer-side enqueue dedupe behavior (reduces duplicate worker drops from rapid re-enqueue).

- `premarket_v1` dispatcher-side late-drop guard (2026-03-05):
  - Updated `src/main.rs` arbiter dispatcher:
    - drops stale premarket ladder requests before worker fanout when market is already open.
    - logs `premarket_enqueue_guard_blocked` with source `arbiter_dispatcher`.
  - Affects: `premarket_v1` late request handling under backlog/queue delay.

- WS market lag reconnect policy hardening (2026-03-05):
  - Updated `src/polymarket_ws.rs` market stream lag handling:
    - small lag bursts can be ignored (`EVPOLY_PM_WS_MARKET_LAG_IGNORE_MISSED_MESSAGES`),
    - reconnect-on-lag now requires both soft-error count and hard missed-message threshold (`EVPOLY_PM_WS_MARKET_LAG_HARD_MISSED_MESSAGES`).
    - adds `polymarket_ws_market_stream_lag_ignored` telemetry.
  - Affects: all strategies consuming PM WS orderbook/user feed freshness paths.

- `mm_rewards_v1` watchdog gating cleanup (2026-03-05):
  - Updated `src/main.rs` watchdog path:
    - `mm_rewards_watchdog_no_ladder` now only evaluates when market is expected to be actively quoting (`state in idle/quoting` and no active reason).
    - suppresses watchdog noise during explicit non-quoting states (rebalance/cooldown/blocked/unwind).
  - Affects: `mm_rewards_v1` watchdog alerting and market-state telemetry.

- `mm_rewards_v1` background auto-scan snapshot fallback hardening (2026-03-05):
  - Updated `src/main.rs` background refresh + loop apply path:
    - auto-ranked candidate snapshot now falls back to last-good ranked set when fresh scan is empty/error/timeout within `EVPOLY_MM_BG_RANKED_STALE_SEC` window (default `600s`).
    - main loop keeps last-good ranked candidates instead of clearing immediately on transient empty snapshots.
    - warmup suppression added: before first successful ranked snapshot, auto-scan empty errors are suppressed for the initial stale window.
    - added telemetry `mm_rewards_auto_snapshot_using_cached_ranked` and richer `mm_rewards_auto_scan_error` stale-age fields.
  - Affects: `mm_rewards_v1` auto market selection stability under transient API/scan gaps.

- `premarket_v1` scheduler-side late intent guard (2026-03-05):
  - Updated `src/strategy_decider.rs` premarket scheduler:
    - skips emitting premarket intents once inside enqueue guard window before open (`EVPOLY_PREMARKET_ENQUEUE_GUARD_MS` with second-level safety rounding).
    - emits `strategy_premarket_skip_late_intent` telemetry.
    - enqueue guard default tightened from `2500ms` to `15000ms` for both scheduler and runtime enqueue checks.
    - near-open enqueue now uses tighter batch/chunk policy (`batch_chunk`, `enqueue_mode`, `enqueue_timeout_ms`) based on seconds-to-open.
  - Affects: `premarket_v1` intent generation, reducing late queue load and worker-side late rejects.

- PM WS refresh reconnection policy (2026-03-05):
  - Updated `src/polymarket_ws.rs` market/user loops:
    - periodic refresh no longer forces reconnect by default; reconnect only when discovered subscription targets actually change.
    - optional force behavior remains via `EVPOLY_PM_WS_RECONNECT_ON_REFRESH=true`.
    - market lag reconnect now honors cooldown (`EVPOLY_PM_WS_MARKET_LAG_RECONNECT_COOLDOWN_SEC`) to prevent reconnect storms on repeated lag bursts.
    - market WS load can now shard across multiple subscription loops (`EVPOLY_PM_WS_MARKET_SHARDS`, default `2`).
    - adds refresh discovery failure telemetry and reconnect reason telemetry.
  - Affects: all strategies consuming PM WS feeds; reduces unnecessary stream churn.

- `premarket_v1` dispatcher backpressure drop policy (2026-03-05):
  - Updated `src/main.rs` arbiter dispatcher premarket path:
    - when all premarket worker queues are full, dispatcher can drop immediately (`EVPOLY_PREMARKET_DISPATCH_DROP_IF_FULL`, default `true`) instead of blocking backlog growth.
    - premarket worker queue cap can be set independently (`EVPOLY_ENTRY_WORKER_QUEUE_CAP_PREMARKET`).
    - emits `premarket_dispatch_drop_full`.
  - Affects: `premarket_v1` queue behavior under burst load; aims to reduce stale late submissions.

- `mm_rewards_v1` auto selection + rebalance rearm update (2026-03-05):
  - Updated `src/main.rs` + `src/mm/reward_scanner.rs`:
    - removed runtime per-mode active cap enforcement in MM auto processing (`max_active_markets_per_mode` no longer blocks candidate execution).
    - auto selection now uses reward-driven global top-N (`auto_top_n`) and assigns each candidate to best-fit mode, instead of per-mode slot caps.
    - on rebalance close (imbalance cleared), bot now auto-rearms ladder quoting by clearing local weak-exit state, canceling stale MM `SELL` exits for the scope, and clearing auto cooldown/offboard flags for that condition.
  - Affects: `mm_rewards_v1` auto market selection breadth and post-imbalance return-to-ladder behavior.

- `evsnipe_v1` market discovery pagination fix (2026-03-06):
  - Updated `src/evsnipe.rs` discovery loop:
    - fixed `events` pagination to advance offset by API page limit (event-offset semantics), not flattened market count.
    - removed premature stop based on accumulated flattened market count.
  - Affects: `evsnipe_v1` hit-market coverage (prevents skipping event ranges such as intraday strike ladders).

- `evsnipe_v1` strike parsing upgrade for SOL/XRP ladders (2026-03-06):
  - Updated `src/evsnipe.rs` strike parser:
    - supports Polymarket slug decimal encoding `pt` (e.g., `0pt8`, `1pt3`).
    - removed hard strike floor `<100` rejection; now allows positive sub-100 strikes.
    - treats decimal-like tokens as valid strike hints so low-price ladders parse correctly.
  - Added tests in `src/evsnipe.rs` for `$0.8`, `0pt8`, and `$70` parsing.
  - Affects: `evsnipe_v1` market inclusion for SOL/XRP low-price hit ladders.

- `mm_rewards_v1` background ranked scan timeout + limit increase (2026-03-05):
  - Updated `src/main.rs` background ranked scan timeout:
    - increased `scan_rank_auto_candidates` timeout from `25s` to `600s`.
  - Updated runtime config in `.env`:
    - set `EVPOLY_MM_AUTO_SCAN_LIMIT=1000` (was `200`).
  - Affects: `mm_rewards_v1` auto-scan breadth and background ranked snapshot completion window.

- `mm_rewards_v1` wallet-backed MM inventory accounting + reconcile/reporting slice (2026-03-06):
  - Updated `src/tracking_db.rs` MM accounting/reporting paths:
    - added dedicated inventory-consumption accounting for merge/reconcile repairs using `inventory_consumed_units` / `inventory_consumed_cost_usd` on `positions_v2` and persisted consumption events.
    - added wallet-backed MM inventory/drift snapshot helpers using `wallet_positions_live_latest_v1`, `wallet_activity_v1`, and `positions_unrealized_mid_latest_v1` when present.
    - extended `summarize_mm_market_attribution(...)` to expose DB-vs-wallet inventory shares, drift, wallet value fields, and latest wallet activity metadata while keeping existing fields.
  - Updated `src/trader.rs` MM runtime/admin paths:
    - successful manual/scheduled merges now consume DB inventory through the dedicated non-SELL path instead of faking exit fills.
    - added callable MM inventory reconcile entrypoint and extended `mm_rewards_status` JSON with wallet-backed source flags and drift fields.
  - Updated `scripts/wallet_history_sync.py`:
    - wallet activity now stores `inventory_consumed_units` for merge-style activity when available.
    - unrealized mark sync now subtracts `inventory_consumed_units` from open remaining inventory.
  - Affects: `mm_rewards_v1` MM timeframe/reporting/reconcile accounting only; strategy execution logic is otherwise unchanged.

### 2026-03-07

- Runtime strategy toggle update (ops config only):
  - Updated `.env`:
    - `EVPOLY_STRATEGY_ENDGAME_ENABLE=false`
    - `EVPOLY_STRATEGY_SESSIONBAND_ENABLE=false`
  - Affects: `endgame_sweep_v1` and `sessionband_v1` loops are disabled at process start/restart; no strategy logic/codepath changes.

- Arbiter execution pipeline hardening (all submit-path strategies via shared entry worker in `src/main.rs`):
  - Added per-request timing telemetry (`entry_execution_timing`) with decision/enqueue/dequeue/worker/submit timestamps and derived `queue_wait_ms` + `decision_to_submit_ms`.
  - Worker submit path now runs `execute_limit_buy(...)` in spawned tasks behind per-pool semaphore (`EVPOLY_ENTRY_SUBMIT_CONCURRENCY` and `EVPOLY_ENTRY_SUBMIT_CONCURRENCY_<POOL>`), reducing head-of-line blocking from slow post-order RTT.
  - Added submit-time late guard (`EVPOLY_ENTRY_SUBMIT_LATE_GUARD_MS`) to drop stale requests that reach submit too close to/after close, plus explicit `entry_submit_late_guard_blocked` event.
  - Affects entry behavior/timing for: `reactive_v1`, `premarket_v1`, `liqaccum_v1`, `snapback_v1_1`, `endgame_sweep_v1`, `endgame_revert_v1`, `evcurve_v1`, `sessionband_v1`, `evsnipe_v1`, `mm_rewards_v1`.

- Submit-cap hot-path DB contention reduction (`src/main.rs` shared entry worker):
  - Added in-memory submit-scope counter cache (`EVPOLY_ENTRY_SUBMIT_SCOPE_CACHE_TTL_MS`) for submit-cap checks, with DB refresh merge and post-submit cache increment.
  - Reduces repeated synchronous `count_entry_submits_for_scope(...)` reads in burst windows while preserving cap guard semantics.
  - Affects all strategies routed through the arbiter entry worker.

- `mm_rewards_v1` watchdog/state recovery hardening (`src/main.rs`):
  - Added watchdog auto-rearm timer (`EVPOLY_MM_WATCHDOG_REARM_SEC`) to avoid permanent `no_active_ladder_watchdog` lockout; market can re-enter quote attempts automatically.
  - Added explicit `rebalancing -> idle` auto-return when imbalance is cleared and no open orders remain.
  - Added `mm_rewards_state_action_mismatch` telemetry event when market state says `quoting` but no open orders exist.
  - Affects MM runtime state transitions and diagnostics only.

- Non-trading MM read resilience (`src/api.rs`):
  - Added per-attempt timeout (`EVPOLY_MM_READ_TIMEOUT_MS`) and jittered retry delay (`EVPOLY_MM_READ_RETRY_BASE_MS`, `EVPOLY_MM_READ_RETRY_JITTER_MS`) in proxy/direct fallback read path.
  - Applies only to read-only MM discovery/metadata requests; trading/cancel writes remain direct execution path.

- Runtime strategy toggle update (ops config only):
  - Updated `.env`:
    - `EVPOLY_STRATEGY_ENDGAME_ENABLE=true`
    - `EVPOLY_STRATEGY_SESSIONBAND_ENABLE=true`
  - Affects: `endgame_sweep_v1` and `sessionband_v1` loops are enabled at process start/restart; no strategy logic/codepath changes.

- `mm_rewards_v1` inventory-exit two-way ladder + status scope guardrails (2026-03-07):
  - Updated `src/main.rs` inventory-exit/rebalance path:
    - inventory-only and low-reward forced-exit modes no longer hard-disable all BUY quoting by reason flag.
    - forced inventory exit (outside near-expiry window) now keeps two-way behavior by allowing light-side BUYs while still blocking heavy-side BUYs.
    - bypassed `rebalance_reduce_heavy_only` suppression during forced inventory two-way mode so light-side deep bids are not skipped on heavily imbalanced markets.
    - added deep opposite-side BUY ladder mode for forced inventory exits: anchor from `best_bid - reward_range` (`max_spread`), then ladder downward by ticks (for example 56/55/54 when bid=59 and reward range=3c).
    - emits `mm_rewards_forced_exit_opposite_deep_ladder` telemetry with anchor/range/top/bottom details.
  - Follow-up correction (same day):
    - forced inventory two-way mode now places deep BUY ladder on the same heavy/exit token (e.g., NO exit keeps deep NO bids), and blocks opposite-token BUYs.
    - fixes mismatched behavior where deep bids could appear on the opposite outcome token.
    - bypassed weak-skew hard block (`mm_rewards_skip_weak_side_inventory_skew`) during forced inventory two-way mode so heavy-side deep bids can actually post.
  - Updated `src/trader.rs` `/admin/mm/status` payload:
    - added explicit `scope_notes` clarifying that `markets` is `status_scope` (`mm_market_state`) and can exclude global open-order/inventory markets.
    - documented global references for truth checks (`pending_orders` and `wallet_positions_live_latest_v1`).
  - Affects: `mm_rewards_v1` inventory-exit quote behavior and MM status diagnostics interpretation.

- Runtime WS target-change debounce hardening (2026-03-07):
  - Updated `src/polymarket_ws.rs` market/user refresh loops:
    - added target-change debounce + hold-time gates before reconnecting on `subscription_target_changed`.
    - added symmetric-delta threshold filter (`bps`) to ignore small target diffs.
    - added telemetry for debounced/ignored/cleared target-change states.
  - New env controls:
    - `EVPOLY_PM_WS_TARGET_CHANGE_DEBOUNCE_SCANS`
    - `EVPOLY_PM_WS_TARGET_CHANGE_MIN_HOLD_SEC`
    - `EVPOLY_PM_WS_TARGET_CHANGE_MIN_DELTA_BPS`
  - Affects runtime WS stability for all strategies consuming Polymarket WS market/user streams.

- `mm_rewards_v1` dust-sell submits raw inventory size without reward min gating (2026-03-07):
  - Updated `src/trader.rs` `/admin/mm/dust-sell` path:
    - removed pre-submit market constraint gating (`minimum_order_size` / `rewards.min_size`) from dust cleanup flow.
    - dust sell now submits floored live balance size directly (`floor_units_for_order`) and keeps only dust-cap/min-submit guards.
    - added explicit telemetry flag `constraints_enforced=false` on dust sell submitted records.
  - Affects: `mm_rewards_v1` inventory cleanup behavior across all MM markets/timeframes; small residual positions are no longer blocked by reward `min_size` hints in pre-check logic.
  - Key env/code paths: `EVPOLY_MM_DUST_SELL_MIN_SUBMIT_SHARES`, `EVPOLY_MM_DUST_SELL_MAX_NOTIONAL_USD`, `src/trader.rs::dust_sell_mm_inventory`.

- `mm_rewards_v1` dust-sell pacing for sequential slow sweeps (2026-03-07):
  - Updated `src/trader.rs` `/admin/mm/dust-sell` loop:
    - added `EVPOLY_MM_DUST_SELL_INTER_ORDER_DELAY_MS` (default `1000`) and enforced delay between attempted dust orders.
    - dust sweep remains sequential and capped by `EVPOLY_MM_DUST_SELL_MAX_ORDERS`, but no longer bursts back-to-back auth/order calls.
  - Runtime env knobs used for deployment pacing:
    - `EVPOLY_MM_DUST_SELL_INTER_ORDER_DELAY_MS=1000`
    - `EVPOLY_MM_DUST_SELL_TIMEOUT_SEC=180` (so hourly wallet-sync dust call can finish 20 paced orders without client timeout).
  - Affects: `mm_rewards_v1` hourly dust cleanup pacing/reliability only (no change to market-making quote logic).

### 2026-03-20

- `mm_sport_v1` ratio-breach hard cooldown to stop cancel/requote churn:
  - Updated `src/main.rs` MM Sport runtime path:
    - added condition-scoped ratio pause state (`ratio_pause_until_by_condition`).
    - on ratio breach, strategy now starts a fixed cooldown and suppresses new BUY quoting until cooldown expiry (inventory exit SELL path remains allowed).
    - cooldown is applied from all ratio breach sources: watchdog pair check, pair planning gate, and per-token runtime ratio gate.
    - heartbeat telemetry now reports `ratio_paused_markets`.
  - Updated `src/mm/mod.rs` config:
    - added `EVPOLY_MM_SPORT_RATIO_PAUSE_SEC` (default `900` seconds).
  - Updated env templates:
    - `.env.example` and `.env.full.example` now include `EVPOLY_MM_SPORT_RATIO_PAUSE_SEC=900`.
  - Affects: `mm_sport_v1` pregame reward markets across all discovered sports conditions in active runtime.

- `mm_rewards_v1` dust-sell default max-notional increased:
  - Updated `src/trader.rs`:
    - `EVPOLY_MM_DUST_SELL_MAX_NOTIONAL_USD` default changed from `5` to `20` (still env-overridable).
  - Updated `.env.full.example` default to `EVPOLY_MM_DUST_SELL_MAX_NOTIONAL_USD=20`.
  - Affects: `mm_rewards_v1` dust cleanup sizing cap (`/admin/mm/dust-sell` and scheduled dust sweep paths).

### 2026-03-23

- `evsnipe_v1` sticky watchlist hardening for degraded discovery windows:
  - Updated `src/main.rs` EVSnipe discovery refresh path:
    - introduced in-memory sticky watchlist cache keyed by `condition_id` (deduped add-only merge during healthy cycles).
    - degraded discovery gate now holds last-good watchlist when `hit_specs < 10` (default threshold, env-overridable).
    - added safety-cap behavior for sticky cache: when a healthy merge would exceed cap, keep last-good snapshot and stop adding; on the next healthy cycle, replace from fresh snapshot when within cap.
    - active watchlist published to trade loop now comes from sticky cache snapshot, not direct per-cycle full replacement.
    - added EVSnipe watchlist telemetry fields/events for degraded-hold and cap-hold transitions.
  - Updated `src/evsnipe.rs` config default:
    - `EVPOLY_EVSNIPE_DISCOVERY_REFRESH_SEC` default changed from `30` to `300` seconds.
  - Updated `.env.full.example`:
    - documented `EVPOLY_EVSNIPE_DEGRADED_HIT_SPECS_MIN` and `EVPOLY_EVSNIPE_WATCHLIST_MAX_SPECS`.
    - updated `EVPOLY_EVSNIPE_DISCOVERY_REFRESH_SEC` comment to reflect default `300`.
  - Affects: `evsnipe_v1` hit/close watchlist refresh behavior across configured symbols (`BTC,ETH,SOL,XRP,DOGE,BNB,HYPE`) and all EVSnipe market families.
