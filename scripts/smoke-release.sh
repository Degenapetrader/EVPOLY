#!/usr/bin/env bash
set -euo pipefail

BOT_BIN="${1:-src-tauri/binaries/evpoly-bot-x86_64-unknown-linux-gnu}"
TIMEOUT_SECONDS="${2:-15}"
MANUAL_BOT_BIN="${3:-src-tauri/binaries/evpoly-manual-bot-x86_64-unknown-linux-gnu}"

if [[ ! -x "$BOT_BIN" ]]; then
  echo "error: bot binary not found or not executable: $BOT_BIN" >&2
  exit 1
fi
if [[ ! -x "$MANUAL_BOT_BIN" ]]; then
  echo "error: manual bot binary not found or not executable: $MANUAL_BOT_BIN" >&2
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

run_manual_service_smoke() {
  local cfg="$1"
  local out="$WORK_DIR/manual.stdout.log"
  local err="$WORK_DIR/manual.stderr.log"
  local log="$WORK_DIR/manual.log"
  local port=$((20000 + (RANDOM % 20000)))
  local token="smoke-$(date +%s)-$RANDOM"

  rm -f "$out" "$err" "$log"
  EVPOLY_MANUAL_BOT_TOKEN="$token" \
    "$MANUAL_BOT_BIN" --config "$cfg" --bind 127.0.0.1 --port "$port" --simulation >"$out" 2>"$err" &
  local pid=$!

  local ready=0
  for _ in $(seq 1 40); do
    local resp
    resp="$(curl -fsS -H "x-evpoly-manual-token: $token" "http://127.0.0.1:${port}/manual/health" 2>/dev/null || true)"
    if echo "$resp" | grep -E '"ok"[[:space:]]*:[[:space:]]*true' >/dev/null 2>&1; then
      ready=1
      break
    fi
    sleep 0.5
  done

  if kill -0 "$pid" >/dev/null 2>&1; then
    kill "$pid" >/dev/null 2>&1 || true
    wait "$pid" 2>/dev/null || true
  fi

  cat "$out" "$err" >"$log" 2>/dev/null || true

  if [[ "$ready" -ne 1 ]]; then
    echo "error: manual service health check failed" >&2
    sed -n '1,200p' "$log" >&2 || true
    exit 1
  fi
  if grep -Ein "$FAIL_PATTERN" "$log" >/dev/null 2>&1; then
    echo "error: manual service log contains blocked patterns" >&2
    grep -Ein "$FAIL_PATTERN" "$log" >&2 || true
    sed -n '1,200p' "$log" >&2 || true
    exit 1
  fi

  echo "smoke ok: manual service"
}

run_mode "simulation" "--simulation"
run_mode "live" "--no-simulation"
run_manual_service_smoke "$WORK_DIR/runtime-simulation.config.json"

echo "all smoke checks passed"
