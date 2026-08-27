# Agent launcher

DesktopCtl's macOS menu-bar process owns a compact agent launcher. `Option-Space`
is registered with Carbon's global-hotkey API, which does not add an
Accessibility permission requirement. The AppKit panel is created in the
existing accessory application and joins all Spaces; it is not a helper app or
terminal process.

## Architecture

- `agent_launcher` owns the adapter-neutral session controller and the macOS
  AppKit panel. Before the panel becomes key it asks the resident daemon to bind
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
`Stop` button. The composer remains disabled until Pi finishes. Stopping sets
the request's cancellation token; the runner kills and reaps the Pi child, then
persists the session as cancelled.

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
Follow-ups pass the persisted native session identifier back to Pi. The appended
system instruction is separate from the user message and contains only the
captured topmost window's concise `--active-window <id>` selector.

`Agent Launcher…` is the first menu-bar menu item. While one or more Pi requests
are running, DesktopCtl's aperture tray icon rotates and returns to the normal
idle/overlay icon when the last request finishes.

## Persistence

Launcher metadata is stored atomically as JSON at:

```text
${DESKTOPCTL_HOME:-${XDG_DATA_HOME:-$HOME/.local/share}/desktopctl}/workspaces/agent-sessions.json
```

`DESKTOPCTL_HOME` overrides the complete DesktopCtl data root. Otherwise
`XDG_DATA_HOME/desktopctl` is used when set, followed by
`$HOME/.local/share/desktopctl`. The document contains a schema version and a
list of DesktopCtl sessions: UUID, adapter and Pi-native session identity,
title, short transcript, target-window metadata, timestamps, status, and
unread/visited state. A malformed file is left untouched and ignored with a
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

Run focused launcher tests and the normal macOS gates:

```bash
cargo test -p desktopctld --manifest-path src/desktop/Cargo.toml agent_
just -f src/desktop/Justfile test-compile
just -f src/desktop/Justfile build
just -f src/desktop/Justfile release-gates
```

For a manual smoke test, focus an email or other app, press `Option-Space`, enter
`summarise this`, and close the panel while Pi runs. Confirm the completion HUD,
reopen the launcher, open the unread session, and send a follow-up. Pi's desktop
operations should use the captured topmost non-DesktopCtl window.
