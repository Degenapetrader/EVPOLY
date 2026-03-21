# EVPoly

EVPoly is a Rust trading engine for Polymarket with multiple strategy loops, shared arbiter/risk enforcement, persistent tracking (`tracking.db`), remote alpha integrations, and a standalone manual execution API.

Work best on VULTR VPS, Recommended 2 vCPU and 4GB RAM, server Amsterdam

$300 free credit: https://www.vultr.com/?ref=9750476

## License
This repository is source-available, non-commercial.

- You can use, modify, and share it for non-commercial use.
- You cannot sell it, offer it as paid SaaS/service, or use it for commercial profit without a separate commercial license.

See [LICENSE](LICENSE).

## Strategy Set
- `premarket_v1`
- `endgame_sweep_v1`
- `evcurve_v1`
- `sessionband_v1`
- `evsnipe_v1`
- `mm_rewards_v1`
- `mm_sport_v1`

## Current Default Runtime Profile
- Strategy toggles default ON: `premarket`, `endgame`, `evcurve`, `sessionband`, `evsnipe`
- Strategy toggles default OFF: `mm_rewards`, `mm_sport`
- Default symbols (`premarket`, `sessionband`): `BTC,ETH,SOL,XRP`
- Default symbols (`endgame`, `evcurve`, `evsnipe`): `BTC,ETH,SOL,XRP,DOGE,BNB,HYPE`
- MM market mode default: `auto`

Defaults are defined by runtime config loaders and reflected in `.env.example` / `.env.full.example`.

## Docs
### Per-strategy guides
- [Premarket v1](docs/premarket_v1.md)
- [Endgame Sweep v1](docs/endgame_sweep_v1.md)
- [EVcurve v1](docs/evcurve_v1.md)
- [SessionBand v1](docs/sessionband_v1.md)
- [EVSnipe v1](docs/evsnipe_v1.md)
- [MM Rewards v1](docs/mm_rewards_v1.md)

### Ops guides
- [Manual endpoint guide](docs/manual_endpoint_guide.md)
- [Strategy combo guide](docs/strategy_combos.md)

## Quick Start
1. Install dependencies:
   `git tmux sqlite3 python3 build-essential pkg-config libssl-dev curl`
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

## Remote Alpha/Discovery Defaults
By default runtime resolves remote endpoints to `https://alpha.evplus.ai/...` and retries to `https://alpha2.evplus.ai/...` on transport/timeout/429/5xx failure classes.

Timeout policy currently hardcoded in runtime:
- Premarket alpha: `1000ms`
- Endgame alpha: `1000ms`
- EVcurve alpha: `1000ms`
- SessionBand alpha: `1000ms`
- EVSnipe remote discovery: `2000ms`
- Shared timeframe discovery: `2000ms`

## Remote Onboarding (Signer + Alpha URLs)
Install helper deps:
```bash
python3 -m pip install --upgrade requests eth-account
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

Onboarding writes signer/discovery/alpha defaults when available from API runtime.

Important sizing note:
- Set strategy base-size vars explicitly:
  - `EVPOLY_PREMARKET_BASE_SIZE_USD`
  - `EVPOLY_ENDGAME_BASE_SIZE_USD`
  - `EVPOLY_EVCURVE_BASE_SIZE_USD`
  - `EVPOLY_SESSIONBAND_BASE_SIZE_USD`
- If left blank, each defaults to `100` USD.

Important relayer note:
- Redeem/merge primary path uses:
  - `RELAYER_API_KEY`
  - `RELAYER_API_KEY_ADDRESS`
- Get these from:
  `https://polymarket.com/settings?tab=api-keys`

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
