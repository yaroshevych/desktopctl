#!/usr/bin/env bash
set -euo pipefail

ROOT="/Users/oleg/Projects/DesktopCtl"
SCRIPTS_DIR="$ROOT/src/desktop/scripts"
PY_SCRIPT="$SCRIPTS_DIR/dev/tokenize_build_mixed_corpus.py"

STAMP="$(date +%Y%m%d-%H%M%S)"
OUTPUT_ROOT="${1:-$ROOT/tmp/tokenize-20260317-phase1/labels/selected/mixed_vm_macpaw_${STAMP}}"

if [[ ! -f "$PY_SCRIPT" ]]; then
  echo "error: missing script: $PY_SCRIPT" >&2
  exit 2
fi

if [[ -x "$SCRIPTS_DIR/.venv/bin/python" ]]; then
  PY_BIN="$SCRIPTS_DIR/.venv/bin/python"
elif command -v python3 >/dev/null 2>&1; then
  PY_BIN="$(command -v python3)"
else
  echo "error: python3 not found" >&2
  exit 2
fi

echo "python=$PY_BIN"
echo "output_root=$OUTPUT_ROOT"

"$PY_BIN" "$PY_SCRIPT" \
  --output-root "$OUTPUT_ROOT" \
  --group-samples 36 \
  --element-samples 36 \
  --seed 20260319

echo "done: $OUTPUT_ROOT"
