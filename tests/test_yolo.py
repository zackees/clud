"""Tests for yolo module."""

import unittest
from io import StringIO
from unittest.mock import patch

from clud.agent_foreground import main, parse_args, run


class TestYolo(unittest.TestCase):
    """Test yolo functionality."""

    def test_parse_args_with_message(self):
        """Test parsing args with -m flag."""
        args = parse_args(["-m", "hello world"])
        self.assertEqual(args.message, "hello world")
        self.assertFalse(args.dry_run)

    def test_parse_args_with_dry_run(self):
        """Test parsing args with --dry-run flag."""
        args = parse_args(["--dry-run"])
        self.assertTrue(args.dry_run)
        self.assertIsNone(args.message)

    def test_parse_args_with_message_and_dry_run(self):
        """Test parsing args with both -m and --dry-run flags."""
        args = parse_args(["-m", "test message", "--dry-run"])
        self.assertEqual(args.message, "test message")
        self.assertTrue(args.dry_run)

    def test_dry_run_with_message(self):
        """Test dry-run mode with a message prints the message and exits 0."""
        args = parse_args(["-m", "hello world", "--dry-run"])

        # Capture stdout
        captured_output = StringIO()
        with patch("sys.stdout", captured_output):
            result = run(args)

        self.assertEqual(result, 0)
        self.assertEqual(captured_output.getvalue().strip(), "hello world")

    def test_dry_run_without_message(self):
        """Test dry-run mode without message prints appropriate message and exits 0."""
        args = parse_args(["--dry-run"])

        # Capture stdout
        captured_output = StringIO()
        with patch("sys.stdout", captured_output):
            result = run(args)

        self.assertEqual(result, 0)
        self.assertEqual(captured_output.getvalue().strip(), "Dry-run mode: No message provided")

    def test_main_with_dry_run_message(self):
        """Test main function with dry-run and message."""
        # Capture stdout
        captured_output = StringIO()
        with patch("sys.stdout", captured_output):
            result = main(["-m", "test message", "--dry-run"])

        self.assertEqual(result, 0)
        self.assertEqual(captured_output.getvalue().strip(), "test message")


if __name__ == "__main__":
    unittest.main()
