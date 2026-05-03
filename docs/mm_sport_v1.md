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

Sports route applies match-only, pregame-only, league filters, and live-game guard checks. Non-sports route remains reward-eligible but does not require match or pregame sports metadata. Non-sports discovery requires an end date when `EVPOLY_MM_SPORT_NONSPORT_END_EXIT_START_SEC` is enabled.

## Safety Controls

- Fresh-entry reward gates:
  - `EVPOLY_MM_SPORT_MIN_REWARD_RATE_PER_DAY=5`
  - `EVPOLY_MM_SPORT_REQUIRE_REWARD_ELIGIBLE=true`
  - discovery and the live quote loop both block fresh BUY quotes below the reward floor
  - when reward eligibility is required, fresh BUY quotes require real reward metadata (`reward_min_size_shares > 0` and `reward_max_spread > 0`)
- Match-market filter:
  - `EVPOLY_MM_SPORT_MATCH_ONLY=true`
  - default ON; sports futures/outrights are skipped for fresh entries unless this is explicitly disabled
- pUSD collateral quote cap:
  - `EVPOLY_MM_SPORT_QUOTE_SIZE_MODE=depth_ratio`
  - `EVPOLY_MM_SPORT_MULTIPLE_COLLATERAL_CAP_MULT=0.45`
  - `EVPOLY_MM_SPORT_DEPTH_RATIO_COLLATERAL_CAP_MULT=0.90`
- Inventory exit loss guard:
  - `EVPOLY_MM_SPORT_INVENTORY_EXIT_MAX_LOSS_CENTS=10`
  - exit SELL quotes are lifted to the configured floor below tracked average entry price
- Non-sports end-date fresh-entry halt:
  - `EVPOLY_MM_SPORT_NONSPORT_END_EXIT_START_SEC=172800`
  - non-sports markets missing an end date are skipped
  - inside the configured end window, fresh BUY entries are canceled/skipped while existing inventory cleanup remains active
- Minimum top-bid fresh-entry guard:
  - `EVPOLY_MM_SPORT_MIN_ENTRY_TOP_BID_PRICE=0.10`
  - paired BUY entry is skipped when either binary outcome top bid is below the configured price
- Sponsored reward filter:
  - `EVPOLY_MM_SPORT_ALLOW_SPONSORED_REWARDS=true`
  - sponsored rewards are allowed by default
  - when set to `false`, markets are excluded only if sponsored rewards are at least `EVPOLY_MM_SPORT_SPONSORED_REWARD_MIN_SHARE=0.50` of total daily rewards
- Route-specific BUY sizing:
  - existing `EVPOLY_MM_SPORT_*` sizing keys remain the Sport/default profile
  - optional `EVPOLY_MM_SPORT_NONSPORT_*` overrides apply to Non-S markets in Non-S or Dual route
  - missing Non-S overrides fall back to Sport/default values
  - overrides affect fresh BUY entry sizing and BUY-side depth/ratio hygiene only; SELL exits and inventory cleanup keep existing logic
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
- `EVPOLY_MM_SPORT_NONSPORT_QUOTE_SIZE_MODE`
- `EVPOLY_MM_SPORT_NONSPORT_QUOTE_SIZE_MULT`
- `EVPOLY_MM_SPORT_NONSPORT_MULTIPLE_COLLATERAL_CAP_MULT`
- `EVPOLY_MM_SPORT_NONSPORT_DEPTH_RATIO_COLLATERAL_CAP_MULT`
- `EVPOLY_MM_SPORT_NONSPORT_MIN_TOP_DEPTH_USD`
- `EVPOLY_MM_SPORT_NONSPORT_MAX_SHARE_RATIO`
- `EVPOLY_MM_SPORT_MIN_ENTRY_TOP_BID_PRICE`
- `EVPOLY_MM_SPORT_NONSPORT_END_EXIT_START_SEC`
- `EVPOLY_MM_SPORT_ALLOW_SPONSORED_REWARDS`
- `EVPOLY_MM_SPORT_SPONSORED_REWARD_MIN_SHARE`
- `EVPOLY_REMOTE_MM_SPORT_DEPTH_SKIP_ALPHA_URL`
- `EVPOLY_REMOTE_MM_SPORT_DEPTH_SKIP_ALPHA_TOKEN`
- `EVPOLY_REMOTE_MM_SPORT_ALPHA_TIMEOUT_MS`
- `EVPOLY_MM_SPORT_POST_ONLY`
- `EVPOLY_MM_SPORT_ORDER_SUBMIT_TIMEOUT_MS`
- `EVPOLY_MM_SPORT_MAX_SHARE_RATIO`
- `EVPOLY_MM_SPORT_FIFO_MAX_SHARE_RATIO`
- `EVPOLY_MM_SPORT_FIFO_WS_GAP_CANCEL_MS`
- `EVPOLY_MM_SPORT_ACTIVE_SPORT_MARKET_CAP`
- `EVPOLY_MM_SPORT_ACTIVE_NONSPORT_MARKET_CAP`
- `EVPOLY_MM_SPORT_MAX_MARKETS`
- `EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC`
- `EVPOLY_MM_SPORT_INVENTORY_EXIT_MAX_LOSS_CENTS`
- `EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC`
- `EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC`
- `EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MIN_SEC`
- `EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MAX_SEC`

## Notes

- The legacy generic rewards-MM strategy is not part of the public V2 runtime.
- Existing inventory and open orders can still be canceled or unwound even when a market no longer qualifies for fresh entry.
- Fresh-entry scope is capped by route: Dual defaults to 50 sports plus 50 non-sports candidates, while single-route Sport or Non-S runs default to 100 candidates for the selected route. Markets with inventory/open orders remain in scope for exit and cancel work.
- FIFO cancellation uses `EVPOLY_MM_SPORT_FIFO_MAX_SHARE_RATIO`, clamped to at least 110% of the larger Sport/Non-S fresh-entry max share ratio. FIFO cancels and natural quote expiry start a randomized fresh-BUY cooldown from `EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MIN_SEC` to `EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MAX_SEC`.
- Prime Line Coverage is platform-only and is not part of MM 2.0 in main2.
- Review sizing and inventory risk before combining MM 2.0 with heavy directional profiles.
