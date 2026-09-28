import importlib.util
import logging
import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from procedure_sdk import DesktopCtl, DesktopCtlError

ADD_PATH = ROOT / "skills/reminders/scripts/add.py"
spec = importlib.util.spec_from_file_location("reminders_add", ADD_PATH)
add = importlib.util.module_from_spec(spec)
assert spec and spec.loader
spec.loader.exec_module(add)


class DesktopCtlTests(unittest.TestCase):
    def setUp(self):
        self.client = DesktopCtl(executable="desktopctl-test", timeout=7)

    @patch("procedure_sdk.desktopctl.subprocess.run")
    def test_json_command_and_window_target(self, run):
        run.return_value = subprocess.CompletedProcess([], 0, '{"ok":true,"result":{"window_id":"w1"}}', "")
        result = self.client.command("screen", "tokenize", window_id="w1")
        self.assertEqual(result["result"]["window_id"], "w1")
        args, kwargs = run.call_args
        self.assertEqual(args[0], ["desktopctl-test", "screen", "tokenize", "--active-window", "w1", "--json"])
        self.assertTrue(kwargs["capture_output"])
        self.assertEqual(kwargs["timeout"], 7)
        self.assertNotIn("shell", kwargs)

    @patch("procedure_sdk.desktopctl.subprocess.run")
    def test_plain_text_output(self, run):
        run.return_value = subprocess.CompletedProcess([], 0, "done\n", "")
        self.assertEqual(self.client.command("help", json_output=False), "done\n")

    @patch("procedure_sdk.desktopctl.subprocess.run")
    def test_nonzero_exit_includes_stderr_and_exit_code(self, run):
        run.return_value = subprocess.CompletedProcess([], 3, '{"ok":false,"error":{"code":"BAD","message":"bad request","retryable":false}}', "use valid target")
        with self.assertRaises(DesktopCtlError) as raised:
            self.client.command("keyboard", "type", "x")
        self.assertEqual(raised.exception.exit_code, 3)
        self.assertEqual(raised.exception.code, "BAD")
        self.assertFalse(raised.exception.recoverable)
        self.assertIn("use valid target", str(raised.exception))
        self.assertIn("keyboard type x", str(raised.exception))

    @patch("procedure_sdk.desktopctl.subprocess.run", side_effect=subprocess.TimeoutExpired(["desktopctl"], 7, stderr=b"slow"))
    def test_timeout_error(self, run):
        with self.assertRaises(DesktopCtlError) as raised:
            self.client.command("screen", "tokenize")
        self.assertEqual(raised.exception.code, "DESKTOPCTL_TIMEOUT")
        self.assertEqual(raised.exception.stderr, "slow")

    @patch("procedure_sdk.desktopctl.subprocess.run")
    def test_stderr_is_logged_and_not_mixed_into_json_result(self, run):
        run.return_value = subprocess.CompletedProcess([], 0, '{"ok":true}', "diagnostic\n")
        with self.assertLogs("procedure_sdk.desktopctl", level=logging.DEBUG) as logs:
            result = self.client.command("app", "open", "Reminders")
        self.assertEqual(result, {"ok": True})
        self.assertIn("diagnostic", " ".join(logs.output))


class RemindersProcedureTests(unittest.TestCase):
    def response(self, app="Reminders", elements=()):
        return {"ok": True, "result": {"windows": [{"app": app, "elements": list(elements)}]}}

    def test_argument_parsing(self):
        self.assertEqual(add.parser().parse_args(["--text", "Buy milk"]).text, "Buy milk")

    def test_sequences_commands_and_passes_saved_window_id(self):
        class FakeClient:
            def __init__(self):
                self.calls = []

            def command(self, *args, window_id=None, **kwargs):
                self.calls.append((args, window_id))
                if args[:3] == ("app", "open", "Reminders"):
                    return {"ok": True, "result": {"window_id": "rem_1"}}
                if args[:2] == ("screen", "tokenize"):
                    count = sum(call[0][:2] == ("screen", "tokenize") for call in self.calls)
                    if count == 1:
                        return add_state.initial
                    if count == 2:
                        return add_state.editor
                    return add_state.final
                return {"ok": True, "result": {}}

        add_state = type("Responses", (), {})()
        add_state.initial = self.response(elements=[{"id": "old", "type": "TextField"}])
        add_state.editor = self.response(elements=[{"id": "old", "type": "TextField"}, {"id": "new", "type": "TextField"}])
        add_state.final = self.response(elements=[{"id": "new", "type": "TextField", "text": "Buy milk"}])
        fake = FakeClient()
        self.assertEqual(add.run("Buy milk", fake), {"ok": True, "text": "Buy milk"})
        self.assertEqual([call[0][:2] for call in fake.calls], [("app", "open"), ("screen", "tokenize"), ("keyboard", "press"), ("screen", "tokenize"), ("keyboard", "type"), ("keyboard", "press"), ("screen", "tokenize")])
        self.assertTrue(all(call[1] == "rem_1" for call in fake.calls[1:]))

    def test_wrong_window_fails_before_mutation(self):
        class FakeClient:
            def __init__(self): self.calls = 0
            def command(self, *args, **kwargs):
                self.calls += 1
                if args[:2] == ("app", "open"):
                    return {"result": {"window_id": "other"}}
                return self.response
            response = self_response = None
        fake = FakeClient()
        fake.response = self.response("Settings")
        with self.assertRaises(DesktopCtlError) as raised:
            add.run("Buy milk", fake)
        self.assertEqual(raised.exception.code, "WRONG_APPLICATION")
        self.assertEqual(fake.calls, 2)

    def test_procedure_and_sdk_syntax_compile(self):
        for path in (ADD_PATH, ROOT / "procedure_sdk/desktopctl.py", ROOT / "procedure_sdk/__init__.py"):
            compile(path.read_text(), str(path), "exec")


if __name__ == "__main__":
    unittest.main()
