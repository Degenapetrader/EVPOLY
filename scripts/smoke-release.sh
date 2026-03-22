#!/usr/bin/env bash
set -euo pipefail

BOT_BIN="${1:-src-tauri/binaries/evpoly-bot-x86_64-unknown-linux-gnu}"
TIMEOUT_SECONDS="${2:-15}"

if [[ ! -x "$BOT_BIN" ]]; then
  echo "error: bot binary not found or not executable: $BOT_BIN" >&2
  exit 1
fi

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

export POLY_PRIVATE_KEY="${POLY_PRIVATE_KEY:-0x1111111111111111111111111111111111111111111111111111111111111111}"
export POLY_PROXY_WALLET_ADDRESS="${POLY_PROXY_WALLET_ADDRESS:-0x1111111111111111111111111111111111111111}"
export POLY_SIGNATURE_TYPE="${POLY_SIGNATURE_TYPE:-1}"

FAIL_PATTERN="missing field 'check_interval_ms'|unexpected argument '--env-file'|unexpected argument '--no-simulation'|error: unexpected argument|command create_profile missing required key walletAddress|thread '.*' panicked|panicked at"

run_mode() {
  local mode="$1"
  local flag="$2"
  local cfg="$WORK_DIR/runtime-${mode}.config.json"
  local out="$WORK_DIR/bot-${mode}.log"

  rm -f "$cfg" "$out"

  (
    set +e
    timeout "${TIMEOUT_SECONDS}s" "$BOT_BIN" --config "$cfg" "$flag" >"$out" 2>&1
    exit 0
  )

  if [[ ! -f "$cfg" ]]; then
    echo "error: expected config file was not created for mode=$mode: $cfg" >&2
    sed -n '1,120p' "$out" >&2 || true
    exit 1
  fi

  if grep -Ein "$FAIL_PATTERN" "$out" >/dev/null 2>&1; then
    echo "error: smoke log contains blocked patterns for mode=$mode" >&2
    grep -Ein "$FAIL_PATTERN" "$out" >&2 || true
    sed -n '1,200p' "$out" >&2 || true
    exit 1
  fi

  echo "smoke ok: mode=$mode"
}

run_mode "simulation" "--simulation"
run_mode "live" "--no-simulation"

echo "all smoke checks passed"
