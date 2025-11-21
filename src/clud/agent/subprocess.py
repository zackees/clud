"""
Subprocess execution utilities.

This module provides utilities for executing external commands and
clud subprocesses.
"""

import subprocess
import sys


def run_clud_subprocess(
    prompt: str,
    use_print_flag: bool = False,
    additional_args: list[str] | None = None,
) -> int:
    """Run clud as a subprocess with the given prompt.

    Args:
        prompt: The prompt/message to pass to clud
        use_print_flag: If True, uses -p flag; if False, uses -m flag
        additional_args: Optional additional command-line arguments

    Returns:
        Exit code from clud subprocess
    """
    try:
        cmd = [sys.executable, "-m", "clud"]

        # Add prompt with appropriate flag
        flag = "-p" if use_print_flag else "-m"
        cmd.extend([flag, prompt])

        # Add any additional arguments
        if additional_args:
            cmd.extend(additional_args)

        result = subprocess.run(
            cmd,
            check=False,  # Don't raise on non-zero exit
            capture_output=False,  # Let output go to terminal
        )
        return result.returncode
    except FileNotFoundError:
        print("Error: Python interpreter not found.", file=sys.stderr)
        return 1
    except Exception as e:
        print(f"Error running clud subprocess: {e}", file=sys.stderr)
        return 1


def _execute_command(cmd: list[str], use_shell: bool = False, verbose: bool = False) -> int:
    """Execute a command and return its exit code."""
    if use_shell:
        # Convert command list to shell string and execute through shell
        cmd_str = subprocess.list2cmdline(cmd)
        if verbose:
            print(f"DEBUG: Retrying with shell=True: {cmd_str}", file=sys.stderr)
        result = subprocess.run(cmd_str, shell=True)
    else:
        result = subprocess.run(cmd)

    return result.returncode
