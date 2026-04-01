pub(crate) fn help_notes() -> &'static str {
    "Notes and hints:\n\
- tokenize response includes request_id in JSON output; reuse it with `desktopctl request response <request_id>`\n\
- for modal dialogs, get IDs via `desktopctl window list`; pass dialog id to act inside dialog, or parent id to act on main window\n\
- pointer scroll direction uses command deltas (`dy > 0` down, `dy < 0` up), independent of macOS natural/classic mode\n\
- to replace existing field content, send `desktopctl keyboard press cmd+a` before typing\n\
- for long lists, repeat scroll -> tokenize; save each request_id and inspect later via `desktopctl request response <request_id>`"
}
