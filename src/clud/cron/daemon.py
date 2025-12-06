"""
Cross-platform daemon process manager for cron scheduler.

Provides lifecycle management (start, stop, status) with PID file handling
and graceful shutdown via signal handlers.
"""

import logging
import logging.handlers
import os
import signal
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Literal

import psutil

from clud.cron.config import CronConfigManager
from clud.cron.executor import TaskExecutor
from clud.cron.scheduler import CronScheduler

logger = logging.getLogger(__name__)


class CronDaemon:
    """Cross-platform daemon process manager for cron scheduler."""

    def __init__(self, config_dir: str | None = None) -> None:
        """
        Initialize daemon manager.

        Args:
            config_dir: Directory for configuration files (default: ~/.clud)
        """
        if config_dir is None:
            config_dir = str(Path.home() / ".clud")
        self.config_dir = Path(config_dir).expanduser()
        self.pid_file = self.config_dir / "cron.pid"
        self.start_time_file = self.config_dir / "cron.start_time"
        self.log_file = self.config_dir / "logs" / "cron-daemon.log"
        config_path = self.config_dir / "cron.json"
        self.config_manager = CronConfigManager(config_path=config_path)
        self.scheduler = CronScheduler(config_manager=self.config_manager)
        self.executor = TaskExecutor()

        # Ensure directories exist
        self.config_dir.mkdir(parents=True, exist_ok=True)
        self.log_file.parent.mkdir(parents=True, exist_ok=True)

    def start(self) -> bool:
        """
        Start the daemon process in the background.

        Returns:
            True if daemon started successfully, False if already running
        """
        # Check if daemon is already running
        if self.is_running():
            logger.warning("Daemon is already running")
            return False

        # Clean up stale PID file
        if self.pid_file.exists():
            logger.info("Cleaning up stale PID file")
            self.pid_file.unlink()

        # Start daemon as background process
        logger.info("Starting daemon process...")

        # Prepare command to run daemon loop
        # Use sys.executable to get the current Python interpreter (from uv's venv)
        # On Windows, MUST use pythonw.exe to prevent console windows
        python_exe = sys.executable
        if sys.platform == "win32":
            # Try to find pythonw.exe in the same directory as python.exe
            pythonw_exe = Path(sys.executable).parent / "pythonw.exe"
            if pythonw_exe.exists():
                python_exe = str(pythonw_exe)
                logger.info(f"Using pythonw.exe: {python_exe}")
            else:
                logger.warning(f"pythonw.exe not found at {pythonw_exe}, using python.exe (may show console)")

        cmd = [python_exe, "-m", "clud.cron", "run"]

        # Debug logging
        logger.info(f"Command to execute: {cmd}")
        logger.info(f"sys.executable: {sys.executable}")
        logger.info(f"Python executable for daemon: {python_exe}")
        logger.info(f"Platform: {sys.platform}")
        logger.info(f"Log file: {self.log_file}")
        logger.info(f"Config dir: {self.config_dir}")

        # Platform-specific background process creation
        if sys.platform == "win32":
            # Windows: Use pythonw.exe + CREATE_NO_WINDOW for no console
            # Using pythonw.exe is critical - it's designed to run without a console window
            CREATE_NO_WINDOW = 0x08000000
            CREATE_NEW_PROCESS_GROUP = 0x00000200
            DETACHED_PROCESS = 0x00000008

            # Use DETACHED_PROCESS only if we're using pythonw.exe
            # pythonw.exe handles file redirection properly with DETACHED_PROCESS
            # Fallback for python.exe: don't use DETACHED_PROCESS to avoid handle issues
            creation_flags = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS if "pythonw" in python_exe.lower() else CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP

            logger.info(f"Using Windows creation flags: {creation_flags:#x}")

            with open(self.log_file, "a", encoding="utf-8") as log_f:
                process = subprocess.Popen(
                    cmd,
                    creationflags=creation_flags,
                    stdout=log_f,
                    stderr=log_f,
                    stdin=subprocess.DEVNULL,
                )
        else:
            # Unix (Linux/macOS): Use start_new_session
            with open(self.log_file, "a", encoding="utf-8") as log_f:
                process = subprocess.Popen(
                    cmd,
                    start_new_session=True,
                    stdout=log_f,
                    stderr=log_f,
                    stdin=subprocess.DEVNULL,
                )

        # Log PID immediately after spawn
        logger.info(f"Process spawned with PID: {process.pid}")

        # Check if process is still alive immediately after spawn
        poll_result = process.poll()
        logger.info(f"Initial process poll (None=running): {poll_result}")

        # Write PID file and start time
        self.pid_file.write_text(str(process.pid), encoding="utf-8")
        start_time = datetime.now(timezone.utc)
        self.start_time_file.write_text(start_time.isoformat(), encoding="utf-8")
        logger.info(f"Daemon started with PID {process.pid} at {start_time.isoformat()}")
        logger.info(f"PID file written to: {self.pid_file}")
        logger.info(f"Start time file written to: {self.start_time_file}")

        # Give the process a moment to start
        time.sleep(0.5)

        # Check process status after sleep
        poll_result_after = process.poll()
        logger.info(f"Process poll after 0.5s (None=running): {poll_result_after}")

        # Verify daemon is running
        if self.is_running():
            logger.info("Daemon started successfully")
            return True
        else:
            logger.error("Daemon failed to start")
            return False

    def stop(self) -> bool:
        """
        Stop the daemon process gracefully.

        Returns:
            True if daemon stopped successfully, False if not running
        """
        # Check if daemon is running
        pid = self._read_pid()
        if pid is None:
            logger.warning("Daemon is not running (no PID file)")
            return False

        if not self._is_process_running(pid):
            logger.warning(f"Daemon is not running (stale PID {pid})")
            self.pid_file.unlink(missing_ok=True)
            return False

        # Send termination signal
        logger.info(f"Stopping daemon (PID {pid})...")
        try:
            if sys.platform == "win32":
                # Windows: Use taskkill
                subprocess.run(
                    ["taskkill", "/F", "/PID", str(pid)],
                    check=False,
                    capture_output=True,
                )
            else:
                # Unix: Send SIGTERM
                os.kill(pid, signal.SIGTERM)

            # Wait for process to exit (with timeout)
            max_wait = 5  # seconds
            waited = 0.0
            while waited < max_wait:
                if not self._is_process_running(pid):
                    logger.info("Daemon stopped successfully")
                    self.pid_file.unlink(missing_ok=True)
                    return True
                time.sleep(0.1)
                waited += 0.1

            # Force kill if still running
            logger.warning("Daemon did not stop gracefully, forcing kill")
            if sys.platform == "win32":
                subprocess.run(
                    ["taskkill", "/F", "/PID", str(pid)],
                    check=False,
                    capture_output=True,
                )
            else:
                os.kill(pid, signal.SIGKILL)

            time.sleep(0.5)
            self.pid_file.unlink(missing_ok=True)
            return True

        except (ProcessLookupError, PermissionError) as e:
            logger.error(f"Failed to stop daemon: {e}")
            self.pid_file.unlink(missing_ok=True)
            return False

    def status(self) -> tuple[Literal["running", "stopped", "stale"], int | None]:
        """
        Check daemon status.

        Returns:
            Tuple of (status, pid) where status is "running", "stopped", or "stale"
        """
        pid = self._read_pid()

        if pid is None:
            return ("stopped", None)

        if self._is_process_running(pid):
            return ("running", pid)
        else:
            return ("stale", pid)

    def is_running(self) -> bool:
        """
        Check if daemon is currently running.

        Returns:
            True if daemon is running, False otherwise
        """
        status, _ = self.status()
        return status == "running"

    def run_loop(self) -> None:
        """
        Main daemon loop - intelligently sleeps until next task is due.

        This method should be run as a background process via start().
        Uses optimized scheduling: sleeps until next task instead of fixed 60s polling.
        """
        # Set up logging for daemon process with rotation (max 10MB, keep 5 backups)
        log_handler = logging.handlers.RotatingFileHandler(
            self.log_file,
            maxBytes=10 * 1024 * 1024,  # 10MB
            backupCount=5,
            encoding="utf-8",
        )
        log_handler.setFormatter(logging.Formatter("%(asctime)s [%(levelname)s] %(message)s"))

        # Prepare log handlers - type as list[logging.Handler] for pyright
        log_handlers: list[logging.Handler] = [log_handler]

        # Only add console handler on non-Windows platforms to prevent console window creation
        # On Windows, pythonw.exe + console output triggers a console window to appear
        if sys.platform != "win32":
            console_handler = logging.StreamHandler()
            console_handler.setFormatter(logging.Formatter("%(asctime)s [%(levelname)s] %(message)s"))
            log_handlers.append(console_handler)

        # Configure root logger
        logging.basicConfig(
            level=logging.INFO,
            handlers=log_handlers,
        )

        # Record start time and get process handle for profiling
        start_time = datetime.now(timezone.utc)
        self.start_time_file.write_text(start_time.isoformat(), encoding="utf-8")
        process = psutil.Process(os.getpid())

        # Log initial resource usage
        cpu_percent = process.cpu_percent(interval=0.1)
        mem_info = process.memory_info()
        mem_mb = mem_info.rss / 1024 / 1024

        logger.info("=" * 80)
        logger.info(f"Daemon starting main loop at {start_time.isoformat()}")
        logger.info(f"PID: {os.getpid()}")
        logger.info(f"Config directory: {self.config_dir}")
        logger.info(f"Log file: {self.log_file}")
        logger.info(f"Initial resource usage: CPU={cpu_percent:.2f}%, Memory={mem_mb:.2f}MB")
        logger.info("=" * 80)

        # Perform crash recovery validation
        self._perform_crash_recovery()

        # Set up signal handlers for graceful shutdown
        self._setup_signal_handlers()

        # Main loop
        self.running = True
        loop_count = 0
        last_profile_time = time.time()
        PROFILE_INTERVAL = 300  # Log resource usage every 5 minutes

        try:
            while self.running:
                try:
                    loop_count += 1
                    current_time = datetime.now()

                    # Periodic resource profiling (every 5 minutes)
                    if time.time() - last_profile_time >= PROFILE_INTERVAL:
                        cpu_percent = process.cpu_percent(interval=0.1)
                        mem_info = process.memory_info()
                        mem_mb = mem_info.rss / 1024 / 1024
                        uptime = time.time() - start_time.timestamp()
                        logger.info(f"[Resource Profile] Uptime={uptime / 3600:.1f}h, CPU={cpu_percent:.2f}%, Memory={mem_mb:.2f}MB, Cycles={loop_count}")
                        last_profile_time = time.time()

                    # Check for due tasks
                    due_tasks = self.scheduler.check_due_tasks(current_time)

                    if due_tasks:
                        logger.info(f"[Cycle #{loop_count}] Found {len(due_tasks)} due task(s)")

                        # Execute each due task
                        for task in due_tasks:
                            # Validate task file exists before execution
                            from pathlib import Path

                            task_path = Path(task.task_file_path).expanduser()
                            if not task_path.exists() or not task_path.is_file():
                                logger.error(f"[Task {task.id}] ✗ Task file missing or invalid: {task.task_file_path}")
                                # Mark as failure and update
                                execution_time = datetime.now()
                                self.scheduler.update_task_after_execution(task.id, execution_time, success=False)
                                continue

                            logger.info(f"[Task {task.id}] Starting execution: {task.task_file_path}")
                            task_start = time.time()
                            try:
                                return_code, log_path = self.executor.execute_task(task)
                                task_duration = time.time() - task_start
                                success = return_code == 0

                                if success:
                                    logger.info(f"[Task {task.id}] ✓ Completed successfully (duration: {task_duration:.2f}s, log: {log_path})")
                                else:
                                    logger.warning(f"[Task {task.id}] ✗ Failed with return code {return_code} (duration: {task_duration:.2f}s, log: {log_path})")

                                # Update task timestamps with success/failure status
                                execution_time = datetime.now()
                                self.scheduler.update_task_after_execution(task.id, execution_time, success=success)
                            except Exception as e:
                                task_duration = time.time() - task_start
                                logger.error(
                                    f"[Task {task.id}] ✗ Exception during execution (duration: {task_duration:.2f}s): {e}",
                                    exc_info=True,
                                )
                                # Mark as failure
                                execution_time = datetime.now()
                                self.scheduler.update_task_after_execution(task.id, execution_time, success=False)

                    # Optimize sleep interval: sleep until next task (max 1 hour for responsiveness)
                    next_task_time = self.scheduler.get_next_task_time(current_time)
                    if next_task_time is not None:
                        # Calculate time until next task
                        current_timestamp = current_time.timestamp()
                        sleep_seconds = max(1, min(3600, next_task_time - current_timestamp))

                        if sleep_seconds > 60:
                            # Only log if sleeping for more than 1 minute
                            next_task_dt = datetime.fromtimestamp(next_task_time)
                            logger.debug(f"[Cycle #{loop_count}] Next task at {next_task_dt.strftime('%Y-%m-%d %H:%M:%S')}, sleeping {sleep_seconds:.0f}s")
                        else:
                            logger.debug(f"[Cycle #{loop_count}] Next task in {sleep_seconds:.0f}s")
                    else:
                        # No tasks scheduled, sleep for 1 hour
                        sleep_seconds = 3600
                        logger.debug(f"[Cycle #{loop_count}] No tasks scheduled, sleeping 1 hour")

                    time.sleep(sleep_seconds)

                except KeyboardInterrupt:
                    raise  # Re-raise to be caught by outer handler
                except Exception as e:
                    logger.error(f"[Cycle #{loop_count}] Error in daemon loop: {e}", exc_info=True)
                    time.sleep(60)  # Sleep 1 minute after error before retrying

        except KeyboardInterrupt:
            logger.info("Daemon received interrupt signal, shutting down...")
        finally:
            # Log final resource usage before cleanup
            cpu_percent = process.cpu_percent(interval=0.1)
            mem_info = process.memory_info()
            mem_mb = mem_info.rss / 1024 / 1024
            uptime = time.time() - start_time.timestamp()
            logger.info(f"[Final Profile] Uptime={uptime / 3600:.1f}h, CPU={cpu_percent:.2f}%, Memory={mem_mb:.2f}MB, Cycles={loop_count}")
            self._cleanup()

    def _perform_crash_recovery(self) -> None:
        """Perform crash recovery checks and task validation."""
        logger.info("Performing crash recovery checks...")

        # Validate task files exist
        missing_files = self.scheduler.validate_task_files()
        if missing_files:
            logger.warning(f"Found {len(missing_files)} task(s) with missing files:")
            for task_id, file_path in missing_files:
                logger.warning(f"  - Task {task_id}: {file_path}")
            logger.warning("Tasks with missing files will fail when due for execution")

        # Recalculate next run times for all tasks (in case of clock skew after crash)
        config = self.config_manager.load()
        tasks_updated = 0
        current_time = datetime.now()

        for task in config.tasks:
            if task.enabled and task.next_run is not None and task.next_run < current_time.timestamp():
                # If next_run is in the past, recalculate to avoid immediate execution burst
                old_next_run = task.next_run
                task.next_run = self.scheduler.get_next_run_time(task.cron_expression, current_time)
                tasks_updated += 1
                logger.info(f"Task {task.id} next_run was in the past (old: {datetime.fromtimestamp(old_next_run).isoformat()}), recalculated to: {datetime.fromtimestamp(task.next_run).isoformat()}")

        if tasks_updated > 0:
            self.config_manager.save(config)
            logger.info(f"Updated {tasks_updated} task(s) with recalculated next run times")

        logger.info("Crash recovery complete")

    def _setup_signal_handlers(self) -> None:
        """Set up signal handlers for graceful shutdown."""

        def signal_handler(signum: int, frame: object) -> None:
            """Handle shutdown signals."""
            logger.info(f"Received signal {signum}, shutting down...")
            self.running = False

        # Handle common termination signals
        if sys.platform != "win32":
            # Unix signals
            signal.signal(signal.SIGTERM, signal_handler)
            signal.signal(signal.SIGINT, signal_handler)
            signal.signal(signal.SIGHUP, signal_handler)
        else:
            # Windows signals (limited support)
            signal.signal(signal.SIGINT, signal_handler)
            signal.signal(signal.SIGBREAK, signal_handler)

    def _cleanup(self) -> None:
        """Clean up daemon resources on shutdown."""
        logger.info("Cleaning up daemon resources...")

        # Remove PID file and start time file
        self.pid_file.unlink(missing_ok=True)
        self.start_time_file.unlink(missing_ok=True)
        logger.info("Daemon shutdown complete")

    def get_uptime(self) -> float | None:
        """
        Get daemon uptime in seconds.

        Returns:
            Uptime in seconds, or None if daemon is not running
        """
        start_time = self.get_start_time()
        if start_time is None:
            return None

        current_time = datetime.now(timezone.utc)
        uptime_delta = current_time - start_time
        return uptime_delta.total_seconds()

    def get_start_time(self) -> datetime | None:
        """
        Get the daemon start time.

        Returns:
            Start time as timezone-aware datetime, or None if daemon is not running
        """
        if not self.start_time_file.exists():
            return None

        try:
            start_time_text = self.start_time_file.read_text(encoding="utf-8").strip()
            return datetime.fromisoformat(start_time_text)
        except (ValueError, OSError) as e:
            logger.error(f"Failed to read start time file: {e}")
            return None

    def get_pid(self) -> int | None:
        """
        Get the PID of the running daemon.

        Returns:
            PID as integer, or None if daemon is not running
        """
        return self._read_pid()

    def get_resource_usage(self) -> tuple[float, float] | None:
        """
        Get current CPU and memory usage of the running daemon.

        Returns:
            Tuple of (cpu_percent, memory_mb), or None if daemon is not running
        """
        pid = self.get_pid()
        if pid is None or not self.is_running():
            return None

        try:
            process = psutil.Process(pid)
            cpu_percent = process.cpu_percent(interval=0.1)
            mem_info = process.memory_info()
            mem_mb = mem_info.rss / 1024 / 1024
            return (cpu_percent, mem_mb)
        except (psutil.NoSuchProcess, psutil.AccessDenied, psutil.ZombieProcess) as e:
            logger.error(f"Failed to get resource usage for PID {pid}: {e}")
            return None

    def _read_pid(self) -> int | None:
        """
        Read PID from PID file.

        Returns:
            PID as integer, or None if file doesn't exist or is invalid
        """
        if not self.pid_file.exists():
            return None

        try:
            pid_text = self.pid_file.read_text(encoding="utf-8").strip()
            return int(pid_text)
        except (ValueError, OSError) as e:
            logger.error(f"Failed to read PID file: {e}")
            return None

    def _is_process_running(self, pid: int) -> bool:
        """
        Check if a process with given PID is running.

        Args:
            pid: Process ID to check

        Returns:
            True if process is running, False otherwise
        """
        try:
            if sys.platform == "win32":
                # Windows: Use tasklist
                result = subprocess.run(
                    ["tasklist", "/FI", f"PID eq {pid}"],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                return str(pid) in result.stdout
            else:
                # Unix: Send signal 0 (no-op that checks if process exists)
                os.kill(pid, 0)
                return True
        except (ProcessLookupError, PermissionError, OSError):
            return False


# CLI entry point for running daemon loop (deprecated - use __main__.py instead)
if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "run":
        daemon = CronDaemon()
        daemon.run_loop()
    else:
        print("Usage: python -m clud.cron run")
        sys.exit(1)
