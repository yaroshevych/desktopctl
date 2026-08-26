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
- `agent_runner` defines the adapter boundary and implements `PiRunner`. Runs
  happen on worker threads and completion is dispatched back to AppKit's main
  thread. One run per DesktopCtl session is permitted at a time.

While a session is running, its view shows a native activity spinner and a
`Stop` button. The composer remains disabled until Pi finishes. Stopping sets
the request's cancellation token; the runner kills and reaps the Pi child, then
persists the session as cancelled.

After Pi has produced a native session identity, the session view also offers
`Open in Terminal`. DesktopCtl activates Apple Terminal and starts interactive
Pi with `--session <path|id>` from the original Pi working directory. The
executable, directory, and session arguments are POSIX-quoted. macOS may ask the
user to allow DesktopCtl to control Terminal the first time this is used.

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
system instruction is separate from the user message and only tells Pi to use
the current topmost non-DesktopCtl window with `--active-window`.

`Agent Launcher…` is the first menu-bar menu item. While one or more Pi requests
are running, the tray icon animates through rotated native-symbol frames and
returns to the normal idle/overlay icon when the last request finishes.

## Persistence

Launcher metadata is stored atomically as JSON at:

```text
~/Library/Application Support/DesktopCtl/agent-sessions.json
```

`DESKTOPCTL_AGENT_DATA_DIR` overrides the directory for tests. The document
contains a schema version and a list of DesktopCtl sessions: UUID, adapter and
Pi-native session identity, title, short transcript, target-window metadata,
timestamps, status, and unread/visited state. A malformed file is quarantined
or ignored with a diagnostic. Sessions left running by a process crash become
failed during startup recovery.

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
operations should use the current topmost non-DesktopCtl window.
