# Contributing

## Scope
- Keep changes minimal and production-safe.
- Do not commit secrets or local runtime artifacts.
- Prefer small, reviewable PRs.

## Local Setup
1. `cp .env.example .env`
2. Fill required credentials in `.env`.
3. `cargo build --release`

## Required Checks (Before Push)
Scale checks to the change:

- Docs-only: no Rust build or test.
- Isolated Rust change: formatting, the narrowest relevant test target/filter, and
  `cargo check --all-targets`.
- Strategy, shared runtime, or multi-module change: formatting, all-target check, and affected
  subsystem tests.
- Broad refactor, release candidate, dependency/lockfile change, or security/financial execution
  boundary: `cargo fmt --all -- --check`, `cargo check --locked --all-targets`,
  `cargo test --locked --all-targets --quiet`, and `./scripts/security_audit.sh`.

A timeout is inconclusive. Diagnose the affected target and rerun it instead of repeatedly restarting
the entire suite.

## Strategy Surface Changes
If strategy logic, risk gates, entry/exit behavior, sizing, checkpoint schedule, or strategy defaults change, update `strategy-changelog.md` in the same PR.

## Docs Freshness Rule
If runtime behavior or env surface changes, update related docs in the same PR:
- `README.md`
- `docs/*.md`
- env template comments (`.env.example`, `.env.full.example`) when relevant.
