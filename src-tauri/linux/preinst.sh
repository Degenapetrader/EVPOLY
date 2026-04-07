#!/bin/sh
set -eu

log() {
  echo "[evpoly preinst] $*"
}

extract_json_bool() {
  json_path="$1"
  key="$2"

  if [ ! -f "$json_path" ]; then
    return 1
  fi

  if grep -Eq "\"$key\"[[:space:]]*:[[:space:]]*true" "$json_path"; then
    echo "true"
    return 0
  fi

  if grep -Eq "\"$key\"[[:space:]]*:[[:space:]]*false" "$json_path"; then
    echo "false"
    return 0
  fi

  return 1
}

extract_active_profile_id() {
  profiles_path="$1"
  if [ ! -f "$profiles_path" ]; then
    return 0
  fi

  sed -n 's/.*"active_profile_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$profiles_path" | head -n 1
}

detect_running_simulation() {
  if ps -eo args 2>/dev/null | grep -F -- '--simulation' | grep -F 'evpoly-bot' | grep -v grep >/dev/null 2>&1; then
    echo "true"
  else
    echo "false"
  fi
}

write_pending_resume_offer() {
  data_dir="$1"
  simulation="$2"
  profile_id="$3"

  [ -d "$data_dir" ] || return 0

  pending_resume_path="$data_dir/pending_resume.json"
  prepared_at="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

  mkdir -p "$data_dir"
  if [ -n "$profile_id" ]; then
    cat > "$pending_resume_path" <<EOF
{"reason":"linux_update","simulation":$simulation,"profile_id":"$profile_id","prepared_at_utc":"$prepared_at"}
EOF
  else
    cat > "$pending_resume_path" <<EOF
{"reason":"linux_update","simulation":$simulation,"profile_id":null,"prepared_at_utc":"$prepared_at"}
EOF
  fi
}

prepare_resume_markers() {
  running_simulation="$(detect_running_simulation)"
  any_runtime_marked="false"

  for data_dir in /home/*/.local/share/evpoly /root/.local/share/evpoly; do
    [ -d "$data_dir" ] || continue

    last_state_path="$data_dir/last_state.json"
    profiles_path="$data_dir/profiles.json"
    was_running="$(extract_json_bool "$last_state_path" "was_running" || echo "false")"
    simulation="$(extract_json_bool "$last_state_path" "simulation" || echo "$running_simulation")"

    if [ "$was_running" = "true" ]; then
      write_pending_resume_offer "$data_dir" "$simulation" "$(extract_active_profile_id "$profiles_path")"
      any_runtime_marked="true"
    fi
  done

  if [ "$any_runtime_marked" = "false" ]; then
    for data_dir in /home/*/.local/share/evpoly /root/.local/share/evpoly; do
      [ -d "$data_dir" ] || continue
      write_pending_resume_offer "$data_dir" "$running_simulation" "$(extract_active_profile_id "$data_dir/profiles.json")"
    done
  fi
}

have_matching_processes() {
  for comm in evpoly-desktop evpoly-bot evpoly-bot-real polymarket-arbitrage-bot; do
    if pgrep -x "$comm" >/dev/null 2>&1; then
      return 0
    fi
  done
  return 1
}

stop_matching_processes() {
  signal="$1"
  for comm in evpoly-bot evpoly-bot-real polymarket-arbitrage-bot evpoly-desktop; do
    pkill "-$signal" -x "$comm" >/dev/null 2>&1 || true
  done
}

case "${1:-install}" in
  install|upgrade)
    if have_matching_processes; then
      log "preparing pending resume marker before Linux upgrade stop"
      prepare_resume_markers
      log "stopping existing EVPoly desktop and bot processes"
      stop_matching_processes TERM

      wait_count=0
      while have_matching_processes && [ "$wait_count" -lt 10 ]; do
        sleep 1
        wait_count=$((wait_count + 1))
      done

      if have_matching_processes; then
        log "forcing remaining EVPoly desktop and bot processes to exit"
        stop_matching_processes KILL
      fi
    fi
    ;;
  *)
    log "no pre-install action for argument: ${1:-}"
    ;;
esac

exit 0
