# HF Box Detection Plan (Post-CV Baseline)

## Goal

Find a better box detector for `tokenize` than the current OpenCV-only approach, with explicit focus on:
- rounded corners
- faint grid lines (Calendar month view)
- stable behavior in light + dark themes

No AI labeling in this plan.

---

## Guardrails

- Do not delete or overwrite existing data under:
  - `/Users/oleg/Projects/DesktopCtl/tmp/tokenize-20260317-phase1`
- Write all new experiments to a new run root:
  - `/Users/oleg/Projects/DesktopCtl/tmp/tokenize-20260317-phase1/labels/box-experiments-hf-20260317`
- Keep current scripts and prior detectors as-is; add new scripts/flags only.
- Do not rebuild/update host `DesktopCtl.app` for this work.

---

## Expanded Model Tracks (license ignored for labeling R&D)

### Track 0: Existing baselines (control group)

1. `cv_edge_rect`
2. `cv_edge_ellipse`
3. `cv_morph_gradient`

### Track 1: Fast detector family

1. OmniParser icon detector (`microsoft/OmniParser-v2.0`, YOLO head)
2. YOLOv8n / YOLOv11n (generic + UI-tuned variants if available)
3. RT-DETR (`PekingU/rtdetr_r18vd`, optional `r50vd`)
4. DETR / Conditional DETR / DINO-DETR variants

### Track 2: Grid/layout specialists

1. Table Transformer (`microsoft/table-transformer-detection`)
2. GroundingDINO (`IDEA-Research/grounding-dino-base` + tiny variant)
3. Layout-oriented models from HF notes for text-region/layout support

### Track 3: Segmentation-first family

1. MobileSAM
2. SAM / SAM2
3. Mask DINO
4. Grounded-SAM pipeline (GroundingDINO + SAM)

For Track 3, convert masks to boxes (tight bbox + optional polygon sidecar) so outputs stay compatible with `*.labels.json`.

---

## Evaluation Slice

Use a fixed, diverse subset first (12 images):
- Calendar (light + dark)
- Calculator (light + dark)
- Reminders (light + dark)
- System Settings (2 panes)
- Finder (2 views)
- Dictionary (1)
- Stocks or App Store (1)

Then expand to full 52-image corpus.

---

## Success Criteria

- Calendar: detect outer month grid + majority of internal cell regions (not just text boxes)
- Rounded controls: fewer chopped/missed boxes around pill/rounded UI
- Noise control: median box count should not explode vs baseline
- Determinism: rerun produces same JSON/overlay output for same input

---

## Phase 1: Harness + Adapters

### Deliverables

- New script: `src/desktop/scripts/dev/tokenize_model_bench.py`
- Unified output schema matching current `*.labels.json`
- Adapters for:
  - CV baselines
  - OmniParser/YOLO
  - RT-DETR
  - Table Transformer
  - GroundingDINO
  - optional SAM-family mask-to-box adapter

### Commands (implementation verification)

```bash
cd /Users/oleg/Projects/DesktopCtl
python3 -m py_compile src/desktop/scripts/dev/tokenize_model_bench.py

cd /Users/oleg/Projects/DesktopCtl/src/desktop/scripts
UV_CACHE_DIR=/tmp/uv-cache uv run dev/tokenize_model_bench.py --help
```

---

## Phase 2: 12-image Benchmark Run

### Deliverables

- Per-model folders with JSON + overlays
- Summary JSON/TSV with per-image counts and timing

### Commands (run)

```bash
cd /Users/oleg/Projects/DesktopCtl/src/desktop/scripts
UV_CACHE_DIR=/tmp/uv-cache uv run dev/tokenize_model_bench.py \
  --input /Users/oleg/Projects/DesktopCtl/tmp/tokenize-20260317-phase1/raw/vm \
  --output /Users/oleg/Projects/DesktopCtl/tmp/tokenize-20260317-phase1/labels/box-experiments-hf-20260317/slice12 \
  --models cv_edge_ellipse,cv_morph_gradient,omniparser_icon,yolov8n,rtdetr_r18,table_transformer,grounding_dino \
  --subset manifest:dev/tokenize_slice12.txt \
  --write-overlays
```

### Commands (quick QA)

```bash
find /Users/oleg/Projects/DesktopCtl/tmp/tokenize-20260317-phase1/labels/box-experiments-hf-20260317/slice12 -name '*.overlay.png' | wc -l
find /Users/oleg/Projects/DesktopCtl/tmp/tokenize-20260317-phase1/labels/box-experiments-hf-20260317/slice12 -name '*.labels.json' | wc -l
```

Manual check:
- Open Calendar overlays for each model first.
- Check whether grid cells/lines are captured beyond OCR text anchors.

---

## Phase 3: Full 52-image Run (Top 2-3 only)

### Gate to enter

- At least one non-CV model clearly beats CV baseline on Calendar + rounded controls in slice12.

### Commands

```bash
cd /Users/oleg/Projects/DesktopCtl/src/desktop/scripts
UV_CACHE_DIR=/tmp/uv-cache uv run dev/tokenize_model_bench.py \
  --input /Users/oleg/Projects/DesktopCtl/tmp/tokenize-20260317-phase1/raw/vm \
  --output /Users/oleg/Projects/DesktopCtl/tmp/tokenize-20260317-phase1/labels/box-experiments-hf-20260317/full52 \
  --models <top3_winners_from_slice12> \
  --write-overlays
```

### Commands (report)

```bash
UV_CACHE_DIR=/tmp/uv-cache uv run dev/tokenize_model_bench.py \
  --report-only \
  --input /Users/oleg/Projects/DesktopCtl/tmp/tokenize-20260317-phase1/labels/box-experiments-hf-20260317/full52
```

---

## Phase 4: Decision + Integration Plan

### Deliverables

- Decision note in KB with:
  - chosen primary detector
  - fallback detector
  - known failure modes
  - expected latency/memory envelope
- Integration issue list for Rust `tokenize` implementation

### Commands (artifacts check)

```bash
ls -la /Users/oleg/Projects/DesktopCtl/tmp/tokenize-20260317-phase1/labels/box-experiments-hf-20260317
rg -n "calendar" /Users/oleg/Projects/DesktopCtl/tmp/tokenize-20260317-phase1/labels/box-experiments-hf-20260317 -g '*.json'
```

---

## Notes on dependencies

- Existing `pyproject.toml` already has optional `grounding` deps.
- Add optional groups/scripts as needed for:
  - `ultralytics` (OmniParser/YOLO)
  - `transformers` + `torch` + `torchvision` (RT-DETR/DETR/Table/GroundingDINO)
  - SAM-family dependencies for mask generation
- Use `UV_CACHE_DIR=/tmp/uv-cache` for all runs.
- If a model cannot run locally (dependency/perf/runtime error), mark it as skipped in the benchmark report and continue.
- Keep all outputs, including failed run logs, for later comparison.

---

## Immediate next step

Implement Phase 1 (`tokenize_model_bench.py`) and run the 12-image slice benchmark with at least:
- `cv_edge_ellipse`
- `omniparser_icon`
- `yolov8n`
- `rtdetr_r18`
- `table_transformer`
- `grounding_dino`
