"""Unit tests for cron scheduler module."""

import shutil
import tempfile
import unittest
from datetime import datetime
from pathlib import Path

from clud.cron.config import CronConfigManager
from clud.cron.scheduler import CronScheduler


class TestCronScheduler(unittest.TestCase):
    """Test cases for CronScheduler class."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        # Create temporary directory for test config
        self.test_dir = Path(tempfile.mkdtemp(prefix="test_cron_scheduler_"))
        self.config_path = self.test_dir / "cron.json"
        self.config_manager = CronConfigManager(self.config_path)
        self.scheduler = CronScheduler(self.config_manager)

        # Create test task files for validation
        self.test_task_file = self.test_dir / "task.md"
        self.test_task_file.write_text("Test task content", encoding="utf-8")
        self.test_task_file1 = self.test_dir / "task1.md"
        self.test_task_file1.write_text("Test task 1 content", encoding="utf-8")
        self.test_task_file2 = self.test_dir / "task2.md"
        self.test_task_file2.write_text("Test task 2 content", encoding="utf-8")

    def tearDown(self) -> None:
        """Clean up test fixtures."""
        if self.test_dir.exists():
            shutil.rmtree(self.test_dir)

    def test_add_task_valid_cron(self) -> None:
        """Test adding task with valid cron expression."""
        task = self.scheduler.add_task("0 9 * * *", str(self.test_task_file))

        self.assertIsNotNone(task.id)
        self.assertEqual(task.cron_expression, "0 9 * * *")
        self.assertEqual(task.task_file_path, str(self.test_task_file))
        self.assertTrue(task.enabled)
        self.assertIsNotNone(task.next_run)

        # Verify task was persisted
        config = self.config_manager.load()
        self.assertEqual(len(config.tasks), 1)
        self.assertEqual(config.tasks[0].id, task.id)

    def test_add_task_invalid_cron(self) -> None:
        """Test adding task with invalid cron expression."""
        with self.assertRaises(ValueError) as ctx:
            self.scheduler.add_task("invalid cron", str(self.test_task_file))

        self.assertIn("Invalid cron expression", str(ctx.exception))

        # Verify no task was added
        config = self.config_manager.load()
        self.assertEqual(len(config.tasks), 0)

    def test_add_multiple_tasks(self) -> None:
        """Test adding multiple tasks."""
        task1 = self.scheduler.add_task("0 9 * * *", str(self.test_task_file1))
        task2 = self.scheduler.add_task("0 12 * * *", str(self.test_task_file2))

        config = self.config_manager.load()
        self.assertEqual(len(config.tasks), 2)
        self.assertNotEqual(task1.id, task2.id)

    def test_remove_task_existing(self) -> None:
        """Test removing existing task."""
        task = self.scheduler.add_task("0 9 * * *", str(self.test_task_file))
        result = self.scheduler.remove_task(task.id)

        self.assertTrue(result)

        # Verify task was removed
        config = self.config_manager.load()
        self.assertEqual(len(config.tasks), 0)

    def test_remove_task_nonexistent(self) -> None:
        """Test removing non-existent task."""
        result = self.scheduler.remove_task("nonexistent-id")

        self.assertFalse(result)

    def test_get_next_run_time_basic(self) -> None:
        """Test calculating next run time."""
        # Daily at 9:00 AM
        base_time = datetime(2025, 1, 15, 8, 0, 0)  # 8:00 AM
        next_run = self.scheduler.get_next_run_time("0 9 * * *", base_time)

        # Should be 9:00 AM same day
        expected = datetime(2025, 1, 15, 9, 0, 0)
        self.assertEqual(next_run, expected.timestamp())

    def test_get_next_run_time_past_time(self) -> None:
        """Test calculating next run time when already past today's time."""
        # Daily at 9:00 AM
        base_time = datetime(2025, 1, 15, 10, 0, 0)  # 10:00 AM (past 9:00)
        next_run = self.scheduler.get_next_run_time("0 9 * * *", base_time)

        # Should be 9:00 AM next day
        expected = datetime(2025, 1, 16, 9, 0, 0)
        self.assertEqual(next_run, expected.timestamp())

    def test_get_next_run_time_hourly(self) -> None:
        """Test calculating next run time for hourly schedule."""
        # Every hour at :30
        base_time = datetime(2025, 1, 15, 10, 0, 0)  # 10:00 AM
        next_run = self.scheduler.get_next_run_time("30 * * * *", base_time)

        # Should be 10:30 AM same day
        expected = datetime(2025, 1, 15, 10, 30, 0)
        self.assertEqual(next_run, expected.timestamp())

    def test_get_next_run_time_invalid_cron(self) -> None:
        """Test calculating next run time with invalid cron expression."""
        with self.assertRaises(ValueError) as ctx:
            self.scheduler.get_next_run_time("invalid cron")

        self.assertIn("Invalid cron expression", str(ctx.exception))

    def test_check_due_tasks_none_due(self) -> None:
        """Test checking for due tasks when none are due."""
        # Add task scheduled for 9:00 AM
        self.scheduler.add_task("0 9 * * *", str(self.test_task_file))

        # Check at 8:00 AM (before scheduled time)
        current_time = datetime(2025, 1, 15, 8, 0, 0)
        due_tasks = self.scheduler.check_due_tasks(current_time)

        self.assertEqual(len(due_tasks), 0)

    def test_check_due_tasks_one_due(self) -> None:
        """Test checking for due tasks when one is due."""
        # Add task scheduled for 9:00 AM
        task = self.scheduler.add_task("0 9 * * *", str(self.test_task_file))

        # Manually set next_run to past time
        config = self.config_manager.load()
        config.tasks[0].next_run = datetime(2025, 1, 15, 9, 0, 0).timestamp()
        self.config_manager.save(config)

        # Check at 10:00 AM (after scheduled time)
        current_time = datetime(2025, 1, 15, 10, 0, 0)
        due_tasks = self.scheduler.check_due_tasks(current_time)

        self.assertEqual(len(due_tasks), 1)
        self.assertEqual(due_tasks[0].id, task.id)

    def test_check_due_tasks_multiple_due(self) -> None:
        """Test checking for due tasks when multiple are due."""
        # Add two tasks
        task1 = self.scheduler.add_task("0 9 * * *", str(self.test_task_file1))
        task2 = self.scheduler.add_task("0 10 * * *", str(self.test_task_file2))

        # Manually set both next_run to past times
        config = self.config_manager.load()
        config.tasks[0].next_run = datetime(2025, 1, 15, 9, 0, 0).timestamp()
        config.tasks[1].next_run = datetime(2025, 1, 15, 10, 0, 0).timestamp()
        self.config_manager.save(config)

        # Check at 11:00 AM (after both scheduled times)
        current_time = datetime(2025, 1, 15, 11, 0, 0)
        due_tasks = self.scheduler.check_due_tasks(current_time)

        self.assertEqual(len(due_tasks), 2)
        due_ids = {task.id for task in due_tasks}
        self.assertEqual(due_ids, {task1.id, task2.id})

    def test_check_due_tasks_disabled_task(self) -> None:
        """Test checking for due tasks skips disabled tasks."""
        # Add task and disable it
        self.scheduler.add_task("0 9 * * *", str(self.test_task_file))

        config = self.config_manager.load()
        config.tasks[0].enabled = False
        config.tasks[0].next_run = datetime(2025, 1, 15, 9, 0, 0).timestamp()
        self.config_manager.save(config)

        # Check at 10:00 AM (after scheduled time)
        current_time = datetime(2025, 1, 15, 10, 0, 0)
        due_tasks = self.scheduler.check_due_tasks(current_time)

        self.assertEqual(len(due_tasks), 0)

    def test_check_due_tasks_missing_next_run(self) -> None:
        """Test checking for due tasks handles missing next_run."""
        # Add task
        self.scheduler.add_task("0 9 * * *", str(self.test_task_file))

        # Manually clear next_run
        config = self.config_manager.load()
        config.tasks[0].next_run = None
        self.config_manager.save(config)

        # Check - should recalculate next_run
        current_time = datetime(2025, 1, 15, 8, 0, 0)
        due_tasks = self.scheduler.check_due_tasks(current_time)

        # Should not be due yet (recalculated to 9:00 AM)
        self.assertEqual(len(due_tasks), 0)

    def test_update_task_after_execution(self) -> None:
        """Test updating task after execution."""
        # Add task
        task = self.scheduler.add_task("0 9 * * *", str(self.test_task_file))

        # Execute and update
        execution_time = datetime(2025, 1, 15, 9, 0, 0)
        self.scheduler.update_task_after_execution(task.id, execution_time)

        # Verify timestamps updated
        config = self.config_manager.load()
        updated_task = config.get_task_by_id(task.id)
        self.assertIsNotNone(updated_task)
        self.assertEqual(updated_task.last_run, execution_time.timestamp())
        self.assertIsNotNone(updated_task.next_run)
        # Next run should be 9:00 AM next day
        expected_next = datetime(2025, 1, 16, 9, 0, 0).timestamp()
        self.assertEqual(updated_task.next_run, expected_next)

    def test_update_task_after_execution_nonexistent(self) -> None:
        """Test updating non-existent task after execution."""
        # Should not raise exception
        self.scheduler.update_task_after_execution("nonexistent-id")

    def test_list_tasks_empty(self) -> None:
        """Test listing tasks when none exist."""
        tasks = self.scheduler.list_tasks()
        self.assertEqual(len(tasks), 0)

    def test_list_tasks_multiple(self) -> None:
        """Test listing multiple tasks."""
        task1 = self.scheduler.add_task("0 9 * * *", str(self.test_task_file1))
        task2 = self.scheduler.add_task("0 12 * * *", str(self.test_task_file2))

        tasks = self.scheduler.list_tasks()
        self.assertEqual(len(tasks), 2)
        task_ids = {task.id for task in tasks}
        self.assertEqual(task_ids, {task1.id, task2.id})

    def test_is_valid_cron_expression_valid(self) -> None:
        """Test cron expression validation with valid expressions."""
        valid_expressions = [
            "0 9 * * *",  # Daily at 9:00 AM
            "*/5 * * * *",  # Every 5 minutes
            "0 0 * * 0",  # Weekly on Sunday
            "0 0 1 * *",  # Monthly on 1st
            "0 0 1 1 *",  # Yearly on Jan 1st
        ]

        for expr in valid_expressions:
            self.assertTrue(
                self.scheduler._is_valid_cron_expression(expr),
                f"Expected '{expr}' to be valid",
            )

    def test_is_valid_cron_expression_invalid(self) -> None:
        """Test cron expression validation with invalid expressions."""
        invalid_expressions = [
            "invalid",
            "60 * * * *",  # Invalid minute
            "* 25 * * *",  # Invalid hour
            "* * 32 * *",  # Invalid day
            "",  # Empty string
        ]

        for expr in invalid_expressions:
            self.assertFalse(
                self.scheduler._is_valid_cron_expression(expr),
                f"Expected '{expr}' to be invalid",
            )


if __name__ == "__main__":
    unittest.main()
