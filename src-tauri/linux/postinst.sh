#!/bin/sh
set -eu

XRDP_STARTWM="/etc/xrdp/startwm.sh"
XRDP_STARTWM_BACKUP="/etc/xrdp/startwm.sh.evpoly.bak"
XRDP_INI="/etc/xrdp/xrdp.ini"
XRDP_INI_BACKUP="/etc/xrdp/xrdp.ini.evpoly.bak"
XRDP_MARKER="# managed-by-evpoly"
SKEL_XSESSION="/etc/skel/.xsession"
SKEL_DESKTOP_DIR="/etc/skel/Desktop"
EVPOLY_DESKTOP_SOURCE="/usr/share/applications/EVPoly.desktop"
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

write_evpoly_desktop_entry() {
  target_path="$1"
  cat > "$target_path" <<'EOF'
[Desktop Entry]
Version=1.0
Type=Application
Name=EVPoly
Comment=Open EVPoly
Exec=evpoly-desktop
Icon=evpoly-desktop
StartupNotify=true
StartupWMClass=Evpoly-desktop
X-GNOME-WMClass=Evpoly-desktop
Terminal=false
Categories=Office;Finance;
OnlyShowIn=XFCE;
X-XFCE-Source=file:///usr/share/applications/EVPoly.desktop
EOF
}

configure_panel_launcher() {
  user_home="$1"

  panel_xml="$user_home/.config/xfce4/xfconf/xfce-perchannel-xml/xfce4-panel.xml"
  panel_dir="$user_home/.config/xfce4/panel"

  [ -f "$panel_xml" ] || return 0
  command -v python3 >/dev/null 2>&1 || return 0

  python3 - "$panel_xml" "$panel_dir" <<'PY'
import os
import sys
import xml.etree.ElementTree as ET

xml_path = sys.argv[1]
panel_dir = sys.argv[2]
launcher_file = "evpoly.desktop"

tree = ET.parse(xml_path)
root = tree.getroot()

plugins_root = root.find("./property[@name='plugins']")
panels_root = root.find("./property[@name='panels']")
panel_two = panels_root.find("./property[@name='panel-2']") if panels_root is not None else None
plugin_ids = panel_two.find("./property[@name='plugin-ids']") if panel_two is not None else None

if plugins_root is None or plugin_ids is None:
    sys.exit(0)

for plugin in plugins_root.findall("./property"):
    if plugin.get("value") != "launcher":
        continue
    items = plugin.find("./property[@name='items']")
    if items is None:
        continue
    for value in items.findall("./value"):
        if value.get("value") == launcher_file:
            sys.exit(0)

existing_ids = []
for plugin in plugins_root.findall("./property"):
    name = plugin.get("name", "")
    if name.startswith("plugin-"):
        try:
            existing_ids.append(int(name.split("-", 1)[1]))
        except ValueError:
            pass

plugin_id = max(existing_ids or [22]) + 1
launcher_dir = os.path.join(panel_dir, f"launcher-{plugin_id}")
os.makedirs(launcher_dir, exist_ok=True)
launcher_path = os.path.join(launcher_dir, launcher_file)
with open(launcher_path, "w", encoding="utf-8") as handle:
    handle.write(
        "[Desktop Entry]\n"
        "Version=1.0\n"
        "Type=Application\n"
        "Name=EVPoly\n"
        "Comment=Open EVPoly\n"
        "Exec=evpoly-desktop\n"
        "Icon=evpoly-desktop\n"
        "StartupNotify=true\n"
        "StartupWMClass=Evpoly-desktop\n"
        "X-GNOME-WMClass=Evpoly-desktop\n"
        "Terminal=false\n"
        "Categories=Office;Finance;\n"
        "OnlyShowIn=XFCE;\n"
        "X-XFCE-Source=file:///usr/share/applications/EVPoly.desktop\n"
    )

new_plugin = ET.Element("property", {
    "name": f"plugin-{plugin_id}",
    "type": "string",
    "value": "launcher",
})
items_prop = ET.SubElement(new_plugin, "property", {
    "name": "items",
    "type": "array",
})
ET.SubElement(items_prop, "value", {
    "type": "string",
    "value": launcher_file,
})

anchor_name = "plugin-21"
inserted = False
children = list(plugins_root)
for idx, child in enumerate(children):
    if child.get("name") == anchor_name:
        plugins_root.insert(idx, new_plugin)
        inserted = True
        break
if not inserted:
    plugins_root.append(new_plugin)

new_value = ET.Element("value", {"type": "int", "value": str(plugin_id)})
id_children = list(plugin_ids)
inserted = False
for idx, child in enumerate(id_children):
    if child.get("value") == "21":
        plugin_ids.insert(idx, new_value)
        inserted = True
        break
if not inserted:
    plugin_ids.append(new_value)

tree.write(xml_path, encoding="UTF-8", xml_declaration=True)
PY
}

configure_user_shortcuts() {
  user_name="$1"
  user_home="$2"
  user_uid="$3"
  user_gid="$4"

  [ -d "$user_home" ] || return 0

  desktop_dir="$user_home/Desktop"
  mkdir -p "$desktop_dir"
  desktop_entry="$desktop_dir/EVPoly.desktop"
  rm -f "$desktop_entry"
  ln -s "$EVPOLY_DESKTOP_SOURCE" "$desktop_entry"
  chown -h "$user_uid:$user_gid" "$desktop_entry" || true

  panel_launcher_root="$user_home/.config/xfce4/panel"
  mkdir -p "$panel_launcher_root"
  configure_panel_launcher "$user_home"
  find "$panel_launcher_root" -maxdepth 2 -type f -name 'evpoly.desktop' -exec chown "$user_uid:$user_gid" {} \; 2>/dev/null || true
  find "$panel_launcher_root" -maxdepth 2 -type f -name 'evpoly.desktop' -exec chmod 0644 {} \; 2>/dev/null || true

  user_runtime_dir="/run/user/$user_uid"
  if [ -d "$user_runtime_dir" ] && command -v runuser >/dev/null 2>&1; then
    desktop_entry_checksum="$(sha256sum "$EVPOLY_DESKTOP_SOURCE" | awk '{print $1}')"
    runuser -u "$user_name" -- env \
      DISPLAY=:0 \
      XDG_RUNTIME_DIR="$user_runtime_dir" \
      DBUS_SESSION_BUS_ADDRESS="unix:path=$user_runtime_dir/bus" \
      gio set -t string "$desktop_entry" metadata::xfce-exe-checksum "$desktop_entry_checksum" >/dev/null 2>&1 || true

    runuser -u "$user_name" -- env \
      DISPLAY=:0 \
      XDG_RUNTIME_DIR="$user_runtime_dir" \
      DBUS_SESSION_BUS_ADDRESS="unix:path=$user_runtime_dir/bus" \
      gio set -t string "$desktop_entry" metadata::trusted "true" >/dev/null 2>&1 || true

    runuser -u "$user_name" -- env \
      DISPLAY=:0 \
      XDG_RUNTIME_DIR="$user_runtime_dir" \
      DBUS_SESSION_BUS_ADDRESS="unix:path=$user_runtime_dir/bus" \
      sh -lc 'xfce4-panel -r >/dev/null 2>&1 </dev/null &' || true

    runuser -u "$user_name" -- env \
      DISPLAY=:0 \
      XDG_RUNTIME_DIR="$user_runtime_dir" \
      DBUS_SESSION_BUS_ADDRESS="unix:path=$user_runtime_dir/bus" \
      sh -lc 'xfdesktop --reload >/dev/null 2>&1 </dev/null &' || true
  fi
}

configure_xsession_defaults() {
  write_file "$SKEL_XSESSION" "$XSESSION_CONTENT"
  chmod 0644 "$SKEL_XSESSION"
  mkdir -p "$SKEL_DESKTOP_DIR"
  rm -f "$SKEL_DESKTOP_DIR/EVPoly.desktop"
  ln -s "$EVPOLY_DESKTOP_SOURCE" "$SKEL_DESKTOP_DIR/EVPoly.desktop"

  getent passwd | while IFS=: read -r user_name _ user_uid user_gid _ user_home user_shell; do
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
    configure_user_shortcuts "$user_name" "$user_home" "$user_uid" "$user_gid"
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
