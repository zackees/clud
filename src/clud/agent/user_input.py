"""
Interactive user prompts.

This module provides utilities for interacting with the user through
command-line prompts and file editors.
"""

import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path

from clud.util.process import launch_detached


def _prompt_for_loop_count() -> int:
    """Prompt user for loop count (default: 50)."""
    while True:
        try:
            sys.stdout.flush()
            response = input("Loop count [50]: ").strip()
            if not response:
                return 50  # Default to 50

            count = int(response)
            if count <= 0:
                print("Loop count must be greater than 0.")
                continue

            return count

        except ValueError:
            print("Invalid input. Please enter a valid number.")
            continue
        except (EOFError, KeyboardInterrupt):
            print("\nOperation cancelled.")
            sys.exit(2)


def _prompt_for_message() -> str:
    """Prompt user for agent message/prompt."""
    while True:
        try:
            sys.stdout.flush()
            response = input("Prompt for agent: ").strip()
            if not response:
                print("Prompt cannot be empty. Please try again.")
                continue

            return response

        except (EOFError, KeyboardInterrupt):
            print("\nOperation cancelled.")
            sys.exit(2)


def _open_file_in_editor(file_path: Path) -> None:
    """Open a file in the default text editor (cross-platform, non-blocking)."""
    try:
        system = platform.system()
        if system == "Darwin":  # macOS
            # Try Sublime Text first with --new-window, then fall back to system open
            if shutil.which("subl"):
                launch_detached(["subl", "--new-window", str(file_path)])
            else:
                # macOS 'open' command is already non-blocking
                subprocess.run(["open", str(file_path)], check=False)
        elif system == "Windows":
            # Try editors in order: Sublime Text, TextPad, Notepad
            sublime_paths = [
                Path("C:\\Program Files\\Sublime Text\\sublime_text.exe"),
                Path("C:\\Program Files\\Sublime Text 3\\sublime_text.exe"),
                Path(os.path.expanduser("~\\AppData\\Local\\Programs\\Sublime Text\\sublime_text.exe")),
            ]

            # Try Sublime Text with --new-window
            for sublime_path in sublime_paths:
                if sublime_path.exists():
                    launch_detached([str(sublime_path), "--new-window", str(file_path)])
                    return

            # Try 'subl' in PATH
            if shutil.which("subl"):
                launch_detached(["subl", "--new-window", str(file_path)])
                return

            # Try TextPad
            textpad_paths = [
                Path("C:\\Program Files\\TextPad 9\\TextPad.exe"),
                Path("C:\\Program Files\\TextPad 8\\TextPad.exe"),
                Path("C:\\Program Files (x86)\\TextPad 9\\TextPad.exe"),
                Path("C:\\Program Files (x86)\\TextPad 8\\TextPad.exe"),
            ]
            for textpad_path in textpad_paths:
                if textpad_path.exists():
                    launch_detached([str(textpad_path), str(file_path)])
                    return

            # Fall back to notepad (always available, non-blocking via detached process)
            launch_detached(["notepad.exe", str(file_path)])

        else:  # Linux and other Unix-like systems
            # Try editors in order with detached process
            editors = ["subl", "sublime_text", "gedit", "kate", "nano"]
            for editor in editors:
                if shutil.which(editor):
                    if editor in ["subl", "sublime_text"]:
                        launch_detached([editor, "--new-window", str(file_path)])
                    else:
                        launch_detached([editor, str(file_path)])
                    return

            # Final fallback: xdg-open (delegates to system default)
            if shutil.which("xdg-open"):
                # xdg-open is typically non-blocking, but use launch_detached to be safe
                launch_detached(["xdg-open", str(file_path)])

    except Exception as e:
        print(f"Warning: Could not open {file_path}: {e}", file=sys.stderr)
