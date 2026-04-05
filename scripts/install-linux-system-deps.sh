#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "error: install-linux-system-deps.sh only supports Linux hosts" >&2
  exit 1
fi

SUDO=""
if [[ "${EUID}" -ne 0 ]]; then
  if command -v sudo >/dev/null 2>&1; then
    SUDO="sudo"
  else
    echo "error: root or sudo is required to install Linux build dependencies" >&2
    exit 1
  fi
fi

PACKAGES=(
  build-essential
  curl
  file
  libayatana-appindicator3-dev
  libssl-dev
  librsvg2-dev
  libwebkit2gtk-4.1-dev
  libxdo-dev
  patchelf
  pkg-config
  wget
)

echo "[install-linux-system-deps] apt-get update"
${SUDO} apt-get update

echo "[install-linux-system-deps] apt-get install ${PACKAGES[*]}"
DEBIAN_FRONTEND=noninteractive ${SUDO} apt-get install -y "${PACKAGES[@]}"

echo "[install-linux-system-deps] done"
