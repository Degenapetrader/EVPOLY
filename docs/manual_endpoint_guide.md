# Manual Endpoint Guide

## Purpose
`manual_bot` exposes authenticated HTTP endpoints for manual trading, run control, wallet checks, and operator recovery tools.

## Start Server
```bash
cargo run --release --bin manual_bot -- --bind 127.0.0.1 --port 8791
```

Optional auth token:
- CLI: `--token <TOKEN>`
- Env: `EVPOLY_MANUAL_BOT_TOKEN=<TOKEN>`

Request headers:
- `x-evpoly-manual-token: <TOKEN>`
- or `x-evpoly-admin-token: <TOKEN>`

## Route Surface
- `GET /health`
- `GET /manual/health`
- `POST /manual/premarket`
- `POST /manual/open`
- `POST /manual/close`
- `POST /manual/open/stop`
- `POST /manual/close/stop`
- `GET /manual/open/runs`
- `GET /manual/close/runs`
- `GET /manual/open/runs/{run_id}`
- `POST /manual/open/runs/{run_id}/stop`
- `GET /manual/close/runs/{run_id}`
- `POST /manual/close/runs/{run_id}/stop`
- `GET /manual/auto-redeem`
- `POST /manual/auto-redeem`
- `POST /manual/redeem`
- `POST /manual/merge`
- `GET /manual/positions`
- `GET /manual/balance`
- `GET /manual/pnl`
- `GET /manual/audit`
- `GET /manual/doctor/check`
- `POST /manual/doctor/fix`

## Open vs Close Semantics
- `/manual/open` is BUY flow.
- `/manual/close` is reduce-only SELL flow (must not create new buy exposure).
- `/manual/open` default price behavior when `price` is omitted:
  - `limit` uses current best bid.
  - if no best quote/orderbook is available for `limit`, the request fails.
- `/manual/open` `mode=limit` is one-shot submit and leaves the maker order resting (no reconcile timeout auto-cancel loop).
- `/manual/close` default price behavior when `price` is omitted:
  - `limit` uses current best ask.
  - if no best quote/orderbook is available for `limit`, the request fails (no silent `0.999` fallback).
  - `chase_limit` behavior is unchanged.
- `tp_price` is allowed only for `/manual/open`.
- `tp_price` on `/manual/close` is rejected (`400`).

## Sizing Semantics (Open/Close)
- Default sizing is **shares**.
- Preferred field: `size` (interpreted as shares unless `size_unit: "usd"` is set).
- Legacy fields still supported:
  - `target_shares` (explicit shares)
  - `size_usd` (explicit USD notional)
- Do not send both `size` and `size_usd` in the same request.

## Manual Order Modes
Supported `mode` values for open/close runs:
- `chase_limit`
- `limit`
- `market`

## Close-Run Safety
Implemented protections include:
- duplicate running scope guard (`action+condition+token`) returns conflict
- overlap prevention for same close target scope
- cancel confirmation before run finalization
- post-cancel fill reconciliation window for late fills
- explicit status + `stop_reason` transitions for timeout/cancel/stop outcomes

## Minimal Request Examples
Open (BUY):
```bash
curl -sS -X POST http://127.0.0.1:8791/manual/open \
  -H 'content-type: application/json' \
  -H 'x-evpoly-manual-token: YOUR_TOKEN' \
  -d '{
    "condition_id":"<condition_id>",
    "side":"up",
    "size":500,
    "mode":"chase_limit"
  }'
```

Close (reduce-only SELL):
```bash
curl -sS -X POST http://127.0.0.1:8791/manual/close \
  -H 'content-type: application/json' \
  -H 'x-evpoly-manual-token: YOUR_TOKEN' \
  -d '{
    "condition_id":"<condition_id>",
    "side":"up",
    "size":20,
    "mode":"limit"
  }'
```

Open using explicit USD:
```bash
curl -sS -X POST http://127.0.0.1:8791/manual/open \
  -H 'content-type: application/json' \
  -H 'x-evpoly-manual-token: YOUR_TOKEN' \
  -d '{
    "condition_id":"<condition_id>",
    "side":"up",
    "size":25,
    "size_unit":"usd",
    "mode":"chase_limit"
  }'
```

Stop run:
```bash
curl -sS -X POST http://127.0.0.1:8791/manual/close/runs/<run_id>/stop \
  -H 'x-evpoly-manual-token: YOUR_TOKEN'
```

Auto-Redeem status:
```bash
curl -sS http://127.0.0.1:8791/manual/auto-redeem \
  -H 'x-evpoly-manual-token: YOUR_TOKEN'
```

Enable Auto-Redeem:
```bash
curl -sS -X POST http://127.0.0.1:8791/manual/auto-redeem \
  -H 'content-type: application/json' \
  -H 'x-evpoly-manual-token: YOUR_TOKEN' \
  -d '{"enabled":true}'
```

## Operator Endpoints
- `/manual/positions`: live positions snapshot.
- `/manual/balance`: USDC balance/allowance plus live position summary.
- `/manual/pnl`: activity cashflow plus live unrealized estimate.
- `/manual/audit`: run-state audit plus fix suggestions.
- `/manual/auto-redeem`: read or toggle Polymarket Auto-Redeem approval for the configured wallet.
- `/manual/doctor/check`: issue scanner.
- `/manual/doctor/fix`: stop/cancel/sync/prune helper.
