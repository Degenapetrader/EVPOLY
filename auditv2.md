# EVPOLY `main2` Rust SDK V2 Audit

Date: 2026-04-27
Branch audited: `main2`
Baseline used for branch comparison: `main`

## Executive Summary

`main2` is a large migration branch, not a narrow SDK dependency bump. The branch changes 56 files versus `main`, with roughly 3,818 insertions and 19,909 deletions. The largest behavioral areas are CLOB SDK V2 order signing/posting, builder attribution, relayer submit changes, alpha/manual API surface, MM Sport reshaping, and the retirement of the old generic rewards-MM runtime.

Build health is mixed:

- `cargo check --all-targets` passed.
- `cargo fmt --check` passed.
- `cargo test --all-targets` passed: 244 lib tests, 3 `alpha_service` tests, 69 main-binary tests, and bin targets with 0 tests.
- `cargo audit` passed locally after scanning 584 lockfile dependencies.
- `cargo clippy --all-targets -- -D warnings` failed on the pinned Rust `1.91.0` toolchain.
- `bash scripts/security_audit.sh` could not be run on this Windows host because `bash` is not installed; GitHub CI runs on Ubuntu, so CI can still execute it.

Release-readiness verdict: the branch compiles and tests pass, but I would not call it production-ready without fixing the high findings below, especially the untracked lockfile/CI reproducibility gap, SessionBand arbiter wiring, V2 exchange approval inconsistency, and alpha CLOB default mismatch.

## Scope Reviewed

Primary reviewed files:

- `Cargo.toml`, `Cargo.lock`, `.gitignore`, `.github/workflows/ci.yml`, `.cargo/audit.toml`
- `src/api.rs`, `src/trader.rs`, `src/builder_attribution.rs`, `src/entry_idempotency.rs`
- `src/arbiter.rs`, `src/strategy.rs`, `src/sessionband.rs`, `src/mm/mod.rs`, `src/main.rs`
- `src/bin/alpha_service.rs`, `src/bin/manual_bot.rs`, `src/bin/test_allowance.rs`, `src/bin/test_sell.rs`
- `.env.example`, `.env.full.example`, `README.md`, `docs/public_v2_migration.md`, `docs/mm_sport_v1.md`

The SDK source for `polymarket_client_sdk_v2 = 0.5.1` was also inspected locally under Cargo's registry cache for order builder behavior.

## Validation Results

| Check | Result | Notes |
| --- | --- | --- |
| `git diff --stat main...HEAD` | Pass | 56 files changed, 3,818 insertions, 19,909 deletions |
| `cargo check --all-targets` | Pass | Completed after waiting for Cargo build lock |
| `cargo fmt --check` | Pass | No formatting diff reported |
| `cargo test --all-targets` | Pass | 316 Rust tests passed across lib, main, and `alpha_service`; other bin targets had 0 tests |
| `cargo audit` | Pass | Scanned 584 lockfile dependencies |
| `cargo clippy --all-targets -- -D warnings` | Fail | 142+ clippy-denied findings under Rust `1.91.0` |
| `bash scripts/security_audit.sh` | Not runnable locally | `bash` is missing on this Windows host |

## Findings

### 1. Critical: `Cargo.lock` Is Ignored And Not Tracked

This repository is an application/runtime, not a reusable library. The SDK V2 branch depends on exact behavior from a new CLOB SDK stack, Alloy, reqwest, rustls, and many transitive crates. The generated `Cargo.lock` exists locally after running Cargo, but Git does not track it and `.gitignore` explicitly ignores it.

Evidence:

```text
.gitignore
1:/target
2:**/*.rs.bk
3:Cargo.lock
4:config.json
```

`git ls-files --error-unmatch Cargo.lock` returned:

```text
error: pathspec 'Cargo.lock' did not match any file(s) known to git
Did you forget to 'git add'?
```

The audit policy itself reasons about lockfile state:

```text
.cargo/audit.toml
11:# expires: 2026-06-30 id: RUSTSEC-2026-0037 reason: quinn-proto is present in Cargo.lock; cargo tree -i quinn-proto --target all reports no active dependency path.
21:# expires: 2026-06-30 id: RUSTSEC-2024-0388 reason: lockfile-only advisory in current build graph; no reachable path from active target/features.
```

Impact:

- CI and operator builds can silently resolve different transitive versions.
- `cargo audit` results are not reproducible from committed source.
- The exact SDK V2 behavior can drift even though `polymarket_client_sdk_v2` itself is pinned to `=0.5.1`.

Recommended fix:

1. Remove `Cargo.lock` from `.gitignore`.
2. Commit `Cargo.lock`.
3. Add `--locked` to CI Cargo commands.
4. Run `cargo check --locked --all-targets`, `cargo test --locked --all-targets`, and `cargo audit --locked`.

### 2. High: CI Does Not Use `--locked`

Even after committing a lockfile, the current CI commands allow dependency drift because they do not use `--locked`.

Evidence:

```yaml
.github/workflows/ci.yml
14:      - name: Rustfmt
15:        run: cargo fmt --all -- --check
16:      - name: Cargo Check
17:        run: cargo check --all-targets
18:      - name: Cargo Test
19:        run: cargo test --all-targets --quiet
20:      - name: Install cargo-audit
21:        uses: taiki-e/install-action@cargo-audit
22:      - name: Security Audit
23:        run: ./scripts/security_audit.sh
```

Impact:

- CI can pass against a graph different from the operator's runtime graph.
- Security audit can pass against a graph different from the built artifact.

Recommended fix:

- Change check/test to `cargo check --locked --all-targets` and `cargo test --locked --all-targets --quiet`.
- Update `scripts/security_audit.sh` to run `cargo audit --locked "$@"`.

### 3. High: SessionBand Is Public But Missing From Arbiter Priority And Per-Strategy Budget Wiring

`sessionband_v1` is one of the six public strategy decision paths, but `src/arbiter.rs` does not import it into the production priority table or per-strategy budget map. Unknown strategies fall to priority `9`, the lowest rank, and missing strategy caps fall back to the global cap.

Evidence that SessionBand is a first-class strategy:

```rust
src/strategy.rs
9:pub const STRATEGY_ID_PREMARKET_V1: &str = "premarket_v1";
10:pub const STRATEGY_ID_ENDGAME_SWEEP_V1: &str = "endgame_sweep_v1";
11:pub const STRATEGY_ID_EVCURVE_V1: &str = "evcurve_v1";
12:pub const STRATEGY_ID_SESSIONBAND_V1: &str = "sessionband_v1";
13:pub const STRATEGY_ID_EVSNIPE_V1: &str = "evsnipe_v1";
14:pub const STRATEGY_ID_MM_SPORT_V1: &str = "mm_sport_v1";
```

Evidence that arbiter imports omit SessionBand:

```rust
src/arbiter.rs
3:use crate::strategy::{
4:    Direction, Timeframe, STRATEGY_ID_ENDGAME_SWEEP_V1, STRATEGY_ID_EVCURVE_V1,
5:    STRATEGY_ID_EVSNIPE_V1, STRATEGY_ID_MM_SPORT_V1, STRATEGY_ID_PREMARKET_V1,
6:};
```

Evidence that the cap map omits SessionBand:

```rust
src/arbiter.rs
40:        let mut per_strategy_max_usd = HashMap::new();
41:        per_strategy_max_usd.insert(
42:            STRATEGY_ID_PREMARKET_V1.to_string(),
43:            env_f64("EVPOLY_ARB_STRAT_PREMARKET_MAX_USD", global_max_usd).max(0.0),
44:        );
45:        per_strategy_max_usd.insert(
46:            STRATEGY_ID_ENDGAME_SWEEP_V1.to_string(),
47:            env_f64("EVPOLY_ARB_STRAT_ENDGAME_MAX_USD", global_max_usd).max(0.0),
48:        );
49:        per_strategy_max_usd.insert(
50:            STRATEGY_ID_EVCURVE_V1.to_string(),
51:            env_f64("EVPOLY_ARB_STRAT_EVCURVE_MAX_USD", global_max_usd).max(0.0),
52:        );
53:        per_strategy_max_usd.insert(
54:            STRATEGY_ID_EVSNIPE_V1.to_string(),
55:            env_f64("EVPOLY_ARB_STRAT_EVSNIPE_MAX_USD", global_max_usd).max(0.0),
56:        );
57:        per_strategy_max_usd.insert(
58:            STRATEGY_ID_MM_SPORT_V1.to_string(),
59:            env_f64("EVPOLY_ARB_STRAT_MM_SPORT_MAX_USD", global_max_usd).max(0.0),
60:        );
```

Evidence that priority omits SessionBand:

```rust
src/arbiter.rs
388:fn strategy_priority(strategy_id: &str) -> u8 {
389:    match strategy_id {
390:        STRATEGY_ID_PREMARKET_V1 => 0,
391:        STRATEGY_ID_ENDGAME_SWEEP_V1 => 1,
392:        STRATEGY_ID_EVCURVE_V1 => 2,
393:        STRATEGY_ID_EVSNIPE_V1 => 3,
394:        STRATEGY_ID_MM_SPORT_V1 => 4,
395:        _ => 9,
396:    }
397:}
```

Evidence that the env surface documents a SessionBand arbiter cap:

```text
.env.full.example
595:# EVPOLY_ARB_STRAT_SESSIONBAND_MAX_USD: Per-strategy cap for sessionband_v1. Blank => inherits global cap.
596:EVPOLY_ARB_STRAT_SESSIONBAND_MAX_USD=10000
```

Impact:

- SessionBand can lose conflicts as an unknown/lowest-priority strategy.
- `EVPOLY_ARB_STRAT_SESSIONBAND_MAX_USD` is documented but ignored.
- A strategy that is enabled by default in `.env.example` is not protected by its advertised arbiter limit.

Recommended fix:

- Import `STRATEGY_ID_SESSIONBAND_V1` in `src/arbiter.rs`.
- Add `EVPOLY_ARB_STRAT_SESSIONBAND_MAX_USD` to `ArbiterConfig::from_env`.
- Add a SessionBand arm to `strategy_priority`.
- Add tests covering SessionBand priority and budget behavior.

### 4. High: Sell Approval Checks Still Use V1 Exchange Address In V2 Branch

Some V2 approval paths correctly use `exchange_v2.unwrap_or(exchange)`, but sell-side token allowance and `isApprovedForAll` diagnostics still use `config.exchange`. That is inconsistent with the migration note that V2 exchange addresses should be used where present.

Correct V2 pattern already used for USDC BUY balance/allowance:

```rust
src/api.rs
5004:        let balance = balance_allowance.balance;
5005:        // Get allowance for the Exchange contract
5006:        let config = contract_config(POLYGON, false)
5007:            .ok_or_else(|| anyhow::anyhow!("Failed to get contract config"))?;
5008:        let exchange_address = config.exchange_v2.unwrap_or(config.exchange);
```

Incorrect sell token allowance path:

```rust
src/api.rs
5155:        let balance = balance_allowance.balance;
5156:
5157:        // Get contract config to check which contract address we should be checking allowance for
5158:        let config = contract_config(POLYGON, false)
5159:            .ok_or_else(|| anyhow::anyhow!("Failed to get contract config"))?;
5160:        let exchange_address = config.exchange;
5161:
5162:        // Get allowance for the Exchange contract specifically
5163:        let allowance = balance_allowance
5164:            .allowances
5165:            .get(&exchange_address)
```

Incorrect `isApprovedForAll` path:

```rust
src/api.rs
5532:    /// Check if setApprovalForAll was already set for the Exchange contract
5533:    /// Returns true if the Exchange is already approved to manage all tokens
5534:    pub async fn check_is_approved_for_all(&self) -> Result<bool> {
5535:        let config = contract_config(POLYGON, false)
5536:            .ok_or_else(|| anyhow::anyhow!("Failed to get contract config from SDK"))?;
5537:
5538:        let ctf_contract_address = config.conditional_tokens;
5539:        let exchange_address = config.exchange;
```

V2 approval writer uses the V2 address:

```rust
src/api.rs
5881:        let config = contract_config(POLYGON, false)
5882:            .ok_or_else(|| anyhow::anyhow!("Failed to get contract config from SDK"))?;
5883:
5884:        let ctf_contract_address = config.conditional_tokens;
5885:        let exchange_address = config.exchange_v2.unwrap_or(config.exchange);
```

Impact:

- `test_allowance`, `test_sell`, and runtime diagnostics can report false missing approvals.
- Operators may run unnecessary approval flows or misdiagnose V2 sell failures.
- Any logic depending on these checks can make decisions from the wrong exchange address.

Recommended fix:

- Replace `config.exchange` with `config.exchange_v2.unwrap_or(config.exchange)` in sell approval checks.
- Consider checking normal and negative-risk V2 exchange operators where appropriate.
- Add a unit test asserting all V2 approval readers and writers use the same operator address.

### 5. High: Alpha Service Defaults To Old CLOB Host While Runtime Defaults To V2

The main runtime default and README say `POLY_CLOB_API_URL` defaults to `https://clob-v2.polymarket.com`, but `alpha_service` still defaults to `https://clob.polymarket.com`.

Alpha service default:

```rust
src/bin/alpha_service.rs
41:const DEFAULT_PLANDAILY_PATH: &str = "/opt/evpoly-alpha-service/alpha/plandaily.md";
42:const DEFAULT_GAMMA_URL: &str = "https://gamma-api.polymarket.com";
43:const DEFAULT_CLOB_URL: &str = "https://clob.polymarket.com";
```

Alpha service env fallback:

```rust
src/bin/alpha_service.rs
2659:    let gamma_url = std::env::var("POLY_GAMMA_API_URL")
2660:        .ok()
2661:        .map(|v| v.trim().to_string())
2662:        .filter(|v| !v.is_empty())
2663:        .unwrap_or_else(|| DEFAULT_GAMMA_URL.to_string());
2664:
2665:    let clob_url = std::env::var("POLY_CLOB_API_URL")
2666:        .ok()
2667:        .map(|v| v.trim().to_string())
2668:        .filter(|v| !v.is_empty())
2669:        .unwrap_or_else(|| DEFAULT_CLOB_URL.to_string());
```

Runtime config default:

```rust
src/config.rs
624:impl Default for Config {
625:    fn default() -> Self {
626:        Self {
627:            polymarket: PolymarketConfig {
628:                gamma_api_url: "https://gamma-api.polymarket.com".to_string(),
629:                clob_api_url: "https://clob-v2.polymarket.com".to_string(),
```

README migration note:

```text
README.md
228:- Uses `polymarket_client_sdk_v2` with CLOB, CTF, WebSocket, Gamma, and Bridge features.
229:- Pins the Rust toolchain to `1.91.0` for the current Alloy/Rust SDK dependency floor.
230:- Defaults `POLY_CLOB_API_URL` to `https://clob-v2.polymarket.com` for pre-cutover testing.
```

Impact:

- Running `alpha_service` without `POLY_CLOB_API_URL` can query a different CLOB host than the V2 runtime.
- This can cause mismatched discovery/market metadata during pre-cutover testing.

Recommended fix:

- Change `DEFAULT_CLOB_URL` in `alpha_service` to `https://clob-v2.polymarket.com`, or centralize the default.
- Add a test or startup log assertion showing the effective CLOB host.

### 6. Medium: Manual Bot Auth Is Optional When Token Is Missing

`manual_bot` defaults to `127.0.0.1`, which is safer, but if an operator binds it to a public interface without setting a token, `check_auth` permits all requests.

Evidence:

```rust
src/bin/manual_bot.rs
44:const DEFAULT_BIND: &str = "127.0.0.1";
45:const DEFAULT_PORT: u16 = 8791;
```

```rust
src/bin/manual_bot.rs
1479:fn check_auth(headers: &HeaderMap, state: &AppState) -> Result<(), ApiError> {
1480:    let Some(expected) = state.auth_token.as_ref() else {
1481:        return Ok(());
1482:    };
1483:    let provided = headers
1484:        .get("x-evpoly-manual-token")
1485:        .or_else(|| headers.get("x-evpoly-admin-token"))
```

Impact:

- Manual execution routes can be exposed without auth if `--bind 0.0.0.0` or equivalent is used and no token is configured.
- This is especially sensitive because manual APIs can place/cancel trading actions.

Recommended fix:

- Require a token whenever bind is not loopback.
- Prefer requiring a token always, with a deliberate `--unsafe-no-token-localhost` style escape hatch for local-only development.
- Log an explicit startup warning/error when auth is disabled.

### 7. Medium: `place_market_order` Re-authenticates Per Call And Has No Caller Price Cap

The primary limit-order path uses a cached authenticated client through `get_or_create_clob_client()`. The older `place_market_order` path builds and authenticates a new CLOB client on each call.

Evidence of cached primary path:

```rust
src/api.rs
3133:        for attempt in 1..=attempts {
3134:            let get_client_started = Instant::now();
3135:            let handle = self.get_or_create_clob_client().await?;
3136:            timing.get_client_ms += get_client_started.elapsed().as_millis() as i64;
3137:            match self.place_order_with_handle(&handle, order).await {
```

Evidence of per-call auth in `place_market_order`:

```rust
src/api.rs
6420:        // Build authentication builder with proxy wallet support
6421:        let mut auth_builder = ClobClient::new(&self.clob_url, self.clob_client_config()?)
6422:            .context("Failed to create CLOB client")?
6423:            .authentication_builder(&signer);
...
6475:        // Create CLOB client with authentication (equivalent to: new ClobClient(HOST, CHAIN_ID, signer, apiCreds, signatureType, funderAddress))
6476:        let client = auth_builder
6477:            .authenticate()
6478:            .await
6479:            .context("Failed to authenticate with CLOB API. Check your API credentials.")?;
```

Evidence of no caller price cap in this path:

```rust
src/api.rs
6568:        // Use actual market order (not limit order)
6569:        // Market orders don't require a price - they execute at the best available market price
6570:        // The SDK handles the price automatically based on current market conditions
...
6641:        let response = loop {
6642:            // Rebuild order builder for each retry (since it's moved when building)
6643:            let order_builder_retry = client
6644:                .market_order()
6645:                .token_id(token_id_u256)
6646:                .amount(amount.clone())
6647:                .side(side_enum)
6648:                .order_type(order_type_enum.clone());
```

SDK evidence: price is optional and the SDK computes a cutoff from orderbook depth when omitted:

```rust
polymarket_client_sdk_v2-0.5.1/src/clob/order_builder.rs
400:impl<K: AuthKind> OrderBuilder<Market, K> {
401:    /// Sets the price for this market builder. This is an optional field.
402:    #[must_use]
403:    pub fn price(mut self, price: Decimal) -> Self {
...
524:        let price = match self.price {
525:            Some(price) => price,
526:            None => self.calculate_price(order_type.clone()).await?,
527:        };
```

Impact:

- Repeated emergency sells or tests can hit auth/derive rate limits faster than the main cached path.
- A no-price market order can walk book depth according to the SDK's computed cutoff. That may be intended for emergency exits, but it is a different risk profile from the capped FAK/FOK path in `build_and_sign_order_with_handle`.

Recommended fix:

- Reuse `get_or_create_clob_client()` in `place_market_order`.
- Add an optional price cap parameter for emergency market orders, or route emergency exits through the existing capped FAK/FOK order path.
- Add a test proving SELL FAK/FOK behavior with and without a cap.

### 8. Medium: Clippy Fails Under The Pinned Toolchain

The branch pins Rust `1.91.0` and installs clippy, but strict clippy fails with `-D warnings`.

Evidence:

```toml
rust-toolchain.toml
1:[toolchain]
2:channel = "1.91.0"
3:components = ["rustfmt", "clippy"]
```

First clippy failures:

```text
src\trader.rs:6402:21 redundant field names in struct initialization
src\api.rs:3126:9 field assignment outside of initializer for an instance created with Default::default()
src\api.rs:4615:37 called `.as_ref().map(String::as_str)` on an `Option` value
src\api.rs:4923:31 using `clone` on type `Option<(Decimal, Decimal, i64)>` which implements the `Copy` trait
src\api.rs:5683:10 very complex type used
src\trader.rs:10587:12 equal expressions as operands to `&&`
```

The command ended with:

```text
error: could not compile `polymarket-arbitrage-bot` (lib) due to 142 previous errors
error: could not compile `polymarket-arbitrage-bot` (lib test) due to 147 previous errors
```

Impact:

- If clippy is added to CI later, this branch fails immediately.
- The `eq_op` finding in `src/trader.rs` points to a real duplicated condition, not just style.

Recommended fix:

- Either add targeted `#[allow(...)]` for accepted legacy patterns or clean the lints.
- At minimum, inspect and fix the duplicated condition in `src/trader.rs`.
- Decide whether clippy is a quality gate; if yes, add it to CI after cleanup.

### 9. Medium: SDK V2 Live Order/Relayer Paths Are Not Covered By Automated Integration Tests

Unit coverage is good for pure logic, but the risky migration surfaces are mostly in manual binaries or live-network methods.

Evidence:

- `cargo test --all-targets` ran many unit tests, but the SDK order/relayer bin targets had 0 tests.
- `src/bin/test_allowance.rs`, `src/bin/test_sell.rs`, `src/bin/test_limit_order.rs`, `src/bin/test_redeem.rs`, and `src/bin/test_relayer_key.rs` compile, but they are executable probes, not automated assertions in CI.

Impact:

- `cargo test` does not prove live CLOB V2 auth, order posting, approval, relayer submit, redeem, or merge behavior.
- The branch can be green locally while still failing against CLOB V2 auth/order/relayer endpoints.

Recommended fix:

- Add an ignored integration test suite for CLOB V2 sandbox/live-smoke credentials.
- Add dry-run contract tests for signed order payload shape, builder code presence, and approval target selection.
- Add CI-only unit tests for env/default consistency where live credentials are not required.

### 10. Low: `POLY_BUILDER_CODE` Documentation Conflicts With Built-In Default

The code and README say the official builder code is built in and normal users should leave `POLY_BUILDER_CODE` blank. Later cutover guidance says to confirm it is set if attribution is required.

Code evidence:

```rust
src/builder_attribution.rs
19:pub fn configured_builder_code() -> String {
20:    std::env::var("POLY_BUILDER_CODE")
21:        .ok()
22:        .map(|value| value.trim().to_string())
23:        .filter(|value| !value.is_empty())
24:        .unwrap_or_else(|| OFFICIAL_BUILDER_CODE.to_string())
25:}
```

README evidence:

```text
README.md
99:## Signer and Discovery Defaults
100:- Order posting uses local CLOB V2 SDK signing with the official EVPOLY builder code built in.
101:- Leave `POLY_BUILDER_CODE` blank unless you are intentionally testing an advanced override.
```

Conflicting guidance:

```text
README.md
236:Operational cutover guidance:
237:
238:1. Run read-only and dry-run tests against `https://clob-v2.polymarket.com`.
239:2. Confirm pUSD balance and allowances before enabling live BUY orders.
240:3. Confirm `POLY_BUILDER_CODE` is set if builder attribution is required.
```

Impact:

- Operators may set unnecessary overrides or think attribution is disabled when the env var is blank.

Recommended fix:

- Rewrite the cutover step to: "Confirm order timing reports `builder_code_configured=true`; leave `POLY_BUILDER_CODE` blank unless overriding the built-in official code."

### 11. Low: Local Script Portability Gaps On Windows

This repo is being audited from Windows. Some helper scripts assume Linux tooling.

Evidence:

```bash
scripts/verify_env_coverage.sh
70:# Runtime env keys read from code paths.
71:{
72:  rg -o --no-filename --glob '!src/bin/**' 'std::env::var\("[A-Z0-9_]+"\)' src
73:  rg -o --no-filename --glob '!src/bin/**' 'env::var\("[A-Z0-9_]+"\)' src
74:  # Catch helper wrappers like env_bool(...), env_u64(...), env_nonempty_named(...), etc.
75:  rg -o --no-filename --glob '!src/bin/**' '(Self::)?env_[a-z0-9_]+\("[A-Z0-9_]+"' src
```

README dependencies omit `ripgrep`:

```text
README.md
57:## Quick Start
58:1. Install dependencies:
59:   `git tmux sqlite3 python3 python3-pip python3-venv build-essential pkg-config libssl-dev curl`
```

Local audit evidence:

```text
bash scripts/security_audit.sh
bash : The term 'bash' is not recognized as the name of a cmdlet, function, script file, or operable program.
```

Impact:

- Windows operators cannot run documented shell checks without WSL/Git Bash.
- Linux operators following Quick Start may miss `rg` for env coverage.

Recommended fix:

- Document WSL/Git Bash requirement for Windows.
- Add `ripgrep` to Linux dependency list if `verify_env_coverage.sh` is part of the supported operator workflow.

## Verified Non-Issues / Intentional Changes

### Generic `mm_rewards_v1` Runtime Is Retired

The old generic rewards-MM modules were removed and only MM Sport remains in the public V2 runtime. This is a major surface reduction, but it appears intentional based on the public migration docs.

Evidence:

```text
docs/public_v2_migration.md
5:## Public Strategy Set
7:- `premarket_v1`
8:- `endgame_sweep_v1`
9:- `evcurve_v1`
10:- `sessionband_v1`
11:- `evsnipe_v1`
12:- `mm_sport_v1`
14:The legacy generic rewards-MM strategy is retired from the public runtime. Historical changelog entries may still mention it.
```

Evidence in strategy constants:

```rust
src/strategy.rs
9:pub const STRATEGY_ID_PREMARKET_V1: &str = "premarket_v1";
10:pub const STRATEGY_ID_ENDGAME_SWEEP_V1: &str = "endgame_sweep_v1";
11:pub const STRATEGY_ID_EVCURVE_V1: &str = "evcurve_v1";
12:pub const STRATEGY_ID_SESSIONBAND_V1: &str = "sessionband_v1";
13:pub const STRATEGY_ID_EVSNIPE_V1: &str = "evsnipe_v1";
14:pub const STRATEGY_ID_MM_SPORT_V1: &str = "mm_sport_v1";
```

### Builder Code Is Applied Through SDK Config

The branch sets builder code through `ClobConfig`, and SDK V2 carries that into order builders.

Repo evidence:

```rust
src/api.rs
735:    fn v2_builder_code(&self) -> Result<Option<B256>> {
736:        let raw = crate::builder_attribution::configured_builder_code();
737:        let code = B256::from_str(raw.trim())
738:            .with_context(|| format!("Invalid POLY_BUILDER_CODE: {}", raw))?;
739:        Ok(Some(code))
740:    }
741:
742:    fn clob_client_config(&self) -> Result<ClobConfig> {
743:        if let Some(builder_code) = self.v2_builder_code()? {
744:            Ok(ClobConfig::builder().builder_code(builder_code).build())
745:        } else {
746:            Ok(ClobConfig::default())
747:        }
```

SDK evidence:

```rust
polymarket_client_sdk_v2-0.5.1/src/clob/client.rs
379:    /// Default builder code inherited by orders built via [`Client::limit_order`] or
380:    /// [`Client::market_order`] when not set on the order itself.
381:    builder_code: Option<B256>,
```

```rust
polymarket_client_sdk_v2-0.5.1/src/clob/client.rs
2806:            post_only: Some(false),
2807:            metadata: None,
2808:            builder_code: self.inner.config.builder_code,
2809:            defer_exec: None,
```

## Recommended Fix Order

1. Track `Cargo.lock` and enforce `--locked` in CI and audit.
2. Fix SessionBand arbiter priority and budget env wiring.
3. Replace V1 exchange address reads with V2 exchange fallback in sell approval checks.
4. Align `alpha_service` default CLOB URL with V2 runtime default.
5. Harden `manual_bot` auth for non-loopback binds.
6. Rework `place_market_order` to reuse cached auth and expose an optional price cap.
7. Clean or explicitly allow clippy lints under Rust `1.91.0`.
8. Add SDK V2 live-smoke/integration coverage for order post, approval, relayer submit, redeem, and merge.

