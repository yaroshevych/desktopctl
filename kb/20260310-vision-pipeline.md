# Vision Pipeline Spec

## Core Idea

The vision system works like a human eye: peripheral vision detects *that* something changed, then focused vision reads *what* changed. This is efficient and scales well — full-resolution analysis only happens where needed.

---

## Pipeline Overview

```
[screenshot loop]
    │
    ▼
[downscale + grayscale]  ← "embedding" (thumbnail ~64×40px)
    │
    ▼
[diff vs previous embedding]
    │
    ├─ no change → discard, loop
    │
    └─ regions changed → identify changed bounding boxes
                              │
                              ▼
                     [crop full-res region]
                              │
                         ┌────┴────┐
                         │         │
                        OCR    Icon/element matching
                         │         │
                    text output   element ID + bounds
```

---

## Screenshot Loop

- Daemon captures screenshots at a fixed interval (e.g. 200ms)
- Screenshots are stored in a **circular buffer** in memory (e.g. last 10 frames)
- Each frame is immediately downscaled to a thumbnail ("embedding")
- Thumbnails are also kept in a parallel circular buffer

---

## Embeddings (Thumbnails)

"Embedding" here means a downscaled, grayscale representation of the screen — not a neural embedding. It is computationally free.

- Target size: ~64×40px (aspect-ratio-preserving, configurable)
- Grayscale: reduce to single channel
- Purpose: fast pixel-diff between frames

This is the **peripheral vision layer**.

---

## Change Detection

On each frame:

1. Diff the current thumbnail against the previous thumbnail (pixel-wise absolute diff)
2. Threshold the diff to get a binary change mask
3. Find connected regions of change (bounding boxes)
4. Filter out regions below a minimum size (noise rejection)

If no regions pass the filter → skip. No further processing.

---

## Region Zoom-In

For each changed region:

1. Map the thumbnail bounding box back to full-resolution coordinates (scale factor is known)
2. Crop that region from the full-resolution screenshot in the circular buffer
3. Pass the crop to one or more analyzers

This is the **focused vision layer**.

---

## Analyzers

### OCR

- Input: full-res crop
- Output: extracted text with character-level bounding boxes
- Use case: reading notifications, labels, status text, chat messages
- Library: platform-native OCR (e.g. Apple Vision on macOS) or Tesseract fallback

### Icon / Element Matching

- Input: full-res crop
- Output: matched element ID + bounding box within the crop
- Approach: template matching against a library of known icon images
  - For each known icon: sliding window cross-correlation or normalized template match
  - Match above threshold → record element type and position
- This is the **simple embedding system**: icon images are the "embeddings"
- Future: replace template matching with learned visual embeddings (e.g. CLIP-style)

---

## Data Structures

```
ScreenFrame {
    timestamp: u64,
    full_res: Image,         // kept in circular buffer
    thumbnail: Image,        // grayscale, downscaled
}

CircularBuffer<ScreenFrame>  // fixed capacity, e.g. 10 frames

ChangedRegion {
    frame_index: usize,
    bounds_thumbnail: Rect,
    bounds_full_res: Rect,
}

AnalysisResult {
    region: ChangedRegion,
    ocr_text: Option<String>,
    matched_elements: Vec<MatchedElement>,
}

MatchedElement {
    element_id: String,        // e.g. "close_button", "send_icon"
    confidence: f32,
    bounds: Rect,              // in full-res coordinates
}
```

---

## Icon Library

- Icons stored as small PNG images (e.g. 16×16 to 64×64)
- Organized by app or category
- Template matching run per icon per crop
- Threshold tunable per icon (some icons are more distinctive than others)
- Library can be extended by users or distributed as packs

---

## Output

The vision pipeline feeds into the main event system:

```
VisionEvent {
    timestamp: u64,
    region: Rect,              // full-res screen coordinates
    kind: VisionEventKind,
}

VisionEventKind =
    | TextChanged { text: String }
    | ElementAppeared { element_id: String, bounds: Rect }
    | ElementDisappeared { element_id: String }
    | UnknownChange
```

Subscribers (e.g. `desktopctl wait --text "..."`) receive these events via the daemon IPC channel.

---

## Design Constraints

- All processing local by default — no pixels leave the machine
- Full-res frames are never written to disk (in-memory only)
- Circular buffer size and capture interval are configurable
- Pipeline must not block the screenshot loop — analysis runs on a separate thread pool

---

## Non-Goals (v1)

- Neural embeddings / learned features
- Semantic scene understanding
- Multi-monitor change correlation
- Video recording or replay
