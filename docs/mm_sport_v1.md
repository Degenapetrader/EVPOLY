# MM 2.0 Guide

`mm_sport_v1` is exposed publicly as MM 2.0. It quotes top-of-book two-outcome BUY orders on selected reward markets.

## Runtime Surface

- Strategy toggle default: `EVPOLY_STRATEGY_MM_SPORT_ENABLE=false`
- Hard-disable gate: `EVPOLY_MM_SPORT_HARD_DISABLE=false`
- Local runtime owns discovery route selection, local filters, quote placement, cancel/reprice, inventory handling, pUSD collateral caps, and local safety gates.
- EVPlus Alpha signal is required before new quotes are allowed.

## Alpha Dependency

During each MM 2.0 discovery cycle, local runtime sends candidate markets to:

`EVPOLY_REMOTE_MM_SPORT_DEPTH_SKIP_ALPHA_URL`

Blank `EVPOLY_REMOTE_MM_SPORT_DEPTH_SKIP_ALPHA_TOKEN` falls back to `EVPOLY_ALPHA_KEY`.

The alpha signal is a required market-risk gate. If alpha is unavailable, invalid, or rejects the request, MM 2.0 fails closed for the discovered markets and does not place new BUY quotes.

Main2 marks this request with `clob_version="v2"`, so the alpha service uses the CLOB V2 path. The alpha server still separately supports legacy SDK v1 clients during migration.

## Discovery Routes

`EVPOLY_MM_SPORT_DISCOVERY_ROUTE` controls candidate classes:

- `sports`: sports reward markets only. This is the default.
- `nonsports`: non-sports reward markets only.
- `dual`: sports and non-sports reward markets with condition-id dedupe.

Sports route applies match-only, pregame-only, league filters, and live-game guard checks. Non-sports route remains reward-eligible but does not require match or pregame sports metadata.

## Safety Controls

- Fresh-entry reward gates:
  - `EVPOLY_MM_SPORT_MIN_REWARD_RATE_PER_DAY=300`
  - `EVPOLY_MM_SPORT_REQUIRE_REWARD_ELIGIBLE=true`
  - when reward eligibility is required, fresh BUY quotes require real reward metadata (`reward_min_size_shares > 0` and `reward_max_spread > 0`)
- Match-market filter:
  - `EVPOLY_MM_SPORT_MATCH_ONLY=true`
  - default ON; sports futures/outrights are skipped for fresh entries unless this is explicitly disabled
- pUSD collateral quote cap:
  - `EVPOLY_MM_SPORT_MULTIPLE_COLLATERAL_CAP_MULT=0.45`
  - `EVPOLY_MM_SPORT_DEPTH_RATIO_COLLATERAL_CAP_MULT=0.90`
- Live-game guard:
  - `EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_ENABLE=true`
  - `EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_WS_ENABLE=true`
  - `EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_WS_STALE_MS=600000`
- Optional filters:
  - `EVPOLY_MM_SPORT_ALLOWED_SPORT_LEAGUE_CODES`
  - `EVPOLY_MM_SPORT_BLOCKED_SPORT_LEAGUE_CODES`
  - `EVPOLY_MM_SPORT_BLOCKED_COMPETITION_LEVELS`
  - `EVPOLY_MM_SPORT_MARKET_ALLOWLIST_KEYWORDS`
  - `EVPOLY_MM_SPORT_MARKET_BLACKLIST_KEYWORDS`
  - `EVPOLY_MM_SPORT_REWARD_MIN_SHARES_CAP`

## Key Env

- `EVPOLY_STRATEGY_MM_SPORT_ENABLE`
- `EVPOLY_MM_SPORT_HARD_DISABLE`
- `EVPOLY_MM_SPORT_DISCOVERY_ROUTE`
- `EVPOLY_MM_SPORT_MATCH_ONLY`
- `EVPOLY_MM_SPORT_MIN_REWARD_RATE_PER_DAY`
- `EVPOLY_MM_SPORT_REQUIRE_REWARD_ELIGIBLE`
- `EVPOLY_MM_SPORT_QUOTE_SIZE_MODE`
- `EVPOLY_MM_SPORT_QUOTE_SIZE_MULT`
- `EVPOLY_MM_SPORT_MULTIPLE_COLLATERAL_CAP_MULT`
- `EVPOLY_MM_SPORT_DEPTH_RATIO_COLLATERAL_CAP_MULT`
- `EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD`
- `EVPOLY_REMOTE_MM_SPORT_DEPTH_SKIP_ALPHA_URL`
- `EVPOLY_REMOTE_MM_SPORT_DEPTH_SKIP_ALPHA_TOKEN`
- `EVPOLY_REMOTE_MM_SPORT_ALPHA_TIMEOUT_MS`
- `EVPOLY_MM_SPORT_POST_ONLY`
- `EVPOLY_MM_SPORT_ORDER_SUBMIT_TIMEOUT_MS`
- `EVPOLY_MM_SPORT_MAX_SHARE_RATIO`
- `EVPOLY_MM_SPORT_MAX_MARKETS`

## Notes

- The legacy generic rewards-MM strategy is not part of the public V2 runtime.
- Prime Line Coverage is platform-only and is not part of MM 2.0 in main2.
- Review sizing and inventory risk before combining MM 2.0 with heavy directional profiles.
