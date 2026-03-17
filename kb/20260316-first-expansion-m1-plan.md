# DesktopCtl M1: First Expansion After MVP

## Milestone Name

Reliable Automation Platform (M1)

## Milestone Goal

After each VM artifact update (without replacing host `DesktopCtl.app`), DesktopCtl can re-enable VM permissions automatically and pass a deterministic smoke suite without manual clicks. Failures produce enough evidence (`--explain`/trace artifacts) to debug in one run.

## Critical Constraint

Host-side VM automation depends on host-granted permissions for `DesktopCtl.app`.
Rebuilding/replacing host `DesktopCtl.app` during validation can reset trust and break the VM init flow.

Planning rule:

- keep host `DesktopCtl.app` stable during VM validation loops
- use skip-host-build / VM-only artifact updates for iteration
- verify host app binary hash remains unchanged across validation runs

## Definition of Done

M1 is complete when all items below are true:

- `just vm-enable-permissions` succeeds repeatedly after fresh builds.
- Window scoping is available and used by semantic actions.
- Semantic click uses hover-aware execution path by default.
- `desktopctl doctor` reports health, permissions, and remediation.
- Failed semantic actions can emit explainable candidate/ranking/postcondition data.
- CI (or local gate script) runs VM smoke checks and enforces pass-rate + latency thresholds.

## Scope

In scope:

- `window list`, `window bounds`, `window focus` (minimum viable window surface)
- Scope-aware `screen tokenize` and `ui click`
- Hover-aware semantic click default path
- `doctor`
- `--explain` + trace artifact wiring for semantic actions
- VM smoke gate and artifact retention

Out of scope for M1:

- Multi-monitor
- Full AX tree merge
- Embedding packs as required dependency
- Linux/Windows parity
- Streaming watch APIs

## Execution Checklist

## 0. Baseline + guardrails

- [ ] Freeze a baseline run command sequence:
  - [ ] `just -f src/desktop/Justfile build`
  - [ ] `just -f src/desktop/Justfile vm-enable-permissions`
  - [ ] VM smoke commands over SSH:
    - [ ] `desktopctl permissions check`
    - [ ] `desktopctl open TextEdit --wait`
    - [ ] `desktopctl screen tokenize --json`
    - [ ] `desktopctl ui click --text "New Document" --timeout 2000`
    - [ ] `desktopctl wait --text "New Document" --timeout 3000`
    - [ ] `desktopctl debug snapshot`
- [ ] Define artifact directory convention:
  - [ ] `/tmp/desktopctl-runs/<timestamp>/host`
  - [ ] `/tmp/desktopctl-runs/<timestamp>/vm`
- [ ] Ensure each run captures:
  - [ ] CLI JSON outputs
  - [ ] trace logs
  - [ ] debug snapshot PNG/JSON
  - [ ] pass/fail summary JSON

## 1. Window primitives

- [ ] Add protocol commands:
  - [ ] `window list`
  - [ ] `window bounds`
  - [ ] `window focus`
- [ ] Add CLI parsing and usage help entries.
- [ ] Add daemon handlers and structured JSON responses.
- [ ] Add tests:
  - [ ] parser tests
  - [ ] handler unit tests
  - [ ] contract shape tests

Acceptance:

- [ ] `desktopctl window list --json` returns visible windows with ids/titles/bounds.
- [ ] `desktopctl window bounds <id|title> --json` returns stable bounds.
- [ ] `desktopctl window focus <id|title>` can focus target window in VM smoke flow.

## 2. Scoped tokenization and actions

- [ ] Support scoping tokenization to active/focused window (M1 minimum).
- [ ] Ensure `ui click --text` defaults to active window search before global search.
- [ ] Preserve explicit global fallback if scoped search fails (with explain data).
- [ ] Add ambiguity/low-confidence assertions in tests.

Acceptance:

- [ ] On noisy desktops, scoped click success rate improves relative to baseline.
- [ ] `TARGET_NOT_FOUND`, `AMBIGUOUS_TARGET`, and `LOW_CONFIDENCE` remain stable.

## 3. Hover-aware semantic click

- [ ] Execution path:
  - [ ] pointer move to target
  - [ ] pre-hover dwell (configurable, default on)
  - [ ] actionability re-check (visible/enabled/stable)
  - [ ] click
  - [ ] postcondition verification
- [ ] Add config knobs (`config get/set`) for dwell and verification timeout.
- [ ] Add retries only within explicit policy bounds.

Acceptance:

- [ ] Click reliability in VM permissions UI improves, especially around dynamic controls.
- [ ] Failures return `POSTCONDITION_FAILED` with actionable details.

## 4. `doctor` command

- [ ] Add `desktopctl doctor` CLI command.
- [ ] Report:
  - [ ] daemon reachable
  - [ ] accessibility permission state
  - [ ] screen recording permission state
  - [ ] capture check
  - [ ] OCR/tokenization check
  - [ ] remediation instructions
- [ ] Add `--json` output mode for automation.

Acceptance:

- [ ] Fresh VM can run `doctor --json` and clearly indicate blocked vs healthy state.
- [ ] Output is machine-parseable and stable.

## 5. Explainability and trace bundles

- [ ] Add `--explain` flag for semantic actions (`ui click`, `wait --text` minimum).
- [ ] Include:
  - [ ] query and selector input
  - [ ] ranked candidates with scores
  - [ ] chosen/rejected reasons
  - [ ] postcondition checks + failure reason
  - [ ] artifact paths
- [ ] Persist per-failure bundle in run directory.

Acceptance:

- [ ] Any failed smoke step links to one bundle with enough data for root cause.

## 6. VM gate automation

- [ ] Add one top-level command/script for post-build validation:
  - [ ] build
  - [ ] permission enable
  - [ ] smoke suite
  - [ ] artifact collection
  - [ ] summary JSON + non-zero exit on failure
- [ ] Add loop mode for reliability measurement (example: positional arg: `... vm-smoke <host> <user> <window> 20`).
- [ ] Record pass rate, median latency, p95 latency for key commands.

Acceptance:

- [ ] Gate fails on regression automatically.
- [ ] Artifacts are retained per iteration.

## Implementation Plan

## Phase A (1-2 days): Gate first

Objective: make every run measurable before adding features.

Tasks:

1. Create post-build VM smoke script and artifact directories.
2. Add run summary JSON (step name, duration, status, artifact paths).
3. Wire command into `src/desktop/Justfile` as a single entrypoint.

Deliverables:

- Repeatable gate command
- Timestamped run artifacts
- Baseline metrics

## Phase B (2-4 days): Window-aware targeting

Objective: reduce false targets from global OCR.

Tasks:

1. Implement `window list/bounds/focus`.
2. Scope tokenization and semantic click to focused window first.
3. Add tests and fixture-based regressions.

Deliverables:

- New window command surface
- Improved click precision in VM scenarios

## Phase C (2-3 days): Execution reliability

Objective: convert semantic click from best-effort to deterministic action + verification.

Tasks:

1. Implement hover-aware default click path.
2. Add configurable dwell and verification timeouts.
3. Harden failure codes and retry policy behavior.

Deliverables:

- More stable click success in Settings/file picker flows
- Better postcondition failure reporting

## Phase D (1-2 days): Doctor + explain

Objective: minimize debug time for failures.

Tasks:

1. Implement `doctor` (`--json` first).
2. Implement `--explain` payload for `ui click` and `wait --text`.
3. Persist trace bundles referenced by command output.

Deliverables:

- One-command environment diagnosis
- One-run root-cause artifacts

## Phase E (1-2 days): Exit gate and lock contracts

Objective: freeze M1 behavior and stop regressions.

Tasks:

1. Run looped VM gate (`>=20` iterations recommended).
2. Set pass/fail thresholds.
3. Lock command JSON fields and document them.

Suggested thresholds:

- pass rate: `>= 95%` on 20-iteration loop
- `screen tokenize --json` p95: `<= 1500ms`
- `ui click --text` + verify p95: `<= 2500ms`
- zero unclassified `INTERNAL` errors in passing runs

## Work Breakdown (suggested order)

1. VM gate script + artifact schema
2. `window list/bounds/focus`
3. scoped tokenize + scoped click
4. hover-aware click
5. `doctor --json`
6. `--explain` + failure bundles
7. threshold tuning + milestone sign-off

## Risks and Mitigations

- VM UI variance breaks anchors: keep semantic-first targeting, offsets as last-resort fallback, and preserve artifacts.
- Performance regressions from extra verification: gate p95 metrics and tune dwell/retry defaults.
- Contract churn: freeze JSON response shapes before broad automation rollout.

## Milestone Sign-off Checklist

- [ ] All checklist sections complete
- [ ] 20-iteration VM gate run captured and archived
- [ ] M1 thresholds met
- [ ] No blocker-severity issues open for M1 scope
- [ ] Next milestone backlog created from observed failures

## Verification Commands I Will Run (Per Phase)

Conventions:

- Run from `src/desktop`.
- Use `./dist/desktopctl` to avoid PATH ambiguity.
- Do not rebuild/replace host `DesktopCtl.app` inside validation loops.
- Capture and compare host app hash before/after validation.

Shared preflight before each phase verification:

```bash
cd /Users/oleg/Projects/DesktopCtl/src/desktop
HOST_APP_BIN="/Applications/DesktopCtl.app/Contents/MacOS/desktopctld"
HOST_APP_SHA_BEFORE="$(shasum -a 256 "$HOST_APP_BIN" | awk '{print $1}')"
VM_SKIP_HOST_BUILD=1 just vm-enable-permissions "$VM_HOST" "$VM_USER" "$VM_WINDOW_APP"
./dist/desktopctl permissions check
```

### Phase A verification (Gate first)

```bash
cd /Users/oleg/Projects/DesktopCtl/src/desktop
just vm-smoke "$VM_HOST" "$VM_USER" "$VM_WINDOW_APP"
RUN_DIR="$(ls -td /tmp/desktopctl-runs/* | head -n 1)"
HOST_APP_SHA_AFTER="$(shasum -a 256 "$HOST_APP_BIN" | awk '{print $1}')"
test "$HOST_APP_SHA_BEFORE" = "$HOST_APP_SHA_AFTER"
echo "$RUN_DIR"
cat "$RUN_DIR/summary.json"
find "$RUN_DIR" -maxdepth 3 -type f | sort
```

Pass criteria:

- `just vm-smoke ...` exits `0`.
- `summary.json` reports all steps `ok`.
- Run directory contains host/vm outputs and debug artifacts.

### Phase B verification (Window-aware targeting)

```bash
cd /Users/oleg/Projects/DesktopCtl/src/desktop
./dist/desktopctl open Calculator --wait
./dist/desktopctl open Reminders --wait
./dist/desktopctl window list --json
./dist/desktopctl window bounds --title "Calculator" --json
./dist/desktopctl window bounds --title "Reminders" --json
./dist/desktopctl window focus --title "Calculator"
./dist/desktopctl screen tokenize --json
./dist/desktopctl window focus --title "Reminders"
./dist/desktopctl screen tokenize --json
```

Pass criteria:

- `window list` includes Calculator and Reminders.
- `window bounds` returns non-empty bounds for both.
- `window focus` changes active target reliably.
- Tokenization after focus is scoped to the focused window first.

### Phase C verification (Hover-aware click + verification loop)

```bash
cd /Users/oleg/Projects/DesktopCtl/src/desktop
./dist/desktopctl open Calculator --wait
./dist/desktopctl config set click.pre_hover_dwell_ms 120
./dist/desktopctl config get click.pre_hover_dwell_ms
./dist/desktopctl ui click --text "7" --timeout 2000
./dist/desktopctl ui click --text "+" --timeout 2000
./dist/desktopctl ui click --text "8" --timeout 2000
./dist/desktopctl ui click --text "=" --timeout 2000
./dist/desktopctl wait --text "15" --timeout 3000
```

Pass criteria:

- Click sequence succeeds without coordinate fallbacks.
- Postcondition (`15` visible) is confirmed.
- On induced failure, error code is `POSTCONDITION_FAILED` with details.

### Phase D verification (`doctor` + `--explain`)

```bash
cd /Users/oleg/Projects/DesktopCtl/src/desktop
./dist/desktopctl doctor --json
./dist/desktopctl ui click --text "__definitely_missing_target__" --timeout 800 --explain || true
./dist/desktopctl wait --text "__definitely_missing_target__" --timeout 800 --interval 100 --explain || true
RUN_DIR="$(ls -td /tmp/desktopctl-runs/* | head -n 1)"
find "$RUN_DIR" -maxdepth 4 -type f | rg "explain|bundle|trace|debug"
```

Pass criteria:

- `doctor --json` reports health and remediation fields.
- `--explain` failures include ranked candidates/reasons/artifact paths.
- Referenced bundle files exist on disk.

### Phase E verification (Looped gate + thresholds)

```bash
cd /Users/oleg/Projects/DesktopCtl/src/desktop
just vm-smoke "$VM_HOST" "$VM_USER" "$VM_WINDOW_APP" 20
RUN_DIR="$(ls -td /tmp/desktopctl-runs/* | head -n 1)"
cat "$RUN_DIR/aggregate.json"
cat "$RUN_DIR/summary.json"
```

Pass criteria:

- Exit code is `0` only if thresholds are met.
- Aggregate includes pass rate and latency percentiles.
- Thresholds match M1 targets (`>=95%` pass, p95 caps, zero unclassified `INTERNAL` in passing run).
