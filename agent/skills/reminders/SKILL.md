# Reminders

## Add a reminder

Use this for a new reminder when the Reminders UI is available. It opens Reminders, binds subsequent commands to its window, checks for the editor, enters the requested text, and verifies that text appears.

```bash
python3 ./skills/reminders/scripts/add.py --text "Buy milk"
```

`--text` is required. The script prints one JSON object: success is `{"ok":true,"text":"..."}`; failure is `{"ok":false,"error":"CODE","recoverable":true|false}`. If it returns a recoverable error, or the UI clearly differs from the documented flow, continue with interactive DesktopCtl exploration. Do not repeatedly retry a failed procedure against a changed UI.
