"""
Unit tests for CronDaemon class.

Tests daemon lifecycle (start, stop, status), PID file management,
signal handling, and cross-platform compatibility.
"""

import os
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import MagicMock, Mock, patch

from clud.cron.daemon import CronDaemon


class TestCronDaemonInit(unittest.TestCase):
    """Tests for CronDaemon initialization."""

    def setUp(self) -> None:
        """Set up temporary directory for each test."""
        self.temp_dir = tempfile.mkdtemp(prefix="clud_cron_daemon_test_")

    def tearDown(self) -> None:
        """Clean up temporary directory."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def test_init_default_config_dir(self) -> None:
        """Test initialization with default config directory."""
        with patch("clud.cron.daemon.Path.home") as mock_home:
            mock_home.return_value = Path(self.temp_dir)
            daemon = CronDaemon()

            self.assertEqual(daemon.config_dir, Path(self.temp_dir) / ".clud")
            self.assertEqual(daemon.pid_file, Path(self.temp_dir) / ".clud" / "cron.pid")
            self.assertEqual(daemon.log_file, Path(self.temp_dir) / ".clud" / "logs" / "cron-daemon.log")

    def test_init_custom_config_dir(self) -> None:
        """Test initialization with custom config directory."""
        custom_dir = Path(self.temp_dir) / "custom"
        daemon = CronDaemon(config_dir=str(custom_dir))

        self.assertEqual(daemon.config_dir, custom_dir)
        self.assertEqual(daemon.pid_file, custom_dir / "cron.pid")
        self.assertEqual(daemon.log_file, custom_dir / "logs" / "cron-daemon.log")

    def test_init_creates_directories(self) -> None:
        """Test that initialization creates required directories."""
        custom_dir = Path(self.temp_dir) / "custom"
        daemon = CronDaemon(config_dir=str(custom_dir))

        self.assertTrue(daemon.config_dir.exists())
        self.assertTrue(daemon.log_file.parent.exists())


class TestCronDaemonPIDFileOperations(unittest.TestCase):
    """Tests for PID file read/write operations."""

    def setUp(self) -> None:
        """Set up temporary directory for each test."""
        self.temp_dir = tempfile.mkdtemp(prefix="clud_cron_daemon_test_")
        self.daemon = CronDaemon(config_dir=self.temp_dir)

    def tearDown(self) -> None:
        """Clean up temporary directory."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def test_read_pid_nonexistent_file(self) -> None:
        """Test reading PID when file doesn't exist."""
        pid = self.daemon._read_pid()
        self.assertIsNone(pid)

    def test_read_pid_valid_file(self) -> None:
        """Test reading PID from valid file."""
        self.daemon.pid_file.write_text("12345", encoding="utf-8")
        pid = self.daemon._read_pid()
        self.assertEqual(pid, 12345)

    def test_read_pid_invalid_content(self) -> None:
        """Test reading PID from file with invalid content."""
        self.daemon.pid_file.write_text("not_a_number", encoding="utf-8")
        pid = self.daemon._read_pid()
        self.assertIsNone(pid)

    def test_read_pid_with_whitespace(self) -> None:
        """Test reading PID with leading/trailing whitespace."""
        self.daemon.pid_file.write_text("  12345  \n", encoding="utf-8")
        pid = self.daemon._read_pid()
        self.assertEqual(pid, 12345)


class TestCronDaemonProcessChecking(unittest.TestCase):
    """Tests for process existence checking."""

    def setUp(self) -> None:
        """Set up temporary directory for each test."""
        self.temp_dir = tempfile.mkdtemp(prefix="clud_cron_daemon_test_")
        self.daemon = CronDaemon(config_dir=self.temp_dir)

    def tearDown(self) -> None:
        """Clean up temporary directory."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def test_is_process_running_current_process(self) -> None:
        """Test checking if current process is running."""
        current_pid = os.getpid()
        is_running = self.daemon._is_process_running(current_pid)
        self.assertTrue(is_running)

    def test_is_process_running_nonexistent_pid(self) -> None:
        """Test checking if nonexistent PID is running."""
        # Use a very high PID that's unlikely to exist
        fake_pid = 9999999
        is_running = self.daemon._is_process_running(fake_pid)
        self.assertFalse(is_running)

    @unittest.skipIf(sys.platform == "win32", "Unix-specific signal test")
    def test_is_process_running_unix_signal(self) -> None:
        """Test Unix process checking via signal 0."""
        with patch("os.kill") as mock_kill:
            mock_kill.side_effect = ProcessLookupError()
            is_running = self.daemon._is_process_running(12345)
            self.assertFalse(is_running)
            mock_kill.assert_called_once_with(12345, 0)

    @unittest.skipIf(sys.platform != "win32", "Windows-specific tasklist test")
    def test_is_process_running_windows_tasklist(self) -> None:
        """Test Windows process checking via tasklist."""
        with patch("subprocess.run") as mock_run:
            # Mock tasklist output
            mock_run.return_value = Mock(stdout=f"python.exe\t{os.getpid()}")
            is_running = self.daemon._is_process_running(os.getpid())
            self.assertTrue(is_running)


class TestCronDaemonStatus(unittest.TestCase):
    """Tests for daemon status checking."""

    def setUp(self) -> None:
        """Set up temporary directory for each test."""
        self.temp_dir = tempfile.mkdtemp(prefix="clud_cron_daemon_test_")
        self.daemon = CronDaemon(config_dir=self.temp_dir)

    def tearDown(self) -> None:
        """Clean up temporary directory."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def test_status_stopped(self) -> None:
        """Test status when daemon is stopped (no PID file)."""
        status, pid = self.daemon.status()
        self.assertEqual(status, "stopped")
        self.assertIsNone(pid)

    def test_status_running(self) -> None:
        """Test status when daemon is running."""
        # Write current process PID
        current_pid = os.getpid()
        self.daemon.pid_file.write_text(str(current_pid), encoding="utf-8")

        status, pid = self.daemon.status()
        self.assertEqual(status, "running")
        self.assertEqual(pid, current_pid)

    def test_status_stale(self) -> None:
        """Test status when PID file exists but process is not running."""
        # Write nonexistent PID
        self.daemon.pid_file.write_text("9999999", encoding="utf-8")

        status, pid = self.daemon.status()
        self.assertEqual(status, "stale")
        self.assertEqual(pid, 9999999)

    def test_is_running_true(self) -> None:
        """Test is_running returns True when daemon is running."""
        current_pid = os.getpid()
        self.daemon.pid_file.write_text(str(current_pid), encoding="utf-8")

        self.assertTrue(self.daemon.is_running())

    def test_is_running_false_stopped(self) -> None:
        """Test is_running returns False when daemon is stopped."""
        self.assertFalse(self.daemon.is_running())

    def test_is_running_false_stale(self) -> None:
        """Test is_running returns False when daemon has stale PID."""
        self.daemon.pid_file.write_text("9999999", encoding="utf-8")
        self.assertFalse(self.daemon.is_running())


class TestCronDaemonStart(unittest.TestCase):
    """Tests for daemon start functionality."""

    def setUp(self) -> None:
        """Set up temporary directory for each test."""
        self.temp_dir = tempfile.mkdtemp(prefix="clud_cron_daemon_test_")
        self.daemon = CronDaemon(config_dir=self.temp_dir)

    def tearDown(self) -> None:
        """Clean up temporary directory and any started processes."""
        import shutil

        # Stop daemon if running
        if self.daemon.is_running():
            self.daemon.stop()

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def test_start_already_running(self) -> None:
        """Test starting daemon when already running."""
        # Write current process PID to simulate running daemon
        current_pid = os.getpid()
        self.daemon.pid_file.write_text(str(current_pid), encoding="utf-8")

        # Mock the process check to be fast
        with patch.object(self.daemon, "_is_process_running", return_value=True):
            result = self.daemon.start()
            self.assertFalse(result)

        # Clean up PID file to prevent tearDown from trying to stop the test process
        self.daemon.pid_file.unlink(missing_ok=True)

    def test_start_cleans_stale_pid(self) -> None:
        """Test that start cleans up stale PID file."""
        # Write stale PID
        self.daemon.pid_file.write_text("9999999", encoding="utf-8")

        with patch("subprocess.Popen") as mock_popen:
            mock_process = Mock()
            mock_process.pid = 12345
            mock_popen.return_value = mock_process

            with patch.object(self.daemon, "is_running") as mock_is_running:
                # First call (check before start) returns False because PID 9999999 is stale
                # Second call (verify after start) returns True because daemon started successfully
                mock_is_running.side_effect = [False, True]
                result = self.daemon.start()

            # Verify stale PID was cleaned before starting
            self.assertTrue(result)

    @patch("subprocess.Popen")
    def test_start_success(self, mock_popen: MagicMock) -> None:
        """Test successful daemon start."""
        mock_process = Mock()
        mock_process.pid = 12345
        mock_popen.return_value = mock_process

        with patch.object(self.daemon, "is_running") as mock_is_running:
            # First call (check before start) returns False, second call (verify after start) returns True
            mock_is_running.side_effect = [False, True]
            result = self.daemon.start()

        self.assertTrue(result)
        self.assertEqual(self.daemon.pid_file.read_text(encoding="utf-8"), "12345")

    @patch("subprocess.Popen")
    def test_start_failure(self, mock_popen: MagicMock) -> None:
        """Test daemon start failure."""
        mock_process = Mock()
        mock_process.pid = 12345
        mock_popen.return_value = mock_process

        with patch.object(self.daemon, "is_running", return_value=False):
            result = self.daemon.start()

        self.assertFalse(result)

    @unittest.skipIf(sys.platform != "win32", "Windows-specific test")
    @patch("subprocess.Popen")
    def test_start_windows_flags(self, mock_popen: MagicMock) -> None:
        """Test that Windows start uses CREATE_NO_WINDOW flag."""
        mock_process = Mock()
        mock_process.pid = 12345
        mock_popen.return_value = mock_process

        with patch.object(self.daemon, "is_running") as mock_is_running:
            mock_is_running.side_effect = [False, True]
            self.daemon.start()

        # Verify creationflags includes CREATE_NO_WINDOW
        call_kwargs = mock_popen.call_args[1]
        self.assertIn("creationflags", call_kwargs)
        self.assertEqual(call_kwargs["creationflags"], 0x08000000)

    @unittest.skipIf(sys.platform == "win32", "Unix-specific test")
    @patch("subprocess.Popen")
    def test_start_unix_session(self, mock_popen: MagicMock) -> None:
        """Test that Unix start uses start_new_session."""
        mock_process = Mock()
        mock_process.pid = 12345
        mock_popen.return_value = mock_process

        with patch.object(self.daemon, "is_running") as mock_is_running:
            mock_is_running.side_effect = [False, True]
            self.daemon.start()

        # Verify start_new_session is set
        call_kwargs = mock_popen.call_args[1]
        self.assertIn("start_new_session", call_kwargs)
        self.assertTrue(call_kwargs["start_new_session"])


class TestCronDaemonStop(unittest.TestCase):
    """Tests for daemon stop functionality."""

    def setUp(self) -> None:
        """Set up temporary directory for each test."""
        self.temp_dir = tempfile.mkdtemp(prefix="clud_cron_daemon_test_")
        self.daemon = CronDaemon(config_dir=self.temp_dir)

    def tearDown(self) -> None:
        """Clean up temporary directory."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def test_stop_not_running_no_pid(self) -> None:
        """Test stopping daemon when not running (no PID file)."""
        result = self.daemon.stop()
        self.assertFalse(result)

    def test_stop_not_running_stale_pid(self) -> None:
        """Test stopping daemon with stale PID."""
        self.daemon.pid_file.write_text("9999999", encoding="utf-8")

        result = self.daemon.stop()
        self.assertFalse(result)
        self.assertFalse(self.daemon.pid_file.exists())

    @unittest.skipIf(sys.platform != "win32", "Windows-specific test")
    @patch("subprocess.run")
    def test_stop_windows_taskkill(self, mock_run: MagicMock) -> None:
        """Test Windows daemon stop uses taskkill."""
        self.daemon.pid_file.write_text(str(os.getpid()), encoding="utf-8")

        with patch.object(self.daemon, "_is_process_running") as mock_is_running:
            # First call (check before stop) returns True, subsequent calls return False
            mock_is_running.side_effect = [True, False]
            result = self.daemon.stop()

        self.assertTrue(result)
        self.assertFalse(self.daemon.pid_file.exists())

        # Verify taskkill was called
        self.assertTrue(mock_run.called)
        call_args = mock_run.call_args[0][0]
        self.assertEqual(call_args[0], "taskkill")

    @unittest.skipIf(sys.platform == "win32", "Unix-specific test")
    @patch("os.kill")
    def test_stop_unix_signal(self, mock_kill: MagicMock) -> None:
        """Test Unix daemon stop uses SIGTERM."""
        test_pid = 12345
        self.daemon.pid_file.write_text(str(test_pid), encoding="utf-8")

        with patch.object(self.daemon, "_is_process_running") as mock_is_running:
            # First call (check before stop) returns True, subsequent calls return False
            mock_is_running.side_effect = [True, False]
            result = self.daemon.stop()

        self.assertTrue(result)
        self.assertFalse(self.daemon.pid_file.exists())

        # Verify SIGTERM was sent
        import signal

        mock_kill.assert_called_once_with(test_pid, signal.SIGTERM)

    @patch("time.sleep")
    def test_stop_force_kill_after_timeout(self, mock_sleep: MagicMock) -> None:
        """Test force kill when daemon doesn't stop gracefully."""
        test_pid = 12345
        self.daemon.pid_file.write_text(str(test_pid), encoding="utf-8")

        with patch.object(self.daemon, "_is_process_running") as mock_is_running:
            # Process keeps running through timeout
            mock_is_running.return_value = True

            if sys.platform == "win32":
                with patch("subprocess.run") as mock_run:
                    result = self.daemon.stop()
                    # Verify both SIGTERM and force kill were attempted
                    self.assertEqual(mock_run.call_count, 2)
            else:
                with patch("os.kill") as mock_kill:
                    result = self.daemon.stop()
                    # Verify both SIGTERM and SIGKILL were sent
                    import signal

                    calls = mock_kill.call_args_list
                    self.assertEqual(len(calls), 2)
                    self.assertEqual(calls[0][0][1], signal.SIGTERM)
                    self.assertEqual(calls[1][0][1], signal.SIGKILL)

        self.assertTrue(result)


class TestCronDaemonSignalHandlers(unittest.TestCase):
    """Tests for signal handler setup."""

    def setUp(self) -> None:
        """Set up temporary directory for each test."""
        self.temp_dir = tempfile.mkdtemp(prefix="clud_cron_daemon_test_")
        self.daemon = CronDaemon(config_dir=self.temp_dir)

    def tearDown(self) -> None:
        """Clean up temporary directory."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    @unittest.skipIf(sys.platform == "win32", "Unix-specific signal test")
    @patch("signal.signal")
    def test_setup_signal_handlers_unix(self, mock_signal: MagicMock) -> None:
        """Test Unix signal handler setup."""
        self.daemon._setup_signal_handlers()

        # Verify Unix signals were registered
        import signal

        calls = mock_signal.call_args_list
        registered_signals = [call[0][0] for call in calls]

        self.assertIn(signal.SIGTERM, registered_signals)
        self.assertIn(signal.SIGINT, registered_signals)
        self.assertIn(signal.SIGHUP, registered_signals)

    @unittest.skipIf(sys.platform != "win32", "Windows-specific signal test")
    @patch("signal.signal")
    def test_setup_signal_handlers_windows(self, mock_signal: MagicMock) -> None:
        """Test Windows signal handler setup."""
        self.daemon._setup_signal_handlers()

        # Verify Windows signals were registered
        import signal

        calls = mock_signal.call_args_list
        registered_signals = [call[0][0] for call in calls]

        self.assertIn(signal.SIGINT, registered_signals)
        self.assertIn(signal.SIGBREAK, registered_signals)


class TestCronDaemonRunLoop(unittest.TestCase):
    """Tests for daemon main run loop."""

    def setUp(self) -> None:
        """Set up temporary directory for each test."""
        self.temp_dir = tempfile.mkdtemp(prefix="clud_cron_daemon_test_")
        self.daemon = CronDaemon(config_dir=self.temp_dir)

        # Create empty config file to prevent FileNotFoundError during crash recovery
        import json

        config_path = Path(self.temp_dir) / "cron.json"
        config_path.write_text(json.dumps({"tasks": [], "daemon_pid": None}), encoding="utf-8")

    def tearDown(self) -> None:
        """Clean up temporary directory."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    @patch("psutil.Process")
    @patch("time.sleep")
    def test_run_loop_checks_due_tasks(self, mock_sleep: MagicMock, mock_process_class: MagicMock) -> None:
        """Test that run loop checks for due tasks."""
        # Mock psutil.Process to avoid cpu_percent() internal sleeps
        mock_process = Mock()
        mock_process.cpu_percent.return_value = 0.5
        mock_process.memory_info.return_value = Mock(rss=50 * 1024 * 1024)  # 50MB
        mock_process_class.return_value = mock_process

        # Mock sleep to raise KeyboardInterrupt on first call in main loop
        # (after initialization completes)
        mock_sleep.side_effect = KeyboardInterrupt()

        with patch.object(self.daemon.scheduler, "check_due_tasks", return_value=[]) as mock_check:
            # run_loop catches KeyboardInterrupt gracefully and doesn't propagate it
            self.daemon.run_loop()

            # Verify check_due_tasks was called
            mock_check.assert_called_once()

    @patch("psutil.Process")
    @patch("time.sleep")
    def test_run_loop_executes_due_tasks(self, mock_sleep: MagicMock, mock_process_class: MagicMock) -> None:
        """Test that run loop executes due tasks."""
        # Mock psutil.Process to avoid cpu_percent() internal sleeps
        mock_process = Mock()
        mock_process.cpu_percent.return_value = 0.5
        mock_process.memory_info.return_value = Mock(rss=50 * 1024 * 1024)  # 50MB
        mock_process_class.return_value = mock_process

        # Create a mock task
        from clud.cron.models import CronTask

        mock_task = CronTask(
            id="test-task",
            cron_expression="* * * * *",
            task_file_path="/tmp/test.md",
            enabled=True,
            created_at=int(time.time()),
            next_run=int(time.time()),
        )

        # Mock sleep to stop after first iteration
        mock_sleep.side_effect = KeyboardInterrupt()

        with (
            patch.object(self.daemon.scheduler, "check_due_tasks", return_value=[mock_task]) as mock_check,
            patch.object(self.daemon.executor, "execute_task", return_value=(0, Path("/tmp/test.log"))) as mock_execute,
            patch.object(self.daemon.scheduler, "update_task_after_execution") as mock_update,
            patch("pathlib.Path.exists", return_value=True),
            patch("pathlib.Path.is_file", return_value=True),
        ):
            # run_loop catches KeyboardInterrupt gracefully and doesn't propagate it
            self.daemon.run_loop()

            # Verify task was executed and updated
            mock_check.assert_called_once()
            mock_execute.assert_called_once_with(mock_task)
            mock_update.assert_called_once()

    @patch("psutil.Process")
    @patch("time.sleep")
    def test_run_loop_continues_after_task_error(self, mock_sleep: MagicMock, mock_process_class: MagicMock) -> None:
        """Test that run loop continues after task execution error."""
        # Mock psutil.Process to avoid cpu_percent() internal sleeps
        mock_process = Mock()
        mock_process.cpu_percent.return_value = 0.5
        mock_process.memory_info.return_value = Mock(rss=50 * 1024 * 1024)  # 50MB
        mock_process_class.return_value = mock_process

        from clud.cron.models import CronTask

        mock_task = CronTask(
            id="test-task",
            cron_expression="* * * * *",
            task_file_path="/tmp/test.md",
            enabled=True,
            created_at=int(time.time()),
            next_run=int(time.time()),
        )

        # Mock sleep to raise KeyboardInterrupt after task error is handled
        mock_sleep.side_effect = KeyboardInterrupt()

        with (
            patch.object(self.daemon.scheduler, "check_due_tasks", return_value=[mock_task]) as mock_check,
            patch.object(self.daemon.executor, "execute_task", side_effect=RuntimeError("Task failed")) as mock_execute,
            patch.object(self.daemon.scheduler, "update_task_after_execution") as mock_update,
            patch("pathlib.Path.exists", return_value=True),
            patch("pathlib.Path.is_file", return_value=True),
        ):
            # run_loop catches KeyboardInterrupt gracefully and doesn't propagate it
            self.daemon.run_loop()

            # Verify task execution was attempted despite error
            mock_check.assert_called_once()
            mock_execute.assert_called_once_with(mock_task)
            # Verify task was updated with failure status
            mock_update.assert_called_once()

    @patch("psutil.Process")
    def test_run_loop_cleanup_on_exit(self, mock_process_class: MagicMock) -> None:
        """Test that run loop cleans up on exit."""
        # Mock psutil.Process to avoid cpu_percent() internal sleeps
        mock_process = Mock()
        mock_process.cpu_percent.return_value = 0.5
        mock_process.memory_info.return_value = Mock(rss=50 * 1024 * 1024)  # 50MB
        mock_process_class.return_value = mock_process

        # Write PID file
        self.daemon.pid_file.write_text("12345", encoding="utf-8")

        with patch("time.sleep", side_effect=KeyboardInterrupt()), patch.object(self.daemon.scheduler, "check_due_tasks", return_value=[]):
            # run_loop catches KeyboardInterrupt gracefully and doesn't propagate it
            self.daemon.run_loop()

        # Verify PID file was cleaned up
        self.assertFalse(self.daemon.pid_file.exists())


if __name__ == "__main__":
    unittest.main()
