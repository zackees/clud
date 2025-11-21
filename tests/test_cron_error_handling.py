"""
Comprehensive error handling tests for cron scheduler.

Tests retry logic, validation, crash recovery, and failure scenarios.
"""

import tempfile
import time
import unittest
from datetime import datetime
from pathlib import Path
from unittest.mock import patch

from clud.cron.config import CronConfigManager
from clud.cron.daemon import CronDaemon
from clud.cron.executor import TaskExecutor
from clud.cron.models import CronTask
from clud.cron.scheduler import CronScheduler


class TestTaskExecutorRetry(unittest.TestCase):
    """Test retry logic with exponential backoff."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.test_dir = tempfile.mkdtemp(prefix="clud_cron_error_")
        self.executor = TaskExecutor(log_directory=self.test_dir)
        self.task = CronTask(
            cron_expression="0 9 * * *",
            task_file_path="/fake/task.md",
        )

    def test_retry_on_failure(self) -> None:
        """Test that failed tasks are retried with exponential backoff."""
        # Mock subprocess to fail twice, then succeed
        call_count = 0

        def mock_run_subprocess(cmd: list[str], log_file: Path, attempt: int = 0) -> int:
            nonlocal call_count
            call_count += 1
            if call_count <= 2:
                return 1  # Failure
            return 0  # Success on third try

        with patch.object(self.executor, "_run_subprocess", side_effect=mock_run_subprocess):
            return_code, log_file = self.executor.execute_task(self.task)

        # Should succeed after retries
        self.assertEqual(return_code, 0)
        self.assertEqual(call_count, 3)  # Initial + 2 retries

    def test_max_retries_exhausted(self) -> None:
        """Test that task fails after MAX_RETRIES attempts."""

        # Mock subprocess to always fail
        def mock_run_subprocess(cmd: list[str], log_file: Path, attempt: int = 0) -> int:
            return 1  # Always fail

        with patch.object(self.executor, "_run_subprocess", side_effect=mock_run_subprocess):
            return_code, log_file = self.executor.execute_task(self.task)

        # Should fail after all retries
        self.assertEqual(return_code, 1)

    def test_exponential_backoff_timing(self) -> None:
        """Test that retry delays follow exponential backoff pattern."""
        # Mock subprocess to fail on first two attempts
        call_times: list[float] = []

        def mock_run_subprocess(cmd: list[str], log_file: Path, attempt: int = 0) -> int:
            call_times.append(time.time())
            if len(call_times) <= 2:
                return 1  # Failure
            return 0  # Success

        with patch.object(self.executor, "_run_subprocess", side_effect=mock_run_subprocess):
            self.executor.execute_task(self.task)

        # Verify backoff delays (2s, 4s between attempts)
        # Allow some tolerance for system delays
        self.assertEqual(len(call_times), 3)
        delay1 = call_times[1] - call_times[0]
        delay2 = call_times[2] - call_times[1]

        # First retry delay should be ~2s (2^0 * 2)
        self.assertGreaterEqual(delay1, 1.8)
        self.assertLessEqual(delay1, 2.5)

        # Second retry delay should be ~4s (2^1 * 2)
        self.assertGreaterEqual(delay2, 3.8)
        self.assertLessEqual(delay2, 4.5)

    def test_no_retry_on_success(self) -> None:
        """Test that successful tasks don't trigger retries."""
        call_count = 0

        def mock_run_subprocess(cmd: list[str], log_file: Path, attempt: int = 0) -> int:
            nonlocal call_count
            call_count += 1
            return 0  # Success

        with patch.object(self.executor, "_run_subprocess", side_effect=mock_run_subprocess):
            return_code, log_file = self.executor.execute_task(self.task)

        # Should succeed on first try, no retries
        self.assertEqual(return_code, 0)
        self.assertEqual(call_count, 1)


class TestSchedulerValidation(unittest.TestCase):
    """Test scheduler validation logic."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.test_dir = tempfile.mkdtemp(prefix="clud_cron_error_")
        config_path = Path(self.test_dir) / "cron.json"
        self.config_manager = CronConfigManager(config_path=config_path)
        self.scheduler = CronScheduler(config_manager=self.config_manager)

        # Create a valid test task file
        self.task_file = Path(self.test_dir) / "task.md"
        self.task_file.write_text("Test task content", encoding="utf-8")

    def test_invalid_cron_expression(self) -> None:
        """Test that invalid cron expressions are rejected."""
        with self.assertRaises(ValueError) as cm:
            self.scheduler.add_task("invalid cron", str(self.task_file))

        self.assertIn("Invalid cron expression", str(cm.exception))

    def test_missing_task_file(self) -> None:
        """Test that missing task files are rejected."""
        missing_file = Path(self.test_dir) / "nonexistent.md"

        with self.assertRaises(ValueError) as cm:
            self.scheduler.add_task("0 9 * * *", str(missing_file))

        self.assertIn("does not exist", str(cm.exception))

    def test_task_file_is_directory(self) -> None:
        """Test that directories are rejected as task files."""
        dir_path = Path(self.test_dir) / "subdir"
        dir_path.mkdir()

        with self.assertRaises(ValueError) as cm:
            self.scheduler.add_task("0 9 * * *", str(dir_path))

        self.assertIn("not a file", str(cm.exception))

    def test_duplicate_task_detection(self) -> None:
        """Test that duplicate tasks are rejected."""
        # Add first task
        task1 = self.scheduler.add_task("0 9 * * *", str(self.task_file))
        self.assertIsNotNone(task1)

        # Try to add duplicate task
        with self.assertRaises(ValueError) as cm:
            self.scheduler.add_task("0 9 * * *", str(self.task_file))

        self.assertIn("Duplicate task", str(cm.exception))

    def test_validate_task_files(self) -> None:
        """Test validation of existing task files."""
        # Add task with valid file
        self.scheduler.add_task("0 9 * * *", str(self.task_file))

        # Add task with file that will be deleted
        temp_file = Path(self.test_dir) / "temp.md"
        temp_file.write_text("Temporary", encoding="utf-8")
        task2 = self.scheduler.add_task("0 10 * * *", str(temp_file))

        # Delete the temp file
        temp_file.unlink()

        # Validate task files
        missing_files = self.scheduler.validate_task_files()

        # Should detect one missing file
        self.assertEqual(len(missing_files), 1)
        self.assertEqual(missing_files[0][0], task2.id)
        self.assertEqual(missing_files[0][1], str(temp_file))


class TestFailureTracking(unittest.TestCase):
    """Test failure tracking and auto-disable logic."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.test_dir = tempfile.mkdtemp(prefix="clud_cron_error_")
        config_path = Path(self.test_dir) / "cron.json"
        self.config_manager = CronConfigManager(config_path=config_path)
        self.scheduler = CronScheduler(config_manager=self.config_manager)

        # Create test task
        task_file = Path(self.test_dir) / "task.md"
        task_file.write_text("Test", encoding="utf-8")
        self.task = self.scheduler.add_task("0 9 * * *", str(task_file))

    def test_consecutive_failures_increment(self) -> None:
        """Test that consecutive failures are tracked."""
        # Initial state
        self.assertEqual(self.task.consecutive_failures, 0)

        # Mark as failed
        self.scheduler.update_task_after_execution(self.task.id, success=False)

        # Reload task
        config = self.config_manager.load()
        task = config.get_task_by_id(self.task.id)
        self.assertIsNotNone(task)
        self.assertEqual(task.consecutive_failures, 1)

        # Mark as failed again
        self.scheduler.update_task_after_execution(self.task.id, success=False)

        # Reload task
        config = self.config_manager.load()
        task = config.get_task_by_id(self.task.id)
        self.assertEqual(task.consecutive_failures, 2)

    def test_success_resets_failure_count(self) -> None:
        """Test that success resets failure counters."""
        # Mark as failed twice
        self.scheduler.update_task_after_execution(self.task.id, success=False)
        self.scheduler.update_task_after_execution(self.task.id, success=False)

        # Mark as successful
        self.scheduler.update_task_after_execution(self.task.id, success=True)

        # Reload task
        config = self.config_manager.load()
        task = config.get_task_by_id(self.task.id)
        self.assertEqual(task.consecutive_failures, 0)
        self.assertIsNone(task.last_failure_time)

    def test_auto_disable_after_max_failures(self) -> None:
        """Test that task is auto-disabled after MAX_CONSECUTIVE_FAILURES."""
        # Fail the task MAX_CONSECUTIVE_FAILURES times
        for _ in range(TaskExecutor.MAX_CONSECUTIVE_FAILURES):
            self.scheduler.update_task_after_execution(self.task.id, success=False)

        # Reload task
        config = self.config_manager.load()
        task = config.get_task_by_id(self.task.id)

        # Should be disabled
        self.assertFalse(task.enabled)
        self.assertEqual(task.consecutive_failures, TaskExecutor.MAX_CONSECUTIVE_FAILURES)

    def test_last_failure_time_updated(self) -> None:
        """Test that last_failure_time is updated on failure."""
        # Initial state
        self.assertIsNone(self.task.last_failure_time)

        # Mark as failed
        before_time = datetime.now().timestamp()
        self.scheduler.update_task_after_execution(self.task.id, success=False)
        after_time = datetime.now().timestamp()

        # Reload task
        config = self.config_manager.load()
        task = config.get_task_by_id(self.task.id)

        # Should have failure time set
        self.assertIsNotNone(task)
        self.assertIsNotNone(task.last_failure_time)
        assert task.last_failure_time is not None  # Type narrowing for pyright
        self.assertGreaterEqual(task.last_failure_time, before_time)
        self.assertLessEqual(task.last_failure_time, after_time)


class TestDaemonCrashRecovery(unittest.TestCase):
    """Test daemon crash recovery logic."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.test_dir = tempfile.mkdtemp(prefix="clud_cron_error_")
        self.daemon = CronDaemon(config_dir=self.test_dir)

        # Create test task file
        self.task_file = Path(self.test_dir) / "task.md"
        self.task_file.write_text("Test", encoding="utf-8")

    def test_crash_recovery_validates_files(self) -> None:
        """Test that crash recovery validates task files."""
        # Add task with valid file
        self.daemon.scheduler.add_task("0 9 * * *", str(self.task_file))

        # Add task with missing file
        missing_file = Path(self.test_dir) / "missing.md"
        # Manually create task to bypass validation
        task2 = CronTask(
            cron_expression="0 10 * * *",
            task_file_path=str(missing_file),
        )
        config = self.daemon.config_manager.load()
        config.add_task(task2)
        self.daemon.config_manager.save(config)

        # Run crash recovery (should log warning but not crash)
        self.daemon._perform_crash_recovery()

        # Both tasks should still exist in config
        config = self.daemon.config_manager.load()
        self.assertEqual(len(config.tasks), 2)

    def test_crash_recovery_recalculates_past_next_run(self) -> None:
        """Test that crash recovery recalculates past next_run times."""
        # Create task with next_run in the past
        task = CronTask(
            cron_expression="0 9 * * *",
            task_file_path=str(self.task_file),
        )
        # Set next_run to 1 hour ago
        past_time = datetime.now().timestamp() - 3600
        task.next_run = past_time

        config = self.daemon.config_manager.load()
        config.add_task(task)
        self.daemon.config_manager.save(config)

        # Run crash recovery
        self.daemon._perform_crash_recovery()

        # Reload task
        config = self.daemon.config_manager.load()
        recovered_task = config.get_task_by_id(task.id)

        # next_run should be recalculated to the future
        self.assertIsNotNone(recovered_task)
        assert recovered_task is not None and recovered_task.next_run is not None  # Type narrowing
        self.assertGreater(recovered_task.next_run, datetime.now().timestamp())

    def test_crash_recovery_skips_disabled_tasks(self) -> None:
        """Test that crash recovery skips disabled tasks."""
        # Create disabled task with past next_run
        task = CronTask(
            cron_expression="0 9 * * *",
            task_file_path=str(self.task_file),
        )
        task.enabled = False
        task.next_run = datetime.now().timestamp() - 3600  # 1 hour ago

        config = self.daemon.config_manager.load()
        config.add_task(task)
        self.daemon.config_manager.save(config)

        old_next_run = task.next_run

        # Run crash recovery
        self.daemon._perform_crash_recovery()

        # Reload task
        config = self.daemon.config_manager.load()
        recovered_task = config.get_task_by_id(task.id)

        # next_run should NOT be recalculated (task is disabled)
        self.assertIsNotNone(recovered_task)
        self.assertEqual(recovered_task.next_run, old_next_run)


class TestDaemonTaskExecution(unittest.TestCase):
    """Test daemon task execution with error handling."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.test_dir = tempfile.mkdtemp(prefix="clud_cron_error_")
        self.daemon = CronDaemon(config_dir=self.test_dir)

        # Create test task file
        self.task_file = Path(self.test_dir) / "task.md"
        self.task_file.write_text("Test", encoding="utf-8")

    def test_missing_file_during_execution(self) -> None:
        """Test that missing task files are handled gracefully during execution."""
        # Add task
        task = self.daemon.scheduler.add_task("0 9 * * *", str(self.task_file))

        # Set next_run to now (task is due) and save
        config = self.daemon.config_manager.load()
        task_obj = config.get_task_by_id(task.id)
        assert task_obj is not None
        task_obj.next_run = datetime.now().timestamp()
        self.daemon.config_manager.save(config)

        # Delete task file
        self.task_file.unlink()

        # Get due tasks
        due_tasks = self.daemon.scheduler.check_due_tasks()
        self.assertEqual(len(due_tasks), 1)

        # Mock executor to verify it's not called
        with patch.object(self.daemon.executor, "execute_task") as mock_execute:
            # Simulate daemon loop processing due task
            for task_obj in due_tasks:
                task_path = Path(task_obj.task_file_path).expanduser()
                if not task_path.exists() or not task_path.is_file():
                    # This is what daemon does
                    execution_time = datetime.now()
                    self.daemon.scheduler.update_task_after_execution(task_obj.id, execution_time, success=False)
                    continue

            # Executor should NOT be called
            mock_execute.assert_not_called()

        # Verify task was marked as failed
        config = self.daemon.config_manager.load()
        failed_task = config.get_task_by_id(task.id)
        self.assertEqual(failed_task.consecutive_failures, 1)


if __name__ == "__main__":
    unittest.main()
