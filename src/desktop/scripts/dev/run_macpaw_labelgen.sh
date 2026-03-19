#!/usr/bin/env bash
set -euo pipefail

ROOT="/Users/oleg/Projects/DesktopCtl"
SCRIPTS_DIR="$ROOT/src/desktop/scripts"
PY_SCRIPT="$SCRIPTS_DIR/dev/tokenize_macpaw_labelgen.py"

INPUT_ROOT="${1:-$ROOT/tmp/tokenize-20260317-phase1/raw/vm}"
STAMP="$(date +%Y%m%d-%H%M%S)"
OUTPUT_ROOT="${2:-$ROOT/tmp/tokenize-20260317-phase1/labels/auto/macpaw_yolo11l_${STAMP}}"

ELEMENTS_WEIGHTS="$ROOT/tmp/macpaw/yolov11l-ui-elements-detection/ui-elements-detection.pt"
GROUPS_WEIGHTS="$ROOT/tmp/macpaw/yolov11l-ui-groups-detection/ui-groups-detection.pt"

if [[ ! -f "$PY_SCRIPT" ]]; then
  echo "error: missing script: $PY_SCRIPT" >&2
  exit 2
fi
if [[ ! -f "$ELEMENTS_WEIGHTS" ]]; then
  echo "error: missing elements weights: $ELEMENTS_WEIGHTS" >&2
  exit 2
fi
if [[ ! -f "$GROUPS_WEIGHTS" ]]; then
  echo "error: missing groups weights: $GROUPS_WEIGHTS" >&2
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

if ! "$PY_BIN" - <<'PY' >/dev/null 2>&1
import importlib.util
raise SystemExit(0 if importlib.util.find_spec("ultralytics") else 1)
PY
then
  echo "error: ultralytics is missing for $PY_BIN" >&2
  echo "hint: cd $SCRIPTS_DIR && uv pip install ultralytics" >&2
  exit 2
fi

mkdir -p "$OUTPUT_ROOT"

echo "input_root=$INPUT_ROOT"
echo "output_root=$OUTPUT_ROOT"
echo "python=$PY_BIN"
echo "elements_weights=$ELEMENTS_WEIGHTS"
echo "groups_weights=$GROUPS_WEIGHTS"

"$PY_BIN" "$PY_SCRIPT" \
  --input "$INPUT_ROOT" \
  --output "$OUTPUT_ROOT" \
  --elements-weights "$ELEMENTS_WEIGHTS" \
  --groups-weights "$GROUPS_WEIGHTS" \
  --text-mode overlap \
  --text-overlap-min 0.03 \
  --elements-conf 0.22 \
  --groups-conf 0.20 \
  --dedupe-iou 0.90 \
  --max-boxes 700 \
  --write-overlays

echo "done: $OUTPUT_ROOT"
