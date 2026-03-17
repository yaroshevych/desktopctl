# DesktopCtl M1 Focused Slice

## Goal (Now)

Ship a narrow reliability slice that improves VM iteration speed immediately:

1. deterministic VM smoke gate with artifacts
2. window primitives (`list/bounds/focus`)
3. window-scoped tokenization and text click

## Critical Constraint: Host App Must Not Change

Host-side VM automation depends on already-granted host permissions for `DesktopCtl.app`.
Rebuilding or replacing host `DesktopCtl.app` can reset host Accessibility/Screen Recording trust and break VM init automation.

For this focused slice:

- do not rebuild/replace host `DesktopCtl.app` during verification loops
- use VM-only build/deploy paths that do not mutate host app bundle
- `vm-enable-permissions` flow must support a skip-host-build mode
- each run must verify host app binary hash is unchanged before/after

## Explicitly Deferred (Later)

- hover-aware click path
- `doctor`
- `--explain` bundles
- CI latency/pass-rate thresholds

Those stay in the full plan: `kb/20260316-first-expansion-m1-plan.md`.

## Focused Definition of Done

- one command runs build + VM permission enable + smoke checks and writes artifacts
- `window list/bounds/focus` is implemented and usable in VM tests
- `ui click --text` and `screen tokenize` are scoped to focused window first
- 10-run VM loop passes at `>=90%` (focused-slice gate only)

## Autonomous Execution Protocol (Required)

After each implementation step, I will run commands in this order:

1. build + VM permission enable
2. phase verification CLI commands
3. capture screenshots and debug snapshot artifacts
4. manually inspect the screenshots before moving to next step

Screenshot capture commands used at each step:

```bash
RUN_DIR="$(ls -td /tmp/desktopctl-runs/* | head -n 1)"
mkdir -p "$RUN_DIR/manual-checks"
./dist/desktopctl screen capture --out "$RUN_DIR/manual-checks/step.png" --overlay
./dist/desktopctl debug snapshot > "$RUN_DIR/manual-checks/debug_snapshot.json"
```

Manual screenshot verification checklist:

- focused app/window is the expected target
- key labels used by OCR targeting are visible
- click target area is visible and not occluded
- no obvious wrong-window interaction happened
- artifacts are readable and saved under the current run directory

## Focused Checklist

## Phase 1: Gate + artifacts

- [x] add `just vm-smoke` entrypoint
- [x] add skip-host-build support in VM flow (example: `VM_SKIP_HOST_BUILD=1`)
- [x] persist run artifacts under `/tmp/desktopctl-runs/<timestamp>/`
- [x] write `summary.json` with step status and durations
- [x] record `host_app_sha_before` and `host_app_sha_after` in summary
- [x] close noisy VM apps before and after each iteration, then verify they are closed

Verification commands I will run:

```bash
cd /Users/oleg/Projects/DesktopCtl/src/desktop
HOST_APP_BIN="/Applications/DesktopCtl.app/Contents/MacOS/desktopctld"
HOST_APP_SHA_BEFORE="$(shasum -a 256 "$HOST_APP_BIN" | awk '{print $1}')"
VM_SKIP_HOST_BUILD=1 just vm-enable-permissions "$VM_HOST" "$VM_USER" "$VM_WINDOW_APP"
just vm-smoke "$VM_HOST" "$VM_USER" "$VM_WINDOW_APP"
RUN_DIR="$(ls -td /tmp/desktopctl-runs/* | head -n 1)"
HOST_APP_SHA_AFTER="$(shasum -a 256 "$HOST_APP_BIN" | awk '{print $1}')"
test "$HOST_APP_SHA_BEFORE" = "$HOST_APP_SHA_AFTER"
./dist/desktopctl screen capture --out "$RUN_DIR/phase1-final.png" --overlay
./dist/desktopctl debug snapshot > "$RUN_DIR/phase1-debug.json"
cat "$RUN_DIR/summary.json"
find "$RUN_DIR" -maxdepth 3 -type f | sort
```

Manual checks I will perform:

- verify `phase1-final.png` shows the VM UI in expected stable state
- verify debug snapshot artifact exists and references recent state
- verify host app hash is unchanged across the run

## Phase 2: Window primitives

- [x] implement `window list --json`
- [x] implement `window bounds --title <text> --json`
- [x] implement `window focus --title <text>`
- [x] add parser + daemon + contract tests

Verification commands I will run:

```bash
cd /Users/oleg/Projects/DesktopCtl/src/desktop
./dist/desktopctl open Calculator --wait
./dist/desktopctl open Reminders --wait
RUN_DIR="$(ls -td /tmp/desktopctl-runs/* | head -n 1)"
mkdir -p "$RUN_DIR/phase2"
./dist/desktopctl window list --json
./dist/desktopctl window bounds --title "Calculator" --json
./dist/desktopctl window bounds --title "Reminders" --json
./dist/desktopctl window focus --title "Calculator"
./dist/desktopctl screen capture --out "$RUN_DIR/phase2/calculator-focus.png" --overlay
./dist/desktopctl window focus --title "Reminders"
./dist/desktopctl screen capture --out "$RUN_DIR/phase2/reminders-focus.png" --overlay
./dist/desktopctl debug snapshot > "$RUN_DIR/phase2/debug.json"
```

Manual checks I will perform:

- `calculator-focus.png` visibly shows Calculator as active target
- `reminders-focus.png` visibly shows Reminders as active target
- no cross-window misfocus is visible in captures

## Phase 3: Focused-window targeting

- [ ] scope `screen tokenize --json` to focused window first
- [ ] scope `ui click --text` to focused window first
- [ ] keep global fallback when scoped search misses
- [ ] preserve stable error codes for not-found/ambiguous/low-confidence

Verification commands I will run:

```bash
cd /Users/oleg/Projects/DesktopCtl/src/desktop
./dist/desktopctl open Calculator --wait
./dist/desktopctl open Reminders --wait
RUN_DIR="$(ls -td /tmp/desktopctl-runs/* | head -n 1)"
mkdir -p "$RUN_DIR/phase3"
./dist/desktopctl window focus --title "Calculator"
./dist/desktopctl screen tokenize --json
./dist/desktopctl ui click --text "7" --timeout 2000
./dist/desktopctl screen capture --out "$RUN_DIR/phase3/calculator-after-click.png" --overlay
./dist/desktopctl window focus --title "Reminders"
./dist/desktopctl screen tokenize --json
./dist/desktopctl ui click --text "New Reminder" --timeout 2000 || true
./dist/desktopctl screen capture --out "$RUN_DIR/phase3/reminders-after-click.png" --overlay
./dist/desktopctl debug snapshot > "$RUN_DIR/phase3/debug.json"
```

Manual checks I will perform:

- Calculator capture shows the click was applied to Calculator context
- Reminders capture shows targeting stayed in Reminders context
- if fallback was used, screenshot still matches intended app context

## Focused Exit Gate

- [ ] run 10-iteration loop and archive aggregate result

Verification commands I will run:

```bash
cd /Users/oleg/Projects/DesktopCtl/src/desktop
just vm-smoke "$VM_HOST" "$VM_USER" "$VM_WINDOW_APP" 10
RUN_DIR="$(ls -td /tmp/desktopctl-runs/* | head -n 1)"
find "$RUN_DIR" -type f | rg "\.png$"
cat "$RUN_DIR/aggregate.json"
```

Pass criteria:

- pass rate `>= 90%` on 10 iterations
- no repeated crash-level `INTERNAL` failures
- every failed step has artifact pointers in `summary.json`
