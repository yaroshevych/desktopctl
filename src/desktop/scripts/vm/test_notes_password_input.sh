#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="${DESKTOP_WORKSPACE_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
ENV_FILE="$WORKSPACE_DIR/.env"
HOST_DCTL="$WORKSPACE_DIR/dist/desktopctl"

load_env_file() {
  if [[ -f "$ENV_FILE" ]]; then
    set -a
    # shellcheck disable=SC1090
    source "$ENV_FILE"
    set +a
  fi
}

require_inputs() {
  if [[ ! -x "$HOST_DCTL" ]]; then
    echo "Missing $HOST_DCTL (build first with: cd src/desktop && just build)"
    exit 1
  fi
  if [[ -z "${VM_HOST:-}" ]]; then
    echo "Missing VM_HOST (set in src/desktop/.env or export VM_HOST)"
    exit 1
  fi
  if [[ -z "${VM_WINDOW_APP:-}" ]]; then
    VM_WINDOW_APP="UTM"
  fi
  if [[ -z "${VM_OS_PASSWORD:-}" ]]; then
    echo "Missing VM_OS_PASSWORD (set in src/desktop/.env or export VM_OS_PASSWORD)"
    exit 1
  fi
}

run_host_dctl_direct() {
  "$HOST_DCTL" "$@"
}

press_hotkey_direct() {
  local combo="$1"
  if [[ "$combo" == cmd+* ]]; then
    run_host_dctl_direct key press "$combo" || run_host_dctl_direct key press "command+${combo#cmd+}"
  else
    run_host_dctl_direct key press "$combo"
  fi
}

type_text_slowly() {
  local text="$1"
  local delay_ms="${2:-15}"
  local i ch
  for ((i = 0; i < ${#text}; i++)); do
    ch="${text:i:1}"
    printf "info: typing char[%d]='%s'\n" "$((i + 1))" "$ch"
    run_host_dctl_direct type "$ch"
    run_host_dctl_direct wait "$delay_ms"
  done
}

main() {
  load_env_file
  VM_HOST="${1:-${VM_HOST:-}}"
  VM_WINDOW_APP="${2:-${VM_WINDOW_APP:-UTM}}"
  VM_OS_PASSWORD="${3:-${VM_OS_PASSWORD:-}}"
  CHAR_DELAY_MS="${CHAR_DELAY_MS:-15}"
  require_inputs

  echo "[1/4] Open Notes in VM"
  ssh "$VM_HOST" "open -a Notes"

  echo "[2/4] Focus VM window on host"
  run_host_dctl_direct open "$VM_WINDOW_APP" --wait
  run_host_dctl_direct app isolate "$VM_WINDOW_APP" >/dev/null
  run_host_dctl_direct wait 500

  echo "[3/4] Create a new note inside VM (avoids host Cmd+N interception)"
  ssh "$VM_HOST" /usr/bin/osascript <<'APPLESCRIPT'
tell application "Notes"
  activate
  make new note
end tell
APPLESCRIPT
  run_host_dctl_direct wait 450

  echo "[4/4] Type password only (no paste)"
  type_text_slowly "$VM_OS_PASSWORD" "$CHAR_DELAY_MS"
  press_hotkey_direct enter

  echo "Done. Inspect Notes in VM: typed password should appear as a single line."
}

main "$@"
