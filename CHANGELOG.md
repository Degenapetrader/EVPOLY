# EVPoly Linux Changelog

## Linux-v2.3.4 - 2026-05-14
- Added the Home builder fee disclosure banner.
- Bumped Linux app/updater version to 2.3.4.
- Repinned the bundled runtime sidecar to runtime `v2.3.4` (`0eb6f36`).

## Linux-v2.3.3 - 2026-05-13
- Bumped Linux app/updater version to 2.3.3.
- Repinned the bundled runtime sidecar to runtime `v2.3.3` (`99f7164`).

## Linux-v2.3.1 - 2026-05-11
- Added an MM 2.0 Entry Price control for Passive vs Best Bid mode.
- Bumped Linux app/updater version to 2.3.1.

## Linux-v2.2.9 - 2026-05-08
- Bumped Linux app/updater version to 2.2.9.
- Repinned the bundled runtime sidecar to runtime v2.2.9 with the BUY hot-path balance-allowance performance fix.

## Linux-v2.2.6 - 2026-05-07
- Ported desktop Deposit Wallet allowance parsing, start approval gating, profile-scoped overview metrics, and live-profile label fixes.
- Added MM 2.0 Sport / Non-S sizing profiles in desktop config and settings UI.
- Added a desktop runtime patch for Non-S sizing overrides that fall back to Sport values and apply only to fresh BUY sizing plus BUY-side ratio hygiene.
- Added a runtime patch that backs off `/balance-allowance` 429s and skips noisy single-order fallback after balance rate limits.

## Linux-v2.2.5 - 2026-05-06
- Bumped Linux app/updater version to 2.2.5.

## Linux-v2.1.0 - 2026-04-29
- Added MM 2.0 inventory-exit max-loss control and Tauri config mapping.
- Bumped Linux app/updater version to 2.1.0.

## Linux-v2.0.1 - 2026-04-28
- Updated the Home strategy rail UI to match the hosted platform controls.
- Hid MM Rewards from the visible strategy list.
- Merged MM 2.0 filters and inventory exit controls into one compact panel.
- Displayed available balance in pUSD.

## Linux-v1.1.2 - 2026-04-13
- Repinned the bundled EVPoly runtime sidecar to runtime `v1.1.2` (`feabdb0`).
- Inherits runtime `v1.1.2` behavior changes:
  - share-sized endgame and sessionband execution support
  - wallet snapshot refresh before merge sweeps
  - MM Sport exit inventory fallback to wallet snapshots
  - more aggressive redeem/merge auto-trigger defaults with unchanged `900s` cooldown
