"""Unit tests for backlog parser module."""

import logging
import tempfile
import unittest
from pathlib import Path

from clud.backlog.parser import BacklogTask, add_task, load_backlog, save_backlog, update_task


class TestBacklogParser(unittest.TestCase):
    """Test cases for backlog parser functionality."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.test_dir = tempfile.mkdtemp()
        self.backlog_path = Path(self.test_dir) / "Backlog.md"
        # Suppress debug logs during tests
        logging.getLogger("clud.backlog.parser").setLevel(logging.WARNING)

    def test_load_backlog_missing_file(self) -> None:
        """Test loading backlog when file doesn't exist."""
        tasks = load_backlog(self.backlog_path)
        self.assertEqual(tasks, [])

    def test_load_backlog_empty_file(self) -> None:
        """Test loading backlog from empty file."""
        self.backlog_path.write_text("", encoding="utf-8")
        tasks = load_backlog(self.backlog_path)
        self.assertEqual(tasks, [])

    def test_parse_github_style_tasks(self) -> None:
        """Test parsing GitHub-style task lists."""
        markdown = """# Backlog

## To Do
- [ ] #1 Add user authentication
- [ ] #2 Create dashboard UI (priority: medium)

## Done
- [x] #3 Setup project structure
"""
        self.backlog_path.write_text(markdown, encoding="utf-8")
        tasks = load_backlog(self.backlog_path)

        self.assertEqual(len(tasks), 3)

        # Check first task
        self.assertEqual(tasks[0].id, "1")
        self.assertEqual(tasks[0].title, "Add user authentication")
        self.assertEqual(tasks[0].status, "todo")
        self.assertIsNone(tasks[0].priority)

        # Check second task
        self.assertEqual(tasks[1].id, "2")
        self.assertEqual(tasks[1].title, "Create dashboard UI")
        self.assertEqual(tasks[1].status, "todo")
        self.assertEqual(tasks[1].priority, "medium")

        # Check third task (done)
        self.assertEqual(tasks[2].id, "3")
        self.assertEqual(tasks[2].title, "Setup project structure")
        self.assertEqual(tasks[2].status, "done")

    def test_parse_task_with_description(self) -> None:
        """Test parsing tasks with multi-line descriptions."""
        markdown = """# Backlog

## To Do
- [ ] #1 Add user authentication
  Implement OAuth2 flow
  Add JWT token handling
"""
        self.backlog_path.write_text(markdown, encoding="utf-8")
        tasks = load_backlog(self.backlog_path)

        self.assertEqual(len(tasks), 1)
        self.assertEqual(tasks[0].description, "Implement OAuth2 flow\nAdd JWT token handling")

    def test_parse_priority_variants(self) -> None:
        """Test parsing different priority formats."""
        markdown = """# Backlog

## To Do
- [ ] #1 Task with priority (priority: high)
- [ ] #2 Task with lowercase (priority: low)
- [ ] #3 Task without priority
"""
        self.backlog_path.write_text(markdown, encoding="utf-8")
        tasks = load_backlog(self.backlog_path)

        self.assertEqual(tasks[0].priority, "high")
        self.assertEqual(tasks[1].priority, "low")
        self.assertIsNone(tasks[2].priority)

    def test_parse_tasks_without_ids(self) -> None:
        """Test parsing tasks without explicit IDs (auto-generate)."""
        markdown = """# Backlog

## To Do
- [ ] First task
- [ ] Second task
"""
        self.backlog_path.write_text(markdown, encoding="utf-8")
        tasks = load_backlog(self.backlog_path)

        self.assertEqual(len(tasks), 2)
        self.assertEqual(tasks[0].id, "1")
        self.assertEqual(tasks[1].id, "2")

    def test_parse_in_progress_section(self) -> None:
        """Test parsing tasks in 'In Progress' section."""
        markdown = """# Backlog

## In Progress
- [ ] #5 Working on this task
"""
        self.backlog_path.write_text(markdown, encoding="utf-8")
        tasks = load_backlog(self.backlog_path)

        self.assertEqual(len(tasks), 1)
        self.assertEqual(tasks[0].status, "in_progress")

    def test_save_backlog(self) -> None:
        """Test saving tasks to Backlog.md."""
        tasks = [
            BacklogTask(id="1", title="Task 1", status="todo", priority="high"),
            BacklogTask(id="2", title="Task 2", status="in_progress", description="Working on it"),
            BacklogTask(id="3", title="Task 3", status="done"),
        ]

        save_backlog(self.backlog_path, tasks)

        # Verify file exists
        self.assertTrue(self.backlog_path.exists())

        # Verify content can be parsed back
        loaded_tasks = load_backlog(self.backlog_path)
        self.assertEqual(len(loaded_tasks), 3)

        # Check first task
        self.assertEqual(loaded_tasks[0].id, "1")
        self.assertEqual(loaded_tasks[0].title, "Task 1")
        self.assertEqual(loaded_tasks[0].status, "todo")
        self.assertEqual(loaded_tasks[0].priority, "high")

        # Check second task
        self.assertEqual(loaded_tasks[1].id, "2")
        self.assertEqual(loaded_tasks[1].status, "in_progress")
        self.assertEqual(loaded_tasks[1].description, "Working on it")

        # Check third task
        self.assertEqual(loaded_tasks[2].id, "3")
        self.assertEqual(loaded_tasks[2].status, "done")

    def test_add_task(self) -> None:
        """Test adding a new task to backlog."""
        # Create initial backlog
        initial_tasks = [BacklogTask(id="1", title="Existing task", status="todo")]
        save_backlog(self.backlog_path, initial_tasks)

        # Add new task
        new_task = BacklogTask(id="", title="New task", status="todo", priority="medium")
        add_task(self.backlog_path, new_task)

        # Verify task was added
        tasks = load_backlog(self.backlog_path)
        self.assertEqual(len(tasks), 2)
        self.assertEqual(tasks[1].title, "New task")
        self.assertEqual(tasks[1].priority, "medium")

        # Verify ID was auto-generated
        self.assertEqual(tasks[1].id, "2")

        # Note: timestamps are not persisted in markdown format
        # They are only used internally during add/update operations

    def test_add_task_with_duplicate_id(self) -> None:
        """Test adding task with duplicate ID (should auto-generate new ID)."""
        # Create initial backlog
        initial_tasks = [BacklogTask(id="1", title="Task 1", status="todo")]
        save_backlog(self.backlog_path, initial_tasks)

        # Try to add task with duplicate ID
        new_task = BacklogTask(id="1", title="Task 2", status="todo")
        add_task(self.backlog_path, new_task)

        # Verify new ID was generated
        tasks = load_backlog(self.backlog_path)
        self.assertEqual(len(tasks), 2)
        self.assertEqual(tasks[1].id, "2")

    def test_update_task(self) -> None:
        """Test updating an existing task."""
        # Create initial backlog
        tasks = [BacklogTask(id="1", title="Original title", status="todo")]
        save_backlog(self.backlog_path, tasks)

        # Update task
        update_task(self.backlog_path, "1", {"title": "Updated title", "status": "in_progress", "priority": "high"})

        # Verify updates
        loaded_tasks = load_backlog(self.backlog_path)
        self.assertEqual(len(loaded_tasks), 1)
        self.assertEqual(loaded_tasks[0].title, "Updated title")
        self.assertEqual(loaded_tasks[0].status, "in_progress")
        self.assertEqual(loaded_tasks[0].priority, "high")

        # Note: timestamps are not persisted in markdown format

    def test_update_task_not_found(self) -> None:
        """Test updating non-existent task raises ValueError."""
        tasks = [BacklogTask(id="1", title="Task 1", status="todo")]
        save_backlog(self.backlog_path, tasks)

        with self.assertRaises(ValueError) as context:
            update_task(self.backlog_path, "999", {"title": "Updated"})

        self.assertIn("Task #999 not found", str(context.exception))

    def test_parse_malformed_markdown(self) -> None:
        """Test parsing malformed markdown (should skip invalid tasks)."""
        markdown = """# Backlog

This is just some text, not a task.

## To Do
- [ ] #1 Valid task
Some random text
- Not a valid task list item
- [ ] #2 Another valid task
"""
        self.backlog_path.write_text(markdown, encoding="utf-8")
        tasks = load_backlog(self.backlog_path)

        # Should only parse valid tasks
        self.assertEqual(len(tasks), 2)
        self.assertEqual(tasks[0].id, "1")
        self.assertEqual(tasks[1].id, "2")

    def test_save_empty_backlog(self) -> None:
        """Test saving empty backlog."""
        save_backlog(self.backlog_path, [])

        # File should exist but have minimal content
        self.assertTrue(self.backlog_path.exists())
        content = self.backlog_path.read_text(encoding="utf-8")
        self.assertIn("# Backlog", content)

        # Should load as empty list
        tasks = load_backlog(self.backlog_path)
        self.assertEqual(tasks, [])

    def test_roundtrip_preserves_data(self) -> None:
        """Test that save -> load preserves all task data."""
        original_tasks = [
            BacklogTask(
                id="1",
                title="Complex task",
                status="in_progress",
                description="Multi-line\ndescription",
                priority="high",
                created_at=1234567890,
                updated_at=1234567900,
            )
        ]

        save_backlog(self.backlog_path, original_tasks)
        loaded_tasks = load_backlog(self.backlog_path)

        self.assertEqual(len(loaded_tasks), 1)
        task = loaded_tasks[0]
        self.assertEqual(task.id, "1")
        self.assertEqual(task.title, "Complex task")
        self.assertEqual(task.status, "in_progress")
        self.assertEqual(task.description, "Multi-line\ndescription")
        self.assertEqual(task.priority, "high")


if __name__ == "__main__":
    unittest.main()
