# Agent launcher

DesktopCtl's normal macOS app process owns a compact agent launcher. `Option-Space`
is registered with Carbon's global-hotkey API, which does not add an
Accessibility permission requirement. The AppKit panel is created in the
existing accessory application and joins all Spaces; it is not a helper app or
terminal process.

## Architecture

- `app/src/launcher` owns the adapter-neutral session controller and the macOS
  AppKit panel. Before the panel becomes key it asks the resident service over IPC to bind
  the active non-DesktopCtl window using the same opaque window-reference logic
  used by `desktopctl --active-window`.
- `agent_sessions` contains the persisted UI model and state transitions. It
  stores only user prompts and final assistant text. Pi's session remains the
  authoritative full transcript.
- Each launcher session gets a private filesystem workspace at
  `<data-root>/workspaces/<session-guid>/`. Pi runs with that directory as its
  working directory, and `Open in Ghostty` reuses it.
- `agent_runner` defines the adapter boundary and implements `PiRunner`. Runs
  happen on worker threads and completion is dispatched back to AppKit's main
  thread. One run per DesktopCtl session is permitted at a time.

While a session is running, its view shows a native activity spinner and a
`Stop` button. The composer remains enabled: follow-ups typed while Pi is
working are shown as queued user bubbles and sent together after Pi finishes.
Stopping sets the request's cancellation token; the runner kills and reaps the
Pi process group on Unix, then persists the session as cancelled. Output pipes
are bounded (8 MiB stdout, 256 KiB stderr); exceeding either limit reports an
error. Reader shutdown is bounded even when a descendant keeps a pipe open.

After Pi has produced a native session identity, the session view also offers
`Open in Ghostty`. DesktopCtl activates Ghostty, creates a new window (never a
tab), and starts interactive Pi with `--session <path|id>` from the original Pi
working directory. The executable and session arguments are POSIX-quoted. macOS
may ask the user to allow DesktopCtl to control Ghostty the first time this is
used.
When that session is opened in the launcher again, DesktopCtl reads Pi's native
JSONL session, follows its active branch, and refreshes the short transcript
with Ghostty-added user messages and final assistant answers. Thinking, tool
calls, tool results, and incomplete or aborted assistant messages stay hidden.

Escape from a session returns to the launcher list. Escape from the launcher
list closes the overlay.

## Pi invocation

The runner locates Pi from `DESKTOPCTL_PI_PATH`, the process `PATH`, and common
GUI-install locations including `/opt/homebrew/bin/pi`, `/usr/local/bin/pi`,
`~/.local/bin/pi`, and `~/bin/pi`. Missing Pi is reported in the launcher; it is
never installed automatically.

Pi is invoked directly with an argument array in non-interactive JSON mode. No
shell is involved and user input is not interpolated into a command string. The
runner reads JSON Lines, records the native session identifier from the session
header, and displays only text blocks from the final assistant `message_end`.
Follow-ups pass the persisted native session identifier back to Pi. When the
launcher's Options menu has `Share window context` enabled (the default), the
appended system instruction is separate from the user message and contains the
captured topmost window's concise `--active-window <id>` selector plus a pointer
to the detailed snapshot file. Each capture is written to a new
`<session-workspace>/<timestamp>_<sequence>_<window_id>.md` file, which Pi can
read when needed; the detailed tokenized payload is not duplicated in the
prompt. With the option disabled, the request includes no target-window or
window-context prompt. Window contents are captured only after submission with
sharing enabled; opening the panel resolves window identity only.

Snapshots are limited to 1 MiB each, eight files / 8 MiB per workspace, and
24 hours of retention. Cleanup runs before a new capture and hourly for idle
sessions, including at startup. Cleanup recognizes launcher filenames and the
launcher markdown header; unrelated files and symlinks are left alone. Older
snapshot references in Pi history may therefore expire.

`Agent Launcher…` is the first menu-bar menu item. While one or more Pi requests
are running, DesktopCtl's aperture tray icon rotates and returns to the normal
idle/overlay icon when the last request finishes.

## Persistence

Launcher metadata uses atomic per-session JSON records under:

```text
${DESKTOPCTL_HOME:-${XDG_DATA_HOME:-$HOME/.local/share}/desktopctl}/workspaces/agent-sessions.records/
```

`DESKTOPCTL_HOME` overrides the complete DesktopCtl data root. Otherwise
`XDG_DATA_HOME/desktopctl` is used when set, followed by
`$HOME/.local/share/desktopctl`. The directory contains a version marker and
one record per DesktopCtl session: UUID, adapter and Pi-native session identity,
title, short transcript, target-window metadata, timestamps, status, and
unread/visited state. A single background writer coalesces session updates and
persists only changed session records outside the launcher state lock. The
legacy `agent-sessions.json` is read until the first write completes migration;
the migration marker is installed last, and the legacy file remains untouched
as a backup. Failed writes stay pending for a later flush retry.
History summaries update by changed session ID; expanded history loads in
batches of 50 rows. Session views send cumulative deltas from Swift's last
acknowledged message count. Transcript replacement forces a full reset. Swift
uses one parser and one replaceable pending snapshot, so bursts do not create
parallel parsing work.
Native transcripts use a bounded cache and parse appended JSONL records from
the last complete offset; truncation/replacement rebuilds the cache, and cyclic
parent links are rejected. A malformed file is left untouched and ignored with a
diagnostic. Sessions left running by a process crash become failed during
startup recovery.

The per-session directories next to this file are agent-visible working
directories; they are separate from Pi's native session database. If a
workspace is removed, the next follow-up or Ghostty launch recreates the empty
directory before proceeding.

`PiRunner` remains a reusable adapter and permits callers to omit a working
directory. Launcher paths must always use `with_current_dir` with the session
workspace; the launcher owns that invariant.

## Testing

Startup reuses the successful service-readiness status and one settings snapshot
across launcher, tray, and any automatically opened permissions dialog. Later
settings opens/reloads still fetch current values. Repeated session opens share
an in-flight native transcript read; active launcher runs skip native sync, and
starting a follow-up invalidates any older read result.

With `DESKTOPCTL_TRACE=1` or `DESKTOPCTL_TRACE_PATH` set, each launcher request
also records `pi_spawned`, `pi_first_stdout_json`,
`pi_first_assistant_text_delta`, `pi_assistant_complete`, `pi_agent_end`,
`pi_process_exit`, `pi_output_drained`, and `pi_return`. Tool boundaries emit
`pi_tool_start` / `pi_tool_end`. Events share the request's `e2e id`; subtract
their `elapsed_ms` values to compare phases. If Pi supplies no text deltas,
the first-text marker falls back to assistant text at `message_end`. Phase
markers contain no message contents, tool names, or tool arguments. Normal
answer delivery remains unchanged.

Run focused launcher tests and the normal macOS gates:

```bash
cargo test -p desktopctld --manifest-path src/desktop/Cargo.toml agent_
just -f src/desktop/Justfile test-compile
just -f src/desktop/Justfile build
just -f src/desktop/Justfile release-gates
```

For a manual smoke test, focus an email or other app, press `Option-Space`, enter
`summarise this`, and close the panel while Pi runs. Confirm the native completion notification,
reopen the launcher, open the unread session, and send a follow-up. Pi's desktop
operations should use the captured topmost non-DesktopCtl window.

## SwiftUI launcher smoke test

Standalone Swift model regression checks (deltas and snapshot coalescing):

```bash
swiftc -D LAUNCHER_MODEL_TESTS -parse-as-library \
  src/desktop/app/ui-swift/LauncherTheme.swift \
  src/desktop/app/ui-swift/LauncherBridge.swift \
  src/desktop/app/ui-swift/LauncherBridgeTests.swift \
  -o /tmp/desktopctl-launcher-model-tests
/tmp/desktopctl-launcher-model-tests
```

Build and launch isolated test state. Direct binary launch preserves env vars;
`just run`/`open` does not provide a reliable env-var path for this test.

```bash
just -f src/desktop/Justfile build
TEST_ROOT="$(mktemp -d -t desktopctl-launcher)"
DESKTOPCTL_HOME="$TEST_ROOT/data" \
DESKTOPCTL_TRACE=1 \
DESKTOPCTL_TRACE_PATH="$TEST_ROOT/trace.log" \
  src/desktop/dist/DesktopCtl.app/Contents/MacOS/desktopctl-app \
  >"$TEST_ROOT/app.log" 2>&1 &
APP_PID=$!
```

Stop after testing: `kill "$APP_PID"`; remove `$TEST_ROOT` when notes are
captured.

Manual checks:

1. Focus TextEdit or Mail. Press `Option-Space`. Panel appears on active Space;
   DesktopCtl does not stay focused after `Escape`.
2. Type normal text. Press `Send`. Confirm panel hides and trace shows the
   request path. Reopen; recent SwiftUI task rows should show title, preview,
   status, and unread state. Open one and confirm Rust receives its session ID.
3. Check keyboard selection, history expansion, session view, follow-up, and
   the native macOS completion notification. On the first notification, macOS
   may ask for notification permission; allow it before checking the banner.
4. Repeat on another Space and a full-screen app. Check panel placement,
   dismissal, and prior-app focus restoration.
5. Select Japanese/Hiragana (or another IME), type marked text, then press
   `Return`. Record whether marked text stays in composition; do not count a
   submit during composition as a pass.

Safe measurements:

```bash
# Live process memory; RSS is KB.
while kill -0 "$APP_PID" 2>/dev/null; do
  ps -o pid,rss,vsz,etime,command -p "$APP_PID"
  sleep 2
done

# Built bundle size.
du -sh src/desktop/dist/DesktopCtl.app

# App/controller trace (Unix-ms timestamps).
tail -f "$TEST_ROOT/trace.log"
```

Each launcher run also emits correlated `e2e` lines with a monotonic
`elapsed_ms` value. After a manual run, filter them with:

```bash
rg 'e2e id=' "$TEST_ROOT/trace.log"
```

The markers cover hotkey receipt, launcher panel visibility, active-window
resolution, window capture/tokenization, Pi launch and response, session
persistence, and completion notification submission. Enable the trace with
`DESKTOPCTL_TRACE=1` as shown above.

The `e2e` markers make hotkey-to-visible latency directly measurable. Record
cold launch, warm `Option-Space` -> visible, first-responder readiness, idle
RSS, and visible RSS when comparing runs.
