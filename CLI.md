# DesktopCtl CLI Reference

- Global output flags: `--markdown` (default human-readable output) and `--json` (machine-readable responses that include `request_id`)
- Daemon keeps a rolling artifact buffer for recent requests, incl. request, response, and screenshot
- Daemon executes at most one command at a time globally; concurrent requests wait in a short queue
- If queue wait exceeds ~5s, request fails with retryable `TIMEOUT` (exit code `3`).

## Observe Mode (Action Feedback)
```bash
# pointer/keyboard actions observe UI change by default
# disable when you need minimum latency
--observe                 # explicit enable (default)
--no-observe              # disable post-action observe loop
--observe-until <mode>    # stable | change | first-change
--observe-timeout <ms>    # observe loop hard timeout (default: 300)
```

## Fast Deterministic Pattern (Recommended)
```bash
# 1) open app and capture window id
raw="$(desktopctl app open Notes --json)"
win_id="$(printf '%s' "$raw" | jq -r '.result.window_id // empty')"

# 2) reuse --active-window <id> for all subsequent commands
desktopctl keyboard press cmd+f --active-window "$win_id" --no-observe
desktopctl keyboard type "Shopping list" --active-window "$win_id" --no-observe
desktopctl pointer click --text "All" --active-window "$win_id" --no-observe

# 3) validate final state
desktopctl screen tokenize --active-window "$win_id"
```

## Examples
```bash
desktopctl app open "Calculator" --wait
desktopctl window focus --title "Settings"
desktopctl screen tokenize --active-window 12345
desktopctl pointer click --id button_ok --active-window 12345
desktopctl keyboard type "hello"
desktopctl request response 12345
```

## App and Window
```bash
# open an app; optionally wait until it is ready or avoid activating it
desktopctl app open <application> [--wait] [--timeout <ms>] [--background] [-- <open-args...>]

# hide other visible apps and activate target app
desktopctl app isolate <application>

# hide an app if it is running
desktopctl app hide <application>

# show and activate an app
desktopctl app show <application>

# list visible windows
desktopctl window list

# find window bounds by title/app text or exact window id
desktopctl window bounds --title <text>
desktopctl window bounds --id <id>

# focus a matching window by title/app text or exact window id
desktopctl window focus --title <text>
desktopctl window focus --id <id>
```

## macOS Menus
```bash
# list native menus for the active window's application
desktopctl menu list --active-window [<window_id>]

# include the Apple/system menu (excluded by default)
desktopctl menu list --system --active-window [<window_id>]

# disable long-list truncation; combines independently with --system
desktopctl menu list --all --active-window [<window_id>]
desktopctl menu list --system --all --active-window [<window_id>]

# invoke an item using an id returned by menu list
desktopctl menu click --id <menu_id> --active-window [<window_id>]

# invoke an item using an exact flattened title path
desktopctl menu click "Edit > Find > Find…" --active-window [<window_id>]
```

Menu commands require `--active-window`. Its optional value is DesktopCtl's opaque guarded window reference, such as `safari_e51aeb`, not a numeric CGWindow ID.

Returned IDs appear as trailing tokens:

```markdown
### History #menu_history
  Show All History (cmd+y) #menu_history_show_all_history
```

Use `--id` when clicking an ID. A positional argument is interpreted as a title path:

```bash
# correct
desktopctl menu click --id menu_history_show_all_history --active-window safari_e51aeb

# path lookup, not id lookup
desktopctl menu click "History > Show All History" --active-window safari_e51aeb
```

Default output:

- Excludes the first structural Apple/system menu; `--system` includes it.
- Omits separators.
- Truncates any submenu with more than 20 children to its first 15 and reports the omitted count. `--all` disables recursive truncation.
- Labels non-interactive titled sections as groups. Group IDs are inspectable but clicking them returns `MENU_ACTION_UNSUPPORTED`.

JSON menu nodes include `id`, `title`, `role`, `kind`, `enabled`, `action_supported`, `shortcut`, `mark`, `children`, `truncated`, and `omitted_count`. `kind` is `item`, `submenu`, or `group`.

Menu support uses macOS Accessibility only. Custom/non-native menus may expose incomplete data. No OCR fallback is attempted.

## Screen and OCR
```bash
# common flags for screen screenshot/tokenize:
# --active-window [<id>]    # target frontmost window (optionally enforce id)
# --region <x> <y> <w> <h>  # region relative to selected target

# take screenshot (display or active window)
desktopctl screen screenshot [--out <path>] [--overlay] [--region <x> <y> <width> <height>]

# tokenize current screen/window into structured OCR + UI elements
desktopctl screen tokenize [--overlay <path>] [--window-query <text>] [--screenshot <path>] [--region <x> <y> <width> <height>] [--all]
# tokenize response window `id` is an opaque window id; pass it back via --active-window <id> to enforce target window
# element ids are semantic and predictable (examples: button_7, button_add, text_settings)
# --all includes off-screen content exposed by the live AX tree; it is bounded to 2,000 nodes,
# depth 64, and 512 KiB of AX text, and cannot be combined with --screenshot or --region

# find text on screen via OCR
desktopctl screen find --text <text> [--all]

# wait for text to appear (default) or disappear (--disappear)
desktopctl screen wait --text <text> [--timeout <ms>] [--interval <ms>] [--disappear]
```

## Pointer and Keyboard
```bash
# common observe flags for pointer/keyboard actions:
# [--observe] [--no-observe]
# [--observe-until <stable|change|first-change>]
# [--observe-timeout <ms>] [--observe-settle-ms <ms>]
# [--active-window [<id>]]  # optional frontmost-window guard for all pointer actions

# move pointer
desktopctl pointer move <x> <y>

# press/release pointer button
desktopctl pointer down <x> <y>
desktopctl pointer up <x> <y>

# click pointer by coordinate, OCR text, or element id
desktopctl pointer click <x> <y> [--absolute]
desktopctl pointer click --text <text>
desktopctl pointer click --id <element_id> --active-window [<id>]

# scroll pointer viewport/content by signed deltas (positive dy scrolls down)
desktopctl pointer scroll <dx> <dy>
desktopctl pointer scroll --id <element_id> <dx> <dy>

# drag pointer between coordinates
desktopctl pointer drag <x1> <y1> <x2> <y2> [hold_ms]

# keyboard text and key/hotkey press
desktopctl keyboard type "text"
desktopctl keyboard press <key-or-hotkey>
```

## Clipboard
```bash
# clipboard operations
desktopctl clipboard read
desktopctl clipboard write <text>
```

## Debug
```bash
# report Accessibility / Screen Recording permission status
desktopctl debug permissions

# check daemon connectivity
desktopctl debug ping

# start/stop debug overlay
desktopctl debug overlay start [--duration <ms>]
desktopctl debug overlay stop

# write debug snapshot payload
desktopctl debug snapshot
```

## Safety
```bash
# disable GUI operations in the running daemon (e.g., app/window/screen/pointer/keyboard)
# non-GUI commands like `debug ping` continue to work
desktopctl disable
```

## Replay
```bash
# start replay recording (default duration: 3000ms)
# use only if you explicitly need trace capture/replay artifacts
desktopctl replay record

# start replay recording with explicit duration (max 1800000ms / 30m)
desktopctl replay record --duration <ms>

# stop active replay recording
desktopctl replay record --stop

# load replay session from disk
desktopctl replay load <session_dir>
```

## Request Artifacts
```bash
# show stored metadata for one request
desktopctl request show <request_id>

# list recent stored requests
desktopctl request list [--limit <n>]

# export stored screenshot for one request
desktopctl request screenshot <request_id> [--out <path>]

# return stored response envelope for one request
desktopctl request response <request_id>

# fuzzy search over stored tokenize responses
desktopctl request search <text> [--limit <n>] [--command <screen_tokenize|...>]
```

## Exit Codes
- `2`: `PERMISSION_DENIED`
- `3`: `TIMEOUT`
- `4`: `TARGET_NOT_FOUND`
- `5`: `INVALID_ARGUMENT`
- `6`: `DAEMON_NOT_RUNNING` or `BACKEND_UNAVAILABLE`
- `7`: `LOW_CONFIDENCE`
- `8`: `AMBIGUOUS_TARGET`
- `9`: `POSTCONDITION_FAILED`
- `10`: `INTERNAL`
