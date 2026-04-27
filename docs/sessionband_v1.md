# SessionBand v1 Guide

## What It Does
`sessionband_v1` is a late-window checkpoint strategy driven by remote alpha decisions.

## Default Scope
- Symbols: `BTC, ETH, SOL, XRP`
- Timeframes: `5m, 15m, 1h, 4h`
- Strategy toggle default: `EVPOLY_STRATEGY_SESSIONBAND_ENABLE=true`

## Hardcoded Checkpoints
SessionBand currently emits only two checkpoints:
- `tau=2s` decision request at `2050ms` before close
- `tau=1s` decision request at `1050ms` before close
- Grace window: `500ms`

No other tau checkpoints are emitted.

## Alpha + Discovery Behavior
- Remote alpha endpoint: `EVPOLY_REMOTE_SESSIONBAND_ALPHA_URL`
- Token: `EVPOLY_REMOTE_SESSIONBAND_ALPHA_TOKEN`
- Runtime alpha timeout is hardcoded `1000ms`

Behavior:
- Remote alpha unavailable/invalid -> skip (no local alpha decision fallback)
- Direction mismatch between alpha and runtime side -> skip
- Market discovery path is remote-first with local fallback

## End-to-End Flow
1. Build symbol proxy feeds and period base anchor.
2. Wait for checkpoint crossing (`tau=2`, `tau=1`).
3. Apply near-base skip gate.
4. Resolve market (remote-first discovery, local fallback).
5. Fetch PM quotes/orderbook and apply freshness checks.
6. Request SessionBand alpha decision for current tau.
7. Enforce alpha-side and price-band consistency checks.
8. Build local size (alpha does not provide target size).
9. Apply scope cap, strategy cap, and arbiter gates.
10. Submit FAK order and enforce decision-to-submit timing gap guard.

## Sizing Policy
Base key: `EVPOLY_SESSIONBAND_BASE_SIZE_USD` (blank defaults to `100`).

Multipliers:
- Symbol: `BTC=1.0`, `ETH=0.8`, `SOL/XRP=0.5`
- Tau split: `tau=2 -> 30%`, `tau=1 -> 70%`

## Key Env Knobs
- `EVPOLY_STRATEGY_SESSIONBAND_ENABLE`
- `EVPOLY_SESSIONBAND_SYMBOLS`
- `EVPOLY_SESSIONBAND_TIMEFRAMES`
- `EVPOLY_SESSIONBAND_BASE_SIZE_USD`
- `EVPOLY_SESSIONBAND_STRATEGY_CAP_USD`
- `EVPOLY_REMOTE_SESSIONBAND_ALPHA_URL`
- `EVPOLY_REMOTE_SESSIONBAND_ALPHA_TOKEN`
