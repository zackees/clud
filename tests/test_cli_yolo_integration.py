"""Tests for CLI integration with yolo functionality."""

import unittest
from io import StringIO
from unittest.mock import patch

from src.clud.cli import main


class TestCliYoloIntegration(unittest.TestCase):
    """Test CLI integration with yolo functionality."""

    def test_cli_with_message_and_dry_run(self):
        """Test CLI with -m and --dry-run flags."""
        # Capture stdout
        captured_output = StringIO()
        with patch("sys.stdout", captured_output):
            result = main(["-m", "test message from CLI", "--dry-run"])

        self.assertEqual(result, 0)
        self.assertEqual(captured_output.getvalue().strip(), "test message from CLI")

    def test_cli_dry_run_without_message(self):
        """Test CLI with --dry-run but no message."""
        # Capture stdout
        captured_output = StringIO()
        with patch("sys.stdout", captured_output):
            result = main(["--dry-run"])

        self.assertEqual(result, 0)
        self.assertEqual(captured_output.getvalue().strip(), "Dry-run mode: No message provided")

    def test_cli_message_without_dry_run_mocked(self):
        """Test CLI with -m but no --dry-run (should try to run Claude, but we mock it)."""
        # Mock subprocess.run to avoid actually trying to run Claude
        with patch("src.clud.yolo.subprocess.run") as mock_yolo_run:
            # Mock successful return for yolo subprocess call
            mock_yolo_run.return_value.returncode = 0

            # Mock shutil.which to return a fake claude path
            with patch("src.clud.yolo.shutil.which", return_value="/fake/claude"):
                result = main(["-m", "test message"])

        self.assertEqual(result, 0)
        # Verify that yolo subprocess.run was called (meaning it tried to run Claude)
        mock_yolo_run.assert_called_once()


if __name__ == "__main__":
    unittest.main()
