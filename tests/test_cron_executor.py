"""Unit tests for cron task executor module."""

import shutil
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from clud.cron.executor import TaskExecutor
from clud.cron.models import CronTask


class TestTaskExecutor(unittest.TestCase):
    """Test cases for TaskExecutor class."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        # Create temporary directory for test logs
        self.test_dir = Path(tempfile.mkdtemp(prefix="test_cron_executor_"))
        self.log_dir = self.test_dir / "logs"
        self.executor = TaskExecutor(str(self.log_dir), test_mode=True)

    def tearDown(self) -> None:
        """Clean up test fixtures."""
        if self.test_dir.exists():
            shutil.rmtree(self.test_dir)

    def test_init_default_log_directory(self) -> None:
        """Test initialization with default log directory."""
        executor = TaskExecutor()
        expected = Path.home() / ".clud" / "logs" / "cron"
        self.assertEqual(executor.log_directory, expected)

    def test_init_custom_log_directory(self) -> None:
        """Test initialization with custom log directory."""
        executor = TaskExecutor("/custom/log/path")
        self.assertEqual(executor.log_directory, Path("/custom/log/path"))

    def test_init_expanduser_log_directory(self) -> None:
        """Test initialization expands ~ in log directory."""
        executor = TaskExecutor("~/custom/logs")
        expected = Path.home() / "custom" / "logs"
        self.assertEqual(executor.log_directory, expected)

    @patch("subprocess.run")
    def test_execute_task_success(self, mock_run: Mock) -> None:
        """Test executing task with successful return code."""
        # Configure mock
        mock_result = Mock()
        mock_result.returncode = 0
        mock_run.return_value = mock_result

        # Create task
        task = CronTask(cron_expression="0 9 * * *", task_file_path="/tmp/task.md")

        # Execute
        return_code, log_file = self.executor.execute_task(task)

        # Verify
        self.assertEqual(return_code, 0)
        self.assertTrue(log_file.exists())
        self.assertTrue(log_file.parent.name == task.id)
        self.assertTrue(log_file.name.endswith(".log"))

        # Verify subprocess was called
        mock_run.assert_called_once()
        call_args = mock_run.call_args
        self.assertEqual(call_args[0][0][0], sys.executable)
        self.assertEqual(call_args[0][0][1:4], ["-m", "clud", "-f"])
        self.assertEqual(call_args[0][0][4], "/tmp/task.md")

    @patch("subprocess.run")
    def test_execute_task_failure(self, mock_run: Mock) -> None:
        """Test executing task with non-zero return code."""
        # Configure mock
        mock_result = Mock()
        mock_result.returncode = 1
        mock_run.return_value = mock_result

        # Create task
        task = CronTask(cron_expression="0 9 * * *", task_file_path="/tmp/task.md")

        # Execute
        return_code, log_file = self.executor.execute_task(task)

        # Verify
        self.assertEqual(return_code, 1)
        self.assertTrue(log_file.exists())

    @patch("subprocess.run")
    def test_execute_task_creates_log_directory(self, mock_run: Mock) -> None:
        """Test executing task creates log directory structure."""
        # Configure mock
        mock_result = Mock()
        mock_result.returncode = 0
        mock_run.return_value = mock_result

        # Create task
        task = CronTask(cron_expression="0 9 * * *", task_file_path="/tmp/task.md")

        # Verify log directory doesn't exist
        task_log_dir = self.log_dir / task.id
        self.assertFalse(task_log_dir.exists())

        # Execute
        return_code, log_file = self.executor.execute_task(task)

        # Verify log directory was created
        self.assertTrue(task_log_dir.exists())
        self.assertTrue(task_log_dir.is_dir())

    @patch("subprocess.run")
    def test_execute_task_log_file_naming(self, mock_run: Mock) -> None:
        """Test log file naming includes timestamp."""
        # Configure mock
        mock_result = Mock()
        mock_result.returncode = 0
        mock_run.return_value = mock_result

        # Create task
        task = CronTask(cron_expression="0 9 * * *", task_file_path="/tmp/task.md")

        # Execute
        return_code, log_file = self.executor.execute_task(task)

        # Verify log file name format (YYYYMMDD_HHMMSS.log)
        log_name = log_file.name
        self.assertTrue(log_name.endswith(".log"))
        # Should match format: 20250115_093000.log
        name_without_ext = log_name[:-4]
        parts = name_without_ext.split("_")
        self.assertEqual(len(parts), 2)
        self.assertEqual(len(parts[0]), 8)  # YYYYMMDD
        self.assertEqual(len(parts[1]), 6)  # HHMMSS

    def test_run_subprocess_command_not_found(self) -> None:
        """Test subprocess execution with command not found."""
        log_file = self.test_dir / "test.log"
        cmd = ["nonexistent-command-xyz", "--arg"]

        return_code = self.executor._run_subprocess(cmd, log_file)

        self.assertEqual(return_code, 127)  # Command not found
        self.assertTrue(log_file.exists())

        # Verify error was logged
        log_content = log_file.read_text(encoding="utf-8")
        self.assertIn("Command not found", log_content)

    def test_run_subprocess_writes_metadata(self) -> None:
        """Test subprocess execution writes execution metadata to log."""
        log_file = self.test_dir / "test.log"
        # Use a simple command that exists on all platforms
        cmd = [sys.executable, "--version"]

        return_code = self.executor._run_subprocess(cmd, log_file)

        self.assertEqual(return_code, 0)
        self.assertTrue(log_file.exists())

        # Verify metadata was written
        log_content = log_file.read_text(encoding="utf-8")
        self.assertIn("=== Cron Task Execution ===", log_content)
        self.assertIn("Timestamp:", log_content)
        self.assertIn("Command:", log_content)
        self.assertIn("Return code:", log_content)
        self.assertIn("Completed:", log_content)

    def test_run_subprocess_captures_output(self) -> None:
        """Test subprocess execution captures stdout."""
        log_file = self.test_dir / "test.log"
        # Use echo command to test output capture
        if sys.platform == "win32":
            cmd = ["cmd", "/c", "echo", "test output"]
        else:
            cmd = ["echo", "test output"]

        return_code = self.executor._run_subprocess(cmd, log_file)

        self.assertEqual(return_code, 0)
        self.assertTrue(log_file.exists())

        # Verify output was captured
        log_content = log_file.read_text(encoding="utf-8")
        self.assertIn("test output", log_content)

    @patch("subprocess.run")
    def test_run_subprocess_permission_error(self, mock_run: Mock) -> None:
        """Test subprocess execution with permission error."""
        # Configure mock to raise PermissionError
        mock_run.side_effect = PermissionError("Permission denied")

        log_file = self.test_dir / "test.log"
        cmd = [sys.executable, "--version"]

        return_code = self.executor._run_subprocess(cmd, log_file)

        self.assertEqual(return_code, 126)  # Permission denied
        self.assertTrue(log_file.exists())

        # Verify error was logged
        log_content = log_file.read_text(encoding="utf-8")
        self.assertIn("Permission denied", log_content)

    @patch("subprocess.run")
    def test_run_subprocess_generic_exception(self, mock_run: Mock) -> None:
        """Test subprocess execution with generic exception."""
        # Configure mock to raise generic exception
        mock_run.side_effect = RuntimeError("Something went wrong")

        log_file = self.test_dir / "test.log"
        cmd = [sys.executable, "--version"]

        return_code = self.executor._run_subprocess(cmd, log_file)

        self.assertEqual(return_code, 1)  # Generic error
        self.assertTrue(log_file.exists())

        # Verify error was logged
        log_content = log_file.read_text(encoding="utf-8")
        self.assertIn("Execution failed", log_content)

    def test_write_error_log(self) -> None:
        """Test writing error log."""
        log_file = self.test_dir / "error.log"
        error_message = "Test error message"

        self.executor._write_error_log(log_file, error_message)

        self.assertTrue(log_file.exists())
        log_content = log_file.read_text(encoding="utf-8")
        self.assertIn("=== Cron Task Execution Error ===", log_content)
        self.assertIn("Timestamp:", log_content)
        self.assertIn("Error:", log_content)
        self.assertIn(error_message, log_content)

    @patch("builtins.open")
    def test_write_error_log_write_failure(self, mock_open: Mock) -> None:
        """Test error log writing when file write fails."""
        # Configure mock to raise exception
        mock_open.side_effect = OSError("Disk full")

        log_file = self.test_dir / "error.log"

        # Should not raise exception
        self.executor._write_error_log(log_file, "Test error")

    def test_get_task_logs_empty(self) -> None:
        """Test getting task logs when none exist."""
        logs = self.executor.get_task_logs("nonexistent-task-id")
        self.assertEqual(len(logs), 0)

    @patch("subprocess.run")
    def test_get_task_logs_multiple(self, mock_run: Mock) -> None:
        """Test getting task logs with multiple log files."""
        # Configure mock
        mock_result = Mock()
        mock_result.returncode = 0
        mock_run.return_value = mock_result

        # Create task and execute multiple times
        task = CronTask(cron_expression="0 9 * * *", task_file_path="/tmp/task.md")

        # Execute 3 times (with small delays to ensure different timestamps)
        self.executor.execute_task(task)
        time.sleep(1.1)  # Ensure different timestamp (YYYYMMDD_HHMMSS format has 1-second precision)
        self.executor.execute_task(task)
        time.sleep(1.1)
        log3 = self.executor.execute_task(task)[1]

        # Get logs
        logs = self.executor.get_task_logs(task.id)

        # Should return 3 logs, sorted by modification time (newest first)
        self.assertEqual(len(logs), 3)
        # Last executed should be first in list
        self.assertEqual(logs[0], log3)

    def test_get_latest_log_none(self) -> None:
        """Test getting latest log when none exist."""
        latest = self.executor.get_latest_log("nonexistent-task-id")
        self.assertIsNone(latest)

    @patch("subprocess.run")
    def test_get_latest_log_exists(self, mock_run: Mock) -> None:
        """Test getting latest log when logs exist."""
        # Configure mock
        mock_result = Mock()
        mock_result.returncode = 0
        mock_run.return_value = mock_result

        # Create task and execute
        task = CronTask(cron_expression="0 9 * * *", task_file_path="/tmp/task.md")
        self.executor.execute_task(task)
        log2 = self.executor.execute_task(task)[1]

        # Get latest
        latest = self.executor.get_latest_log(task.id)

        # Should return most recent log
        self.assertIsNotNone(latest)
        self.assertEqual(latest, log2)


if __name__ == "__main__":
    unittest.main()
