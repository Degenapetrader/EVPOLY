#!/bin/sh
set -eu

XRDP_STARTWM="/etc/xrdp/startwm.sh"
XRDP_STARTWM_BACKUP="/etc/xrdp/startwm.sh.evpoly.bak"
XRDP_INI="/etc/xrdp/xrdp.ini"
XRDP_INI_BACKUP="/etc/xrdp/xrdp.ini.evpoly.bak"
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

backup_once() {
  source_path="$1"
  backup_path="$2"
  if [ -f "$source_path" ] && [ ! -f "$backup_path" ]; then
    cp -f "$source_path" "$backup_path"
  fi
}

configure_xrdp_ini() {
  backup_once "$XRDP_INI" "$XRDP_INI_BACKUP"

  if [ -f "$XRDP_INI" ]; then
    sed -i 's/^autorun=.*/autorun=Xorg/' "$XRDP_INI"
  fi
}

configure_xrdp_session() {
  backup_once "$XRDP_STARTWM" "$XRDP_STARTWM_BACKUP"

  cat > "$XRDP_STARTWM" <<'EOF'
#!/bin/sh
# managed-by-evpoly
if [ -r /etc/profile ]; then
  . /etc/profile
fi
if [ -r "$HOME/.profile" ]; then
  . "$HOME/.profile"
fi

set_rdp_scaling() {
  if ! command -v xrandr >/dev/null 2>&1; then
    return 0
  fi

  MODE="$(xrandr --current 2>/dev/null | awk '/\*/ { print $1; exit }')"
  WIDTH="${MODE%x*}"
  HEIGHT="${MODE#*x}"

  case "${WIDTH:-}" in
    ''|*[!0-9]*) return 0 ;;
  esac
  case "${HEIGHT:-}" in
    ''|*[!0-9]*) return 0 ;;
  esac

  if [ "$WIDTH" -ge 3000 ] || [ "$HEIGHT" -ge 1800 ]; then
    export GDK_SCALE=2
    export QT_AUTO_SCREEN_SCALE_FACTOR=0
    export QT_SCALE_FACTOR=2
    export XCURSOR_SIZE=48
  elif [ "$WIDTH" -ge 2200 ] || [ "$HEIGHT" -ge 1400 ]; then
    export GDK_SCALE=2
    export QT_AUTO_SCREEN_SCALE_FACTOR=0
    export QT_SCALE_FACTOR=1.5
    export XCURSOR_SIZE=36
  fi
}

set_rdp_scaling
unset SESSION_MANAGER
unset DBUS_SESSION_BUS_ADDRESS
unset DESKTOP_SESSION
export XDG_CURRENT_DESKTOP=XFCE
export XDG_SESSION_DESKTOP=xfce
export XDG_SESSION_TYPE=x11

if command -v dbus-run-session >/dev/null 2>&1; then
  exec dbus-run-session -- startxfce4
fi

if command -v dbus-launch >/dev/null 2>&1; then
  exec dbus-launch --exit-with-session startxfce4
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
      if ! ufw status 2>/dev/null | grep -Eq '(^22/tcp|^OpenSSH)'; then
        ufw allow OpenSSH >/dev/null 2>&1 || true
      fi
      if ! ufw status 2>/dev/null | grep -q '^3389/tcp'; then
        ufw allow 3389/tcp >/dev/null 2>&1 || true
      fi
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

configure_xrdp_tls() {
  if command -v getent >/dev/null 2>&1 && getent group ssl-cert >/dev/null 2>&1 && id xrdp >/dev/null 2>&1; then
    log "granting xrdp access to TLS private key"
    usermod -a -G ssl-cert xrdp >/dev/null 2>&1 || true
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
    configure_xrdp_ini
    configure_xrdp_session
    configure_xsession_defaults
    configure_xrdp_tls
    configure_firewall
    enable_xrdp_services
    log "EVPoly Remote Desktop is ready on TCP 3389"
    ;;
  *)
    log "no post-install action for argument: ${1:-}"
    ;;
esac

exit 0
