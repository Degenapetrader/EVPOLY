#!/usr/bin/env bash
set -euo pipefail

CORE_REF="${CORE_REF:-main}"
CORE_REPO="${CORE_REPO:-https://github.com/Degenapetrader/EVPOLY.git}"
WORK_ROOT="${RUNNER_TEMP:-/tmp}"
WORK_DIR="$(mktemp -d "${WORK_ROOT%/}/evpoly-core-linux-XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT

echo "[build-sidecar-linux] cloning ${CORE_REPO} ref=${CORE_REF}"
git clone --depth 1 --branch "${CORE_REF}" "${CORE_REPO}" "${WORK_DIR}"

echo "[build-sidecar-linux] building sidecar binaries"
cargo build --release --manifest-path "${WORK_DIR}/Cargo.toml" --bin polymarket-arbitrage-bot
cargo build --release --manifest-path "${WORK_DIR}/Cargo.toml" --bin manual_bot

mkdir -p src-tauri/binaries
cp "${WORK_DIR}/target/release/polymarket-arbitrage-bot" "src-tauri/binaries/evpoly-bot-x86_64-unknown-linux-gnu"
cp "${WORK_DIR}/target/release/manual_bot" "src-tauri/binaries/evpoly-manual-bot-x86_64-unknown-linux-gnu"
chmod +x "src-tauri/binaries/evpoly-bot-x86_64-unknown-linux-gnu"
chmod +x "src-tauri/binaries/evpoly-manual-bot-x86_64-unknown-linux-gnu"

echo "[build-sidecar-linux] done"
