"""Unit tests for cron autostart functionality."""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import MagicMock, Mock, patch

from clud.cron.autostart import AutostartInstaller


class TestAutostartInstaller(unittest.TestCase):
    """Test AutostartInstaller initialization and platform detection."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.temp_dir = tempfile.mkdtemp(prefix="clud_test_")
        self.installer = AutostartInstaller(config_dir=self.temp_dir)

    def test_init_creates_config_dir(self) -> None:
        """Test that initialization creates config directory."""
        self.assertTrue(self.installer.config_dir.exists())
        self.assertTrue(self.installer.config_dir.is_dir())

    def test_platform_detection(self) -> None:
        """Test platform detection logic."""
        self.assertIsInstance(self.installer.platform, str)
        # Should detect exactly one platform
        platform_count = sum(
            [
                self.installer.is_linux,
                self.installer.is_macos,
                self.installer.is_windows,
            ]
        )
        self.assertEqual(platform_count, 1)


class TestLinuxAutostart(unittest.TestCase):
    """Test Linux autostart installation (systemd + crontab)."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.temp_dir = tempfile.mkdtemp(prefix="clud_test_")
        self.installer = AutostartInstaller(config_dir=self.temp_dir)

    @patch("sys.platform", "linux")
    @patch("subprocess.run")
    @patch("pathlib.Path.write_text")
    def test_install_systemd_success(self, mock_write: Mock, mock_run: Mock) -> None:
        """Test successful systemd installation."""
        # Mock successful systemctl command
        mock_run.return_value = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout="",
            stderr="",
        )

        # Force Linux platform
        self.installer.platform = "linux"
        self.installer.is_linux = True
        self.installer.is_macos = False
        self.installer.is_windows = False

        success, message = self.installer._install_systemd()

        self.assertTrue(success)
        self.assertIn("Systemd unit installed", message)
        mock_write.assert_called_once()
        mock_run.assert_called_once()

    @patch("sys.platform", "linux")
    @patch("subprocess.run")
    def test_install_systemd_command_not_found(self, mock_run: Mock) -> None:
        """Test systemd installation when systemctl is not available."""
        mock_run.side_effect = FileNotFoundError()

        self.installer.platform = "linux"
        self.installer.is_linux = True

        success, message = self.installer._install_systemd()

        self.assertFalse(success)
        self.assertIn("systemctl command not found", message)

    @patch("sys.platform", "linux")
    @patch("subprocess.run")
    @patch("pathlib.Path.write_text")
    def test_install_systemd_enable_fails(self, mock_write: Mock, mock_run: Mock) -> None:
        """Test systemd installation when enable command fails."""
        mock_run.return_value = subprocess.CompletedProcess(
            args=[],
            returncode=1,
            stdout="",
            stderr="Failed to enable unit",
        )

        self.installer.platform = "linux"
        self.installer.is_linux = True

        success, message = self.installer._install_systemd()

        self.assertFalse(success)
        self.assertIn("Failed to enable systemd unit", message)

    @patch("sys.platform", "linux")
    @patch("subprocess.run")
    @patch("shutil.which")
    def test_install_crontab_success(self, mock_which: Mock, mock_run: Mock) -> None:
        """Test successful crontab installation."""
        mock_which.return_value = "/usr/bin/clud"

        # Mock crontab -l (no existing entries)
        # Mock crontab - (successful write)
        mock_run.side_effect = [
            subprocess.CompletedProcess(args=[], returncode=1, stdout="", stderr="no crontab"),
            subprocess.CompletedProcess(args=[], returncode=0, stdout="", stderr=""),
        ]

        self.installer.platform = "linux"
        self.installer.is_linux = True

        success, message = self.installer._install_crontab()

        self.assertTrue(success)
        self.assertIn("Crontab @reboot entry installed", message)

    @patch("sys.platform", "linux")
    @patch("subprocess.run")
    @patch("shutil.which")
    def test_install_crontab_already_exists(self, mock_which: Mock, mock_run: Mock) -> None:
        """Test crontab installation when entry already exists."""
        mock_which.return_value = "/usr/bin/clud"

        # Mock crontab -l (existing entry)
        mock_run.return_value = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout="@reboot /usr/bin/clud --cron start\n",
            stderr="",
        )

        self.installer.platform = "linux"
        self.installer.is_linux = True

        success, message = self.installer._install_crontab()

        self.assertTrue(success)
        self.assertIn("already exists", message)

    @patch("sys.platform", "linux")
    @patch("shutil.which")
    def test_install_crontab_clud_not_found(self, mock_which: Mock) -> None:
        """Test crontab installation when clud is not in PATH."""
        mock_which.return_value = None

        self.installer.platform = "linux"
        self.installer.is_linux = True

        success, message = self.installer._install_crontab()

        self.assertFalse(success)
        self.assertIn("clud executable not found", message)

    @patch("sys.platform", "linux")
    @patch("subprocess.run")
    def test_install_linux_fallback_to_crontab(self, mock_run: Mock) -> None:
        """Test Linux installation falls back to crontab when systemd fails."""
        # First call: systemctl fails
        # Second call: crontab -l returns empty
        # Third call: crontab - succeeds
        mock_run.side_effect = [
            FileNotFoundError(),  # systemctl not found
            subprocess.CompletedProcess(args=[], returncode=1, stdout="", stderr=""),  # crontab -l empty
            subprocess.CompletedProcess(args=[], returncode=0, stdout="", stderr=""),  # crontab - success
        ]

        with patch("shutil.which", return_value="/usr/bin/clud"):
            self.installer.platform = "linux"
            self.installer.is_linux = True

            success, message, method = self.installer._install_linux()

            self.assertTrue(success)
            self.assertIn("Fallback", message)
            self.assertEqual(method, "crontab")

    @patch("sys.platform", "linux")
    @patch("subprocess.run")
    def test_status_linux_systemd_enabled(self, mock_run: Mock) -> None:
        """Test Linux status when systemd is enabled."""
        mock_run.return_value = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout="enabled\n",
            stderr="",
        )

        self.installer.platform = "linux"
        self.installer.is_linux = True

        # Create mock systemd file
        systemd_dir = Path.home() / ".config" / "systemd" / "user"
        systemd_dir.mkdir(parents=True, exist_ok=True)
        systemd_file = systemd_dir / "clud-cron.service"
        systemd_file.write_text("[Unit]\nDescription=Test\n")

        try:
            status, message, method = self.installer._status_linux()

            self.assertEqual(status, "installed")
            self.assertIn("Systemd unit enabled", message)
            self.assertEqual(method, "systemd")
        finally:
            # Clean up
            if systemd_file.exists():
                systemd_file.unlink()

    @patch("sys.platform", "linux")
    @patch("subprocess.run")
    def test_status_linux_crontab_found(self, mock_run: Mock) -> None:
        """Test Linux status when crontab entry exists."""
        # Mock crontab -l with clud entry
        mock_run.return_value = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout="@reboot /usr/bin/clud --cron start\n",
            stderr="",
        )

        self.installer.platform = "linux"
        self.installer.is_linux = True

        status, message, method = self.installer._status_linux()

        self.assertEqual(status, "installed")
        self.assertIn("Crontab @reboot entry found", message)
        self.assertEqual(method, "crontab")

    @patch("sys.platform", "linux")
    @patch("subprocess.run")
    def test_status_linux_not_installed(self, mock_run: Mock) -> None:
        """Test Linux status when nothing is installed."""
        mock_run.return_value = subprocess.CompletedProcess(
            args=[],
            returncode=1,
            stdout="",
            stderr="no crontab",
        )

        self.installer.platform = "linux"
        self.installer.is_linux = True

        status, message, method = self.installer._status_linux()

        self.assertEqual(status, "not_installed")
        self.assertIn("No autostart configuration found", message)
        self.assertIsNone(method)


class TestMacOSAutostart(unittest.TestCase):
    """Test macOS autostart installation (launchd + Login Items)."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.temp_dir = tempfile.mkdtemp(prefix="clud_test_")
        self.installer = AutostartInstaller(config_dir=self.temp_dir)

    @patch("sys.platform", "darwin")
    @patch("subprocess.run")
    @patch("pathlib.Path.write_text")
    def test_install_launchd_success(self, mock_write: Mock, mock_run: Mock) -> None:
        """Test successful launchd installation."""
        mock_run.return_value = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout="",
            stderr="",
        )

        self.installer.platform = "darwin"
        self.installer.is_macos = True
        self.installer.is_linux = False
        self.installer.is_windows = False

        success, message = self.installer._install_launchd()

        self.assertTrue(success)
        self.assertIn("Launchd agent installed", message)
        mock_write.assert_called_once()

    @patch("sys.platform", "darwin")
    @patch("subprocess.run")
    def test_install_launchd_command_not_found(self, mock_run: Mock) -> None:
        """Test launchd installation when launchctl is not available."""
        mock_run.side_effect = FileNotFoundError()

        self.installer.platform = "darwin"
        self.installer.is_macos = True

        success, message = self.installer._install_launchd()

        self.assertFalse(success)
        self.assertIn("launchctl command not found", message)

    @patch("sys.platform", "darwin")
    @patch("subprocess.run")
    @patch("pathlib.Path.write_text")
    def test_install_launchd_already_loaded(self, mock_write: Mock, mock_run: Mock) -> None:
        """Test launchd installation when agent is already loaded."""
        # launchctl load fails, but list shows it's loaded
        mock_run.side_effect = [
            subprocess.CompletedProcess(args=[], returncode=1, stdout="", stderr="already loaded"),
            subprocess.CompletedProcess(args=[], returncode=0, stdout="com.clud.cron\n", stderr=""),
        ]

        self.installer.platform = "darwin"
        self.installer.is_macos = True

        success, message = self.installer._install_launchd()

        self.assertTrue(success)
        self.assertIn("already loaded", message)

    @patch("sys.platform", "darwin")
    @patch("subprocess.run")
    @patch("shutil.which")
    @patch("pathlib.Path.write_text")
    @patch("pathlib.Path.chmod")
    def test_install_login_items_success(
        self,
        mock_chmod: Mock,
        mock_write: Mock,
        mock_which: Mock,
        mock_run: Mock,
    ) -> None:
        """Test successful Login Items installation."""
        mock_which.return_value = "/usr/local/bin/clud"
        mock_run.return_value = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout="",
            stderr="",
        )

        self.installer.platform = "darwin"
        self.installer.is_macos = True

        success, message = self.installer._install_login_items()

        self.assertTrue(success)
        self.assertIn("Login item installed", message)
        mock_write.assert_called_once()
        mock_chmod.assert_called_once_with(0o755)

    @patch("sys.platform", "darwin")
    @patch("subprocess.run")
    def test_install_macos_fallback_to_login_items(self, mock_run: Mock) -> None:
        """Test macOS installation falls back to Login Items when launchd fails."""
        with patch("shutil.which", return_value="/usr/local/bin/clud"), patch("pathlib.Path.write_text"), patch("pathlib.Path.chmod"):
            # First call: launchctl fails
            # Second call: osascript succeeds
            mock_run.side_effect = [
                FileNotFoundError(),  # launchctl not found
                subprocess.CompletedProcess(args=[], returncode=0, stdout="", stderr=""),  # osascript success
            ]

            self.installer.platform = "darwin"
            self.installer.is_macos = True
            self.installer.is_linux = False
            self.installer.is_windows = False

            success, message, method = self.installer._install_macos()

            self.assertTrue(success)
            self.assertIn("Fallback", message)
            self.assertEqual(method, "login_items")

    @patch("sys.platform", "darwin")
    @patch("subprocess.run")
    def test_status_macos_launchd_loaded(self, mock_run: Mock) -> None:
        """Test macOS status when launchd is loaded."""
        mock_run.return_value = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout="com.clud.cron\n",
            stderr="",
        )

        self.installer.platform = "darwin"
        self.installer.is_macos = True

        # Create mock plist file
        plist_dir = Path.home() / "Library" / "LaunchAgents"
        plist_dir.mkdir(parents=True, exist_ok=True)
        plist_file = plist_dir / "com.clud.cron.plist"
        plist_file.write_text("<?xml version='1.0'?>\n<plist>\n</plist>")

        try:
            status, message, method = self.installer._status_macos()

            self.assertEqual(status, "installed")
            self.assertIn("Launchd agent loaded", message)
            self.assertEqual(method, "launchd")
        finally:
            # Clean up
            if plist_file.exists():
                plist_file.unlink()

    @patch("sys.platform", "darwin")
    def test_status_macos_login_items_found(self) -> None:
        """Test macOS status when Login Items script exists."""
        self.installer.platform = "darwin"
        self.installer.is_macos = True

        # Create mock autostart script
        script_path = self.installer.config_dir / "autostart.sh"
        script_path.write_text("#!/bin/bash\nclud --cron start\n")

        status, message, method = self.installer._status_macos()

        self.assertEqual(status, "installed")
        self.assertIn("Login item script found", message)
        self.assertEqual(method, "login_items")


class TestWindowsAutostart(unittest.TestCase):
    """Test Windows autostart installation (Task Scheduler + Registry)."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.temp_dir = tempfile.mkdtemp(prefix="clud_test_")
        self.installer = AutostartInstaller(config_dir=self.temp_dir)

    @patch("sys.platform", "win32")
    @patch("subprocess.run")
    def test_install_task_scheduler_success(self, mock_run: Mock) -> None:
        """Test successful Task Scheduler installation."""
        # First call: delete existing (ignore result)
        # Second call: create new task
        mock_run.side_effect = [
            subprocess.CompletedProcess(args=[], returncode=0, stdout="", stderr=""),  # delete
            subprocess.CompletedProcess(args=[], returncode=0, stdout="", stderr=""),  # create
        ]

        self.installer.platform = "win32"
        self.installer.is_windows = True
        self.installer.is_linux = False
        self.installer.is_macos = False

        success, message = self.installer._install_task_scheduler()

        self.assertTrue(success)
        self.assertIn("Task Scheduler task installed", message)

    @patch("sys.platform", "win32")
    @patch("subprocess.run")
    def test_install_task_scheduler_command_not_found(self, mock_run: Mock) -> None:
        """Test Task Scheduler installation when schtasks is not available."""
        mock_run.side_effect = FileNotFoundError()

        self.installer.platform = "win32"
        self.installer.is_windows = True

        success, message = self.installer._install_task_scheduler()

        self.assertFalse(success)
        self.assertIn("schtasks command not found", message)

    @patch("sys.platform", "win32")
    @patch("subprocess.run")
    def test_install_task_scheduler_create_fails(self, mock_run: Mock) -> None:
        """Test Task Scheduler installation when create command fails."""
        # Delete succeeds, create fails
        mock_run.side_effect = [
            subprocess.CompletedProcess(args=[], returncode=0, stdout="", stderr=""),  # delete
            subprocess.CompletedProcess(args=[], returncode=1, stdout="", stderr="Access denied"),  # create fails
        ]

        self.installer.platform = "win32"
        self.installer.is_windows = True

        success, message = self.installer._install_task_scheduler()

        self.assertFalse(success)
        self.assertIn("Failed to create task", message)

    @patch("sys.platform", "win32")
    @unittest.skipUnless(sys.platform == "win32", "Windows-only test")
    @patch("sys.platform", "win32")
    @patch("subprocess.run")
    def test_install_windows_fallback_to_registry(self, mock_run: Mock) -> None:
        """Test Windows installation falls back to Registry when Task Scheduler fails."""
        with patch("clud.cron.autostart.winreg", create=True) as mock_winreg:
            # Mock winreg
            mock_key = MagicMock()
            mock_winreg.OpenKey.return_value = mock_key
            mock_winreg.HKEY_CURRENT_USER = 0x80000001
            mock_winreg.KEY_SET_VALUE = 0x0002
            mock_winreg.REG_SZ = 1

            # schtasks fails
            mock_run.side_effect = FileNotFoundError()

            self.installer.platform = "win32"
            self.installer.is_windows = True
            self.installer.is_linux = False
            self.installer.is_macos = False

            success, message, method = self.installer._install_windows()

            self.assertTrue(success)
            self.assertIn("Fallback", message)
            self.assertEqual(method, "registry")

    @patch("sys.platform", "win32")
    @patch("subprocess.run")
    def test_status_windows_task_scheduler_found(self, mock_run: Mock) -> None:
        """Test Windows status when Task Scheduler task exists."""
        mock_run.return_value = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout="CludCron task details...\n",
            stderr="",
        )

        self.installer.platform = "win32"
        self.installer.is_windows = True

        status, message, method = self.installer._status_windows()

        self.assertEqual(status, "installed")
        self.assertIn("Task Scheduler task found", message)
        self.assertEqual(method, "task_scheduler")

    @patch("sys.platform", "win32")
    @patch("subprocess.run")
    @patch("clud.cron.autostart.winreg", create=True)
    def test_status_windows_registry_found(self, mock_winreg: Mock, mock_run: Mock) -> None:
        """Test Windows status when Registry key exists."""
        # schtasks query fails (task not found)
        mock_run.return_value = subprocess.CompletedProcess(
            args=[],
            returncode=1,
            stdout="",
            stderr="Task not found",
        )

        # Mock winreg
        mock_key = MagicMock()
        mock_winreg.OpenKey.return_value = mock_key
        mock_winreg.QueryValueEx.return_value = ('"python.exe" -m clud.cron.daemon run', 1)
        mock_winreg.HKEY_CURRENT_USER = 0x80000001
        mock_winreg.KEY_READ = 0x0001

        self.installer.platform = "win32"
        self.installer.is_windows = True

        status, message, method = self.installer._status_windows()

        self.assertEqual(status, "installed")
        self.assertIn("Registry Run key found", message)
        self.assertEqual(method, "registry")


class TestAutostartIntegration(unittest.TestCase):
    """Test integration of autostart with main installer interface."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.temp_dir = tempfile.mkdtemp(prefix="clud_test_")
        self.installer = AutostartInstaller(config_dir=self.temp_dir)

    @patch("sys.platform", "linux")
    @patch("subprocess.run")
    @patch("pathlib.Path.write_text")
    def test_install_returns_correct_format(
        self,
        mock_write: Mock,
        mock_run: Mock,
    ) -> None:
        """Test that install() returns (success, message, method) tuple."""
        mock_run.return_value = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout="",
            stderr="",
        )

        self.installer.platform = "linux"
        self.installer.is_linux = True
        self.installer.is_macos = False
        self.installer.is_windows = False

        result = self.installer.install()

        self.assertIsInstance(result, tuple)
        self.assertEqual(len(result), 3)
        success, message, method = result
        self.assertIsInstance(success, bool)
        self.assertIsInstance(message, str)
        self.assertIn(method, ["systemd", "crontab", "launchd", "login_items", "task_scheduler", "registry", None])

    @patch("sys.platform", "linux")
    @patch("subprocess.run")
    def test_status_returns_correct_format(self, mock_run: Mock) -> None:
        """Test that status() returns (status, message, method) tuple."""
        mock_run.return_value = subprocess.CompletedProcess(
            args=[],
            returncode=1,
            stdout="",
            stderr="",
        )

        self.installer.platform = "linux"
        self.installer.is_linux = True
        self.installer.is_macos = False
        self.installer.is_windows = False

        result = self.installer.status()

        self.assertIsInstance(result, tuple)
        self.assertEqual(len(result), 3)
        status, message, method = result
        self.assertIn(status, ["installed", "not_installed", "unknown"])
        self.assertIsInstance(message, str)

    def test_unsupported_platform_install(self) -> None:
        """Test install on unsupported platform."""
        self.installer.platform = "unknown_os"
        self.installer.is_linux = False
        self.installer.is_macos = False
        self.installer.is_windows = False

        success, message, method = self.installer.install()

        self.assertFalse(success)
        self.assertIn("Unsupported platform", message)
        self.assertIsNone(method)

    def test_unsupported_platform_status(self) -> None:
        """Test status on unsupported platform."""
        self.installer.platform = "unknown_os"
        self.installer.is_linux = False
        self.installer.is_macos = False
        self.installer.is_windows = False

        status, message, method = self.installer.status()

        self.assertEqual(status, "unknown")
        self.assertIn("Unsupported platform", message)
        self.assertIsNone(method)


if __name__ == "__main__":
    unittest.main()
