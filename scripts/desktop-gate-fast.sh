#!/usr/bin/env bash
set -euo pipefail

INSTALL_DEPS="${INSTALL_DEPS:-0}"
RUN_FRONTEND_BUILD="${RUN_FRONTEND_BUILD:-1}"
RUN_CARGO_CHECK="${RUN_CARGO_CHECK:-1}"
RUN_SMOKE="${RUN_SMOKE:-0}"

echo "[desktop-gate-fast] install_deps=${INSTALL_DEPS} frontend_build=${RUN_FRONTEND_BUILD} cargo_check=${RUN_CARGO_CHECK} smoke=${RUN_SMOKE}"

if [[ "${INSTALL_DEPS}" == "1" || ! -d "node_modules" ]]; then
  echo "[desktop-gate-fast] npm ci"
  npm ci
fi

echo "[desktop-gate-fast] npm test"
npm test

if [[ "${RUN_FRONTEND_BUILD}" == "1" ]]; then
  echo "[desktop-gate-fast] npm run build"
  npm run build
fi

if [[ "${RUN_CARGO_CHECK}" == "1" ]]; then
  echo "[desktop-gate-fast] cargo check"
  cargo check --manifest-path src-tauri/Cargo.toml
fi

if [[ "${RUN_SMOKE}" == "1" ]]; then
  echo "[desktop-gate-fast] smoke-release.sh"
  bash scripts/smoke-release.sh
fi

echo "[desktop-gate-fast] all checks passed"
