"""Tests for yolo module."""

import unittest
from io import StringIO
from unittest.mock import patch

from clud.agent.foreground import main, parse_args, run


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

    def test_parse_args_with_prompt(self):
        """Test parsing args with -p flag."""
        args = parse_args(["-p", "say hello and exit"])
        self.assertEqual(args.prompt, "say hello and exit")
        self.assertFalse(args.dry_run)

    def test_parse_args_with_prompt_and_dry_run(self):
        """Test parsing args with both -p and --dry-run flags."""
        args = parse_args(["-p", "say hello and exit", "--dry-run"])
        self.assertEqual(args.prompt, "say hello and exit")
        self.assertTrue(args.dry_run)

    def test_dry_run_with_message(self):
        """Test dry-run mode with a message shows the full command and exits 0."""
        args = parse_args(["-m", "hello world", "--dry-run"])

        # Capture stdout
        captured_output = StringIO()
        with patch("sys.stdout", captured_output):
            result = run(args)

        self.assertEqual(result, 0)
        expected_output = "Would execute: claude --dangerously-skip-permissions hello world"
        self.assertEqual(captured_output.getvalue().strip(), expected_output)

    def test_dry_run_without_message(self):
        """Test dry-run mode without message prints appropriate message and exits 0."""
        args = parse_args(["--dry-run"])

        # Capture stdout
        captured_output = StringIO()
        with patch("sys.stdout", captured_output):
            result = run(args)

        self.assertEqual(result, 0)
        self.assertIn("Would execute: claude --dangerously-skip-permissions", captured_output.getvalue())

    def test_dry_run_with_prompt(self):
        """Test dry-run mode with a prompt shows the full command and exits 0."""
        args = parse_args(["-p", "say hello and exit", "--dry-run"])

        # Capture stdout
        captured_output = StringIO()
        with patch("sys.stdout", captured_output):
            result = run(args)

        self.assertEqual(result, 0)
        expected_output = "Would execute: claude --dangerously-skip-permissions -p say hello and exit --output-format stream-json --verbose"
        self.assertEqual(captured_output.getvalue().strip(), expected_output)

    def test_main_with_dry_run_message(self):
        """Test main function with dry-run and message."""
        # Capture stdout
        captured_output = StringIO()
        with patch("sys.stdout", captured_output):
            result = main(["-m", "test message", "--dry-run"])

        self.assertEqual(result, 0)
        self.assertIn("Would execute: claude --dangerously-skip-permissions test message", captured_output.getvalue())

    def test_main_with_dry_run_prompt(self):
        """Test main function with dry-run and prompt."""
        # Capture stdout
        captured_output = StringIO()
        with patch("sys.stdout", captured_output):
            result = main(["-p", "say hello and exit", "--dry-run"])

        self.assertEqual(result, 0)
        expected_output = "Would execute: claude --dangerously-skip-permissions -p say hello and exit --output-format stream-json --verbose"
        self.assertEqual(captured_output.getvalue().strip(), expected_output)


if __name__ == "__main__":
    unittest.main()
