# EVPoly Linux Changelog

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
