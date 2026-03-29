pub(crate) fn usage() -> &'static str {
    "usage:
  desktopctl app open <application> [--wait] [--timeout <ms>] [-- <open-args...>]
  desktopctl app hide <application>
  desktopctl app show <application>
  desktopctl app isolate <application>
  desktopctl window list
    hint: compact output with | jq '.result.windows[] | \"\\(.id) \\(.parent_id // \"-\") \\(.modal) \\(.visible) \\(.title)\"'
  desktopctl window bounds (--title <text> | --id <id>)
  desktopctl window focus (--title <text> | --id <id>)
    hint: when starting, the focused window likely belongs to AI agent, get its ID with tokenise command, then open/focus target window, and in the end of the session focus AI agent window again
    hint: after focusing, use --active-window <id> on subsequent commands to ensure they target the correct window
    hint: for modal dialogs, get IDs via `desktopctl window list`; pass dialog id to act inside dialog, or parent id to act on main window
  desktopctl screen screenshot [--out <path>] [--overlay] [--active-window [<id>]] [--region <x> <y> <width> <height>]
    note: --region is relative to the selected active-window/display target
    hint: prefer `screen tokenize` for automation flows; use screenshot as last resort for visual artifacts/debug
  desktopctl screen tokenize [--overlay <path>] [--active-window [<id>]] [--window-query <text>] [--screenshot <path>] [--region <x> <y> <width> <height>]
    note: --window-query cannot be combined with --screenshot
    note: --active-window cannot be combined with --window-query or --screenshot
    note: --region is relative to the selected window/screenshot target
    hint: tokenize response includes request_id in JSON output; reuse it with `desktopctl request response <request_id>`
    hint: compact output with | jq -r '.result.text_dump'
  desktopctl screen find --text <text> [--all]
  desktopctl screen wait --text <text> [--timeout <ms>] [--interval <ms>] [--disappear]
  desktopctl clipboard read
  desktopctl clipboard write <text>
  desktopctl debug permissions
  desktopctl debug ping
  desktopctl debug overlay start [--duration <ms>]
  desktopctl debug overlay stop
  desktopctl debug snapshot
  desktopctl request show <request_id>
  desktopctl request list [--limit <n>]
  desktopctl request screenshot <request_id> [--out <path>]
  desktopctl request response <request_id>
  desktopctl request search <text> [--limit <n>] [--command <screen_tokenize|...>]
    hint: use it to re-read output of previous commands, like tokenize, without perf penalty
  desktopctl replay record [--duration <ms>]
  desktopctl replay record --stop
  desktopctl replay load <session_dir>
  desktopctl pointer move [--absolute] <x> <y> [--active-window [<id>]]
    hint: use relative coordinates by default, it works better with tokenize
  desktopctl pointer down <x> <y> [--button <left|right>] [--active-window [<id>]]
    hint: include --active-window [<id>] to avoid acting in the wrong window (get id via window list or screen tokenize)
  desktopctl pointer up <x> <y> [--button <left|right>] [--active-window [<id>]]
  desktopctl pointer click [--absolute] [--button <left|right>] <x> <y> [--active-window [<id>]]
  desktopctl pointer click [--absolute] [--button <left|right>] [--observe|--no-observe] [--observe-until <stable|change|first-change>] [--observe-timeout <ms>] [--observe-settle-ms <ms>] [--active-window [<id>]] <x> <y>
    note: prefer pointer click x y when you have coordinates
  desktopctl pointer click --text <text> [--button <left|right>] [--active-window [<id>]] [--observe|--no-observe] [--observe-until <stable|change|first-change>] [--observe-timeout <ms>] [--observe-settle-ms <ms>]
    hint: use --button right to open context menus
  desktopctl pointer click --id <element_id> --active-window [<id>] [--button <left|right>] [--observe|--no-observe] [--observe-until <stable|change|first-change>] [--observe-timeout <ms>] [--observe-settle-ms <ms>]
    hint: id might change after screen update, either re-tokenize, or use x y for clicks
  desktopctl pointer scroll <dx> <dy> [--active-window [<id>]] [--observe|--no-observe] [--observe-until <stable|change|first-change>] [--observe-timeout <ms>] [--observe-settle-ms <ms>]
  desktopctl pointer scroll --id <element_id> <dx> <dy> [--active-window [<id>]] [--observe|--no-observe] [--observe-until <stable|change|first-change>] [--observe-timeout <ms>] [--observe-settle-ms <ms>]
    hint: before scroll, move pointer into the target scroll area
    hint: scroll direction uses command deltas (`dy > 0` down, `dy < 0` up), independent of macOS natural/classic mode
    hint: for long lists, repeat scroll -> tokenize; save each request_id and inspect later via `desktopctl request response <request_id>`
  desktopctl pointer drag <x1> <y1> <x2> <y2> [hold_ms] [--active-window [<id>]]
  desktopctl keyboard type \"text\" [--active-window [<id>]] [--observe|--no-observe] [--observe-until <stable|change|first-change>] [--observe-timeout <ms>] [--observe-settle-ms <ms>]
    hint: to replace existing field content, send `desktopctl keyboard press cmd+a` before typing
    hint: press enter, or click outside (app-dependent) of text area to apply your change
  desktopctl keyboard press <key-or-hotkey> [--active-window [<id>]] [--observe|--no-observe] [--observe-until <stable|change|first-change>] [--observe-timeout <ms>] [--observe-settle-ms <ms>]
    hint: common keys: delete, left/right/up/down, tab, home/end, pageup/pagedown, f1..f12 (hotkeys like cmd+left also supported)"
}
