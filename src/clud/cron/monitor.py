"""
Monitoring and health check utilities for cron daemon.

Provides health checks, metrics collection, and diagnostic information
about daemon and task execution status.
"""

import logging
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from clud.cron.config import CronConfigManager
from clud.cron.daemon import CronDaemon
from clud.cron.models import CronTask

logger = logging.getLogger(__name__)


class CronMonitor:
    """Monitor for cron daemon health and performance."""

    def __init__(self, config_dir: str | None = None) -> None:
        """
        Initialize cron monitor.

        Args:
            config_dir: Directory for configuration files (default: ~/.clud)
        """
        if config_dir is None:
            config_dir = str(Path.home() / ".clud")
        self.config_dir = Path(config_dir).expanduser()
        self.daemon = CronDaemon(config_dir=str(self.config_dir))
        config_path = self.config_dir / "cron.json"
        self.config_manager = CronConfigManager(config_path=config_path)

    def check_daemon_health(self) -> dict[str, Any]:
        """
        Check daemon health and return diagnostic information.

        Returns:
            Dictionary with health information:
            {
                "status": "running" | "stopped" | "stale",
                "pid": int | None,
                "start_time": datetime | None,
                "uptime_seconds": float | None,
                "is_healthy": bool,
                "message": str
            }
        """
        status, pid = self.daemon.status()
        start_time = self.daemon.get_start_time()
        uptime = self.daemon.get_uptime()

        is_healthy = status == "running"
        message = self._get_health_message(status, uptime)

        return {
            "status": status,
            "pid": pid,
            "start_time": start_time,
            "uptime_seconds": uptime,
            "is_healthy": is_healthy,
            "message": message,
        }

    def get_task_execution_history(self, task_id: str, limit: int = 10) -> list[dict[str, Any]]:
        """
        Get recent execution history for a task.

        Args:
            task_id: Task ID to get history for
            limit: Maximum number of history entries to return

        Returns:
            List of execution history entries with timestamps and results
        """
        task = self._get_task_by_id(task_id)
        if task is None:
            logger.warning(f"Task {task_id} not found")
            return []

        # Get task log directory
        task_log_dir = self.config_dir / "logs" / "cron" / task_id
        if not task_log_dir.exists():
            return []

        # Get all log files sorted by filename timestamp (newest first)
        # Sort by filename instead of mtime to ensure correct chronological order
        log_files = sorted(task_log_dir.glob("*.log"), key=lambda f: f.stem, reverse=True)

        history: list[dict[str, Any]] = []
        for log_file in log_files[:limit]:
            try:
                # Parse timestamp from filename (format: YYYYMMDD_HHMMSS.log)
                timestamp_str = log_file.stem
                timestamp = datetime.strptime(timestamp_str, "%Y%m%d_%H%M%S")

                # Read log file to determine success/failure
                log_content = log_file.read_text(encoding="utf-8")
                success = self._parse_execution_result(log_content)

                entry: dict[str, Any] = {
                    "timestamp": timestamp.replace(tzinfo=timezone.utc),
                    "log_file": str(log_file),
                    "success": success,
                }
                history.append(entry)
            except (ValueError, OSError) as e:
                logger.error(f"Failed to parse log file {log_file}: {e}")

        return history

    def get_stale_pid_files(self) -> list[Path]:
        """
        Detect stale PID files in the config directory.

        Returns:
            List of stale PID file paths
        """
        stale_pids: list[Path] = []

        # Check main daemon PID file
        if self.daemon.pid_file.exists():
            status, _ = self.daemon.status()
            if status == "stale":
                stale_pids.append(self.daemon.pid_file)

        return stale_pids

    def verify_task_files_exist(self) -> dict[str, bool]:
        """
        Verify that all scheduled task files exist on disk.

        Returns:
            Dictionary mapping task_id to file_exists boolean
        """
        config = self.config_manager.load()
        task_file_status: dict[str, bool] = {}

        for task in config.tasks:
            task_path = Path(task.task_file_path).expanduser()
            task_file_status[task.id] = task_path.exists()

        return task_file_status

    def get_recent_activity(self, minutes: int = 60) -> list[dict[str, Any]]:
        """
        Get recent daemon and task activity within the specified time window.

        Args:
            minutes: Time window in minutes to look back

        Returns:
            List of activity events with timestamps and descriptions
        """
        activity: list[dict[str, Any]] = []
        cutoff_time = datetime.now(timezone.utc).timestamp() - (minutes * 60)

        # Check daemon start time
        start_time = self.daemon.get_start_time()
        if start_time and start_time.timestamp() >= cutoff_time:
            daemon_entry: dict[str, Any] = {
                "timestamp": start_time,
                "type": "daemon_start",
                "description": f"Daemon started (PID: {self.daemon.get_pid()})",
            }
            activity.append(daemon_entry)

        # Check task execution logs
        config = self.config_manager.load()
        for task in config.tasks:
            task_log_dir = self.config_dir / "logs" / "cron" / task.id
            if not task_log_dir.exists():
                continue

            # Get log files modified within the time window
            for log_file in task_log_dir.glob("*.log"):
                try:
                    mtime = log_file.stat().st_mtime
                    if mtime >= cutoff_time:
                        # Parse timestamp from filename
                        timestamp_str = log_file.stem
                        timestamp = datetime.strptime(timestamp_str, "%Y%m%d_%H%M%S")
                        timestamp = timestamp.replace(tzinfo=timezone.utc)

                        # Read log to determine result
                        log_content = log_file.read_text(encoding="utf-8")
                        success = self._parse_execution_result(log_content)
                        result = "success" if success else "failure"

                        task_entry: dict[str, Any] = {
                            "timestamp": timestamp,
                            "type": "task_execution",
                            "description": f"Task {task.id} executed ({result})",
                            "task_id": task.id,
                            "result": result,
                            "log_file": str(log_file),
                        }
                        activity.append(task_entry)
                except (ValueError, OSError) as e:
                    logger.error(f"Failed to parse log file {log_file}: {e}")

        # Sort by timestamp descending
        activity.sort(key=lambda x: x["timestamp"], reverse=True)

        return activity

    def _get_health_message(self, status: str, uptime: float | None) -> str:
        """
        Generate human-readable health message.

        Args:
            status: Daemon status ("running", "stopped", "stale")
            uptime: Daemon uptime in seconds, or None

        Returns:
            Health message string
        """
        if status == "running":
            if uptime is None:
                return "Daemon is running"
            uptime_str = self._format_uptime(uptime)
            return f"Daemon is healthy (uptime: {uptime_str})"
        elif status == "stopped":
            return "Daemon is not running"
        elif status == "stale":
            return "Daemon has stale PID file (process not running)"
        else:
            return f"Daemon is in unknown state: {status}"

    def _format_uptime(self, uptime_seconds: float) -> str:
        """
        Format uptime seconds into human-readable string.

        Args:
            uptime_seconds: Uptime in seconds

        Returns:
            Formatted uptime string (e.g., "2h 15m", "45s")
        """
        uptime_int = int(uptime_seconds)

        days = uptime_int // 86400
        hours = (uptime_int % 86400) // 3600
        minutes = (uptime_int % 3600) // 60
        seconds = uptime_int % 60

        parts: list[str] = []
        if days > 0:
            parts.append(f"{days}d")
        if hours > 0:
            parts.append(f"{hours}h")
        if minutes > 0:
            parts.append(f"{minutes}m")
        if seconds > 0 or not parts:  # Always show seconds if nothing else
            parts.append(f"{seconds}s")

        return " ".join(parts)

    def _parse_execution_result(self, log_content: str) -> bool:
        """
        Parse log content to determine if execution was successful.

        Args:
            log_content: Log file content

        Returns:
            True if execution was successful, False otherwise
        """
        # Look for success/failure indicators in log
        # For now, consider it a success if no error keywords are found
        error_keywords = ["error", "failed", "exception", "traceback"]
        log_lower = log_content.lower()

        return all(keyword not in log_lower for keyword in error_keywords)

    def _get_task_by_id(self, task_id: str) -> CronTask | None:
        """
        Get task by ID from configuration.

        Args:
            task_id: Task ID to find

        Returns:
            CronTask if found, None otherwise
        """
        config = self.config_manager.load()
        for task in config.tasks:
            if task.id == task_id:
                return task
        return None
