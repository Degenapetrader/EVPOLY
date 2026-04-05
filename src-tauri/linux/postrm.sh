#!/bin/sh
set -eu

XRDP_STARTWM="/etc/xrdp/startwm.sh"
XRDP_STARTWM_BACKUP="/etc/xrdp/startwm.sh.evpoly.bak"
XRDP_INI="/etc/xrdp/xrdp.ini"
XRDP_INI_BACKUP="/etc/xrdp/xrdp.ini.evpoly.bak"
XRDP_MARKER="# managed-by-evpoly"

log() {
  echo "[evpoly postrm] $*"
}

command_exists() {
  command -v "$1" >/dev/null 2>&1
}

systemd_available() {
  command_exists systemctl && [ -d /run/systemd/system ]
}

restore_xrdp_session() {
  if [ -f "$XRDP_STARTWM_BACKUP" ]; then
    mv -f "$XRDP_STARTWM_BACKUP" "$XRDP_STARTWM"
    chmod 0755 "$XRDP_STARTWM" || true
    return 0
  fi

  if [ -f "$XRDP_STARTWM" ] && grep -q "$XRDP_MARKER" "$XRDP_STARTWM"; then
    rm -f "$XRDP_STARTWM"
  fi
}

restore_xrdp_ini() {
  if [ -f "$XRDP_INI_BACKUP" ]; then
    mv -f "$XRDP_INI_BACKUP" "$XRDP_INI"
  fi
}

case "${1:-}" in
  remove|purge)
    if systemd_available; then
      log "stopping xrdp services"
      systemctl stop xrdp >/dev/null 2>&1 || true
      systemctl stop xrdp-sesman >/dev/null 2>&1 || true
      systemctl disable xrdp >/dev/null 2>&1 || true
      systemctl disable xrdp-sesman >/dev/null 2>&1 || true
    fi
    restore_xrdp_session
    restore_xrdp_ini
    ;;
  upgrade|failed-upgrade|abort-install|abort-upgrade|disappear)
    ;;
  *)
    log "no post-remove action for argument: ${1:-}"
    ;;
esac

exit 0
