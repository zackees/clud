"""Unit tests for cron configuration manager."""

import json
import shutil
import tempfile
import unittest
from pathlib import Path

from clud.cron.config import CronConfigManager
from clud.cron.models import CronConfig, CronTask


class TestCronConfigManager(unittest.TestCase):
    """Test cases for CronConfigManager."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        # Create temporary directory for test files
        self.test_dir = tempfile.mkdtemp()
        self.config_path = Path(self.test_dir) / "cron.json"
        self.manager = CronConfigManager(config_path=self.config_path)

    def tearDown(self) -> None:
        """Clean up test fixtures."""
        # Remove test directory and all its contents recursively
        if Path(self.test_dir).exists():
            shutil.rmtree(self.test_dir)

    def test_load_nonexistent_config(self) -> None:
        """Test loading config when file doesn't exist."""
        config = self.manager.load()

        self.assertIsInstance(config, CronConfig)
        self.assertEqual(len(config.tasks), 0)
        self.assertIsNone(config.daemon_pid)

    def test_save_and_load_empty_config(self) -> None:
        """Test saving and loading an empty configuration."""
        config = CronConfig()
        self.manager.save(config)

        loaded_config = self.manager.load()

        self.assertEqual(len(loaded_config.tasks), 0)
        self.assertIsNone(loaded_config.daemon_pid)

    def test_save_and_load_config_with_tasks(self) -> None:
        """Test saving and loading configuration with tasks."""
        task1 = CronTask(id="task-1", cron_expression="0 9 * * *", task_file_path="/path/to/task1.md")
        task2 = CronTask(id="task-2", cron_expression="0 */2 * * *", task_file_path="/path/to/task2.md")
        config = CronConfig(tasks=[task1, task2], daemon_pid=12345)

        self.manager.save(config)
        loaded_config = self.manager.load()

        self.assertEqual(len(loaded_config.tasks), 2)
        self.assertEqual(loaded_config.tasks[0].id, "task-1")
        self.assertEqual(loaded_config.tasks[1].id, "task-2")
        self.assertEqual(loaded_config.daemon_pid, 12345)

    def test_save_creates_parent_directory(self) -> None:
        """Test that save creates parent directory if it doesn't exist."""
        nested_path = Path(self.test_dir) / "nested" / "dir" / "cron.json"
        manager = CronConfigManager(config_path=nested_path)

        config = CronConfig()
        manager.save(config)

        self.assertTrue(nested_path.exists())
        self.assertTrue(nested_path.parent.exists())

    def test_save_overwrites_existing_file(self) -> None:
        """Test that save overwrites existing configuration file."""
        # Save initial config
        config1 = CronConfig(daemon_pid=111)
        self.manager.save(config1)

        # Save updated config
        config2 = CronConfig(daemon_pid=222)
        self.manager.save(config2)

        # Load and verify
        loaded_config = self.manager.load()
        self.assertEqual(loaded_config.daemon_pid, 222)

    def test_load_invalid_json(self) -> None:
        """Test loading config with invalid JSON."""
        # Write invalid JSON to config file
        with open(self.config_path, "w", encoding="utf-8") as f:
            f.write("{invalid json}")

        with self.assertRaises(ValueError) as context:
            self.manager.load()

        self.assertIn("Invalid JSON", str(context.exception))

    def test_exists_returns_false_when_file_missing(self) -> None:
        """Test exists returns False when config file doesn't exist."""
        self.assertFalse(self.manager.exists())

    def test_exists_returns_true_when_file_present(self) -> None:
        """Test exists returns True when config file exists."""
        config = CronConfig()
        self.manager.save(config)

        self.assertTrue(self.manager.exists())

    def test_delete_removes_config_file(self) -> None:
        """Test delete removes configuration file."""
        config = CronConfig()
        self.manager.save(config)

        self.assertTrue(self.config_path.exists())

        self.manager.delete()

        self.assertFalse(self.config_path.exists())

    def test_delete_when_file_doesnt_exist(self) -> None:
        """Test delete doesn't raise error when file doesn't exist."""
        self.assertFalse(self.config_path.exists())

        # Should not raise an error
        self.manager.delete()

        self.assertFalse(self.config_path.exists())

    def test_saved_json_is_formatted(self) -> None:
        """Test that saved JSON is properly formatted with indentation."""
        task = CronTask(id="task-1", cron_expression="0 9 * * *", task_file_path="/path/to/task.md")
        config = CronConfig(tasks=[task])

        self.manager.save(config)

        # Read raw file contents
        with open(self.config_path, encoding="utf-8") as f:
            content = f.read()

        # Check that JSON is formatted with indentation
        self.assertIn("\n", content)
        self.assertIn("  ", content)

        # Verify it's valid JSON
        json_data = json.loads(content)
        self.assertIn("tasks", json_data)

    def test_default_config_path(self) -> None:
        """Test that default config path is ~/.clud/cron.json."""
        manager = CronConfigManager()

        expected_path = Path.home() / ".clud" / "cron.json"
        self.assertEqual(manager.config_path, expected_path)

    def test_roundtrip_preserves_all_fields(self) -> None:
        """Test that save/load roundtrip preserves all task fields."""
        task = CronTask(
            id="task-1",
            cron_expression="0 9 * * *",
            task_file_path="/path/to/task.md",
            enabled=False,
            created_at=1234567890.0,
            last_run=1234567900.0,
            next_run=1234567910.0,
        )
        config = CronConfig(tasks=[task], daemon_pid=12345, log_directory="/custom/log")

        self.manager.save(config)
        loaded_config = self.manager.load()

        loaded_task = loaded_config.tasks[0]
        self.assertEqual(loaded_task.id, "task-1")
        self.assertEqual(loaded_task.cron_expression, "0 9 * * *")
        self.assertEqual(loaded_task.task_file_path, "/path/to/task.md")
        self.assertFalse(loaded_task.enabled)
        self.assertEqual(loaded_task.created_at, 1234567890.0)
        self.assertEqual(loaded_task.last_run, 1234567900.0)
        self.assertEqual(loaded_task.next_run, 1234567910.0)
        self.assertEqual(loaded_config.daemon_pid, 12345)
        self.assertEqual(loaded_config.log_directory, "/custom/log")


if __name__ == "__main__":
    unittest.main()
