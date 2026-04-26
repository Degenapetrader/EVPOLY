#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

status_ok=true

check_cmd() {
  local c="$1"
  if command -v "$c" >/dev/null 2>&1; then
    echo "[OK] command: $c"
  else
    echo "[ERR] missing command: $c"
    status_ok=false
  fi
}

check_env_key() {
  local key="$1"
  if [[ -f .env ]] && rg -n "^${key}=.+" .env >/dev/null 2>&1; then
    echo "[OK] .env key: $key"
  else
    echo "[WARN] .env key missing/blank: $key"
  fi
}

check_cmd cargo
check_cmd tmux
check_cmd rg

if [[ -f .env ]]; then
  echo "[OK] .env exists"
else
  echo "[ERR] .env not found (copy from .env.example)"
  status_ok=false
fi

check_env_key POLY_PRIVATE_KEY
check_env_key POLY_SIGNATURE_TYPE
check_env_key EVPOLY_RELAYER_REMOTE_SIGNER_TOKEN

bash scripts/verify_env_coverage.sh --env-file .env || true

if [[ "$status_ok" == true ]]; then
  echo "Doctor: PASS"
  exit 0
fi

echo "Doctor: FAIL"
exit 1
