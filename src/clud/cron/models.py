"""Data models for cron task scheduling.

This module defines the core data structures for storing and managing cron tasks.
"""

import uuid
from dataclasses import dataclass, field
from datetime import datetime
from typing import Any, cast


@dataclass
class CronTask:
    """Represents a scheduled cron task.

    Attributes:
        id: Unique identifier for the task
        cron_expression: Cron expression for scheduling (e.g., "0 9 * * *")
        task_file_path: Absolute path to the task file to execute
        enabled: Whether the task is currently enabled
        created_at: Timestamp when task was created
        last_run: Timestamp of last execution (None if never run)
        next_run: Timestamp of next scheduled execution (None if not calculated)
        consecutive_failures: Number of consecutive failures (0 if no failures)
        last_failure_time: Timestamp of last failure (None if no recent failure)
    """

    cron_expression: str
    task_file_path: str
    id: str = field(default_factory=lambda: str(uuid.uuid4()))
    enabled: bool = True
    created_at: float = field(default_factory=lambda: datetime.now().timestamp())
    last_run: float | None = None
    next_run: float | None = None
    consecutive_failures: int = 0
    last_failure_time: float | None = None

    def to_dict(self) -> dict[str, Any]:
        """Convert task to dictionary for JSON serialization.

        Returns:
            Dictionary representation of the task
        """
        return {
            "id": self.id,
            "cron_expression": self.cron_expression,
            "task_file_path": self.task_file_path,
            "enabled": self.enabled,
            "created_at": self.created_at,
            "last_run": self.last_run,
            "next_run": self.next_run,
            "consecutive_failures": self.consecutive_failures,
            "last_failure_time": self.last_failure_time,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "CronTask":
        """Create task from dictionary (JSON deserialization).

        Args:
            data: Dictionary containing task data

        Returns:
            CronTask instance
        """
        return cls(
            id=str(data["id"]),
            cron_expression=str(data["cron_expression"]),
            task_file_path=str(data["task_file_path"]),
            enabled=bool(data.get("enabled", True)),
            created_at=float(data.get("created_at", datetime.now().timestamp())),
            last_run=float(data["last_run"]) if data.get("last_run") is not None else None,
            next_run=float(data["next_run"]) if data.get("next_run") is not None else None,
            consecutive_failures=int(data.get("consecutive_failures", 0)),
            last_failure_time=float(data["last_failure_time"]) if data.get("last_failure_time") is not None else None,
        )


@dataclass
class CronConfig:
    """Configuration for the cron scheduler.

    Attributes:
        tasks: List of scheduled cron tasks
        daemon_pid: Process ID of running daemon (None if not running)
        log_directory: Directory path for storing logs
    """

    tasks: list[CronTask] = field(default_factory=list)  # pyright: ignore[reportUnknownVariableType]
    daemon_pid: int | None = None
    log_directory: str = field(default_factory=lambda: "~/.clud/logs/cron")

    def to_dict(self) -> dict[str, Any]:
        """Convert config to dictionary for JSON serialization.

        Returns:
            Dictionary representation of the config
        """
        return {
            "tasks": [task.to_dict() for task in self.tasks],
            "daemon_pid": self.daemon_pid,
            "log_directory": self.log_directory,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "CronConfig":
        """Create config from dictionary (JSON deserialization).

        Args:
            data: Dictionary containing config data

        Returns:
            CronConfig instance
        """
        tasks_data = data.get("tasks", [])
        tasks: list[CronTask] = []
        if isinstance(tasks_data, list):
            for task_data_item in tasks_data:  # pyright: ignore[reportUnknownVariableType]
                if isinstance(task_data_item, dict):
                    # Use cast to assert type after runtime check
                    task_dict = cast(dict[str, Any], task_data_item)
                    tasks.append(CronTask.from_dict(task_dict))

        daemon_pid_value = data.get("daemon_pid")
        daemon_pid = int(daemon_pid_value) if daemon_pid_value is not None else None

        log_dir = data.get("log_directory", "~/.clud/logs/cron")
        log_directory = str(log_dir) if log_dir is not None else "~/.clud/logs/cron"

        return cls(
            tasks=tasks,
            daemon_pid=daemon_pid,
            log_directory=log_directory,
        )

    def get_task_by_id(self, task_id: str) -> CronTask | None:
        """Find task by ID.

        Args:
            task_id: Task ID to search for

        Returns:
            CronTask if found, None otherwise
        """
        for task in self.tasks:
            if task.id == task_id:
                return task
        return None

    def add_task(self, task: CronTask) -> None:
        """Add a new task to the configuration.

        Args:
            task: CronTask to add
        """
        self.tasks.append(task)

    def remove_task(self, task_id: str) -> bool:
        """Remove a task by ID.

        Args:
            task_id: Task ID to remove

        Returns:
            True if task was removed, False if not found
        """
        initial_count = len(self.tasks)
        self.tasks = [task for task in self.tasks if task.id != task_id]
        return len(self.tasks) < initial_count
