"""API handlers for Claude Code Web UI."""

import asyncio
import logging
import os
import shutil
import subprocess
from collections.abc import AsyncGenerator
from pathlib import Path
from typing import Any

from running_process import RunningProcess

from ..views import DiffTreeView, DiffView

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

                # Parse JSON line and extract displayable content
                async for chunk in self._parse_json_line(line):
                    yield chunk

        except subprocess.CalledProcessError as e:
            logger.error("Claude Code execution failed: %s", e)
            yield f"Error: Claude Code execution failed with exit code {e.returncode}\n"
        except Exception as e:
            logger.exception("Error executing Claude Code")
            yield f"Error: {e}\n"
        finally:
            os.chdir(original_cwd)

    async def _parse_json_line(self, line: str) -> AsyncGenerator[str]:
        """Parse a JSON line from Claude Code and extract displayable content.

        Args:
            line: JSON line from Claude Code output

        Yields:
            Display text chunks
        """
        import json

        try:
            data = json.loads(line.strip())
            msg_type = data.get("type")

            if msg_type == "system":
                # System initialization - show basic info
                subtype = data.get("subtype")
                if subtype == "init":
                    model = data.get("model", "unknown")
                    yield f"[Using {model}]\n\n"

            elif msg_type == "assistant":
                # Assistant messages - extract text and tool uses
                message = data.get("message", {})
                content = message.get("content", [])

                for item in content:
                    item_type = item.get("type")

                    if item_type == "text":
                        # Plain text from Claude
                        text = item.get("text", "")
                        if text:
                            yield text

                    elif item_type == "tool_use":
                        # Tool use - show what tool is being called
                        tool_name = item.get("name", "unknown")
                        tool_input = item.get("input", {})

                        # Format tool use nicely
                        yield f"\n\n[Calling {tool_name}]\n"

                        # Show description if available (Bash commands)
                        if tool_name == "Bash" and "description" in tool_input:
                            yield f"  {tool_input['description']}\n"
                        elif tool_name == "Read" and "file_path" in tool_input:
                            yield f"  Reading: {tool_input['file_path']}\n"
                        elif tool_name == "Edit" and "file_path" in tool_input:
                            yield f"  Editing: {tool_input['file_path']}\n"
                        elif tool_name == "Write" and "file_path" in tool_input:
                            yield f"  Writing: {tool_input['file_path']}\n"

            elif msg_type == "result":
                # Final result - show summary
                subtype = data.get("subtype")
                result_text = data.get("result", "")

                if subtype == "success" and result_text:
                    yield f"\n\n---\n\n{result_text}\n"
                elif subtype == "error":
                    yield f"\n\n[Error: {result_text}]\n"

            # Ignore "user" type messages (tool results) - they're internal

        except json.JSONDecodeError:
            # Not JSON, pass through as-is (might be plain text error)
            logger.debug("Non-JSON line: %s", line[:100])
            yield line
        except Exception as e:
            logger.warning("Error parsing JSON line: %s", e)
            # Don't fail, just skip malformed lines


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


class DiffHandler:
    """Handle diff-related requests."""

    def __init__(self) -> None:
        """Initialize diff handler."""
        self.diff_trees: dict[str, DiffTreeView] = {}  # project_path -> DiffTreeView

    @staticmethod
    def _normalize_path(path: str) -> str:
        """Normalize a file path to use forward slashes for consistent keys.

        Args:
            path: File path to normalize

        Returns:
            Normalized path with forward slashes
        """
        return str(Path(path).resolve()).replace("\\", "/")

    def get_or_create_tree(self, project_path: str) -> DiffTreeView:
        """Get or create a diff tree for a project.

        Args:
            project_path: Path to project

        Returns:
            DiffTreeView instance
        """
        normalized_path = self._normalize_path(project_path)
        if normalized_path not in self.diff_trees:
            self.diff_trees[normalized_path] = DiffTreeView(project_path)
        return self.diff_trees[normalized_path]

    def add_diff(self, project_path: str, file_path: str, old_content: str, new_content: str) -> None:
        """Add a diff to the tree.

        Args:
            project_path: Path to project
            file_path: Path to file (relative to project)
            old_content: Original content
            new_content: Modified content
        """
        tree = self.get_or_create_tree(project_path)
        tree.add_diff(file_path, old_content, new_content)

    def remove_diff(self, project_path: str, file_path: str) -> None:
        """Remove a diff from the tree.

        Args:
            project_path: Path to project
            file_path: Path to file (relative to project)
        """
        tree = self.get_or_create_tree(project_path)
        tree.remove_diff(file_path)

    def get_diff_tree(self, project_path: str) -> dict[str, Any]:
        """Get diff tree structure.

        Args:
            project_path: Path to project

        Returns:
            Tree structure with file list and stats
        """
        tree = self.get_or_create_tree(project_path)
        return tree.render_webui()

    def get_file_diff(self, project_path: str, file_path: str) -> str:
        """Get unified diff for a specific file.

        Args:
            project_path: Path to project
            file_path: Path to file (relative to project)

        Returns:
            Unified diff string

        Raises:
            ValueError: If no diff available
        """
        tree = self.get_or_create_tree(project_path)
        return tree.get_unified_diff(file_path)

    def render_diff_html(self, project_path: str, file_path: str) -> str:
        """Render diff as HTML.

        Args:
            project_path: Path to project
            file_path: Path to file (relative to project)

        Returns:
            HTML-rendered diff

        Raises:
            ValueError: If no diff available
        """
        tree = self.get_or_create_tree(project_path)
        if file_path not in tree.modified_files:
            raise ValueError(f"No diff available for {file_path}")

        old_content, new_content = tree.modified_files[file_path]
        diff_view = DiffView("webui")
        return diff_view.render_diff(file_path, old_content, new_content)

    def clear_diffs(self, project_path: str) -> None:
        """Clear all diffs for a project.

        Args:
            project_path: Path to project
        """
        normalized_path = self._normalize_path(project_path)
        if normalized_path in self.diff_trees:
            self.diff_trees[normalized_path].clear_diffs()

    def scan_git_changes(self, project_path: str) -> int:
        """Scan git working directory for changes and populate diff tree.

        Args:
            project_path: Path to project (must be a git repository)

        Returns:
            Number of changed files found

        Raises:
            RuntimeError: If not a git repository or git command fails
        """
        import subprocess

        project_dir = Path(project_path)
        if not (project_dir / ".git").exists():
            raise RuntimeError(f"Not a git repository: {project_path}")

        try:
            # Get list of modified and added files
            result = subprocess.run(
                ["git", "diff", "--name-only", "HEAD"],
                cwd=project_path,
                capture_output=True,
                text=True,
                check=True,
            )

            changed_files = [line.strip() for line in result.stdout.strip().split("\n") if line.strip()]

            if not changed_files:
                # No changes found
                return 0

            # For each changed file, get old content (from HEAD) and new content (from working tree)
            count = 0
            for file_path in changed_files:
                full_path = project_dir / file_path

                # Skip if file doesn't exist in working tree (deleted files)
                if not full_path.exists():
                    logger.info(f"Skipping deleted file: {file_path}")
                    continue

                try:
                    # Get old content from git (HEAD version)
                    git_show_result = subprocess.run(
                        ["git", "show", f"HEAD:{file_path}"],
                        cwd=project_path,
                        capture_output=True,
                        text=True,
                        check=False,  # Don't fail if file doesn't exist in HEAD (new file)
                    )

                    # File is new (doesn't exist in HEAD) if returncode != 0
                    old_content = git_show_result.stdout if git_show_result.returncode == 0 else ""

                    # Get new content from working tree
                    new_content = full_path.read_text(encoding="utf-8", errors="replace")

                    # Add diff to tree
                    self.add_diff(project_path, file_path, old_content, new_content)
                    count += 1

                except Exception as e:
                    logger.warning(f"Failed to read file {file_path}: {e}")
                    continue

            return count

        except subprocess.CalledProcessError as e:
            raise RuntimeError(f"Git command failed: {e.stderr}") from e
        except Exception as e:
            raise RuntimeError(f"Failed to scan git changes: {e}") from e
