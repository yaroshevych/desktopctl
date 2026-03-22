# Control Detection Notes

This document describes the current box-detection pipeline used by
`tokenize_boxes.rs`.

## Pipeline

1. Build `ProcessedFrame` (`metal_pipeline.rs`):
- raw grayscale (`gray`)
- contrast-stretched Sobel edges (`edge`)
- SATs for edge/gray/gray^2/text-mask occupancy

2. For each text line:
- scan left/right/top/bottom for first significant SAT strip edge
- build a candidate rectangle
- refine candidate with Sobel loop traversal (`sobel_box_detector.rs`)
- if Sobel loop is not reliable, optional background-separation fallback
  (`background_fill_detector.rs`)
- reject candidates that conflict with neighboring text lines

3. Deduplicate overlapping candidates by IoU and border energy.

## Key Thresholds

`tokenize_boxes.rs`
- edge strip trigger: `mean_e >= 4.0` and (`>= 2.5x` rolling avg or `>= 8.0`)
- near-text skip before trigger checks: `max(3px, 0.3 * font_h)`
- max expansion: `x <= 20 * font_h`, `y <= 4 * font_h`
- strict enclosure gate: all four directional scans must find edges

`sobel_box_detector.rs`
- Sobel threshold from pre-border energy:
  `edge_thr = clamp(pre_border_e * 0.6, 3..20)`
- horizontal confidence gate:
  `min_cov = clamp(0.82 + 0.10 * edge_thr/20, 0.82..0.92)`
- connected-component loop checks:
  touches top and bottom bands, side support in middle rows, minimum component
  size proportional to candidate perimeter

`background_fill_detector.rs`
- only for compact candidates (width/area ratio guards)
- compare inner region vs surrounding ring using SAT means/variance
- require absolute and relative background delta:
  `delta >= 7` and (`delta >= texture * 0.8 + 3` or `delta >= 12`)

## Current Scope

The detector is currently optimized for accurate **control boxes**.
Control type classification (text field vs button) is intentionally simplified
at the moment.

## Tuning Guidance

1. Prefer changing one threshold family at a time (scan, Sobel, or background).
2. Validate on both dark/light fixtures.
3. Keep debug traces enabled while tuning:
- `TOKENIZE_DEBUG=1`
- `TOKENIZE_CONTROLS_DEBUG=1`
