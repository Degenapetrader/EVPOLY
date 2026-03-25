# Setup Doctor

EVPoly Setup Doctor checks the baseline setup fields a healthy profile should have, regenerates the remote credentials onboarding can provide, and tells the operator exactly what still needs manual input.

## What it checks

- private key
- proxy wallet for proxy/safe modes
- remote signer token
- primary order-signer token
- remote market discovery token
- Premarket alpha token
- Endgame alpha token
- MM Rewards alpha token
- EVSnipe discovery token
- effective EVCurve / SessionBand shared alpha coverage
- relayer API key
- relayer API key address

## What it can auto-fix

Setup Doctor reruns onboarding to refill every remote credential the onboarding service currently returns.

That includes:
- signer token
- primary order signer token
- shared discovery token
- Premarket alpha token
- Endgame alpha token
- MM Rewards alpha token
- EVSnipe discovery token
- any shared-alpha runtime backfill used for EVCurve and SessionBand

## What it cannot auto-fix

Setup Doctor does not generate:
- `RELAYER_API_KEY`
- `RELAYER_API_KEY_ADDRESS`

Get those from:

`https://polymarket.com/settings?tab=api-keys`

Doctor reports missing relayer fields as `needs_you`, but it does not block the bot from running.

## For users

- Open Home.
- Click `Doctor`.
- If EVPoly can regenerate the missing setup, it does it for you.
- If something still needs manual input, Doctor tells you exactly what to enter and where.

## For AI helpers

Use Doctor as the first missing-setup check before telling a user to re-enter remote credentials manually.

Doctor semantics:
- `ready`: baseline setup is present
- `fixed`: Doctor regenerated missing remote setup
- `needs_you`: manual or external-only fields are still missing
- `failed`: Doctor itself hit an execution error

Important:
- Doctor is advisory only and should not be treated as a launch gate.
- Relayer credentials are reported, not generated.
- Onboarding is expected to populate all remote token destinations for all strategies, not only enabled ones.
