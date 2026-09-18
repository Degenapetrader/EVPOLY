# EVPoly

EVPoly is a Rust trading engine for Polymarket with multiple strategy loops, shared arbiter/risk enforcement, persistent tracking (`tracking.db`), remote alpha integrations, and a standalone manual execution API.

Work best on VULTR VPS, Recommended 2 vCPU and 4GB RAM, server Amsterdam

$300 free credit: https://www.vultr.com/?ref=9750476

## Polymarket Referral

New to Polymarket? Create your account with EVPoly to support the project:

`https://polymarket.com/?r=EVPOLY`

Use the referral link before creating a new Polymarket account. Existing users likely cannot retroactively apply it through the bot or by importing a private key.

## License
This repository is source-available, non-commercial.

- You can use, modify, and share it for non-commercial use.
- You cannot sell it, offer it as paid SaaS/service, or use it for commercial profit without a separate commercial license.

Licensed under [PolyForm Noncommercial 1.0.0](LICENSE).

## Strategy Set
- `premarket_v1`
- `endgame_sweep_v1` (retired)
- `evcurve_v1` (retired)
- `sessionband_v1` (disabled)
- `evsnipe_v1`
- `mm_sport_v1` (MM 2.0)

## Current Default Runtime Profile
- Strategy toggles default ON: `premarket`, `evsnipe`
- Strategy toggles default OFF: `mm_sport`; Endgame, EVCurve and SessionBand cannot be enabled
- Default symbols (`premarket`): `BTC,ETH,SOL,XRP`
- Default symbols (`evcurve`, `sessionband`): `BTC,ETH,SOL,XRP`
- Default symbols (`endgame`, `evsnipe`): `BTC,ETH,SOL,XRP,DOGE,BNB,HYPE`

Defaults are defined by runtime config loaders and reflected in `.env.example` / `.env.full.example`.

## Docs
### Per-strategy guides
- [Premarket v1](docs/premarket_v1.md)
- [Endgame Sweep v1](docs/endgame_sweep_v1.md)
- [EVcurve v1](docs/evcurve_v1.md)
- [S-Band v1](docs/sessionband_v1.md)
- [EVSnipe v1](docs/evsnipe_v1.md)
- [MM 2.0](docs/mm_sport_v1.md)

### Ops guides
- [Manual endpoint guide](docs/manual_endpoint_guide.md)
- [Setup Doctor](docs/setup_doctor.md)
- [Strategy combo guide](docs/strategy_combos.md)
- [Public V2 migration guide](docs/public_v2_migration.md)

## Quick Start
1. Install dependencies:
   `git tmux sqlite3 python3 python3-pip python3-venv build-essential pkg-config libssl-dev curl`
2. Install Rust:
   `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y`
3. Build:
   `cargo build --release --bin polymarket-arbitrage-bot`
4. Create env file:
   `cp .env.example .env`
5. Fill wallet fields and strategy base sizes in `.env`.
6. Start:
   `./ev start live`

## Runtime Control (Required)
Use `./ev` for bot lifecycle control. Do not run live bot with direct `cargo run` for the main runtime.

`./ev` handles release build checks, tmux session management, `.env` loading, disk-guard pruning, wallet-sync worker management, and managed auto-restart.

## Useful Commands
- `./ev start live`
- `./ev start dry`
- `./ev restart live`
- `./ev status`
- `./ev logs 200`
- `./ev stop`
- `./ev autorestart status`
- `./ev autorestart on 6h`
- `./ev autorestart off`
- `./ev autorestart run-now`
- `./scripts/doctor.sh`
- `./scripts/bootstrap_oss.sh`

## Environment Files
- `.env.example`: minimal runtime template.
- `.env.full.example`: full env surface reference.
- `.env`: local runtime secrets/overrides (not committed).

Validate env coverage:
```bash
bash scripts/verify_env_coverage.sh --env-file .env
```

## Signer and Discovery Defaults
- Order posting uses local CLOB V2 SDK signing with the official EVPOLY builder code built in.
- Builder fees apply to all trades: 0.1% on both taker and maker fills.
- Leave `POLY_BUILDER_CODE` blank unless you are intentionally testing an advanced override.
- Builder fee rates are not configured locally. The CLOB validates the builder code and applies the active server-side rates at match time.
- Gasless proxy/safe approvals, merge and redeem require the user's own `RELAYER_API_KEY` and `RELAYER_API_KEY_ADDRESS` from [Polymarket](https://polymarket.com/settings?tab=api-keys). These are separate from locally derived CLOB trading credentials.
- Discovery queries Polymarket directly. Premarket uses its existing local ladder policy; EVSnipe retains its existing Hit-market strategy.
- Alpha hosts, EVPOLY AWS onboarding/signing and api-web/Magic wallet creation are no longer used. Import your existing signer private key; keep its matching wallet mode and funder address.
- Endgame and EVCurve are retired, including for saved profiles. SessionBand remains disabled. Existing positions retain the existing close/redeem paths.

## Setup Doctor
Run `python3 scripts/setup_doctor.py --env-file .env` to check local wallet fields and optional gasless relayer credentials. It is advisory and does not gate trading on EVPOLY service credentials.

## Manual Endpoint Service
Standalone HTTP API binary:
```bash
cargo run --release --bin manual_bot -- --bind 127.0.0.1 --port 8791 --token "$EVPOLY_MANUAL_BOT_TOKEN"
```

`manual_bot` requires `--token`, `EVPOLY_MANUAL_BOT_TOKEN`, or `EVPOLY_ADMIN_API_TOKEN`. Tokenless loopback access is disabled unless `EVPOLY_MANUAL_BOT_ALLOW_UNAUTH_LOCAL=true` is explicitly set for disposable local development.

Route details and payload examples:
- [docs/manual_endpoint_guide.md](docs/manual_endpoint_guide.md)

## Retention Cleanup
Install/update retention cron:
```bash
./scripts/install_evpoly_retention_cron.sh
```

Run cleanup now:
```bash
./scripts/evpoly_retention_cleanup.sh
```

## Development Checks
Run the same checks as CI before pushing:

```bash
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets --quiet
./scripts/security_audit.sh
```

## Runtime Data (Not Committed)
- `tracking.db`
- `events.jsonl`
- `history.toml`

## Contribution and Security
- [CONTRIBUTING.md](CONTRIBUTING.md)
- [SECURITY.md](SECURITY.md)
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)

## Strategy Change Rule
If strategy logic/risk/sizing/defaults change, update `strategy-changelog.md` in the same task/PR.

## Polymarket CLOB V2 migration notes

This local branch has been migrated for Polymarket CLOB V2 while preserving the six public EVPOLY strategy decision paths.

Runtime-level changes in this branch:

- Uses `polymarket_client_sdk_v2` with CLOB, CTF, WebSocket, Gamma, and Bridge features.
- Pins the Rust toolchain to `1.91.0` for the current Alloy/Rust SDK dependency floor.
- Defaults `POLY_CLOB_API_URL` to `https://clob.polymarket.com`; main2 is CLOB V2 only.
- Uses CLOB V2 order signing through the SDK; V2 signed orders carry `timestamp`, `metadata`, and `builder` through the SDK instead of legacy `nonce`, `feeRateBps`, and `taker` fields.
- Uses the built-in official builder code for V2 builder attribution. EVPOLY does not send local maker/taker fee bps on orders; Polymarket applies the active builder fee rates attached to that code at match time.
- Non-order relayer flows use locally signed payloads and user-owned Polymarket relayer API keys.
- Moves direct collateral contract checks to the pUSD collateral token address and uses the SDK V2 exchange addresses through `exchange_v2` where present.
- Replaces Gamma offset discovery with `/events/keyset` and `/markets/keyset`, using `after_cursor` / `next_cursor` and local skipping only for callers that still pass a legacy `offset` argument.

Operational cutover guidance:

1. Run read-only and dry-run tests against `https://clob.polymarket.com`.
2. Confirm pUSD balance and allowances before enabling live BUY orders.
3. Confirm order timing reports `builder_code_configured=true`; leave `POLY_BUILDER_CODE` blank unless overriding the built-in official code.
4. Confirm active builder fee rates from Polymarket, for example `/fees/builder-fees/<builderCode>`; scheduled fee changes may not be active immediately.
5. Stop live trading and cancel resting orders before the Polymarket V2 cutover window.
6. Keep main2 on the CLOB V2 API surface; the alpha service separately preserves SDK v1 compatibility for old clients.
