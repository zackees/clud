"""Unit tests for clud utility functions."""

import logging
import subprocess
import unittest
from io import StringIO
from unittest.mock import MagicMock, patch

from clud.util import _is_git_bash, detect_git_bash, handle_keyboard_interrupt


class TestHandleKeyboardInterrupt(unittest.TestCase):
    """Test keyboard interrupt handling utility."""

    def test_successful_execution(self) -> None:
        """Test that function executes normally without interrupt."""

        def add(x: int, y: int) -> int:
            return x + y

        result = handle_keyboard_interrupt(add, 2, 3)
        self.assertEqual(result, 5)

    def test_keyboard_interrupt_reraises(self) -> None:
        """Test that KeyboardInterrupt is always re-raised."""

        def raises_interrupt() -> None:
            raise KeyboardInterrupt()

        with self.assertRaises(KeyboardInterrupt):
            handle_keyboard_interrupt(raises_interrupt)

    def test_cleanup_called_on_interrupt(self) -> None:
        """Test that cleanup function is called before re-raising KeyboardInterrupt."""
        cleanup_called = False

        def cleanup() -> None:
            nonlocal cleanup_called
            cleanup_called = True

        def raises_interrupt() -> None:
            raise KeyboardInterrupt()

        with self.assertRaises(KeyboardInterrupt):
            handle_keyboard_interrupt(raises_interrupt, cleanup=cleanup)

        self.assertTrue(cleanup_called, "Cleanup should be called before re-raising")

    def test_cleanup_failure_does_not_prevent_interrupt(self) -> None:
        """Test that cleanup failures don't prevent KeyboardInterrupt propagation."""

        def failing_cleanup() -> None:
            raise RuntimeError("Cleanup failed!")

        def raises_interrupt() -> None:
            raise KeyboardInterrupt()

        # Should still raise KeyboardInterrupt even if cleanup fails
        with self.assertRaises(KeyboardInterrupt):
            handle_keyboard_interrupt(raises_interrupt, cleanup=failing_cleanup)

    def test_logging_on_interrupt(self) -> None:
        """Test that interrupt is logged when logger is provided."""
        logger = logging.getLogger("test")
        logger.setLevel(logging.INFO)
        log_stream = StringIO()
        handler = logging.StreamHandler(log_stream)
        logger.addHandler(handler)

        def raises_interrupt() -> None:
            raise KeyboardInterrupt()

        try:
            with self.assertRaises(KeyboardInterrupt):
                handle_keyboard_interrupt(raises_interrupt, logger=logger, log_message="Test interrupted")
        finally:
            logger.removeHandler(handler)

        log_output = log_stream.getvalue()
        self.assertIn("Test interrupted", log_output)

    def test_default_log_message(self) -> None:
        """Test that default log message is used when none provided."""
        logger = logging.getLogger("test_default")
        logger.setLevel(logging.INFO)
        log_stream = StringIO()
        handler = logging.StreamHandler(log_stream)
        logger.addHandler(handler)

        def raises_interrupt() -> None:
            raise KeyboardInterrupt()

        try:
            with self.assertRaises(KeyboardInterrupt):
                handle_keyboard_interrupt(raises_interrupt, logger=logger)
        finally:
            logger.removeHandler(handler)

        log_output = log_stream.getvalue()
        self.assertIn("Operation interrupted by user", log_output)

    def test_other_exceptions_propagate(self) -> None:
        """Test that non-KeyboardInterrupt exceptions are propagated normally."""

        def raises_value_error() -> None:
            raise ValueError("Test error")

        with self.assertRaises(ValueError) as ctx:
            handle_keyboard_interrupt(raises_value_error)

        self.assertEqual(str(ctx.exception), "Test error")

    def test_kwargs_passed_correctly(self) -> None:
        """Test that keyword arguments are passed to the function."""

        def func_with_kwargs(x: int, y: int, z: int = 0) -> int:
            return x + y + z

        result = handle_keyboard_interrupt(func_with_kwargs, 1, 2, z=3)
        self.assertEqual(result, 6)

    def test_cleanup_failure_logged_when_logger_provided(self) -> None:
        """Test that cleanup failures are logged when logger is available."""
        logger = logging.getLogger("test_cleanup_failure")
        logger.setLevel(logging.WARNING)
        log_stream = StringIO()
        handler = logging.StreamHandler(log_stream)
        logger.addHandler(handler)

        def failing_cleanup() -> None:
            raise RuntimeError("Cleanup explosion!")

        def raises_interrupt() -> None:
            raise KeyboardInterrupt()

        try:
            with self.assertRaises(KeyboardInterrupt):
                handle_keyboard_interrupt(raises_interrupt, cleanup=failing_cleanup, logger=logger)
        finally:
            logger.removeHandler(handler)

        log_output = log_stream.getvalue()
        self.assertIn("Cleanup failed during keyboard interrupt", log_output)
        self.assertIn("Cleanup explosion!", log_output)


class TestIsGitBash(unittest.TestCase):
    """Test _is_git_bash validation function."""

    @patch("subprocess.run")
    @patch("os.path.isfile")
    def test_valid_git_bash(self, mock_isfile: MagicMock, mock_run: MagicMock) -> None:
        """Test that valid git-bash is detected correctly."""
        mock_isfile.return_value = True
        mock_run.return_value = MagicMock(
            returncode=0,
            stdout="GNU bash, version 4.4.23(1)-release (x86_64-pc-msys)\n",
        )

        result = _is_git_bash(r"C:\Program Files\Git\bin\bash.exe")
        self.assertTrue(result)

    @patch("subprocess.run")
    def test_wsl_bash_rejected_by_path(self, mock_run: MagicMock) -> None:
        """Test that WSL bash is rejected based on path indicators."""
        # Should not even try to run --version if path contains WSL indicators
        result = _is_git_bash(r"C:\Windows\System32\wsl.exe")
        self.assertFalse(result)
        mock_run.assert_not_called()

    @patch("subprocess.run")
    def test_wsl_bash_rejected_by_version(self, mock_run: MagicMock) -> None:
        """Test that WSL bash is rejected based on version output."""
        mock_run.return_value = MagicMock(
            returncode=0,
            stdout="GNU bash, version 5.0.17(1)-release (x86_64-pc-linux-gnu)\n",
        )

        result = _is_git_bash(r"C:\Users\test\bash.exe")
        self.assertFalse(result)

    @patch("subprocess.run")
    def test_bash_version_check_failure(self, mock_run: MagicMock) -> None:
        """Test that bash is rejected if --version fails."""
        mock_run.return_value = MagicMock(returncode=1, stdout="")

        result = _is_git_bash(r"C:\invalid\bash.exe")
        self.assertFalse(result)

    @patch("subprocess.run")
    def test_subprocess_error_handling(self, mock_run: MagicMock) -> None:
        """Test that subprocess errors are handled gracefully."""
        mock_run.side_effect = subprocess.SubprocessError("Test error")

        result = _is_git_bash(r"C:\test\bash.exe")
        self.assertFalse(result)


class TestDetectGitBash(unittest.TestCase):
    """Test detect_git_bash function."""

    @patch("platform.system")
    def test_non_windows_returns_none(self, mock_system: MagicMock) -> None:
        """Test that non-Windows systems return None."""
        mock_system.return_value = "Linux"
        result = detect_git_bash()
        self.assertIsNone(result)

    @patch("platform.system")
    @patch("subprocess.run")
    @patch("os.path.isfile")
    @patch("clud.util._is_git_bash")
    def test_finds_bash_from_where_command(self, mock_is_git_bash: MagicMock, mock_isfile: MagicMock, mock_run: MagicMock, mock_system: MagicMock) -> None:
        """Test that detect_git_bash finds bash using 'where bash' command."""
        mock_system.return_value = "Windows"
        mock_run.return_value = MagicMock(
            returncode=0,
            stdout=r"C:\Program Files\Git\bin\bash.exe",
        )
        mock_isfile.return_value = True
        mock_is_git_bash.return_value = True

        result = detect_git_bash()
        self.assertEqual(result, r"C:\Program Files\Git\bin\bash.exe")

    @patch("platform.system")
    @patch("subprocess.run")
    @patch("os.path.isfile")
    @patch("clud.util._is_git_bash")
    def test_fallback_to_common_paths(self, mock_is_git_bash: MagicMock, mock_isfile: MagicMock, mock_run: MagicMock, mock_system: MagicMock) -> None:
        """Test that detect_git_bash falls back to common installation paths."""
        mock_system.return_value = "Windows"
        # 'where' commands fail
        mock_run.return_value = MagicMock(returncode=1, stdout="")

        # First common path exists and is valid git-bash
        def isfile_side_effect(path: str) -> bool:
            return path == r"C:\Program Files\Git\bin\bash.exe"

        mock_isfile.side_effect = isfile_side_effect
        mock_is_git_bash.return_value = True

        result = detect_git_bash()
        self.assertEqual(result, r"C:\Program Files\Git\bin\bash.exe")

    @patch("platform.system")
    @patch("subprocess.run")
    @patch("os.path.isfile")
    @patch("clud.util._is_git_bash")
    def test_returns_none_when_not_found(self, mock_is_git_bash: MagicMock, mock_isfile: MagicMock, mock_run: MagicMock, mock_system: MagicMock) -> None:
        """Test that detect_git_bash returns None when git-bash is not found."""
        mock_system.return_value = "Windows"
        mock_run.return_value = MagicMock(returncode=1, stdout="")
        mock_isfile.return_value = False

        result = detect_git_bash()
        self.assertIsNone(result)

    @patch("platform.system")
    @patch("subprocess.run")
    @patch("os.path.isfile")
    @patch("clud.util._is_git_bash")
    def test_skips_wsl_bash(self, mock_is_git_bash: MagicMock, mock_isfile: MagicMock, mock_run: MagicMock, mock_system: MagicMock) -> None:
        """Test that detect_git_bash skips WSL bash and finds real git-bash."""
        mock_system.return_value = "Windows"
        # 'where bash' returns WSL bash first, then git-bash
        mock_run.return_value = MagicMock(
            returncode=0,
            stdout=r"C:\Windows\System32\wsl.exe\nC:\Program Files\Git\bin\bash.exe",
        )
        mock_isfile.return_value = True
        # First call (WSL) returns False, second call (git-bash) returns True
        mock_is_git_bash.side_effect = [False, True]

        result = detect_git_bash()
        self.assertEqual(result, r"C:\Program Files\Git\bin\bash.exe")

    @patch("platform.system")
    @patch("subprocess.run")
    def test_handles_subprocess_errors_gracefully(self, mock_run: MagicMock, mock_system: MagicMock) -> None:
        """Test that subprocess errors are handled gracefully."""
        mock_system.return_value = "Windows"
        mock_run.side_effect = subprocess.SubprocessError("Test error")

        # Should not raise, should return None or fallback to common paths
        result = detect_git_bash()
        # Result could be None or a common path if it exists
        self.assertIsInstance(result, (str, type(None)))


if __name__ == "__main__":
    unittest.main()
