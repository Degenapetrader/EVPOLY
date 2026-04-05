#!/bin/sh
set -eu

XRDP_STARTWM="/etc/xrdp/startwm.sh"
XRDP_STARTWM_BACKUP="/etc/xrdp/startwm.sh.evpoly.bak"
XRDP_MARKER="# managed-by-evpoly"
SKEL_XSESSION="/etc/skel/.xsession"
XSESSION_CONTENT="${XRDP_MARKER}
startxfce4
"

log() {
  echo "[evpoly postinst] $*"
}

command_exists() {
  command -v "$1" >/dev/null 2>&1
}

systemd_available() {
  command_exists systemctl && [ -d /run/systemd/system ]
}

write_file() {
  target_path="$1"
  file_content="$2"
  printf "%s" "$file_content" > "$target_path"
}

configure_xrdp_session() {
  if [ -f "$XRDP_STARTWM" ] && ! grep -q "$XRDP_MARKER" "$XRDP_STARTWM"; then
    cp -f "$XRDP_STARTWM" "$XRDP_STARTWM_BACKUP"
  fi

  cat > "$XRDP_STARTWM" <<'EOF'
#!/bin/sh
# managed-by-evpoly
if [ -r /etc/profile ]; then
  . /etc/profile
fi
if [ -r "$HOME/.profile" ]; then
  . "$HOME/.profile"
fi
exec startxfce4
EOF
  chmod 0755 "$XRDP_STARTWM"
}

configure_user_xsession() {
  user_home="$1"
  user_uid="$2"
  user_gid="$3"

  [ -d "$user_home" ] || return 0

  write_file "$user_home/.xsession" "$XSESSION_CONTENT"
  chown "$user_uid:$user_gid" "$user_home/.xsession" || true
  chmod 0644 "$user_home/.xsession" || true
}

configure_xsession_defaults() {
  write_file "$SKEL_XSESSION" "$XSESSION_CONTENT"
  chmod 0644 "$SKEL_XSESSION"

  getent passwd | while IFS=: read -r _ _ user_uid user_gid _ user_home user_shell; do
    case "$user_home" in
      /home/*) ;;
      *) continue ;;
    esac
    case "$user_shell" in
      */false|*/nologin) continue ;;
    esac
    if [ "$user_uid" -lt 1000 ]; then
      continue
    fi
    configure_user_xsession "$user_home" "$user_uid" "$user_gid"
  done
}

configure_firewall() {
  if command_exists ufw; then
    if ufw status 2>/dev/null | grep -q '^Status: active'; then
      log "allowing OpenSSH and 3389/tcp in ufw"
      ufw allow OpenSSH >/dev/null 2>&1 || true
      ufw allow 3389/tcp >/dev/null 2>&1 || true
    else
      log "ufw present but inactive; leaving host firewall state unchanged"
    fi
  fi

  if command_exists firewall-cmd; then
    if firewall-cmd --state >/dev/null 2>&1; then
      log "allowing 3389/tcp in firewalld"
      firewall-cmd --permanent --add-port=3389/tcp >/dev/null 2>&1 || true
      firewall-cmd --reload >/dev/null 2>&1 || true
    fi
  fi
}

enable_xrdp_services() {
  if ! systemd_available; then
    log "systemd is unavailable in this environment; skipping xrdp auto-start"
    return 0
  fi

  log "enabling xrdp services"
  systemctl daemon-reload >/dev/null 2>&1 || true
  systemctl enable xrdp >/dev/null 2>&1 || true
  systemctl enable xrdp-sesman >/dev/null 2>&1 || true
  systemctl restart xrdp >/dev/null 2>&1 || true
  systemctl restart xrdp-sesman >/dev/null 2>&1 || true
}

case "${1:-configure}" in
  configure|abort-upgrade|abort-remove|abort-deconfigure)
    configure_xrdp_session
    configure_xsession_defaults
    configure_firewall
    enable_xrdp_services
    log "EVPoly Remote Desktop is ready on TCP 3389"
    ;;
  *)
    log "no post-install action for argument: ${1:-}"
    ;;
esac

exit 0
