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
The submit path also enforces a final close guard: strategy ticks and trader fast-submit reject orders that reach the path inside the last 250ms before market close.

## End-to-End Flow
1. Build symbol proxy feeds (Coinbase primary feeds plus Binance guard/routing feeds).
2. Require an exact REST period-open base anchor whose candle timestamp equals the market open. `BTC/ETH/SOL/XRP` require exact Coinbase and Binance opens; Coinbase intraday anchors are composed from live 1-minute candles, and Binance-routed symbols require an exact Binance open. `HYPE` uses Binance futures 1-minute klines for live intraday anchors.
3. Prewarm the current Polymarket market, constraints, metadata, and quote cache before close.
4. Use the local V1 checkpoint offsets for due ticks from `t-10s` through the final `t-100ms` slot.
5. For each due checkpoint, refresh scheduler time after any anchor fetches, then require Coinbase/Binance direction agreement versus exact period opens for `BTC/ETH/SOL/XRP`.
6. Build direction/probability plan.
7. Read market context and quotes from the Endgame registry/cache.
8. Evaluate both UP and DOWN candidates unless an already-submitted period direction lock restricts the period to one side.
9. Evaluate live CEX-depth cost to flip the period base boundary.
10. Evaluate Book99-port DVOL fair probability against the live Polymarket ask/mid and remaining tau, including edge at the probed Polymarket price.
11. Derive a dynamic edge-safe limit price from DVOL fair probability, Polymarket taker fees, and the configured edge floor, then floor it to the market tick.
12. Enforce quote freshness, market constraints, entry-price floor, visible-depth VWAP, and fee-aware edge.
13. Apply share sizing and cap checks.
14. Enqueue to arbiter/trader only outside the final 250ms close guard.
15. Enforce submit-time proxy freshness, close guard, and cached-metadata fast submit checks.

## Sizing Policy
Endgame execution is fixed to share sizing. Base share key: `EVPOLY_ENDGAME_BASE_SIZE_SHARES` (blank defaults to `50`). `EVPOLY_ENDGAME_EXECUTION_SIZE_MODE` is retained for desktop profile compatibility but does not change runtime mode.

Multipliers:
- Symbol: `BTC=1.0`, `ETH=0.8`, `SOL/XRP/DOGE/BNB/HYPE=0.5`
- Checkpoint size weights use fixed runtime defaults: `5/5.2/5.5/6/6.7/7.6/8.8/10.2/11.8/13.2/20` percent across the eleven default checkpoints. The weights sum to one full base-size period allocation, with t-10s smallest and t-100ms largest.

## Core Guards
- `BTC/ETH/SOL/XRP` require exact Coinbase and Binance period-open anchors whose candle timestamps equal the market open, and both sources must agree on up/down direction before an Endgame tick can submit. Coinbase intraday REST anchors use live 1-minute candles so current-period opens are available during the period.
- `HYPE` exact REST anchors use Binance futures 1-minute klines for intraday periods because Binance spot does not expose the `HYPEUSDT` kline symbol.
- Book99-port CEX depth checks whether live external orderbook depth is strong enough for the current distance-to-base bucket and can reduce size on weak depth.
- Book99-port DVOL fetches Deribit BTC/ETH DVOL, uses ETH-DVOL synthetic multipliers plus RV30 overrides for alt symbols, and converts distance-to-base plus remaining tau into a fair probability.
- Entry price is no longer fixed at 99c. Each tick uses the current fair probability, fees, edge floor, and live visible Polymarket asks to compute the max acceptable limit price and executable share count. Hidden-depth backfill is not assumed.
- Quote/proxy freshness gates
- Submit-time stale guard from the local V1 policy plus a 250ms final close guard
- Safety stop defaults to `0s` so local late-window checkpoints can fire.
- Min entry and fee-aware VWAP edge gates
- Per-period and strategy cap gates

## Key Env Knobs
- `EVPOLY_STRATEGY_ENDGAME_ENABLE`
- `EVPOLY_ENDGAME_BASE_SIZE_SHARES`
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
