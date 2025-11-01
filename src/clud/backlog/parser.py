"""
Backlog.md parser for managing project tasks.

Supports multiple markdown formats:
- GitHub-style task lists: `- [ ] Task` (todo), `- [x] Task` (done)
- Task IDs: `#123` in title
- Priorities: `(priority: high|medium|low)` in title or description
- Section-based status: Tasks under "## To Do", "## In Progress", "## Done" headers
"""

import logging
import re
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

logger = logging.getLogger(__name__)

StatusType = Literal["todo", "in_progress", "done"]
PriorityType = Literal["low", "medium", "high"]


@dataclass
class BacklogTask:
    """Represents a task in the backlog."""

    id: str
    title: str
    status: StatusType
    description: str | None = None
    priority: PriorityType | None = None
    created_at: int | None = None
    updated_at: int | None = None


def load_backlog(path: Path) -> list[BacklogTask]:
    """
    Load and parse tasks from Backlog.md.

    Args:
        path: Path to Backlog.md file

    Returns:
        List of BacklogTask objects

    Handles:
        - Missing file (returns empty list)
        - Malformed markdown (skips invalid tasks, logs warnings)
        - File permission errors (logs error, returns empty list)
    """
    if not path.exists():
        logger.debug(f"Backlog.md not found at {path}, returning empty list")
        return []

    try:
        content = path.read_text(encoding="utf-8")
    except PermissionError:
        logger.error(f"Permission denied reading {path}")
        return []
    except Exception as e:
        logger.error(f"Error reading {path}: {e}")
        return []

    return _parse_markdown(content)


def save_backlog(path: Path, tasks: list[BacklogTask]) -> None:
    """
    Save tasks to Backlog.md in markdown format.

    Args:
        path: Path to Backlog.md file
        tasks: List of tasks to save

    Raises:
        PermissionError: If unable to write to file
        OSError: If unable to create parent directories
    """
    # Group tasks by status
    todo_tasks = [t for t in tasks if t.status == "todo"]
    in_progress_tasks = [t for t in tasks if t.status == "in_progress"]
    done_tasks = [t for t in tasks if t.status == "done"]

    # Build markdown content
    lines = ["# Backlog", ""]

    if todo_tasks:
        lines.append("## To Do")
        for task in todo_tasks:
            lines.append(_task_to_markdown(task, checked=False))
        lines.append("")

    if in_progress_tasks:
        lines.append("## In Progress")
        for task in in_progress_tasks:
            lines.append(_task_to_markdown(task, checked=False))
        lines.append("")

    if done_tasks:
        lines.append("## Done")
        for task in done_tasks:
            lines.append(_task_to_markdown(task, checked=True))
        lines.append("")

    # Ensure parent directory exists
    path.parent.mkdir(parents=True, exist_ok=True)

    # Write to file
    path.write_text("\n".join(lines), encoding="utf-8")
    logger.debug(f"Saved {len(tasks)} tasks to {path}")


def add_task(path: Path, task: BacklogTask) -> None:
    """
    Add a new task to Backlog.md.

    Args:
        path: Path to Backlog.md file
        task: Task to add

    Raises:
        PermissionError: If unable to write to file
    """
    tasks = load_backlog(path)

    # Ensure task has timestamps
    now = int(time.time())
    if task.created_at is None:
        task.created_at = now
    if task.updated_at is None:
        task.updated_at = now

    # Ensure unique ID
    if not task.id or any(t.id == task.id for t in tasks):
        task.id = _generate_task_id(tasks)

    tasks.append(task)
    save_backlog(path, tasks)
    logger.info(f"Added task #{task.id}: {task.title}")


def update_task(path: Path, task_id: str, updates: dict[str, str | int | None]) -> None:
    """
    Update an existing task in Backlog.md.

    Args:
        path: Path to Backlog.md file
        task_id: ID of task to update
        updates: Dictionary of fields to update

    Raises:
        ValueError: If task not found
        PermissionError: If unable to write to file
    """
    tasks = load_backlog(path)

    # Find task
    task_index = next((i for i, t in enumerate(tasks) if t.id == task_id), None)
    if task_index is None:
        raise ValueError(f"Task #{task_id} not found")

    task = tasks[task_index]

    # Apply updates
    for key, value in updates.items():
        if hasattr(task, key):
            setattr(task, key, value)

    # Update timestamp
    task.updated_at = int(time.time())

    save_backlog(path, tasks)
    logger.info(f"Updated task #{task_id}")


def _parse_markdown(content: str) -> list[BacklogTask]:
    """
    Parse markdown content into BacklogTask objects.

    Supports:
    - GitHub-style task lists: `- [ ] Task` (todo), `- [x] Task` (done)
    - Section headers to determine status: "## To Do", "## In Progress", "## Done"
    - Task IDs: `#123` anywhere in title
    - Priorities: `(priority: high)` anywhere in title or description
    - Multi-line descriptions (indented under task)
    """
    tasks: list[BacklogTask] = []
    lines = content.split("\n")
    current_status: StatusType = "todo"
    current_task: BacklogTask | None = None
    task_counter = 1

    for line in lines:
        # Check for section headers
        if line.startswith("##"):
            section_name = line[2:].strip().lower()
            if "to do" in section_name or "todo" in section_name:
                current_status = "todo"
            elif "in progress" in section_name or "in-progress" in section_name:
                current_status = "in_progress"
            elif "done" in section_name or "completed" in section_name:
                current_status = "done"
            continue

        # Check for task list items
        task_match = re.match(r"^- \[([x ])\]\s+(.+)$", line.strip())
        if task_match:
            # Save previous task if exists
            if current_task:
                tasks.append(current_task)

            checked = task_match.group(1).lower() == "x"
            title_raw = task_match.group(2).strip()

            # Extract task ID
            task_id_match = re.search(r"#(\d+)", title_raw)
            if task_id_match:
                task_id = task_id_match.group(1)
                title_raw = re.sub(r"#\d+\s*", "", title_raw).strip()
            else:
                task_id = str(task_counter)
                task_counter += 1

            # Extract priority
            priority_match = re.search(r"\(priority:\s*(low|medium|high)\)", title_raw, re.IGNORECASE)
            priority: PriorityType | None = None
            if priority_match:
                priority = priority_match.group(1).lower()  # type: ignore
                title_raw = re.sub(r"\(priority:\s*\w+\)", "", title_raw, flags=re.IGNORECASE).strip()

            # Determine status (checked overrides section)
            status = "done" if checked else current_status

            current_task = BacklogTask(
                id=task_id,
                title=title_raw,
                status=status,
                priority=priority,
                created_at=None,
                updated_at=None,
            )
            continue

        # Check for indented description lines
        if current_task and line.startswith("  ") and line.strip():
            desc_line = line.strip()
            # Skip metadata lines
            if desc_line.startswith("-") or ":" in desc_line[:20]:
                # Check for priority in description
                priority_match = re.search(r"priority:\s*(low|medium|high)", desc_line, re.IGNORECASE)
                if priority_match and not current_task.priority:
                    current_task.priority = priority_match.group(1).lower()  # type: ignore
                continue

            # Add to description
            if current_task.description:
                current_task.description += "\n" + desc_line
            else:
                current_task.description = desc_line

    # Save last task
    if current_task:
        tasks.append(current_task)

    logger.debug(f"Parsed {len(tasks)} tasks from markdown")
    return tasks


def _task_to_markdown(task: BacklogTask, checked: bool = False) -> str:
    """Convert a BacklogTask to markdown format."""
    checkbox = "[x]" if checked else "[ ]"
    priority_str = f" (priority: {task.priority})" if task.priority else ""
    title = f"- {checkbox} #{task.id} {task.title}{priority_str}"

    if task.description:
        # Indent description lines
        desc_lines = task.description.split("\n")
        desc_str = "\n".join(f"  {line}" for line in desc_lines)
        return f"{title}\n{desc_str}"

    return title


def _generate_task_id(tasks: list[BacklogTask]) -> str:
    """Generate a unique task ID."""
    if not tasks:
        return "1"

    # Find highest numeric ID
    max_id = 0
    for task in tasks:
        try:
            task_num = int(task.id)
            max_id = max(max_id, task_num)
        except ValueError:
            continue

    return str(max_id + 1)
