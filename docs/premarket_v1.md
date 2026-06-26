# Premarket v1 Guide

## What It Does
`premarket_v1` builds deterministic pre-open ladder BUY orders on both sides (UP and DOWN) before each market opens.

## Default Scope
- Symbols: `BTC, ETH, SOL, XRP` (from global symbol enables)
- Timeframes: `5m, 15m, 1h, 4h`
- Strategy toggle default: `EVPOLY_STRATEGY_PREMARKET_ENABLE=true`
- Timeframe gate key: `EVPOLY_PREMARKET_TIMEFRAMES=5m,15m,1h,4h`

## Timing Model
The scheduler emits intents about 4 minutes before open:
- `5m`: minute `%5 == 1`
- `15m`: minute `%15 == 11`
- `1h`: minute `56`
- `4h`: minute `56` when hour `%4 == 3`

## Discovery Behavior
Premarket ladder prices are selected locally from deterministic mode settings. The runtime no longer calls a remote Premarket alpha endpoint before building the submit ladder.

Market discovery:
- Shared timeframe discovery is remote-first.
- Local discovery fallback is enabled in runtime.

## Order Ladder
Premarket uses a fixed base ladder per timeframe bucket and optional local Safe/Aggressive price bias.
Weights, min-notional sizing, caps, discovery, cancel scheduling, and order placement remain local.

Base prices:
- `5m`: `0.31, 0.26, 0.22, 0.16, 0.09, 0.03`
- `15m/1h/4h`: `0.40, 0.30, 0.24, 0.18, 0.12, 0.06`

Weights: `23%, 23%, 17%, 14%, 12%, 11%`

Mode keys:
- `EVPOLY_PREMARKET_LADDER_MODE_5M`: `normal`, `safe`, or `aggressive`
- `EVPOLY_PREMARKET_LADDER_MODE_NON_M5`: `normal`, `safe`, or `aggressive`

Bias keys:
- `EVPOLY_PREMARKET_SAFE_BIAS_PCT=-10`
- `EVPOLY_PREMARKET_AGGRESSIVE_BIAS_PCT=10`

`normal` uses the base ladder. `safe` and `aggressive` multiply the base prices by `1 + bias_pct / 100`, round up to the nearest cent, and clamp to `0.01..0.99`. Bias values are clamped to `-90..200`.

Rungs are clamped to a fixed `$5` minimum per order.
Reward `min_size` is ignored for Premarket ladder sizing and submit-time constraints.
Tick-size validation still applies, and some tiny orders may still be rejected by the venue.

## Sizing Policy
Base key: `EVPOLY_PREMARKET_BASE_SIZE_USD` (blank defaults to `10`).

Multipliers:
- Symbol: `BTC=1.0`, `ETH=0.8`, `SOL/XRP=0.5`
- Timeframe: `5m=0.75`, `15m=1.0`, `1h/4h=1.25`

Effective side budget:
`base_size * symbol_multiplier * timeframe_multiplier`

## Premarket TP Worker
Premarket TP is enabled by default:
- Toggle: `EVPOLY_PREMARKET_TP_ENABLE=true`
- Applies only to `15m/1h/4h` (not `5m`)
- Starts at `T+5m` after market launch
- Retries every `30s` until entry basis is available
- TP sell limit price rule: `max(2x entry, top_ask, 0.60)` then tick-aligned

## Execution Guards / Hardcoded Controls
- Submit cap per token-side: hardcoded `48`
- Premarket scope lanes: hardcoded max `48`
- Premarket scope lane queue cap: hardcoded `32`
- Premarket worker count: hardcoded `4`

## Key Env Knobs
- `EVPOLY_STRATEGY_PREMARKET_ENABLE`
- `EVPOLY_PREMARKET_BASE_SIZE_USD`
- `EVPOLY_PREMARKET_TIMEFRAMES`
- `EVPOLY_PREMARKET_LADDER_MODE_5M`
- `EVPOLY_PREMARKET_LADDER_MODE_NON_M5`
- `EVPOLY_PREMARKET_SAFE_BIAS_PCT`
- `EVPOLY_PREMARKET_AGGRESSIVE_BIAS_PCT`
- `EVPOLY_PREMARKET_TP_ENABLE`
- `EVPOLY_REMOTE_MARKET_DISCOVERY_URL`
- `EVPOLY_REMOTE_MARKET_DISCOVERY_TOKEN`
