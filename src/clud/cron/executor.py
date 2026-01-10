"""Task executor for running clud tasks.

This module handles the execution of scheduled tasks via subprocess,
capturing output and saving logs. Includes retry logic with exponential backoff.
"""

import logging
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path

from clud.cron.models import CronTask

logger = logging.getLogger(__name__)


class TaskExecutor:
    """Executes cron tasks and manages output logging with retry support."""

    # Maximum number of retry attempts for failed tasks
    MAX_RETRIES = 3
    # Base delay for exponential backoff (seconds)
    BASE_RETRY_DELAY = 2
    # Maximum number of consecutive failures before marking task as failing
    MAX_CONSECUTIVE_FAILURES = 3

    def __init__(self, log_directory: str | None = None, test_mode: bool = False) -> None:
        """Initialize task executor.

        Args:
            log_directory: Directory for storing execution logs (defaults to ~/.clud/logs/cron)
            test_mode: If True, use minimal delays for faster testing
        """
        if log_directory is None:
            log_directory = "~/.clud/logs/cron"
        self.log_directory = Path(log_directory).expanduser()
        self.test_mode = test_mode

    def execute_task(self, task: CronTask) -> tuple[int, Path]:
        """Spawn clud subprocess for task execution with retry logic.

        Args:
            task: CronTask to execute

        Returns:
            Tuple of (return_code, log_file_path)
        """
        # Create log directory structure
        task_log_dir = self.log_directory / task.id
        task_log_dir.mkdir(parents=True, exist_ok=True)

        # Generate log file path with timestamp
        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        log_file = task_log_dir / f"{timestamp}.log"

        logger.info(f"Executing task {task.id}: {task.task_file_path}")

        # Prepare command
        cmd = [sys.executable, "-m", "clud", "-f", task.task_file_path]

        # Execute task with retry logic
        return_code = self._run_with_retries(cmd, log_file, task)

        if return_code == 0:
            logger.info(f"Task {task.id} completed successfully (return code: {return_code})")
        else:
            logger.warning(f"Task {task.id} failed with return code: {return_code}")

        return return_code, log_file

    def _run_with_retries(self, cmd: list[str], log_file: Path, task: CronTask) -> int:
        """Run subprocess with exponential backoff retry logic.

        Args:
            cmd: Command to execute
            log_file: Path to log file for output
            task: CronTask being executed (for logging context)

        Returns:
            Process return code (0 on success, non-zero on failure)
        """
        attempt = 0
        last_return_code = 1

        while attempt <= self.MAX_RETRIES:
            # Execute subprocess
            return_code = self._run_subprocess(cmd, log_file, attempt)

            # Success - return immediately
            if return_code == 0:
                if attempt > 0:
                    logger.info(f"Task {task.id} succeeded on retry attempt {attempt}/{self.MAX_RETRIES}")
                return 0

            # Failure - log and potentially retry
            last_return_code = return_code
            attempt += 1

            if attempt <= self.MAX_RETRIES:
                # Calculate exponential backoff delay: 2^attempt * BASE_RETRY_DELAY
                delay = (2 ** (attempt - 1)) * self.BASE_RETRY_DELAY
                # In test mode, use minimal delays for faster testing
                if self.test_mode:
                    delay = 0.01  # 10ms delay in test mode
                logger.warning(f"Task {task.id} failed (attempt {attempt}/{self.MAX_RETRIES + 1}, return code: {return_code}), retrying in {delay}s...")
                time.sleep(delay)
            else:
                logger.error(f"Task {task.id} failed after {self.MAX_RETRIES + 1} attempts (final return code: {return_code})")

        return last_return_code

    def _run_subprocess(self, cmd: list[str], log_file: Path, attempt: int = 0) -> int:
        """Run subprocess and capture output to log file.

        Args:
            cmd: Command to execute
            log_file: Path to log file for output
            attempt: Attempt number for retry tracking (0 for first attempt)

        Returns:
            Process return code
        """
        try:
            # Use append mode for retries to preserve previous attempt logs
            mode = "a" if attempt > 0 else "w"
            with open(log_file, mode, encoding="utf-8") as log_handle:
                # Write execution metadata
                if attempt > 0:
                    log_handle.write(f"\n{'=' * 60}\n")
                    log_handle.write(f"=== RETRY ATTEMPT {attempt} ===\n")
                    log_handle.write(f"{'=' * 60}\n\n")
                log_handle.write("=== Cron Task Execution ===\n")
                log_handle.write(f"Timestamp: {datetime.now().isoformat()}\n")
                log_handle.write(f"Command: {' '.join(cmd)}\n")
                if attempt > 0:
                    log_handle.write(f"Attempt: {attempt + 1}/{self.MAX_RETRIES + 1}\n")
                log_handle.write(f"{'=' * 60}\n\n")
                log_handle.flush()

                # Execute task (with console hiding on Windows)
                # On Windows, use CREATE_NO_WINDOW to prevent console window from appearing
                if sys.platform == "win32":
                    CREATE_NO_WINDOW = 0x08000000
                    result = subprocess.run(
                        cmd,
                        stdout=log_handle,
                        stderr=subprocess.STDOUT,  # Merge stderr into stdout
                        text=True,
                        check=False,  # Don't raise on non-zero exit
                        creationflags=CREATE_NO_WINDOW,
                    )
                else:
                    result = subprocess.run(
                        cmd,
                        stdout=log_handle,
                        stderr=subprocess.STDOUT,  # Merge stderr into stdout
                        text=True,
                        check=False,  # Don't raise on non-zero exit
                    )

                # Write completion metadata
                log_handle.write(f"\n{'=' * 60}\n")
                log_handle.write(f"Return code: {result.returncode}\n")
                log_handle.write(f"Completed: {datetime.now().isoformat()}\n")

                return result.returncode

        except FileNotFoundError as e:
            logger.error(f"Command not found: {e}")
            self._write_error_log(log_file, f"Command not found: {e}", attempt)
            return 127  # Command not found exit code

        except PermissionError as e:
            logger.error(f"Permission denied: {e}")
            self._write_error_log(log_file, f"Permission denied: {e}", attempt)
            return 126  # Permission denied exit code

        except Exception as e:
            logger.error(f"Failed to execute subprocess: {e}")
            self._write_error_log(log_file, f"Execution failed: {e}", attempt)
            return 1  # Generic error exit code

    def _write_error_log(self, log_file: Path, error_message: str, attempt: int = 0) -> None:
        """Write error message to log file.

        Args:
            log_file: Path to log file
            error_message: Error message to write
            attempt: Attempt number for retry tracking (0 for first attempt)
        """
        try:
            # Use append mode for retries
            mode = "a" if attempt > 0 else "w"
            with open(log_file, mode, encoding="utf-8") as log_handle:
                if attempt > 0:
                    log_handle.write(f"\n{'=' * 60}\n")
                    log_handle.write(f"=== RETRY ATTEMPT {attempt} ===\n")
                    log_handle.write(f"{'=' * 60}\n\n")
                log_handle.write("=== Cron Task Execution Error ===\n")
                log_handle.write(f"Timestamp: {datetime.now().isoformat()}\n")
                if attempt > 0:
                    log_handle.write(f"Attempt: {attempt + 1}/{self.MAX_RETRIES + 1}\n")
                log_handle.write(f"Error: {error_message}\n")
        except Exception as write_error:
            logger.error(f"Failed to write error log: {write_error}")

    def get_task_logs(self, task_id: str) -> list[Path]:
        """Get list of log files for a task.

        Args:
            task_id: Task ID to get logs for

        Returns:
            List of log file paths, sorted by modification time (newest first)
        """
        task_log_dir = self.log_directory / task_id

        if not task_log_dir.exists():
            return []

        # Get all .log files in task directory
        log_files = list(task_log_dir.glob("*.log"))

        # Sort by modification time (newest first)
        log_files.sort(key=lambda p: p.stat().st_mtime, reverse=True)

        return log_files

    def get_latest_log(self, task_id: str) -> Path | None:
        """Get the most recent log file for a task.

        Args:
            task_id: Task ID to get log for

        Returns:
            Path to latest log file, or None if no logs exist
        """
        logs = self.get_task_logs(task_id)
        return logs[0] if logs else None
