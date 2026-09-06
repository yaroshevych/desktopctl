# Desktop app notes

This directory contains the macOS desktop app (`desktopctl-app`) plus its
SwiftUI/AppKit UI.

## Layout

- `src/main.rs` — binary entry point; delegates macOS startup to
  `runtime/macos_app.rs`.
- `src/runtime/` — tray app lifecycle, menu actions, settings/dialog launch,
  permissions, overlay handling.
- `src/launcher/` — agent launcher state, window/session control, Rust ↔ Swift
  launcher bridge.
- `src/service_client.rs` — client for the `desktopctld` Unix-socket service.
- `ui-swift/LauncherBridge.swift` — main launcher/session SwiftUI view and
  exported bridge entry points. Session bubbles, launcher controls, and
  keyboard-chip visibility live here.
- `ui-swift/LauncherTheme.swift` — shared visual tokens and macOS colors.
- `ui-swift/Models.swift` — Swift-side Codable UI models.
- `ui-swift/DaemonIPC.swift` — Swift dialog/client IPC to `desktopctld`.
- `ui-swift/DesktopCtlSettings.swift` — Settings window. Launcher tab is first;
  settings changes are saved live through `DaemonIPC`.
- `ui-swift/JournalDialog.swift`, `AppPolicyDialog.swift`,
  `SetupAccessDialog.swift` — standalone Swift dialogs.
- `ui-swift/main.swift` — dialog executable dispatcher. It reads JSON from
  stdin; first argument selects `journal`, `app-policy`, `setup-access`, or
  `settings`.

## Build model

`build.rs` compiles `LauncherTheme.swift` and `LauncherBridge.swift` into a
static Swift library linked into the Rust app. Other Swift dialogs are compiled
by the root `src/desktop/Justfile` into
`dist/DesktopCtl.app/Contents/MacOS/desktopctl-dialogs`.

Run from `src/desktop` after UI changes:

```sh
just run
```

This kills old app/daemon/dialog processes, removes `/tmp/desktopctl.sock`,
builds release binaries and Swift UI, then opens the app. `just build` only
builds; `just test-compile` and release gates are also defined in the root
Justfile.

## IPC

Requests use JSON envelopes over a Unix socket. Payload format:

1. 4-byte big-endian payload length
2. JSON request/response body

The normal socket is `/tmp/desktopctl.sock`; `DESKTOPCTL_SOCKET_PATH` can
override it. Swift settings currently send launcher preference updates
immediately, so the Settings window does not need to close or reload manually.

## UI conventions and recent behavior

- Launcher starts with the active window context. Opening a session keeps the
  same app context (for example, Safari) until selection changes.
- The Options menu's `Share window context` checkbox is enabled by default and
  applies to both launcher and session prompts. Disabling it sends the prompt
  without target-window or detailed DesktopCtl context.
- The Options menu's `Read-only mode` checkbox is off by default, applies to
  both launcher and session prompts, and is toggled with `R` while the menu is
  open (`Cmd-K`).
- Context button label is `Agent Context` when no window is selected; otherwise
  it shows the selected app/window name. Its icon is
  `macwindow.badge.plus`.
- Settings opens on the Launcher tab from both the menu and Cmd-comma.
  Repeated Cmd-comma activates the existing Settings window instead of making
  duplicates.
- Launcher controls expose the installed agents and Ghostty, Kitty, and
  Terminal as terminal choices.
- `render_keyboard_shortcuts` controls all shortcut keycap/chip visibility.
  Default is `true` for backward-compatible decoding.
- Session message bubbles are in `LauncherBridge.swift`. User and agent tails
  share the same geometry; user tails extend to the bottom-right and agent
  tails mirror them. Agent bubbles use opaque system gray.

## Change safety

Another agent may modify nearby files. Inspect `git status` and the focused diff
before editing. Stage only intended files. Remote git operations are human-only.

Known test issue: `golden_controls_have_expected_text_fields_and_buttons` is
currently failing; do not fix it unless explicitly requested.
