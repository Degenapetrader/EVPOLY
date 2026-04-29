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

## Alpha + Discovery Behavior
- Remote alpha endpoint: `EVPOLY_REMOTE_PREMARKET_ALPHA_URL`
- Token: `EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN`
- Runtime timeout: hardcoded `1000ms`

Alpha behavior:
1. Scheduler emits the local intent around `T-4m`.
2. Runtime requests an EVPlus Alpha signal for the local base ladder.
3. Missing, rejected, malformed, or unavailable alpha signal -> fail-closed skip for that asset intent.

Market discovery:
- Shared timeframe discovery is remote-first.
- Local discovery fallback is enabled in runtime.

## Order Ladder
Premarket uses an internal fixed base ladder and requests the final EVPlus Alpha ladder signal before submit.
The local runtime no longer exposes or honors public price-preset overrides.
Weights, min-notional sizing, caps, discovery, cancel scheduling, and order placement remain local.

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
- `EVPOLY_PREMARKET_TP_ENABLE`
- `EVPOLY_REMOTE_PREMARKET_ALPHA_URL`
- `EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN`
- `EVPOLY_REMOTE_MARKET_DISCOVERY_URL`
- `EVPOLY_REMOTE_MARKET_DISCOVERY_TOKEN`
