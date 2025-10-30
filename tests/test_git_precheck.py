"""Tests for git_precheck module."""

import sys
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

# Add src to path for testing
sys.path.insert(0, str(Path(__file__).parent.parent / "src"))

from clud.git_precheck import GitPreCheckResult, display_git_status, run_git_precheck, run_two_phase_precheck


class TestGitPreCheckResult(unittest.TestCase):
    """Test GitPreCheckResult NamedTuple."""

    def test_result_creation(self) -> None:
        """Test creating a GitPreCheckResult."""
        result = GitPreCheckResult(
            success=True,
            error_message="",
            has_changes=True,
            untracked_files=["file1.py", "file2.py"],
            staged_files=["file3.py"],
            unstaged_files=["file4.py"],
        )

        self.assertTrue(result.success)
        self.assertEqual(result.error_message, "")
        self.assertTrue(result.has_changes)
        self.assertEqual(len(result.untracked_files), 2)
        self.assertEqual(len(result.staged_files), 1)
        self.assertEqual(len(result.unstaged_files), 1)

    def test_result_immutability(self) -> None:
        """Test that GitPreCheckResult is immutable (NamedTuple)."""
        result = GitPreCheckResult(
            success=True,
            error_message="",
            has_changes=False,
            untracked_files=[],
            staged_files=[],
            unstaged_files=[],
        )

        # Should not be able to modify fields
        with self.assertRaises(AttributeError):
            result.success = False  # type: ignore


class TestRunGitPrecheck(unittest.TestCase):
    """Test run_git_precheck function."""

    @patch("clud.git_precheck.CODEUP_AVAILABLE", True)
    @patch("clud.git_precheck.Codeup")
    def test_successful_check_no_changes(self, mock_codeup: MagicMock) -> None:
        """Test successful check with no changes."""
        # Mock the Codeup.pre_check_git() return value
        mock_result = MagicMock()
        mock_result.success = True
        mock_result.error_message = None
        mock_result.has_changes = False
        mock_result.untracked_files = []
        mock_result.staged_files = []
        mock_result.unstaged_files = []
        mock_codeup.pre_check_git.return_value = mock_result

        result = run_git_precheck(allow_interactive=False)

        self.assertTrue(result.success)
        self.assertEqual(result.error_message, "")
        self.assertFalse(result.has_changes)
        self.assertEqual(len(result.untracked_files), 0)
        mock_codeup.pre_check_git.assert_called_once_with(allow_interactive=False)

    @patch("clud.git_precheck.CODEUP_AVAILABLE", True)
    @patch("clud.git_precheck.Codeup")
    def test_successful_check_with_changes(self, mock_codeup: MagicMock) -> None:
        """Test successful check with changes detected."""
        # Mock the Codeup.pre_check_git() return value
        mock_result = MagicMock()
        mock_result.success = True
        mock_result.error_message = None
        mock_result.has_changes = True
        mock_result.untracked_files = ["untracked.py"]
        mock_result.staged_files = ["staged.py"]
        mock_result.unstaged_files = ["unstaged.py"]
        mock_codeup.pre_check_git.return_value = mock_result

        result = run_git_precheck(allow_interactive=False)

        self.assertTrue(result.success)
        self.assertTrue(result.has_changes)
        self.assertEqual(len(result.untracked_files), 1)
        self.assertEqual(len(result.staged_files), 1)
        self.assertEqual(len(result.unstaged_files), 1)

    @patch("clud.git_precheck.CODEUP_AVAILABLE", True)
    @patch("clud.git_precheck.Codeup")
    def test_failed_check(self, mock_codeup: MagicMock) -> None:
        """Test failed check with error message."""
        # Mock the Codeup.pre_check_git() return value
        mock_result = MagicMock()
        mock_result.success = False
        mock_result.error_message = "Git command failed"
        mock_result.has_changes = False
        mock_result.untracked_files = []
        mock_result.staged_files = []
        mock_result.unstaged_files = []
        mock_codeup.pre_check_git.return_value = mock_result

        result = run_git_precheck(allow_interactive=False)

        self.assertFalse(result.success)
        self.assertEqual(result.error_message, "Git command failed")

    def test_import_error_handling(self) -> None:
        """Test handling when codeup is not available."""
        with patch("clud.git_precheck.CODEUP_AVAILABLE", False), patch("clud.git_precheck.Codeup", None):
            result = run_git_precheck(allow_interactive=False)

            self.assertFalse(result.success)
            self.assertIn("CodeUp package is not installed", result.error_message)
            self.assertFalse(result.has_changes)


class TestRunTwoPhasePrecheck(unittest.TestCase):
    """Test run_two_phase_precheck function."""

    @patch("clud.git_precheck.run_git_precheck")
    def test_clean_repository(self, mock_run_git_precheck: MagicMock) -> None:
        """Test two-phase precheck with clean repository."""
        # Mock non-interactive check returning no changes
        mock_run_git_precheck.return_value = GitPreCheckResult(
            success=True,
            error_message="",
            has_changes=False,
            untracked_files=[],
            staged_files=[],
            unstaged_files=[],
        )

        result = run_two_phase_precheck(verbose=False)

        self.assertTrue(result.success)
        self.assertFalse(result.has_changes)
        # Should only call once (non-interactive)
        mock_run_git_precheck.assert_called_once_with(allow_interactive=False)

    @patch("clud.git_precheck.run_git_precheck")
    @patch("clud.git_precheck.sys.stdin.isatty", return_value=True)
    def test_untracked_files_interactive(self, mock_isatty: MagicMock, mock_run_git_precheck: MagicMock) -> None:
        """Test two-phase precheck with untracked files in interactive mode."""
        # First call (non-interactive) returns untracked files
        non_interactive_result = GitPreCheckResult(
            success=True,
            error_message="",
            has_changes=True,
            untracked_files=["untracked.py"],
            staged_files=[],
            unstaged_files=[],
        )

        # Second call (interactive) returns no untracked files (user added them)
        interactive_result = GitPreCheckResult(
            success=True,
            error_message="",
            has_changes=True,
            untracked_files=[],
            staged_files=["untracked.py"],
            unstaged_files=[],
        )

        mock_run_git_precheck.side_effect = [non_interactive_result, interactive_result]

        result = run_two_phase_precheck(verbose=False)

        self.assertTrue(result.success)
        self.assertTrue(result.has_changes)
        # Files should be staged now
        self.assertEqual(len(result.staged_files), 1)
        self.assertEqual(len(result.untracked_files), 0)
        # Should call twice (non-interactive then interactive)
        self.assertEqual(mock_run_git_precheck.call_count, 2)

    @patch("clud.git_precheck.run_git_precheck")
    @patch("clud.git_precheck.sys.stdin.isatty", return_value=False)
    def test_untracked_files_no_tty(self, mock_isatty: MagicMock, mock_run_git_precheck: MagicMock) -> None:
        """Test two-phase precheck with untracked files but no TTY."""
        # Non-interactive check returns untracked files
        mock_run_git_precheck.return_value = GitPreCheckResult(
            success=True,
            error_message="",
            has_changes=True,
            untracked_files=["untracked.py"],
            staged_files=[],
            unstaged_files=[],
        )

        result = run_two_phase_precheck(verbose=False)

        self.assertTrue(result.success)
        self.assertTrue(result.has_changes)
        # Should not run interactive mode without TTY
        mock_run_git_precheck.assert_called_once_with(allow_interactive=False)

    @patch("clud.git_precheck.run_git_precheck")
    def test_error_handling(self, mock_run_git_precheck: MagicMock) -> None:
        """Test two-phase precheck with error in non-interactive phase."""
        mock_run_git_precheck.return_value = GitPreCheckResult(
            success=False,
            error_message="Git command failed",
            has_changes=False,
            untracked_files=[],
            staged_files=[],
            unstaged_files=[],
        )

        result = run_two_phase_precheck(verbose=False)

        self.assertFalse(result.success)
        self.assertEqual(result.error_message, "Git command failed")


class TestDisplayGitStatus(unittest.TestCase):
    """Test display_git_status function."""

    def test_display_clean_repository(self) -> None:
        """Test displaying clean repository status."""
        result = GitPreCheckResult(
            success=True,
            error_message="",
            has_changes=False,
            untracked_files=[],
            staged_files=[],
            unstaged_files=[],
        )

        # Should not raise any exceptions
        with patch("builtins.print") as mock_print:
            display_git_status(result)
            # Should print clean repository message
            mock_print.assert_called()

    def test_display_with_changes(self) -> None:
        """Test displaying status with changes."""
        result = GitPreCheckResult(
            success=True,
            error_message="",
            has_changes=True,
            untracked_files=["file1.py", "file2.py"],
            staged_files=["file3.py"],
            unstaged_files=["file4.py"],
        )

        with patch("builtins.print") as mock_print:
            display_git_status(result)
            # Should print multiple status lines
            self.assertGreater(mock_print.call_count, 1)

    def test_display_error(self) -> None:
        """Test displaying error status."""
        result = GitPreCheckResult(
            success=False,
            error_message="Git command failed",
            has_changes=False,
            untracked_files=[],
            staged_files=[],
            unstaged_files=[],
        )

        with patch("builtins.print") as mock_print:
            display_git_status(result)
            # Should print error message
            mock_print.assert_called()

    def test_display_many_files(self) -> None:
        """Test displaying status with many files (should truncate)."""
        result = GitPreCheckResult(
            success=True,
            error_message="",
            has_changes=True,
            untracked_files=[f"file{i}.py" for i in range(10)],
            staged_files=[],
            unstaged_files=[],
        )

        with patch("builtins.print") as mock_print:
            display_git_status(result, max_files=5)
            # Should truncate to max_files
            mock_print.assert_called()


if __name__ == "__main__":
    unittest.main()
