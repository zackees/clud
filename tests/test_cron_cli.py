"""Integration tests for clud --cron CLI commands."""

import tempfile
import unittest
from io import StringIO
from pathlib import Path
from unittest.mock import MagicMock, Mock, patch

from clud.cron.cli_handler import (
    handle_cron_add,
    handle_cron_command,
    handle_cron_install,
    handle_cron_list,
    handle_cron_remove,
    handle_cron_start,
    handle_cron_status,
    handle_cron_stop,
)
from clud.cron.config import CronConfigManager
from clud.cron.models import CronTask


class TestCronCLIHelp(unittest.TestCase):
    """Test help command."""

    def test_help_command(self) -> None:
        """Test that help command prints usage information."""
        with patch("sys.stdout", new_callable=StringIO) as mock_stdout:
            exit_code = handle_cron_command("help", [])
            output = mock_stdout.getvalue()

        self.assertEqual(exit_code, 0)
        self.assertIn("clud --cron", output)
        self.assertIn("Subcommands:", output)
        self.assertIn("add", output)
        self.assertIn("list", output)
        self.assertIn("remove", output)
        self.assertIn("start", output)
        self.assertIn("stop", output)
        self.assertIn("status", output)
        self.assertIn("install", output)

    def test_no_subcommand_shows_help(self) -> None:
        """Test that no subcommand shows help."""
        with patch("sys.stdout", new_callable=StringIO) as mock_stdout:
            exit_code = handle_cron_command(None, [])
            output = mock_stdout.getvalue()

        self.assertEqual(exit_code, 0)
        self.assertIn("clud --cron", output)

    def test_unknown_subcommand(self) -> None:
        """Test that unknown subcommand shows error."""
        with patch("sys.stderr", new_callable=StringIO) as mock_stderr:
            exit_code = handle_cron_command("unknown", [])
            output = mock_stderr.getvalue()

        self.assertEqual(exit_code, 1)
        self.assertIn("Unknown subcommand", output)


class TestCronCLIAdd(unittest.TestCase):
    """Test add command."""

    def setUp(self) -> None:
        """Set up test environment."""
        self.test_dir = Path(tempfile.mkdtemp(prefix="clud_cron_cli_"))
        self.config_dir = self.test_dir / ".clud"
        self.config_dir.mkdir(parents=True)
        self.task_file = self.test_dir / "task.md"
        self.task_file.write_text("Test task")

        # Create .cron_initialized marker to skip installation prompt
        (self.config_dir / ".cron_initialized").touch()

        # Patch Path.home() to use test directory
        self.home_patcher = patch("pathlib.Path.home", return_value=self.test_dir)
        self.home_patcher.start()

    def tearDown(self) -> None:
        """Clean up test environment."""
        self.home_patcher.stop()
        import shutil

        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_add_valid_task(self) -> None:
        """Test adding a valid task."""
        with patch("sys.stdout", new_callable=StringIO) as mock_stdout:
            exit_code = handle_cron_add("0 9 * * *", str(self.task_file))
            output = mock_stdout.getvalue()

        self.assertEqual(exit_code, 0)
        self.assertIn("scheduled successfully", output)
        self.assertIn("0 9 * * *", output)

        # Verify task was saved
        config_manager = CronConfigManager()
        config = config_manager.load()
        self.assertEqual(len(config.tasks), 1)
        self.assertEqual(config.tasks[0].cron_expression, "0 9 * * *")

    def test_add_invalid_cron_expression(self) -> None:
        """Test adding task with invalid cron expression."""
        with patch("sys.stderr", new_callable=StringIO) as mock_stderr:
            exit_code = handle_cron_add("invalid", str(self.task_file))
            output = mock_stderr.getvalue()

        self.assertEqual(exit_code, 1)
        self.assertIn("Invalid cron expression", output)

    def test_add_nonexistent_file(self) -> None:
        """Test adding task with nonexistent file."""
        with patch("sys.stderr", new_callable=StringIO) as mock_stderr:
            exit_code = handle_cron_add("0 9 * * *", str(self.test_dir / "nonexistent.md"))
            output = mock_stderr.getvalue()

        self.assertEqual(exit_code, 1)
        self.assertIn("not found", output)

    def test_add_via_command_handler(self) -> None:
        """Test add command via command handler."""
        with patch("sys.stdout", new_callable=StringIO):
            exit_code = handle_cron_command("add", ["0 9 * * *", str(self.task_file)])

        self.assertEqual(exit_code, 0)

        # Verify task was saved
        config_manager = CronConfigManager()
        config = config_manager.load()
        self.assertEqual(len(config.tasks), 1)

    def test_add_missing_arguments(self) -> None:
        """Test add command with missing arguments."""
        with patch("sys.stderr", new_callable=StringIO) as mock_stderr:
            exit_code = handle_cron_command("add", ["0 9 * * *"])  # Missing file
            output = mock_stderr.getvalue()

        self.assertEqual(exit_code, 1)
        self.assertIn("Missing arguments", output)


class TestCronCLIList(unittest.TestCase):
    """Test list command."""

    def setUp(self) -> None:
        """Set up test environment."""
        self.test_dir = Path(tempfile.mkdtemp(prefix="clud_cron_cli_"))
        self.config_dir = self.test_dir / ".clud"
        self.config_dir.mkdir(parents=True)

        # Patch Path.home() to use test directory
        self.home_patcher = patch("pathlib.Path.home", return_value=self.test_dir)
        self.home_patcher.start()

    def tearDown(self) -> None:
        """Clean up test environment."""
        self.home_patcher.stop()
        import shutil

        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_list_empty(self) -> None:
        """Test listing with no tasks."""
        with patch("sys.stdout", new_callable=StringIO) as mock_stdout:
            exit_code = handle_cron_list()
            output = mock_stdout.getvalue()

        self.assertEqual(exit_code, 0)
        self.assertIn("No scheduled tasks", output)

    def test_list_with_tasks(self) -> None:
        """Test listing with tasks."""
        # Add some tasks
        config_manager = CronConfigManager()
        config = config_manager.load()
        config.tasks.append(CronTask(id="task1", cron_expression="0 9 * * *", task_file_path="/path/to/task1.md"))
        config.tasks.append(CronTask(id="task2", cron_expression="0 10 * * *", task_file_path="/path/to/task2.md"))
        config_manager.save(config)

        with patch("sys.stdout", new_callable=StringIO) as mock_stdout:
            exit_code = handle_cron_list()
            output = mock_stdout.getvalue()

        self.assertEqual(exit_code, 0)
        self.assertIn("Scheduled Tasks", output)
        self.assertIn("task1", output)
        self.assertIn("task2", output)
        self.assertIn("0 9 * * *", output)
        self.assertIn("0 10 * * *", output)


class TestCronCLIRemove(unittest.TestCase):
    """Test remove command."""

    def setUp(self) -> None:
        """Set up test environment."""
        self.test_dir = Path(tempfile.mkdtemp(prefix="clud_cron_cli_"))
        self.config_dir = self.test_dir / ".clud"
        self.config_dir.mkdir(parents=True)

        # Create .cron_initialized marker to skip installation prompt
        (self.config_dir / ".cron_initialized").touch()

        # Patch Path.home() to use test directory
        self.home_patcher = patch("pathlib.Path.home", return_value=self.test_dir)
        self.home_patcher.start()

        # Add a task
        config_manager = CronConfigManager()
        config = config_manager.load()
        config.tasks.append(CronTask(id="task1", cron_expression="0 9 * * *", task_file_path="/path/to/task1.md"))
        config_manager.save(config)

    def tearDown(self) -> None:
        """Clean up test environment."""
        self.home_patcher.stop()
        import shutil

        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_remove_existing_task(self) -> None:
        """Test removing an existing task."""
        with patch("sys.stdout", new_callable=StringIO) as mock_stdout:
            exit_code = handle_cron_remove("task1")
            output = mock_stdout.getvalue()

        self.assertEqual(exit_code, 0)
        self.assertIn("removed", output)

        # Verify task was removed
        config_manager = CronConfigManager()
        config = config_manager.load()
        self.assertEqual(len(config.tasks), 0)

    def test_remove_nonexistent_task(self) -> None:
        """Test removing a nonexistent task."""
        with patch("sys.stderr", new_callable=StringIO) as mock_stderr:
            exit_code = handle_cron_remove("nonexistent")
            output = mock_stderr.getvalue()

        self.assertEqual(exit_code, 1)
        self.assertIn("not found", output)

    def test_remove_missing_argument(self) -> None:
        """Test remove command with missing task ID."""
        with patch("sys.stderr", new_callable=StringIO) as mock_stderr:
            exit_code = handle_cron_command("remove", [])
            output = mock_stderr.getvalue()

        self.assertEqual(exit_code, 1)
        self.assertIn("Missing task ID", output)


class TestCronCLIDaemon(unittest.TestCase):
    """Test daemon commands (start, stop, status)."""

    def setUp(self) -> None:
        """Set up test environment."""
        self.test_dir = Path(tempfile.mkdtemp(prefix="clud_cron_cli_"))
        self.config_dir = self.test_dir / ".clud"
        self.config_dir.mkdir(parents=True)

        # Patch Path.home() to use test directory
        self.home_patcher = patch("pathlib.Path.home", return_value=self.test_dir)
        self.home_patcher.start()

    def tearDown(self) -> None:
        """Clean up test environment."""
        self.home_patcher.stop()
        import shutil

        shutil.rmtree(self.test_dir, ignore_errors=True)

    @patch("clud.cron.cli_handler.Daemon")
    def test_start_daemon_success(self, mock_daemon_class: Mock) -> None:
        """Test starting daemon successfully."""
        from clud.cron import DaemonStatus

        # Mock Daemon.status() to return running status after start
        mock_daemon_class.status.side_effect = [
            DaemonStatus(state="stopped", pid=None),  # First call (line 401)
            DaemonStatus(state="running", pid=12345),  # Second call (line 417)
        ]
        mock_daemon_class.start.return_value = True

        with patch("sys.stdout", new_callable=StringIO) as mock_stdout:
            exit_code = handle_cron_start()
            output = mock_stdout.getvalue()

        self.assertEqual(exit_code, 0)
        self.assertIn("started successfully", output)
        mock_daemon_class.start.assert_called_once()

    @patch("clud.cron.cli_handler.Daemon")
    def test_start_daemon_already_running(self, mock_daemon_class: Mock) -> None:
        """Test starting daemon when already running."""
        from clud.cron import DaemonStatus

        # Mock Daemon.status() to return running status
        mock_daemon_class.status.return_value = DaemonStatus(state="running", pid=12345)

        with patch("sys.stdout", new_callable=StringIO) as mock_stdout:
            exit_code = handle_cron_start()
            output = mock_stdout.getvalue()

        self.assertEqual(exit_code, 0)
        self.assertIn("already running", output)
        mock_daemon_class.start.assert_not_called()

    @patch("clud.cron.cli_handler.Daemon")
    def test_stop_daemon_success(self, mock_daemon_class: Mock) -> None:
        """Test stopping daemon successfully."""
        from clud.cron import DaemonStatus

        # Mock Daemon methods
        mock_daemon_class.is_running.return_value = True
        mock_daemon_class.stop.return_value = True
        mock_daemon_class.status.return_value = DaemonStatus(state="stopped", pid=None)

        with patch("sys.stdout", new_callable=StringIO) as mock_stdout:
            exit_code = handle_cron_stop()
            output = mock_stdout.getvalue()

        self.assertEqual(exit_code, 0)
        self.assertIn("stopped successfully", output)
        mock_daemon_class.stop.assert_called_once()

    @patch("clud.cron.cli_handler.Daemon")
    def test_stop_daemon_not_running(self, mock_daemon_class: Mock) -> None:
        """Test stopping daemon when not running."""
        # Mock Daemon.is_running() to return False
        mock_daemon_class.is_running.return_value = False

        with patch("sys.stdout", new_callable=StringIO) as mock_stdout:
            exit_code = handle_cron_stop()
            output = mock_stdout.getvalue()

        self.assertEqual(exit_code, 0)
        self.assertIn("not running", output)
        mock_daemon_class.stop.assert_not_called()

    @patch("clud.cron.cli_handler.CronMonitor")
    @patch("clud.cron.cli_handler.CronScheduler")
    def test_status_daemon_running(self, mock_scheduler_class: Mock, mock_monitor_class: Mock) -> None:
        """Test status command with daemon running."""
        # Mock CronMonitor health check
        mock_monitor = MagicMock()
        mock_monitor.check_daemon_health.return_value = {
            "status": "running",
            "pid": 12345,
            "start_time": None,
            "uptime_seconds": 3600.0,
            "is_healthy": True,
            "message": "Daemon is running",
        }
        mock_monitor._format_uptime.return_value = "1h 0m 0s"
        mock_monitor.get_recent_activity.return_value = []
        mock_monitor_class.return_value = mock_monitor

        mock_scheduler = MagicMock()
        mock_scheduler.list_tasks.return_value = []
        mock_scheduler_class.return_value = mock_scheduler

        with patch("sys.stdout", new_callable=StringIO) as mock_stdout:
            exit_code = handle_cron_status()
            output = mock_stdout.getvalue()

        self.assertEqual(exit_code, 0)
        self.assertIn("Running", output)
        self.assertIn("12345", output)

    @patch("clud.cron.cli_handler.CronMonitor")
    @patch("clud.cron.cli_handler.CronScheduler")
    def test_status_daemon_stopped(self, mock_scheduler_class: Mock, mock_monitor_class: Mock) -> None:
        """Test status command with daemon stopped."""
        # Mock CronMonitor health check for stopped daemon
        mock_monitor = MagicMock()
        mock_monitor.check_daemon_health.return_value = {
            "status": "stopped",
            "pid": None,
            "start_time": None,
            "uptime_seconds": None,
            "is_healthy": False,
            "message": "Daemon is not running",
        }
        mock_monitor.get_recent_activity.return_value = []
        mock_monitor_class.return_value = mock_monitor

        mock_scheduler = MagicMock()
        mock_scheduler.list_tasks.return_value = []
        mock_scheduler_class.return_value = mock_scheduler

        with patch("sys.stdout", new_callable=StringIO) as mock_stdout:
            exit_code = handle_cron_status()
            output = mock_stdout.getvalue()

        self.assertEqual(exit_code, 0)
        self.assertIn("Stopped", output)


class TestCronCLIInstall(unittest.TestCase):
    """Test install command."""

    @patch("clud.cron.cli_handler.Daemon")
    @patch("clud.cron.cli_handler.AutostartInstaller")
    def test_install_success(self, mock_installer_class: Mock, mock_daemon_class: Mock) -> None:
        """Test successful autostart installation."""
        # Mock installer to return success
        mock_installer = Mock()
        mock_installer.status.return_value = ("not_installed", "Not configured", None)
        mock_installer.install.return_value = (True, "Installed successfully", "systemd")
        mock_installer_class.return_value = mock_installer

        # Mock Daemon.is_running() to return False
        mock_daemon_class.is_running.return_value = False

        with patch("sys.stdout", new_callable=StringIO) as mock_stdout:
            exit_code = handle_cron_install()
            output = mock_stdout.getvalue()

        self.assertEqual(exit_code, 0)
        self.assertIn("successfully", output)
        mock_installer.install.assert_called_once()


class TestCronCLIIntegration(unittest.TestCase):
    """Integration tests for complete workflows."""

    def setUp(self) -> None:
        """Set up test environment."""
        self.test_dir = Path(tempfile.mkdtemp(prefix="clud_cron_cli_"))
        self.config_dir = self.test_dir / ".clud"
        self.config_dir.mkdir(parents=True)
        self.task_file = self.test_dir / "task.md"
        self.task_file.write_text("Test task")

        # Create .cron_initialized marker to skip installation prompt
        (self.config_dir / ".cron_initialized").touch()

        # Patch Path.home() to use test directory
        self.home_patcher = patch("pathlib.Path.home", return_value=self.test_dir)
        self.home_patcher.start()

    def tearDown(self) -> None:
        """Clean up test environment."""
        self.home_patcher.stop()
        import shutil

        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_add_list_remove_workflow(self) -> None:
        """Test complete workflow: add, list, remove."""
        # Add task
        with patch("sys.stdout", new_callable=StringIO):
            exit_code = handle_cron_command("add", ["0 9 * * *", str(self.task_file)])
        self.assertEqual(exit_code, 0)

        # List tasks
        with patch("sys.stdout", new_callable=StringIO) as mock_stdout:
            exit_code = handle_cron_command("list", [])
            output = mock_stdout.getvalue()
        self.assertEqual(exit_code, 0)
        self.assertIn("0 9 * * *", output)

        # Get task ID from config
        config_manager = CronConfigManager()
        config = config_manager.load()
        task_id = config.tasks[0].id

        # Remove task
        with patch("sys.stdout", new_callable=StringIO):
            exit_code = handle_cron_command("remove", [task_id])
        self.assertEqual(exit_code, 0)

        # Verify task removed
        with patch("sys.stdout", new_callable=StringIO) as mock_stdout:
            exit_code = handle_cron_command("list", [])
            output = mock_stdout.getvalue()
        self.assertEqual(exit_code, 0)
        self.assertIn("No scheduled tasks", output)


if __name__ == "__main__":
    unittest.main()
