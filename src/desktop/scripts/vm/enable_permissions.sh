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
  VM_DIALOG_DIR="${VM_DIALOG_DIR:-/Users/${VM_USER}/Downloads}"
  VM_APP_PATH="${VM_APP_DIR}/DesktopCtl.app"
  VM_DIALOG_APP_PATH="${VM_DIALOG_DIR}/DesktopCtl.app"
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
    echo "info: VM_OS_PASSWORD=$VM_OS_PASSWORD"
    return
  fi
  read -r -s -p "VM macOS password (for Settings unlock prompts): " VM_OS_PASSWORD
  echo
  echo "info: VM_OS_PASSWORD=$VM_OS_PASSWORD"
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
  "$HOST_DCTL" "$@"
}

run_host_dctl_direct() {
  "$HOST_DCTL" "$@"
}

press_hotkey() {
  local combo="$1"
  if [[ "$combo" == cmd+* ]]; then
    run_host_dctl key press "$combo" || run_host_dctl key press "command+${combo#cmd+}"
  else
    run_host_dctl key press "$combo"
  fi
}

press_hotkey_direct() {
  local combo="$1"
  if [[ "$combo" == cmd+* ]]; then
    run_host_dctl_direct key press "$combo" || run_host_dctl_direct key press "command+${combo#cmd+}"
  else
    run_host_dctl_direct key press "$combo"
  fi
}

run_host_dctl_soft() {
  "$HOST_DCTL" "$@" || true
}

run_host_dctl_try() {
  set +e
  "$HOST_DCTL" "$@"
  local status=$?
  set -e
  return $status
}

run_host_dctl_try_quiet() {
  set +e
  "$HOST_DCTL" "$@" >/dev/null 2>&1
  local status=$?
  set -e
  return $status
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

text_center_from_payload() {
  local payload="$1"
  local x y w h cx cy
  x="$(jq -r '.bounds.x // empty' <<<"$payload" 2>/dev/null || true)"
  y="$(jq -r '.bounds.y // empty' <<<"$payload" 2>/dev/null || true)"
  w="$(jq -r '.bounds.width // empty' <<<"$payload" 2>/dev/null || true)"
  h="$(jq -r '.bounds.height // empty' <<<"$payload" 2>/dev/null || true)"
  if [[ -z "$x" || -z "$y" || -z "$w" || -z "$h" ]]; then
    return 1
  fi
  cx="$(awk -v x="$x" -v w="$w" 'BEGIN { printf("%d", x + (w / 2.0) + 0.5) }')"
  cy="$(awk -v y="$y" -v h="$h" 'BEGIN { printf("%d", y + (h / 2.0) + 0.5) }')"
  if [[ ! "$cx" =~ ^[0-9]+$ || ! "$cy" =~ ^[0-9]+$ ]]; then
    return 1
  fi
  printf '%s %s\n' "$cx" "$cy"
}

click_text_fast() {
  local label="$1"
  local timeout_ms="${2:-100}"
  local payload coords cx cy
  if ! payload="$(run_host_dctl wait --text "$label" --timeout "$timeout_ms" --interval 35 2>/dev/null)"; then
    return 1
  fi
  if ! coords="$(text_center_from_payload "$payload")"; then
    return 1
  fi
  cx="${coords%% *}"
  cy="${coords##* }"
  run_host_dctl pointer click "$cx" "$cy" >/dev/null
}

double_click_text_fast() {
  local label="$1"
  local timeout_ms="${2:-100}"
  local payload coords cx cy
  if ! payload="$(run_host_dctl wait --text "$label" --timeout "$timeout_ms" --interval 35 2>/dev/null)"; then
    return 1
  fi
  if ! coords="$(text_center_from_payload "$payload")"; then
    return 1
  fi
  cx="${coords%% *}"
  cy="${coords##* }"
  run_host_dctl pointer click "$cx" "$cy" >/dev/null
  run_host_dctl wait 25 >/dev/null
  run_host_dctl pointer click "$cx" "$cy" >/dev/null
  run_host_dctl wait 45 >/dev/null
}

click_open_in_open_dialog() {
  local attempts="${1:-4}"
  local i
  for ((i = 1; i <= attempts; i++)); do
    if click_text_fast "Open" 100; then
      return 0
    fi
    run_host_dctl wait 40
  done
  return 1
}

select_desktopctl_from_open_dialog() {
  local attempts="${1:-1}"
  local i
  for ((i = 1; i <= attempts; i++)); do
    if double_click_text_fast "DesktopCtl" 100; then
      # If dialog is still open, submit explicitly.
      if run_host_dctl_try wait --text "Open" --timeout 180; then
        click_open_in_open_dialog 3 || press_hotkey_direct cmd+o || press_hotkey_direct enter
      fi
      return 0
    fi
    if double_click_text_fast "DesktopCtl.app" 100; then
      # If dialog is still open, submit explicitly.
      if run_host_dctl_try wait --text "Open" --timeout 180; then
        click_open_in_open_dialog 3 || press_hotkey_direct cmd+o || press_hotkey_direct enter
      fi
      return 0
    fi
    run_host_dctl wait 35
  done
  return 1
}

submit_password_for_open_dialog() {
  echo "info: unlock prompt detected after '+' click; submitting password"
  echo "info: typing VM_OS_PASSWORD=$VM_OS_PASSWORD"

  # First choice: use daemon unlock helper that re-focuses the password field.
  if run_host_dctl_try ui settings unlock --password "$VM_OS_PASSWORD" --timeout 100; then
    if run_host_dctl_try wait --text "Open" --timeout 100; then
      return 0
    fi
    echo "warn: settings unlock helper did not open file dialog; retrying"
  fi

  # Fallback: clear current field value, then type with short pacing.
  press_hotkey_direct cmd+a
  run_host_dctl_direct wait 30
  type_text_slowly "$VM_OS_PASSWORD" 15
  press_hotkey_direct enter
  run_host_dctl_direct wait 180
}

submit_password_for_settings_change() {
  local reason="${1:-settings change}"
  echo "info: unlock prompt detected after ${reason}; submitting password"
  echo "info: typing VM_OS_PASSWORD=$VM_OS_PASSWORD"

  # Optional dialog: only handle it when present.
  if run_host_dctl_try ui settings unlock --password "$VM_OS_PASSWORD" --timeout 100; then
    run_host_dctl wait 140
    return 0
  fi

  press_hotkey_direct cmd+a
  run_host_dctl_direct wait 30
  type_text_slowly "$VM_OS_PASSWORD" 15
  press_hotkey_direct enter
  run_host_dctl_direct wait 180
}

wait_for_password_prompt() {
  local timeout_ms="${1:-100}"
  run_host_dctl_try wait --text "Password" --timeout "$timeout_ms" \
    || run_host_dctl_try wait --text "Use Password" --timeout "$timeout_ms" \
    || run_host_dctl_try wait --text "Touch ID or Password" --timeout "$timeout_ms"
}

wait_for_screen_recording_pane() {
  local timeout_ms="${1:-8000}"
  run_host_dctl_try wait --text "Screen & System Audio Recording" --timeout "$timeout_ms" \
    || run_host_dctl_try wait --text "Allow the applications below to record the contents of your screen and system audio." --timeout "$timeout_ms" \
    || run_host_dctl_try wait --text "Allow the applications below to record screen and system audio." --timeout "$timeout_ms" \
    || run_host_dctl_try wait --text "Allow the applications below" --timeout "$timeout_ms"
}

close_settings_window_via_close_button() {
  local payload wx wy cx cy attempt
  for attempt in 1 2 3; do
    payload="$(run_host_dctl_try screen settings --json 2>/dev/null || true)"
    if [[ -z "$payload" ]]; then
      payload="$(run_host_dctl_try screen layout --json 2>/dev/null || true)"
    fi
    wx="$(jq -r '.regions.settings_window.x // .regions.window.x // .frontmost_window.x // empty' <<<"$payload" 2>/dev/null || true)"
    wy="$(jq -r '.regions.settings_window.y // .regions.window.y // .frontmost_window.y // empty' <<<"$payload" 2>/dev/null || true)"
    if [[ -z "$wx" || -z "$wy" || "$wx" == "null" || "$wy" == "null" ]]; then
      run_host_dctl wait 120 >/dev/null
      continue
    fi
    # Red traffic-light center is near top-left + ~20px in both axes.
    cx="$(awk -v x="$wx" 'BEGIN { printf("%d", x + 20.5) }')"
    cy="$(awk -v y="$wy" 'BEGIN { printf("%d", y + 20.5) }')"
    if [[ ! "$cx" =~ ^[0-9]+$ || ! "$cy" =~ ^[0-9]+$ ]]; then
      run_host_dctl wait 120 >/dev/null
      continue
    fi
    echo "info: closing Settings via traffic-light at (${cx},${cy}) [attempt ${attempt}]"
    run_host_dctl pointer click "$cx" "$cy" >/dev/null
    run_host_dctl wait 200 >/dev/null
    return 0
  done
  return 1
}

click_settings_add_control() {
  local mode="${1:-default}"
  if [[ "$mode" == "screen_audio" ]]; then
    # Anchor on "System Audio Recording Only" and click above it for the top +/- row.
    # +/- buttons sit ~48px above the "System Audio Recording Only" label.
    run_host_dctl ui click --text-offset "System Audio Recording Only" --dx -100 --dy -58 --timeout 100 \
      || run_host_dctl ui click --text-offset "Allow the applications" --dx -170 --dy 46 --timeout 100 \
      || run_host_dctl ui click --settings-add \
      || run_host_dctl ui click --text-offset "No Items" --dx -182 --dy 20 --timeout 100
    return
  fi
  run_host_dctl ui click --settings-add \
    || run_host_dctl ui click --text-offset "No Items" --dx -182 --dy 20 --timeout 100 \
    || run_host_dctl ui click --text-offset "Allow the applications" --dx -170 --dy 46 --timeout 100
}

maximize_vm_window_on_host() {
  /usr/bin/osascript "$VM_WINDOW_APP" <<'APPLESCRIPT' >/dev/null 2>&1 || {
on run argv
  set appName to item 1 of argv
  set menuBarInset to 28
  set bottomInset to 6

  tell application appName to activate

  set desktopBounds to {0, 0, 1470, 956}
  try
    tell application "Finder"
      set desktopBounds to bounds of window of desktop
    end tell
  end try

  set {x0, y0, x1, y1} to desktopBounds
  set targetX to x0
  set targetY to y0 + menuBarInset
  set targetW to x1 - x0
  set targetH to (y1 - y0) - menuBarInset - bottomInset
  if targetW < 640 then set targetW to 640
  if targetH < 480 then set targetH to 480

  tell application "System Events"
    if not (exists process appName) then return
    tell process appName
      if (count of windows) = 0 then return
      tell front window
        try
          set position to {targetX, targetY}
        end try
        try
          set size to {targetW, targetH}
        end try
      end tell
    end tell
  end tell
end run
APPLESCRIPT
    echo "warn: failed to maximize ${VM_WINDOW_APP} window on host (continuing)"
  }
}

add_entry_in_current_pane() {
  local add_mode="${1:-default}"
  normalize_host_workspace
  if click_text_fast "DesktopCtl" 100; then
    if [[ "$add_mode" == "screen_audio" ]]; then
      run_host_dctl ui click --text-offset "System Audio Recording Only" --dx -56 --dy -58 --timeout 100
    else
      run_host_dctl_soft ui click --settings-remove
    fi
    if wait_for_password_prompt 100; then
      submit_password_for_settings_change "remove (-) click"
    fi
    sleep 0.1
    # After remove, macOS reflows the controls row; allow it to settle.
    run_host_dctl wait 200
  else
    echo "info: DesktopCtl row not found in current pane; skipping remove"
  fi

  click_settings_add_control "$add_mode"

  if wait_for_password_prompt 100; then
    submit_password_for_open_dialog
    if ! run_host_dctl_try wait --text "Open" --timeout 100; then
      echo "info: Open dialog not visible after unlock; retrying '+' click once"
      click_settings_add_control "$add_mode"
    fi
  fi

  run_host_dctl wait --text "Open" --timeout 100
  local submitted_with_open=0
  if select_desktopctl_from_open_dialog 1; then
    submitted_with_open=1
  else
    echo "warn: OCR could not find DesktopCtl in Open dialog; falling back to full path"
    press_hotkey cmd+shift+g
    run_host_dctl wait 60
    run_host_dctl type "$VM_DIALOG_APP_PATH"
    press_hotkey enter
    run_host_dctl wait 80
  fi
  if [[ "$submitted_with_open" -eq 0 ]]; then
    click_open_in_open_dialog 4 || press_hotkey_direct cmd+o || press_hotkey_direct enter
  fi
  run_host_dctl wait 100
}

ensure_row_enabled() {
  local row_text="$1"
  if run_host_dctl ui settings enable "$row_text" --timeout 100; then
    return
  fi
  run_host_dctl ui settings unlock --password "$VM_OS_PASSWORD" --timeout 100
  run_host_dctl ui settings enable "$row_text" --timeout 100
}

build_host_artifacts() {
  echo "[1/7] Build host artifacts"
  run_build
}

deploy_artifacts_to_vm() {
  echo "[2/7] Copy artifacts to VM (${VM_HOST})"
  echo "      VM deploy dir: ${VM_APP_DIR}"
  echo "      VM Open-dialog dir: ${VM_DIALOG_DIR}"
  run_ssh "mkdir -p '$VM_APP_DIR' '$VM_DIALOG_DIR'"
  run_scp -r "$HOST_APP_PATH" "$VM_HOST:$VM_APP_DIR/"
  if [[ "$VM_DIALOG_DIR" != "$VM_APP_DIR" ]]; then
    run_scp -r "$HOST_APP_PATH" "$VM_HOST:$VM_DIALOG_DIR/"
  fi
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
  maximize_vm_window_on_host
  normalize_host_workspace
}

configure_accessibility_pane() {
  echo "[5/7] Configure Accessibility pane in VM"
  run_ssh "open 'x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility'"
  run_host_dctl wait --text "Allow the applications below to control your computer." --timeout 8000
  add_entry_in_current_pane default
  # ensure_row_enabled "DesktopCtl"
}

configure_screen_recording_pane() {
  echo "[6/7] Configure Screen Recording pane in VM"
  echo "      sending Cmd+Q to close Settings in VM before Screen Recording deep-link"
  press_hotkey cmd+q
  run_host_dctl wait 150
  run_ssh "open -a 'System Settings' 'x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture' >/dev/null 2>&1 || open 'x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture'"
  if ! wait_for_screen_recording_pane 600; then
    echo "warn: deep-link did not land on Screen Recording pane; trying sidebar navigation"
    # run_host_dctl_soft ui click --text "Screen & System Audio Recording" --timeout 100
    run_host_dctl ui click --text-offset "System Audio Recording Only" --dx -96 --dy -58 --timeout 100
    wait_for_screen_recording_pane 600
  fi
  add_entry_in_current_pane screen_audio
  # ensure_row_enabled "DesktopCtl"
  run_host_dctl ui click --text "Quit & Reopen" --timeout 100 \
    || run_host_dctl_soft ui click --text "Later" --timeout 100
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
