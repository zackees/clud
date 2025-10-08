"""Task management functionality for clud -t option.

This module provides functionality for the -t/--task command line option that allows
users to process task files with automatic editor integration and lint checking.
"""

import platform
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


def _exec(cmd: list[str], **kwargs: Any) -> subprocess.CompletedProcess[Any]:
    """Execute a subprocess command.

    Args:
        cmd: Command and arguments to execute.
        **kwargs: Additional arguments to pass to subprocess.run.

    Returns:
        CompletedProcess instance with result.
    """
    return subprocess.run(cmd, **kwargs)  # type: ignore[misc]


def find_editor() -> str | None:
    """Find available text editor based on platform."""
    system = platform.system()

    if system == "Windows":
        return _find_windows_editor()
    elif system == "Darwin":
        return _find_macos_editor()
    else:
        return _find_linux_editor()


def _find_windows_editor() -> str:
    """Find available editor on Windows.

    Returns:
        Path to editor executable, defaults to notepad.exe.
    """
    # Check for sublime text first
    sublime_paths = [
        r"C:\Program Files\Sublime Text\sublime_text.exe",
        r"C:\Program Files\Sublime Text 3\sublime_text.exe",
        r"C:\Program Files\Sublime Text 4\sublime_text.exe",
        r"C:\Program Files (x86)\Sublime Text\sublime_text.exe",
        r"C:\Program Files (x86)\Sublime Text 3\sublime_text.exe",
        r"C:\Program Files (x86)\Sublime Text 4\sublime_text.exe",
    ]
    for path in sublime_paths:
        if Path(path).exists():
            return path

    # Check if sublime is in PATH
    sublime = shutil.which("subl") or shutil.which("sublime_text")
    if sublime:
        return sublime

    # Fallback to notepad
    return "notepad.exe"


def _find_macos_editor() -> str | None:
    """Find available editor on macOS.

    Returns:
        Path to editor executable or None if none found.
    """
    editors = ["subl", "sublime", "code", "nano", "vim", "vi"]
    for editor in editors:
        if shutil.which(editor):
            return editor
    return None


def _find_linux_editor() -> str | None:
    """Find available editor on Linux/Unix.

    Returns:
        Path to editor executable or None if none found.
    """
    editors = ["nano", "pico", "vim", "vi", "emacs"]
    for editor in editors:
        if shutil.which(editor):
            return editor
    return None


def open_in_editor(file_path: Path) -> bool:
    """Open a file in the system's text editor.

    Args:
        file_path: Path to the file to open.

    Returns:
        True if editor was launched successfully, False otherwise.
    """
    editor = find_editor()
    if not editor:
        print("Error: No suitable text editor found on this system.", file=sys.stderr)
        return False

    try:
        print(f"Opening {file_path} in editor ({editor})...")
        cmd = _build_editor_command(editor, file_path)
        _exec(cmd, check=False, shell=platform.system() == "Windows" and cmd[0] == "start")
        return True
    except Exception as e:
        print(f"Error opening editor: {e}", file=sys.stderr)
        return False


def _build_editor_command(editor: str, file_path: Path) -> list[str]:
    """Build command to launch editor.

    Args:
        editor: Path to editor executable.
        file_path: Path to file to open.

    Returns:
        Command list for subprocess.
    """
    system = platform.system()

    if system == "Windows" and "notepad" not in editor.lower():
        # For GUI editors on Windows, use start to detach
        return ["start", "", editor, str(file_path)]
    elif system == "Darwin" and editor in ["subl", "sublime", "code"]:
        # For GUI editors on macOS
        return [editor, str(file_path)]
    else:
        # For terminal editors or notepad, run in foreground
        return [editor, str(file_path)]


def run_lint() -> bool:
    """Run lint command if available.

    Returns:
        True if lint passes or no lint script exists, False if lint fails.
    """
    if not _lint_script_exists():
        return True

    try:
        print("Running lint...")
        result = _exec(["bash", "lint"], capture_output=True, text=True, timeout=300)
        return _handle_lint_result(result)
    except subprocess.TimeoutExpired:
        print("Lint timed out after 5 minutes.", file=sys.stderr)
        return False
    except Exception as e:
        print(f"Error running lint: {e}", file=sys.stderr)
        return False


def _lint_script_exists() -> bool:
    """Check if lint script exists.

    Returns:
        True if lint script is found.
    """
    return Path("lint").exists() or Path("./lint").exists()


def _handle_lint_result(result: subprocess.CompletedProcess[str]) -> bool:
    """Handle lint command result.

    Args:
        result: Completed process from lint command.

    Returns:
        True if lint passed, False otherwise.
    """
    if result.returncode == 0:
        print("Lint passed successfully.")
        return True
    else:
        print("Lint failed with errors:")
        if result.stdout:
            print(result.stdout)
        if result.stderr:
            print(result.stderr, file=sys.stderr)
        return False


def fix_lint_errors() -> bool:
    """Try to fix lint errors iteratively.

    Attempts to run lint multiple times to allow for manual fixes.

    Returns:
        True if all lint errors are resolved, False otherwise.
    """
    max_iterations = 10

    for i in range(max_iterations):
        print(f"Lint iteration {i + 1}/{max_iterations}...")

        if run_lint():
            print("All lint errors fixed!")
            return True

        print(f"Lint still has errors after iteration {i + 1}")
        time.sleep(1)  # Give user a chance to fix manually

    print(f"Unable to fix all lint errors after {max_iterations} iterations.", file=sys.stderr)
    return False


def process_task_file(task_path: Path) -> int:
    """Process a task file according to PATH_HAS_TASK or PATH_EMPTY_TASK workflow.

    Args:
        task_path: Path to the task file to process.

    Returns:
        Exit code: 0 for success, non-zero for failure.
    """
    if _task_file_has_content(task_path):
        print(f"Processing existing task file: {task_path}")
        return process_existing_task(task_path)
    else:
        print(f"Creating new task file: {task_path}")
        return process_new_task(task_path)


def _task_file_has_content(task_path: Path) -> bool:
    """Check if task file exists and has content.

    Args:
        task_path: Path to check.

    Returns:
        True if file exists and has content.
    """
    return task_path.exists() and task_path.stat().st_size > 0


def _build_task_execution_prompt(task_path: Path) -> str:
    """Build prompt for autonomous task execution.

    Args:
        task_path: Path to task file.

    Returns:
        Prompt string for clud execution.
    """
    return (
        f"implement {task_path}, the prompt will be research-plan-implement-test-fix-lint-fix-update_task "
        "and continue until you are done, run into something you can't figure out and need user developer help on, "
        "or 50 iterations pass. When you halt, give a final summary of the current state and whether you finished "
        "because of SUCCESS! ALL DONE! or because NEED FEEDBACK: XXX or TASK NOT DONE AFTER 50 ITERATIONS: Reason"
    )


def _execute_task_with_clud(prompt: str) -> int:
    """Execute task by invoking clud with a prompt.

    Args:
        prompt: Task execution prompt.

    Returns:
        Exit code from clud execution.
    """
    try:
        result = subprocess.run(
            [sys.executable, "-m", "clud", "-m", prompt],
            check=False,
            capture_output=False,
        )
        return result.returncode
    except FileNotFoundError:
        print("Error: Python interpreter not found.", file=sys.stderr)
        return 1
    except Exception as e:
        print(f"Error running clud: {e}", file=sys.stderr)
        return 1


def process_existing_task(task_path: Path) -> int:
    """Process an existing task file (PATH_HAS_TASK workflow).

    Args:
        task_path: Path to the existing task file.

    Returns:
        Exit code: 0 for success, 1 for failure.
    """
    try:
        content = task_path.read_text(encoding="utf-8")
        if not content.strip():
            print("Task file is empty, switching to new task workflow...")
            return process_new_task(task_path)

        print(f"Processing task file: {task_path}")

        # Open the task in editor for user review/editing
        if not open_in_editor(task_path):
            print("Warning: Could not open editor, continuing anyway...")

        # Wait for user to finish editing
        _wait_for_user_edit()

        # Build prompt to execute the task autonomously
        task_prompt = _build_task_execution_prompt(task_path)

        # Execute the task using clud
        return _execute_task_with_clud(task_prompt)

    except Exception as e:
        print(f"Error processing existing task: {e}", file=sys.stderr)
        return 1


def _read_task_content(task_path: Path) -> str:
    """Read task file content.

    Args:
        task_path: Path to task file.

    Returns:
        File content as string.
    """
    print(f"Reading task from {task_path}...")
    return task_path.read_text(encoding="utf-8")


def _display_task_content(content: str) -> None:
    """Display task content to user.

    Args:
        content: Task file content to display.
    """
    print("Current task content:")
    print("-" * 40)
    print(content)
    print("-" * 40)


def _wait_for_user_edit() -> None:
    """Wait for user to complete editing."""
    print("\nTask file opened in editor. Make any necessary changes.")
    print("The task processor will continue after you save and close the editor.")
    print("Press Enter to continue after editing...")
    input()


def _has_blocking_problem(content: str) -> bool:
    """Check if content contains blocking problem indicators.

    Args:
        content: Content to check.

    Returns:
        True if blocking problem is detected.
    """
    upper_content = content.upper()
    return "BLOCKING PROBLEM" in upper_content or "CRITICAL DECISION" in upper_content


def _display_blocking_problem_warning() -> None:
    """Display warning about blocking problem."""
    print("\n" + "=" * 60)
    print("BLOCKING PROBLEM or CRITICAL DECISION detected!")
    print("Please resolve the issue before continuing.")
    print("=" * 60)


def _run_lint_check() -> None:
    """Run lint checking with warning if issues remain."""
    print("\nChecking for lint script...")
    if not fix_lint_errors():
        print("Warning: Lint errors remain. Please fix them manually.")


def _display_completion_message() -> None:
    """Display task completion message."""
    print("\nTask processing completed.")
    print("Please review the changes and continue with implementation as needed.")


def process_new_task(task_path: Path) -> int:
    """Process a new/empty task file (PATH_EMPTY_TASK workflow).

    Args:
        task_path: Path where new task file should be created.

    Returns:
        Exit code: 0 for success, 1 for failure, 2 for cancellation.
    """
    try:
        task_path.parent.mkdir(parents=True, exist_ok=True)

        user_input = _prompt_for_task_description()
        if not user_input:
            print("No task description provided. Exiting.", file=sys.stderr)
            return 1

        initial_content = _create_initial_task_content(user_input)
        task_path.write_text(initial_content, encoding="utf-8")
        print(f"\nInitial task written to {task_path}")
        print("Starting autonomous task processing...\n")

        # Now process the task autonomously
        return process_existing_task(task_path)

    except KeyboardInterrupt:
        print("\nTask creation cancelled.", file=sys.stderr)
        return 2
    except Exception as e:
        print(f"Error creating new task: {e}", file=sys.stderr)
        return 1


def _prompt_for_task_description() -> str:
    """Prompt user for task description.

    Returns:
        User input as string.
    """
    print("\nNo task file found or task file is empty.")
    print("Please describe the issue or task you want to work on:")
    print("(Press Ctrl+D or Ctrl+Z when done)")
    print("-" * 40)

    lines: list[str] = []
    try:
        while True:
            line = input()
            lines.append(line)
    except EOFError:
        pass

    return "\n".join(lines).strip()


def _create_initial_task_content(user_input: str) -> str:
    """Create initial task file content.

    Args:
        user_input: User's task description.

    Returns:
        Formatted task content.
    """
    return f"""# Task Description

## Initial Request
{user_input}

## Task Details
(To be filled in after analysis)

## Implementation Plan
(To be determined)

## Open Questions
- What specific requirements need clarification?
- What are the technical constraints?
- What is the expected outcome?

## Status
- [ ] Requirements gathered
- [ ] Plan created
- [ ] Implementation started
- [ ] Testing completed
- [ ] Documentation updated
"""


def _wait_for_task_enhancement() -> None:
    """Wait for user to enhance task description."""
    print("\nPlease enhance the task description with more details.")
    print("Press Enter to continue after editing...")
    input()


def _display_next_steps() -> None:
    """Display information about next steps."""
    print("\nNext step: Run task enhancement using Claude.")
    print('This would normally invoke: clud -p "enhance task.md ..."')
    print("For now, please manually enhance the task file.")


def _prompt_to_create_task_file(task_path: Path) -> bool:
    """Prompt user to create a new task file.

    Args:
        task_path: Path to the task file that doesn't exist.

    Returns:
        True if user wants to create the file, False otherwise.
    """
    print(f"{task_path.name} doesn't exist, create it? [y]/n: ", end="", flush=True)
    try:
        response = input().strip().lower()
        # Empty response or 'y' means yes (default to yes)
        return response == "" or response == "y"
    except (EOFError, KeyboardInterrupt):
        print()  # New line after interrupt
        return False


def handle_task_command(task_path_str: str) -> int:
    """Main entry point for -t/--task command.

    Args:
        task_path_str: String path to task file.

    Returns:
        Exit code: 0 for success, 2 for invalid input, 1 for other errors.
    """
    if not task_path_str:
        print("Error: -t/--task requires a file path", file=sys.stderr)
        return 2

    try:
        task_path = Path(task_path_str).resolve()

        # Check if file exists before processing
        if not task_path.exists() and not _prompt_to_create_task_file(task_path):
            print("Task creation cancelled.", file=sys.stderr)
            return 0

        return process_task_file(task_path)
    except Exception as e:
        print(f"Error handling task command: {e}", file=sys.stderr)
        return 1
