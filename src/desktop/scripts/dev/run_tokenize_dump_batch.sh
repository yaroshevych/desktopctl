#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
DEFAULT_INPUT="$ROOT/tmp/tokenize-20260317-phase1/raw/vm"
STAMP="$(date +%Y%m%d-%H%M%S)"
DEFAULT_OUTPUT="$ROOT/tmp/tokenize-review-overlays-${STAMP}-text-anchor-v3"

INPUT_ROOT="${1:-$DEFAULT_INPUT}"
OUTPUT_ROOT="${2:-$DEFAULT_OUTPUT}"
BUILD_MODE="${BUILD_MODE:-release}"
TEXT_LABELS_ROOT="${TEXT_LABELS_ROOT:-}"

if [[ ! -d "$INPUT_ROOT" ]]; then
  echo "error: input root not found: $INPUT_ROOT" >&2
  exit 1
fi
if [[ -n "$TEXT_LABELS_ROOT" && ! -d "$TEXT_LABELS_ROOT" ]]; then
  echo "error: text labels root not found: $TEXT_LABELS_ROOT" >&2
  exit 1
fi

case "$BUILD_MODE" in
  release|debug) ;;
  *)
    echo "error: BUILD_MODE must be release or debug (got $BUILD_MODE)" >&2
    exit 1
    ;;
esac

cd "$ROOT/src/desktop"

if [[ "$BUILD_MODE" == "release" ]]; then
  cargo build -p desktopctld --bin tokenize_dump --release >/dev/null
  BIN="$ROOT/src/desktop/target/release/tokenize_dump"
else
  cargo build -p desktopctld --bin tokenize_dump >/dev/null
  BIN="$ROOT/src/desktop/target/debug/tokenize_dump"
fi

mkdir -p "$OUTPUT_ROOT"
LOG_FILE="$OUTPUT_ROOT/run.log"
: > "$LOG_FILE"

IMAGES=()
while IFS= read -r line; do
  IMAGES+=("$line")
done < <(find "$INPUT_ROOT" -type f -name '*.png' | sort)
if [[ ${#IMAGES[@]} -eq 0 ]]; then
  echo "error: no PNG files found under $INPUT_ROOT" >&2
  exit 1
fi

echo "input_root=$INPUT_ROOT" | tee -a "$LOG_FILE"
echo "output_root=$OUTPUT_ROOT" | tee -a "$LOG_FILE"
echo "build_mode=$BUILD_MODE" | tee -a "$LOG_FILE"
echo "text_labels_root=${TEXT_LABELS_ROOT:-<none>}" | tee -a "$LOG_FILE"
echo "images=${#IMAGES[@]}" | tee -a "$LOG_FILE"

failures=0
start_epoch=$(date +%s)

for img in "${IMAGES[@]}"; do
  rel="${img#$INPUT_ROOT/}"
  rel_no_ext="${rel%.png}"
  out_json="$OUTPUT_ROOT/${rel_no_ext}.tokenize.json"
  out_overlay="$OUTPUT_ROOT/${rel_no_ext}.overlay.png"
  mkdir -p "$(dirname "$out_json")"
  extra_args=()
  if [[ -n "$TEXT_LABELS_ROOT" ]]; then
    label_path="$TEXT_LABELS_ROOT/${rel_no_ext}.labels.json"
    if [[ -f "$label_path" ]]; then
      extra_args+=(--text-labels "$label_path")
    else
      echo "warn: missing text labels for $rel at $label_path" | tee -a "$LOG_FILE"
    fi
  fi

  echo "--- $rel" | tee -a "$LOG_FILE"
  if summary="$($BIN --input "$img" --json "$out_json" --overlay "$out_overlay" "${extra_args[@]}" --timings 2>>"$LOG_FILE")"; then
    echo "$summary" | tee -a "$LOG_FILE"
  else
    failures=$((failures + 1))
    echo "error: tokenize_dump failed for $img" | tee -a "$LOG_FILE"
  fi

done

end_epoch=$(date +%s)

echo "duration_sec=$((end_epoch - start_epoch))" | tee -a "$LOG_FILE"
echo "failures=$failures" | tee -a "$LOG_FILE"

echo "done: overlays at $OUTPUT_ROOT"
if [[ $failures -ne 0 ]]; then
  exit 2
fi
