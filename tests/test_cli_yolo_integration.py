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
        # Mock subprocess.run to avoid actually running Claude
        with patch("clud.agent.subprocess.subprocess.run") as mock_run:
            # Mock successful return for command execution
            mock_run.return_value.returncode = 0

            # Mock _find_claude_path to return a fake claude path
            with patch("clud.agent.runner._find_claude_path", return_value="/fake/claude"):
                result = main(["-m", "test message"])

        self.assertEqual(result, 0)
        # Verify that subprocess.run was called (meaning it tried to run Claude)
        # Note: subprocess.run is called multiple times (git-bash detection + claude execution)
        self.assertGreaterEqual(mock_run.call_count, 1)
        # Verify the last call was to execute claude with the message
        last_call = mock_run.call_args_list[-1]
        self.assertIn("claude", str(last_call))
        self.assertIn("test message", str(last_call))


if __name__ == "__main__":
    unittest.main()
