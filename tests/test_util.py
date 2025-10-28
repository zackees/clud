"""Unit tests for clud utility functions."""

import logging
import unittest
from io import StringIO

from clud.util import handle_keyboard_interrupt


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


if __name__ == "__main__":
    unittest.main()
