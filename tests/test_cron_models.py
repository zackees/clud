"""Unit tests for cron data models."""

import unittest
from datetime import datetime
from typing import Any

from clud.cron.models import CronConfig, CronTask


class TestCronTask(unittest.TestCase):
    """Test cases for CronTask model."""

    def test_create_task_with_defaults(self) -> None:
        """Test creating a task with default values."""
        task = CronTask(cron_expression="0 9 * * *", task_file_path="/path/to/task.md")

        self.assertEqual(task.cron_expression, "0 9 * * *")
        self.assertEqual(task.task_file_path, "/path/to/task.md")
        self.assertTrue(task.enabled)
        self.assertIsNotNone(task.id)
        self.assertIsNotNone(task.created_at)
        self.assertIsNone(task.last_run)
        self.assertIsNone(task.next_run)

    def test_create_task_with_explicit_id(self) -> None:
        """Test creating a task with explicit ID."""
        task = CronTask(
            id="test-id-123",
            cron_expression="*/5 * * * *",
            task_file_path="/path/to/task.md",
        )

        self.assertEqual(task.id, "test-id-123")

    def test_create_task_with_all_fields(self) -> None:
        """Test creating a task with all fields specified."""
        created_time = datetime.now().timestamp()
        last_run_time = created_time + 100
        next_run_time = created_time + 200

        task = CronTask(
            id="custom-id",
            cron_expression="0 */2 * * *",
            task_file_path="/path/to/task.md",
            enabled=False,
            created_at=created_time,
            last_run=last_run_time,
            next_run=next_run_time,
        )

        self.assertEqual(task.id, "custom-id")
        self.assertEqual(task.cron_expression, "0 */2 * * *")
        self.assertEqual(task.task_file_path, "/path/to/task.md")
        self.assertFalse(task.enabled)
        self.assertEqual(task.created_at, created_time)
        self.assertEqual(task.last_run, last_run_time)
        self.assertEqual(task.next_run, next_run_time)

    def test_to_dict(self) -> None:
        """Test converting task to dictionary."""
        task = CronTask(
            id="test-id",
            cron_expression="0 9 * * *",
            task_file_path="/path/to/task.md",
            enabled=True,
            created_at=1234567890.0,
            last_run=1234567900.0,
            next_run=1234567910.0,
        )

        task_dict = task.to_dict()

        self.assertEqual(task_dict["id"], "test-id")
        self.assertEqual(task_dict["cron_expression"], "0 9 * * *")
        self.assertEqual(task_dict["task_file_path"], "/path/to/task.md")
        self.assertTrue(task_dict["enabled"])
        self.assertEqual(task_dict["created_at"], 1234567890.0)
        self.assertEqual(task_dict["last_run"], 1234567900.0)
        self.assertEqual(task_dict["next_run"], 1234567910.0)

    def test_from_dict(self) -> None:
        """Test creating task from dictionary."""
        data = {
            "id": "test-id",
            "cron_expression": "0 9 * * *",
            "task_file_path": "/path/to/task.md",
            "enabled": True,
            "created_at": 1234567890.0,
            "last_run": 1234567900.0,
            "next_run": 1234567910.0,
        }

        task = CronTask.from_dict(data)

        self.assertEqual(task.id, "test-id")
        self.assertEqual(task.cron_expression, "0 9 * * *")
        self.assertEqual(task.task_file_path, "/path/to/task.md")
        self.assertTrue(task.enabled)
        self.assertEqual(task.created_at, 1234567890.0)
        self.assertEqual(task.last_run, 1234567900.0)
        self.assertEqual(task.next_run, 1234567910.0)

    def test_from_dict_with_minimal_fields(self) -> None:
        """Test creating task from dictionary with minimal fields."""
        data = {
            "id": "test-id",
            "cron_expression": "0 9 * * *",
            "task_file_path": "/path/to/task.md",
        }

        task = CronTask.from_dict(data)

        self.assertEqual(task.id, "test-id")
        self.assertEqual(task.cron_expression, "0 9 * * *")
        self.assertEqual(task.task_file_path, "/path/to/task.md")
        self.assertTrue(task.enabled)  # Default value
        self.assertIsNotNone(task.created_at)  # Auto-generated
        self.assertIsNone(task.last_run)
        self.assertIsNone(task.next_run)

    def test_roundtrip_serialization(self) -> None:
        """Test that task can be serialized and deserialized without loss."""
        original = CronTask(
            id="test-id",
            cron_expression="0 9 * * *",
            task_file_path="/path/to/task.md",
            enabled=True,
            created_at=1234567890.0,
            last_run=1234567900.0,
            next_run=1234567910.0,
        )

        task_dict = original.to_dict()
        restored = CronTask.from_dict(task_dict)

        self.assertEqual(restored.id, original.id)
        self.assertEqual(restored.cron_expression, original.cron_expression)
        self.assertEqual(restored.task_file_path, original.task_file_path)
        self.assertEqual(restored.enabled, original.enabled)
        self.assertEqual(restored.created_at, original.created_at)
        self.assertEqual(restored.last_run, original.last_run)
        self.assertEqual(restored.next_run, original.next_run)


class TestCronConfig(unittest.TestCase):
    """Test cases for CronConfig model."""

    def test_create_empty_config(self) -> None:
        """Test creating an empty configuration."""
        config = CronConfig()

        self.assertEqual(len(config.tasks), 0)
        self.assertIsNone(config.daemon_pid)
        self.assertEqual(config.log_directory, "~/.clud/logs/cron")

    def test_create_config_with_tasks(self) -> None:
        """Test creating a configuration with tasks."""
        task1 = CronTask(cron_expression="0 9 * * *", task_file_path="/path/to/task1.md")
        task2 = CronTask(cron_expression="0 */2 * * *", task_file_path="/path/to/task2.md")

        config = CronConfig(tasks=[task1, task2])

        self.assertEqual(len(config.tasks), 2)
        self.assertEqual(config.tasks[0].cron_expression, "0 9 * * *")
        self.assertEqual(config.tasks[1].cron_expression, "0 */2 * * *")

    def test_create_config_with_daemon_pid(self) -> None:
        """Test creating a configuration with daemon PID."""
        config = CronConfig(daemon_pid=12345)

        self.assertEqual(config.daemon_pid, 12345)

    def test_create_config_with_custom_log_directory(self) -> None:
        """Test creating a configuration with custom log directory."""
        config = CronConfig(log_directory="/custom/log/path")

        self.assertEqual(config.log_directory, "/custom/log/path")

    def test_to_dict(self) -> None:
        """Test converting config to dictionary."""
        task = CronTask(
            id="test-id",
            cron_expression="0 9 * * *",
            task_file_path="/path/to/task.md",
        )
        config = CronConfig(tasks=[task], daemon_pid=12345, log_directory="/custom/log/path")

        config_dict = config.to_dict()

        self.assertEqual(len(config_dict["tasks"]), 1)
        self.assertEqual(config_dict["tasks"][0]["id"], "test-id")
        self.assertEqual(config_dict["daemon_pid"], 12345)
        self.assertEqual(config_dict["log_directory"], "/custom/log/path")

    def test_from_dict(self) -> None:
        """Test creating config from dictionary."""
        data = {
            "tasks": [
                {
                    "id": "test-id",
                    "cron_expression": "0 9 * * *",
                    "task_file_path": "/path/to/task.md",
                    "enabled": True,
                    "created_at": 1234567890.0,
                    "last_run": None,
                    "next_run": None,
                }
            ],
            "daemon_pid": 12345,
            "log_directory": "/custom/log/path",
        }

        config = CronConfig.from_dict(data)

        self.assertEqual(len(config.tasks), 1)
        self.assertEqual(config.tasks[0].id, "test-id")
        self.assertEqual(config.daemon_pid, 12345)
        self.assertEqual(config.log_directory, "/custom/log/path")

    def test_from_dict_with_minimal_fields(self) -> None:
        """Test creating config from dictionary with minimal fields."""
        data: dict[str, Any] = {"tasks": []}

        config = CronConfig.from_dict(data)

        self.assertEqual(len(config.tasks), 0)
        self.assertIsNone(config.daemon_pid)
        self.assertEqual(config.log_directory, "~/.clud/logs/cron")

    def test_get_task_by_id_found(self) -> None:
        """Test finding a task by ID when it exists."""
        task1 = CronTask(id="task-1", cron_expression="0 9 * * *", task_file_path="/path/to/task1.md")
        task2 = CronTask(id="task-2", cron_expression="0 */2 * * *", task_file_path="/path/to/task2.md")
        config = CronConfig(tasks=[task1, task2])

        found_task = config.get_task_by_id("task-2")

        self.assertIsNotNone(found_task)
        self.assertEqual(found_task.id, "task-2")  # type: ignore
        self.assertEqual(found_task.cron_expression, "0 */2 * * *")  # type: ignore

    def test_get_task_by_id_not_found(self) -> None:
        """Test finding a task by ID when it doesn't exist."""
        task = CronTask(id="task-1", cron_expression="0 9 * * *", task_file_path="/path/to/task.md")
        config = CronConfig(tasks=[task])

        found_task = config.get_task_by_id("nonexistent-id")

        self.assertIsNone(found_task)

    def test_add_task(self) -> None:
        """Test adding a task to the configuration."""
        config = CronConfig()
        task = CronTask(cron_expression="0 9 * * *", task_file_path="/path/to/task.md")

        self.assertEqual(len(config.tasks), 0)

        config.add_task(task)

        self.assertEqual(len(config.tasks), 1)
        self.assertEqual(config.tasks[0].cron_expression, "0 9 * * *")

    def test_remove_task_found(self) -> None:
        """Test removing a task that exists."""
        task1 = CronTask(id="task-1", cron_expression="0 9 * * *", task_file_path="/path/to/task1.md")
        task2 = CronTask(id="task-2", cron_expression="0 */2 * * *", task_file_path="/path/to/task2.md")
        config = CronConfig(tasks=[task1, task2])

        result = config.remove_task("task-1")

        self.assertTrue(result)
        self.assertEqual(len(config.tasks), 1)
        self.assertEqual(config.tasks[0].id, "task-2")

    def test_remove_task_not_found(self) -> None:
        """Test removing a task that doesn't exist."""
        task = CronTask(id="task-1", cron_expression="0 9 * * *", task_file_path="/path/to/task.md")
        config = CronConfig(tasks=[task])

        result = config.remove_task("nonexistent-id")

        self.assertFalse(result)
        self.assertEqual(len(config.tasks), 1)

    def test_roundtrip_serialization(self) -> None:
        """Test that config can be serialized and deserialized without loss."""
        task1 = CronTask(id="task-1", cron_expression="0 9 * * *", task_file_path="/path/to/task1.md")
        task2 = CronTask(id="task-2", cron_expression="0 */2 * * *", task_file_path="/path/to/task2.md")
        original = CronConfig(tasks=[task1, task2], daemon_pid=12345, log_directory="/custom/log/path")

        config_dict = original.to_dict()
        restored = CronConfig.from_dict(config_dict)

        self.assertEqual(len(restored.tasks), 2)
        self.assertEqual(restored.tasks[0].id, "task-1")
        self.assertEqual(restored.tasks[1].id, "task-2")
        self.assertEqual(restored.daemon_pid, 12345)
        self.assertEqual(restored.log_directory, "/custom/log/path")


if __name__ == "__main__":
    unittest.main()
