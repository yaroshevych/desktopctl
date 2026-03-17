#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="${DESKTOP_WORKSPACE_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
DIST_DIR="$WORKSPACE_DIR/dist"
HOST_DCTL="$DIST_DIR/desktopctl"
ENV_FILE="$WORKSPACE_DIR/.env"

RUNS_ROOT="${DESKTOPCTL_RUNS_ROOT:-/tmp/desktopctl-runs}"
VM_SKIP_PERMISSION_FLOW="${VM_SKIP_PERMISSION_FLOW:-0}"
VM_SKIP_HOST_BUILD="${VM_SKIP_HOST_BUILD:-1}"
VM_SMOKE_MIN_PASS_RATE="${VM_SMOKE_MIN_PASS_RATE:-100}"
VM_CLEAN_APPS_BETWEEN_TESTS="${VM_CLEAN_APPS_BETWEEN_TESTS:-1}"

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
  local iterations_input="${4:-}"

  VM_HOST="${host_input:-${VM_HOST:-}}"
  VM_USER="${user_input:-${VM_USER:-}}"
  VM_WINDOW_APP="${window_input:-${VM_WINDOW_APP:-UTM}}"
  VM_APP_DIR="${VM_APP_DIR:-/Users/${VM_USER}/DesktopCtl/dist}"
  VM_APP_PATH="${VM_APP_DIR}/DesktopCtl.app"
  VM_CLI_PATH="${VM_APP_DIR}/desktopctl"
  ITERATIONS="${iterations_input:-${VM_SMOKE_ITERATIONS:-1}}"
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
  if [[ ! "$ITERATIONS" =~ ^[0-9]+$ || "$ITERATIONS" -lt 1 ]]; then
    echo "Invalid iterations value: $ITERATIONS"
    exit 1
  fi
  if [[ ! "$HOST_DCTL" =~ .+ || ! -x "$HOST_DCTL" ]]; then
    echo "Missing host CLI at $HOST_DCTL. Build once first with: just -f src/desktop/Justfile build"
    exit 1
  fi
}

now_ms() {
  python3 -c 'import time; print(int(time.time() * 1000))'
}

timestamp_id() {
  python3 - <<'PY'
from datetime import datetime, timezone
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

run_vm_cli() {
  local command_str="$1"
  run_ssh "DESKTOPCTL_APP_PATH='$VM_APP_PATH' '$VM_CLI_PATH' $command_str"
}

close_vm_apps_for_ocr_stability() {
  # Keep Finder and System Settings available; close common foreground apps that add OCR noise.
  run_ssh "pkill -x TextEdit || true; pkill -x Calculator || true; pkill -x Reminders || true; pkill -x Notes || true; pkill -x Preview || true; pkill -x Safari || true"
}

record_step() {
  local iteration="$1"
  local step="$2"
  local status="$3"
  local exit_code="$4"
  local duration_ms="$5"
  local stdout_path="$6"
  local stderr_path="$7"
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$iteration" "$step" "$status" "$exit_code" "$duration_ms" "$stdout_path" "$stderr_path" >> "$RESULTS_TSV"
}

run_step() {
  local iteration="$1"
  local step="$2"
  shift 2

  local stdout_path="$RUN_DIR/vm/iter-${iteration}-${step}.stdout"
  local stderr_path="$RUN_DIR/vm/iter-${iteration}-${step}.stderr"
  local start_ms end_ms duration_ms status exit_code

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

  record_step "$iteration" "$step" "$status" "$exit_code" "$duration_ms" "$stdout_path" "$stderr_path"
  return "$exit_code"
}

run_host_capture() {
  local iteration="$1"
  local step_name="$2"
  local output_png="$RUN_DIR/host/iter-${iteration}-${step_name}.png"
  local output_json="$RUN_DIR/host/iter-${iteration}-${step_name}-debug.json"

  run_step "$iteration" "host_capture_${step_name}" "$HOST_DCTL" screen capture --out "$output_png" --overlay || true
  run_step "$iteration" "host_debug_${step_name}" "$HOST_DCTL" debug snapshot || true
  if [[ -f "$RUN_DIR/vm/iter-${iteration}-host_debug_${step_name}.stdout" ]]; then
    cp "$RUN_DIR/vm/iter-${iteration}-host_debug_${step_name}.stdout" "$output_json" || true
  fi
}

generate_reports() {
  python3 - "$RESULTS_TSV" "$ITERATIONS_TSV" "$SUMMARY_JSON" "$AGGREGATE_JSON" "$RUN_DIR" "$HOST_APP_SHA_BEFORE" "$HOST_APP_SHA_AFTER" <<'PY'
import csv
import json
import math
import os
import statistics
import sys
from datetime import datetime, timezone

results_tsv, iterations_tsv, summary_json, aggregate_json, run_dir, sha_before, sha_after = sys.argv[1:]

def pctl(values, q):
    if not values:
        return None
    vals = sorted(values)
    idx = max(0, min(len(vals) - 1, math.ceil(q * len(vals)) - 1))
    return vals[idx]

steps_by_iter = {}
with open(results_tsv, newline="") as f:
    reader = csv.DictReader(f, delimiter="\t")
    for row in reader:
        row["exit_code"] = int(row["exit_code"])
        row["duration_ms"] = int(row["duration_ms"])
        row["ok"] = row["status"] == "ok"
        steps_by_iter.setdefault(row["iteration"], []).append(row)

iterations = []
pass_count = 0
fail_count = 0
durations_by_step = {}

with open(iterations_tsv, newline="") as f:
    reader = csv.DictReader(f, delimiter="\t")
    for row in reader:
        idx = row["iteration"]
        ok = row["status"] == "ok"
        if ok:
            pass_count += 1
        else:
            fail_count += 1
        iteration_steps = steps_by_iter.get(idx, [])
        for step in iteration_steps:
            durations_by_step.setdefault(step["step"], []).append(step["duration_ms"])
        iterations.append(
            {
                "iteration": int(idx),
                "status": row["status"],
                "duration_ms": int(row["duration_ms"]),
                "steps": iteration_steps,
            }
        )

total = pass_count + fail_count
pass_rate = (pass_count / total * 100.0) if total else 0.0
host_unchanged = (sha_before != "missing") and (sha_before == sha_after)

summary = {
    "generated_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
    "run_dir": run_dir,
    "host_app_sha_before": sha_before,
    "host_app_sha_after": sha_after,
    "host_app_unchanged": host_unchanged,
    "iterations_total": total,
    "iterations_passed": pass_count,
    "iterations_failed": fail_count,
    "pass_rate_percent": round(pass_rate, 2),
    "iterations": iterations,
}

step_metrics = {}
for step_name, durations in sorted(durations_by_step.items()):
    step_metrics[step_name] = {
        "count": len(durations),
        "p50_ms": pctl(durations, 0.5),
        "p95_ms": pctl(durations, 0.95),
        "mean_ms": round(statistics.mean(durations), 2),
    }

aggregate = {
    "generated_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
    "run_dir": run_dir,
    "iterations_total": total,
    "iterations_passed": pass_count,
    "iterations_failed": fail_count,
    "pass_rate_percent": round(pass_rate, 2),
    "host_app_unchanged": host_unchanged,
    "step_latency_ms": step_metrics,
}

with open(summary_json, "w", encoding="utf-8") as f:
    json.dump(summary, f, indent=2)

with open(aggregate_json, "w", encoding="utf-8") as f:
    json.dump(aggregate, f, indent=2)
PY
}

main() {
  load_env_file
  resolve_inputs "${1:-}" "${2:-}" "${3:-}" "${4:-}"
  ensure_required_inputs

  local host_app_bin="${HOST_APP_BIN:-/Applications/DesktopCtl.app/Contents/MacOS/desktopctld}"
  if [[ ! -f "$host_app_bin" && -f "$DIST_DIR/DesktopCtl.app/Contents/MacOS/desktopctld" ]]; then
    host_app_bin="$DIST_DIR/DesktopCtl.app/Contents/MacOS/desktopctld"
  fi

  RUN_DIR="$RUNS_ROOT/$(timestamp_id)"
  mkdir -p "$RUN_DIR/host" "$RUN_DIR/vm"
  RESULTS_TSV="$RUN_DIR/results.tsv"
  ITERATIONS_TSV="$RUN_DIR/iterations.tsv"
  SUMMARY_JSON="$RUN_DIR/summary.json"
  AGGREGATE_JSON="$RUN_DIR/aggregate.json"

  printf 'iteration\tstep\tstatus\texit_code\tduration_ms\tstdout\tstderr\n' > "$RESULTS_TSV"
  printf 'iteration\tstatus\tduration_ms\n' > "$ITERATIONS_TSV"

  HOST_APP_SHA_BEFORE="$(sha256_or_missing "$host_app_bin")"

  local permission_flow_ok
  permission_flow_ok=1
  if [[ "$VM_SKIP_PERMISSION_FLOW" != "1" ]]; then
    if ! run_step "0" "vm_enable_permissions" env \
      DESKTOP_WORKSPACE_DIR="$WORKSPACE_DIR" \
      VM_SKIP_HOST_BUILD="$VM_SKIP_HOST_BUILD" \
      "$WORKSPACE_DIR/scripts/vm/enable_permissions.sh" \
      "$VM_HOST" "$VM_USER" "$VM_WINDOW_APP"; then
      permission_flow_ok=0
      printf '%s\t%s\t%s\n' "0" "fail" "0" >> "$ITERATIONS_TSV"
    fi
  fi

  local i iter_start_ms iter_end_ms iter_duration_ms iter_status any_fail
  any_fail=0
  if [[ "$permission_flow_ok" -eq 1 ]]; then
    for ((i = 1; i <= ITERATIONS; i++)); do
      echo "vm-smoke: iteration $i/$ITERATIONS"
      iter_start_ms="$(now_ms)"
      iter_status="ok"

      if [[ "$VM_CLEAN_APPS_BETWEEN_TESTS" == "1" ]]; then
        run_step "$i" "vm_cleanup_apps" close_vm_apps_for_ocr_stability || iter_status="fail"
      fi

      run_step "$i" "permissions_check" run_vm_cli "permissions check" || iter_status="fail"
      run_step "$i" "open_textedit_wait" run_vm_cli "open TextEdit --wait" || iter_status="fail"
      run_step "$i" "screen_capture" run_vm_cli "screen capture --out /tmp/dctl-smoke-cap.png" || iter_status="fail"
      run_step "$i" "screen_snapshot_json" run_vm_cli "screen snapshot --json" || iter_status="fail"
      run_step "$i" "screen_tokenize_json" run_vm_cli "screen tokenize --json" || iter_status="fail"
      run_step "$i" "debug_snapshot" run_vm_cli "debug snapshot" || iter_status="fail"
      run_host_capture "$i" "final"

      iter_end_ms="$(now_ms)"
      iter_duration_ms=$((iter_end_ms - iter_start_ms))
      printf '%s\t%s\t%s\n' "$i" "$iter_status" "$iter_duration_ms" >> "$ITERATIONS_TSV"
      if [[ "$iter_status" != "ok" ]]; then
        any_fail=1
      fi
    done
  else
    echo "vm-smoke: skipping smoke iterations because vm_enable_permissions failed"
    any_fail=1
  fi

  HOST_APP_SHA_AFTER="$(sha256_or_missing "$host_app_bin")"
  generate_reports

  echo "vm-smoke: run_dir=$RUN_DIR"
  echo "vm-smoke: summary=$SUMMARY_JSON"
  echo "vm-smoke: aggregate=$AGGREGATE_JSON"

  local pass_rate
  pass_rate="$(python3 -c "import json; print(json.load(open('$AGGREGATE_JSON'))['pass_rate_percent'])")"
  local min_pass_rate
  min_pass_rate="$(python3 -c "print(float('$VM_SMOKE_MIN_PASS_RATE'))")"

  if [[ "$HOST_APP_SHA_BEFORE" == "missing" || "$HOST_APP_SHA_BEFORE" != "$HOST_APP_SHA_AFTER" ]]; then
    echo "vm-smoke: host app hash changed or missing ($HOST_APP_SHA_BEFORE -> $HOST_APP_SHA_AFTER)"
    exit 1
  fi

  if python3 - <<PY
pass_rate = float("$pass_rate")
min_rate = float("$min_pass_rate")
raise SystemExit(0 if pass_rate >= min_rate else 1)
PY
  then
    :
  else
    echo "vm-smoke: pass rate below threshold (actual=$pass_rate required=$min_pass_rate)"
    exit 1
  fi

  if [[ "$any_fail" -ne 0 && "$VM_SMOKE_MIN_PASS_RATE" == "100" ]]; then
    echo "vm-smoke: failures detected and threshold is strict (100%)"
    exit 1
  fi
}

main "$@"
