# S-Band v1 Guide

## What It Does
`sessionband_v1` is exposed publicly as S-Band. It is a late-window checkpoint strategy driven by remote alpha decisions.

## Default Scope
- Symbols: `BTC, ETH, SOL, XRP`
- Timeframes: `5m, 15m, 1h, 4h`
- Strategy toggle default: `EVPOLY_STRATEGY_SESSIONBAND_ENABLE=true`

## Checkpoint Model
S-Band requests an EVPlus Alpha decision during the late close window. The public client treats the exact checkpoint policy as an opaque required signal.

## Alpha + Discovery Behavior
- Remote alpha endpoint: `EVPOLY_REMOTE_SESSIONBAND_ALPHA_URL`
- Token: `EVPOLY_REMOTE_SESSIONBAND_ALPHA_TOKEN`
- Runtime alpha timeout is hardcoded `1000ms`

Behavior:
- Remote alpha unavailable/invalid -> skip (no local alpha decision fallback)
- Direction mismatch between alpha and runtime side -> skip
- Market discovery path is remote-first with local fallback
- The local client treats alpha as an opaque required signal.

## End-to-End Flow
1. Build symbol proxy feeds and period base anchor.
2. Wait for the late-window checkpoint crossing.
3. Apply near-base skip gate.
4. Resolve market (remote-first discovery, local fallback).
5. Fetch PM quotes/orderbook and apply freshness checks.
6. Request S-Band alpha signal for current tau.
7. Enforce alpha-side and price-band consistency checks.
8. Build local size (alpha does not provide target size).
9. Apply scope cap, strategy cap, and arbiter gates.
10. Submit FAK order and enforce decision-to-submit timing gap guard.

## Sizing Policy
Base key: `EVPOLY_SESSIONBAND_BASE_SIZE_USD` (blank defaults to `10`).

Default execution mode is USD sizing with FAK submits.

Optional share execution mode:
- `EVPOLY_SESSIONBAND_EXECUTION_SIZE_MODE=shares`
- `EVPOLY_SESSIONBAND_BASE_SIZE_SHARES`
- Uses fixed `0.99` resting-limit BUYs with about 60s TTL.

Multipliers:
- Symbol: `BTC=1.0`, `ETH=0.8`, `SOL/XRP=0.5`

## Key Env Knobs
- `EVPOLY_STRATEGY_SESSIONBAND_ENABLE`
- `EVPOLY_SESSIONBAND_SYMBOLS`
- `EVPOLY_SESSIONBAND_TIMEFRAMES`
- `EVPOLY_SESSIONBAND_BASE_SIZE_USD`
- `EVPOLY_SESSIONBAND_EXECUTION_SIZE_MODE`
- `EVPOLY_SESSIONBAND_BASE_SIZE_SHARES`
- `EVPOLY_SESSIONBAND_STRATEGY_CAP_USD`
- `EVPOLY_REMOTE_SESSIONBAND_ALPHA_URL`
- `EVPOLY_REMOTE_SESSIONBAND_ALPHA_TOKEN`
