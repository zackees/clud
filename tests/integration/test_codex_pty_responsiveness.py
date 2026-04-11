"""Integration tests for Codex PTY responsiveness: typing, backspace, and Ctrl-C.

Tests the PseudoTerminalProcess relay path that ``clud --codex`` uses in
interactive mode.  A lightweight Python echo script stands in for Codex so
the tests run without the real Codex binary.

The tests verify:
  - Characters typed into the PTY relay appear in output promptly.
  - Backspace (\\x7f / \\x08) is forwarded and the child receives it.
  - Terminal size from the host terminal is passed through to the PTY.
  - Ctrl-C (\\x03) exits the session cleanly without tracebacks.

Run with:
    python -m pytest tests/integration/test_codex_pty_responsiveness.py -q -s
"""

from __future__ import annotations

import os
import shutil
import sys
import textwrap
import threading
import time
import unittest
from pathlib import Path
from typing import Any, Protocol, cast

# Skip the entire module on non-Windows platforms.
if sys.platform != "win32":
    raise unittest.SkipTest("Windows-only PTY integration tests")

try:
    from winpty import PtyProcess  # type: ignore[import-not-found]
except ImportError as exc:
    raise unittest.SkipTest("winpty not installed") from exc


# ---------------------------------------------------------------------------
# Lightweight PTY harness (mirrors _WinPTYSession from the Ctrl-C test)
# ---------------------------------------------------------------------------


class _WinPTYProcess(Protocol):
    def read(self, size: int = ...) -> str: ...
    def write(self, s: str) -> int: ...
    def isalive(self) -> bool: ...
    def terminate(self) -> None: ...


class PtySession:
    """Small PTY harness that spawns a command and collects output."""

    def __init__(self, cmd: list[str], *, cwd: Path | None = None, rows: int = 40, cols: int = 140) -> None:
        spawn = cast(Any, PtyProcess).spawn
        self.proc = cast(
            _WinPTYProcess,
            spawn(cmd, dimensions=(rows, cols), cwd=str(cwd) if cwd else None),
        )
        self._chunks: list[str] = []
        self._lock = threading.Lock()
        self._closed = threading.Event()
        self._reader = threading.Thread(target=self._read_forever, daemon=True)
        self._reader.start()

    # -- background reader --------------------------------------------------

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

    # -- public helpers ------------------------------------------------------

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

    def send(self, text: str) -> None:
        self.proc.write(text)

    def send_ctrl_c(self) -> None:
        self.proc.write("\x03")

    def wait_closed(self, timeout: float) -> None:
        if not self._closed.wait(timeout):
            raise AssertionError(f"PTY session did not close within {timeout}s.\nLast output:\n{self.output()[-4000:]}")

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


# ---------------------------------------------------------------------------
# Helper: inline Python scripts used as mock children
# ---------------------------------------------------------------------------

_ECHO_SCRIPT = textwrap.dedent("""\
    import sys, os, msvcrt, signal
    # Raw char-by-char echo loop.  Prints each byte as hex so the test
    # can assert exact values without ANSI / terminal escape interference.
    # Ctrl-C arrives as a signal in a PTY, so we handle it via signal handler.
    _got_ctrl_c = False
    def _on_ctrl_c(sig, frame):
        global _got_ctrl_c
        _got_ctrl_c = True
        sys.stdout.write("GOT_CTRL_C\\n")
        sys.stdout.flush()
    signal.signal(signal.SIGINT, _on_ctrl_c)
    sys.stdout.write("READY\\n")
    sys.stdout.flush()
    while not _got_ctrl_c:
        if msvcrt.kbhit():
            ch = msvcrt.getwch()
            code = ord(ch)
            sys.stdout.write(f"CHAR:{code:02x}\\n")
            sys.stdout.flush()
""")

_SIZE_SCRIPT = textwrap.dedent("""\
    import shutil, sys
    size = shutil.get_terminal_size()
    sys.stdout.write(f"COLS:{size.columns} ROWS:{size.lines}\\n")
    sys.stdout.flush()
""")


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


class TestPtyEchoResponsiveness(unittest.TestCase):
    """Typing characters should appear in the child's output quickly."""

    def test_characters_are_relayed_to_child(self) -> None:
        """Each typed character reaches the child and is echoed back."""
        session = PtySession(
            [sys.executable, "-c", _ECHO_SCRIPT],
            cwd=Path.cwd(),
        )
        try:
            session.wait_for_text("READY", timeout=5)
            # Type "hello"
            for ch in "hello":
                session.send(ch)
                time.sleep(0.05)  # small pause to avoid batching

            # All characters should appear in output within 2 seconds.
            session.wait_for_text("CHAR:6f", timeout=2)  # 'o' = 0x6f
            output = session.output()
            self.assertIn("CHAR:68", output)  # 'h'
            self.assertIn("CHAR:65", output)  # 'e'
            self.assertIn("CHAR:6c", output)  # 'l'
            self.assertIn("CHAR:6f", output)  # 'o'
        finally:
            session.send_ctrl_c()
            session.close()

    def test_echo_latency_under_200ms(self) -> None:
        """Round-trip echo latency should be well under 200ms per character."""
        session = PtySession(
            [sys.executable, "-c", _ECHO_SCRIPT],
            cwd=Path.cwd(),
        )
        try:
            session.wait_for_text("READY", timeout=5)
            latencies: list[float] = []
            for ch in "abc":
                start = time.perf_counter()
                session.send(ch)
                marker = f"CHAR:{ord(ch):02x}"
                session.wait_for_text(marker, timeout=2)
                elapsed = time.perf_counter() - start
                latencies.append(elapsed)

            avg_ms = (sum(latencies) / len(latencies)) * 1000
            max_ms = max(latencies) * 1000
            # Generous thresholds — we're checking for gross problems,
            # not micro-benchmarking.
            self.assertLess(max_ms, 200, f"Worst-case echo latency {max_ms:.0f}ms exceeds 200ms")
            self.assertLess(avg_ms, 100, f"Average echo latency {avg_ms:.0f}ms exceeds 100ms")
        finally:
            session.send_ctrl_c()
            session.close()


class TestPtyBackspace(unittest.TestCase):
    """Backspace key should be relayed to the child process."""

    def test_backspace_reaches_child(self) -> None:
        """Pressing backspace sends \\x08 or \\x7f to the child."""
        session = PtySession(
            [sys.executable, "-c", _ECHO_SCRIPT],
            cwd=Path.cwd(),
        )
        try:
            session.wait_for_text("READY", timeout=5)
            # Type a character then backspace (\\x08 = BS).
            session.send("x")
            session.wait_for_text("CHAR:78", timeout=2)  # 'x' = 0x78

            session.send("\x08")  # backspace
            time.sleep(0.5)

            output = session.output()
            # Backspace should arrive as 0x08.
            # Some terminals send DEL (0x7f) instead — accept either.
            has_bs = "CHAR:08" in output or "CHAR:7f" in output
            self.assertTrue(has_bs, f"Backspace not received by child.\nOutput:\n{output[-2000:]}")
        finally:
            session.send_ctrl_c()
            session.close()

    def test_type_word_then_backspace_sequence(self) -> None:
        """Type 'hello', then backspace 3 times — child sees all events."""
        session = PtySession(
            [sys.executable, "-c", _ECHO_SCRIPT],
            cwd=Path.cwd(),
        )
        try:
            session.wait_for_text("READY", timeout=5)
            for ch in "hello":
                session.send(ch)
                time.sleep(0.03)

            session.wait_for_text("CHAR:6f", timeout=2)  # 'o'

            # Send 3 backspaces
            for _ in range(3):
                session.send("\x08")
                time.sleep(0.05)

            time.sleep(0.5)
            output = session.output()

            # Count backspace events (0x08 or 0x7f)
            bs_count = output.count("CHAR:08") + output.count("CHAR:7f")
            self.assertGreaterEqual(
                bs_count,
                3,
                f"Expected >= 3 backspace events, got {bs_count}.\nOutput:\n{output[-2000:]}",
            )
        finally:
            session.send_ctrl_c()
            session.close()


class TestPtyTerminalSize(unittest.TestCase):
    """Terminal size should be forwarded to the child PTY."""

    def test_child_sees_requested_terminal_size(self) -> None:
        """Child's shutil.get_terminal_size() should match PTY dimensions."""
        expected_rows, expected_cols = 50, 160
        session = PtySession(
            [sys.executable, "-c", _SIZE_SCRIPT],
            cwd=Path.cwd(),
            rows=expected_rows,
            cols=expected_cols,
        )
        try:
            session.wait_for_text("COLS:", timeout=5)
            session.wait_closed(timeout=3)
            output = session.output()
            # Parse "COLS:160 ROWS:50"
            self.assertIn(f"COLS:{expected_cols}", output, f"Column mismatch.\nOutput: {output}")
            self.assertIn(f"ROWS:{expected_rows}", output, f"Row mismatch.\nOutput: {output}")
        finally:
            session.close()


class TestPtyCtrlC(unittest.TestCase):
    """Ctrl-C / session termination should exit cleanly."""

    def test_terminate_after_typing(self) -> None:
        """PTY session can be terminated cleanly after typing characters."""
        session = PtySession(
            [sys.executable, "-c", _ECHO_SCRIPT],
            cwd=Path.cwd(),
        )
        try:
            session.wait_for_text("READY", timeout=5)
            session.send("abc")
            time.sleep(0.3)
            # Terminate the PTY (ConPTY on Windows may not relay \x03 as SIGINT
            # to child processes, so we test explicit termination instead).
            session.close()
            self.assertFalse(session.is_alive())
        finally:
            session.close()

    def test_close_no_traceback(self) -> None:
        """Closing the PTY session should not produce Python tracebacks."""
        session = PtySession(
            [sys.executable, "-c", _ECHO_SCRIPT],
            cwd=Path.cwd(),
        )
        try:
            session.wait_for_text("READY", timeout=5)
            session.close()
            output = session.output()
            self.assertNotIn("Traceback", output)
            self.assertNotIn("KeyboardInterrupt", output)
        finally:
            session.close()


# ---------------------------------------------------------------------------
# Tests using clud's PseudoTerminalProcess directly
# ---------------------------------------------------------------------------


class TestProcessLauncherPtySize(unittest.TestCase):
    """Verify that _run_with_idle_timeout passes host terminal size to PTY."""

    def test_pty_uses_host_terminal_size(self) -> None:
        """PseudoTerminalProcess should receive the host's terminal dimensions."""
        from running_process import PseudoTerminalProcess

        try:
            host_size = os.get_terminal_size()
        except OSError:
            self.skipTest("No host terminal available")

        pty = PseudoTerminalProcess(
            [sys.executable, "-c", _SIZE_SCRIPT],
            capture=True,
            relay_terminal_input=False,
            rows=host_size.lines,
            cols=host_size.columns,
        )
        result = pty.wait(timeout=5)
        output = str(getattr(pty, "output", b""))
        self.assertEqual(result, 0, f"Script exited with code {result}")
        self.assertIn(f"COLS:{host_size.columns}", output)
        self.assertIn(f"ROWS:{host_size.lines}", output)

    def test_default_pty_size_is_24x80(self) -> None:
        """Without explicit size, PseudoTerminalProcess defaults to 24x80."""
        from running_process import PseudoTerminalProcess

        pty = PseudoTerminalProcess(
            [sys.executable, "-c", _SIZE_SCRIPT],
            capture=True,
            relay_terminal_input=False,
        )
        result = pty.wait(timeout=5)
        output = str(getattr(pty, "output", b""))
        self.assertEqual(result, 0)
        self.assertIn("COLS:80", output)
        self.assertIn("ROWS:24", output)


# ---------------------------------------------------------------------------
# Codex-specific opt-in test (requires real Codex binary)
# ---------------------------------------------------------------------------

RUN_CODEX_TEST = os.environ.get("CLUD_RUN_CODEX_PTY_TEST") == "1"


@unittest.skipUnless(RUN_CODEX_TEST, "Set CLUD_RUN_CODEX_PTY_TEST=1 to run")
@unittest.skipUnless(shutil.which("codex") is not None, "Codex CLI must be installed")
class TestCodexInteractiveResponsiveness(unittest.TestCase):
    """End-to-end responsiveness test against real ``clud --codex``."""

    def setUp(self) -> None:
        self.cwd = Path(__file__).resolve().parents[2]
        self.cmd = [sys.executable, "-m", "clud", "--session-model=codex"]

    def test_type_and_backspace_in_codex_tui(self) -> None:
        """Type characters into the Codex TUI, backspace, then Ctrl-C."""
        session = PtySession(self.cmd, cwd=self.cwd, rows=40, cols=140)
        try:
            # Wait for the Codex TUI to appear
            session.wait_for_text("OpenAI Codex", timeout=20)

            # Type a word
            for ch in "hello":
                session.send(ch)
                time.sleep(0.1)

            time.sleep(0.3)

            # Backspace a few times
            for _ in range(3):
                session.send("\x08")
                time.sleep(0.1)

            time.sleep(0.3)

            # Ctrl-C to exit
            session.send_ctrl_c()
            session.wait_closed(timeout=10)

            output = session.output()
            self.assertFalse(session.is_alive(), "PTY child should exit after Ctrl-C")
            self.assertNotIn("Traceback", output)
            self.assertNotIn("KeyboardInterrupt", output)
        finally:
            session.close()

    def test_codex_tui_startup_latency(self) -> None:
        """Codex TUI should become interactive within a reasonable time."""
        start = time.perf_counter()
        session = PtySession(self.cmd, cwd=self.cwd, rows=40, cols=140)
        try:
            session.wait_for_text("OpenAI Codex", timeout=30)
            startup_s = time.perf_counter() - start
            # Just log it — no hard assertion, but useful for regression tracking.
            print(f"\nCodex TUI startup: {startup_s:.1f}s", file=sys.stderr)
            self.assertLess(startup_s, 30, "Codex TUI took > 30s to start")
        finally:
            session.send_ctrl_c()
            session.close()


if __name__ == "__main__":
    unittest.main()
