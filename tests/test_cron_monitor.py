"""Unit tests for cron monitor (health checks and metrics)."""

import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path
from unittest.mock import MagicMock, Mock, patch

from clud.cron.config import CronConfigManager
from clud.cron.models import CronConfig, CronTask
from clud.cron.monitor import CronMonitor


class TestCronMonitor(unittest.TestCase):
    """Test CronMonitor initialization and basic functionality."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.test_dir = tempfile.mkdtemp(prefix="clud_test_monitor_")
        self.monitor = CronMonitor(config_dir=self.test_dir)

    def tearDown(self) -> None:
        """Clean up test fixtures."""
        # Clean up test directory
        import shutil

        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_init(self) -> None:
        """Test monitor initialization."""
        self.assertEqual(self.monitor.config_dir, Path(self.test_dir))
        self.assertIsNotNone(self.monitor.daemon)
        self.assertIsNotNone(self.monitor.config_manager)

    def test_init_default_dir(self) -> None:
        """Test monitor initialization with default directory."""
        monitor = CronMonitor()
        self.assertEqual(monitor.config_dir, Path.home() / ".clud")


class TestDaemonHealthCheck(unittest.TestCase):
    """Test daemon health check functionality."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.test_dir = tempfile.mkdtemp(prefix="clud_test_monitor_health_")
        self.monitor = CronMonitor(config_dir=self.test_dir)

    def tearDown(self) -> None:
        """Clean up test fixtures."""
        import shutil

        shutil.rmtree(self.test_dir, ignore_errors=True)

    @patch("clud.cron.monitor.CronDaemon")
    def test_check_daemon_health_running(self, mock_daemon_class: Mock) -> None:
        """Test health check when daemon is running."""
        # Mock daemon
        mock_daemon = MagicMock()
        mock_daemon.status.return_value = ("running", 12345)
        start_time = datetime.now(timezone.utc)
        mock_daemon.get_start_time.return_value = start_time
        mock_daemon.get_uptime.return_value = 3661.5  # 1h 1m 1.5s
        self.monitor.daemon = mock_daemon

        # Check health
        health = self.monitor.check_daemon_health()

        # Verify results
        self.assertEqual(health["status"], "running")
        self.assertEqual(health["pid"], 12345)
        self.assertEqual(health["start_time"], start_time)
        self.assertEqual(health["uptime_seconds"], 3661.5)
        self.assertTrue(health["is_healthy"])
        self.assertIn("uptime", health["message"].lower())

    @patch("clud.cron.monitor.CronDaemon")
    def test_check_daemon_health_stopped(self, mock_daemon_class: Mock) -> None:
        """Test health check when daemon is stopped."""
        # Mock daemon
        mock_daemon = MagicMock()
        mock_daemon.status.return_value = ("stopped", None)
        mock_daemon.get_start_time.return_value = None
        mock_daemon.get_uptime.return_value = None
        self.monitor.daemon = mock_daemon

        # Check health
        health = self.monitor.check_daemon_health()

        # Verify results
        self.assertEqual(health["status"], "stopped")
        self.assertIsNone(health["pid"])
        self.assertIsNone(health["start_time"])
        self.assertIsNone(health["uptime_seconds"])
        self.assertFalse(health["is_healthy"])
        self.assertIn("not running", health["message"].lower())

    @patch("clud.cron.monitor.CronDaemon")
    def test_check_daemon_health_stale(self, mock_daemon_class: Mock) -> None:
        """Test health check when daemon has stale PID file."""
        # Mock daemon
        mock_daemon = MagicMock()
        mock_daemon.status.return_value = ("stale", 99999)
        mock_daemon.get_start_time.return_value = None
        mock_daemon.get_uptime.return_value = None
        self.monitor.daemon = mock_daemon

        # Check health
        health = self.monitor.check_daemon_health()

        # Verify results
        self.assertEqual(health["status"], "stale")
        self.assertEqual(health["pid"], 99999)
        self.assertFalse(health["is_healthy"])
        self.assertIn("stale", health["message"].lower())


class TestTaskExecutionHistory(unittest.TestCase):
    """Test task execution history retrieval."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.test_dir = tempfile.mkdtemp(prefix="clud_test_monitor_history_")
        self.monitor = CronMonitor(config_dir=self.test_dir)

        # Create mock task in config
        self.task_id = "task-12345"
        config = CronConfig(
            tasks=[
                CronTask(
                    id=self.task_id,
                    cron_expression="0 9 * * *",
                    task_file_path="/tmp/test.md",
                    enabled=True,
                )
            ]
        )
        config_path = Path(self.test_dir) / "cron.json"
        config_manager = CronConfigManager(config_path=config_path)
        config_manager.save(config)

    def tearDown(self) -> None:
        """Clean up test fixtures."""
        import shutil

        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_get_task_execution_history_no_logs(self) -> None:
        """Test getting history when no log files exist."""
        history = self.monitor.get_task_execution_history(self.task_id, limit=10)
        self.assertEqual(history, [])

    def test_get_task_execution_history_with_logs(self) -> None:
        """Test getting history with existing log files."""
        # Create log directory and files
        log_dir = Path(self.test_dir) / "logs" / "cron" / self.task_id
        log_dir.mkdir(parents=True, exist_ok=True)

        # Create mock log files
        log1 = log_dir / "20250115_090000.log"
        log1.write_text("Task executed successfully\n", encoding="utf-8")

        log2 = log_dir / "20250115_100000.log"
        log2.write_text("Task failed with error: something went wrong\n", encoding="utf-8")

        # Get history
        history = self.monitor.get_task_execution_history(self.task_id, limit=10)

        # Verify results (should be in reverse chronological order)
        self.assertEqual(len(history), 2)
        self.assertEqual(history[0]["timestamp"], datetime(2025, 1, 15, 10, 0, 0, tzinfo=timezone.utc))
        self.assertEqual(history[1]["timestamp"], datetime(2025, 1, 15, 9, 0, 0, tzinfo=timezone.utc))
        self.assertFalse(history[0]["success"])  # Contains "error"
        self.assertTrue(history[1]["success"])  # No error keywords

    def test_get_task_execution_history_limit(self) -> None:
        """Test history retrieval with limit."""
        # Create log directory with many files
        log_dir = Path(self.test_dir) / "logs" / "cron" / self.task_id
        log_dir.mkdir(parents=True, exist_ok=True)

        # Create 5 log files
        for i in range(5):
            log_file = log_dir / f"2025011{i}_090000.log"
            log_file.write_text("Task executed\n", encoding="utf-8")

        # Get history with limit=3
        history = self.monitor.get_task_execution_history(self.task_id, limit=3)

        # Verify only 3 entries returned (most recent)
        self.assertEqual(len(history), 3)

    def test_get_task_execution_history_nonexistent_task(self) -> None:
        """Test getting history for nonexistent task."""
        history = self.monitor.get_task_execution_history("nonexistent", limit=10)
        self.assertEqual(history, [])


class TestStalePIDDetection(unittest.TestCase):
    """Test stale PID file detection."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.test_dir = tempfile.mkdtemp(prefix="clud_test_monitor_stale_")
        self.monitor = CronMonitor(config_dir=self.test_dir)

    def tearDown(self) -> None:
        """Clean up test fixtures."""
        import shutil

        shutil.rmtree(self.test_dir, ignore_errors=True)

    @patch("clud.cron.monitor.CronDaemon")
    def test_get_stale_pid_files_none(self, mock_daemon_class: Mock) -> None:
        """Test stale PID detection when no stale files exist."""
        # Mock daemon as running
        mock_daemon = MagicMock()
        mock_daemon.status.return_value = ("running", 12345)
        mock_daemon.pid_file = Path(self.test_dir) / "cron.pid"
        mock_daemon.pid_file.write_text("12345", encoding="utf-8")
        self.monitor.daemon = mock_daemon

        # Get stale PID files
        stale_pids = self.monitor.get_stale_pid_files()
        self.assertEqual(stale_pids, [])

    @patch("clud.cron.monitor.CronDaemon")
    def test_get_stale_pid_files_stale(self, mock_daemon_class: Mock) -> None:
        """Test stale PID detection when stale file exists."""
        # Mock daemon as stale
        mock_daemon = MagicMock()
        mock_daemon.status.return_value = ("stale", 99999)
        mock_daemon.pid_file = Path(self.test_dir) / "cron.pid"
        mock_daemon.pid_file.write_text("99999", encoding="utf-8")
        self.monitor.daemon = mock_daemon

        # Get stale PID files
        stale_pids = self.monitor.get_stale_pid_files()
        self.assertEqual(len(stale_pids), 1)
        self.assertEqual(stale_pids[0], mock_daemon.pid_file)


class TestTaskFileVerification(unittest.TestCase):
    """Test task file existence verification."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.test_dir = tempfile.mkdtemp(prefix="clud_test_monitor_verify_")
        self.monitor = CronMonitor(config_dir=self.test_dir)

    def tearDown(self) -> None:
        """Clean up test fixtures."""
        import shutil

        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_verify_task_files_exist_empty_config(self) -> None:
        """Test verification with no tasks."""
        # Create empty config
        config = CronConfig(tasks=[])
        config_path = Path(self.test_dir) / "cron.json"
        config_manager = CronConfigManager(config_path=config_path)
        config_manager.save(config)

        # Verify task files
        status = self.monitor.verify_task_files_exist()
        self.assertEqual(status, {})

    def test_verify_task_files_exist_with_tasks(self) -> None:
        """Test verification with tasks."""
        # Create temporary task files
        task_file1 = Path(self.test_dir) / "task1.md"
        task_file1.write_text("Task 1\n", encoding="utf-8")

        # Create config with two tasks (one file exists, one doesn't)
        config = CronConfig(
            tasks=[
                CronTask(
                    id="task-1",
                    cron_expression="0 9 * * *",
                    task_file_path=str(task_file1),
                    enabled=True,
                ),
                CronTask(
                    id="task-2",
                    cron_expression="0 10 * * *",
                    task_file_path=str(Path(self.test_dir) / "nonexistent.md"),
                    enabled=True,
                ),
            ]
        )
        config_path = Path(self.test_dir) / "cron.json"
        config_manager = CronConfigManager(config_path=config_path)
        config_manager.save(config)

        # Verify task files
        status = self.monitor.verify_task_files_exist()
        self.assertTrue(status["task-1"])
        self.assertFalse(status["task-2"])


class TestRecentActivity(unittest.TestCase):
    """Test recent activity retrieval."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.test_dir = tempfile.mkdtemp(prefix="clud_test_monitor_activity_")
        self.monitor = CronMonitor(config_dir=self.test_dir)

    def tearDown(self) -> None:
        """Clean up test fixtures."""
        import shutil

        shutil.rmtree(self.test_dir, ignore_errors=True)

    @patch("clud.cron.monitor.CronDaemon")
    def test_get_recent_activity_empty(self, mock_daemon_class: Mock) -> None:
        """Test getting recent activity when no activity exists."""
        # Mock daemon with no start time
        mock_daemon = MagicMock()
        mock_daemon.get_start_time.return_value = None
        mock_daemon.get_pid.return_value = None
        self.monitor.daemon = mock_daemon

        # Create empty config
        config = CronConfig(tasks=[])
        config_path = Path(self.test_dir) / "cron.json"
        config_manager = CronConfigManager(config_path=config_path)
        config_manager.save(config)

        # Get recent activity
        activity = self.monitor.get_recent_activity(minutes=60)
        self.assertEqual(activity, [])

    @patch("clud.cron.monitor.CronDaemon")
    def test_get_recent_activity_with_daemon_start(self, mock_daemon_class: Mock) -> None:
        """Test getting recent activity including daemon start."""
        # Mock daemon with recent start time
        start_time = datetime.now(timezone.utc)
        mock_daemon = MagicMock()
        mock_daemon.get_start_time.return_value = start_time
        mock_daemon.get_pid.return_value = 12345
        self.monitor.daemon = mock_daemon

        # Create empty config
        config = CronConfig(tasks=[])
        config_path = Path(self.test_dir) / "cron.json"
        config_manager = CronConfigManager(config_path=config_path)
        config_manager.save(config)

        # Get recent activity
        activity = self.monitor.get_recent_activity(minutes=60)
        self.assertEqual(len(activity), 1)
        self.assertEqual(activity[0]["type"], "daemon_start")
        self.assertEqual(activity[0]["timestamp"], start_time)


class TestUptimeFormatting(unittest.TestCase):
    """Test uptime formatting utility."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.test_dir = tempfile.mkdtemp(prefix="clud_test_monitor_uptime_")
        self.monitor = CronMonitor(config_dir=self.test_dir)

    def tearDown(self) -> None:
        """Clean up test fixtures."""
        import shutil

        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_format_uptime_seconds(self) -> None:
        """Test formatting uptime in seconds."""
        result = self.monitor._format_uptime(45)
        self.assertEqual(result, "45s")

    def test_format_uptime_minutes(self) -> None:
        """Test formatting uptime in minutes."""
        result = self.monitor._format_uptime(125)  # 2m 5s
        self.assertEqual(result, "2m 5s")

    def test_format_uptime_hours(self) -> None:
        """Test formatting uptime in hours."""
        result = self.monitor._format_uptime(3661)  # 1h 1m 1s
        self.assertEqual(result, "1h 1m 1s")

    def test_format_uptime_days(self) -> None:
        """Test formatting uptime in days."""
        result = self.monitor._format_uptime(90061)  # 1d 1h 1m 1s
        self.assertEqual(result, "1d 1h 1m 1s")

    def test_format_uptime_zero(self) -> None:
        """Test formatting zero uptime."""
        result = self.monitor._format_uptime(0)
        self.assertEqual(result, "0s")


class TestExecutionResultParsing(unittest.TestCase):
    """Test execution result parsing from logs."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.test_dir = tempfile.mkdtemp(prefix="clud_test_monitor_parse_")
        self.monitor = CronMonitor(config_dir=self.test_dir)

    def tearDown(self) -> None:
        """Clean up test fixtures."""
        import shutil

        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_parse_execution_result_success(self) -> None:
        """Test parsing successful execution log."""
        log_content = "Task executed successfully\nAll tests passed\n"
        result = self.monitor._parse_execution_result(log_content)
        self.assertTrue(result)

    def test_parse_execution_result_failure_error(self) -> None:
        """Test parsing failed execution log with error."""
        log_content = "Task started\nError: something went wrong\nTask failed\n"
        result = self.monitor._parse_execution_result(log_content)
        self.assertFalse(result)

    def test_parse_execution_result_failure_exception(self) -> None:
        """Test parsing failed execution log with exception."""
        log_content = "Task started\nException: ValueError\nTraceback (most recent call last):\n"
        result = self.monitor._parse_execution_result(log_content)
        self.assertFalse(result)

    def test_parse_execution_result_failure_failed(self) -> None:
        """Test parsing failed execution log with 'failed' keyword."""
        log_content = "Task started\nTask failed to complete\n"
        result = self.monitor._parse_execution_result(log_content)
        self.assertFalse(result)


if __name__ == "__main__":
    unittest.main()
