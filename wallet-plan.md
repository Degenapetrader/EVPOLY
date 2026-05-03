# Deposit Wallet Migration Plan

Source checked: https://docs.polymarket.com/trading/deposit-wallet-migration#rust

## Decision

Add deposit-wallet support as a new wallet mode for new API users only. Do not migrate existing Proxy Wallet or Safe Wallet profiles unless the user explicitly creates a new deposit-wallet profile.

This repo must be deposit-wallet aware in two places:

- Desktop/Tauri profile, onboarding, portfolio, rewards, config, and generated env.
- Bundled runtime sidecar through `src-tauri/core-contract` and `src-tauri/core-patches`, because live order placement is in the sidecar runtime, not in the React UI.

The owner private key must stay local. Desktop may sign deposit-wallet batches locally, but it must not send the private key to EVPOLY web/API services.

## Polymarket Requirements That Matter

- Deposit wallets are for new API users; existing Proxy/Safe users stay on signature types `1` and `2`.
- Deposit wallet orders use CLOB `signatureType = 3`, also called `POLY_1271`.
- For deposit-wallet orders, both CLOB `maker` and `signer` must be the deposit wallet address.
- pUSD must be held by the deposit wallet. pUSD in the owner EOA does not fund deposit-wallet orders.
- Approvals must be executed from the deposit wallet through a relayer `WALLET` batch.
- After funding or approval changes, call CLOB balance allowance update with `signature_type = 3`.
- Rust SDK support is only the CLOB order path. The Rust SDK does not include the builder relayer client for `WALLET-CREATE` or `WALLET` batches.
- Polymarket's Rust CLOB path requires `polymarket_client_sdk_v2 = "=0.6.0-canary.1"` and `SignatureType::Poly1271`.

## Current Repo State

Desktop UI and profile model:

- `src/pages/Config.tsx` exposes only `Proxy Wallet`, `Safe Wallet`, and `EOA`.
- `src/lib/tauri-commands.ts` profile/config types expose `eoa_wallet_address`, `proxy_wallet_address`, and `signature_type`.
- `src-tauri/src/profile_manager.rs` stores only `eoa_wallet_address`, `proxy_wallet_address`, `wallet_address`, and `signature_type`.
- `Profile::primary_wallet_address()` returns EOA for `0`, otherwise `proxy_wallet_address`.

Desktop Rust API helpers:

- `src-tauri/src/portfolio_api.rs` and `src-tauri/src/liquidity_rewards.rs` reject any `POLY_SIGNATURE_TYPE` other than `0`, `1`, or `2`.
- Both helpers authenticate non-EOA profiles with `.funder(maker_address)`, so the pattern can extend cleanly to `Poly1271` once the SDK is upgraded.

Onboarding/config handoff:

- `src-tauri/src/onboard.rs` rejects `signature_type > 2`.
- `src-tauri/src/lib.rs` derives only proxy and Safe funders from the private key.
- `src-tauri/src/config_io.rs` writes `POLY_SIGNATURE_TYPE` and `POLY_PROXY_WALLET_ADDRESS` into the generated sidecar env.

Runtime sidecar:

- The desktop app launches `evpoly-bot.exe` with generated env from `src-tauri/src/bot_manager.rs`.
- This repo carries runtime changes as patches under `src-tauri/core-patches`, so deposit-wallet live trading must be added to the sidecar patch stack, not only to the desktop UI.

## Target Model

Add a fourth signature mode:

- `0`: EOA
- `1`: Proxy Wallet
- `2`: Safe Wallet
- `3`: Deposit Wallet / `POLY_1271`

Profile storage should add an explicit `deposit_wallet_address`. Keep `proxy_wallet_address` for existing users and backward compatibility.

Primary wallet selection should become:

- type `0`: `eoa_wallet_address`
- type `1`: `proxy_wallet_address`
- type `2`: `proxy_wallet_address` for the existing Safe field
- type `3`: `deposit_wallet_address`

Generated sidecar env should include:

- `POLY_SIGNATURE_TYPE=3`
- `POLY_DEPOSIT_WALLET_ADDRESS=<deposit wallet>`
- `POLY_FUNDER_WALLET_ADDRESS=<deposit wallet>`
- temporary compatibility: `POLY_PROXY_WALLET_ADDRESS=<deposit wallet>` until all runtime code uses the explicit funder key

## Onboarding Architecture

Use the backend/onboarding service for coordination, but keep private-key signing local.

1. Desktop derives the owner EOA from the local private key.
2. Desktop or backend derives/returns the deterministic deposit wallet address.
3. Backend or relayer submits `WALLET-CREATE`; this request does not need a user signature.
4. Desktop stores `deposit_wallet_address` in the new profile.
5. User funds the deposit wallet with pUSD.
6. Desktop signs required approval `WALLET` batches locally with the owner private key and submits them through the approved relayer route.
7. Desktop/runtime calls CLOB balance allowance update with `signature_type = 3`.
8. Runtime places orders with `SignatureType::Poly1271` and deposit wallet as funder/maker/signer.

Do not make Magic or EVPOLY web server sign for the user. For Magic-created wallets, the desktop path still must export/decrypt the owner private key locally, then run the same deposit-wallet onboarding flow as imported private keys.

## Implementation Plan

### 1. Desktop Profile And UI

- Add `deposit_wallet_address` to:
  - `src-tauri/src/profile_manager.rs`
  - `src-tauri/src/lib.rs` profile command payloads
  - `src/lib/tauri-commands.ts`
  - profile tests in `src-tauri/src/profile_manager.rs` and `src/lib/tauri-commands.test.ts`
- Add wallet mode `3` to `src/pages/Config.tsx`.
- Label mode `3` as `Deposit Wallet`.
- For new-wallet creation, default new API users to Deposit Wallet once backend/runtime support exists.
- Keep existing imported Proxy/Safe profile behavior unchanged.
- Make Settings display "Deposit Wallet Address" for mode `3`, not "Proxy Wallet Address".

### 2. Desktop Portfolio, Rewards, And Wallet Snapshot

- Upgrade `src-tauri/Cargo.toml` to `polymarket_client_sdk_v2 = "=0.6.0-canary.1"` only after confirming the rest of the desktop Rust code still compiles.
- Update signature parsing in:
  - `src-tauri/src/portfolio_api.rs`
  - `src-tauri/src/liquidity_rewards.rs`
- Map `3 -> SignatureType::Poly1271`.
- Authenticate type `3` with `.funder(deposit_wallet_address)`.
- Confirm portfolio, positions, open orders, and rewards use `profile.primary_wallet_address()`.
- Add tests that type `3` no longer rejects in the desktop API helpers.

### 3. Onboarding And Relayer Flow

- Update `src-tauri/src/onboard.rs` to accept `signature_type = 3`.
- Do not overload `proxy_wallet` for the long-term API contract. Add explicit `deposit_wallet_address`.
- Extend EVPOLY onboarding API contract so `/onboard/start` and `/onboard/finish` can return/store:
  - deposit wallet address
  - relayer URL
  - factory address
  - approval status
  - balance allowance sync status
- Decide whether desktop submits raw relayer `WALLET` batches directly or EVPOLY API proxies them. If EVPOLY proxies them, the payload must contain the signed batch, not the private key.
- Add clear failure states:
  - deposit wallet not deployed
  - pUSD not funded to deposit wallet
  - approvals missing
  - CLOB balance allowance not synced

### 4. Runtime Sidecar Order Path

This is mandatory before any Deposit Wallet profile can trade.

- Add explicit env parsing in the runtime patch stack:
  - `POLY_DEPOSIT_WALLET_ADDRESS`
  - `POLY_FUNDER_WALLET_ADDRESS`
  - `POLY_SIGNATURE_TYPE=3`
- Upgrade the runtime's `polymarket_client_sdk_v2` to `=0.6.0-canary.1`.
- Map signature type `3` to `SignatureType::Poly1271`.
- Configure the CLOB client with `.funder(deposit_wallet_address)`.
- Ensure order creation signs with `POLY_1271`; raw CLOB orders must have maker and signer equal to the deposit wallet.
- Ensure balance allowance update uses `signature_type = 3`.
- Verify negative-risk and standard exchange orders use the correct verifying contract. Prefer SDK-built orders, not hand-built wrapped signatures.

### 5. Generated Env And Docs

- Update `src-tauri/src/config_io.rs` generated env.
- Update `src-tauri/core-contract/.env.example`.
- Update relevant runtime patch docs under `src-tauri/core-patches`.
- Update `docs/setup_doctor.md` so Doctor explains Deposit Wallet readiness separately from Proxy/Safe readiness.
- Keep old `POLY_PROXY_WALLET_ADDRESS` text for legacy profiles, but avoid showing it as the primary label for deposit-wallet profiles.

### 6. Verification

Minimum desktop checks:

- `npm run test -- src/lib/tauri-commands.test.ts src/lib/desktop-config.test.ts --run`
- `npm run build`
- `cargo fmt --all -- --check` in `src-tauri`
- `cargo check --all-targets` in `src-tauri`
- `cargo test --all-targets --quiet` in `src-tauri`

Minimum runtime checks after patching sidecar:

- Build patched sidecar from `src-tauri/core-contract` plus all `src-tauri/core-patches`.
- Verify `POLY_SIGNATURE_TYPE=3` generated env maps to `Poly1271`.
- Verify CLOB client uses deposit wallet as funder.
- Verify order payload maker/signer are deposit wallet in a preprod or dry-run order test.
- Verify CLOB balance allowance update sends `signature_type=3`.
- Verify old Proxy/Safe profiles still place and cancel orders unchanged.

## Rollout Order

1. Backend/onboarding API supports deposit wallet deployment metadata and approval batch submission without private-key custody.
2. Runtime sidecar supports signature type `3` and `POLY_1271` orders.
3. Desktop stores/displays deposit wallet profiles and writes explicit deposit-wallet env.
4. UI enables Deposit Wallet creation for new users.
5. Existing Proxy/Safe users remain untouched.

## Main Risk

Do not ship the UI toggle before the runtime sidecar and onboarding/relayer path are ready. A Deposit Wallet profile that only changes `POLY_SIGNATURE_TYPE=3` in desktop will still fail trading unless:

- the deposit wallet is deployed,
- pUSD is held by the deposit wallet,
- approvals were submitted from the deposit wallet,
- CLOB balance allowance was synced with `signature_type=3`,
- runtime uses `Poly1271` and deposit wallet maker/signer/funder.

