# EVPoly Linux Changelog

## Linux-v2.4.6 - 2026-06-17
- Bumped Linux app/updater version to 2.4.6.
- Repinned the bundled runtime sidecar to runtime `v2.4.6` (`e02c283`) with local secret hardening, fail-closed manual API auth, HTTPS-only remote URL defaults, and remote EVcurve payload validation.

## Linux-v2.4.5 - 2026-06-16
- Bumped Linux app/updater version to 2.4.5.
- Repinned the bundled runtime sidecar to runtime `v2.4.5` (`4921f7d`) with fatal-panic exit and the cheap `/bot/liveness` admin endpoint.
- Added Linux bot watchdog recovery for wedged sidecars, using `/bot/liveness` every 30 seconds with restart-loop protection.
- Fixed active-profile bot state reporting so unattributed running processes are not shown as the active profile's bot.

## Linux-v2.4.3 - 2026-06-15
- Bumped Linux app/updater version to 2.4.3.
- Repinned the bundled runtime sidecar to runtime `v2.4.3` (`7086e95`) with the EVSNIPE pre-hit cutoff guard.
- Fixed the Linux Magic finish bridge payload so SaaS no longer rejects it for unexpected request fields.
- Fixed Linux Magic wallet profile saves so the provisioned signer credentials are not overwritten.

## Linux-v2.4.2 - 2026-06-13
- Bumped Linux app/updater version to 2.4.2.
- Repinned the bundled runtime sidecar to runtime `v2.4.2` (`9f26297`) with the MM Sport retired depth-skip Alpha gate hard-disabled.

## Linux-v2.4.1 - 2026-06-12
- Bumped Linux app/updater version to 2.4.1.
- Repinned the bundled runtime sidecar to runtime `v2.4.1` (`46a5afa`) with MM Sport rewards discovery, stable cache, and live-guard delta repair fixes.

## Linux-v2.4.0 - 2026-06-12
- Bumped Linux app/updater version to 2.4.0.
- Repinned the bundled runtime sidecar to runtime `v2.4.0` (`defe47b`).
- Includes the MM Sport throughput/default tuning and desktop sport market cap cleanup from the v2.4.0 runtime.

## Linux-v2.3.6 - 2026-05-14
- Bumped Linux app/updater version to 2.3.6.
- Repinned the bundled runtime sidecar to runtime `v2.3.6` (`822b03f`) with EVSnipe Hit Price discovery.

## Linux-v2.3.5 - 2026-05-14
- Added the MM 2.0 Non-S Active Hours controls to the Linux Strategy Settings UI.
- Aligned Linux MM 2.0 defaults with the desktop app, including Best Bid entry, quote-share caps, FIFO, and hidden pUSD cap controls.
- Hid SessionBand in the Linux strategy list and kept it disabled by default.
- Bumped Linux app/updater version to 2.3.5.

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
