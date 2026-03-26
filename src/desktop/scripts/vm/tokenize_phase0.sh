#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="${DESKTOP_WORKSPACE_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
DIST_DIR="$WORKSPACE_DIR/dist"
HOST_DCTL="$DIST_DIR/desktopctl"
ENV_FILE="$WORKSPACE_DIR/.env"

RUNS_ROOT="${DESKTOPCTL_RUNS_ROOT:-/tmp/desktopctl-tokenize-runs}"
VM_CLEAN_APPS_BETWEEN_TESTS="${VM_CLEAN_APPS_BETWEEN_TESTS:-1}"
VM_PHASE0_OPEN_IMAGES="${VM_PHASE0_OPEN_IMAGES:-0}"

usage() {
  cat <<'USAGE'
Usage: tokenize_phase0.sh [vm_host] [vm_user] [vm_window_app] [run_dir]

Runs Tokenize Phase 0 (environment lock):
- host/VM doctor checks
- baseline host + VM screenshots
- run artifact directory initialization

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
  run_ssh "for app in TextEdit Calculator Reminders Notes Preview Safari \"System Settings\" Settings; do osascript -e \"tell application \\\"\$app\\\" to if it is running then quit saving no\" >/dev/null 2>&1 || true; done; sleep 0.3; pkill -x TextEdit || true; pkill -x Calculator || true; pkill -x Reminders || true; pkill -x Notes || true; pkill -x Preview || true; pkill -x Safari || true; pkill -x 'System Settings' || true; pkill -x Settings || true"
}

verify_vm_apps_closed_for_ocr() {
  run_ssh "for app in TextEdit Calculator Reminders Notes Preview Safari 'System Settings' Settings; do if pgrep -x \"\$app\" >/dev/null; then echo \"still-running:\$app\"; exit 1; fi; done"
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
    ANY_FAIL=1
  fi
  record_step "$step" "$status" "$exit_code" "$duration_ms" "$stdout_path" "$stderr_path"
  return "$exit_code"
}

generate_summary() {
  python3 - "$RESULTS_TSV" "$SUMMARY_JSON" "$RUN_DIR" "$HOST_APP_SHA_BEFORE" "$HOST_APP_SHA_AFTER" <<'PY'
import csv
import json
import sys
from datetime import datetime, timezone

results_tsv, summary_json, run_dir, sha_before, sha_after = sys.argv[1:]

steps = []
failed = 0
with open(results_tsv, newline="", encoding="utf-8") as f:
    reader = csv.DictReader(f, delimiter="\t")
    for row in reader:
        row["exit_code"] = int(row["exit_code"])
        row["duration_ms"] = int(row["duration_ms"])
        steps.append(row)
        if row["status"] != "ok":
            failed += 1

summary = {
    "generated_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
    "run_dir": run_dir,
    "host_app_sha_before": sha_before,
    "host_app_sha_after": sha_after,
    "host_app_unchanged": (sha_before != "missing") and (sha_before == sha_after),
    "steps_total": len(steps),
    "steps_failed": failed,
    "steps": steps,
    "manual_check_required": [
        "open raw/host-baseline.png and raw/vm-baseline.png",
        "confirm both captures are clean and usable"
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
    RUN_DIR="$RUNS_ROOT/$(timestamp_id)"
  fi

  mkdir -p "$RUN_DIR/raw" "$RUN_DIR/labels/auto" "$RUN_DIR/labels/ai" "$RUN_DIR/overlay" "$RUN_DIR/logs"

  RESULTS_TSV="$RUN_DIR/logs/results.tsv"
  SUMMARY_JSON="$RUN_DIR/logs/summary.json"
  printf 'step\tstatus\texit_code\tduration_ms\tstdout\tstderr\n' > "$RESULTS_TSV"

  local host_app_bin="${HOST_APP_BIN:-/Applications/DesktopCtl.app/Contents/MacOS/desktopctld}"
  if [[ ! -f "$host_app_bin" && -f "$DIST_DIR/DesktopCtl.app/Contents/MacOS/desktopctld" ]]; then
    host_app_bin="$DIST_DIR/DesktopCtl.app/Contents/MacOS/desktopctld"
  fi
  HOST_APP_SHA_BEFORE="$(sha256_or_missing "$host_app_bin")"

  ANY_FAIL=0
  run_step "host_doctor_json" "$HOST_DCTL" doctor --json || true
  run_step "vm_connectivity" run_ssh "echo vm-ok" || true
  run_step "vm_cli_exists" run_ssh "test -x '$VM_CLI_PATH'" || true
  run_step "vm_doctor_json" run_vm_cli "doctor --json" || true

  if [[ "$VM_CLEAN_APPS_BETWEEN_TESTS" == "1" ]]; then
    run_step "vm_cleanup_apps" close_vm_apps_for_ocr_stability || true
    run_step "vm_cleanup_verify" verify_vm_apps_closed_for_ocr || true
  fi

  run_step "host_baseline_capture" "$HOST_DCTL" screen screenshot --out "$RUN_DIR/raw/host-baseline.png" --overlay || true
  run_step "vm_baseline_capture" run_vm_cli "screen screenshot --out /tmp/dctl-tokenize-phase0-baseline.png" || true
  run_step "vm_baseline_copy" run_scp "$VM_HOST:/tmp/dctl-tokenize-phase0-baseline.png" "$RUN_DIR/raw/vm-baseline.png" || true

  run_step "host_snapshot_json" "$HOST_DCTL" screen snapshot --json || true
  run_step "vm_snapshot_json" run_vm_cli "screen snapshot --json" || true

  if [[ "$VM_PHASE0_OPEN_IMAGES" == "1" ]]; then
    run_step "open_host_baseline" open "$RUN_DIR/raw/host-baseline.png" || true
    run_step "open_vm_baseline" open "$RUN_DIR/raw/vm-baseline.png" || true
  fi

  HOST_APP_SHA_AFTER="$(sha256_or_missing "$host_app_bin")"
  generate_summary

  echo "tokenize-phase0: run_dir=$RUN_DIR"
  echo "tokenize-phase0: summary=$SUMMARY_JSON"
  echo "tokenize-phase0: manual-check -> open '$RUN_DIR/raw/host-baseline.png' and '$RUN_DIR/raw/vm-baseline.png'"

  if [[ "$ANY_FAIL" -ne 0 ]]; then
    echo "tokenize-phase0: one or more steps failed"
    exit 1
  fi
}

main "$@"
