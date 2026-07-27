# EVPOLY Agent Notes

## Mandatory Update Rule

- If any strategy logic, checkpoint schedule, entry/exit behavior, risk gate, sizing, or strategy env defaults are changed, update `strategy-changelog.md` in the same task.
- Add a dated entry under `## Change Log` describing:
  - what changed,
  - which strategy/timeframe/symbol it affects,
  - key env or code path touched.

## Scope

Strategies include (minimum):
- `premarket_v1`
- `endgame_sweep_v1`
- `evcurve_v1`
- `sessionband_v1`
- `evsnipe_v1`
- `mm_sport_v1`

## Details: Low-Level AI Operating Guide

### 1) Mission
- Ship safe, minimal diffs.
- Preserve runtime behavior unless explicitly requested to change.
- Keep docs and env examples aligned with runtime.

### 2) Repo Map
- Main runtime: `src/main.rs`
- Premarket scheduler: `src/strategy_decider.rs`
- Strategy configs:
  - `src/config.rs` (endgame/global)
  - `src/evcurve.rs`
  - `src/sessionband.rs`
  - `src/evsnipe.rs`
  - `src/mm/mod.rs`
- Shared sizing policy: `src/size_policy.rs`
- Manual HTTP API: `src/bin/manual_bot.rs`
- Alpha service binary: `src/bin/alpha_service.rs`
- Env templates: `.env.example`, `.env.full.example`
- Strategy behavior history: `strategy-changelog.md`

### 3) Current Runtime Defaults
- Strategy toggles:
  - ON: premarket, endgame, evcurve, sessionband, evsnipe
  - OFF: mm_sport
- Default symbols:
  - premarket: `BTC, ETH, SOL, XRP`
  - evcurve/sessionband: `BTC, ETH, SOL, XRP`
  - endgame/evsnipe: `BTC, ETH, SOL, XRP, DOGE, BNB, HYPE`

### 4) Known Combo Constraints
- `endgame_sweep_v1` and `sessionband_v1` are both late-window close strategies; review sizing before running both heavy.
- `mm_sport_v1` should not be combined with heavy directional profiles until risk sizing is reviewed.
- See `docs/strategy_combos.md`.

### Runtime Control Rule
- Use `./ev` for runtime lifecycle (`start/restart/stop/status/logs/autorestart`) and do not run the live main bot using direct `cargo run`.

### 5) Remote Dependency Facts
- Shared timeframe discovery: remote-first with local fallback.
- EVSnipe discovery: remote-first (alpha host failover) with local fallback.
- Premarket alpha:
  - remote ladder selection at scheduled pre-open intent time,
  - local bot sends base ladder prices and alpha returns an aligned shifted ladder,
  - unavailable/invalid alpha ladder fail-closed skips that asset intent.
- Endgame/EVcurve alpha:
  - remote decision path,
  - unavailable/invalid responses skip decision (strategy-specific skip behavior).
- SessionBand alpha:
  - remote decision path at tau checkpoints,
  - local runtime owns market discovery, base anchoring, sizing, caps, and order placement,
  - unavailable/invalid responses skip decision.
- MM Sport:
  - remote alpha owns low-depth skip decisions,
  - local runtime owns quote placement and inventory handling.
- Setup Doctor:
  - command: `python3 scripts/setup_doctor.py --env-file .env`
  - checks wallet fields, alpha self-onboarding posture, and relayer manual fields
  - treats blank per-strategy remote tokens as acceptable when `EVPOLY_ALPHA_KEY` is present or `EVPOLY_ALPHA_AUTO_ONBOARD=true`
  - reports relayer fields as manual `needs_you` items and does not fake-generate them
  - is advisory only and should not be treated as a runtime gate

### 6) Verification by Scope and Risk
1. Run `git status --short`.
2. Inspect target code paths with `rg` and focused reads.
3. Implement only requested changes.
4. If strategy behavior/defaults changed, update `strategy-changelog.md`.
5. Run the lightest credible checks for the changed surface:
   - Docs-only: no Rust build or test is required.
   - Isolated Rust change: `cargo fmt --all -- --check`, the narrowest relevant test target or
     filter, and `cargo check --all-targets`.
   - Strategy, shared runtime, or multi-module change: formatting, all-target check, and the
     affected subsystem tests.
   - Broad refactor, release candidate, dependency/lockfile change, or changes to authentication,
     signing, custody, deposits, credits, trading execution/risk, permissions, or destructive
     operations: the full CI-equivalent suite below.
6. Full CI-equivalent suite, only when required by the previous rule:
   - `cargo fmt --all -- --check`
   - `cargo check --locked --all-targets`
   - `cargo test --locked --all-targets --quiet`
   - `./scripts/security_audit.sh`
7. A timeout is inconclusive, not a failure. Diagnose and rerun the failing target; do not repeatedly
   restart the whole suite. After a fix, rerun the focused check and one required broad gate.
8. Re-check `git status --short` and diff.
9. Commit with clear scope message.

### 7) Editing Rules
- Do not revert unrelated user changes.
- Do not use destructive git commands.
- Keep comments concise and useful.
- Prefer consistency with existing architecture.

### 8) Manual Endpoint Facts
- Binary: `manual_bot`
- Auth headers:
  - `x-evpoly-manual-token`
  - `x-evpoly-admin-token`
- Open/close semantics:
  - `/manual/open` is BUY path
  - `/manual/close` is reduce-only SELL path
  - `tp_price` allowed for open only
- Full route guide: `docs/manual_endpoint_guide.md`

### 9) Definition of Done
- Requested changes implemented.
- Verification appropriate to the changed scope and risk passes; full-suite evidence is required
  only for the broad/high-risk cases above.
- Docs updated when behavior/surface changed.
- Commit and push completed.
