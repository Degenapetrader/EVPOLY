#!/usr/bin/env bash
set -euo pipefail

BUNDLE_DIR="${1:-src-tauri/target/release/bundle/deb}"

if [[ ! -d "${BUNDLE_DIR}" ]]; then
  echo "error: deb bundle directory not found: ${BUNDLE_DIR}" >&2
  exit 1
fi

DEB_PATH="$(find "${BUNDLE_DIR}" -maxdepth 1 -type f -name '*.deb' | sort | tail -n 1)"
if [[ -z "${DEB_PATH}" ]]; then
  echo "error: no .deb artifact found in ${BUNDLE_DIR}" >&2
  exit 1
fi

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "${WORK_DIR}"' EXIT

dpkg-deb -I "${DEB_PATH}" >/dev/null
dpkg-deb -x "${DEB_PATH}" "${WORK_DIR}/root"

DESKTOP_FILE="$(find "${WORK_DIR}/root/usr/share/applications" -maxdepth 1 -type f -name '*.desktop' | head -n 1)"
if [[ -z "${DESKTOP_FILE}" ]]; then
  echo "error: packaged .deb is missing a desktop entry" >&2
  exit 1
fi

EXEC_LINE="$(sed -n 's/^Exec=//p' "${DESKTOP_FILE}" | head -n 1)"
if [[ -z "${EXEC_LINE}" ]]; then
  echo "error: desktop entry is missing an Exec line" >&2
  exit 1
fi

EXEC_BIN="${EXEC_LINE%% *}"
if [[ "${EXEC_BIN}" = /* ]]; then
  EXEC_PATH="${WORK_DIR}/root${EXEC_BIN}"
else
  EXEC_PATH="$(find "${WORK_DIR}/root" -type f -name "${EXEC_BIN}" | head -n 1)"
fi

if [[ -z "${EXEC_PATH}" || ! -x "${EXEC_PATH}" ]]; then
  echo "error: packaged .deb is missing the desktop executable" >&2
  exit 1
fi

SIDECAR_PATH="$(find "${WORK_DIR}/root" -type f \( -name 'evpoly-bot*' -o -name 'polymarket-arbitrage-bot*' \) | head -n 1)"
if [[ -z "${SIDECAR_PATH}" ]]; then
  echo "error: packaged .deb is missing the bundled bot sidecar" >&2
  exit 1
fi

DOC_PATH="${WORK_DIR}/root/usr/share/doc/evpoly/linux-install.md"
if [[ ! -f "${DOC_PATH}" ]]; then
  echo "error: packaged .deb is missing the Linux install guide" >&2
  exit 1
fi

echo "installer smoke ok: ${DEB_PATH}"
