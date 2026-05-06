# EVPoly Changelog

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
