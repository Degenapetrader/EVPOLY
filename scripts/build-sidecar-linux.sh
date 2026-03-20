#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOCK_PATH="${REPO_ROOT}/src-tauri/sidecar-core.lock"
CORE_REF="${CORE_REF:-}"
CORE_REPO="${CORE_REPO:-}"
FORCE=0
CARGO_BIN="${HOME}/.cargo/bin"

if [[ -d "${CARGO_BIN}" && ":${PATH}:" != *":${CARGO_BIN}:"* ]]; then
  export PATH="${CARGO_BIN}:${PATH}"
fi

for arg in "$@"; do
  if [[ "${arg}" == "--force" ]]; then
    FORCE=1
  fi
done

if [[ -f "${LOCK_PATH}" ]]; then
  while IFS='=' read -r key value; do
    [[ -z "${key}" ]] && continue
    [[ "${key}" =~ ^[[:space:]]*# ]] && continue
    key="$(echo "${key}" | xargs)"
    value="$(echo "${value}" | xargs)"
    if [[ -z "${CORE_REF}" && "${key}" == "CORE_REF" ]]; then
      CORE_REF="${value}"
    fi
    if [[ -z "${CORE_REPO}" && "${key}" == "CORE_REPO" ]]; then
      CORE_REPO="${value}"
    fi
  done < "${LOCK_PATH}"
fi

CORE_REF="${CORE_REF:-main}"
CORE_REPO="${CORE_REPO:-https://github.com/Degenapetrader/EVPOLY.git}"
TARGET_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
if [[ -z "${TARGET_TRIPLE}" ]]; then
  echo "[build-sidecar-linux] unable to determine Rust host target triple" >&2
  exit 1
fi

BINARIES_DIR="${REPO_ROOT}/src-tauri/binaries"
TARGET_DIR="${REPO_ROOT}/src-tauri/target/core-sidecars"
CORE_CONTRACT_DIR="${REPO_ROOT}/src-tauri/core-contract"
STAMP_PATH="${TARGET_DIR}/sidecar-build-stamp-${TARGET_TRIPLE}.lock"
BOT_OUTPUT="${BINARIES_DIR}/evpoly-bot-${TARGET_TRIPLE}"
MANUAL_OUTPUT="${BINARIES_DIR}/evpoly-manual-bot-${TARGET_TRIPLE}"

if [[ "${FORCE}" -eq 0 && -f "${BOT_OUTPUT}" && -f "${MANUAL_OUTPUT}" && -f "${STAMP_PATH}" ]]; then
  stamp_core_ref="$(sed -n 's/^CORE_REF=//p' "${STAMP_PATH}" | head -n 1)"
  stamp_target="$(sed -n 's/^TARGET_TRIPLE=//p' "${STAMP_PATH}" | head -n 1)"
  if [[ "${stamp_core_ref}" == "${CORE_REF}" && "${stamp_target}" == "${TARGET_TRIPLE}" ]]; then
    echo "[build-sidecar-linux] sidecars already prepared core_ref=${CORE_REF} target=${TARGET_TRIPLE}"
    exit 0
  fi
fi

WORK_ROOT="${RUNNER_TEMP:-/tmp}"
WORK_DIR="$(mktemp -d "${WORK_ROOT%/}/evpoly-core-linux-XXXXXX")"
SOURCE_MODE="remote-clone"
USE_LOCAL_WORKTREE=0

cleanup() {
  if [[ -d "${WORK_DIR}" ]]; then
    if [[ "${USE_LOCAL_WORKTREE}" -eq 1 ]]; then
      git worktree remove --force "${WORK_DIR}" >/dev/null 2>&1 || true
    else
      rm -rf "${WORK_DIR}"
    fi
  fi
}
trap cleanup EXIT

if git rev-parse --verify "${CORE_REF}^{commit}" >/dev/null 2>&1; then
  echo "[build-sidecar-linux] creating local worktree ref=${CORE_REF}"
  git worktree add --detach "${WORK_DIR}" "${CORE_REF}"
  SOURCE_MODE="local-worktree"
  USE_LOCAL_WORKTREE=1
else
  echo "[build-sidecar-linux] cloning ${CORE_REPO} ref=${CORE_REF}"
  git clone --filter=blob:none "${CORE_REPO}" "${WORK_DIR}"
  git -C "${WORK_DIR}" checkout --detach "${CORE_REF}"
fi

echo "[build-sidecar-linux] building sidecar binaries ref=${CORE_REF} target=${TARGET_TRIPLE}"
cargo build --release --manifest-path "${WORK_DIR}/Cargo.toml" --target-dir "${TARGET_DIR}" --target "${TARGET_TRIPLE}" --bin polymarket-arbitrage-bot --bin manual_bot

mkdir -p "${BINARIES_DIR}" "${TARGET_DIR}" "${CORE_CONTRACT_DIR}"
cp "${TARGET_DIR}/${TARGET_TRIPLE}/release/polymarket-arbitrage-bot" "${BOT_OUTPUT}"
cp "${TARGET_DIR}/${TARGET_TRIPLE}/release/manual_bot" "${MANUAL_OUTPUT}"
cp "${WORK_DIR}/.env.example" "${CORE_CONTRACT_DIR}/.env.example"
if ! grep -q '^EVPOLY_MM_MARKET_MODE=' "${CORE_CONTRACT_DIR}/.env.example"; then
  cat >> "${CORE_CONTRACT_DIR}/.env.example" <<'EOF'

# MM rewards market selection mode (`auto` = local rewards discovery only;
# `hybrid` also honors single-market selectors when present)
EVPOLY_MM_MARKET_MODE=auto
EOF
fi
chmod +x "${BOT_OUTPUT}" "${MANUAL_OUTPUT}"

cat > "${STAMP_PATH}" <<EOF
CORE_REF=${CORE_REF}
CORE_REPO=${CORE_REPO}
TARGET_TRIPLE=${TARGET_TRIPLE}
SOURCE_MODE=${SOURCE_MODE}
PREPARED_AT_UTC=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
EOF

echo "[build-sidecar-linux] done"
