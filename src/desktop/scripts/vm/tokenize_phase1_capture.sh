#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="${DESKTOP_WORKSPACE_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
DIST_DIR="$WORKSPACE_DIR/dist"
HOST_DCTL="$DIST_DIR/desktopctl"
ENV_FILE="$WORKSPACE_DIR/.env"

RUNS_ROOT="${DESKTOPCTL_RUNS_ROOT:-/tmp/desktopctl-tokenize-runs}"
VM_CLEAN_APPS_BETWEEN_CAPTURES="${VM_CLEAN_APPS_BETWEEN_CAPTURES:-1}"
VM_PHASE1_OPEN_DIR="${VM_PHASE1_OPEN_DIR:-0}"
VM_PHASE1_THEMES="${VM_PHASE1_THEMES:-light,dark}"
VM_PHASE1_APPS="${VM_PHASE1_APPS:-Calculator,Reminders,System Settings,Calendar,Finder,Weather,Mail,Messages,Notes,Photos,Maps,Safari,Music,Podcasts,Preview,TextEdit,App Store,FaceTime,Contacts,Terminal}"
VM_PHASE1_SETTINGS_DEEPLINKS="${VM_PHASE1_SETTINGS_DEEPLINKS:-x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility,x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture}"

usage() {
  cat <<'USAGE'
Usage: tokenize_phase1_capture.sh [vm_host] [vm_user] [vm_window_app] [run_dir]

Runs Tokenize Phase 1 (VM screenshot corpus capture):
- cleans noisy apps between captures
- captures app/state/theme screenshots on VM
- copies each screenshot immediately to host
- stores sidecar metadata (window list, window bounds, snapshot)

Inputs can be passed as args or via src/desktop/.env:
- VM_HOST
- VM_USER
- VM_WINDOW_APP (optional, default: UTM)
USAGE
}

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
  local run_dir_input="${4:-}"

  VM_HOST="${host_input:-${VM_HOST:-}}"
  VM_USER="${user_input:-${VM_USER:-}}"
  VM_WINDOW_APP="${window_input:-${VM_WINDOW_APP:-UTM}}"
  VM_APP_DIR="${VM_APP_DIR:-/Users/${VM_USER}/DesktopCtl/dist}"
  VM_APP_PATH="${VM_APP_DIR}/DesktopCtl.app"
  VM_CLI_PATH="${VM_APP_DIR}/desktopctl"
  RUN_DIR_INPUT="$run_dir_input"
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
  if [[ ! -x "$HOST_DCTL" ]]; then
    echo "Missing host CLI at $HOST_DCTL. Build once first with: just -f src/desktop/Justfile build"
    exit 1
  fi
}

now_ms() {
  python3 -c 'import time; print(int(time.time() * 1000))'
}

timestamp_id() {
  python3 - <<'PY'
from datetime import datetime
import os
print(f"{datetime.now().strftime('%Y%m%dT%H%M%S')}-{os.getpid()}")
PY
}

sha256_or_missing() {
  local path="$1"
  if [[ -f "$path" ]]; then
    shasum -a 256 "$path" | awk '{print $1}'
    return
  fi
  echo "missing"
}

trim() {
  printf '%s' "$1" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'
}

slugify() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | sed -E 's/[^a-z0-9]+/-/g; s/^-+//; s/-+$//'
}

escape_squote() {
  printf '%s' "$1" | sed "s/'/'\"'\"'/g"
}

run_ssh() {
  ssh "$VM_HOST" "$@"
}

run_scp() {
  scp "$@"
}

run_vm_cli() {
  local command_str="$1"
  run_ssh "DESKTOPCTL_APP_PATH='$VM_APP_PATH' '$VM_CLI_PATH' $command_str"
}

close_vm_apps_for_ocr_stability() {
  run_ssh "for app in TextEdit Calculator Reminders Notes Preview Safari \"System Settings\" Settings Calendar Weather Mail Messages Photos Maps Music Podcasts \"App Store\" FaceTime Contacts Terminal; do osascript -e \"tell application \\\"\$app\\\" to if it is running then quit saving no\" >/dev/null 2>&1 || true; done; osascript -e 'tell application \"Finder\" to close every window' >/dev/null 2>&1 || true; sleep 0.3; pkill -x TextEdit || true; pkill -x Calculator || true; pkill -x Reminders || true; pkill -x Notes || true; pkill -x Preview || true; pkill -x Safari || true; pkill -x 'System Settings' || true; pkill -x Settings || true; pkill -x Calendar || true; pkill -x Weather || true; pkill -x Mail || true; pkill -x Messages || true; pkill -x Photos || true; pkill -x Maps || true; pkill -x Music || true; pkill -x Podcasts || true; pkill -x 'App Store' || true; pkill -x FaceTime || true; pkill -x Contacts || true; pkill -x Terminal || true"
}

verify_vm_apps_closed_for_ocr() {
  run_ssh "for app in TextEdit Calculator Reminders Notes Preview Safari 'System Settings' Settings Calendar Weather Mail Messages Photos Maps Music Podcasts 'App Store' FaceTime Contacts Terminal; do if pgrep -x \"\$app\" >/dev/null; then echo \"still-running:\$app\"; exit 1; fi; done"
}

record_step() {
  local step="$1"
  local status="$2"
  local exit_code="$3"
  local duration_ms="$4"
  local stdout_path="$5"
  local stderr_path="$6"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$step" "$status" "$exit_code" "$duration_ms" "$stdout_path" "$stderr_path" >> "$RESULTS_TSV"
}

run_step() {
  local step="$1"
  shift

  local stdout_path="$RUN_DIR/logs/${step}.stdout"
  local stderr_path="$RUN_DIR/logs/${step}.stderr"
  local start_ms end_ms duration_ms exit_code status

  start_ms="$(now_ms)"
  set +e
  "$@" >"$stdout_path" 2>"$stderr_path"
  exit_code=$?
  set -e
  end_ms="$(now_ms)"
  duration_ms=$((end_ms - start_ms))

  if [[ "$exit_code" -eq 0 ]]; then
    status="ok"
  else
    status="fail"
  fi

  LAST_STEP_STDOUT="$stdout_path"
  LAST_STEP_STDERR="$stderr_path"
  LAST_STEP_EXIT_CODE="$exit_code"
  record_step "$step" "$status" "$exit_code" "$duration_ms" "$stdout_path" "$stderr_path"
  return "$exit_code"
}

record_capture() {
  local capture_id="$1"
  local app_name="$2"
  local app_slug="$3"
  local state="$4"
  local state_slug="$5"
  local theme="$6"
  local status="$7"
  local reason="$8"
  local png_path="$9"
  local window_list_json="${10}"
  local window_bounds_json="${11}"
  local snapshot_json="${12}"

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$capture_id" "$app_name" "$app_slug" "$state" "$state_slug" "$theme" "$status" "$reason" "$png_path" "$window_list_json" "$window_bounds_json" "$snapshot_json" \
    >> "$CAPTURES_TSV"
}

save_last_output_to_file() {
  local target_path="$1"
  if [[ "$LAST_STEP_EXIT_CODE" -eq 0 ]]; then
    cp "$LAST_STEP_STDOUT" "$target_path"
    return 0
  fi
  return 1
}

set_theme() {
  local theme="$1"
  local dark_mode="false"
  if [[ "$theme" == "dark" ]]; then
    dark_mode="true"
  fi
  run_step "theme_$(slugify "$theme")" run_ssh "osascript -e 'tell application \"System Events\" to tell appearance preferences to set dark mode to $dark_mode'" || true
  if [[ "$LAST_STEP_EXIT_CODE" -ne 0 ]]; then
    ANY_FAIL=1
  fi
}

capture_case() {
  local app_name="$1"
  local state="$2"
  local theme="$3"
  local launch_command="$4"
  local bounds_title="$5"
  local wait_ms="${6:-1300}"

  local app_slug state_slug theme_slug capture_id capture_prefix wait_secs
  CAPTURE_SEQ=$((CAPTURE_SEQ + 1))
  capture_id="$(printf '%04d' "$CAPTURE_SEQ")"
  app_slug="$(slugify "$app_name")"
  state_slug="$(slugify "$state")"
  theme_slug="$(slugify "$theme")"
  capture_prefix="cap_${capture_id}_${app_slug}_${state_slug}_${theme_slug}"
  wait_secs="$(awk -v ms="$wait_ms" 'BEGIN { printf("%.2f", ms / 1000.0) }')"

  local host_dir host_png remote_png host_windows_json host_bounds_json host_snapshot_json
  local case_status reason
  case_status="ok"
  reason=""

  host_dir="$RUN_DIR/raw/vm/$app_slug"
  mkdir -p "$host_dir"
  host_png="$host_dir/${app_slug}_${state_slug}_${theme_slug}_${capture_id}.png"
  host_windows_json="$host_dir/${app_slug}_${state_slug}_${theme_slug}_${capture_id}.windows.json"
  host_bounds_json="$host_dir/${app_slug}_${state_slug}_${theme_slug}_${capture_id}.bounds.json"
  host_snapshot_json="$host_dir/${app_slug}_${state_slug}_${theme_slug}_${capture_id}.snapshot.json"
  remote_png="/tmp/dctl-tokenize-${capture_id}-${app_slug}-${state_slug}-${theme_slug}.png"

  if [[ "$VM_CLEAN_APPS_BETWEEN_CAPTURES" == "1" ]]; then
    run_step "${capture_prefix}_cleanup_apps" close_vm_apps_for_ocr_stability || true
    if [[ "$LAST_STEP_EXIT_CODE" -ne 0 ]]; then
      case_status="fail"
      reason="cleanup_failed"
      ANY_FAIL=1
    fi

    run_step "${capture_prefix}_cleanup_verify" verify_vm_apps_closed_for_ocr || true
    if [[ "$LAST_STEP_EXIT_CODE" -ne 0 ]]; then
      case_status="fail"
      reason="cleanup_verify_failed"
      ANY_FAIL=1
    fi
  fi

  run_step "${capture_prefix}_launch" run_ssh "$launch_command" || true
  if [[ "$LAST_STEP_EXIT_CODE" -ne 0 ]]; then
    record_capture "$capture_id" "$app_name" "$app_slug" "$state" "$state_slug" "$theme" "skipped" "launch_failed" "" "" "" ""
    return
  fi

  run_step "${capture_prefix}_wait" run_ssh "sleep $wait_secs" || true

  run_step "${capture_prefix}_screen_capture" run_vm_cli "screen capture --out '$remote_png'" || true
  if [[ "$LAST_STEP_EXIT_CODE" -ne 0 ]]; then
    case_status="fail"
    reason="screen_capture_failed"
    ANY_FAIL=1
  fi

  run_step "${capture_prefix}_copy_png" run_scp "$VM_HOST:$remote_png" "$host_png" || true
  if [[ "$LAST_STEP_EXIT_CODE" -ne 0 || ! -f "$host_png" ]]; then
    case_status="fail"
    reason="copy_png_failed"
    ANY_FAIL=1
  fi

  run_step "${capture_prefix}_window_list" run_vm_cli "window list --json" || true
  if ! save_last_output_to_file "$host_windows_json"; then
    : > "$host_windows_json"
  fi

  local bounds_escaped
  bounds_escaped="$(escape_squote "$bounds_title")"
  run_step "${capture_prefix}_window_bounds" run_vm_cli "window bounds --title '$bounds_escaped' --json" || true
  if ! save_last_output_to_file "$host_bounds_json"; then
    : > "$host_bounds_json"
  fi

  run_step "${capture_prefix}_screen_snapshot" run_vm_cli "screen snapshot --json" || true
  if ! save_last_output_to_file "$host_snapshot_json"; then
    : > "$host_snapshot_json"
  fi

  record_capture \
    "$capture_id" "$app_name" "$app_slug" "$state" "$state_slug" "$theme" "$case_status" "$reason" \
    "$host_png" "$host_windows_json" "$host_bounds_json" "$host_snapshot_json"
}

generate_summary() {
  python3 - "$RESULTS_TSV" "$CAPTURES_TSV" "$SUMMARY_JSON" "$RUN_DIR" "$HOST_APP_SHA_BEFORE" "$HOST_APP_SHA_AFTER" <<'PY'
import csv
import json
import sys
from datetime import datetime, timezone

results_tsv, captures_tsv, summary_json, run_dir, sha_before, sha_after = sys.argv[1:]

steps = []
failed_steps = 0
with open(results_tsv, newline="", encoding="utf-8") as f:
    reader = csv.DictReader(f, delimiter="\t")
    for row in reader:
        row["exit_code"] = int(row["exit_code"])
        row["duration_ms"] = int(row["duration_ms"])
        steps.append(row)
        if row["status"] != "ok":
            failed_steps += 1

captures = []
status_counts = {"ok": 0, "skipped": 0, "fail": 0}
with open(captures_tsv, newline="", encoding="utf-8") as f:
    reader = csv.DictReader(f, delimiter="\t")
    for row in reader:
        captures.append(row)
        status = row["status"]
        status_counts[status] = status_counts.get(status, 0) + 1

summary = {
    "generated_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
    "run_dir": run_dir,
    "host_app_sha_before": sha_before,
    "host_app_sha_after": sha_after,
    "host_app_unchanged": (sha_before != "missing") and (sha_before == sha_after),
    "steps_total": len(steps),
    "steps_failed": failed_steps,
    "captures_total": len(captures),
    "captures_ok": status_counts.get("ok", 0),
    "captures_skipped": status_counts.get("skipped", 0),
    "captures_failed": status_counts.get("fail", 0),
    "steps": steps,
    "captures": captures,
    "manual_check_required": [
        "open raw/vm and verify each screenshot has focused target app window",
        "confirm no stray apps/windows polluted OCR for accepted captures"
    ],
}

with open(summary_json, "w", encoding="utf-8") as f:
    json.dump(summary, f, indent=2)
PY
}

main() {
  if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    usage
    exit 0
  fi

  load_env_file
  resolve_inputs "${1:-}" "${2:-}" "${3:-}" "${4:-}"
  ensure_required_inputs

  if [[ -n "$RUN_DIR_INPUT" ]]; then
    RUN_DIR="$RUN_DIR_INPUT"
  else
    RUN_DIR="$RUNS_ROOT/$(timestamp_id)-phase1"
  fi

  mkdir -p "$RUN_DIR/raw/vm" "$RUN_DIR/labels/auto" "$RUN_DIR/labels/ai" "$RUN_DIR/overlay" "$RUN_DIR/logs"
  RESULTS_TSV="$RUN_DIR/logs/results.tsv"
  CAPTURES_TSV="$RUN_DIR/logs/captures.tsv"
  SUMMARY_JSON="$RUN_DIR/logs/summary.json"
  printf 'step\tstatus\texit_code\tduration_ms\tstdout\tstderr\n' > "$RESULTS_TSV"
  printf 'capture_id\tapp_name\tapp_slug\tstate\tstate_slug\ttheme\tstatus\treason\tpng_path\twindow_list_json\twindow_bounds_json\tsnapshot_json\n' > "$CAPTURES_TSV"

  local host_app_bin="${HOST_APP_BIN:-/Applications/DesktopCtl.app/Contents/MacOS/desktopctld}"
  if [[ ! -f "$host_app_bin" && -f "$DIST_DIR/DesktopCtl.app/Contents/MacOS/desktopctld" ]]; then
    host_app_bin="$DIST_DIR/DesktopCtl.app/Contents/MacOS/desktopctld"
  fi
  HOST_APP_SHA_BEFORE="$(sha256_or_missing "$host_app_bin")"

  ANY_FAIL=0
  CAPTURE_SEQ=0
  LAST_STEP_STDOUT=""
  LAST_STEP_STDERR=""
  LAST_STEP_EXIT_CODE=0

  run_step "host_doctor_json" "$HOST_DCTL" doctor --json || true
  run_step "vm_doctor_json" run_vm_cli "doctor --json" || true

  local theme_raw theme app_raw app app_escaped launch_cmd deeplink_raw deeplink deep_escaped

  IFS=',' read -r -a THEMES <<< "$VM_PHASE1_THEMES"
  IFS=',' read -r -a APPS <<< "$VM_PHASE1_APPS"
  IFS=',' read -r -a SETTINGS_DEEPLINKS <<< "$VM_PHASE1_SETTINGS_DEEPLINKS"

  for theme_raw in "${THEMES[@]}"; do
    theme="$(trim "$theme_raw")"
    if [[ -z "$theme" ]]; then
      continue
    fi
    set_theme "$theme"

    for app_raw in "${APPS[@]}"; do
      app="$(trim "$app_raw")"
      if [[ -z "$app" ]]; then
        continue
      fi
      app_escaped="$(escape_squote "$app")"
      launch_cmd="open -a '$app_escaped'"
      capture_case "$app" "default" "$theme" "$launch_cmd" "$app" 1400
    done

    for deeplink_raw in "${SETTINGS_DEEPLINKS[@]}"; do
      deeplink="$(trim "$deeplink_raw")"
      if [[ -z "$deeplink" ]]; then
        continue
      fi
      deep_escaped="$(escape_squote "$deeplink")"
      launch_cmd="open -a 'System Settings' '$deep_escaped' >/dev/null 2>&1 || open '$deep_escaped'"
      capture_case "System Settings" "deeplink-$(slugify "$deeplink")" "$theme" "$launch_cmd" "System Settings" 1800
    done
  done

  HOST_APP_SHA_AFTER="$(sha256_or_missing "$host_app_bin")"
  generate_summary

  echo "tokenize-phase1: run_dir=$RUN_DIR"
  echo "tokenize-phase1: summary=$SUMMARY_JSON"
  echo "tokenize-phase1: captures_tsv=$CAPTURES_TSV"
  echo "tokenize-phase1: manual-check -> open '$RUN_DIR/raw/vm'"

  if [[ "$VM_PHASE1_OPEN_DIR" == "1" ]]; then
    open "$RUN_DIR/raw/vm" || true
  fi

  if [[ "$ANY_FAIL" -ne 0 ]]; then
    echo "tokenize-phase1: one or more required steps failed"
    exit 1
  fi
}

main "$@"
