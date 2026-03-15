#!/usr/bin/env bash
set -euo pipefail

# Host-driven VM permission flow entrypoint.
# This script intentionally contains VM-specific automation logic so that
# generic build/run recipes in the Justfile remain clean.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="${DESKTOP_WORKSPACE_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
DIST_DIR="$WORKSPACE_DIR/dist"
HOST_APP_PATH="$DIST_DIR/DesktopCtl.app"
HOST_DCTL="$DIST_DIR/desktopctl"
ENV_FILE="$WORKSPACE_DIR/.env"

load_env_file() {
  if [[ -f "$ENV_FILE" ]]; then
    set -a
    # shellcheck disable=SC1090
    source "$ENV_FILE"
    set +a
  fi
}

resolve_inputs() {
  local host_input="${1:-}"
  local user_input="${2:-}"
  local window_input="${3:-}"

  VM_HOST="${host_input:-${VM_HOST:-}}"
  VM_USER="${user_input:-${VM_USER:-}}"
  VM_WINDOW_APP="${window_input:-${VM_WINDOW_APP:-UTM}}"
  HOST_RETURN_APP="${HOST_RETURN_APP:-}"
  VM_APP_DIR="${VM_APP_DIR:-/Users/${VM_USER}/DesktopCtl/dist}"
  VM_APP_PATH="${VM_APP_DIR}/DesktopCtl.app"
  VM_CLI_PATH="${VM_APP_DIR}/desktopctl"
}

ensure_required_inputs() {
  if [[ -z "$VM_HOST" ]]; then
    echo "Missing VM host. Set VM_HOST in src/desktop/.env or pass it as arg 1."
    exit 1
  fi
  if [[ -z "$VM_USER" ]]; then
    echo "Missing VM user. Set VM_USER in src/desktop/.env or pass it as arg 2."
    exit 1
  fi
}

run_build() {
  (cd "$WORKSPACE_DIR" && just build)
}

ensure_host_cli() {
  if [[ ! -x "$HOST_DCTL" ]]; then
    run_build
  fi
}

capture_host_return_app_if_needed() {
  if [[ -n "$HOST_RETURN_APP" ]]; then
    return
  fi
  HOST_RETURN_APP="$(
    /usr/bin/osascript -e \
      'tell application "System Events" to get name of first process whose frontmost is true' \
      2>/dev/null || true
  )"
}

ensure_vm_password() {
  if [[ -n "${VM_OS_PASSWORD:-}" ]]; then
    return
  fi
  read -r -s -p "VM macOS password (for Settings unlock prompts): " VM_OS_PASSWORD
  echo
}

restore_host_app() {
  if [[ -n "$HOST_RETURN_APP" && "$HOST_RETURN_APP" != "$VM_WINDOW_APP" && -x "$HOST_DCTL" ]]; then
    "$HOST_DCTL" app show "$HOST_RETURN_APP" || true
  fi
}

run_ssh() {
  ssh "$VM_HOST" "$@"
}

run_scp() {
  scp "$@"
}

normalize_host_workspace() {
  "$HOST_DCTL" app isolate "$VM_WINDOW_APP" >/dev/null
}

run_host_dctl() {
  normalize_host_workspace
  "$HOST_DCTL" "$@"
}

run_host_dctl_soft() {
  normalize_host_workspace
  "$HOST_DCTL" "$@" || true
}

add_entry_in_current_pane() {
  run_host_dctl_soft ui click --text "DesktopCtl" --timeout 1200
  run_host_dctl_soft ui click --settings-remove
  run_host_dctl wait 220

  run_host_dctl ui click --settings-add \
    || run_host_dctl ui click --text-offset "No Items" --dx -182 --dy 20 --timeout 1800 \
    || run_host_dctl ui click --text-offset "Allow the applications" --dx -170 --dy 46 --timeout 1800

  run_host_dctl wait --text "Open" --timeout 6000
  run_host_dctl key press cmd+shift+g
  run_host_dctl type "$VM_APP_DIR"
  run_host_dctl key press enter
  run_host_dctl wait --text "DesktopCtl.app" --timeout 6000
  run_host_dctl ui click --text "DesktopCtl.app" --timeout 3000
  run_host_dctl ui click --text "Open" --timeout 3000
  run_host_dctl wait 380
}

ensure_row_enabled() {
  local row_text="$1"
  if run_host_dctl ui settings enable "$row_text" --timeout 1600; then
    return
  fi
  run_host_dctl ui settings unlock --password "$VM_OS_PASSWORD" --timeout 2800
  run_host_dctl ui settings enable "$row_text" --timeout 1600
}

build_host_artifacts() {
  echo "[1/7] Build host artifacts"
  run_build
}

deploy_artifacts_to_vm() {
  echo "[2/7] Copy artifacts to VM (${VM_HOST})"
  run_ssh "mkdir -p '$VM_APP_DIR'"
  run_scp -r "$HOST_APP_PATH" "$VM_HOST:$VM_APP_DIR/"
  run_scp "$HOST_DCTL" "$VM_HOST:$VM_APP_DIR/"
  run_ssh "chmod +x '$VM_CLI_PATH' '$VM_APP_PATH/Contents/MacOS/desktopctld'"
}

stop_old_vm_processes() {
  echo "[3/7] Stop old daemon in VM"
  run_ssh "pkill -f desktopctld || true; pkill -f 'DesktopCtl.app' || true"
}

focus_vm_window_on_host() {
  echo "[4/7] Focus VM window on host"
  if [[ -n "$HOST_RETURN_APP" && "$HOST_RETURN_APP" != "$VM_WINDOW_APP" ]]; then
    "$HOST_DCTL" app hide "$HOST_RETURN_APP" || true
  fi
  "$HOST_DCTL" open "$VM_WINDOW_APP" --wait
  normalize_host_workspace
}

configure_accessibility_pane() {
  echo "[5/7] Configure Accessibility pane in VM"
  run_ssh "open 'x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility'"
  run_host_dctl wait --text "Allow the applications below to control your computer." --timeout 14000
  add_entry_in_current_pane
  ensure_row_enabled "DesktopCtl"
}

configure_screen_recording_pane() {
  echo "[6/7] Configure Screen Recording pane in VM"
  run_ssh "open 'x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture'"
  run_host_dctl wait --text "Allow the applications below to record the contents of your screen and system audio." --timeout 14000
  add_entry_in_current_pane
  ensure_row_enabled "DesktopCtl"
  run_host_dctl ui click --text "Quit & Reopen" --timeout 2500 \
    || run_host_dctl_soft ui click --text "Later" --timeout 1500
}

verify_vm_permissions_and_capture() {
  echo "[7/7] Verify in VM over SSH"
  run_ssh "DESKTOPCTL_APP_PATH='$VM_APP_PATH' '$VM_CLI_PATH' permissions check"
  run_ssh "DESKTOPCTL_APP_PATH='$VM_APP_PATH' '$VM_CLI_PATH' screen capture --out /tmp/dctl-cap.png"
  run_ssh "DESKTOPCTL_APP_PATH='$VM_APP_PATH' '$VM_CLI_PATH' screen snapshot --json"
  run_ssh "DESKTOPCTL_APP_PATH='$VM_APP_PATH' '$VM_CLI_PATH' screen tokenize --json"
}

main() {
  load_env_file
  resolve_inputs "${1:-}" "${2:-}" "${3:-}"
  ensure_required_inputs
  ensure_host_cli
  capture_host_return_app_if_needed
  ensure_vm_password
  trap restore_host_app EXIT

  build_host_artifacts
  deploy_artifacts_to_vm
  stop_old_vm_processes
  focus_vm_window_on_host
  configure_accessibility_pane
  configure_screen_recording_pane
  verify_vm_permissions_and_capture

  echo "Done: VM permissions and basic verification completed."
}

main "$@"
