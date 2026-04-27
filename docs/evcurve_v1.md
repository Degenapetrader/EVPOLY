# EVcurve v1 Guide

## What It Does
`evcurve_v1` trades checkpoint-based entries using remote alpha decisions with max-buy curve controls and chase-limit execution.

## Default Scope
- Symbols: `BTC, ETH, SOL, XRP`
- Timeframes: `15m, 1h, 4h, 1d`
- `5m` is removed from EVcurve runtime path
- Strategy toggle default: `EVPOLY_STRATEGY_EVCURVE_ENABLE=true`

## Checkpoints
- `15m`: hardcoded `300, 240, 180` sec (`T-5m`, `T-4m`, `T-3m`)
- `1h`: `900, 600, 300` sec
- `4h`: `7200, 6300, 5400, 4500, 3600, 2700, 1800, 900`
- `1d`: hourly ladder from `57600` to `3600`

## Alpha + Discovery Behavior
- Remote alpha endpoint: `EVPOLY_REMOTE_EVCURVE_ALPHA_URL`
- Token: `EVPOLY_REMOTE_EVCURVE_ALPHA_TOKEN` (blank uses `EVPOLY_ALPHA_KEY`)
- Runtime alpha timeout is hardcoded `1000ms`

Behavior:
- Alpha owns Plan3/PlanDaily lookup, `p_flip`, hold-side selection, D1 sub-strategy selection, and max-buy decisioning.
- Runtime keeps checkpoint scheduling, base anchoring, near-base skip, market discovery, PM quote freshness, sizing/caps, and chase/FAK execution local.
- Alpha unavailable/invalid -> checkpoint skip (no local alpha fallback model).
- Market discovery path is remote-first with local fallback.
- Decision thresholds are fixed in code: `p_flip` cap is `0.11`, D1 EV-gap threshold is `0.12`; `EVPOLY_EVCURVE_MAX_FLIP_PROB` and `EVPOLY_EVCURVE_D1_EV_GAP` env overrides are ignored.

## End-to-End Flow
1. Poll active symbol/timeframe periods.
2. Build/restore base anchor per period.
3. Apply near-base skip gate.
4. Resolve market (remote-first discovery, local fallback).
5. Fetch PM quotes/orderbook and apply freshness checks.
6. Call remote EVcurve alpha for Plan3/PlanDaily checkpoint decision.
7. If trade allowed, compute size and cap checks.
8. Submit through chase-limit lifecycle.
9. Cancel/reprice loop manages working order until completion/stop.

## Sizing Policy
Base key: `EVPOLY_EVCURVE_BASE_SIZE_USD` (blank defaults to `10`).

Multipliers:
- Symbol: `BTC=1.0`, `ETH=0.8`, `SOL/XRP=0.5`
- Timeframe: `15m=0.75`, `1h=1.0`, `4h/1d=1.25`

## Order-Type Note
EVcurve has FAK logic only for very-late checkpoints (`<=60s`) on `15m/1h`.
With current hardcoded 15m checkpoints (`300/240/180`), default 15m flow is non-FAK.

## Key Env Knobs
- `EVPOLY_STRATEGY_EVCURVE_ENABLE`
- `EVPOLY_EVCURVE_SYMBOLS`
- `EVPOLY_EVCURVE_TIMEFRAMES`
- `EVPOLY_EVCURVE_BASE_SIZE_USD`
- `EVPOLY_EVCURVE_STRATEGY_CAP_USD`
- `EVPOLY_EVCURVE_D1_STRATEGY_CAP_USD`
- `EVPOLY_EVCURVE_MAX_FLIP_PROB` (ignored; fixed `0.11`)
- `EVPOLY_EVCURVE_D1_EV_GAP` (ignored; fixed `0.12`)
- `EVPOLY_REMOTE_EVCURVE_ALPHA_URL`
- `EVPOLY_REMOTE_EVCURVE_ALPHA_TOKEN`
