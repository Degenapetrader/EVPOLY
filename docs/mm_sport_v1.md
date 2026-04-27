# MM Sport v1 Guide

`mm_sport_v1` is the public sports market-making strategy. It quotes top-of-book two-outcome BUY orders on pregame sports reward markets.

## Runtime Surface

- Strategy toggle default: `EVPOLY_STRATEGY_MM_SPORT_ENABLE=false`
- Hard-disable gate: `EVPOLY_MM_SPORT_HARD_DISABLE=false`
- Local runtime owns quote placement, cancel/reprice, inventory handling, and safety gates.
- Remote alpha owns the low-depth skip list.

## Remote Alpha Dependency

During each MM Sport discovery cycle, local runtime sends candidate sports reward markets to:

`EVPOLY_REMOTE_MM_SPORT_DEPTH_SKIP_ALPHA_URL`

Blank `EVPOLY_REMOTE_MM_SPORT_DEPTH_SKIP_ALPHA_TOKEN` falls back to `EVPOLY_ALPHA_KEY`.

Alpha returns markets whose two-outcome depth is below the alpha floor. Local runtime skips new quotes for those markets until the next discovery refresh.

## Key Env

- `EVPOLY_STRATEGY_MM_SPORT_ENABLE`
- `EVPOLY_MM_SPORT_HARD_DISABLE`
- `EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD`
- `EVPOLY_REMOTE_MM_SPORT_DEPTH_SKIP_ALPHA_URL`
- `EVPOLY_REMOTE_MM_SPORT_DEPTH_SKIP_ALPHA_TOKEN`
- `EVPOLY_REMOTE_MM_SPORT_ALPHA_TIMEOUT_MS`
- `EVPOLY_MM_SPORT_QUOTE_SIZE_MODE`
- `EVPOLY_MM_SPORT_QUOTE_SIZE_MULT`
- `EVPOLY_MM_SPORT_MAX_SHARE_RATIO`
- `EVPOLY_MM_SPORT_MAX_MARKETS`

## Notes

- The legacy generic rewards-MM strategy is not part of the public V2 runtime.
- Do not enable MM Sport together with heavy directional profiles until sizing and inventory risk are reviewed.
