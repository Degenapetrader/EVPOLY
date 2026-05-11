# EVPoly Desktop Changelog

## UI-v2.3.1 - 2026-05-11
- Added an MM 2.0 Entry Price control for Passive vs Best Bid mode.
- Bumped desktop app/updater version to 2.3.1.

## UI-v2.2.9 - 2026-05-08
- Bumped desktop app/updater version to 2.2.9.
- Repinned the bundled runtime sidecar to runtime v2.2.9 with the BUY hot-path balance-allowance performance fix.

## UI-v2.2.6 - 2026-05-07
- Scoped overview PnL, latency, and liquidity rewards to the active desktop profile/wallet, and cleaned the rewards unavailable state for wallets without reward access.
- Fixed Deposit Wallet allowance parsing and gated Deposit Wallet starts until the API bridge reports approval readiness.
- Clarified the live-profile control label when another profile bot is already running.
- Fixed MM 2.0 route cap normalization so Dual route migrates stale Sport-only or Non-S-only caps back to `50/50`, and Home route buttons save the route-default caps.
- Added MM 2.0 Sport / Non-S sizing profiles in desktop config and settings UI.
- Added a desktop runtime patch for Non-S sizing overrides that fall back to Sport values and apply only to fresh BUY sizing plus BUY-side ratio hygiene.
- Added Endgame Tick Split controls to the desktop strategy editor.
- Added a desktop runtime patch that backs off `/balance-allowance` 429s and skips noisy single-order fallback after balance rate limits.

## UI-v2.2.5 - 2026-05-06
- Bumped desktop app/updater version to 2.2.5.

## UI-v2.1.0 - 2026-04-29
- Added MM 2.0 inventory-exit max-loss control and Tauri config mapping.
- Bumped desktop app/updater version to 2.1.0.

## UI-v2.0.1 - 2026-04-28
- Updated the Home strategy rail UI to match the hosted platform controls.
- Hid MM Rewards from the visible strategy list.
- Merged MM 2.0 filters and inventory exit controls into one compact panel.
- Displayed available balance in pUSD.

## UI-v1.1.2 - 2026-04-13
- Repinned the bundled EVPoly runtime sidecar to runtime `v1.1.2` (`feabdb0`).
- Inherits runtime `v1.1.2` behavior changes:
  - share-sized endgame and sessionband execution support
  - wallet snapshot refresh before merge sweeps
  - MM Sport exit inventory fallback to wallet snapshots
  - more aggressive redeem/merge auto-trigger defaults with unchanged `900s` cooldown
