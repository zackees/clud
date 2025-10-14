"""Diff view for rendering code changes with syntax highlighting."""

import difflib
import logging
import subprocess
from typing import Literal

logger = logging.getLogger(__name__)


class DiffView:
    """Unified diff view using diff2html."""

    def __init__(self, view_type: Literal["terminal", "webui", "vscode"]) -> None:
        """Initialize diff view.

        Args:
            view_type: Type of view (terminal, webui, vscode)
        """
        self.view_type = view_type

    def render_diff(
        self,
        file_path: str,
        old_content: str,
        new_content: str,
        context_lines: int = 3,
    ) -> str:
        """Render diff between old and new content.

        Args:
            file_path: Path to the file being diffed
            old_content: Original file content
            new_content: Modified file content
            context_lines: Number of context lines around changes

        Returns:
            Formatted diff output (HTML for webui, ANSI for terminal)
        """
        # Generate unified diff
        diff = self._generate_unified_diff(old_content, new_content, file_path, context_lines)

        if self.view_type == "terminal":
            return self._render_terminal_diff(diff, max_width=120)
        elif self.view_type == "webui":
            return self._render_html_diff(diff)
        elif self.view_type == "vscode":
            return self._render_vscode_diff(diff)
        else:
            return diff

    def _render_terminal_diff(self, unified_diff: str, max_width: int) -> str:
        """Render diff for terminal using ANSI colors.

        Args:
            unified_diff: Unified diff string
            max_width: Maximum width constraint

        Returns:
            ANSI-colored diff
        """
        # Try to use diff2html CLI if available
        try:
            result = subprocess.run(
                ["diff2html", "--style", "line", "--format", "ansi"],
                input=unified_diff.encode(),
                capture_output=True,
                timeout=5,
            )
            if result.returncode == 0:
                return result.stdout.decode()
        except (subprocess.SubprocessError, FileNotFoundError):
            # Fall back to simple ANSI coloring
            pass

        # Simple ANSI coloring as fallback
        lines = unified_diff.split("\n")
        colored_lines: list[str] = []

        for line in lines:
            if line.startswith("+++") or line.startswith("---"):
                # File headers (bold)
                colored_lines.append(f"\033[1m{line}\033[0m")
            elif line.startswith("+"):
                # Additions (green)
                colored_lines.append(f"\033[32m{line}\033[0m")
            elif line.startswith("-"):
                # Deletions (red)
                colored_lines.append(f"\033[31m{line}\033[0m")
            elif line.startswith("@@"):
                # Hunk headers (cyan)
                colored_lines.append(f"\033[36m{line}\033[0m")
            else:
                # Context (normal)
                colored_lines.append(line)

        # Apply width constraint
        wrapped_lines: list[str] = []
        separator = "─" * min(max_width, 120)
        wrapped_lines.append(separator)

        for line in colored_lines:
            # Note: This is simplified - proper ANSI-aware wrapping would be more complex
            if len(line) > max_width:
                # Truncate long lines
                wrapped_lines.append(line[: max_width - 3] + "...")
            else:
                wrapped_lines.append(line)

        wrapped_lines.append(separator)
        return "\n".join(wrapped_lines)

    def _render_html_diff(self, unified_diff: str) -> str:
        """Render diff for Web UI using diff2html.

        Args:
            unified_diff: Unified diff string

        Returns:
            HTML diff
        """
        # Try to use diff2html CLI if available
        try:
            result = subprocess.run(
                ["diff2html", "--style", "side", "--format", "html"],
                input=unified_diff.encode(),
                capture_output=True,
                timeout=5,
            )
            if result.returncode == 0:
                return result.stdout.decode()
        except (subprocess.SubprocessError, FileNotFoundError):
            logger.warning("diff2html not found, falling back to simple HTML")

        # Fall back to simple HTML rendering
        return self._simple_html_diff(unified_diff)

    def _render_vscode_diff(self, unified_diff: str) -> str:
        """Render diff for VSCode (placeholder for future implementation).

        Args:
            unified_diff: Unified diff string

        Returns:
            Formatted diff for VSCode
        """
        # For now, just return the unified diff
        # In the future, this could integrate with VSCode's native diff viewer
        return unified_diff

    def _generate_unified_diff(
        self,
        old_content: str,
        new_content: str,
        file_path: str,
        context_lines: int = 3,
    ) -> str:
        """Generate unified diff format.

        Args:
            old_content: Original content
            new_content: Modified content
            file_path: Path to file
            context_lines: Number of context lines

        Returns:
            Unified diff string
        """
        old_lines = old_content.splitlines(keepends=True)
        new_lines = new_content.splitlines(keepends=True)

        diff_lines = difflib.unified_diff(
            old_lines,
            new_lines,
            fromfile=f"a/{file_path}",
            tofile=f"b/{file_path}",
            lineterm="",
            n=context_lines,
        )

        return "".join(diff_lines)

    def _simple_html_diff(self, unified_diff: str) -> str:
        """Generate simple HTML diff as fallback.

        Args:
            unified_diff: Unified diff string

        Returns:
            Basic HTML representation
        """
        lines = unified_diff.split("\n")
        html_lines: list[str] = ['<div class="diff-container" style="font-family: monospace; font-size: 14px;">']

        for line in lines:
            escaped_line = line.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")

            if line.startswith("+++") or line.startswith("---"):
                # File headers (bold)
                html_lines.append(f'<div style="font-weight: bold;">{escaped_line}</div>')
            elif line.startswith("+"):
                # Additions (green background)
                html_lines.append(f'<div style="background-color: #e6ffed; color: #24292e;">{escaped_line}</div>')
            elif line.startswith("-"):
                # Deletions (red background)
                html_lines.append(f'<div style="background-color: #ffeef0; color: #24292e;">{escaped_line}</div>')
            elif line.startswith("@@"):
                # Hunk headers (light blue background)
                html_lines.append(f'<div style="background-color: #f1f8ff; color: #0366d6;">{escaped_line}</div>')
            else:
                # Context (normal)
                html_lines.append(f"<div>{escaped_line}</div>")

        html_lines.append("</div>")
        return "\n".join(html_lines)
