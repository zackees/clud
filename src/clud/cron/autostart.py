"""
Cross-platform autostart configuration for cron daemon.

Provides platform-specific installation methods with fallback strategies:
- Linux: systemd user unit (primary), crontab @reboot (fallback)
- macOS: launchd user agent (primary), Login Items via AppleScript (fallback)
- Windows: Task Scheduler user task (primary), Registry Run key (fallback)
"""

import logging
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Literal

logger = logging.getLogger(__name__)

AutostartMethod = Literal[
    "systemd",
    "crontab",
    "launchd",
    "login_items",
    "task_scheduler",
    "registry",
]
AutostartStatus = Literal["installed", "not_installed", "unknown"]


def _get_python_executable() -> str:
    """
    Get the appropriate Python executable for the current platform.

    On Windows, tries to use pythonw.exe to prevent console windows.
    On other platforms, returns sys.executable.

    Returns:
        Path to Python executable
    """
    python_exe = sys.executable

    if sys.platform == "win32":
        # Try to find pythonw.exe in the same directory as python.exe
        pythonw_exe = Path(sys.executable).parent / "pythonw.exe"
        if pythonw_exe.exists():
            python_exe = str(pythonw_exe)
            logger.info(f"Using pythonw.exe for Windows: {python_exe}")
        else:
            logger.warning(f"pythonw.exe not found at {pythonw_exe}, using python.exe (may show console)")

    return python_exe


class AutostartInstaller:
    """Cross-platform autostart configuration manager."""

    def __init__(self, config_dir: str | None = None) -> None:
        """
        Initialize autostart installer.

        Args:
            config_dir: Directory for configuration files (default: ~/.clud)
        """
        if config_dir is None:
            config_dir = str(Path.home() / ".clud")
        self.config_dir = Path(config_dir).expanduser()
        self.config_dir.mkdir(parents=True, exist_ok=True)

        # Detect platform
        self.platform = sys.platform
        self.is_linux = self.platform.startswith("linux")
        self.is_macos = self.platform == "darwin"
        self.is_windows = self.platform == "win32"

    def install(self) -> tuple[bool, str, AutostartMethod | None]:
        """
        Install autostart configuration for current platform.

        Returns:
            Tuple of (success, message, method_used)
        """
        logger.info(f"Installing autostart on platform: {self.platform}")

        if self.is_linux:
            return self._install_linux()
        elif self.is_macos:
            return self._install_macos()
        elif self.is_windows:
            return self._install_windows()
        else:
            msg = f"Unsupported platform: {self.platform}"
            logger.error(msg)
            return False, msg, None

    def status(self) -> tuple[AutostartStatus, str, AutostartMethod | None]:
        """
        Check autostart installation status.

        Returns:
            Tuple of (status, message, method_detected)
        """
        if self.is_linux:
            return self._status_linux()
        elif self.is_macos:
            return self._status_macos()
        elif self.is_windows:
            return self._status_windows()
        else:
            return "unknown", f"Unsupported platform: {self.platform}", None

    # Linux Implementation

    def _install_linux(self) -> tuple[bool, str, AutostartMethod | None]:
        """Install autostart on Linux (systemd primary, crontab fallback)."""
        # Try systemd first
        success, message = self._install_systemd()
        if success:
            return True, message, "systemd"

        logger.warning(f"Systemd installation failed: {message}")
        logger.info("Trying crontab fallback...")

        # Fallback to crontab
        success, message = self._install_crontab()
        if success:
            return True, f"Fallback: {message}", "crontab"

        return False, f"All methods failed. Last error: {message}", None

    def _install_systemd(self) -> tuple[bool, str]:
        """Install systemd user unit."""
        try:
            # Create systemd user directory
            systemd_dir = Path.home() / ".config" / "systemd" / "user"
            systemd_dir.mkdir(parents=True, exist_ok=True)

            # Get absolute paths
            python_path = sys.executable
            if not Path(python_path).exists():
                return False, f"Python executable not found: {python_path}"

            # Create systemd unit file
            unit_file = systemd_dir / "clud-cron.service"
            unit_content = f"""[Unit]
Description=Clud Cron Scheduler
After=network.target

[Service]
Type=simple
ExecStart={python_path} -m clud.cron.daemon run
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
"""
            unit_file.write_text(unit_content)
            logger.info(f"Created systemd unit: {unit_file}")

            # Enable the unit
            result = subprocess.run(
                ["systemctl", "--user", "enable", "clud-cron.service"],
                capture_output=True,
                text=True,
                timeout=10,
            )

            if result.returncode == 0:
                msg = f"Systemd unit installed: {unit_file}"
                logger.info(msg)
                return True, msg
            else:
                return False, f"Failed to enable systemd unit: {result.stderr}"

        except FileNotFoundError:
            return False, "systemctl command not found"
        except subprocess.TimeoutExpired:
            return False, "systemctl command timed out"
        except Exception as e:
            return False, f"Unexpected error: {e}"

    def _install_crontab(self) -> tuple[bool, str]:
        """Install crontab @reboot entry."""
        try:
            # Get absolute paths
            clud_path = shutil.which("clud")
            if not clud_path:
                # Try to find clud in the same directory as Python
                python_dir = Path(sys.executable).parent
                clud_path = str(python_dir / "clud")
                if not Path(clud_path).exists():
                    return False, "clud executable not found in PATH"

            # Create crontab entry
            crontab_entry = f"@reboot {clud_path} --cron start"

            # Get current crontab
            result = subprocess.run(
                ["crontab", "-l"],
                capture_output=True,
                text=True,
                timeout=5,
            )

            current_crontab = result.stdout if result.returncode == 0 else ""

            # Check if entry already exists
            if crontab_entry in current_crontab:
                return True, "Crontab entry already exists"

            # Add entry to crontab
            new_crontab = current_crontab + f"\n{crontab_entry}\n"
            result = subprocess.run(
                ["crontab", "-"],
                input=new_crontab,
                capture_output=True,
                text=True,
                timeout=5,
            )

            if result.returncode == 0:
                msg = "Crontab @reboot entry installed"
                logger.info(msg)
                return True, msg
            else:
                return False, f"Failed to update crontab: {result.stderr}"

        except FileNotFoundError:
            return False, "crontab command not found"
        except subprocess.TimeoutExpired:
            return False, "crontab command timed out"
        except Exception as e:
            return False, f"Unexpected error: {e}"

    def _status_linux(self) -> tuple[AutostartStatus, str, AutostartMethod | None]:
        """Check Linux autostart status."""
        # Check systemd first
        systemd_file = Path.home() / ".config" / "systemd" / "user" / "clud-cron.service"
        if systemd_file.exists():
            try:
                result = subprocess.run(
                    ["systemctl", "--user", "is-enabled", "clud-cron.service"],
                    capture_output=True,
                    text=True,
                    timeout=5,
                )
                if result.returncode == 0 and "enabled" in result.stdout:
                    return "installed", f"Systemd unit enabled: {systemd_file}", "systemd"
            except (FileNotFoundError, subprocess.TimeoutExpired):
                pass

        # Check crontab fallback
        try:
            result = subprocess.run(
                ["crontab", "-l"],
                capture_output=True,
                text=True,
                timeout=5,
            )
            if result.returncode == 0 and "@reboot" in result.stdout and "clud --cron start" in result.stdout:
                return "installed", "Crontab @reboot entry found", "crontab"
        except (FileNotFoundError, subprocess.TimeoutExpired):
            pass

        return "not_installed", "No autostart configuration found", None

    # macOS Implementation

    def _install_macos(self) -> tuple[bool, str, AutostartMethod | None]:
        """Install autostart on macOS (launchd primary, Login Items fallback)."""
        # Try launchd first
        success, message = self._install_launchd()
        if success:
            return True, message, "launchd"

        logger.warning(f"Launchd installation failed: {message}")
        logger.info("Trying Login Items fallback...")

        # Fallback to Login Items
        success, message = self._install_login_items()
        if success:
            return True, f"Fallback: {message}", "login_items"

        return False, f"All methods failed. Last error: {message}", None

    def _install_launchd(self) -> tuple[bool, str]:
        """Install launchd user agent."""
        try:
            # Create LaunchAgents directory
            launchagents_dir = Path.home() / "Library" / "LaunchAgents"
            launchagents_dir.mkdir(parents=True, exist_ok=True)

            # Get absolute paths
            python_path = sys.executable
            if not Path(python_path).exists():
                return False, f"Python executable not found: {python_path}"

            # Create launchd plist file
            plist_file = launchagents_dir / "com.clud.cron.plist"
            plist_content = f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.clud.cron</string>
    <key>ProgramArguments</key>
    <array>
        <string>{python_path}</string>
        <string>-m</string>
        <string>clud.cron.daemon</string>
        <string>run</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{self.config_dir}/logs/cron-daemon.log</string>
    <key>StandardErrorPath</key>
    <string>{self.config_dir}/logs/cron-daemon.log</string>
</dict>
</plist>
"""
            plist_file.write_text(plist_content)
            logger.info(f"Created launchd plist: {plist_file}")

            # Load the plist
            result = subprocess.run(
                ["launchctl", "load", str(plist_file)],
                capture_output=True,
                text=True,
                timeout=10,
            )

            if result.returncode == 0:
                msg = f"Launchd agent installed: {plist_file}"
                logger.info(msg)
                return True, msg
            else:
                # Load can fail if already loaded, check if it's in the list
                list_result = subprocess.run(
                    ["launchctl", "list"],
                    capture_output=True,
                    text=True,
                    timeout=5,
                )
                if "com.clud.cron" in list_result.stdout:
                    return True, f"Launchd agent already loaded: {plist_file}"
                return False, f"Failed to load launchd agent: {result.stderr}"

        except FileNotFoundError:
            return False, "launchctl command not found"
        except subprocess.TimeoutExpired:
            return False, "launchctl command timed out"
        except Exception as e:
            return False, f"Unexpected error: {e}"

    def _install_login_items(self) -> tuple[bool, str]:
        """Install Login Items via AppleScript."""
        try:
            # Get absolute path to clud
            clud_path = shutil.which("clud")
            if not clud_path:
                python_dir = Path(sys.executable).parent
                clud_path = str(python_dir / "clud")
                if not Path(clud_path).exists():
                    return False, "clud executable not found in PATH"

            # Create a small shell script as the login item
            script_path = self.config_dir / "autostart.sh"
            script_content = f"""#!/bin/bash
{clud_path} --cron start
"""
            script_path.write_text(script_content)
            script_path.chmod(0o755)

            # Use osascript to add login item
            applescript = f"""
tell application "System Events"
    make login item at end with properties {{path:"{script_path}", hidden:false}}
end tell
"""
            result = subprocess.run(
                ["osascript", "-e", applescript],
                capture_output=True,
                text=True,
                timeout=10,
            )

            if result.returncode == 0:
                msg = f"Login item installed: {script_path}"
                logger.info(msg)
                return True, msg
            else:
                return False, f"Failed to add login item: {result.stderr}"

        except FileNotFoundError:
            return False, "osascript command not found"
        except subprocess.TimeoutExpired:
            return False, "osascript command timed out"
        except Exception as e:
            return False, f"Unexpected error: {e}"

    def _status_macos(self) -> tuple[AutostartStatus, str, AutostartMethod | None]:
        """Check macOS autostart status."""
        # Check launchd first
        plist_file = Path.home() / "Library" / "LaunchAgents" / "com.clud.cron.plist"
        if plist_file.exists():
            try:
                result = subprocess.run(
                    ["launchctl", "list"],
                    capture_output=True,
                    text=True,
                    timeout=5,
                )
                if result.returncode == 0 and "com.clud.cron" in result.stdout:
                    return "installed", f"Launchd agent loaded: {plist_file}", "launchd"
            except (FileNotFoundError, subprocess.TimeoutExpired):
                pass

        # Check Login Items fallback (check for autostart script)
        script_path = self.config_dir / "autostart.sh"
        if script_path.exists():
            return "installed", f"Login item script found: {script_path}", "login_items"

        return "not_installed", "No autostart configuration found", None

    # Windows Implementation

    def _install_windows(self) -> tuple[bool, str, AutostartMethod | None]:
        """Install autostart on Windows (Task Scheduler primary, Registry fallback)."""
        # Try Task Scheduler first
        success, message = self._install_task_scheduler()
        if success:
            return True, message, "task_scheduler"

        logger.warning(f"Task Scheduler installation failed: {message}")
        logger.info("Trying Registry fallback...")

        # Fallback to Registry
        success, message = self._install_registry()
        if success:
            return True, f"Fallback: {message}", "registry"

        return False, f"All methods failed. Last error: {message}", None

    def _install_task_scheduler(self) -> tuple[bool, str]:
        """Install Task Scheduler task."""
        try:
            # Get absolute paths (use pythonw.exe on Windows to prevent console windows)
            python_path = _get_python_executable()
            if not Path(python_path).exists():
                return False, f"Python executable not found: {python_path}"

            # Create task with schtasks
            task_name = "CludCron"
            command = f'"{python_path}" -m clud.cron.daemon run'

            # Delete existing task if present (ignore errors)
            subprocess.run(
                ["schtasks", "/delete", "/tn", task_name, "/f"],
                capture_output=True,
                timeout=10,
            )

            # Create new task
            result = subprocess.run(
                [
                    "schtasks",
                    "/create",
                    "/tn",
                    task_name,
                    "/tr",
                    command,
                    "/sc",
                    "onlogon",
                    "/rl",
                    "limited",
                    "/f",  # Force overwrite
                ],
                capture_output=True,
                text=True,
                timeout=10,
            )

            if result.returncode == 0:
                msg = f"Task Scheduler task installed: {task_name}"
                logger.info(msg)
                return True, msg
            else:
                return False, f"Failed to create task: {result.stderr}"

        except FileNotFoundError:
            return False, "schtasks command not found"
        except subprocess.TimeoutExpired:
            return False, "schtasks command timed out"
        except Exception as e:
            return False, f"Unexpected error: {e}"

    def _install_registry(self) -> tuple[bool, str]:
        """Install Registry Run key."""
        try:
            import winreg

            # Get absolute paths (use pythonw.exe on Windows to prevent console windows)
            python_path = _get_python_executable()
            if not Path(python_path).exists():
                return False, f"Python executable not found: {python_path}"

            # Create Registry key
            key_path = r"Software\Microsoft\Windows\CurrentVersion\Run"
            key = winreg.OpenKey(
                winreg.HKEY_CURRENT_USER,
                key_path,
                0,
                winreg.KEY_SET_VALUE,
            )

            command = f'"{python_path}" -m clud.cron.daemon run'
            winreg.SetValueEx(key, "CludCron", 0, winreg.REG_SZ, command)
            winreg.CloseKey(key)

            msg = "Registry Run key installed"
            logger.info(msg)
            return True, msg

        except ImportError:
            return False, "winreg module not available (non-Windows platform?)"
        except PermissionError:
            return False, "Permission denied accessing Registry"
        except Exception as e:
            return False, f"Unexpected error: {e}"

    def _status_windows(self) -> tuple[AutostartStatus, str, AutostartMethod | None]:
        """Check Windows autostart status."""
        # Check Task Scheduler first
        try:
            result = subprocess.run(
                ["schtasks", "/query", "/tn", "CludCron"],
                capture_output=True,
                text=True,
                timeout=5,
            )
            if result.returncode == 0:
                return "installed", "Task Scheduler task found: CludCron", "task_scheduler"
        except (FileNotFoundError, subprocess.TimeoutExpired):
            pass

        # Check Registry fallback
        try:
            import winreg

            key_path = r"Software\Microsoft\Windows\CurrentVersion\Run"
            key = winreg.OpenKey(
                winreg.HKEY_CURRENT_USER,
                key_path,
                0,
                winreg.KEY_READ,
            )
            try:
                value, _ = winreg.QueryValueEx(key, "CludCron")
                winreg.CloseKey(key)
                if "clud.cron.daemon" in value:
                    return "installed", "Registry Run key found: CludCron", "registry"
            except FileNotFoundError:
                winreg.CloseKey(key)
        except (ImportError, PermissionError):
            pass

        return "not_installed", "No autostart configuration found", None
