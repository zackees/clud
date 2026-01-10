"""CLI handler for clud --cron commands with user-friendly output."""

import sys
import threading
import time
from pathlib import Path

from croniter import croniter

from clud.cron import Daemon

from .autostart import AutostartInstaller
from .monitor import CronMonitor
from .scheduler import CronScheduler


def is_cron_initialized() -> bool:
    """Check if cron has been initialized (user has confirmed installation).

    Returns:
        True if cron has been initialized, False otherwise
    """
    marker_file = Path.home() / ".clud" / ".cron_initialized"
    return marker_file.exists()


def mark_cron_initialized() -> None:
    """Mark cron as initialized by creating marker file."""
    marker_file = Path.home() / ".clud" / ".cron_initialized"
    marker_file.parent.mkdir(parents=True, exist_ok=True)
    marker_file.touch()


def prompt_cron_installation() -> bool:
    """Prompt user to confirm cron installation.

    Returns:
        True if user confirmed (or default yes), False if user declined
    """
    print()
    print(f"{Colors.CYAN}ℹ{Colors.RESET} {Colors.BOLD}First-time cron setup{Colors.RESET}")
    print()
    print("Using cron will install a service manager that runs in the background")
    print("to execute scheduled tasks. The daemon requires minimal resources and")
    print("can be stopped at any time with 'clud --cron stop'.")
    print()

    try:
        response = input(f"Proceed with installation? [{Colors.GREEN}Y{Colors.RESET}/n]: ").strip().lower()
        print()  # Add newline after input

        # Default is yes (empty input or 'y')
        if response == "" or response == "y" or response == "yes":
            mark_cron_initialized()
            print_success("Cron service manager initialized")
            print()
            return True
        else:
            print_warning("Cron installation cancelled")
            print_info("You can enable cron later by running any 'clud --cron' command")
            print()
            return False
    except (KeyboardInterrupt, EOFError):
        print()
        print_warning("Cron installation cancelled")
        print()
        return False


# ANSI color codes for terminal output
class Colors:
    """ANSI color codes for terminal output."""

    GREEN = "\033[92m"
    RED = "\033[91m"
    YELLOW = "\033[93m"
    BLUE = "\033[94m"
    CYAN = "\033[96m"
    BOLD = "\033[1m"
    RESET = "\033[0m"
    DIM = "\033[2m"


class Spinner:
    """Simple spinner for long operations."""

    def __init__(self, message: str) -> None:
        """Initialize spinner with message.

        Args:
            message: Message to display alongside spinner
        """
        self.message = message
        self.spinning = False
        self.thread: threading.Thread | None = None
        self.frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
        self.frame_idx = 0

    def start(self) -> None:
        """Start the spinner animation."""
        self.spinning = True
        self.thread = threading.Thread(target=self._spin)
        self.thread.daemon = True
        self.thread.start()

    def _spin(self) -> None:
        """Internal spinner animation loop."""
        while self.spinning:
            frame = self.frames[self.frame_idx % len(self.frames)]
            sys.stdout.write(f"\r{Colors.BLUE}{frame}{Colors.RESET} {self.message}")
            sys.stdout.flush()
            self.frame_idx += 1
            time.sleep(0.1)

    def stop(self, success: bool = True, final_message: str | None = None) -> None:
        """Stop the spinner animation.

        Args:
            success: Whether operation succeeded (affects icon)
            final_message: Optional final message to display
        """
        self.spinning = False
        if self.thread:
            self.thread.join(timeout=0.5)

        # Clear the line and show final status
        sys.stdout.write("\r" + " " * (len(self.message) + 10) + "\r")
        if final_message:
            if success:
                print(f"{Colors.GREEN}✓{Colors.RESET} {final_message}")
            else:
                print(f"{Colors.RED}✗{Colors.RESET} {final_message}")
        sys.stdout.flush()


def print_success(message: str) -> None:
    """Print success message in green."""
    print(f"{Colors.GREEN}✓{Colors.RESET} {message}")


def print_error(message: str) -> None:
    """Print error message in red."""
    print(f"{Colors.RED}✗{Colors.RESET} {message}", file=sys.stderr)


def print_warning(message: str) -> None:
    """Print warning message in yellow."""
    print(f"{Colors.YELLOW}⚠{Colors.RESET} {message}")


def print_info(message: str) -> None:
    """Print info message in blue."""
    print(f"{Colors.BLUE}ℹ{Colors.RESET} {message}")


def print_header(message: str) -> None:
    """Print header message in bold."""
    print(f"\n{Colors.BOLD}{message}{Colors.RESET}")


def format_next_run(seconds_until_next: float) -> str:
    """Format next run time as human-readable relative time.

    Args:
        seconds_until_next: Seconds until next run

    Returns:
        Formatted string like "in 2 hours" or "in 5 minutes"
    """
    if seconds_until_next < 60:
        return f"in {int(seconds_until_next)} seconds"
    elif seconds_until_next < 3600:
        minutes = int(seconds_until_next / 60)
        return f"in {minutes} minute{'s' if minutes != 1 else ''}"
    elif seconds_until_next < 86400:
        hours = int(seconds_until_next / 3600)
        return f"in {hours} hour{'s' if hours != 1 else ''}"
    else:
        days = int(seconds_until_next / 86400)
        return f"in {days} day{'s' if days != 1 else ''}"


def print_cron_syntax_help() -> None:
    """Print cron expression syntax reference."""
    print("\n" + Colors.BOLD + "Cron Expression Syntax:" + Colors.RESET)
    print("""
  ┌─────── minute (0-59)
  │ ┌────── hour (0-23)
  │ │ ┌───── day of month (1-31)
  │ │ │ ┌──── month (1-12)
  │ │ │ │ ┌─── day of week (0-6, 0=Sunday)
  │ │ │ │ │
  * * * * *

Common expressions:
  "0 9 * * *"      → Every day at 9:00 AM
  "*/5 * * * *"    → Every 5 minutes
  "0 */2 * * *"    → Every 2 hours
  "0 0 * * 0"      → Every Sunday at midnight
  "0 0 1 * *"      → First day of every month at midnight
  "30 14 * * 1-5"  → 2:30 PM on weekdays
""")


def print_help() -> None:
    """Print comprehensive help text for --cron command."""
    print(Colors.BOLD + "clud --cron" + Colors.RESET + " - Schedule recurring tasks\n")
    print("Automated task execution on recurring schedules using standard cron expressions.")
    print("Works on Linux, macOS, and Windows with zero configuration required.\n")

    print(Colors.BOLD + "Usage:" + Colors.RESET)
    print("  clud --cron <subcommand> [options]\n")

    print(Colors.BOLD + "Subcommands:" + Colors.RESET)
    print(f"  {Colors.CYAN}add{Colors.RESET} <cron_expr> <task_file>  Schedule new task")
    print(f"  {Colors.CYAN}list{Colors.RESET}                         Show all scheduled tasks")
    print(f"  {Colors.CYAN}remove{Colors.RESET} <task_id>            Delete scheduled task")
    print(f"  {Colors.CYAN}start{Colors.RESET}                        Start daemon process")
    print(f"  {Colors.CYAN}stop{Colors.RESET}                         Stop daemon process")
    print(f"  {Colors.CYAN}status{Colors.RESET}                       Show daemon and task status")
    print(f"  {Colors.CYAN}install{Colors.RESET}                      Enable autostart on boot")
    print(f"  {Colors.CYAN}help{Colors.RESET}                         Show this help message")

    print_cron_syntax_help()

    print(Colors.BOLD + "Examples:" + Colors.RESET)
    print(f"  {Colors.DIM}# Schedule a daily task at 9 AM{Colors.RESET}")
    print('  clud --cron add "0 9 * * *" daily-report.md\n')
    print(f"  {Colors.DIM}# Start the scheduler daemon{Colors.RESET}")
    print("  clud --cron start\n")
    print(f"  {Colors.DIM}# List all scheduled tasks{Colors.RESET}")
    print("  clud --cron list\n")
    print(f"  {Colors.DIM}# Check daemon and task status{Colors.RESET}")
    print("  clud --cron status\n")
    print(f"  {Colors.DIM}# Enable autostart on system boot{Colors.RESET}")
    print("  clud --cron install\n")
    print(f"  {Colors.DIM}# Remove a task{Colors.RESET}")
    print("  clud --cron remove task-abc-123\n")

    print(Colors.BOLD + "Key Features:" + Colors.RESET)
    print(f"  {Colors.GREEN}•{Colors.RESET} Automatic retry with exponential backoff (handles transient failures)")
    print(f"  {Colors.GREEN}•{Colors.RESET} Comprehensive logging to ~/.clud/logs/cron/")
    print(f"  {Colors.GREEN}•{Colors.RESET} Crash recovery (validates tasks, recalculates times on restart)")
    print(f"  {Colors.GREEN}•{Colors.RESET} Cross-platform daemon (Linux, macOS, Windows)")
    print(f"  {Colors.GREEN}•{Colors.RESET} Autostart on boot (systemd, launchd, Task Scheduler)")
    print(f"  {Colors.GREEN}•{Colors.RESET} No admin/root permissions required\n")

    print(Colors.BOLD + "Task Files:" + Colors.RESET)
    print("  Task files are markdown files with instructions for clud to execute.")
    print("  Example: 'Create backup of ~/projects to ~/backups/backup-YYYY-MM-DD.tar.gz'\n")
    print(f"  {Colors.DIM}See examples in: examples/cron/{Colors.RESET}\n")

    print(Colors.BOLD + "Logs and Monitoring:" + Colors.RESET)
    print("  Daemon log:  ~/.clud/logs/cron-daemon.log")
    print("  Task logs:   ~/.clud/logs/cron/{task-id}/{timestamp}.log")
    print("  Config file: ~/.clud/cron.json\n")

    print(Colors.BOLD + "Troubleshooting:" + Colors.RESET)
    print(f"  {Colors.YELLOW}•{Colors.RESET} Task not executing? Check 'clud --cron list' for status")
    print(f"  {Colors.YELLOW}•{Colors.RESET} Daemon won't start? Check ~/.clud/logs/cron-daemon.log")
    print(f"  {Colors.YELLOW}•{Colors.RESET} Invalid cron expression? Use https://crontab.guru/ to validate")
    print(f"  {Colors.YELLOW}•{Colors.RESET} Task failing? View logs: cat ~/.clud/logs/cron/task-*/latest.log\n")

    print(f"{Colors.DIM}For complete documentation, see CLAUDE.md in the project repository.{Colors.RESET}\n")


def handle_cron_add(cron_expr: str, task_file_str: str) -> int:
    """Handle 'clud --cron add' command.

    Args:
        cron_expr: Cron expression (e.g., "0 9 * * *")
        task_file_str: Path to task file

    Returns:
        Exit code (0 for success, non-zero for error)
    """
    # Auto-create config directory if missing
    config_dir = Path.home() / ".clud"
    config_dir.mkdir(parents=True, exist_ok=True)

    # Validate cron expression
    if not croniter.is_valid(cron_expr):
        print_error(f"Invalid cron expression: '{cron_expr}'")
        print_info("Use 'clud --cron help' to see syntax examples")
        return 1

    # Validate task file exists
    task_file = Path(task_file_str).resolve()
    if not task_file.exists():
        print_error(f"Task file not found: {task_file}")
        return 1

    if not task_file.is_file():
        print_error(f"Path is not a file: {task_file}")
        return 1

    # Add task to scheduler
    try:
        scheduler = CronScheduler()
        task = scheduler.add_task(cron_expr, str(task_file))
        print_success(f"Task '{task.id}' scheduled successfully")
        print_info(f"Schedule: {cron_expr}")
        print_info(f"Task file: {task_file}")

        # Show next run time
        from datetime import datetime

        if task.next_run:
            next_run_dt = datetime.fromtimestamp(task.next_run)
            seconds_until = task.next_run - datetime.now().timestamp()
            relative_time = format_next_run(seconds_until)
            print_info(f"Next run: {next_run_dt.strftime('%Y-%m-%d %H:%M:%S')} ({relative_time})")

        # Show helpful next steps
        print()
        if not Daemon.is_running():
            print_info("Start the daemon to begin executing tasks: clud --cron start")
        else:
            print_info("Daemon is running - task will execute on schedule")

        return 0
    except Exception as e:
        print_error(f"Failed to add task: {e}")
        return 1


def handle_cron_list() -> int:
    """Handle 'clud --cron list' command.

    Returns:
        Exit code (0 for success, non-zero for error)
    """
    try:
        scheduler = CronScheduler()
        tasks = scheduler.list_tasks()

        if not tasks:
            print_info("No scheduled tasks")
            print_info('Add a task with: clud --cron add "<cron_expr>" <task_file>')
            return 0

        # Print table header
        print_header(f"Scheduled Tasks ({len(tasks)} total)")
        print()
        print(f"{Colors.BOLD}{'ID':<8} {'Schedule':<15} {'Next Run':<20} {'Task File'}{Colors.RESET}")
        print("─" * 80)

        # Print each task
        for task in tasks:
            # Note: seconds_until_next_run() will be implemented in a future iteration
            next_run = "N/A (future)"  # Placeholder until seconds_until_next_run() is implemented
            # Truncate long file paths
            task_file = str(task.task_file_path)
            if len(task_file) > 40:
                task_file = "..." + task_file[-37:]

            status_color = Colors.GREEN if task.enabled else Colors.DIM
            print(f"{status_color}{task.id:<8}{Colors.RESET} {task.cron_expression:<15} {next_run:<20} {task_file}")

        print()
        print_info(f"Logs: {Path.home() / '.clud' / 'logs' / 'cron'}")
        return 0
    except Exception as e:
        print_error(f"Failed to list tasks: {e}")
        return 1


def handle_cron_remove(task_id: str) -> int:
    """Handle 'clud --cron remove' command.

    Args:
        task_id: Task ID to remove

    Returns:
        Exit code (0 for success, non-zero for error)
    """
    try:
        scheduler = CronScheduler()
        removed = scheduler.remove_task(task_id)

        if removed:
            print_success(f"Task '{task_id}' removed")
            return 0
        else:
            print_error(f"Task '{task_id}' not found")
            print_info("Use 'clud --cron list' to see all tasks")
            return 1
    except Exception as e:
        print_error(f"Failed to remove task: {e}")
        return 1


def handle_cron_start() -> int:
    """Handle 'clud --cron start' command.

    Returns:
        Exit code (0 for success, non-zero for error)
    """
    try:
        # Check if already running
        status = Daemon.status()
        if status.state == "running":
            print_warning("Daemon is already running")
            if status.pid:
                print_info(f"PID: {status.pid}")
            return 0

        # Start daemon with spinner
        spinner = Spinner("Starting daemon...")
        spinner.start()
        Daemon.start()

        # Give it a moment to fully start
        time.sleep(0.5)

        # Verify it started
        status = Daemon.status()
        if status.state == "running":
            spinner.stop(success=True, final_message=f"Daemon started successfully (PID: {status.pid})")
            print_info(f"Logs: {Path.home() / '.clud' / 'logs' / 'cron-daemon.log'}")
            return 0
        else:
            spinner.stop(success=False, final_message="Daemon failed to start")
            print_info(f"Check logs: {Path.home() / '.clud' / 'logs' / 'cron-daemon.log'}")
            return 1
    except Exception as e:
        print_error(f"Failed to start daemon: {e}")
        return 1


def handle_cron_stop() -> int:
    """Handle 'clud --cron stop' command.

    Returns:
        Exit code (0 for success, non-zero for error)
    """
    try:
        # Check if running
        if not Daemon.is_running():
            print_warning("Daemon is not running")
            return 0

        # Stop daemon
        print_info("Stopping daemon...")
        Daemon.stop()

        # Verify it stopped
        status = Daemon.status()
        if status.state != "running":
            print_success("Daemon stopped successfully")
            return 0
        else:
            print_error("Daemon failed to stop")
            print_info("You may need to manually kill the process")
            if status.pid:
                print_info(f"PID: {status.pid}")
            return 1
    except Exception as e:
        print_error(f"Failed to stop daemon: {e}")
        return 1


def handle_cron_status() -> int:
    """Handle 'clud --cron status' command.

    Returns:
        Exit code (0 for success, non-zero for error)
    """
    try:
        scheduler = CronScheduler()
        monitor = CronMonitor()

        # Daemon status with health check
        print_header("Daemon Status")
        health = monitor.check_daemon_health()

        if health["is_healthy"]:
            print(f"  Status: {Colors.GREEN}●{Colors.RESET} Running")
            if health["pid"]:
                print(f"  PID: {health['pid']}")
            if health["uptime_seconds"]:
                uptime_str = monitor._format_uptime(health["uptime_seconds"])
                print(f"  Uptime: {uptime_str}")
            if health["start_time"]:
                start_str = health["start_time"].strftime("%Y-%m-%d %H:%M:%S UTC")
                print(f"  Started: {start_str}")

            # Show resource usage if available
            resource_usage = health.get("resource_usage")
            if resource_usage:
                cpu_percent, mem_mb = resource_usage
                print(f"  CPU: {cpu_percent:.1f}%")
                print(f"  Memory: {mem_mb:.1f} MB")
        else:
            status_color = Colors.RED if health["status"] == "stopped" else Colors.YELLOW
            status_text = health["status"].capitalize()
            print(f"  Status: {status_color}●{Colors.RESET} {status_text}")
            if health["status"] == "stopped":
                print_info("Start with: clud --cron start")
            elif health["status"] == "stale":
                print_warning("Stale PID file detected - daemon process not running")
                print_info("Clean up with: clud --cron start")

        # Task summary
        tasks = scheduler.list_tasks()
        print_header(f"Task Summary ({len(tasks)} total)")
        if tasks:
            enabled_count = sum(1 for t in tasks if t.enabled)
            print(f"  Enabled: {enabled_count}")
            print(f"  Disabled: {len(tasks) - enabled_count}")

            # Show next upcoming task
            # Note: This will be properly implemented when seconds_until_next_run() is available
            if enabled_count > 0 and tasks:
                next_task = tasks[0]  # Placeholder - will use proper calculation later
                print(f"  Next run: N/A (future) ({next_task.id})")
        else:
            print_info("  No tasks scheduled")

        # Recent activity (last 60 minutes)
        recent_activity = monitor.get_recent_activity(minutes=60)
        if recent_activity:
            print_header("Recent Activity (last 60 minutes)")
            for activity in recent_activity[:5]:  # Show at most 5 recent events
                timestamp_str = activity["timestamp"].strftime("%H:%M:%S")
                activity_type = activity["type"]
                description = activity["description"]

                if activity_type == "daemon_start":
                    print(f"  {Colors.BLUE}[{timestamp_str}]{Colors.RESET} {description}")
                elif activity_type == "task_execution":
                    result_color = Colors.GREEN if activity["result"] == "success" else Colors.RED
                    result_symbol = "✓" if activity["result"] == "success" else "✗"
                    print(f"  {Colors.BLUE}[{timestamp_str}]{Colors.RESET} {result_color}{result_symbol}{Colors.RESET} {description}")

        # Autostart status
        print_header("Autostart Configuration")
        installer = AutostartInstaller()
        auto_status, auto_message, auto_method = installer.status()
        if auto_status == "installed":
            print(f"  Status: {Colors.GREEN}●{Colors.RESET} Enabled")
            print(f"  Method: {auto_method}")
            print(f"  {Colors.DIM}{auto_message}{Colors.RESET}")
        elif auto_status == "not_installed":
            print(f"  Status: {Colors.RED}●{Colors.RESET} Not configured")
            print_info("Enable with: clud --cron install")
        else:
            print(f"  Status: {Colors.YELLOW}?{Colors.RESET} Unknown")
            print(f"  {Colors.DIM}{auto_message}{Colors.RESET}")

        print()
        return 0
    except Exception as e:
        print_error(f"Failed to get status: {e}")
        return 1


def handle_cron_install() -> int:
    """Handle 'clud --cron install' command.

    Returns:
        Exit code (0 for success, non-zero for error)
    """
    try:
        installer = AutostartInstaller()

        # Check if already installed
        status, status_message, method = installer.status()
        if status == "installed":
            print_warning("Autostart is already configured")
            print(f"  Method: {method}")
            print(f"  {Colors.DIM}{status_message}{Colors.RESET}")
            print_info("To reinstall, manually remove the existing configuration first")
            return 0

        # Install autostart with spinner
        spinner = Spinner("Installing autostart configuration...")
        spinner.start()
        success, message, install_method = installer.install()
        spinner.stop(success=success, final_message="Autostart configuration installed" if success else "Failed to install autostart")

        if success:
            print(f"  Method: {install_method}")
            print(f"  {Colors.DIM}{message}{Colors.RESET}")
            print()
            print_info("The daemon will now start automatically on system boot")
            print_info("You can check status with: clud --cron status")

            # Suggest starting daemon now if not running
            if not Daemon.is_running():
                print()
                print_info("Daemon is not currently running")
                print_info("Start it now with: clud --cron start")

            return 0
        else:
            print_error("Failed to install autostart configuration")
            print(f"  {Colors.DIM}{message}{Colors.RESET}")
            print()
            print_warning("Troubleshooting:")
            print("  1. Check that you have sufficient permissions")
            print("  2. Verify that required system tools are available:")
            print("     - Linux: systemctl or crontab")
            print("     - macOS: launchctl or osascript")
            print("     - Windows: schtasks or registry access")
            print("  3. Check logs for more details")
            print()
            print_info("You can still manually start the daemon with: clud --cron start")
            return 1

    except Exception as e:
        print_error(f"Failed to install autostart: {e}")
        return 1


def handle_cron_command(subcommand: str | None, args: list[str]) -> int:
    """Handle all --cron subcommands.

    Args:
        subcommand: The cron subcommand (add, list, remove, etc.)
        args: Additional arguments for the subcommand

    Returns:
        Exit code (0 for success, non-zero for error)
    """
    # No subcommand or help requested - help doesn't require initialization
    if not subcommand or subcommand == "help" or subcommand == "--help" or subcommand == "-h":
        print_help()
        return 0

    # List of valid subcommands
    valid_subcommands = ["add", "list", "remove", "start", "stop", "status", "install"]

    # Check for unknown subcommands before initialization
    if subcommand not in valid_subcommands:
        print_error(f"Unknown subcommand: '{subcommand}'")
        print_info("Use 'clud --cron help' to see all available subcommands")
        return 1

    # Check if cron has been initialized (first-time setup prompt)
    if not is_cron_initialized() and not prompt_cron_installation():
        # User declined installation
        return 1

    # Route to appropriate handler
    if subcommand == "add":
        if len(args) < 2:
            print_error("Missing arguments for 'add' command")
            print_info('Usage: clud --cron add "<cron_expr>" <task_file>')
            print_info('Example: clud --cron add "0 9 * * *" daily-report.md')
            return 1
        cron_expr = args[0]
        task_file = args[1]
        return handle_cron_add(cron_expr, task_file)

    elif subcommand == "list":
        return handle_cron_list()

    elif subcommand == "remove":
        if len(args) < 1:
            print_error("Missing task ID for 'remove' command")
            print_info("Usage: clud --cron remove <task_id>")
            return 1
        task_id = args[0]
        return handle_cron_remove(task_id)

    elif subcommand == "start":
        return handle_cron_start()

    elif subcommand == "stop":
        return handle_cron_stop()

    elif subcommand == "status":
        return handle_cron_status()

    elif subcommand == "install":
        return handle_cron_install()

    # This should never be reached due to earlier validation
    return 1
