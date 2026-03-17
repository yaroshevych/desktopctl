# Tokenize Plan of Attack (Focused, Autonomous)

## Scope for this pass

- Goal: ship a reliable non-semantic `tokenize` slice (`text`, `box`, `glyph`) with reproducible debug artifacts.
- Focus app set in VM: Calculator, Reminders, System Settings (Privacy panes).
- Mandatory constraint: **do not rebuild/update host `/Applications/DesktopCtl.app`**.
- Dependency rule: live overlay/debug is prioritized, but goes active only after labels + working region detection exist.

## Execution guardrails

- Build/test only from source tree (`cargo test`, `cargo build`) and existing CLI binaries.
- Before every VM test case, close noisy apps to protect OCR quality.
- Every phase produces screenshots and overlays that are manually checked.

---

## Phase 0: Baseline + Environment Lock

### Deliverable

- Reproducible run folder and env vars for all later automation.

### Commands I will run

```bash
cd /Users/oleg/Projects/DesktopCtl/src/desktop
export VM_HOST="${VM_HOST:?set in env}"
export VM_USER="${VM_USER:?set in env}"
export VM_APP="/Users/${VM_USER}/DesktopCtl/dist/DesktopCtl.app"
export VM_CLI="/Users/${VM_USER}/DesktopCtl/dist/desktopctl"
export HOST_CLI="/Users/oleg/Projects/DesktopCtl/src/desktop/dist/desktopctl"
export RUN_ROOT="/tmp/desktopctl-tokenize-$(date +%Y%m%dT%H%M%S)"
mkdir -p "$RUN_ROOT"/{raw,labels,overlay,logs}
```

### Verification commands

```bash
"$HOST_CLI" doctor --json > "$RUN_ROOT/logs/host-doctor.json"
ssh "$VM_HOST" "DESKTOPCTL_APP_PATH='$VM_APP' '$VM_CLI' doctor --json" > "$RUN_ROOT/logs/vm-doctor.json"
```

### Screenshot/manual check

- Capture one host and one VM screenshot and confirm files exist and open.

```bash
"$HOST_CLI" screen capture --out "$RUN_ROOT/raw/host-baseline.png" --overlay
ssh "$VM_HOST" "DESKTOPCTL_APP_PATH='$VM_APP' '$VM_CLI' screen capture --out /tmp/dctl-baseline.png"
scp "$VM_HOST:/tmp/dctl-baseline.png" "$RUN_ROOT/raw/vm-baseline.png"
open "$RUN_ROOT/raw/host-baseline.png"
open "$RUN_ROOT/raw/vm-baseline.png"
```

---

## Phase 1: VM Screenshot Corpus (Focused)

### Deliverable

- 36-48 VM screenshots across Calculator/Reminders/System Settings in light+dark and key states.

### Commands I will run

1. Clean foreground apps before each case.

```bash
ssh "$VM_HOST" 'for app in TextEdit Calculator Reminders Notes Preview Safari "System Settings" Settings; do osascript -e "tell application \"$app\" to if it is running then quit saving no" >/dev/null 2>&1 || true; done; sleep 0.5'
ssh "$VM_HOST" 'for app in TextEdit Calculator Reminders Notes Preview Safari "System Settings" Settings; do pgrep -x "$app" >/dev/null && echo "still-running:$app" && exit 1 || true; done'
```

2. Switch appearance and capture per app/state.

```bash
# light mode
ssh "$VM_HOST" "osascript -e 'tell application \"System Events\" to tell appearance preferences to set dark mode to false'"

# Calculator
ssh "$VM_HOST" "open -a Calculator; sleep 1; DESKTOPCTL_APP_PATH='$VM_APP' '$VM_CLI' screen capture --out /tmp/tokenize-calculator-light.png"

# Reminders
ssh "$VM_HOST" "open -a Reminders; sleep 1; DESKTOPCTL_APP_PATH='$VM_APP' '$VM_CLI' screen capture --out /tmp/tokenize-reminders-light.png"

# System Settings deep link
ssh "$VM_HOST" "open 'x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility'; sleep 2; DESKTOPCTL_APP_PATH='$VM_APP' '$VM_CLI' screen capture --out /tmp/tokenize-settings-privacy-light.png"

# dark mode
ssh "$VM_HOST" "osascript -e 'tell application \"System Events\" to tell appearance preferences to set dark mode to true'"
ssh "$VM_HOST" "open -a Calculator; sleep 1; DESKTOPCTL_APP_PATH='$VM_APP' '$VM_CLI' screen capture --out /tmp/tokenize-calculator-dark.png"
ssh "$VM_HOST" "open -a Reminders; sleep 1; DESKTOPCTL_APP_PATH='$VM_APP' '$VM_CLI' screen capture --out /tmp/tokenize-reminders-dark.png"
ssh "$VM_HOST" "open 'x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility'; sleep 2; DESKTOPCTL_APP_PATH='$VM_APP' '$VM_CLI' screen capture --out /tmp/tokenize-settings-privacy-dark.png"
```

3. Copy corpus to host.

```bash
mkdir -p "$RUN_ROOT/raw/vm"
scp "$VM_HOST:/tmp/tokenize-*.png" "$RUN_ROOT/raw/vm/"
```

### Verification commands

```bash
ls -1 "$RUN_ROOT/raw/vm"/*.png | wc -l
```

### Screenshot/manual check

- Open all captured PNGs and visually confirm: target app is focused, no extra app windows polluting OCR.

```bash
open "$RUN_ROOT/raw/vm"
```

---

## Phase 2: Label Bootstrap (Auto + AI + Human Gate)

### Deliverable

- Primitive labels (`text|box|glyph`) + overlays for focused corpus.

### Commands I will run

```bash
cd /Users/oleg/Projects/DesktopCtl/src/desktop/scripts
uv sync

# existing detector baseline for screenshot geometry
uv run dev/generate_settings_window_data.py \
  "$RUN_ROOT/raw/vm" \
  --write-overlays \
  --output-dir "$RUN_ROOT/labels/auto"

# new script to produce tokenize primitive labels (to be implemented)
uv run dev/tokenize_label_corpus.py \
  --input "$RUN_ROOT/raw/vm" \
  --output "$RUN_ROOT/labels/auto" \
  --write-overlays

# non-interactive AI verification (to be implemented)
uv run dev/tokenize_ai_verify.py \
  --input "$RUN_ROOT/labels/auto" \
  --output "$RUN_ROOT/labels/ai"
```

### Verification commands

```bash
find "$RUN_ROOT/labels/ai" -name '*.json' | wc -l
find "$RUN_ROOT/labels/ai" -name '*.overlay.png' | wc -l
```

### Screenshot/manual check

- Open 100% of `needs_human_review`; random sample 20% of `accept`.

```bash
open "$RUN_ROOT/labels/ai"
```

---

## Phase 3: Rust Region Detection + Token Output

### Deliverable

- `screen tokenize --json` emits stable primitive tokens for focused corpus.

### Commands I will run

```bash
cd /Users/oleg/Projects/DesktopCtl/src/desktop
cargo test -p desktopctld

# fixture run on screenshots
cargo test -p desktopctld --test settings_screenshot_labels -- --nocapture

# tokenize smoke in VM without host app rebuild
VM_SKIP_HOST_BUILD=1 VM_CLEAN_APPS_BETWEEN_TESTS=1 just -f Justfile vm-smoke "$VM_HOST" "$VM_USER" "UTM" 2
```

### Verification commands

```bash
"$HOST_CLI" screen tokenize --json > "$RUN_ROOT/logs/host-tokenize.json"
ssh "$VM_HOST" "DESKTOPCTL_APP_PATH='$VM_APP' '$VM_CLI' screen tokenize --json" > "$RUN_ROOT/logs/vm-tokenize.json"
jq '.tokens | length' "$RUN_ROOT/logs/host-tokenize.json" "$RUN_ROOT/logs/vm-tokenize.json"
```

### Screenshot/manual check

```bash
"$HOST_CLI" screen capture --out "$RUN_ROOT/overlay/host-tokenize.png" --overlay
open "$RUN_ROOT/overlay/host-tokenize.png"
```

---

## Phase 4: Live Overlay + Debug Surface (Front Priority, Post-Detector)

### Deliverable

- Real-time overlay and debug artifacts for fast tuning on host apps.

### Commands I will run

```bash
cd /Users/oleg/Projects/DesktopCtl/src/desktop
cargo test -p desktopctld overlay

# expected CLI after implementation
"$HOST_CLI" overlay start
"$HOST_CLI" screen tokenize --json > "$RUN_ROOT/logs/live-overlay-frame.json"
"$HOST_CLI" overlay stop
```

### Verification commands

```bash
"$HOST_CLI" debug snapshot > "$RUN_ROOT/logs/debug-snapshot.json"
```

### Screenshot/manual check

- With overlay running, open Calculator and Reminders on host and take screenshots proving boxes/text/glyphs align.

```bash
"$HOST_CLI" screen capture --out "$RUN_ROOT/overlay/live-calculator.png" --overlay
"$HOST_CLI" screen capture --out "$RUN_ROOT/overlay/live-reminders.png" --overlay
open "$RUN_ROOT/overlay"
```

---

## Labeling Rubric (Mandatory)

- `text`: OCR-backed readable text span; keep normalized text exactly.
- `box`: visible UI container/control boundary; choose tight box, avoid neighboring controls.
- `glyph`: small non-text visual mark (plus, circle, check-like, icon) that is visually distinct.
- Keep `text`/`glyph` inside `box` when both are present; do not dedupe away valid child tokens.
- Reject obvious OCR noise unless it is clearly UI-significant.
- If uncertain, prefer conservative smaller `box` and mark for review.

## AI Labeling QA Policy

- Stage 1: detector proposes JSON + overlay.
- Stage 2: AI verifier returns corrected JSON, verdict (`accept` or `needs_human_review`), and issue list.
- Stage 3: human review gates release labels.
- Human review coverage:
  - 100% of `needs_human_review`
  - random 20% of `accept`
- Escalation rule: if sampled error rate exceeds 5%, re-review full batch and update prompts/rubric.

## Regression / CI Gate

Release candidate passes only if all checks below pass:

1. `cargo test -p desktopctld` passes.
2. Golden fixture tests pass for focused corpus (light+dark for 3 apps).
3. `vm-smoke` pass rate is 100% with `VM_SKIP_HOST_BUILD=1` and app-cleanup enabled.
4. Token JSON is deterministic across two consecutive runs on same screenshot (no unstable ordering).
5. Overlay spot-check set (at least 6 images) is manually approved.

Recommended CI command block:

```bash
cd /Users/oleg/Projects/DesktopCtl/src/desktop
cargo test -p desktopctld
VM_SKIP_HOST_BUILD=1 VM_CLEAN_APPS_BETWEEN_TESTS=1 just -f Justfile vm-smoke "$VM_HOST" "$VM_USER" "UTM" 2
```
