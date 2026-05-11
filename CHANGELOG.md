# EVPoly Changelog

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
