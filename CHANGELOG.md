# EVPoly Changelog

## v2.5.0 - 2026-06-29
- Hardened local Endgame fill readiness by allowing valid single-venue proxy direction when the peer venue sample is missing, retrying hot metadata prewarm, and lengthening fast submit outcome waits without retrying ambiguous FAK posts.
- Fixed MM Sport stale websocket book snapshots so stale books fall back to the FIFO websocket-gap path instead of feeding FIFO/front-ratio decisions.

## v2.4.8 - 2026-06-27
- Widened MM Sport full Polymarket rewards discovery to 20 pages and 2000 detail rows while keeping delta discovery bounded and cache-protected.
- Removed the stale MM Sport discovery detail-cap env surface from runtime docs/examples.

## v2.4.7 - 2026-06-26
- Restored local Premarket ladder controls and retained the local ladder bias percentages for Safe/Aggressive modes.
- Restored legacy Endgame V1 timing while keeping the RTDS, FAK, and Book99 CEX-depth sizing safeguards.
- Added MM Sport snapshot fallback handling so degraded discovery can recover from the local snapshot before falling back to CLOB rewards.

## v2.4.6 - 2026-06-17
- Hardened local secret handling so generated config/env files are written owner-only on Unix and existing `config.json` files are hardened on load.
- Stopped serializing env-loaded private keys back into generated `config.json` while preserving compatibility with existing configs.
- Made `manual_bot` fail closed by default unless a manual/admin token is configured or loopback unauthenticated mode is explicitly enabled.
- Enforced HTTPS for configured remote alpha/relayer URLs by default and added remote EVcurve payload bounds validation.

## v2.4.5 - 2026-06-16
- Added a cheap `/bot/liveness` admin endpoint for external supervisors.
- Changed fatal runtime panics to exit the bot process instead of leaving a half-alive async runtime.

## v2.4.3 - 2026-06-15
- Disabled EVSnipe pre-hit entries during the final 4 hours before market cutoff while keeping confirm-hit entries active.

## v2.4.2 - 2026-06-13
- Hard-disabled the retired MM Sport remote depth-skip Alpha gate so saved/generated env values cannot shrink the quote universe.
- Removed the retired MM Sport depth-skip Alpha env surface from runtime examples and docs.

## v2.4.1 - 2026-06-12
- Fixed MM Sport rewards discovery so skipped Polymarket reward cursor pages do not shrink the discovered market universe.
- Added stable last-good market cache handling for degraded MM Sport discovery responses.
- Changed MM Sport live-guard prune recovery to use debounced delta discovery repairs instead of repeated full discovery sweeps.

## v2.4.0 - 2026-06-12
- Added the local vendor Polymarket SDK websocket lifecycle fix.
- Updated MM Sport runtime defaults for higher sports market throughput, restored quote expiry override defaults to 65/185 seconds, and removed the stale desktop sport market cap surface.
- Upgraded the direct `reqwest` dependency to remove the expired `rustls-pemfile` audit exception.

## v2.3.6 - 2026-05-14
- Changed EVSnipe local market discovery to use the dedicated Polymarket Hit Price tag so daily, weekly, and monthly 7-symbol hit-price ladders are found without broad crypto paging.

## v2.3.4 - 2026-05-14
- Added the builder fee disclosure to runtime docs and env templates.
- Includes MM Sport live-guard market pruning from `strategy-changelog.md` so stale live sports markets can recover without restarting the bot.

## v2.3.0 - 2026-05-11
- Added MM 2.0 max quote share caps for Sport and Non-S routes.
- Added MM 2.0 entry price mode so BUY entries can use passive one-tick-behind pricing or current best-bid pricing.

## v2.2.9 - 2026-05-08
- Restored fast EOA/Proxy/Safe limit BUY submission by avoiding cold CLOB `/balance-allowance` fee-sizing probes on the hot path.
- Kept Deposit Wallet BUY collateral readiness on the explicit pUSD balance/allowance path while avoiding duplicate fee preflight checks.
- Hardened `/balance-allowance` 429 cooldown and batch fallback detection so rate-limited balance checks do not fan out into noisy single-order retries.

## v2.2.6 - 2026-05-07
- Added shared `/balance-allowance` 429 backoff so concurrent BUY sizing checks reuse a fresh cached pUSD snapshot instead of spamming CLOB.
- Skipped per-order single fallback after batch placement fails on `/balance-allowance` rate limits.

## v2.2.5 - 2026-05-06
- Fixed Deposit Wallet CLOB balance/allowance parsing so runtime readiness uses the normalized CLOB collateral payload correctly.
- Prepared the runtime branch for the v2.2.5 desktop and Linux UI release.

## v1.1.2 - 2026-04-13
- Added share-sized execution mode for `endgame_sweep_v1` and `sessionband_v1`.
- Refresh live wallet snapshots before merge sweeps and persist wallet snapshot/activity tables in `tracking.db`.
- Added MM Sport wallet snapshot fallback for exit inventory when local tracked inventory lags live balances.
- Tightened settlement auto-trigger defaults:
  - redeemable condition threshold `5`
  - mergeable pair-condition threshold `5`
  - available-ratio trigger `0.50`
  - cooldown unchanged at `900s`
