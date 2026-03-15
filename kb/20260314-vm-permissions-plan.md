# DesktopCtl VM Permission Plan

## Goal

Use one host command to:

1. Build DesktopCtl artifacts.
2. Copy them to the macOS VM.
3. Drive VM System Settings UI from the host (Accessibility + Screen Recording).
4. Verify basic VM DesktopCtl commands over SSH.

This document is aligned to the current `just vm-enable-permissions` recipe.

## Current Use Case (From `Justfile`)

Primary entrypoint:

```bash
just vm-enable-permissions [vm_host] [vm_user] [vm_window_app]
```

Inputs:

- `VM_HOST` (or arg): SSH target for the VM.
- `VM_USER` (or arg): VM username.
- `VM_WINDOW_APP`: host app name for the VM window (default `UTM`).
- `VM_OS_PASSWORD`: used for unlock prompts in VM Settings.
- `HOST_RETURN_APP`: optional host app to restore after automation.

Current 7-step flow:

1. Build host artifacts (`just build`).
2. Copy `DesktopCtl.app` + `desktopctl` to VM via `scp` and set executable bits.
3. Stop old daemon/app in VM.
4. Focus VM window on host and isolate host workspace to that window app.
5. Open VM Accessibility pane and run remove/add/enable flow for `DesktopCtl`.
6. Open VM Screen Recording pane and run the same flow, then handle `Quit & Reopen` or `Later`.
7. Verify in VM over SSH:
   - `permissions check`
   - `screen capture`
   - `screen snapshot --json`
   - `screen tokenize --json`

## Permission UI Flow (Per Pane)

The automation executes this pattern:

1. Wait for pane-specific anchor text.
2. Attempt to select existing `DesktopCtl` row.
3. Click `-` (best-effort cleanup).
4. Click `+` using fallback chain:
   - `ui click --settings-add`
   - `ui click --text-offset "No Items" ...`
   - `ui click --text-offset "Allow the applications" ...`
5. In file picker:
   - wait for `Open`
   - `cmd+shift+g`
   - type VM deploy directory
   - select `DesktopCtl.app`
   - click `Open`
6. Ensure row is enabled:
   - `ui settings enable DesktopCtl`
   - if locked: `ui settings unlock --password ...` then enable again

## Technical Approach

### Orchestration Model

- SSH is used for VM file operations, process control, pane opening, and verification commands.
- Host DesktopCtl performs all UI interactions on the visible VM surface.
- Host workspace is normalized (`app isolate`) before each interaction to reduce OCR noise.

### Vision and Clicking Strategy

- OCR-driven anchors: heading text, instruction text, row labels, and file-picker labels.
- Region heuristics:
  - detect window/content/list/footer areas from image regions and contrast edges
  - infer `+/-` controls using symbol pairing and anchor geometry
- Fallback ordering is explicit and deterministic (semantic click first, offsets last).

### Robustness Controls

- Per-step waits with explicit timeouts and hard failures.
- Unlock handling is integrated for password-gated Settings actions.
- Post-action verification (`ui settings enable`, `permissions check`) is mandatory.
- Tracing + overlay screenshots are available for debugging failed clicks/regions.

## Known Constraints

- Requires stable VM window size/position and display scaling.
- UI text and layout changes between macOS versions can shift offsets.
- This flow is UI-bound and cannot be fully headless in current macOS permission model.

## Recommended Next Improvements

1. Add explicit preflight calibration step (known anchor + known click confirmation).
2. Persist run artifacts in timestamped directories by default.
3. Replace hardcoded offset fallbacks with region-derived click targets only.
4. Split recipe internals into reusable scripts while preserving one top-level command.

## Exit Criteria

`just vm-enable-permissions` is considered successful when it can repeatedly:

- deploy a fresh build to VM
- enable both required permission panes without manual clicks
- pass VM-side basic verification commands
- provide enough logs/overlays to debug any failure in one run
