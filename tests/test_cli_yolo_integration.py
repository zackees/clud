"""Tests for CLI integration with yolo functionality."""

import unittest
from io import StringIO
from unittest.mock import patch

from clud.cli import main


class TestCliYoloIntegration(unittest.TestCase):
    """Test CLI integration with yolo functionality."""

    def test_cli_with_message_and_dry_run(self) -> None:
        """Test CLI with -m and --dry-run flags."""
        # Capture stdout
        captured_output = StringIO()
        with patch("sys.stdout", captured_output):
            result = main(["-m", "test message from CLI", "--dry-run"])

        self.assertEqual(result, 0)
        self.assertEqual(captured_output.getvalue().strip(), "Would execute: claude --dangerously-skip-permissions test message from CLI")

    def test_cli_dry_run_without_message(self) -> None:
        """Test CLI with --dry-run but no message."""
        # Capture stdout
        captured_output = StringIO()
        with patch("sys.stdout", captured_output):
            result = main(["--dry-run"])

        self.assertEqual(result, 0)
        self.assertEqual(captured_output.getvalue().strip(), "Would execute: claude --dangerously-skip-permissions")

    def test_cli_message_without_dry_run_mocked(self) -> None:
        """Test CLI with -m but no --dry-run (should try to run Claude, but we mock it)."""
        # Mock _execute_command to avoid actually running Claude
        with patch("clud.agent_cli._execute_command") as mock_execute:
            # Mock successful return for command execution
            mock_execute.return_value = 0

            # Mock shutil.which to return a fake claude path
            # Also mock detect_git_bash to avoid subprocess calls in git-bash detection
            with patch("clud.agent_cli.shutil.which", return_value="/fake/claude"), patch("clud.agent_cli.detect_git_bash", return_value=None):
                result = main(["-m", "test message"])

        self.assertEqual(result, 0)
        # Verify that _execute_command was called (meaning it tried to run Claude)
        mock_execute.assert_called_once()


if __name__ == "__main__":
    unittest.main()
