# Reusable DesktopCtl procedures

Application procedures are ordinary Python scripts stored in `skills/<app>/scripts/`. They use the standard-library wrapper in `procedure_sdk/desktopctl.py`; no shell command strings are needed.

## Reminders example

From a Hermes workspace:

```bash
python3 ./skills/reminders/scripts/add.py --text "Buy milk"
```

This is the real UI procedure and will add a reminder. For a non-mutating local check, run `python3 -m unittest discover -s <repo>/agent/tests -v` from any directory. Tests mock the SDK boundary.

## Notes learning check

1. Confirm `<DesktopCtl support>/agent/skills/notes/` does not exist.
2. Start a fresh agent and ask it to perform a simple Notes operation using the UI.
3. Wait for the existing 30-second idle follow-up session to inspect the successful trace.
4. Confirm it created `skills/notes/SKILL.md` and a parameterized script in `skills/notes/scripts/`.
5. Start another fresh agent and request the same operation with different input. It should read and run the procedure before UI exploration.
6. If the procedure returns a structured error, verify the agent falls back to interactive DesktopCtl use rather than repeatedly retrying it.

Do not repeat a mutating operation just to validate a generated procedure; the follow-up validates syntax and can use mocked checks.

## Hermes idle-review diagnostics and snapshots

Idle-review diagnostics are opt-in with `DESKTOPCTL_HERMES_REVIEW_LOG=1` and go to `<DesktopCtl data>/logs/hermes-idle-review.log` (normally `~/.local/share/desktopctl/logs/hermes-idle-review.log`). This small log records trigger, history source/count, workspace readiness, and completion/error without recording prompts or conversation contents. Set `DESKTOPCTL_HERMES_REVIEW_LOG_PATH` to choose another file. The much higher-volume DesktopCtl trace remains separately controlled by `DESKTOPCTL_TRACE`.

The prompt tests use two representative, sanitized Hermes-export-shaped JSONL snapshots in `src/desktop/app/testdata/hermes-review/`, plus expected transcript snapshots. To add one of your real sessions as a fixture, export it with `hermes sessions export --format jsonl --session-id <id> --redact /tmp/hermes-review.jsonl`, remove task-specific or identifying text, and copy the sanitized record into this directory. Update its expected `.transcript` and run:

```bash
cargo test -p desktop-app agent_runner::tests::hermes_review_session_snapshots_format_like_live_history --manifest-path src/desktop/Cargo.toml
```

These snapshots exercise history parsing, transcript formatting, and the follow-up prompt without calling Hermes or repeating UI operations.
