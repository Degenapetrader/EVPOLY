#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SESSION_NAME="${EV_WALLET_SYNC_SESSION:-evpoly-wallet-sync}"
DB_PATH="${EV_WALLET_SYNC_DB:-$ROOT_DIR/tracking.db}"
INTERVAL_SEC="${EV_WALLET_SYNC_INTERVAL_SEC:-3600}"
LOG_FILE="${EV_WALLET_SYNC_LOG:-$ROOT_DIR/history/wallet_sync.log}"
PY_BIN="${EV_WALLET_SYNC_PYTHON:-python3}"
SCRIPT_PATH="$ROOT_DIR/scripts/wallet_history_sync.py"

usage() {
  cat <<EOF
Usage:
  ./scripts/wallet_sync_worker.sh start
  ./scripts/wallet_sync_worker.sh stop
  ./scripts/wallet_sync_worker.sh restart
  ./scripts/wallet_sync_worker.sh status
  ./scripts/wallet_sync_worker.sh logs [lines]
  ./scripts/wallet_sync_worker.sh run-once

Env overrides:
  EV_WALLET_SYNC_SESSION (default: evpoly-wallet-sync)
  EV_WALLET_SYNC_DB (default: tracking.db)
  EV_WALLET_SYNC_INTERVAL_SEC (default: 3600)
  EV_WALLET_SYNC_LOG (default: history/wallet_sync.log)
  EV_WALLET_SYNC_PYTHON (default: python3)
EOF
}

need_tmux() {
  if ! command -v tmux >/dev/null 2>&1; then
    echo "tmux is required but not installed" >&2
    exit 1
  fi
}

has_session() {
  tmux has-session -t "$SESSION_NAME" 2>/dev/null
}

run_once() {
  cd "$ROOT_DIR"
  set -a
  set -f
  [[ -f .env ]] && source .env || true
  set +f
  set +a
  "$PY_BIN" "$SCRIPT_PATH" --db "$DB_PATH"
}

start_worker() {
  mkdir -p "$(dirname "$LOG_FILE")"
  if has_session; then
    echo "wallet sync worker already running in session '$SESSION_NAME'"
    return 0
  fi
  local cmd
  cmd="cd '$ROOT_DIR' && set -a && set -f && { [ -f .env ] && source .env; true; } && set +f && set +a && '$PY_BIN' '$SCRIPT_PATH' --db '$DB_PATH' --loop --interval-sec '$INTERVAL_SEC' >> '$LOG_FILE' 2>&1"
  tmux new-session -d -s "$SESSION_NAME" "bash -lc \"$cmd\""
  echo "started wallet sync worker: session='$SESSION_NAME' interval=${INTERVAL_SEC}s"
}

stop_worker() {
  if has_session; then
    tmux kill-session -t "$SESSION_NAME"
    echo "stopped wallet sync worker session '$SESSION_NAME'"
  else
    echo "wallet sync worker is not running"
  fi
}

status_worker() {
  if has_session; then
    echo "wallet sync worker is running (session '$SESSION_NAME')"
    tmux list-panes -t "$SESSION_NAME" -F "pane=#{pane_index} pid=#{pane_pid} cmd=#{pane_current_command}" | head -n 3
  else
    echo "wallet sync worker is not running"
    return 1
  fi
}

logs_worker() {
  local lines="${1:-120}"
  if ! [[ "$lines" =~ ^[0-9]+$ ]]; then
    echo "invalid lines: $lines" >&2
    exit 1
  fi
  if has_session; then
    tmux capture-pane -pt "$SESSION_NAME" -S "-$lines"
  elif [[ -f "$LOG_FILE" ]]; then
    tail -n "$lines" "$LOG_FILE"
  else
    echo "no worker session/log found"
  fi
}

main() {
  need_tmux
  local action="${1:-}"
  case "$action" in
    start)
      start_worker
      ;;
    stop)
      stop_worker
      ;;
    restart)
      stop_worker || true
      start_worker
      ;;
    status)
      status_worker
      ;;
    logs)
      logs_worker "${2:-120}"
      ;;
    run-once)
      run_once
      ;;
    ''|help|-h|--help)
      usage
      ;;
    *)
      echo "unknown action: $action" >&2
      usage
      exit 1
      ;;
  esac
}

main "$@"
