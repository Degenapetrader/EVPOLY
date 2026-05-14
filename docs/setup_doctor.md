# Setup Doctor

EVPOLY Setup Doctor checks the baseline public V2 setup fields a healthy profile should have, confirms alpha self-onboarding posture, and tells the operator exactly what still needs manual input.

## What it checks

- `POLY_PRIVATE_KEY`
- `POLY_PROXY_WALLET_ADDRESS` for proxy/safe modes
- `EVPOLY_ALPHA_KEY` or `EVPOLY_ALPHA_AUTO_ONBOARD=true`
- `RELAYER_API_KEY`
- `RELAYER_API_KEY_ADDRESS`

## What it can auto-fix

Setup Doctor creates the target env file from the repo template if it is missing.

Normal alpha access is self-serve at runtime: blank `EVPOLY_ALPHA_KEY` is acceptable when `EVPOLY_ALPHA_AUTO_ONBOARD=true`.

## What it cannot auto-fix

Setup Doctor does not generate:

- `RELAYER_API_KEY`
- `RELAYER_API_KEY_ADDRESS`

Get those from:

`https://polymarket.com/settings?tab=api-keys`

Doctor reports them as `needs_you`, but it does not block the bot from running.

## For AI helpers

Use Setup Doctor as the first missing-setup step before telling a user to re-enter remote credentials manually.

Doctor result meanings:
- `ready`: baseline setup is present
- `fixed`: doctor regenerated missing remote setup
- `needs_you`: manual or external-only fields are still missing
- `failed`: doctor itself hit an execution error

Important:
- order posting is local through the CLOB V2 SDK with built-in official builder attribution
- builder fees apply to all trades: 0.1% on both taker and maker fills
- builder fee rates are server-side Polymarket Builder Profile settings; Setup Doctor does not create or validate local maker/taker fee bps
- per-strategy remote tokens are optional because blank values fall back to `EVPOLY_ALPHA_KEY`
- relayer credentials are reported, not generated
- doctor is advisory only and should not be treated as a runtime gate

## Usage

```bash
python3 scripts/setup_doctor.py --env-file .env
```

JSON output:

```bash
python3 scripts/setup_doctor.py --env-file .env --json
```
