#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BENCH_SCRIPT="$ROOT_DIR/dev/tokenize_model_bench.py"

INPUT_DIR="${1:-/Users/oleg/Projects/DesktopCtl/tmp/tokenize-20260317-phase1/raw/vm}"
OUTPUT_ROOT="${2:-/Users/oleg/Projects/DesktopCtl/tmp/tokenize-20260317-phase1/labels/box-experiments-hf-20260317/grounding_dino_tune}"
MODEL_ID="${3:-IDEA-Research/grounding-dino-base}"

if [[ ! -d "$INPUT_DIR" ]]; then
  echo "error: input directory not found: $INPUT_DIR" >&2
  exit 1
fi

mkdir -p "$OUTPUT_ROOT"

# Optional knobs via env vars:
#   SWEEP_SUBSET='manifest:dev/tokenize_slice12.txt'
#   SWEEP_DEVICE='auto|cpu|mps|cuda'
#   SWEEP_ALLOW_DOWNLOAD='1' (default)
#   SWEEP_PARALLEL='1'
SWEEP_SUBSET="${SWEEP_SUBSET:-}"
SWEEP_DEVICE="${SWEEP_DEVICE:-auto}"
SWEEP_ALLOW_DOWNLOAD="${SWEEP_ALLOW_DOWNLOAD:-1}"
SWEEP_PARALLEL="${SWEEP_PARALLEL:-1}"

# profile_name|box_threshold|text_threshold|prompt
PROFILES=(
  "broad_020_020|0.20|0.20|button . icon . input field . list item . panel . table cell . grid cell . toolbar ."
  "broad_030_030|0.30|0.30|button . icon . input field . list item . panel . table cell . grid cell . toolbar ."
  "broad_040_040|0.40|0.40|button . icon . input field . list item . panel . table cell . grid cell . toolbar ."
  "focused_030_035|0.30|0.35|button . list row . table cell . grid cell . panel ."
  "focused_040_045|0.40|0.45|button . list row . table cell . grid cell . panel ."
  "grid_bias_035_040|0.35|0.40|table cell . grid cell . row . column . panel ."
)

echo "sweep: input=$INPUT_DIR"
echo "sweep: output_root=$OUTPUT_ROOT"
echo "sweep: model=$MODEL_ID"

declare -a PROFILE_DIRS=()
for entry in "${PROFILES[@]}"; do
  IFS='|' read -r name box_th text_th prompt <<< "$entry"
  run_dir="$OUTPUT_ROOT/$name"
  PROFILE_DIRS+=("$run_dir")
  mkdir -p "$run_dir"

  echo ""
  echo "=== profile: $name (box=$box_th text=$text_th) ==="

  cmd=(
    uv run dev/tokenize_model_bench.py
    --input "$INPUT_DIR"
    --output "$run_dir"
    --models grounding_dino
    --write-overlays
    --parallel-models "$SWEEP_PARALLEL"
    --device "$SWEEP_DEVICE"
    --grounding-model "$MODEL_ID"
    --grounding-box-threshold "$box_th"
    --grounding-text-threshold "$text_th"
    --grounding-prompt "$prompt"
  )

  if [[ -n "$SWEEP_SUBSET" ]]; then
    cmd+=(--subset "$SWEEP_SUBSET")
  fi
  if [[ "$SWEEP_ALLOW_DOWNLOAD" == "1" ]]; then
    cmd+=(--allow-download)
  fi

  (
    cd "$ROOT_DIR"
    UV_CACHE_DIR=/tmp/uv-cache "${cmd[@]}"
  ) | tee "$run_dir/run.log"
done

echo ""
echo "=== sweep summary ==="
python3 - <<'PY' "$OUTPUT_ROOT"
import json
import sys
from pathlib import Path

root = Path(sys.argv[1])
rows = []
for profile in sorted(p for p in root.iterdir() if p.is_dir()):
    summary = profile / "bench.summary.json"
    if not summary.exists():
        continue
    data = json.loads(summary.read_text())
    models = data.get("models", [])
    if not models:
        continue
    m = models[0]
    rows.append(
        (
            profile.name,
            m.get("status"),
            m.get("images_ok"),
            m.get("images_failed"),
            m.get("boxes_total"),
            m.get("boxes_avg_per_ok_image"),
            m.get("avg_ms_per_image"),
        )
    )

print("profile\tstatus\tok\tfailed\tboxes_total\tboxes_avg\tavg_ms")
for row in rows:
    print("\t".join(str(v) for v in row))
PY

echo ""
echo "done: compare overlays under $OUTPUT_ROOT/<profile>/grounding_dino/..."
