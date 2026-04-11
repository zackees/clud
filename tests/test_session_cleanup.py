"""Unit tests for stale session detection and cleanup."""

import unittest
from unittest.mock import MagicMock, patch

from clud.session_cleanup import (
    StaleProcess,
    StaleSessionReport,
    _find_stale_via_psutil,
    detect_stale_sessions,
    prompt_and_cleanup_stale_sessions,
)


class TestStaleSessionReport(unittest.TestCase):
    """Test StaleSessionReport dataclass."""

    def test_empty_report_has_no_stale(self) -> None:
        """Empty report should not flag stale sessions."""
        report = StaleSessionReport()
        self.assertFalse(report.has_stale)

    def test_report_with_processes_has_stale(self) -> None:
        """Report with processes should flag stale sessions."""
        report = StaleSessionReport(processes=[StaleProcess(pid=1234, name="node", cmdline="node claude.js", session_id="old-id")])
        self.assertTrue(report.has_stale)


class TestDetectStaleSessions(unittest.TestCase):
    """Test stale session detection via psutil."""

    @patch("clud.session_cleanup.psutil")
    def test_skips_current_session(self, mock_psutil: MagicMock) -> None:
        """Processes from the current session should be excluded."""
        current_id = "current-session-123"
        proc = MagicMock()
        proc.info = {
            "pid": 9999,
            "name": "node",
            "cmdline": ["node", "claude.js"],
            "environ": {"CLUD_SESSION_ID": current_id},
        }
        mock_psutil.process_iter.return_value = [proc]
        mock_psutil.NoSuchProcess = Exception
        mock_psutil.AccessDenied = Exception
        mock_psutil.ZombieProcess = Exception

        report = detect_stale_sessions(current_id)
        self.assertFalse(report.has_stale)

    @patch("clud.session_cleanup.psutil")
    def test_detects_stale_session(self, mock_psutil: MagicMock) -> None:
        """Processes from a different session should be detected as stale."""
        proc = MagicMock()
        proc.info = {
            "pid": 5555,
            "name": "node",
            "cmdline": ["node", "claude.js"],
            "environ": {"CLUD_SESSION_ID": "old-session-456"},
        }
        mock_psutil.process_iter.return_value = [proc]
        mock_psutil.NoSuchProcess = Exception
        mock_psutil.AccessDenied = Exception
        mock_psutil.ZombieProcess = Exception

        report = detect_stale_sessions("current-session-123")
        self.assertTrue(report.has_stale)
        self.assertEqual(len(report.processes), 1)
        self.assertEqual(report.processes[0].pid, 5555)
        self.assertEqual(report.processes[0].session_id, "old-session-456")

    @patch("clud.session_cleanup.psutil")
    def test_skips_processes_without_env(self, mock_psutil: MagicMock) -> None:
        """Processes without CLUD_SESSION_ID should be ignored."""
        proc = MagicMock()
        proc.info = {
            "pid": 7777,
            "name": "cargo",
            "cmdline": ["cargo", "test"],
            "environ": {"PATH": "/usr/bin"},
        }
        mock_psutil.process_iter.return_value = [proc]
        mock_psutil.NoSuchProcess = Exception
        mock_psutil.AccessDenied = Exception
        mock_psutil.ZombieProcess = Exception

        report = detect_stale_sessions("current-session-123")
        self.assertFalse(report.has_stale)

    @patch("clud.session_cleanup.os.getpid", return_value=1000)
    @patch("clud.session_cleanup.psutil")
    def test_skips_own_pid(self, mock_psutil: MagicMock, _mock_getpid: MagicMock) -> None:
        """Our own process should be excluded even with a different session ID."""
        proc = MagicMock()
        proc.info = {
            "pid": 1000,
            "name": "python",
            "cmdline": ["python", "-m", "clud"],
            "environ": {"CLUD_SESSION_ID": "different-id"},
        }
        mock_psutil.process_iter.return_value = [proc]
        mock_psutil.NoSuchProcess = Exception
        mock_psutil.AccessDenied = Exception
        mock_psutil.ZombieProcess = Exception

        report = detect_stale_sessions("current-session-123")
        self.assertFalse(report.has_stale)

    @patch("clud.session_cleanup.psutil")
    def test_handles_access_denied_gracefully(self, mock_psutil: MagicMock) -> None:
        """AccessDenied errors from psutil should be handled silently."""

        class AccessDenied(Exception):
            pass

        mock_psutil.AccessDenied = AccessDenied
        mock_psutil.NoSuchProcess = Exception
        mock_psutil.ZombieProcess = Exception

        proc = MagicMock()
        proc.info.__getitem__ = MagicMock(side_effect=AccessDenied("denied"))

        mock_psutil.process_iter.return_value = [proc]

        report = detect_stale_sessions("current-session-123")
        self.assertFalse(report.has_stale)


class TestPromptAndCleanup(unittest.TestCase):
    """Test the user-facing prompt and cleanup flow."""

    @patch("clud.session_cleanup.detect_stale_sessions")
    def test_no_prompt_when_no_stale(self, mock_detect: MagicMock) -> None:
        """Should return silently when no stale sessions exist."""
        mock_detect.return_value = StaleSessionReport()
        # Should not raise or print anything
        prompt_and_cleanup_stale_sessions("current-id")
        mock_detect.assert_called_once_with("current-id")

    @patch("clud.session_cleanup.detect_stale_sessions")
    @patch("sys.stdin")
    def test_warns_without_prompt_when_not_tty(self, mock_stdin: MagicMock, mock_detect: MagicMock) -> None:
        """Non-TTY mode should warn but not prompt."""
        mock_stdin.isatty.return_value = False
        mock_detect.return_value = StaleSessionReport(processes=[StaleProcess(pid=1234, name="node", cmdline="node claude.js", session_id="old-id")])
        # Should not raise (no input() call)
        prompt_and_cleanup_stale_sessions("current-id")

    @patch("clud.session_cleanup._kill_processes")
    @patch("builtins.input", return_value="y")
    @patch("clud.session_cleanup.detect_stale_sessions")
    @patch("sys.stdin")
    def test_kills_on_user_confirm(
        self,
        mock_stdin: MagicMock,
        mock_detect: MagicMock,
        mock_input: MagicMock,
        mock_kill: MagicMock,
    ) -> None:
        """User confirming 'y' should trigger kill."""
        mock_stdin.isatty.return_value = True
        stale_procs = [StaleProcess(pid=1234, name="node", cmdline="node claude.js", session_id="old-id")]
        mock_detect.return_value = StaleSessionReport(processes=stale_procs)
        mock_kill.return_value = (1, 0)

        prompt_and_cleanup_stale_sessions("current-id")
        mock_kill.assert_called_once_with(stale_procs)

    @patch("clud.session_cleanup._kill_processes")
    @patch("builtins.input", return_value="n")
    @patch("clud.session_cleanup.detect_stale_sessions")
    @patch("sys.stdin")
    def test_skips_on_user_decline(
        self,
        mock_stdin: MagicMock,
        mock_detect: MagicMock,
        mock_input: MagicMock,
        mock_kill: MagicMock,
    ) -> None:
        """User declining should not trigger kill."""
        mock_stdin.isatty.return_value = True
        mock_detect.return_value = StaleSessionReport(processes=[StaleProcess(pid=1234, name="node", cmdline="node claude.js", session_id="old-id")])

        prompt_and_cleanup_stale_sessions("current-id")
        mock_kill.assert_not_called()

    @patch("clud.session_cleanup.detect_stale_sessions")
    def test_handles_detection_error_gracefully(self, mock_detect: MagicMock) -> None:
        """Errors during detection should be swallowed."""
        mock_detect.side_effect = RuntimeError("scan failed")
        # Should not raise
        prompt_and_cleanup_stale_sessions("current-id")


class TestFindStaleViaPsutil(unittest.TestCase):
    """Test the psutil-based stale process finder."""

    @patch("clud.session_cleanup.psutil")
    def test_returns_empty_when_no_matches(self, mock_psutil: MagicMock) -> None:
        """No matching processes should return empty list."""
        mock_psutil.process_iter.return_value = []
        mock_psutil.NoSuchProcess = Exception
        mock_psutil.AccessDenied = Exception
        mock_psutil.ZombieProcess = Exception

        result = _find_stale_via_psutil("current-id")
        self.assertEqual(result, [])

    @patch("clud.session_cleanup.psutil")
    def test_handles_none_environ(self, mock_psutil: MagicMock) -> None:
        """Processes with environ=None should be skipped."""
        proc = MagicMock()
        proc.info = {
            "pid": 2222,
            "name": "bash",
            "cmdline": ["bash"],
            "environ": None,
        }
        mock_psutil.process_iter.return_value = [proc]
        mock_psutil.NoSuchProcess = Exception
        mock_psutil.AccessDenied = Exception
        mock_psutil.ZombieProcess = Exception

        result = _find_stale_via_psutil("current-id")
        self.assertEqual(result, [])


if __name__ == "__main__":
    unittest.main()
