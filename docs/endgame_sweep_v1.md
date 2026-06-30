# Endgame Sweep v1 Guide

## What It Does
`endgame_sweep_v1` enters late in the period using the local V1 three-checkpoint policy. It keeps the old proxy/base direction model and fixed 99c limit, then adds RTDS and Book99-style CEX-depth as guard/sizing layers.

## Default Scope
- Symbols: `BTC, ETH, SOL, XRP, DOGE, BNB, HYPE`
- Timeframes: `5m, 15m, 1h, 4h`
- Strategy toggle default: `EVPOLY_STRATEGY_ENDGAME_ENABLE=true`

## Local V1 Policy
The default checkpoint schedule is:

- `t-2000ms`: 20% of the period base size, minimum proxy/base move `2.0 bps`
- `t-1000ms`: 40% of the period base size, minimum proxy/base move `1.5 bps`
- `t-100ms`: 40% of the period base size, minimum proxy/base move `1.0 bps`

The checkpoint offsets are code-owned for this legacy path; saved profile/env offset values are ignored.

RTDS not-ready surcharge is added to the checkpoint bps threshold. With the default `EVPOLY_ENDGAME_RTDS_NO_GUARD_EXTRA_BPS=1.5`, missing/stale RTDS makes the effective thresholds `3.5/3.0/2.5 bps`.

## End-to-End Flow
1. Prewarm the current Polymarket market, constraints, metadata, and compact quote cache before close.
2. Read the live proxy/base move from the configured proxy feed.
3. For `BTC/ETH/SOL/XRP`, require Coinbase and Binance to agree on direction before the tick can trade.
4. Evaluate RTDS Chainlink guard. If RTDS has the exact period-open sample and fresh current sample, skip if direction disagrees or absolute RTDS move is below `EVPOLY_ENDGAME_RTDS_GUARD_MIN_BPS`.
5. If RTDS is unavailable, keep the proxy base and add `EVPOLY_ENDGAME_RTDS_NO_GUARD_EXTRA_BPS` to the tick's near-base threshold.
6. Require the proxy/base move to exceed the effective near-base threshold.
7. Require the old Polymarket mid band for the tick: `95-99c`, `97-99c`, then `98-99c`.
8. Evaluate Book99-style CEX-depth cost-to-flip against rolling depth quantiles.
9. Apply CEX-depth size multiplier: weak depth reduces size, and strong depth increases size.
10. Clamp the applied CEX-depth multiplier to `0.0..2.0`, then apply period cap and minimum order checks.
11. Submit a non-resting Endgame `FAK` buy at the fixed 99c limit, accepting positive partial fills and treating no-fill as failed.
12. Reject strategy/trader submits that reach the path inside the final 25ms close guard.

## Sizing Policy
Endgame execution is fixed to share sizing. Base share key: `EVPOLY_ENDGAME_BASE_SIZE_SHARES` (blank defaults to `50`). If the share key is blank, the legacy desktop profile field `EVPOLY_ENDGAME_BASE_SIZE_USD` is accepted as the share-size fallback. `EVPOLY_ENDGAME_EXECUTION_SIZE_MODE` is retained for desktop profile compatibility but does not change runtime mode.

Multipliers:

- Symbol: `BTC=1.0`, `ETH=0.8`, `SOL/XRP/DOGE/BNB/HYPE=0.5`
- Checkpoint split: `20/40/40`
- CEX-depth: multiplier can reduce weak-depth trades below 1.0 or increase strong-depth trades above 1.0, with live submit capped by period remaining size.

## Core Guards
- Coinbase/Binance same-direction guard for `BTC/ETH/SOL/XRP`
- RTDS exact-open guard when ready; RTDS no-guard bps surcharge when not ready
- Per-tick proxy/base bps thresholds
- Polymarket mid-band gate by tick
- Mandatory CEX-depth freshness/readiness
- Quote/constraint/metadata prewarm gates
- Per-period cap and minimum order checks
- Worker/trader final 25ms close guard
- FAK submit with partial-fill accounting

## Key Env Knobs
- `EVPOLY_STRATEGY_ENDGAME_ENABLE`
- `EVPOLY_ENDGAME_BASE_SIZE_SHARES`
- `EVPOLY_ENDGAME_BASE_SIZE_USD`
- `EVPOLY_ENDGAME_PER_PERIOD_CAP_USD`
- `EVPOLY_ENDGAME_SYMBOLS`
- `EVPOLY_ENDGAME_TIMEFRAMES`
- `EVPOLY_ENDGAME_RTDS_GUARD_ENABLE`
- `EVPOLY_ENDGAME_RTDS_GUARD_MIN_BPS`
- `EVPOLY_ENDGAME_RTDS_GUARD_STALE_MS`
- `EVPOLY_ENDGAME_RTDS_NO_GUARD_EXTRA_BPS`
- `EVPOLY_ENDGAME_CEX_DEPTH_GUARD_ENABLE`
- `EVPOLY_ENDGAME_CEX_DEPTH_MAX_AGE_MS`
- `EVPOLY_ENDGAME_CEX_DEPTH_REDUCE_QUANTILE`
- `EVPOLY_ENDGAME_CEX_DEPTH_INCREASE_QUANTILE`

Near-base distance is fixed in runtime: base tick thresholds are `2 / 1.5 / 1` bps, multiplied by symbol (`BTC` x1, `ETH`/`BNB` x1.5, all other Endgame symbols x2.5).
- `EVPOLY_ENDGAME_REQUIRE_PREWARMED_METADATA`
- `EVPOLY_ENDGAME_SUBMIT_TIMEOUT_MS`
