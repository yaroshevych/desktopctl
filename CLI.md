# DesktopCtl CLI Reference

- Global `--json` CLI flag for machine-readable responses, which include `request_id`
- Daemon keeps a rolling artifact buffer for recent requests, incl. request, response, and screenshot

## Global Output Mode
```bash
# global machine-readable envelope mode (includes ok, request_id, result/error)
desktopctl --json <command...>
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

# find window bounds by title/app text
desktopctl window bounds --title <text>

# focus a matching window
desktopctl window focus --title <text>
```

## Screen and OCR
```bash
# take screenshot (display or active window)
desktopctl screen screenshot [--out <path>] [--overlay] [--active-window]

# tokenize current screen/window into structured OCR + UI elements
desktopctl screen tokenize [--overlay <path>] [--active-window] [--window <id>] [--screenshot <path>]

# find text on screen via OCR
desktopctl screen find --text <text> [--all]

# wait for text to appear (default) or disappear (--disappear)
desktopctl screen wait --text <text> [--timeout <ms>] [--interval <ms>] [--disappear]
```

## Pointer and Keyboard
```bash
# move pointer
desktopctl pointer move <x> <y>

# press/release pointer button
desktopctl pointer down <x> <y>
desktopctl pointer up <x> <y>

# click pointer by coordinate, OCR text, element id, or token
desktopctl pointer click <x> <y>
desktopctl pointer click --text <text>
desktopctl pointer click --id <element_id>
desktopctl pointer click --token <n>

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

# export stored screenshot for one request
desktopctl request screenshot <request_id> [--out <path>]

# return stored response envelope for one request
desktopctl request response <request_id>
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
