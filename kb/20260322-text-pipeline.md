# 2026-03-22 Text Pipeline Data Flows

## Goal

Document the current end-to-end flow for capture, OCR, tokenize, and live overlay after switching runtime capture to in-memory by default.

## Core Rule: Memory First

- `capture_screen_png(None)` now captures into memory (`RgbaImage`) and does **not** write PNG to disk.
- Disk write happens only when an explicit output path is provided (`Some(out_path)`).

Implementation reference:
- `src/desktop/daemon/src/vision/capture.rs`

## Flow 1: Live Overlay / `screen tokenize` (window mode)

1. Overlay loop sends `Command::ScreenTokenize` without `--screenshot`.
2. Daemon resolves focused/frontmost window bounds.
3. `vision::pipeline::tokenize_window(...)` calls `capture_and_update_active_window(None, bounds)`.
4. Capture is in-memory:
   - full-display image captured
   - cropped to active-window bounds in memory
   - OCR runs from in-memory RGBA (`ocr::recognize_text`)
5. Text grouping + control detection runs on the same in-memory frame:
   - word split/tighten
   - line grouping
   - paragraph grouping
   - final text field merge
   - bordered control detection
6. Payload (`TokenizePayload`) is emitted and fed to overlay renderer.

Key files:
- `src/desktop/daemon/src/main.rs`
- `src/desktop/daemon/src/daemon.rs`
- `src/desktop/daemon/src/vision/pipeline.rs`
- `src/desktop/daemon/src/overlay.rs`

## Flow 2: `screen tokenize --screenshot <file>`

1. Screenshot PNG is loaded from the provided path.
2. OCR runs from loaded RGBA in memory.
3. Same post-processing stack as window mode.
4. `TokenizePayload.image.path` is set to the screenshot path.

Key file:
- `src/desktop/daemon/src/vision/pipeline.rs`

## Flow 3: `screen capture`

1. Command now explicitly provides an output path even when user does not pass `--out`:
   - default path is `/tmp/desktopctl-captures/capture-<ts>.png`
2. Capture writes PNG to that path.
3. OCR/snapshot state still runs from in-memory capture data.
4. Response includes `"path"` and optional `"overlay_path"`.

Key files:
- `src/desktop/daemon/src/daemon.rs`
- `src/desktop/daemon/src/vision/capture.rs`

## Flow 4: Debug Snapshot

`vision::debug::write_debug_snapshot()` now guarantees an on-disk frame by forcing a capture with explicit output path when needed. This keeps debug artifacts stable even though normal runtime capture is memory-first.

Key file:
- `src/desktop/daemon/src/vision/debug.rs`

## Data Structures

- `CapturedImage`:
  - `frame: CapturedFrame` (metadata)
  - `image: RgbaImage` (pixel buffer)
- `CapturedFrame.image_path: Option<PathBuf>`
- `CaptureResult`:
  - `snapshot`
  - `image_path: Option<PathBuf>`
  - `image: RgbaImage`
  - `event_ids`

Key files:
- `src/desktop/daemon/src/vision/types.rs`
- `src/desktop/daemon/src/vision/pipeline.rs`

## Notes

- `write_tokenize_overlay(...)` requires a file-backed source image path and returns an error for in-memory-only payloads (`"<memory>"`).
- This is expected for live overlay, which draws from payload boxes directly and does not require writing static overlay PNGs.
