# Setup Doctor

EVPOLY Setup Doctor checks the baseline setup fields a healthy profile should have, regenerates the remote credentials that onboarding can provide, and tells the operator exactly what still needs manual input.

## What it checks

- `POLY_PRIVATE_KEY`
- `POLY_PROXY_WALLET_ADDRESS` for proxy/safe modes
- `EVPOLY_RELAYER_REMOTE_SIGNER_TOKEN`
- `EVPOLY_REMOTE_MARKET_DISCOVERY_TOKEN`
- `EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN`
- `EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN`
- `EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TOKEN`
- `EVPOLY_REMOTE_EVCURVE_ALPHA_TOKEN`
- `RELAYER_API_KEY`
- `RELAYER_API_KEY_ADDRESS`

## What it can auto-fix

Setup Doctor reruns remote onboarding to refill every remote credential the onboarding API currently returns. That includes relayer submit signer, discovery, and strategy remote tokens for all strategies.

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
- order posting is local through the CLOB V2 SDK; the remote signer token is only for non-order relayer fallback
- relayer credentials are reported, not generated
- doctor is advisory only and should not be treated as a runtime gate
- onboarding should populate all remote token destinations for all strategies

## Usage

```bash
python3 scripts/setup_doctor.py --env-file .env
```

JSON output:

```bash
python3 scripts/setup_doctor.py --env-file .env --json
```
