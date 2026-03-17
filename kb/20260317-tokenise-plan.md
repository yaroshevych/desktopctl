# Tokenise Milestone Plan

## Goal

Build a deterministic pipeline that takes a macOS window screenshot and outputs a structured set of primitive tokens (text, box, glyph) per window. No semantic classification yet. Debuggable via static PNG overlays and fixture tests.

---

## Execution guardrails

- Do not rebuild or update the host `DesktopCtl.app` during development — use source tree builds only (`cargo test`, `cargo build`) and the existing CLI binary.
- Before every VM capture session, close all foreground apps to protect OCR quality.
- Every phase produces screenshots and overlays that are manually checked before proceeding.

---

## Milestone exit criteria

- `screen tokenize --json` produces stable, deterministic output on repeated runs
- Text extraction is broadly correct across native and Electron apps
- Most text-backed controls have a plausible surrounding box
- Glyphs capture obvious non-text small controls without flooding noise
- All elements are grouped by window
- Fixture tests pass against the golden set
- Static PNG overlay looks plausibly useful to a human

---

## Phase 0: Environment Lock

Establish a reproducible run environment before any capture or implementation work.

- Verify host CLI and VM CLI are both healthy (`doctor` check)
- Confirm VM is reachable and DesktopCtl daemon is running
- Capture one baseline screenshot from both host and VM and confirm files open correctly
- Define and lock the run folder structure for all artifacts:

```
<run-root>/
  raw/        ← source-of-truth screenshots from VM
  labels/
    auto/     ← FOSS model output (JSON + overlay PNGs)
    ai/       ← AI-verified JSON with verdicts
  overlay/    ← overlays generated during Rust implementation
  logs/       ← CLI output, doctor checks
```

Manual check gate: both baseline screenshots open and show clean window captures.

---

## Phase 1: Screenshot Corpus

### 1.1 VM setup

Use the VM already configured for the permissions flow. Automate via DesktopCtl. Capture window-only screenshots (not full screen) using the OS window bounds API already implemented.

Before each capture session: close all running apps to prevent stray windows from polluting OCR. Use graceful quit first; if an app is stuck after a short wait, use `kill` to force-terminate it before proceeding.

### 1.2 Apps to capture

Capture as many stock macOS apps as possible. No focused subset — breadth is the goal. Every app adds visual diversity that improves detector robustness.

**Known high-value apps:**
- Calculator (round button grid)
- Reminders (simple flat UI, colored circle glyphs)
- System Settings — multiple panes via deeplinks, especially Privacy & Security
- Calendar (complex layout, multiple views)
- Finder (toolbar, sidebar, list/icon views)
- Weather
- Mail
- Messages
- Notes
- Photos
- Maps
- Safari
- Music
- Podcasts
- Preview
- TextEdit
- App Store
- FaceTime
- Contacts
- Terminal

Capture every stock app that can be launched and has a visible window. If an app requires sign-in or has no useful default state, skip it and note why.

### 1.3 States to capture per app

- Default/empty state
- Populated state (data visible)
- Modal or dialog open where applicable
- For System Settings: navigate specific panes via `x-apple.systempreferences:` deeplinks, especially Privacy & Security

### 1.4 Theme variants

- Light mode and dark mode for every state
- Default window size only

### 1.5 Volume target

- Target: 300–500 screenshots across all stock apps, states, and themes
- More is better — breadth across visual styles matters more than depth per app

### 1.6 Output structure

```
data/
  raw/
    calculator/
    reminders/
    settings/
    calendar/
    finder/
    .../
```

Each filename encodes app, state, and theme (e.g. `reminders_populated_light_001.png`). Each screenshot is transferred to the host immediately after capture to prevent VM disk overflow.

### 1.7 Transfer to host

Transfer each screenshot to host immediately after capture, not in a batch at the end. Raw images are source of truth — never modify them.

Manual check gate: open all captured PNGs and confirm the target app is focused with no extra windows polluting the frame.

---

## Phase 2: Labeling Pipeline

### 2.1 Model comparison (small sample first)

Before labeling the full corpus, run a benchmark on 5–10 diverse screenshots from the focused set. Candidates:

- **OmniParser** — purpose-built for UI screenshot parsing, outputs structured elements
- **Florence-2** — single-model detection + OCR + region description
- **GroundingDINO** — zero-shot detection with text prompts
- **Apple Vision (via pyobjc)** — native macOS OCR, best text-position accuracy for macOS screenshots
- **PaddleOCR** — strong OCR + layout detection

For each candidate, generate a PNG overlay and inspect visually. Pick the best region detector. Apple Vision is likely the best OCR source regardless of which region detector wins — the final pipeline may combine both.

### 2.2 Label schema

Freeze this schema before running the full corpus. Labels stay primitive-only — no semantic roles at this stage.

Fields per element:
- `id` — stable identifier within the screenshot
- `type` — one of: `text`, `box`, `glyph`
- `bbox` — `[x, y, w, h]` in screenshot coordinates, consistent format everywhere
- `text` — OCR content (for text type; optional for glyph)
- `confidence` — float, optional, from OCR or detector
- `source` — which model/stage produced this element (useful for later debug)

Top-level wrapper:
- `image` — path, width, height

### 2.3 Labeling rubric

Annotators (human and AI) must follow these rules:

- `text` — OCR-backed readable text span; store normalized text exactly as OCR returns it
- `box` — visible UI container or control boundary; choose the tight box, avoid encroaching on neighboring controls
- `glyph` — small non-text visual mark (plus, circle, check-like shape, icon) that is visually distinct from text
- When both `text`/`glyph` and `box` are present for the same control, keep both — do not deduplicate away valid child tokens
- Reject obvious OCR noise unless it is clearly UI-significant
- When uncertain, prefer a conservative smaller `box` and flag for human review

### 2.4 Auto-labeling

Run the chosen Python pipeline on the corpus. Output one JSON file per screenshot. Also produce a PNG overlay for visual inspection. These are noisy initial candidates, not ground truth.

### 2.5 AI verification

Send each screenshot + candidate JSON to Claude (`claude -p`) or Codex in non-interactive mode. The agent returns:
- Corrected JSON (primitive types only: text / box / glyph — no semantic labels)
- A verdict: `accept` or `needs_human_review`
- An issue list describing what was changed or flagged

### 2.6 Human spot-check

Review policy:
- 100% of `needs_human_review` cases
- Random 20% sample of `accept` cases

Escalation rule: if the sampled error rate on `accept` cases exceeds 5%, re-review the full batch and update the AI prompt and rubric before continuing.

Weight spot-check toward hard cases: System Settings (dense, nested), dark mode variants.

### 2.7 Golden set

Select 20–30 representative screenshots from the human-reviewed set:
- A few easy (Calculator, Reminders)
- A few medium (Calendar, Finder)
- A few hard (Settings, Weather)

Save each as a PNG + `expected.json` pair in `data/golden/`. The golden set is these pairs — the PNG is the input, the JSON is the ground truth the Rust detector is tested against.

Golden set assertions are tolerance-based:
- Key text labels must be present
- At least one box must overlap important control regions above a threshold
- Glyph count must not exceed a reasonable ceiling
- Window metadata must be correct

Manual check gate: open all golden overlays and confirm labels look correct before moving to Phase 3.

---

## Phase 3: Rust Implementation

Work against the full corpus throughout.

### 3.1 Schema and types first

Define the output struct in Rust matching the frozen JSON schema. Do this before any image processing. Fixture tests are written against this schema from the start.

### 3.2 OCR adapter

Wrap Apple Vision OCR. Input is a window crop derived from OS window bounds. Output is normalized OCR elements in screenshot coordinate space. Preserve raw OCR output alongside normalized output.

Exit condition: rendering OCR output on the golden screenshots looks correct. Manual check gate.

### 3.3 Text tokens

Convert normalized OCR output into `text` elements. One OCR box becomes one text element. No post-processing yet. Store exact text and confidence.

Exit condition: text overlay on golden set looks sane. Manual check gate.

### 3.4 Padded text-anchored boxes (v0)

For each text element, expand left/right/up/down with fixed padding to create a candidate box. Crude but enables end-to-end pipeline testing before harder image processing.

Exit condition: overlays show boxes around most text-backed controls. Manual check gate.

### 3.5 Static PNG overlay

Generate an overlay image showing detected elements over the screenshot. Color conventions:
- Text: green
- Boxes: blue
- Glyphs: yellow
- Window outlines: white

This is the primary visual feedback loop for all remaining steps. Keep color conventions stable.

### 3.6 Fixture tests against golden set

The golden set is the 20–30 hand-inspected PNG + `expected.json` pairs. Each pair is a fixture: run the detector against the PNG, assert output matches the JSON within coordinate tolerance. Add these now and run throughout all remaining phases.

### 3.7 Edge-based box growth (v1)

Replace padded boxes with geometry-driven growth. Try multiple strategies and compare on the golden set — keep whichever scores best, or combine:

- **Canny + contour** — edge detect, find contours, snap to nearest enclosing rectangle around the OCR anchor
- **Sobel gradient flood** — grow outward from OCR anchor, stop when cumulative gradient exceeds threshold
- **Hough line snap** — detect dominant horizontal/vertical lines in the window region, snap box edges to nearest detected line
- **Color/contrast boundary** — grow while background pixel variance stays low, stop at sharp tonal shift

All strategies: conservative. Prefer smaller plausible box over large noisy region.

Exit condition: best strategy (or combination) improves golden overlays materially over padded boxes. Manual check gate.

### 3.8 Box deduplication and merge

Multiple overlapping boxes accumulate from padded boxes, edge-grown boxes, and multiple OCR fragments. Merge stage: remove near-duplicates, handle containment (keep both if size difference is meaningful — smaller may be a control, larger a container). Goal is manageable output, not perfect hierarchy.

Exit condition: element counts are manageable, overlays are readable, obvious duplicates eliminated.

### 3.9 Glyph extraction

Intentionally conservative. Catch obvious non-text small visual marks: `+`, circles, check-like shapes, small icon-only controls. Use connected components or contour detection with size and contrast thresholds. Do not emit glyphs that overlap OCR text.

False positives are the main failure mode here — err toward fewer, higher-confidence glyphs.

Exit condition: obvious glyphs appear without noise flooding. Manual check gate.

### 3.10 Window metadata

Since every screenshot is a single-window crop, there is always exactly one window per image. What this step does:

- Record window title, app name, and original OS bounds as metadata in the JSON output (captured at screenshot time, embedded in filename or sidecar)
- Ensure all elements reference the single window ID for consistency with the live tokenizer

Exit condition: JSON output contains correct window metadata, fixture tests pass.

---

## Phase 4: CLI Surface

Commands to ship for this milestone:

- `screen tokenize --json` — token stream for focused window
- `screen tokenize --overlay <output.png>` — static overlay for any screenshot
- `screen tokenize --window <id> --json` — single window mode

Error codes unchanged from existing protocol: `TARGET_NOT_FOUND`, `AMBIGUOUS_TARGET`, `LOW_CONFIDENCE`.

---

## Phase 5: Live Overlay (Milestone Exit Verification)

After the Rust detector works on static screenshots, build the live overlay system:

- Transparent macOS `NSWindow` at screen-saver level, ignores mouse events
- Fed token bounds via IPC from the daemon
- Same color conventions as static overlay
- `desktopctl overlay start` / `stop`

Verification: open apps on the host machine — including apps not present in the VM — and confirm the overlay renders sensible tokens in real time. This is the milestone exit gate.

Manual check gate: at least 6 live overlay screenshots (Calculator, Reminders, and 4 other apps) must be manually approved before the milestone is considered done.

---

## Release / CI gate

A release candidate passes only when all of the following are true:

1. `cargo test -p desktopctld` passes
2. Golden fixture tests pass for the full golden set (light + dark)
3. Token JSON is deterministic: two consecutive runs on the same screenshot produce identical output
4. vm-smoke runs with `VM_SKIP_HOST_BUILD=1` and app-cleanup enabled; pass rate is 100%
5. Host `DesktopCtl.app` binary hash is identical before and after the full CI run — confirms no host rebuild occurred
6. Overlay spot-check set (minimum 6 images) is manually approved

---

## Phase 6: Debug Milestone (Deferred)

After the tokenize milestone ships, add operational observability:

- Trace bundle per run: raw outputs per stage, separate overlays per stage, decisions log
- `screen tokenize --explain` human-readable summary
- Failure taxonomy (OCR_MISS, BOX_OVEREXPANDED, GLYPH_FALSE_POSITIVE, etc.)
- Golden regression tests wired into CI

Deferred because fixture tests against static screenshots provide sufficient feedback during development. Trace tooling becomes necessary when debugging failures against live, unknown apps.

---

## Sequencing summary

| Phase | Work |
|-------|------|
| 0 | Environment lock — baseline check, run folder, CLI health |
| 1 | VM corpus capture (all stock apps, 300–500 screenshots) + transfer to host |
| 2 | Model benchmark → labeling → AI verification (accept/needs_human_review) → human spot-check → golden set |
| 3 | Rust: schema → OCR → text → padded boxes → overlay → fixture tests → edge boxes → merge → glyphs → window metadata → expand corpus |
| 4 | CLI surface |
| 5 | Live overlay (milestone exit, 6-image manual approval) |
| 6 | Debug milestone (post-ship) |
