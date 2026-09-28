#!/usr/bin/env python3
"""Add one reminder using the supported DesktopCtl CLI operations."""

from __future__ import annotations

import argparse
import json
import logging
import sys
from pathlib import Path
from typing import Any, Sequence

AGENT_ROOT = Path(__file__).resolve().parents[3]
if str(AGENT_ROOT) not in sys.path:
    sys.path.insert(0, str(AGENT_ROOT))

from procedure_sdk import DesktopCtl, DesktopCtlError, window_id


def _payload(response: Any) -> dict[str, Any]:
    if isinstance(response, dict) and isinstance(response.get("result"), dict):
        return response["result"]
    return response if isinstance(response, dict) else {}


def _elements(response: Any) -> list[dict[str, Any]]:
    payload = _payload(response)
    values = payload.get("elements", [])
    elements = [item for item in values if isinstance(item, dict)] if isinstance(values, list) else []
    windows = payload.get("windows", [])
    if isinstance(windows, list):
        for window in windows:
            if isinstance(window, dict) and isinstance(window.get("elements"), list):
                elements.extend(item for item in window["elements"] if isinstance(item, dict))
    return elements


def _is_reminders(response: Any) -> bool:
    windows = _payload(response).get("windows", [])
    return isinstance(windows, list) and any(
        isinstance(item, dict) and "reminders" in str(item.get("app") or "").casefold()
        for item in windows
    )


def _editable_ids(response: Any) -> set[str]:
    editable = ("textfield", "textview", "textinput", "textbox", "text field")
    return {
        str(item["id"])
        for item in _elements(response)
        if item.get("id") and any(token in str(item.get("type", "")).casefold() for token in editable)
    }


def _contains_text(response: Any, text: str) -> bool:
    return any(text in str(item.get("text", "")) for item in _elements(response))


def run(text: str, client: Any) -> dict[str, Any]:
    opened = client.command("app", "open", "Reminders", "--wait")
    target = window_id(opened)

    before = client.command("screen", "tokenize", window_id=target)
    if not _is_reminders(before):
        raise DesktopCtlError(("screen", "tokenize"), "target window is not Reminders", code="WRONG_APPLICATION")
    client.command("keyboard", "press", "cmd+n", window_id=target)
    editor = client.command("screen", "tokenize", window_id=target)
    if not _is_reminders(editor):
        raise DesktopCtlError(("screen", "tokenize"), "Reminders editor window was lost", code="EXPECTED_EDITOR_NOT_FOUND")
    after_editors = _editable_ids(editor)
    editor_hint = any(
        marker in str(item.get("text", "")).casefold()
        for item in _elements(editor)
        for marker in ("new reminder", "reminder title")
    )
    if not (after_editors or editor_hint):
        raise DesktopCtlError(("keyboard", "press", "cmd+n"), "new reminder editor was not detected", code="EXPECTED_EDITOR_NOT_FOUND")

    client.command("keyboard", "type", text, window_id=target)
    client.command("keyboard", "press", "enter", window_id=target)
    final = client.command("screen", "tokenize", window_id=target)
    if not _is_reminders(final) or not _contains_text(final, text):
        raise DesktopCtlError(("screen", "tokenize"), "reminder text was not visible after submission", code="REMINDER_NOT_VERIFIED")
    return {"ok": True, "text": text}


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--text", required=True, help="reminder text to add")
    return result


def main(argv: Sequence[str] | None = None, client: Any | None = None) -> int:
    try:
        args = parser().parse_args(argv)
        result = run(args.text, client or DesktopCtl())
        status = 0
    except DesktopCtlError as error:
        logging.error("%s", error)
        result = {"ok": False, "error": error.code, "recoverable": error.recoverable}
        status = 1
    except SystemExit as error:
        # argparse owns usage formatting; keep procedure result stdout JSON-only.
        if error.code == 0:
            return 0
        result = {"ok": False, "error": "INVALID_ARGUMENTS", "recoverable": False}
        status = 2
    except Exception:
        logging.exception("Unexpected error adding reminder")
        result = {"ok": False, "error": "UNEXPECTED_ERROR", "recoverable": False}
        status = 1
    print(json.dumps(result, ensure_ascii=False, separators=(",", ":")))
    return status


if __name__ == "__main__":
    logging.basicConfig(stream=sys.stderr, level=logging.WARNING, format="%(levelname)s: %(message)s")
    raise SystemExit(main())
