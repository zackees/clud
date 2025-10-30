"""Shell configuration for launching Claude Code with proper path handling.

This module provides a dataclass-based approach to handling shell selection
and path normalization. It ensures paths are only normalized at the last moment
before launch, based on the actual shell that will be used.
"""

import platform
import subprocess
from dataclasses import dataclass
from enum import Enum
from pathlib import Path


class ShellType(Enum):
    """Supported shell types."""

    CMD = "cmd"
    POWERSHELL = "powershell"
    GIT_BASH = "git-bash"


@dataclass
class ShellLaunchConfig:
    """Configuration for launching a command through a specific shell.

    Attributes:
        claude_path: Original path to Claude executable (not normalized)
        command_args: Additional command-line arguments
        preferred_shell: Preferred shell to use (will try to upgrade to git-bash)
        fallback_shell: Fallback shell if preferred shell fails
    """

    claude_path: str
    command_args: list[str]
    preferred_shell: ShellType = ShellType.CMD
    fallback_shell: ShellType = ShellType.CMD

    def normalize_path_for_shell(self, shell: ShellType) -> str:
        """Normalize the Claude path for the target shell.

        Args:
            shell: The shell type to normalize for

        Returns:
            Normalized path string appropriate for the shell
        """
        path_obj = Path(self.claude_path)

        if shell == ShellType.GIT_BASH:
            # Git-bash on Windows needs Unix-style paths: /c/Users/... not C:/Users/...
            if platform.system() == "Windows":
                # Try using cygpath (official conversion utility) if available
                import shutil

                cygpath = shutil.which("cygpath")
                if cygpath:
                    try:
                        result = subprocess.run(
                            [cygpath, "-u", str(path_obj)],
                            capture_output=True,
                            text=True,
                            timeout=1.0,
                            check=False,
                        )
                        if result.returncode == 0:
                            return result.stdout.strip()
                    except (subprocess.TimeoutExpired, FileNotFoundError):
                        pass  # Fall through to manual conversion

                # Fallback: Manual conversion
                # C:\Users\niteris\... -> /c/Users/niteris/...
                posix_path = path_obj.as_posix()

                # Check if path has a drive letter (e.g., C:/)
                if len(posix_path) >= 2 and posix_path[1] == ":":
                    drive_letter = posix_path[0].lower()
                    rest_of_path = posix_path[2:].lstrip("/")
                    return f"/{drive_letter}/{rest_of_path}"

                # No drive letter, return as-is
                return posix_path
            else:
                # On Unix, as_posix() is already correct
                return path_obj.as_posix()
        elif shell in (ShellType.CMD, ShellType.POWERSHELL):
            # Windows shells prefer backslashes
            return str(path_obj)
        else:
            # Unknown shell, return original
            return self.claude_path

    def can_launch_shell(self, shell_path: str) -> bool:
        """Test if a shell can be launched successfully.

        Args:
            shell_path: Path to the shell executable

        Returns:
            True if the shell can be launched, False otherwise
        """
        try:
            # Try to launch the shell with a simple command
            result = subprocess.run(
                [shell_path, "--version"],
                capture_output=True,
                timeout=2.0,
                check=False,
            )
            return result.returncode == 0
        except (OSError, subprocess.TimeoutExpired, FileNotFoundError):
            return False

    def determine_shell(self) -> tuple[ShellType, str | None]:
        """Determine which shell to use, with fallback logic.

        Returns:
            Tuple of (shell_type, shell_path)
            shell_path is None if no suitable shell found
        """
        from .util import detect_git_bash

        # On Windows, try to upgrade to git-bash if possible
        if platform.system() == "Windows":
            # Check if git-bash is available
            git_bash_path = detect_git_bash()
            if git_bash_path and self.can_launch_shell(git_bash_path):
                return ShellType.GIT_BASH, git_bash_path

            # Fallback to cmd (always available on Windows)
            return ShellType.CMD, None

        # On Unix-like systems, use default shell
        return ShellType.CMD, None

    def build_command(self) -> list[str]:
        """Build the final command with normalized path.

        Returns:
            Command list ready for subprocess.run()
        """
        # Determine which shell to use
        shell_type, shell_path = self.determine_shell()

        # Normalize path for the determined shell
        normalized_path = self.normalize_path_for_shell(shell_type)

        # Build command based on shell type
        if shell_type == ShellType.GIT_BASH and shell_path:
            # Wrap command for git-bash
            cmd_parts = [normalized_path] + self.command_args

            # Quote each argument for bash
            bash_cmd_parts: list[str] = []
            for arg in cmd_parts:
                # Escape single quotes
                arg_escaped = arg.replace("'", "'\\''")
                bash_cmd_parts.append(f"'{arg_escaped}'")

            cmd_str = " ".join(bash_cmd_parts)
            return [shell_path, "-c", cmd_str]
        else:
            # Direct execution (Windows cmd or Unix shell)
            return [normalized_path] + self.command_args
