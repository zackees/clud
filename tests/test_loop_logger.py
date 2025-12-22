"""Unit tests for loop logger functionality."""

import tempfile
import unittest
from pathlib import Path

from clud.agent.loop_logger import LoopLogger


class TestLoopLogger(unittest.TestCase):
    """Test cases for LoopLogger class."""

    def test_logger_creates_log_file(self) -> None:
        """Test that LoopLogger creates log file on context manager entry."""
        with tempfile.TemporaryDirectory() as tmpdir:
            log_file = Path(tmpdir) / "test_log.txt"

            # Log file should not exist yet
            self.assertFalse(log_file.exists())

            # Enter context manager
            with LoopLogger(log_file):
                # Log file should now exist
                self.assertTrue(log_file.exists())

    def test_logger_writes_to_file(self) -> None:
        """Test that LoopLogger writes output to log file."""
        with tempfile.TemporaryDirectory() as tmpdir:
            log_file = Path(tmpdir) / "test_log.txt"

            with LoopLogger(log_file) as logger:
                # Write some test data
                logger.write_stdout("test stdout output\n")
                logger.write_stderr("test stderr output\n")

            # Read log file and verify content
            log_content = log_file.read_text(encoding="utf-8")
            self.assertIn("test stdout output", log_content)
            self.assertIn("test stderr output", log_content)

    def test_logger_appends_to_existing_file(self) -> None:
        """Test that LoopLogger appends to existing log file."""
        with tempfile.TemporaryDirectory() as tmpdir:
            log_file = Path(tmpdir) / "test_log.txt"

            # Write initial content
            with LoopLogger(log_file) as logger:
                logger.write_stdout("first write\n")

            # Append more content
            with LoopLogger(log_file) as logger:
                logger.write_stdout("second write\n")

            # Read log file and verify both writes are present
            log_content = log_file.read_text(encoding="utf-8")
            self.assertIn("first write", log_content)
            self.assertIn("second write", log_content)

    def test_logger_print_methods(self) -> None:
        """Test that print_stdout and print_stderr methods work correctly."""
        with tempfile.TemporaryDirectory() as tmpdir:
            log_file = Path(tmpdir) / "test_log.txt"

            with LoopLogger(log_file) as logger:
                # Use print methods
                logger.print_stdout("stdout message")
                logger.print_stderr("stderr message")

            # Read log file and verify content
            log_content = log_file.read_text(encoding="utf-8")
            self.assertIn("stdout message", log_content)
            self.assertIn("stderr message", log_content)

    def test_logger_handles_unicode(self) -> None:
        """Test that LoopLogger handles unicode characters correctly."""
        with tempfile.TemporaryDirectory() as tmpdir:
            log_file = Path(tmpdir) / "test_log.txt"

            with LoopLogger(log_file) as logger:
                # Write unicode characters
                logger.write_stdout("✅ Success\n")
                logger.write_stderr("🔧 Tool use\n")
                logger.write_stdout("📋 DONE.md\n")

            # Read log file and verify unicode is preserved
            log_content = log_file.read_text(encoding="utf-8")
            self.assertIn("✅ Success", log_content)
            self.assertIn("🔧 Tool use", log_content)
            self.assertIn("📋 DONE.md", log_content)


if __name__ == "__main__":
    unittest.main()
