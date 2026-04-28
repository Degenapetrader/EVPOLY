# Endgame Sweep v1 Guide

## What It Does
`endgame_sweep_v1` enters late in the period, with checkpoint timing controlled by remote alpha policy and a strict execution guard stack.

## Default Scope
- Symbols: `BTC, ETH, SOL, XRP, DOGE, BNB, HYPE`
- Timeframes: `5m, 15m, 1h, 4h`
- Strategy toggle default: `EVPOLY_STRATEGY_ENDGAME_ENABLE=true`

## Proxy Routing Defaults
- `BTC/ETH/SOL/XRP` use Coinbase as the primary proxy feed path.
- `BTC/ETH/SOL/XRP` also require a Binance direction-agreement guard before each alpha tick can trade.
- `DOGE/BNB/HYPE` use Binance trade proxy feed path.
- Alpha `submit_proxy_max_age_ms` is multiplied by `3x` for `DOGE/BNB/HYPE`.

## Alpha-Driven Timing
At runtime the bot requests one alpha policy per symbol/timeframe/period at `T-3m` before the current period close / next period open.

Policy includes:
- `tick_offsets_ms` (`t0/t1/t2` based on `2000,1000,100` ms before close)
- Alpha may move each offset up to `25%` closer to `T` only, so `2000ms` can become `1700ms` but not `2300ms`.
- `submit_proxy_max_age_ms` (submit-time stale guard)

Compatibility:
- SDK v1 clients that do not send `builder_code` still use the same `/v1/alpha/endgame/policy` endpoint.
- For those legacy shared-token requests, alpha returns the SDK v1-compatible `3000,1000,100` ms base schedule with small symmetric jitter.
- `main2` requests include the official builder code and keep the `2000,1000,100` near-T policy.

Config:
- `EVPOLY_REMOTE_ENDGAME_ALPHA_URL`
- `EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN`
- `EVPOLY_ENDGAME_ALPHA_REQUIRED=true` (default)
- Runtime alpha timeout is hardcoded `1000ms`

If required policy is unavailable, the period is skipped (fail-closed).

## End-to-End Flow
1. Build symbol proxy feeds (Coinbase primary feeds plus Binance guard/routing feeds).
2. Compute/restore period base anchor.
3. Request alpha policy at `T-3m` before close / next period open.
4. If no valid policy and required mode is enabled, skip period.
5. For each due alpha tick, require Coinbase/Binance direction agreement for `BTC/ETH/SOL/XRP`.
6. Build direction/probability plan.
7. Apply mandatory near-base skip gate.
8. Resolve market with remote-first discovery + local fallback.
9. Enforce quote/book freshness + market constraints.
10. Apply poly price-band / entry-price guards.
11. Apply EV-safe sizing and cap checks.
12. Enqueue to arbiter/trader.
13. Enforce submit-time stale guard from alpha policy.

## Sizing Policy
Base key: `EVPOLY_ENDGAME_BASE_SIZE_USD` (blank defaults to `50`).

Multipliers:
- Symbol: `BTC=1.0`, `ETH=0.8`, `SOL/XRP/DOGE/BNB/HYPE=0.5`
- Tick split: `tick0=20%`, `tick1=40%`, `tick2=40%`

## Core Guards
- Mandatory Endgame near-base skip gate fixed at `3.0` bps.
- `BTC/ETH/SOL/XRP` require Coinbase and Binance to agree on up/down direction versus their period-open proxy base before an Endgame tick can submit.
- Quote/proxy freshness gates
- Submit-time stale guard (policy-aware)
- Safety stop defaults to `0s` so alpha-owned millisecond ticks, including `t-100ms`, can fire.
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
- `EVPOLY_ENDGAME_TICK_OFFSETS_MS` (local labels/capacity only; alpha owns actual policy)
- `EVPOLY_ENDGAME_SAFETY_STOP_SEC` (default `0`)
