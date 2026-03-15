# DesktopCtl Python Test Data Tools

This directory is isolated from production code. It exists only for fixture and test data generation.

## Setup

From [`src/desktop/scripts`](/Users/oleg/Projects/DesktopCtl/src/desktop/scripts):

```bash
uv sync
```

To enable the optional local AI detector:

```bash
uv sync --extra grounding
```

## Generate window JSON from screenshots

```bash
cd src/desktop/scripts
uv run dev/generate_settings_window_data.py \
  ../daemon/tests/fixtures/settings-screenshots \
  --write-overlays \
  --write-crops \
  --write-sips-crops
```

Hybrid detection is the default. It will use GroundingDINO when the `grounding`
extra is installed and otherwise fall back to the OpenCV-only detector.

To force a specific detector:

```bash
uv run dev/generate_settings_window_data.py \
  ../daemon/tests/fixtures/settings-screenshots/dark-abstract-overlap.png \
  --detector grounding-dino \
  --device cpu \
  --stdout
```

This writes:

- `out/*.window.json`
- `out/*.window.overlay.png`
- `out/*.crop.png`
- `out/*.sips.crop.png`

Use `--overwrite` to replace existing JSON files.
Use `--output-dir <dir>` to write artifacts somewhere else.
