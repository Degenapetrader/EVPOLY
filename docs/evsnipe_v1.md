# EVSnipe v1 Guide

## What It Does
`evsnipe_v1` trades hit-price crypto markets using Binance live data and fast PM execution.

## Default Scope
- Symbols: `BTC, ETH, SOL, XRP, DOGE, BNB, HYPE`
- Strategy toggle default: `EVPOLY_STRATEGY_EVSNIPE_ENABLE=true`

## Discovery Model
EVSnipe discovery is remote-first with local fallback.

Remote config:
- `EVPOLY_REMOTE_EVSNIPE_DISCOVERY_URL`
- `EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TOKEN` (blank uses `EVPOLY_ALPHA_KEY`)

Runtime behavior:
- Remote EVSnipe discovery sends the official builder code and the proxy wallet header for auto-generated alpha keys.
- Remote timeout is hardcoded to `2000ms` (no env timeout knob).
- Remote host failover is supported (`alpha.evplus.ai` -> `alpha2.evplus.ai`) for transport/timeout/429/5xx classes.
- If remote is missing/unavailable/empty, runtime falls back to local Gamma discovery.

Local and remote discovery are aligned to the same EVSnipe filtering model (Poly-builder parity), including strike-window filtering.

## End-to-End Flow
1. Periodic discovery refresh tries remote first, then local fallback when needed.
2. Refresh anchor spot and apply strike-window filter.
3. Binance trade stream handles hit triggers through the low-latency trigger index.
4. On trigger, map rule -> side/token and build a materialized EVSnipe intent.
5. Submit FAK buy immediately through the EVSnipe fast path with the fixed max-buy guard.
6. Confirm-hit in-flight/completed conditions block later pre-hit legs for the same condition.
7. Dedupe/prune prevents duplicate fire for same condition/leg.

## Sizing and Caps
- `EVPOLY_EVSNIPE_SIZE_USD` default `5`
- `EVPOLY_EVSNIPE_PRE_LEG_RATIO` default `0.30`
- `EVPOLY_EVSNIPE_STRATEGY_CAP_USD` default `10000`; shared arbiter caps still apply.

## Key Env Knobs
- `EVPOLY_STRATEGY_EVSNIPE_ENABLE`
- `EVPOLY_EVSNIPE_SYMBOLS`
- `EVPOLY_ENTRY_WORKER_COUNT_EVSNIPE` (code default `4`)
- `EVPOLY_EVSNIPE_DISCOVERY_REFRESH_SEC`
- `EVPOLY_EVSNIPE_MAX_DAYS_TO_EXPIRY`
- `EVPOLY_EVSNIPE_STRIKE_WINDOW_PCT`
- `EVPOLY_EVSNIPE_SIZE_USD`
- `EVPOLY_EVSNIPE_STRATEGY_CAP_USD`
- `EVPOLY_EVSNIPE_PRICE_CACHE_ENABLE`
- `EVPOLY_EVSNIPE_REQUIRE_PREWARMED_METADATA`
- `EVPOLY_EVSNIPE_TRIGGER_MAX_AGE_MS`
- `EVPOLY_EVSNIPE_TICK_DROP_IF_FULL`
- `EVPOLY_EVSNIPE_SELECTED_TOKEN_PREWARM_MAX_TOKENS_PER_REFRESH`
- `EVPOLY_EVSNIPE_SELECTED_TOKEN_PREWARM_COOLDOWN_MS`
- `EVPOLY_REMOTE_EVSNIPE_DISCOVERY_URL`
- `EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TOKEN`
