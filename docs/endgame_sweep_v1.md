# Endgame Sweep v1 Guide

## What It Does
`endgame_sweep_v1` enters late in the period, with local V1 checkpoint timing and a strict execution guard stack.

## Default Scope
- Symbols: `BTC, ETH, SOL, XRP, DOGE, BNB, HYPE`
- Timeframes: `5m, 15m, 1h, 4h`
- Strategy toggle default: `EVPOLY_STRATEGY_ENDGAME_ENABLE=true`

## Proxy Routing Defaults
- `BTC/ETH/SOL/XRP` use Coinbase as the primary proxy feed path.
- `BTC/ETH/SOL/XRP` also require a Binance direction-agreement guard before each alpha checkpoint can trade.
- `DOGE/BNB/HYPE` use Binance trade proxy feed path.
- `DOGE/BNB/HYPE` use a more tolerant alpha freshness guard for Binance-routed submits.

## Local V1 Policy And Fast Path
Endgame V1 uses the configured local tick offsets as its checkpoint policy. A background registry worker discovers the current Polymarket market, prewarms token metadata and market constraints, and keeps the Endgame Polymarket websocket scope subscribed before the due ticks.

Polymarket quotes are read from the compact Endgame quote cache. The REST `/books` batch worker refreshes that cache, and websocket book updates can refresh it as well. At the due tick, the decision path skips if the current market context or quote snapshot is missing/stale instead of doing cold discovery/orderbook work on the submit path.

Fast submit uses a typed Endgame intent and requires prewarmed order metadata by default. This keeps build/sign/post on cached metadata and avoids a submit-time token metadata probe.

## End-to-End Flow
1. Build symbol proxy feeds (Coinbase primary feeds plus Binance guard/routing feeds).
2. Compute/restore period base anchor.
3. Prewarm the current Polymarket market, constraints, metadata, and quote cache before close.
4. Use the local V1 checkpoint offsets for due ticks.
5. For each due checkpoint, require Coinbase/Binance direction agreement for `BTC/ETH/SOL/XRP`.
6. Build direction/probability plan.
7. Apply mandatory near-base skip gate.
8. Read market context and quotes from the Endgame registry/cache.
9. Enforce quote freshness + market constraints.
10. Apply poly price-band / entry-price guards.
11. Apply EV-safe sizing and cap checks.
12. Enqueue to arbiter/trader.
13. Enforce submit-time proxy freshness and cached-metadata fast submit checks.

## Sizing Policy
Endgame execution is fixed to share sizing. Base share key: `EVPOLY_ENDGAME_BASE_SIZE_SHARES` (blank defaults to `50`). `EVPOLY_ENDGAME_EXECUTION_SIZE_MODE` is retained for desktop profile compatibility but does not change runtime mode.

Multipliers:
- Symbol: `BTC=1.0`, `ETH=0.8`, `SOL/XRP/DOGE/BNB/HYPE=0.5`
- Checkpoint size weights use fixed runtime defaults.

## Core Guards
- Mandatory Endgame near-base skip gate defaults to `1.5` bps (`EVPOLY_ENDGAME_NEAR_BASE_SKIP_BPS`).
- `BTC/ETH/SOL/XRP` require Coinbase and Binance to agree on up/down direction versus their period-open proxy base before an Endgame tick can submit.
- Quote/proxy freshness gates
- Submit-time stale guard from the local V1 policy
- Safety stop defaults to `0s` so local late-window checkpoints can fire.
- Min entry / price-band gates
- Per-period and strategy cap gates

## Key Env Knobs
- `EVPOLY_STRATEGY_ENDGAME_ENABLE`
- `EVPOLY_ENDGAME_BASE_SIZE_USD`
- `EVPOLY_ENDGAME_PER_PERIOD_CAP_USD`
- `EVPOLY_ENDGAME_SYMBOLS`
- `EVPOLY_ENDGAME_TIMEFRAMES`
- `EVPOLY_ENTRY_WORKER_COUNT_ENDGAME` (code default `8`)
- `EVPOLY_ENDGAME_FAST_SUBMIT_ENABLE`
- `EVPOLY_ENDGAME_PM_QUOTE_CACHE_ENABLE`
- `EVPOLY_ENDGAME_QUOTE_MAX_AGE_MS`
- `EVPOLY_ENDGAME_REGISTRY_WORKER_ENABLE`
- `EVPOLY_ENDGAME_REST_BATCH_POLL_ENABLE`
- `EVPOLY_ENDGAME_REQUIRE_PREWARMED_METADATA`
- `EVPOLY_ENDGAME_NEAR_BASE_SKIP_BPS`
- `EVPOLY_ENDGAME_SAFETY_STOP_SEC` (default `0`)
