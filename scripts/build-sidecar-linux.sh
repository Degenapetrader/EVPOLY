#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CORE_DIR="${REPO_ROOT}/core"
MANIFEST_PATH="${CORE_DIR}/Cargo.toml"
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

if [[ ! -f "${MANIFEST_PATH}" ]]; then
  echo "[build-sidecar-linux] desktop-owned core source is missing: ${MANIFEST_PATH}" >&2
  exit 1
fi
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

latest_source_epoch() {
  find "${CORE_DIR}" -type f ! -path "*/target/*" ! -path "*/.git/*" -printf '%T@\n' | sort -nr | head -n 1
}

if [[ "${FORCE}" -eq 0 && -f "${BOT_OUTPUT}" && -f "${MANUAL_OUTPUT}" && -f "${STAMP_PATH}" ]]; then
  stamp_source="$(sed -n 's/^CORE_SOURCE=//p' "${STAMP_PATH}" | head -n 1)"
  stamp_target="$(sed -n 's/^TARGET_TRIPLE=//p' "${STAMP_PATH}" | head -n 1)"
  latest_source="$(latest_source_epoch)"
  stamp_epoch="$(stat -c '%Y' "${STAMP_PATH}")"
  latest_source_int="${latest_source%.*}"
  if [[ "${stamp_source}" == "core" && "${stamp_target}" == "${TARGET_TRIPLE}" && "${stamp_epoch}" -ge "${latest_source_int:-0}" ]]; then
    echo "[build-sidecar-linux] sidecars already prepared source=core target=${TARGET_TRIPLE}"
    exit 0
  fi
fi

echo "[build-sidecar-linux] building sidecar binaries source=core target=${TARGET_TRIPLE}"
cargo build --release --manifest-path "${MANIFEST_PATH}" --target-dir "${TARGET_DIR}" --target "${TARGET_TRIPLE}" --bin polymarket-arbitrage-bot --bin manual_bot

mkdir -p "${BINARIES_DIR}" "${TARGET_DIR}" "${CORE_CONTRACT_DIR}"
cp "${TARGET_DIR}/${TARGET_TRIPLE}/release/polymarket-arbitrage-bot" "${BOT_OUTPUT}"
cp "${TARGET_DIR}/${TARGET_TRIPLE}/release/manual_bot" "${MANUAL_OUTPUT}"
# Desktop owns its runtime env template in this branch. We sync the core bot
# code from the pinned ref, but keep desktop defaults versioned locally.
chmod +x "${BOT_OUTPUT}" "${MANUAL_OUTPUT}"

cat > "${STAMP_PATH}" <<EOF
CORE_SOURCE=core
TARGET_TRIPLE=${TARGET_TRIPLE}
SOURCE_MODE=desktop-local
PREPARED_AT_UTC=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
EOF

echo "[build-sidecar-linux] done"
