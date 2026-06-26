# EVPoly Desktop Changelog

## UI-v2.4.7 - 2026-06-26
- Bumped desktop app/updater version to 2.4.7.
- Repinned the bundled runtime sidecar to runtime `v2.4.7` (`ba7a56a`) with restored Premarket ladder controls, legacy Endgame timing with RTDS/FAK/CEX-depth safeguards, and MM Sport discovery snapshot fallback.
- Added portfolio/performance share-card updates and restored editable Premarket Safe/Aggressive ladder bias controls.

## Local - 2026-06-25
- Repinned the local desktop sidecar build to runtime `31f7115`, preserving the pUSD balance/allowance fallback cache during MM Sport and Endgame collateral backoff.
- Repinned the local desktop sidecar build to runtime `a8b94da`, bypassing synchronous pUSD collateral preflight for Endgame and EVSnipe fast FAK BUY submits while preserving it for resting BUY flows.
- Repinned the local desktop sidecar build to the local runtime commit `eaa14ae`, restoring legacy Endgame V1 timing with RTDS, FAK, and Book99 CEX-depth sizing for local testing.

## UI-v2.4.6 - 2026-06-17
- Bumped desktop app/updater version to 2.4.6.
- Repinned the bundled runtime sidecar to runtime `v2.4.6` (`e02c283`) with local secret hardening, fail-closed manual API auth, HTTPS-only remote URL defaults, and remote EVcurve payload validation.

## UI-v2.4.5 - 2026-06-16
- Bumped desktop app/updater version to 2.4.5.
- Repinned the bundled runtime sidecar to runtime `v2.4.5` (`4921f7d`) with fatal-panic exit and the cheap `/bot/liveness` admin endpoint.
- Added desktop bot watchdog recovery for wedged sidecars, using `/bot/liveness` every 30 seconds with restart-loop protection.

## UI-v2.4.3 - 2026-06-15
- Bumped desktop app/updater version to 2.4.3.
- Repinned the bundled runtime sidecar to runtime `v2.4.3` (`7086e95`) with the EVSNIPE pre-hit cutoff guard.
- Humanized Magic Core bridge provisioning failures during desktop Magic wallet creation.
- Fixed the desktop Magic finish bridge payload so SaaS no longer rejects it for unexpected request fields.

## UI-v2.4.2 - 2026-06-13
- Bumped desktop app/updater version to 2.4.2.
- Repinned the bundled runtime sidecar to runtime `v2.4.2` (`9f26297`) with the MM Sport retired depth-skip Alpha gate hard-disabled.

## UI-v2.4.1 - 2026-06-12
- Bumped desktop app/updater version to 2.4.1.
- Repinned the bundled runtime sidecar to runtime `v2.4.1` (`46a5afa`) with MM Sport rewards discovery, stable cache, and live-guard delta repair fixes.

## UI-v2.3.6 - 2026-05-14
- Bumped desktop app/updater version to 2.3.6.
- Repinned the bundled runtime sidecar to runtime `v2.3.6` (`822b03f`) with EVSnipe Hit Price discovery.

## UI-v2.3.4 - 2026-05-14
- Added the Home builder fee disclosure banner.
- Bumped desktop app/updater version to 2.3.4.
- Repinned the bundled runtime sidecar to runtime `v2.3.4` (`0eb6f36`).

## UI-v2.3.3 - 2026-05-13
- Bumped desktop app/updater version to 2.3.3.
- Repinned the bundled runtime sidecar to runtime `v2.3.3` (`99f7164`).

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
