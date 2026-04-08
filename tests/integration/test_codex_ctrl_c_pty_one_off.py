"""One-off Windows PTY integration test for Ctrl-C handling in interactive Codex launches.

This test is intentionally opt-in because it requires:
- Windows
- pywinpty / ConPTY support
- a working `codex` installation
- launching the real interactive TUI

Run manually with:
    $env:CLUD_RUN_ONE_OFF_PTY_TEST='1'
    python -m pytest tests/integration/test_codex_ctrl_c_pty_one_off.py -q -s
"""

from __future__ import annotations

import os
import shutil
import sys
import threading
import time
import unittest
from pathlib import Path
from typing import Any, Protocol, cast

from winpty import PtyProcess  # type: ignore[import-not-found]

RUN_ONE_OFF_PTY_TEST = os.environ.get("CLUD_RUN_ONE_OFF_PTY_TEST") == "1"


class _WinPTYProcess(Protocol):
    def read(self, size: int = ...) -> str: ...
    def write(self, s: str) -> int: ...
    def isalive(self) -> bool: ...
    def terminate(self) -> None: ...


class _WinPTYSession:
    """Small PTY harness for one-off interactive integration tests."""

    def __init__(self, cmd: list[str], cwd: Path) -> None:
        spawn = cast(Any, PtyProcess).spawn
        self.proc = cast(_WinPTYProcess, spawn(cmd, dimensions=(40, 140), cwd=str(cwd)))
        self._chunks: list[str] = []
        self._lock = threading.Lock()
        self._closed = threading.Event()
        self._reader = threading.Thread(target=self._read_forever, daemon=True)
        self._reader.start()

    def _read_forever(self) -> None:
        try:
            while True:
                data = self.proc.read(4096)
                if data:
                    with self._lock:
                        self._chunks.append(data)
        except EOFError:
            pass
        except Exception as exc:
            with self._lock:
                self._chunks.append(f"\nREADERR:{exc!r}\n")
        finally:
            self._closed.set()

    def output(self) -> str:
        with self._lock:
            return "".join(self._chunks)

    def wait_for_text(self, marker: str, timeout: float) -> str:
        deadline = time.time() + timeout
        while time.time() < deadline:
            current = self.output()
            if marker in current:
                return current
            if self._closed.is_set():
                break
            time.sleep(0.05)
        raise AssertionError(f"Timed out waiting for marker {marker!r}.\nLast output:\n{self.output()[-4000:]}")

    def send_ctrl_c(self) -> None:
        self.proc.write("\x03")

    def send_ctrl_c_until_closed(self, max_presses: int, pause: float) -> int:
        presses = 0
        while presses < max_presses and not self._closed.is_set():
            self.send_ctrl_c()
            presses += 1
            if self._closed.wait(pause):
                break
        return presses

    def wait_closed(self, timeout: float) -> None:
        if not self._closed.wait(timeout):
            raise AssertionError(f"PTY session did not close within {timeout} seconds.\nLast output:\n{self.output()[-4000:]}")

    def is_alive(self) -> bool:
        try:
            return bool(self.proc.isalive())
        except Exception:
            return False

    def close(self) -> None:
        try:
            if self.is_alive():
                self.proc.terminate()
        except Exception:
            pass
        self._closed.wait(2)


@unittest.skipUnless(sys.platform == "win32", "Windows-only PTY integration test")
@unittest.skipUnless(RUN_ONE_OFF_PTY_TEST, "Set CLUD_RUN_ONE_OFF_PTY_TEST=1 to run this one-off PTY test")
@unittest.skipUnless(shutil.which("codex") is not None, "Codex CLI must be installed for PTY integration test")
class TestCodexCtrlCPtyOneOff(unittest.TestCase):
    """One-off end-to-end Ctrl-C checks against the real interactive Codex launcher."""

    def setUp(self) -> None:
        self.cwd = Path(__file__).resolve().parents[2]
        self.cmd = [sys.executable, "-m", "clud", "--session-model=codex"]

    def test_ctrl_c_during_launcher_startup_has_no_traceback(self) -> None:
        """Ctrl-C during the launch/banner phase should not show a KeyboardInterrupt traceback."""
        session = _WinPTYSession(self.cmd, self.cwd)
        try:
            session.wait_for_text("LAUNCHING CODEX", timeout=10)
            presses = session.send_ctrl_c_until_closed(max_presses=1, pause=2)
            session.wait_closed(timeout=15)

            output = session.output()
            self.assertEqual(presses, 1)
            self.assertFalse(session.is_alive(), "PTY child should exit after Ctrl-C during startup")
            self.assertNotIn("KeyboardInterrupt", output)
            self.assertNotIn("Traceback", output)
        finally:
            session.close()

    def test_ctrl_c_after_ui_is_live_has_no_traceback(self) -> None:
        """Ctrl-C after the Codex UI is visible should still exit cleanly."""
        session = _WinPTYSession(self.cmd, self.cwd)
        try:
            session.wait_for_text("OpenAI Codex", timeout=20)
            presses = session.send_ctrl_c_until_closed(max_presses=2, pause=2)
            session.wait_closed(timeout=15)

            output = session.output()
            self.assertGreaterEqual(presses, 1)
            self.assertLessEqual(presses, 2)
            self.assertFalse(session.is_alive(), "PTY child should exit after Ctrl-C with the UI live")
            self.assertNotIn("KeyboardInterrupt", output)
            self.assertNotIn("Traceback", output)
        finally:
            session.close()


if __name__ == "__main__":
    unittest.main()
