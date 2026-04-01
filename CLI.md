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

## App and Window
```bash
# open an app; optionally wait until it is ready
desktopctl app open <application> [--wait] [--timeout <ms>] [-- <open-args...>]

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

## Screen and OCR
```bash
# common flags for screen screenshot/tokenize:
# --active-window [<id>]    # target frontmost window (optionally enforce id)
# --region <x> <y> <w> <h>  # region relative to selected target

# take screenshot (display or active window)
desktopctl screen screenshot [--out <path>] [--overlay] [--region <x> <y> <width> <height>]

# tokenize current screen/window into structured OCR + UI elements
desktopctl screen tokenize [--overlay <path>] [--window-query <text>] [--screenshot <path>] [--region <x> <y> <width> <height>]
# tokenize response window `id` is an opaque window id; pass it back via --active-window <id> to enforce target window
# element ids are semantic and predictable (examples: button_7, button_add, text_settings)

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

## Replay
```bash
# start replay recording (default duration: 3000ms)
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
