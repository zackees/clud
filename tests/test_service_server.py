"""Unit tests for daemon server spawning and health checking."""
# pyright: reportUnknownParameterType=false, reportMissingParameterType=false, reportUnknownLambdaType=false

import unittest
from pathlib import Path
from unittest.mock import MagicMock, Mock, patch

from clud.service.server import (
    DAEMON_HOST,
    DAEMON_PID_FILE,
    DAEMON_PORT,
    ensure_daemon_running,
    is_daemon_running,
    probe_daemon_health,
    spawn_daemon,
)


class TestIsDaemonRunning(unittest.TestCase):
    """Test daemon running detection."""

    @patch("socket.socket")
    def test_is_daemon_running_success(self, mock_socket_class: MagicMock) -> None:
        """Test when daemon is running."""
        mock_sock = Mock()
        mock_sock.connect_ex.return_value = 0
        mock_socket_class.return_value = mock_sock

        result = is_daemon_running()

        self.assertTrue(result)
        mock_sock.connect_ex.assert_called_once_with((DAEMON_HOST, DAEMON_PORT))
        mock_sock.close.assert_called_once()

    @patch("socket.socket")
    def test_is_daemon_running_not_running(self, mock_socket_class: MagicMock) -> None:
        """Test when daemon is not running."""
        mock_sock = Mock()
        mock_sock.connect_ex.return_value = 1  # Connection refused
        mock_socket_class.return_value = mock_sock

        result = is_daemon_running()

        self.assertFalse(result)
        mock_sock.connect_ex.assert_called_once_with((DAEMON_HOST, DAEMON_PORT))
        mock_sock.close.assert_called_once()

    @patch("socket.socket")
    def test_is_daemon_running_exception(self, mock_socket_class: MagicMock) -> None:
        """Test when socket operation raises exception."""
        mock_socket_class.side_effect = Exception("Socket error")

        result = is_daemon_running()

        self.assertFalse(result)


class TestProbeDaemonHealth(unittest.TestCase):
    """Test daemon health probing."""

    @patch("urllib.request.urlopen")
    def test_probe_daemon_health_success(self, mock_urlopen: MagicMock) -> None:
        """Test successful health check."""
        mock_response = Mock()
        mock_response.read.return_value = b'{"status": "ok", "pid": 12345, "agents": {"total": 0, "running": 0, "stale": 0}}'
        mock_response.__enter__ = Mock(return_value=mock_response)
        mock_response.__exit__ = Mock(return_value=False)
        mock_urlopen.return_value = mock_response

        result = probe_daemon_health()

        self.assertIsNotNone(result)
        self.assertEqual(result["status"], "ok")  # type: ignore[index]
        self.assertEqual(result["pid"], 12345)  # type: ignore[index]
        mock_urlopen.assert_called_once()

    @patch("urllib.request.urlopen")
    def test_probe_daemon_health_timeout(self, mock_urlopen: MagicMock) -> None:
        """Test health check with timeout."""
        mock_urlopen.side_effect = TimeoutError("Timed out")

        result = probe_daemon_health()

        self.assertIsNone(result)

    @patch("urllib.request.urlopen")
    def test_probe_daemon_health_connection_error(self, mock_urlopen: MagicMock) -> None:
        """Test health check with connection error."""
        mock_urlopen.side_effect = ConnectionError("Connection refused")

        result = probe_daemon_health()

        self.assertIsNone(result)

    @patch("urllib.request.urlopen")
    def test_probe_daemon_health_invalid_json(self, mock_urlopen: MagicMock) -> None:
        """Test health check with invalid JSON response."""
        mock_response = Mock()
        mock_response.read.return_value = b"not json"
        mock_response.__enter__ = Mock(return_value=mock_response)
        mock_response.__exit__ = Mock(return_value=False)
        mock_urlopen.return_value = mock_response

        result = probe_daemon_health()

        self.assertIsNone(result)


class TestSpawnDaemon(unittest.TestCase):
    """Test daemon spawning."""

    @patch("subprocess.Popen")
    @patch("clud.service.server.DAEMON_PID_FILE")
    def test_spawn_daemon_windows_success(self, mock_pid_file: MagicMock, mock_popen: MagicMock) -> None:
        """Test successful daemon spawn on Windows."""
        with patch("sys.platform", "win32"):
            mock_process = Mock()
            mock_process.pid = 12345
            mock_popen.return_value = mock_process

            # Mock PID file
            mock_pid_file.parent.mkdir = Mock()
            mock_pid_file.write_text = Mock()

            result = spawn_daemon()

            self.assertTrue(result)
            mock_popen.assert_called_once()
            # Check that DETACHED_PROCESS flag was used
            call_kwargs = mock_popen.call_args[1]
            self.assertIn("creationflags", call_kwargs)
            mock_pid_file.write_text.assert_called_once_with("12345")

    @patch("subprocess.Popen")
    @patch("clud.service.server.DAEMON_PID_FILE")
    def test_spawn_daemon_unix_success(self, mock_pid_file: MagicMock, mock_popen: MagicMock) -> None:
        """Test successful daemon spawn on Unix."""
        with patch("sys.platform", "linux"):
            mock_process = Mock()
            mock_process.pid = 12345
            mock_popen.return_value = mock_process

            # Mock PID file
            mock_pid_file.parent.mkdir = Mock()
            mock_pid_file.write_text = Mock()

            result = spawn_daemon()

            self.assertTrue(result)
            mock_popen.assert_called_once()
            # Check that start_new_session was used
            call_kwargs = mock_popen.call_args[1]
            self.assertTrue(call_kwargs.get("start_new_session"))
            mock_pid_file.write_text.assert_called_once_with("12345")

    @patch("subprocess.Popen")
    @patch("clud.service.server.DAEMON_PID_FILE")
    def test_spawn_daemon_popen_exception(self, mock_pid_file: MagicMock, mock_popen: MagicMock) -> None:
        """Test daemon spawn with Popen exception."""
        mock_pid_file.parent.mkdir = Mock()
        mock_popen.side_effect = Exception("Failed to spawn")

        result = spawn_daemon()

        self.assertFalse(result)

    @patch("subprocess.Popen")
    @patch("clud.service.server.DAEMON_PID_FILE")
    def test_spawn_daemon_creates_config_dir(self, mock_pid_file: MagicMock, mock_popen: MagicMock) -> None:
        """Test that daemon spawn creates config directory."""
        mock_process = Mock()
        mock_process.pid = 12345
        mock_popen.return_value = mock_process

        # Mock PID file and parent
        mock_parent = Mock()
        mock_pid_file.parent = mock_parent
        mock_pid_file.write_text = Mock()

        result = spawn_daemon()

        self.assertTrue(result)
        mock_parent.mkdir.assert_called_once_with(parents=True, exist_ok=True)


class TestEnsureDaemonRunning(unittest.TestCase):
    """Test ensuring daemon is running."""

    @patch("clud.service.server.is_daemon_running")
    def test_ensure_daemon_already_running(self, mock_is_running: MagicMock) -> None:
        """Test when daemon is already running."""
        mock_is_running.return_value = True

        result = ensure_daemon_running()

        self.assertTrue(result)
        mock_is_running.assert_called_once()

    @patch("clud.service.server.spawn_daemon")
    @patch("clud.service.server.is_daemon_running")
    def test_ensure_daemon_spawn_success(self, mock_is_running: MagicMock, mock_spawn: MagicMock) -> None:
        """Test successful daemon spawn and startup."""
        # First call: daemon not running, subsequent calls: daemon is running
        mock_is_running.side_effect = [False, True]
        mock_spawn.return_value = True

        result = ensure_daemon_running()

        self.assertTrue(result)
        self.assertEqual(mock_is_running.call_count, 2)
        mock_spawn.assert_called_once()

    @patch("clud.service.server.spawn_daemon")
    @patch("clud.service.server.is_daemon_running")
    def test_ensure_daemon_spawn_fails(self, mock_is_running: MagicMock, mock_spawn: MagicMock) -> None:
        """Test when spawn_daemon fails."""
        mock_is_running.return_value = False
        mock_spawn.return_value = False

        result = ensure_daemon_running()

        self.assertFalse(result)
        mock_spawn.assert_called_once()

    @patch("clud.service.server.time.sleep")
    @patch("clud.service.server.time.time")
    @patch("clud.service.server.spawn_daemon")
    @patch("clud.service.server.is_daemon_running")
    def test_ensure_daemon_timeout(self, mock_is_running: MagicMock, mock_spawn: MagicMock, mock_time: MagicMock, mock_sleep: MagicMock) -> None:
        """Test when daemon doesn't start within timeout."""
        # Mock time.time() to simulate timeout quickly
        # Need more values: start_time, while condition checks, and final elapsed
        mock_time.side_effect = [0.0, 0.1, 0.2, 0.3, 0.4, 0.6, 0.6]  # Last value exceeds max_wait

        # Daemon never becomes running
        mock_is_running.return_value = False
        mock_spawn.return_value = True

        result = ensure_daemon_running(max_wait=0.5)

        self.assertFalse(result)
        mock_spawn.assert_called_once()
        # Should have multiple is_running checks
        self.assertGreater(mock_is_running.call_count, 1)

    @patch("time.sleep")
    @patch("clud.service.server.spawn_daemon")
    @patch("clud.service.server.is_daemon_running")
    def test_ensure_daemon_eventual_success(self, mock_is_running: MagicMock, mock_spawn: MagicMock, mock_sleep: MagicMock) -> None:
        """Test when daemon starts after a few retries."""
        # Daemon not running at first, then becomes running after a few checks
        mock_is_running.side_effect = [False, False, False, True]
        mock_spawn.return_value = True

        result = ensure_daemon_running()

        self.assertTrue(result)
        mock_spawn.assert_called_once()
        self.assertEqual(mock_is_running.call_count, 4)


class TestDaemonConfiguration(unittest.TestCase):
    """Test daemon configuration constants."""

    def test_daemon_host(self) -> None:
        """Test that daemon host is localhost."""
        self.assertEqual(DAEMON_HOST, "127.0.0.1")

    def test_daemon_port(self) -> None:
        """Test that daemon port is configured."""
        self.assertIsInstance(DAEMON_PORT, int)
        self.assertGreater(DAEMON_PORT, 1024)
        self.assertLess(DAEMON_PORT, 65536)

    def test_daemon_pid_file_path(self) -> None:
        """Test that PID file is in expected location."""
        self.assertIsInstance(DAEMON_PID_FILE, Path)
        self.assertEqual(DAEMON_PID_FILE.name, "daemon.pid")
        self.assertIn(".config", str(DAEMON_PID_FILE))
        self.assertIn("clud", str(DAEMON_PID_FILE))


if __name__ == "__main__":
    unittest.main()
