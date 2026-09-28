"""Synchronous, deliberately small wrapper around the DesktopCtl CLI."""

from __future__ import annotations

import json
import logging
import os
import shlex
import shutil
import subprocess
from typing import Any, Sequence


class DesktopCtlError(RuntimeError):
    def __init__(
        self,
        command: Sequence[str],
        message: str,
        *,
        exit_code: int | None = None,
        stdout: str = "",
        stderr: str = "",
        code: str = "DESKTOPCTL_FAILED",
        recoverable: bool = True,
    ) -> None:
        self.command = tuple(command)
        self.exit_code = exit_code
        self.stdout = stdout
        self.stderr = stderr
        self.code = code
        self.recoverable = recoverable
        status = f" (exit {exit_code})" if exit_code is not None else ""
        detail = stderr.strip() or message.strip()
        super().__init__(f"desktopctl {' '.join(self.command)}{status}: {detail or code}")


class DesktopCtl:
    """Invoke existing DesktopCtl commands and decode their output."""

    def __init__(self, executable: str | None = None, timeout: float = 30) -> None:
        configured = (
            executable
            or os.environ.get("DESKTOPCTL_CLI_BIN")
            or os.environ.get("DESKTOPCTL_PATH")
        )
        self.executable = configured or shutil.which("desktopctl") or "desktopctl"
        self.timeout = timeout

    def command(
        self,
        *args: str,
        window_id: str | None = None,
        timeout: float | None = None,
        json_output: bool = True,
    ) -> Any:
        command = [self.executable, *map(str, args)]
        if window_id:
            command.extend(("--active-window", window_id))
        if json_output:
            command.append("--json")
        logging.getLogger("procedure_sdk.desktopctl").debug("Running %s", shlex.join(command))
        try:
            completed = subprocess.run(
                command,
                capture_output=True,
                text=True,
                timeout=self.timeout if timeout is None else timeout,
                check=False,
            )
        except subprocess.TimeoutExpired as error:
            stdout = _as_text(error.stdout)
            stderr = _as_text(error.stderr)
            raise DesktopCtlError(
                command,
                f"command timed out after {error.timeout} seconds",
                stdout=stdout,
                stderr=stderr,
                code="DESKTOPCTL_TIMEOUT",
            ) from error
        stdout, stderr = completed.stdout, completed.stderr
        if completed.returncode != 0:
            message, code, recoverable = _error_details(stdout)
            raise DesktopCtlError(
                command,
                message or "DesktopCtl command failed",
                exit_code=completed.returncode,
                stdout=stdout,
                stderr=stderr,
                code=code,
                recoverable=recoverable,
            )
        if stderr:
            logging.getLogger("procedure_sdk.desktopctl").debug("desktopctl stderr: %s", stderr.rstrip())
        if not json_output:
            return stdout
        try:
            return json.loads(stdout)
        except json.JSONDecodeError:
            return stdout


def window_id(response: Any) -> str:
    """Extract a non-empty window_id from a DesktopCtl JSON envelope."""
    value = response
    if isinstance(value, dict) and isinstance(value.get("result"), dict):
        value = value["result"]
    if isinstance(value, dict) and isinstance(value.get("window_id"), str) and value["window_id"]:
        return value["window_id"]
    raise DesktopCtlError(("app", "open"), "response did not contain result.window_id", code="WINDOW_ID_MISSING")


def _as_text(value: str | bytes | None) -> str:
    return value.decode(errors="replace") if isinstance(value, bytes) else (value or "")


def _error_details(stdout: str) -> tuple[str, str, bool]:
    try:
        data = json.loads(stdout)
    except json.JSONDecodeError:
        return "", "DESKTOPCTL_FAILED", True
    error = data.get("error") if isinstance(data, dict) else None
    if not isinstance(error, dict):
        return "", "DESKTOPCTL_FAILED", True
    return (
        str(error.get("message", "")),
        str(error.get("code", "DESKTOPCTL_FAILED")),
        bool(error.get("retryable", error.get("recoverable", True))),
    )
