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
Endgame V1 uses the configured local tick offsets as its checkpoint policy. The default schedule is `10000,9000,8000,7000,6000,5000,4000,3000,2000,1000,100` ms before close. A background registry worker discovers the current Polymarket market, prewarms token metadata and market constraints, and keeps the Endgame Polymarket websocket scope subscribed before the due ticks.

Polymarket quotes are read from the compact Endgame quote cache. The REST `/books` batch worker refreshes that cache, and websocket book updates can refresh it as well. At the due tick, the decision path skips if the current market context or quote snapshot is missing/stale instead of doing cold discovery/orderbook work on the submit path.

Fast submit uses a typed Endgame intent and requires prewarmed order metadata by default. This keeps build/sign/post on cached metadata and avoids a submit-time token metadata probe.

## End-to-End Flow
1. Build symbol proxy feeds (Coinbase primary feeds plus Binance guard/routing feeds).
2. Compute/restore period base anchor.
3. Prewarm the current Polymarket market, constraints, metadata, and quote cache before close.
4. Use the local V1 checkpoint offsets for due ticks from `t-10s` through the final `t-100ms` slot.
5. For each due checkpoint, require Coinbase/Binance direction agreement for `BTC/ETH/SOL/XRP`.
6. Build direction/probability plan.
7. Read market context and quotes from the Endgame registry/cache.
8. Evaluate live CEX-depth cost to flip the period base boundary.
9. Evaluate Book99-port DVOL fair probability against the live Polymarket ask/mid and remaining tau.
10. Enforce quote freshness, market constraints, entry-price floor, and fee-aware VWAP edge.
11. Apply share sizing and cap checks.
12. Enqueue to arbiter/trader.
13. Enforce submit-time proxy freshness and cached-metadata fast submit checks.

## Sizing Policy
Endgame execution is fixed to share sizing. Base share key: `EVPOLY_ENDGAME_BASE_SIZE_SHARES` (blank defaults to `50`). `EVPOLY_ENDGAME_EXECUTION_SIZE_MODE` is retained for desktop profile compatibility but does not change runtime mode.

Multipliers:
- Symbol: `BTC=1.0`, `ETH=0.8`, `SOL/XRP/DOGE/BNB/HYPE=0.5`
- Checkpoint size weights use fixed runtime defaults: `4/5/6/7/8/9/10/11/12/13/15` percent across the eleven default checkpoints.

## Core Guards
- `BTC/ETH/SOL/XRP` require Coinbase and Binance to agree on up/down direction versus their period-open proxy base before an Endgame tick can submit.
- Book99-port CEX depth checks whether live external orderbook depth is strong enough for the current distance-to-base bucket and can reduce size on weak depth.
- Book99-port DVOL fetches Deribit BTC/ETH DVOL, uses ETH-DVOL synthetic multipliers plus RV30 overrides for alt symbols, and converts distance-to-base plus remaining tau into a fair probability.
- Quote/proxy freshness gates
- Submit-time stale guard from the local V1 policy
- Safety stop defaults to `0s` so local late-window checkpoints can fire.
- Min entry and fee-aware VWAP edge gates
- Per-period and strategy cap gates

## Key Env Knobs
- `EVPOLY_STRATEGY_ENDGAME_ENABLE`
- `EVPOLY_ENDGAME_BASE_SIZE_USD`
- `EVPOLY_ENDGAME_PER_PERIOD_CAP_USD`
- `EVPOLY_ENDGAME_SYMBOLS`
- `EVPOLY_ENDGAME_TIMEFRAMES`
- `EVPOLY_ENDGAME_TICK_OFFSETS_MS`
- `EVPOLY_ENTRY_WORKER_COUNT_ENDGAME` (code default `8`)
- `EVPOLY_ENDGAME_FAST_SUBMIT_ENABLE`
- `EVPOLY_ENDGAME_PM_QUOTE_CACHE_ENABLE`
- `EVPOLY_ENDGAME_QUOTE_MAX_AGE_MS`
- `EVPOLY_ENDGAME_REGISTRY_WORKER_ENABLE`
- `EVPOLY_ENDGAME_REST_BATCH_POLL_ENABLE`
- `EVPOLY_ENDGAME_REQUIRE_PREWARMED_METADATA`
- `EVPOLY_ENDGAME_SAFETY_STOP_SEC` (default `0`)
- `EVPOLY_ENDGAME_DVOL_ENABLE`
- `EVPOLY_ENDGAME_DVOL_REFRESH_MS`
- `EVPOLY_ENDGAME_DVOL_STALE_MS`
- `EVPOLY_ENDGAME_DVOL_RV_SYNTHETIC_ENABLE`
- `EVPOLY_ENDGAME_CEX_DEPTH_GUARD_ENABLE`
