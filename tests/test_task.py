"""Unit tests for clud task management functionality."""
# pyright: reportUnknownParameterType=false, reportMissingParameterType=false, reportUnknownLambdaType=false

import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from clud.task import (
    _build_editor_command,
    _create_initial_task_content,
    _find_linux_editor,
    _find_macos_editor,
    _find_windows_editor,
    _handle_lint_result,
    _has_blocking_problem,
    _lint_script_exists,
    _prompt_to_create_task_file,
    _task_file_has_content,
    find_editor,
    fix_lint_errors,
    handle_task_command,
    open_in_editor,
    process_existing_task,
    process_new_task,
    process_task_file,
    run_lint,
)


class TestFindEditor(unittest.TestCase):
    """Test editor finding functionality."""

    @patch("platform.system")
    @patch("clud.task._find_windows_editor")
    def test_find_editor_windows(self, mock_windows, mock_system):
        """Test finding editor on Windows."""
        mock_system.return_value = "Windows"
        mock_windows.return_value = "notepad.exe"

        result = find_editor()
        self.assertEqual(result, "notepad.exe")
        mock_windows.assert_called_once()  # type: ignore[misc]

    @patch("platform.system")
    @patch("clud.task._find_macos_editor")
    def test_find_editor_macos(self, mock_macos, mock_system):
        """Test finding editor on macOS."""
        mock_system.return_value = "Darwin"
        mock_macos.return_value = "nano"

        result = find_editor()
        self.assertEqual(result, "nano")
        mock_macos.assert_called_once()  # type: ignore[misc]

    @patch("platform.system")
    @patch("clud.task._find_linux_editor")
    def test_find_editor_linux(self, mock_linux, mock_system):
        """Test finding editor on Linux."""
        mock_system.return_value = "Linux"
        mock_linux.return_value = "vim"

        result = find_editor()
        self.assertEqual(result, "vim")
        mock_linux.assert_called_once()  # type: ignore[misc]

    @patch("pathlib.Path.exists")
    def test_find_windows_editor_sublime(self, mock_exists):
        """Test finding Sublime Text on Windows."""
        mock_exists.return_value = True

        result = _find_windows_editor()
        self.assertIn("Sublime Text", result)

    @patch("pathlib.Path.exists")
    @patch("shutil.which")
    def test_find_windows_editor_fallback(self, mock_which, mock_exists):
        """Test fallback to notepad on Windows."""
        mock_exists.return_value = False
        mock_which.return_value = None

        result = _find_windows_editor()
        self.assertEqual(result, "notepad.exe")

    @patch("shutil.which")
    def test_find_macos_editor(self, mock_which):
        """Test finding editor on macOS."""
        mock_which.side_effect = lambda cmd: "nano" if cmd == "nano" else None

        result = _find_macos_editor()
        self.assertEqual(result, "nano")

    @patch("shutil.which")
    def test_find_linux_editor(self, mock_which):
        """Test finding editor on Linux."""
        mock_which.side_effect = lambda cmd: "vim" if cmd == "vim" else None

        result = _find_linux_editor()
        self.assertEqual(result, "vim")


class TestBuildEditorCommand(unittest.TestCase):
    """Test editor command building."""

    @patch("platform.system")
    def test_build_editor_command_windows_gui(self, mock_system):
        """Test building command for GUI editor on Windows."""
        mock_system.return_value = "Windows"

        cmd = _build_editor_command("sublime_text.exe", Path("test.md"))
        self.assertEqual(cmd, ["start", "", "sublime_text.exe", "test.md"])

    @patch("platform.system")
    def test_build_editor_command_windows_notepad(self, mock_system):
        """Test building command for notepad on Windows."""
        mock_system.return_value = "Windows"

        cmd = _build_editor_command("notepad.exe", Path("test.md"))
        self.assertEqual(cmd, ["notepad.exe", "test.md"])

    @patch("platform.system")
    def test_build_editor_command_macos_gui(self, mock_system):
        """Test building command for GUI editor on macOS."""
        mock_system.return_value = "Darwin"

        cmd = _build_editor_command("subl", Path("test.md"))
        self.assertEqual(cmd, ["subl", "test.md"])

    @patch("platform.system")
    def test_build_editor_command_terminal(self, mock_system):
        """Test building command for terminal editor."""
        mock_system.return_value = "Linux"

        cmd = _build_editor_command("vim", Path("test.md"))
        self.assertEqual(cmd, ["vim", "test.md"])


class TestOpenInEditor(unittest.TestCase):
    """Test opening files in editor."""

    @patch("clud.task.find_editor")
    def test_open_in_editor_no_editor(self, mock_find_editor):
        """Test handling when no editor is found."""
        mock_find_editor.return_value = None

        result = open_in_editor(Path("test.md"))
        self.assertFalse(result)

    @patch("clud.task.find_editor")
    @patch("clud.task._exec")
    @patch("clud.task._build_editor_command")
    def test_open_in_editor_success(self, mock_build_cmd, mock_exec, mock_find_editor):
        """Test successful editor opening."""
        mock_find_editor.return_value = "nano"
        mock_build_cmd.return_value = ["nano", "test.md"]
        mock_exec.return_value = Mock(returncode=0)

        result = open_in_editor(Path("test.md"))
        self.assertTrue(result)
        mock_exec.assert_called_once()  # type: ignore[misc]

    @patch("clud.task.find_editor")
    @patch("clud.task._exec")
    def test_open_in_editor_exception(self, mock_exec, mock_find_editor):
        """Test handling editor launch exception."""
        mock_find_editor.return_value = "nano"
        mock_exec.side_effect = Exception("Launch failed")

        result = open_in_editor(Path("test.md"))
        self.assertFalse(result)


class TestLintFunctionality(unittest.TestCase):
    """Test lint-related functions."""

    @patch("pathlib.Path.exists")
    def test_lint_script_exists_true(self, mock_exists):
        """Test when lint script exists."""
        mock_exists.return_value = True

        result = _lint_script_exists()
        self.assertTrue(result)

    @patch("pathlib.Path.exists")
    def test_lint_script_exists_false(self, mock_exists):
        """Test when lint script doesn't exist."""
        mock_exists.return_value = False

        result = _lint_script_exists()
        self.assertFalse(result)

    def test_handle_lint_result_success(self):
        """Test handling successful lint result."""
        result = Mock(returncode=0)

        success = _handle_lint_result(result)
        self.assertTrue(success)

    def test_handle_lint_result_failure(self):
        """Test handling failed lint result."""
        result = Mock(returncode=1, stdout="Error", stderr="")

        success = _handle_lint_result(result)
        self.assertFalse(success)

    @patch("clud.task._lint_script_exists")
    def test_run_lint_no_script(self, mock_lint_exists):
        """Test when no lint script exists."""
        mock_lint_exists.return_value = False

        result = run_lint()
        self.assertTrue(result)

    @patch("clud.task._lint_script_exists")
    @patch("clud.task._exec")
    @patch("clud.task._handle_lint_result")
    def test_run_lint_success(self, mock_handle, mock_exec, mock_lint_exists):
        """Test successful lint run."""
        mock_lint_exists.return_value = True
        mock_exec.return_value = Mock(returncode=0)
        mock_handle.return_value = True

        result = run_lint()
        self.assertTrue(result)
        mock_exec.assert_called_once()  # type: ignore[misc]

    @patch("clud.task.run_lint")
    def test_fix_lint_errors_immediate_success(self, mock_run_lint):
        """Test immediate lint success."""
        mock_run_lint.return_value = True

        result = fix_lint_errors()
        self.assertTrue(result)
        self.assertEqual(mock_run_lint.call_count, 1)  # type: ignore[misc]

    @patch("clud.task.run_lint")
    @patch("time.sleep")
    def test_fix_lint_errors_max_iterations(self, mock_sleep, mock_run_lint):
        """Test reaching max iterations."""
        mock_run_lint.return_value = False

        result = fix_lint_errors()
        self.assertFalse(result)
        self.assertEqual(mock_run_lint.call_count, 10)  # type: ignore[misc]


class TestTaskFileHelpers(unittest.TestCase):
    """Test task file helper functions."""

    def test_task_file_has_content_true(self):
        """Test when task file has content."""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".md", delete=False) as f:
            f.write("# Task\nSome content")
            temp_path = Path(f.name)

        try:
            result = _task_file_has_content(temp_path)
            self.assertTrue(result)
        finally:
            temp_path.unlink()

    def test_task_file_has_content_empty(self):
        """Test when task file is empty."""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".md", delete=False) as f:
            temp_path = Path(f.name)

        try:
            result = _task_file_has_content(temp_path)
            self.assertFalse(result)
        finally:
            temp_path.unlink()

    def test_task_file_has_content_nonexistent(self):
        """Test when task file doesn't exist."""
        temp_path = Path("nonexistent_task.md")

        result = _task_file_has_content(temp_path)
        self.assertFalse(result)

    def test_has_blocking_problem_true(self):
        """Test detecting blocking problem."""
        content = "# Task\nBLOCKING PROBLEM: Can't continue"

        result = _has_blocking_problem(content)
        self.assertTrue(result)

    def test_has_blocking_problem_critical_decision(self):
        """Test detecting critical decision."""
        content = "# Task\nCRITICAL DECISION needs to be made"

        result = _has_blocking_problem(content)
        self.assertTrue(result)

    def test_has_blocking_problem_false(self):
        """Test when no blocking problem exists."""
        content = "# Task\nNormal task content"

        result = _has_blocking_problem(content)
        self.assertFalse(result)

    def test_create_initial_task_content(self):
        """Test creating initial task content."""
        user_input = "Fix the login bug"

        content = _create_initial_task_content(user_input)
        self.assertIn("Fix the login bug", content)
        self.assertIn("# Task Description", content)
        self.assertIn("## Initial Request", content)


class TestProcessTaskFile(unittest.TestCase):
    """Test main task file processing."""

    @patch("clud.task._task_file_has_content")
    @patch("clud.task.process_existing_task")
    def test_process_task_file_existing(self, mock_process_existing, mock_has_content):
        """Test processing existing task file."""
        mock_has_content.return_value = True
        mock_process_existing.return_value = 0

        result = process_task_file(Path("task.md"))
        self.assertEqual(result, 0)
        mock_process_existing.assert_called_once()  # type: ignore[misc]

    @patch("clud.task._task_file_has_content")
    @patch("clud.task.process_new_task")
    def test_process_task_file_new(self, mock_process_new, mock_has_content):
        """Test processing new task file."""
        mock_has_content.return_value = False
        mock_process_new.return_value = 0

        result = process_task_file(Path("task.md"))
        self.assertEqual(result, 0)
        mock_process_new.assert_called_once()  # type: ignore[misc]


class TestProcessExistingTask(unittest.TestCase):
    """Test existing task processing."""

    @patch("clud.task.process_new_task")
    def test_process_existing_task_empty_content(self, mock_process_new):
        """Test when task content is empty."""
        mock_process_new.return_value = 0

        with tempfile.TemporaryDirectory() as tmpdir:
            task_path = Path(tmpdir) / "task.md"
            task_path.write_text("", encoding="utf-8")
            result = process_existing_task(task_path)
            self.assertEqual(result, 0)
            mock_process_new.assert_called_once()  # type: ignore[misc]

    @patch("clud.task._wait_for_user_edit")
    @patch("clud.task.open_in_editor")
    @patch("clud.task._execute_task_with_clud")
    def test_process_existing_task_execution_success(self, mock_execute, mock_editor, mock_wait):
        """Test successful task execution."""
        mock_execute.return_value = 0
        mock_editor.return_value = True
        mock_wait.return_value = None

        with tempfile.TemporaryDirectory() as tmpdir:
            task_path = Path(tmpdir) / "task.md"
            task_path.write_text("# Task\nContent", encoding="utf-8")
            result = process_existing_task(task_path)
            self.assertEqual(result, 0)
            mock_editor.assert_called_once_with(task_path)  # type: ignore[misc]
            mock_wait.assert_called_once()  # type: ignore[misc]
            mock_execute.assert_called_once()  # type: ignore[misc]


class TestProcessNewTask(unittest.TestCase):
    """Test new task processing."""

    @patch("clud.task._prompt_for_task_description")
    def test_process_new_task_no_input(self, mock_prompt):
        """Test when user provides no input."""
        mock_prompt.return_value = ""

        with tempfile.TemporaryDirectory() as tmpdir:
            task_path = Path(tmpdir) / "task.md"
            result = process_new_task(task_path)
            self.assertEqual(result, 1)

    @patch("clud.task._prompt_for_task_description")
    @patch("clud.task.process_existing_task")
    def test_process_new_task_success(self, mock_process_existing, mock_prompt):
        """Test successful new task creation."""
        mock_prompt.return_value = "Test task description"
        mock_process_existing.return_value = 0

        with tempfile.TemporaryDirectory() as tmpdir:
            task_path = Path(tmpdir) / "task.md"
            result = process_new_task(task_path)
            self.assertEqual(result, 0)
            self.assertTrue(task_path.exists())
            mock_process_existing.assert_called_once()  # type: ignore[misc]


class TestPromptToCreateTaskFile(unittest.TestCase):
    """Test prompting to create task file."""

    @patch("builtins.input")
    def test_prompt_to_create_yes(self, mock_input):
        """Test user confirms creation with 'y'."""
        mock_input.return_value = "y"
        result = _prompt_to_create_task_file(Path("new_task.md"))
        self.assertTrue(result)

    @patch("builtins.input")
    def test_prompt_to_create_default(self, mock_input):
        """Test user confirms creation with default (empty input)."""
        mock_input.return_value = ""
        result = _prompt_to_create_task_file(Path("new_task.md"))
        self.assertTrue(result)

    @patch("builtins.input")
    def test_prompt_to_create_no(self, mock_input):
        """Test user declines creation with 'n'."""
        mock_input.return_value = "n"
        result = _prompt_to_create_task_file(Path("new_task.md"))
        self.assertFalse(result)

    @patch("builtins.input")
    def test_prompt_to_create_keyboard_interrupt(self, mock_input):
        """Test handling keyboard interrupt."""
        mock_input.side_effect = KeyboardInterrupt()
        result = _prompt_to_create_task_file(Path("new_task.md"))
        self.assertFalse(result)

    @patch("builtins.input")
    def test_prompt_to_create_eof(self, mock_input):
        """Test handling EOF."""
        mock_input.side_effect = EOFError()
        result = _prompt_to_create_task_file(Path("new_task.md"))
        self.assertFalse(result)


class TestHandleTaskCommand(unittest.TestCase):
    """Test main task command handler."""

    def test_handle_task_command_no_path(self):
        """Test when no path is provided."""
        result = handle_task_command("")
        self.assertEqual(result, 2)

    @patch("clud.task.process_task_file")
    def test_handle_task_command_existing_file(self, mock_process):
        """Test handling existing task file."""
        mock_process.return_value = 0

        with tempfile.TemporaryDirectory() as tmpdir:
            task_path = Path(tmpdir) / "task.md"
            task_path.write_text("# Task", encoding="utf-8")
            result = handle_task_command(str(task_path))
            self.assertEqual(result, 0)
            mock_process.assert_called_once()  # type: ignore[misc]

    @patch("clud.task._prompt_to_create_task_file")
    @patch("clud.task.process_task_file")
    def test_handle_task_command_new_file_yes(self, mock_process, mock_prompt):
        """Test handling new task file when user confirms creation."""
        mock_prompt.return_value = True
        mock_process.return_value = 0

        with tempfile.TemporaryDirectory() as tmpdir:
            task_path = Path(tmpdir) / "new_task.md"
            result = handle_task_command(str(task_path))
            self.assertEqual(result, 0)
            mock_prompt.assert_called_once()  # type: ignore[misc]
            mock_process.assert_called_once()  # type: ignore[misc]

    @patch("clud.task._prompt_to_create_task_file")
    @patch("clud.task.process_task_file")
    def test_handle_task_command_new_file_no(self, mock_process, mock_prompt):
        """Test handling new task file when user declines creation."""
        mock_prompt.return_value = False

        with tempfile.TemporaryDirectory() as tmpdir:
            task_path = Path(tmpdir) / "new_task.md"
            result = handle_task_command(str(task_path))
            self.assertEqual(result, 0)
            mock_prompt.assert_called_once()  # type: ignore[misc]
            mock_process.assert_not_called()  # type: ignore[misc]

    @patch("clud.task.process_task_file")
    def test_handle_task_command_exception(self, mock_process):
        """Test handling exceptions."""
        mock_process.side_effect = Exception("Test error")

        with tempfile.TemporaryDirectory() as tmpdir:
            task_path = Path(tmpdir) / "task.md"
            task_path.write_text("# Task", encoding="utf-8")
            result = handle_task_command(str(task_path))
            self.assertEqual(result, 1)


if __name__ == "__main__":
    unittest.main()
