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

Ladder behavior:
1. Scheduler emits the local intent around `T-4m`.
2. Runtime sends the local base ladder prices to alpha.
3. Alpha returns one aligned price shift across all rungs, bounded to about `+/-10%`.
4. Missing, rejected, malformed, or unavailable alpha ladder -> fail-closed skip for that asset intent.

Market discovery:
- Shared timeframe discovery is remote-first.
- Local discovery fallback is enabled in runtime.

## Order Ladder
Premarket base ladder is hardcoded to 6x6 with two timeframe buckets:
- `5m` prices: `0.31, 0.26, 0.22, 0.16, 0.09, 0.03`
- `15m / 1h / 4h` prices: `0.40, 0.30, 0.24, 0.18, 0.12, 0.06`
- Weights: `0.23, 0.23, 0.17, 0.14, 0.12, 0.11`

Alpha owns the final ladder prices used for submit. It applies one aligned random shift to the base prices; weights, min-notional sizing, caps, discovery, cancel scheduling, and order placement remain local.

Mode keys:
- `EVPOLY_PREMARKET_LADDER_MODE_5M`
- `EVPOLY_PREMARKET_LADDER_MODE_NON_M5`

Supported values:
- `normal`: use the default bucket ladder as-is
- `safe`: move every rung 10% lower, rounded up to the nearest cent
- `aggressive`: move every rung 10% higher, rounded up to the nearest cent

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
- `EVPOLY_PREMARKET_TP_ENABLE`
- `EVPOLY_REMOTE_PREMARKET_ALPHA_URL`
- `EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN`
- `EVPOLY_REMOTE_MARKET_DISCOVERY_URL`
- `EVPOLY_REMOTE_MARKET_DISCOVERY_TOKEN`
