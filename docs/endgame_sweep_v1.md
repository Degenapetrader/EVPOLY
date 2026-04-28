# Endgame Sweep v1 Guide

## What It Does
`endgame_sweep_v1` enters late in the period, with checkpoint timing controlled by remote alpha policy and a strict execution guard stack.

## Default Scope
- Symbols: `BTC, ETH, SOL, XRP, DOGE, BNB, HYPE`
- Timeframes: `5m, 15m, 1h, 4h`
- Strategy toggle default: `EVPOLY_STRATEGY_ENDGAME_ENABLE=true`

## Proxy Routing Defaults
- `BTC/ETH/SOL/XRP` use Coinbase as the primary proxy feed path.
- `BTC/ETH/SOL/XRP` also require a Binance direction-agreement guard before each alpha checkpoint can trade.
- `DOGE/BNB/HYPE` use Binance trade proxy feed path.
- `DOGE/BNB/HYPE` use a more tolerant alpha freshness guard for Binance-routed submits.

## Alpha-Driven Signal
At runtime the bot requests one EVPlus Alpha signal per symbol/timeframe/period before the current period close / next period open.

The alpha signal controls the late-window checkpoint policy and submit freshness policy. The public client treats this as an opaque required signal.

Compatibility:
- SDK v1 clients that do not send `builder_code` still use the same `/v1/alpha/endgame/policy` endpoint.
- `main2` requests include the official builder code and use the v2 alpha signal path.

Config:
- `EVPOLY_REMOTE_ENDGAME_ALPHA_URL`
- `EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN`
- `EVPOLY_ENDGAME_ALPHA_REQUIRED=true` (default)
- Runtime alpha timeout is hardcoded `1000ms`

If required policy is unavailable, the period is skipped (fail-closed).

## End-to-End Flow
1. Build symbol proxy feeds (Coinbase primary feeds plus Binance guard/routing feeds).
2. Compute/restore period base anchor.
3. Request alpha signal before close / next period open.
4. If no valid policy and required mode is enabled, skip period.
5. For each due alpha checkpoint, require Coinbase/Binance direction agreement for `BTC/ETH/SOL/XRP`.
6. Build direction/probability plan.
7. Apply mandatory near-base skip gate.
8. Resolve market with remote-first discovery + local fallback.
9. Enforce quote/book freshness + market constraints.
10. Apply poly price-band / entry-price guards.
11. Apply EV-safe sizing and cap checks.
12. Enqueue to arbiter/trader.
13. Enforce submit-time stale guard from alpha signal.

## Sizing Policy
Base key: `EVPOLY_ENDGAME_BASE_SIZE_USD` (blank defaults to `50`).

Multipliers:
- Symbol: `BTC=1.0`, `ETH=0.8`, `SOL/XRP/DOGE/BNB/HYPE=0.5`
- Tick split: `tick0=20%`, `tick1=40%`, `tick2=40%`

## Core Guards
- Mandatory Endgame near-base skip gate fixed at `3.0` bps.
- `BTC/ETH/SOL/XRP` require Coinbase and Binance to agree on up/down direction versus their period-open proxy base before an Endgame tick can submit.
- Quote/proxy freshness gates
- Submit-time stale guard from the alpha signal
- Safety stop defaults to `0s` so alpha-owned late-window checkpoints can fire.
- Min entry / price-band gates
- Per-period and strategy cap gates

## Key Env Knobs
- `EVPOLY_STRATEGY_ENDGAME_ENABLE`
- `EVPOLY_ENDGAME_BASE_SIZE_USD`
- `EVPOLY_ENDGAME_PER_PERIOD_CAP_USD`
- `EVPOLY_ENDGAME_SYMBOLS`
- `EVPOLY_ENDGAME_TIMEFRAMES`
- `EVPOLY_ENTRY_WORKER_COUNT_ENDGAME` (code default `8`)
- `EVPOLY_ENDGAME_ALPHA_REQUIRED`
- `EVPOLY_REMOTE_ENDGAME_ALPHA_URL`
- `EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN`
- `EVPOLY_ENDGAME_TICK_OFFSETS_MS` (local labels/capacity only; alpha owns the live signal)
- `EVPOLY_ENDGAME_SAFETY_STOP_SEC` (default `0`)
