"""Loop execution logging utilities.

This module provides utilities for logging all output during --loop execution
to .loop/log.txt.
"""

import sys
from pathlib import Path
from typing import Any, TextIO


class LoopLogger:
    """Logger that writes all output to .loop/log.txt while still displaying to console."""

    def __init__(self, log_file_path: Path) -> None:
        """Initialize the loop logger.

        Args:
            log_file_path: Path to the log file (.loop/log.txt)
        """
        self.log_file_path = log_file_path
        self.log_file: TextIO | None = None

    def __enter__(self) -> "LoopLogger":
        """Open log file for appending."""
        # Open in append mode with UTF-8 encoding
        # Use errors='replace' to handle any encoding issues gracefully
        self.log_file = open(self.log_file_path, "a", encoding="utf-8", errors="replace")
        return self

    def __exit__(self, exc_type: Any, exc_val: Any, exc_tb: Any) -> None:
        """Close log file."""
        if self.log_file:
            self.log_file.close()
            self.log_file = None

    def write_stdout(self, text: str) -> None:
        """Write text to both stdout and log file.

        Args:
            text: Text to write
        """
        sys.stdout.write(text)
        sys.stdout.flush()
        if self.log_file:
            self.log_file.write(text)
            self.log_file.flush()

    def write_stderr(self, text: str) -> None:
        """Write text to both stderr and log file.

        Args:
            text: Text to write
        """
        sys.stderr.write(text)
        sys.stderr.flush()
        if self.log_file:
            self.log_file.write(text)
            self.log_file.flush()

    def print_stdout(self, *args: Any, **kwargs: Any) -> None:
        """Print to stdout and log file (mimics print() function).

        Args:
            *args: Arguments to print
            **kwargs: Keyword arguments (end, sep, etc.)
        """
        # Capture what would be printed
        import io

        buffer = io.StringIO()
        print(*args, file=buffer, **kwargs)
        output = buffer.getvalue()

        # Write to both stdout and log
        sys.stdout.write(output)
        sys.stdout.flush()
        if self.log_file:
            self.log_file.write(output)
            self.log_file.flush()

    def print_stderr(self, *args: Any, **kwargs: Any) -> None:
        """Print to stderr and log file (mimics print() function).

        Args:
            *args: Arguments to print
            **kwargs: Keyword arguments (end, sep, etc.)
        """
        # Capture what would be printed
        import io

        buffer = io.StringIO()
        print(*args, file=buffer, **kwargs)
        output = buffer.getvalue()

        # Write to both stderr and log
        sys.stderr.write(output)
        sys.stderr.flush()
        if self.log_file:
            self.log_file.write(output)
            self.log_file.flush()


def create_logging_formatter_callback(formatter: Any, logger: LoopLogger) -> Any:
    """Create a callback function that formats JSON and logs output.

    Args:
        formatter: StreamJsonFormatter instance
        logger: LoopLogger instance for writing output

    Returns:
        Callback function that can be passed to RunningProcess
    """

    def callback(line: str) -> None:
        """Format and print a line of JSON output, and log it."""
        formatted = formatter.format_line(line)
        if formatted:
            logger.write_stdout(formatted)

    return callback
