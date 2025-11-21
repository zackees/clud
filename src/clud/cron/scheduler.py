"""Cron scheduler for managing task execution timing.

This module provides the core scheduling engine that evaluates cron expressions
and determines when tasks should be executed.
"""

import logging
from datetime import datetime

from croniter import croniter

from clud.cron.config import CronConfigManager
from clud.cron.models import CronTask

logger = logging.getLogger(__name__)


class CronScheduler:
    """Manages cron task scheduling and next run time calculations."""

    def __init__(self, config_manager: CronConfigManager | None = None) -> None:
        """Initialize scheduler.

        Args:
            config_manager: Configuration manager (creates default if None)
        """
        self.config_manager = config_manager or CronConfigManager()

    def add_task(self, cron_expr: str, task_file: str) -> CronTask:
        """Register new scheduled task.

        Args:
            cron_expr: Cron expression (e.g., "0 9 * * *")
            task_file: Absolute path to task file

        Returns:
            Created CronTask instance

        Raises:
            ValueError: If cron expression is invalid, file doesn't exist, or duplicate task exists
        """
        # Validate cron expression
        if not self._is_valid_cron_expression(cron_expr):
            raise ValueError(f"Invalid cron expression: {cron_expr}")

        # Validate task file exists
        from pathlib import Path

        task_path = Path(task_file).expanduser()
        if not task_path.exists():
            raise ValueError(f"Task file does not exist: {task_file}")
        if not task_path.is_file():
            raise ValueError(f"Task path is not a file: {task_file}")

        # Check for duplicate tasks (same cron expression and file path)
        config = self.config_manager.load()
        for existing_task in config.tasks:
            if existing_task.cron_expression == cron_expr and existing_task.task_file_path == task_file:
                raise ValueError(f"Duplicate task already exists: task {existing_task.id} with schedule '{cron_expr}' for file {task_file}")

        # Create task with calculated next run time
        task = CronTask(
            cron_expression=cron_expr,
            task_file_path=task_file,
        )

        # Calculate next run time
        next_run = self.get_next_run_time(cron_expr)
        task.next_run = next_run

        # Add to configuration
        config.add_task(task)
        self.config_manager.save(config)

        logger.info(f"Added task {task.id} with schedule '{cron_expr}' for file {task_file}")
        return task

    def remove_task(self, task_id: str) -> bool:
        """Remove scheduled task.

        Args:
            task_id: Task ID to remove

        Returns:
            True if task was removed, False if not found
        """
        config = self.config_manager.load()
        result = config.remove_task(task_id)

        if result:
            self.config_manager.save(config)
            logger.info(f"Removed task {task_id}")
        else:
            logger.warning(f"Task {task_id} not found")

        return result

    def get_next_run_time(self, cron_expr: str, base_time: datetime | None = None) -> float:
        """Calculate next execution time for a cron expression.

        Args:
            cron_expr: Cron expression (e.g., "0 9 * * *")
            base_time: Base time for calculation (defaults to now)

        Returns:
            Unix timestamp of next run time

        Raises:
            ValueError: If cron expression is invalid
        """
        if base_time is None:
            base_time = datetime.now()

        try:
            cron = croniter(cron_expr, base_time)
            next_run = cron.get_next(datetime)
            return next_run.timestamp()
        except Exception as e:
            raise ValueError(f"Invalid cron expression '{cron_expr}': {e}") from e

    def check_due_tasks(self, current_time: datetime | None = None) -> list[CronTask]:
        """Return list of tasks ready to execute.

        Args:
            current_time: Current time for comparison (defaults to now)

        Returns:
            List of CronTask instances that are due for execution
        """
        if current_time is None:
            current_time = datetime.now()

        current_timestamp = current_time.timestamp()
        config = self.config_manager.load()
        due_tasks: list[CronTask] = []

        for task in config.tasks:
            # Skip disabled tasks
            if not task.enabled:
                continue

            # Skip tasks without next_run time (should not happen, but be defensive)
            if task.next_run is None:
                logger.warning(f"Task {task.id} has no next_run time, recalculating")
                task.next_run = self.get_next_run_time(task.cron_expression, current_time)
                continue

            # Check if task is due
            if task.next_run <= current_timestamp:
                due_tasks.append(task)
                logger.debug(f"Task {task.id} is due (next_run: {task.next_run}, current: {current_timestamp})")

        return due_tasks

    def update_task_after_execution(
        self,
        task_id: str,
        execution_time: datetime | None = None,
        success: bool = True,
    ) -> None:
        """Update task timestamps and failure tracking after execution.

        Args:
            task_id: Task ID to update
            execution_time: Time of execution (defaults to now)
            success: Whether execution was successful (True) or failed (False)
        """
        if execution_time is None:
            execution_time = datetime.now()

        config = self.config_manager.load()
        task = config.get_task_by_id(task_id)

        if task is None:
            logger.warning(f"Task {task_id} not found for update")
            return

        # Update last run time
        task.last_run = execution_time.timestamp()

        # Update failure tracking
        if success:
            # Reset failure counters on success
            task.consecutive_failures = 0
            task.last_failure_time = None
            logger.debug(f"Task {task_id} executed successfully, reset failure counters")
        else:
            # Increment failure counters on failure
            task.consecutive_failures += 1
            task.last_failure_time = execution_time.timestamp()
            logger.warning(f"Task {task_id} failed, consecutive failures: {task.consecutive_failures}")

            # Auto-disable task if it exceeds max consecutive failures
            from clud.cron.executor import TaskExecutor

            if task.consecutive_failures >= TaskExecutor.MAX_CONSECUTIVE_FAILURES:
                task.enabled = False
                logger.error(f"Task {task_id} disabled after {task.consecutive_failures} consecutive failures")

        # Calculate next run time
        task.next_run = self.get_next_run_time(task.cron_expression, execution_time)

        # Save updated config
        self.config_manager.save(config)
        logger.debug(f"Updated task {task_id}: last_run={task.last_run}, next_run={task.next_run}, consecutive_failures={task.consecutive_failures}")

    def _is_valid_cron_expression(self, cron_expr: str) -> bool:
        """Validate cron expression syntax.

        Args:
            cron_expr: Cron expression to validate

        Returns:
            True if valid, False otherwise
        """
        try:
            croniter(cron_expr)
            return True
        except Exception as e:
            logger.debug(f"Invalid cron expression '{cron_expr}': {e}")
            return False

    def list_tasks(self) -> list[CronTask]:
        """Get list of all scheduled tasks.

        Returns:
            List of all CronTask instances
        """
        config = self.config_manager.load()
        return config.tasks

    def validate_task_files(self) -> list[tuple[str, str]]:
        """Validate that all task files exist.

        Returns:
            List of tuples (task_id, task_file_path) for tasks with missing files
        """
        from pathlib import Path

        config = self.config_manager.load()
        missing_files: list[tuple[str, str]] = []

        for task in config.tasks:
            task_path = Path(task.task_file_path).expanduser()
            if not task_path.exists() or not task_path.is_file():
                missing_files.append((task.id, task.task_file_path))
                logger.warning(f"Task {task.id} has missing or invalid file: {task.task_file_path}")

        return missing_files

    def get_next_task_time(self, current_time: datetime | None = None) -> float | None:
        """Get the timestamp of the next task that will be due.

        This is used to optimize the scheduler loop by sleeping until the next task
        instead of polling every 60 seconds.

        Args:
            current_time: Current time for comparison (defaults to now)

        Returns:
            Unix timestamp of next task, or None if no enabled tasks exist
        """
        if current_time is None:
            current_time = datetime.now()

        config = self.config_manager.load()
        next_task_time: float | None = None

        for task in config.tasks:
            # Skip disabled tasks
            if not task.enabled:
                continue

            # Skip tasks without next_run time
            if task.next_run is None:
                continue

            # Find earliest next_run time
            if next_task_time is None or task.next_run < next_task_time:
                next_task_time = task.next_run

        return next_task_time
