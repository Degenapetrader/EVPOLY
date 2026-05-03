# Deposit Wallet Sidecar Plan

Source: https://docs.polymarket.com/trading/deposit-wallet-migration#rust

## Current Truth

- The desktop sidecar runtime is GitHub `main`, pinned by `src-tauri/sidecar-core.lock`.
- Local runtime worktree is `C:\Users\Administrator\Desktop\UI` on `main2`.
- Desktop true north is `C:\Users\Administrator\Desktop\UI-v2.1.0-build`.
- Desktop UI/config can store deposit-wallet fields, but live trading is not complete until the sidecar supports `POLY_1271`.
- Existing EOA, Proxy, and Safe profiles must remain unchanged.

## Goal

Make the sidecar runtime trade from an already deployed, funded, approved deposit wallet:

- `POLY_SIGNATURE_TYPE=3`
- `POLY_DEPOSIT_WALLET_ADDRESS=<deposit wallet>`
- `POLY_FUNDER_WALLET_ADDRESS=<deposit wallet>`
- CLOB auth uses `SignatureType::Poly1271`
- CLOB funder, maker, and signer are the deposit wallet

Deposit wallet creation, funding, and approval batches are separate onboarding work. The sidecar change makes trading correct after that state exists and refreshes CLOB balance/allowance cache for deposit-wallet order checks.

## Magic Wallet Creation Boundary

Copy only the SaaS Magic email OTP and Magic Core owner-wallet generation/export flow.

Do not copy SaaS Polymarket Safe/proxy provisioning, `signature_type=2`, or server-side runtime signing.

Target desktop flow:

1. User enters email and completes Magic OTP.
2. Magic/Core creates the owner EOA wallet.
3. Desktop receives/decrypts the owner private key locally.
4. Desktop creates a new local profile; existing profiles are untouched.
5. Deposit-wallet onboarding creates/records the new deposit wallet for that owner.
6. Runtime trades with `POLY_SIGNATURE_TYPE=3` and `SignatureType::Poly1271`.

## Runtime Work In `C:\Users\Administrator\Desktop\UI`

1. Upgrade runtime `polymarket_client_sdk_v2` from `=0.5.1` to `=0.6.0-canary.1`.
2. Add config/env support for `POLY_DEPOSIT_WALLET_ADDRESS` and `POLY_FUNDER_WALLET_ADDRESS`.
3. Resolve funder addresses by signature type:
   - signature `1/2`: only `POLY_PROXY_WALLET_ADDRESS`
   - signature `3`: `POLY_FUNDER_WALLET_ADDRESS`, then `POLY_DEPOSIT_WALLET_ADDRESS`
   - unset signature with deposit/funder fields fails closed
4. Map signature types:
   - `0 -> SignatureType::Eoa`
   - `1 -> SignatureType::Proxy`
   - `2 -> SignatureType::GnosisSafe`
   - `3 -> SignatureType::Poly1271`
5. Update every CLOB authentication path in `src/api.rs`:
   - cached authenticated client
   - create/derive API creds
   - limit order posting
   - market order posting
   - batch posting
   - collateral and conditional balance allowance update
6. For `signature_type=3`, require a deposit/funder wallet and use SDK order builders so maker/signer are handled as `POLY_1271`.
7. Block old Safe/proxy relayer redeem, merge, and wrap paths for `signature_type=3` until deposit-wallet `WALLET` batch signing exists.
8. Update runtime `.env.example` and `strategy-changelog.md` if strategy/order behavior changes.

## Desktop Work In `C:\Users\Administrator\Desktop\UI-v2.1.0-build`

1. After the runtime commit exists, update `src-tauri/sidecar-core.lock` to the new `main2` commit SHA.
2. Remove any sidecar patch that duplicates the same runtime changes if the main commit already contains them.
3. Force rebuild the sidecar:
   - `npm run desktop:prepare -- --force`
4. Rebuild/reinstall desktop locally after sidecar prepare succeeds.

## Local Alignment Gate

Implementation order is fixed:

1. Implement sidecar/runtime support in `C:\Users\Administrator\Desktop\UI`.
2. Implement desktop changes in `C:\Users\Administrator\Desktop\UI-v2.1.0-build` from that runtime commit.
3. Implement Linux changes in `C:\Users\Administrator\Desktop\UI-linux` last.

Current local state to clean before push:

- `C:\Users\Administrator\Desktop\UI` / `main2`: `937f97e`, dirty `.gitignore`, untracked `wallet_timing_winloss.json`.
- `C:\Users\Administrator\Desktop\UI-v2.1.0-build` / `fix/ui-v210-clean-onboarding`: `c73d429`, untracked `mockups/`.
- `C:\Users\Administrator\Desktop\UI-linux` / `release/linux-v2.1.1`: `02e32d0`, clean.

Before any push, all three worktrees must be clean and aligned to the same runtime sidecar commit. Desktop stays the true north for UI behavior; runtime stays true north for sidecar trading behavior.

## Verification

Runtime:

- `cargo fmt --all -- --check`
- `cargo check --all-targets`
- `cargo test --all-targets --quiet`
- prove `POLY_SIGNATURE_TYPE=3` maps to `Poly1271`
- prove deposit wallet is the CLOB funder
- prove old EOA/Proxy/Safe paths still work

Desktop:

- `npm run test -- src/lib/tauri-commands.test.ts src/lib/desktop-config.test.ts --run`
- `npm run build`
- `npm run desktop:prepare -- --force`
- local reinstall smoke test

## Local Push Order Later

1. Commit runtime sidecar support in `C:\Users\Administrator\Desktop\UI`.
2. Commit desktop lock/update in `C:\Users\Administrator\Desktop\UI-v2.1.0-build`.
3. Port the same lock/update to `C:\Users\Administrator\Desktop\UI-linux`.
4. Push only after local desktop is rebuilt and verified.
