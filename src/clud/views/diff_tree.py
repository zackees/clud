"""Diff tree view for navigating files with pending changes."""

import difflib
import logging
from pathlib import Path
from typing import Any, Literal

logger = logging.getLogger(__name__)


class DiffTreeView:
    """Tree view showing files with diffs, integrated with diff2html."""

    def __init__(self, root_path: str) -> None:
        """Initialize diff tree view.

        Args:
            root_path: Root project path
        """
        self.root_path = Path(root_path)
        self.expanded_dirs: set[Path] = set()
        self.modified_files: dict[str, tuple[str, str]] = {}  # path -> (old_content, new_content)
        self.gitignore_patterns = self._load_gitignore()

    def _load_gitignore(self) -> list[str]:
        """Load .gitignore patterns.

        Returns:
            List of gitignore patterns
        """
        patterns: list[str] = []
        gitignore_path = self.root_path / ".gitignore"

        if gitignore_path.exists():
            try:
                with open(gitignore_path, encoding="utf-8") as f:
                    for line in f:
                        line = line.strip()
                        if line and not line.startswith("#"):
                            patterns.append(line)
            except Exception as e:
                logger.warning("Could not load .gitignore: %s", e)

        return patterns

    def add_diff(self, file_path: str, old_content: str, new_content: str) -> None:
        """Add a file diff to the tree view.

        Args:
            file_path: Path to file relative to root
            old_content: Original file content
            new_content: Modified file content
        """
        self.modified_files[file_path] = (old_content, new_content)

    def remove_diff(self, file_path: str) -> None:
        """Remove a file diff from the tree view.

        Args:
            file_path: Path to file relative to root
        """
        self.modified_files.pop(file_path, None)

    def clear_diffs(self) -> None:
        """Clear all diffs."""
        self.modified_files.clear()

    def get_diff_stats(self, file_path: str) -> dict[str, int]:
        """Get diff statistics for a file.

        Args:
            file_path: Path to file

        Returns:
            Dict with 'additions' and 'deletions' counts
        """
        if file_path not in self.modified_files:
            return {"additions": 0, "deletions": 0}

        old_content, new_content = self.modified_files[file_path]
        old_lines = old_content.splitlines()
        new_lines = new_content.splitlines()

        additions = 0
        deletions = 0

        for tag, _i1, _i2, _j1, j2 in difflib.SequenceMatcher(None, old_lines, new_lines).get_opcodes():
            if tag == "insert":
                additions += j2 - _j1
            elif tag == "delete":
                deletions += _i2 - _i1
            elif tag == "replace":
                deletions += _i2 - _i1
                additions += j2 - _j1

        return {"additions": additions, "deletions": deletions}

    def get_file_status(self, file_path: str) -> Literal["modified", "added", "deleted"]:
        """Get the status of a file.

        Args:
            file_path: Path to file

        Returns:
            File status: 'modified', 'added', or 'deleted'
        """
        if file_path not in self.modified_files:
            return "modified"

        old_content, new_content = self.modified_files[file_path]

        if not old_content:
            return "added"
        elif not new_content:
            return "deleted"
        else:
            return "modified"

    def get_tree_with_diffs(self) -> dict[str, Any]:
        """Get tree structure containing only files with diffs.

        Returns:
            Tree structure with diff metadata for diff2html
        """
        tree: dict[str, Any] = {}

        for file_path in self.modified_files:
            stats = self.get_diff_stats(file_path)
            status = self.get_file_status(file_path)

            # Build nested tree structure
            parts = Path(file_path).parts
            current = tree

            for i, part in enumerate(parts):
                if i == len(parts) - 1:
                    # Leaf node (file)
                    current[part] = {
                        "has_diff": True,
                        "diff_stats": stats,
                        "status": status,
                        "path": file_path,
                    }
                else:
                    # Directory node
                    if part not in current:
                        current[part] = {}
                    current = current[part]

        return tree

    def get_file_list_with_stats(self) -> list[dict[str, Any]]:
        """Get flat list of files with diff stats.

        Returns:
            List of file info dictionaries
        """
        files: list[dict[str, Any]] = []

        for file_path in sorted(self.modified_files.keys()):
            stats = self.get_diff_stats(file_path)
            status = self.get_file_status(file_path)

            files.append(
                {
                    "path": file_path,
                    "status": status,
                    "additions": stats["additions"],
                    "deletions": stats["deletions"],
                }
            )

        return files

    def render_terminal(self, max_width: int = 40) -> str:
        """Render tree view for terminal (ASCII art) showing modified files.

        Args:
            max_width: Maximum width for rendering

        Returns:
            ASCII art tree representation
        """
        if not self.modified_files:
            return "No modified files"

        lines: list[str] = ["Modified files:"]

        # Build tree structure
        tree = self.get_tree_with_diffs()

        def render_node(node: dict[str, Any], prefix: str = "", is_last: bool = True) -> None:
            """Recursively render tree nodes."""
            items = list(node.items())

            for i, (name, value) in enumerate(items):
                is_last_item = i == len(items) - 1
                connector = "└── " if is_last_item else "├── "
                extension = "    " if is_last_item else "│   "

                if isinstance(value, dict) and "has_diff" in value:
                    # File node
                    diff_stats: dict[str, int] = value["diff_stats"]  # type: ignore[assignment]
                    status_icon = "✏️" if value["status"] == "modified" else ("➕" if value["status"] == "added" else "➖")
                    diff_text = f"(+{diff_stats['additions']} -{diff_stats['deletions']})"

                    # Truncate if too long
                    line = f"{prefix}{connector}{status_icon} {name} {diff_text}"
                    if len(line) > max_width:
                        name_truncated = name[: max_width - len(connector) - len(diff_text) - 10] + "..."
                        line = f"{prefix}{connector}{status_icon} {name_truncated} {diff_text}"

                    lines.append(line)
                elif isinstance(value, dict):
                    # Directory node - recursively render subdirectory
                    lines.append(f"{prefix}{connector}📁 {name}/")
                    # Type narrowing limitation - value is dict[str, Any] from our tree structure
                    render_node(value, prefix + extension, is_last_item)  # type: ignore[arg-type]

        render_node(tree)

        lines.append("")
        lines.append("Click file name to view diff")

        return "\n".join(lines)

    def render_webui(self) -> dict[str, Any]:
        """Render tree view for Web UI (JSON structure with diff metadata).

        Returns:
            JSON structure for frontend rendering
        """
        return {
            "files": self.get_file_list_with_stats(),
            "tree": self.get_tree_with_diffs(),
        }

    def get_unified_diff(self, file_path: str) -> str:
        """Get unified diff for a specific file to feed to diff2html.

        Args:
            file_path: Path to file

        Returns:
            Unified diff string that diff2html can render

        Raises:
            ValueError: If no diff available for the file
        """
        if file_path not in self.modified_files:
            raise ValueError(f"No diff available for {file_path}")

        old_content, new_content = self.modified_files[file_path]
        return self._generate_unified_diff(old_content, new_content, file_path)

    def _generate_unified_diff(self, old_content: str, new_content: str, file_path: str) -> str:
        """Generate unified diff format.

        Args:
            old_content: Original content
            new_content: Modified content
            file_path: Path to file

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
        )

        return "".join(diff_lines)
