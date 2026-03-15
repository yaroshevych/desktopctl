# Settings Region Detection Test Guide

## Purpose

This test validates `SettingsRegions` detection for the VM permissions screen:

- window bounds
- sidebar/content split
- table/list band bounds
- inferred `+` click location

It uses static fixture screenshots so results are repeatable.

## How Detection Works (Brief)

`detect_settings_regions` in `src/desktop/daemon/src/vision/regions.rs`:

1. Finds the large neutral Settings panel candidate (`window`).
2. Splits sidebar vs content, then refines the split by scanning for the real divider edge.
3. Detects the table/list band via multi-pass luminance preprocessing:
   - raw grayscale
   - contrast-stretched grayscale
   - binary threshold image
4. Uses edge-voting to pick top/bottom and left/right borders of the band.
5. Cross-checks with detected `+/-` control pair when available.
6. Falls back to heuristics if border evidence is weak.

## Fixtures

Location:

- `src/desktop/daemon/tests/fixtures/settings-plus/`

Current images:

- `vm-accessibility-empty-left.png`
- `vm-accessibility-empty-center.png`
- `vm-accessibility-empty-right.png`

## Automated Test

Run:

```bash
cargo test -p desktopctld --manifest-path src/desktop/Cargo.toml --test settings_plus_fixtures
```

What it checks:

1. Window/content bounds are within tolerance of expected values.
2. Inferred `+` click point is within tolerance of expected coordinates.

Test file:

- `src/desktop/daemon/tests/settings_plus_fixtures.rs`

## Visual Debug Overlay

To render bounds on top of a screenshot:

```bash
src/desktop/scripts/dev/render-settings-regions.sh <input.png> <output.png>
```

Example:

```bash
src/desktop/scripts/dev/render-settings-regions.sh \
  src/desktop/daemon/tests/fixtures/settings-plus/vm-accessibility-empty-center.png \
  /tmp/vm-accessibility-empty-center.regions.png
```

Renderer:

- `src/desktop/daemon/examples/render_settings_regions.rs`

The command prints detected bounds as JSON and writes an annotated image.

## Overlay Legend

- Yellow: window bounds
- Gray: sidebar bounds
- Green: content bounds
- Red: table/list band bounds
- Green crosshair: inferred `+` click point (`table.x + 12`, `table.y + table.height - 8`)

## Expected Geometry Rule of Thumb

For a correct detection on these fixtures:

- Green left edge should align with the sidebar/content divider.
- Red band should cover the top permissions table area containing:
  - instruction line
  - `No Items` row area
  - `+/-` controls

## When a Fixture Fails

1. Run the renderer for the failing image and inspect the overlay.
2. Check whether failure is:
   - divider split error (green misplaced)
   - table band height/vertical alignment error (red misplaced)
3. Update detection logic in:
   - `src/desktop/daemon/src/vision/regions.rs`
4. Re-run:

```bash
cargo test -p desktopctld --manifest-path src/desktop/Cargo.toml --test settings_plus_fixtures
```

If behavior changed intentionally, update expected fixture coordinates in:

- `src/desktop/daemon/tests/settings_plus_fixtures.rs`
