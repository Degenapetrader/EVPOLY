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
CORE_PATCH_DIR="${REPO_ROOT}/src-tauri/core-patches"
STAMP_PATH="${TARGET_DIR}/sidecar-build-stamp-${TARGET_TRIPLE}.lock"
BOT_OUTPUT="${BINARIES_DIR}/evpoly-bot-${TARGET_TRIPLE}"
PATCH_HASH="none"

if [[ -d "${CORE_PATCH_DIR}" ]]; then
  if compgen -G "${CORE_PATCH_DIR}"'/*.patch' > /dev/null; then
    PATCH_HASH="$(
      {
        while IFS= read -r patch; do
          printf '%s\n' "$(basename "${patch}")"
          cat "${patch}"
        done < <(find "${CORE_PATCH_DIR}" -maxdepth 1 -type f -name '*.patch' | sort)
      } | sha256sum | awk '{print $1}'
    )"
  fi
fi

if [[ "${FORCE}" -eq 0 && -f "${BOT_OUTPUT}" && -f "${STAMP_PATH}" ]]; then
  stamp_core_ref="$(sed -n 's/^CORE_REF=//p' "${STAMP_PATH}" | head -n 1)"
  stamp_target="$(sed -n 's/^TARGET_TRIPLE=//p' "${STAMP_PATH}" | head -n 1)"
  stamp_patch_hash="$(sed -n 's/^PATCHES_SHA256=//p' "${STAMP_PATH}" | head -n 1)"
  if [[ "${stamp_core_ref}" == "${CORE_REF}" && "${stamp_target}" == "${TARGET_TRIPLE}" && "${stamp_patch_hash}" == "${PATCH_HASH}" ]]; then
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

resolve_remote_core_ref() {
  local repo="$1"
  local ref="$2"

  if [[ -z "${ref}" ]]; then
    return 1
  fi

  if [[ ! "${ref}" =~ ^[0-9a-fA-F]{7,40}$ ]]; then
    printf '%s\n' "${ref}"
    return 0
  fi

  local normalized_ref
  normalized_ref="$(echo "${ref}" | tr '[:upper:]' '[:lower:]')"
  while IFS=$'\t' read -r sha remote_ref; do
    sha="$(echo "${sha}" | tr '[:upper:]' '[:lower:]')"
    if [[ "${sha}" == "${normalized_ref}"* ]]; then
      printf '%s\n' "${remote_ref}"
      return 0
    fi
  done < <(git ls-remote --heads --tags "${repo}" 2>/dev/null)

  return 1
}

ensure_local_core_ref() {
  if git rev-parse --verify "${CORE_REF}^{commit}" >/dev/null 2>&1; then
    return 0
  fi

  if git remote get-url origin >/dev/null 2>&1; then
    remote_fetch_ref="$(resolve_remote_core_ref origin "${CORE_REF}" || true)"
    if [[ -n "${remote_fetch_ref}" ]]; then
      echo "[build-sidecar-linux] fetching pinned core ref from origin via ${remote_fetch_ref}"
      git fetch --depth=1 origin "${remote_fetch_ref}" >/dev/null 2>&1 || true
    fi
  fi

  git rev-parse --verify "${CORE_REF}^{commit}" >/dev/null 2>&1
}

if ensure_local_core_ref; then
  echo "[build-sidecar-linux] creating local worktree ref=${CORE_REF}"
  git worktree add --detach "${WORK_DIR}" "${CORE_REF}"
  SOURCE_MODE="local-worktree"
  USE_LOCAL_WORKTREE=1
else
  echo "[build-sidecar-linux] cloning ${CORE_REPO} ref=${CORE_REF}"
  git clone --filter=blob:none "${CORE_REPO}" "${WORK_DIR}"
  if ! git -C "${WORK_DIR}" checkout --detach "${CORE_REF}"; then
    remote_fetch_ref="$(resolve_remote_core_ref "${CORE_REPO}" "${CORE_REF}" || true)"
    if [[ -z "${remote_fetch_ref}" ]]; then
      echo "[build-sidecar-linux] unable to resolve remote ref for pinned core ref ${CORE_REF}" >&2
      exit 1
    fi
    echo "[build-sidecar-linux] direct checkout failed; fetching ${remote_fetch_ref} and retrying"
    git -C "${WORK_DIR}" fetch --depth=1 origin "${remote_fetch_ref}"
    git -C "${WORK_DIR}" checkout --detach "${CORE_REF}"
  fi
fi

if [[ -d "${CORE_PATCH_DIR}" ]]; then
  while IFS= read -r patch; do
    echo "[build-sidecar-linux] applying core patch $(basename "${patch}")"
    git -C "${WORK_DIR}" apply --whitespace=nowarn "${patch}"
  done < <(find "${CORE_PATCH_DIR}" -maxdepth 1 -type f -name '*.patch' | sort)
fi

echo "[build-sidecar-linux] building sidecar binaries ref=${CORE_REF} target=${TARGET_TRIPLE}"
cargo build --release --manifest-path "${WORK_DIR}/Cargo.toml" --target-dir "${TARGET_DIR}" --target "${TARGET_TRIPLE}" --bin polymarket-arbitrage-bot

mkdir -p "${BINARIES_DIR}" "${TARGET_DIR}" "${CORE_CONTRACT_DIR}"
cp "${TARGET_DIR}/${TARGET_TRIPLE}/release/polymarket-arbitrage-bot" "${BOT_OUTPUT}"
# Desktop owns its runtime env template in this branch. We sync the core bot
# code from the pinned ref, but keep desktop defaults versioned locally.
chmod +x "${BOT_OUTPUT}"

cat > "${STAMP_PATH}" <<EOF
CORE_REF=${CORE_REF}
CORE_REPO=${CORE_REPO}
TARGET_TRIPLE=${TARGET_TRIPLE}
SOURCE_MODE=${SOURCE_MODE}
PATCHES_SHA256=${PATCH_HASH}
PREPARED_AT_UTC=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
EOF

echo "[build-sidecar-linux] done"
