"""Unit tests for PTY manager."""

import os
import platform
import sys
import threading
import time
import unittest
from unittest.mock import Mock, patch

# Add src to path for testing
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))

from clud.webui.pty_manager import PTYManager, PTYSession


class TestPTYManager(unittest.TestCase):
    """Test PTYManager functionality."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.manager = PTYManager()

    def tearDown(self) -> None:
        """Clean up after tests."""
        # Close all sessions
        for session_id in list(self.manager.sessions.keys()):
            self.manager.close_session(session_id)

    def test_init(self) -> None:
        """Test PTYManager initialization."""
        self.assertIsInstance(self.manager.sessions, dict)
        self.assertEqual(len(self.manager.sessions), 0)
        self.assertIsNotNone(self.manager._lock)

    def test_get_shell_windows(self) -> None:
        """Test shell detection on Windows."""
        with patch("platform.system", return_value="Windows"):
            with patch("clud.webui.pty_manager.detect_git_bash", return_value=r"C:\Program Files\Git\bin\bash.exe"):
                shell = PTYManager._get_shell()
                self.assertIn("bash.exe", shell[0].lower())

            with patch("clud.webui.pty_manager.detect_git_bash", return_value=None):
                shell = PTYManager._get_shell()
                self.assertEqual(shell, ["cmd.exe"])

    def test_get_shell_unix(self) -> None:
        """Test shell detection on Unix."""
        with patch("platform.system", return_value="Linux"):
            with patch.dict(os.environ, {"SHELL": "/bin/zsh"}):
                shell = PTYManager._get_shell()
                self.assertEqual(shell, ["/bin/zsh"])

            with patch.dict(os.environ, {}, clear=True):
                shell = PTYManager._get_shell()
                self.assertEqual(shell, ["/bin/bash"])

    @unittest.skipIf(platform.system() == "Windows", "PTY not fully supported on Windows in test env")
    def test_create_session(self) -> None:
        """Test creating a PTY session."""
        output_received = threading.Event()
        output_data: list[bytes] = []

        def on_output(data: bytes) -> None:
            output_data.append(data)
            output_received.set()

        def on_exit(code: int) -> None:
            pass

        session = self.manager.create_session(
            session_id="test-session",
            cwd=os.getcwd(),
            cols=80,
            rows=24,
            on_output=on_output,
            on_exit=on_exit,
        )

        self.assertIsInstance(session, PTYSession)
        self.assertEqual(session.session_id, "test-session")
        self.assertEqual(session.cols, 80)
        self.assertEqual(session.rows, 24)
        self.assertIn("test-session", self.manager.sessions)

        # Write something and check we get output
        self.manager.write_input("test-session", b"echo test\n")
        output_received.wait(timeout=2)
        self.assertTrue(len(output_data) > 0)

    def test_create_duplicate_session(self) -> None:
        """Test that creating duplicate session raises error."""
        on_output = Mock()
        on_exit = Mock()

        # Create first session (may fail on Windows, so wrap in try)
        try:
            self.manager.create_session(
                session_id="dup-session",
                cwd=os.getcwd(),
                cols=80,
                rows=24,
                on_output=on_output,
                on_exit=on_exit,
            )

            # Try to create duplicate
            with self.assertRaises(ValueError) as ctx:
                self.manager.create_session(
                    session_id="dup-session",
                    cwd=os.getcwd(),
                    cols=80,
                    rows=24,
                    on_output=on_output,
                    on_exit=on_exit,
                )
            self.assertIn("already exists", str(ctx.exception))
        except Exception:
            # Skip test on Windows if PTY creation fails
            if platform.system() == "Windows":
                self.skipTest("PTY not supported in test environment on Windows")
            raise

    @unittest.skipIf(platform.system() == "Windows", "PTY not fully supported on Windows in test env")
    def test_write_input(self) -> None:
        """Test writing input to PTY session."""
        output_data: list[bytes] = []

        def on_output(data: bytes) -> None:
            output_data.append(data)

        def on_exit(code: int) -> None:
            pass

        self.manager.create_session(
            session_id="write-test",
            cwd=os.getcwd(),
            cols=80,
            rows=24,
            on_output=on_output,
            on_exit=on_exit,
        )

        # Write input
        self.manager.write_input("write-test", b"echo hello\n")
        time.sleep(0.5)  # Give time for output

        # Check we got some output
        self.assertTrue(len(output_data) > 0)

    def test_write_input_nonexistent_session(self) -> None:
        """Test writing to nonexistent session raises error."""
        with self.assertRaises(ValueError) as ctx:
            self.manager.write_input("nonexistent", b"test")
        self.assertIn("not found", str(ctx.exception))

    @unittest.skipIf(platform.system() == "Windows", "PTY not fully supported on Windows in test env")
    def test_resize(self) -> None:
        """Test resizing PTY session."""
        on_output = Mock()
        on_exit = Mock()

        self.manager.create_session(
            session_id="resize-test",
            cwd=os.getcwd(),
            cols=80,
            rows=24,
            on_output=on_output,
            on_exit=on_exit,
        )

        # Resize
        self.manager.resize("resize-test", 100, 30)

        session = self.manager.sessions["resize-test"]
        self.assertEqual(session.cols, 100)
        self.assertEqual(session.rows, 30)

    def test_resize_nonexistent_session(self) -> None:
        """Test resizing nonexistent session raises error."""
        with self.assertRaises(ValueError) as ctx:
            self.manager.resize("nonexistent", 80, 24)
        self.assertIn("not found", str(ctx.exception))

    @unittest.skipIf(platform.system() == "Windows", "PTY not fully supported on Windows in test env")
    def test_close_session(self) -> None:
        """Test closing PTY session."""
        on_output = Mock()
        on_exit = Mock()

        self.manager.create_session(
            session_id="close-test",
            cwd=os.getcwd(),
            cols=80,
            rows=24,
            on_output=on_output,
            on_exit=on_exit,
        )

        self.assertIn("close-test", self.manager.sessions)

        # Close session
        self.manager.close_session("close-test")

        self.assertNotIn("close-test", self.manager.sessions)

    def test_close_nonexistent_session(self) -> None:
        """Test closing nonexistent session doesn't raise error."""
        # Should not raise an error
        self.manager.close_session("nonexistent")

    def test_set_pty_size(self) -> None:
        """Test _set_pty_size doesn't crash."""
        # This is a static method that uses fcntl/termios
        # We can't easily test it without a real PTY, but we can
        # verify it's callable
        self.assertTrue(callable(PTYManager._set_pty_size))


if __name__ == "__main__":
    unittest.main()
