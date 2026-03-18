#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../../../.." && pwd)"
DESKTOP_DIR="$ROOT_DIR/src/desktop"
HOST_APP_BIN="$DESKTOP_DIR/runtime-stable/DesktopCtl.app/Contents/MacOS/desktopctld"
JUSTFILE_PATH="$DESKTOP_DIR/Justfile"

hash_file() {
  local path="$1"
  if [[ -f "$path" ]]; then
    shasum -a 256 "$path" | awk '{print $1}'
  else
    echo "missing"
  fi
}

before_hash="$(hash_file "$HOST_APP_BIN")"
echo "[tokenize-ci] host_app_hash_before=$before_hash"

echo "[tokenize-ci] running Rust test suites"
cd "$DESKTOP_DIR"
cargo test -p desktop-core
cargo test -p desktopctl
cargo test -p desktopctld

echo "[tokenize-ci] running deterministic tokenize checks"
cargo test -p desktopctld vision::pipeline::tests::build_window_elements_is_deterministic_for_same_input -- --nocapture
cargo test -p desktopctld broad_grounding_labels_have_minimum_box_recall -- --nocapture

if [[ "${VM_SMOKE:-0}" == "1" ]]; then
  echo "[tokenize-ci] running vm-smoke with VM_SKIP_HOST_BUILD=1"
  VM_SKIP_HOST_BUILD=1 just -f "$JUSTFILE_PATH" vm-smoke
else
  echo "[tokenize-ci] skipping vm-smoke (set VM_SMOKE=1 to enable)"
fi

after_hash="$(hash_file "$HOST_APP_BIN")"
echo "[tokenize-ci] host_app_hash_after=$after_hash"

if [[ "$before_hash" != "$after_hash" ]]; then
  echo "[tokenize-ci] ERROR: host DesktopCtl.app changed during gate run" >&2
  exit 1
fi

echo "[tokenize-ci] PASS"
