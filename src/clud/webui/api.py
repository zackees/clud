"""API handlers for Claude Code Web UI."""

import asyncio
import logging
import os
import shutil
import subprocess
from collections.abc import AsyncGenerator
from pathlib import Path

from running_process import RunningProcess

logger = logging.getLogger(__name__)


class ChatHandler:
    """Handle chat requests and stream responses from Claude Code."""

    def __init__(self) -> None:
        """Initialize chat handler."""
        self.claude_path = self._find_claude_path()
        if not self.claude_path:
            logger.warning("Claude Code executable not found in PATH")

    def _find_claude_path(self) -> str | None:
        """Find the path to the Claude executable."""
        import platform

        # Try to find claude in PATH
        if platform.system() == "Windows":
            # On Windows, prefer .cmd and .exe extensions
            claude_path = shutil.which("claude.cmd") or shutil.which("claude.exe")
            if claude_path:
                return claude_path

        # Fall back to generic "claude" (for Unix or git bash on Windows)
        claude_path = shutil.which("claude")
        if claude_path:
            return claude_path

        # Check common Windows npm global locations
        if platform.system() == "Windows":
            possible_paths = [
                os.path.expanduser("~/AppData/Roaming/npm/claude.cmd"),
                os.path.expanduser("~/AppData/Roaming/npm/claude.exe"),
                "C:/Users/" + os.environ.get("USERNAME", "") + "/AppData/Roaming/npm/claude.cmd",
            ]
            for path in possible_paths:
                if os.path.exists(path):
                    return path

        return None

    async def handle_chat(self, message: str, project_path: str) -> AsyncGenerator[str]:
        """Handle a chat message and stream the response.

        Args:
            message: User message to send to Claude
            project_path: Working directory for Claude Code

        Yields:
            Chunks of text from Claude's response
        """
        if not self.claude_path:
            yield "Error: Claude Code is not installed or not in PATH\n"
            yield "Install Claude Code from: https://claude.ai/download\n"
            return

        # Build command with dangerous permissions flag (YOLO mode)
        cmd = [
            self.claude_path,
            "--dangerously-skip-permissions",
            "-p",
            message,
            "--output-format",
            "stream-json",
            "--verbose",
        ]

        logger.info("Executing Claude Code: %s", " ".join(cmd))
        logger.info("Working directory: %s", project_path)

        # Change to project directory
        original_cwd = os.getcwd()
        try:
            os.chdir(project_path)

            # Create a queue for communication between threads
            queue: asyncio.Queue[str | None] = asyncio.Queue()

            # Capture the event loop from the async context to use in thread callbacks
            loop = asyncio.get_running_loop()

            def stdout_callback(line: str) -> None:
                """Callback for stdout lines."""
                # Put line in queue for async processing
                asyncio.run_coroutine_threadsafe(queue.put(line), loop)

            def run_process() -> None:
                """Run process in thread."""
                try:
                    RunningProcess.run_streaming(cmd, stdout_callback=stdout_callback)
                finally:
                    # Signal completion with None
                    asyncio.run_coroutine_threadsafe(queue.put(None), loop)

            # Start process in background thread
            import threading

            thread = threading.Thread(target=run_process, daemon=True)
            thread.start()

            # Stream output from queue
            while True:
                line = await queue.get()
                if line is None:
                    # Process finished
                    break
                yield line.rstrip() + "\n"

        except subprocess.CalledProcessError as e:
            logger.error("Claude Code execution failed: %s", e)
            yield f"Error: Claude Code execution failed with exit code {e.returncode}\n"
        except Exception as e:
            logger.exception("Error executing Claude Code")
            yield f"Error: {e}\n"
        finally:
            os.chdir(original_cwd)


class ProjectHandler:
    """Handle project-related requests."""

    @staticmethod
    def list_projects(base_path: str | None = None) -> list[dict[str, str]]:
        """List available projects/directories.

        Args:
            base_path: Base directory to search from (default: current directory)

        Returns:
            List of project dictionaries with 'name' and 'path' keys
        """
        if base_path is None:
            base_path = os.getcwd()

        projects: list[dict[str, str]] = []

        # Add current directory
        projects.append({"name": os.path.basename(base_path) or base_path, "path": base_path})

        # Add subdirectories (one level deep)
        try:
            for entry in Path(base_path).iterdir():
                if entry.is_dir() and not entry.name.startswith("."):
                    projects.append({"name": entry.name, "path": str(entry.absolute())})
        except PermissionError:
            logger.warning("Permission denied accessing %s", base_path)

        return projects

    @staticmethod
    def validate_project_path(path: str) -> bool:
        """Validate that a project path exists and is accessible.

        Args:
            path: Project path to validate

        Returns:
            True if path is valid, False otherwise
        """
        try:
            return Path(path).exists() and Path(path).is_dir()
        except Exception:
            return False


class HistoryHandler:
    """Handle conversation history."""

    def __init__(self) -> None:
        """Initialize history handler."""
        self.history: list[dict[str, str]] = []

    def add_message(self, role: str, content: str) -> None:
        """Add a message to history.

        Args:
            role: Message role ('user' or 'assistant')
            content: Message content
        """
        self.history.append({"role": role, "content": content})

    def get_history(self) -> list[dict[str, str]]:
        """Get conversation history.

        Returns:
            List of message dictionaries
        """
        return self.history.copy()

    def clear_history(self) -> None:
        """Clear conversation history."""
        self.history.clear()
