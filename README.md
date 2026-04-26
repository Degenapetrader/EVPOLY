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

See [LICENSE](LICENSE).

## Strategy Set
- `premarket_v1`
- `endgame_sweep_v1`
- `evcurve_v1`
- `evsnipe_v1`
- `mm_sport_v1`

## Current Default Runtime Profile
- Strategy toggles default ON: `premarket`, `endgame`, `evcurve`, `evsnipe`
- Strategy toggles default OFF: `mm_sport`
- Default symbols (`premarket`): `BTC,ETH,SOL,XRP`
- Default symbols (`endgame`, `evcurve`, `evsnipe`): `BTC,ETH,SOL,XRP,DOGE,BNB,HYPE`
- MM market mode default: `auto`

Defaults are defined by runtime config loaders and reflected in `.env.example` / `.env.full.example`.

## Docs
### Per-strategy guides
- [Premarket v1](docs/premarket_v1.md)
- [Endgame Sweep v1](docs/endgame_sweep_v1.md)
- [EVcurve v1](docs/evcurve_v1.md)
- [EVSnipe v1](docs/evsnipe_v1.md)

### Ops guides
- [Manual endpoint guide](docs/manual_endpoint_guide.md)
- [Setup Doctor](docs/setup_doctor.md)
- [Strategy combo guide](docs/strategy_combos.md)

## Quick Start
1. Install dependencies:
   `git tmux sqlite3 python3 python3-pip python3-venv build-essential pkg-config libssl-dev curl`
2. Install Rust:
   `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y`
3. Build:
   `cargo build --release --bin polymarket-arbitrage-bot`
4. Create env file:
   `cp .env.example .env`
5. Fill required secrets/URLs in `.env`.
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
- Order posting now uses local CLOB V2 SDK signing. Set `POLY_BUILDER_CODE` only when builder attribution is required.
- Relayer submit fallback uses `EVPOLY_RELAYER_REMOTE_SIGNER_TOKEN` only for non-order proxy wallet flows such as redeem, merge, approvals, and Auto-Redeem approval toggles.
- Shared timeframe discovery and EVSnipe discovery are local-first.
- If local discovery returns no usable result, runtime falls back to the configured remote discovery endpoints.

## Remote Alpha/Discovery Fallbacks
Remote endpoints still default to `https://alpha.evplus.ai/...` and retry to `https://alpha2.evplus.ai/...` on transport/timeout/429/5xx failure classes.

Timeout policy currently hardcoded in runtime:
- Premarket ladder alpha: `1000ms`
- Endgame alpha: `1000ms`
- EVcurve alpha: `1000ms`
- EVSnipe remote discovery: `2000ms`
- Shared timeframe discovery: `2000ms`

## Remote Onboarding (Signer + Alpha URLs)
Recommended helper env:
```bash
python3 -m venv .venv
. .venv/bin/activate
pip install --upgrade requests eth-account
```

Debian 12 fallback if you do not want a venv:
```bash
python3 -m pip install --break-system-packages --upgrade requests eth-account
```

Run onboarding:
```bash
python3 scripts/remote_onboard.py \
  --wallet "0xYOUR_EOA_WALLET" \
  --private-key "$POLY_PRIVATE_KEY" \
  --signature-type 1 \
  --proxy-wallet "$POLY_PROXY_WALLET_ADDRESS" \
  --write-env-file .env
```

Onboarding writes all remote token destinations it can populate from API runtime, including relayer submit signer, discovery, per-strategy alpha, and admin token defaults.
Order posting stays local through the CLOB V2 SDK.
It should populate those destinations for all strategies, not only the strategies currently enabled.

Important sizing note:
- Set strategy base-size vars explicitly:
  - `EVPOLY_PREMARKET_BASE_SIZE_USD`
  - `EVPOLY_ENDGAME_BASE_SIZE_USD`
  - `EVPOLY_EVCURVE_BASE_SIZE_USD`
- If left blank, each defaults to `100` USD.

Important relayer note:
- Redeem/merge primary path uses:
  - `RELAYER_API_KEY`
  - `RELAYER_API_KEY_ADDRESS`
- If relayer API keys are not available, proxy wallet relayer flows can fall back to `EVPOLY_RELAYER_REMOTE_SIGNER_TOKEN`.
- Onboarding does not generate relayer credentials; you must create them manually in Polymarket.
- Get these from:
  `https://polymarket.com/settings?tab=api-keys`

## Setup Doctor
Run Setup Doctor when a setup looks incomplete or a remote credential was cleared:

```bash
python3 scripts/setup_doctor.py --env-file .env
```

Setup Doctor:
- checks the baseline runtime credentials a healthy EVPOLY setup should have,
- reruns remote onboarding to refill any missing generateable remote fields,
- reports manual-only fields like relayer credentials as `needs_you`,
- does not block the bot from running.

## Manual Endpoint Service
Standalone HTTP API binary:
```bash
cargo run --release --bin manual_bot -- --bind 127.0.0.1 --port 8791
```

Use `--token` (or `EVPOLY_MANUAL_BOT_TOKEN`) to protect endpoints.

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

This local branch has been migrated for Polymarket CLOB V2 while preserving the seven EVPOLY strategy decision paths.

Runtime-level changes in this branch:

- Uses `polymarket_client_sdk_v2` with CLOB, CTF, WebSocket, Gamma, and Bridge features.
- Pins the Rust toolchain to `1.91.0` for the current Alloy/Rust SDK dependency floor.
- Defaults `POLY_CLOB_API_URL` to `https://clob-v2.polymarket.com` for pre-cutover testing.
- Uses CLOB V2 order signing through the SDK; V2 signed orders carry `timestamp`, `metadata`, and `builder` through the SDK instead of legacy `nonce`, `feeRateBps`, and `taker` fields.
- Uses `POLY_BUILDER_CODE` for V2 builder attribution. Remote submit signing is restricted to non-order relayer flows.
- Moves direct collateral contract checks to the pUSD collateral token address and uses the SDK V2 exchange addresses through `exchange_v2` where present.
- Replaces Gamma offset discovery with `/events/keyset` and `/markets/keyset`, using `after_cursor` / `next_cursor` and local skipping only for callers that still pass a legacy `offset` argument.

Operational cutover guidance:

1. Run read-only and dry-run tests against `https://clob-v2.polymarket.com`.
2. Confirm pUSD balance and allowances before enabling live BUY orders.
3. Confirm `POLY_BUILDER_CODE` is set if builder attribution is required.
4. Stop live trading and cancel resting orders before the Polymarket V2 cutover window.
5. After cutover, production can use `https://clob.polymarket.com` once it reports V2.
