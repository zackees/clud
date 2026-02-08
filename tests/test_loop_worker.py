"""Unit tests for loop worker functionality."""

import tempfile
import unittest
from pathlib import Path
from unittest.mock import MagicMock

from clud.agent_args import Args
from clud.loop_tui.loop_worker import LoopWorkerApp


class TestLoopWorkerApp(unittest.TestCase):
    """Test cases for LoopWorkerApp class."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        # Create temporary directory for test files
        self.tmpdir = tempfile.mkdtemp()
        self.update_file = Path(self.tmpdir) / "UPDATE.md"
        self.update_file.write_text("test content")

        # Create mock Args object
        self.args = MagicMock(spec=Args)
        self.args.model = "sonnet"
        self.args.output_format = "stream-json"
        self.args.yolo = 10
        self.args.code_editor_cmd = None
        self.args.plain_mode = False
        self.args.prompt = "Test prompt"

        self.claude_path = "/path/to/claude"
        self.loop_count = 5

    def tearDown(self) -> None:
        """Clean up test fixtures."""
        import shutil

        shutil.rmtree(self.tmpdir, ignore_errors=True)

    def test_initialization(self) -> None:
        """Test that LoopWorkerApp initializes correctly."""
        app = LoopWorkerApp(
            args=self.args,
            claude_path=self.claude_path,
            loop_count=self.loop_count,
            update_file=self.update_file,
        )

        self.assertEqual(app.args, self.args)
        self.assertEqual(app.claude_path, self.claude_path)
        self.assertEqual(app.loop_count, self.loop_count)
        self.assertEqual(app.update_file, self.update_file)
        self.assertFalse(app._halt_requested)
        self.assertEqual(app._exit_code, 0)

    def test_initialization_callbacks(self) -> None:
        """Test that LoopWorkerApp initializes with proper callbacks."""
        app = LoopWorkerApp(
            args=self.args,
            claude_path=self.claude_path,
            loop_count=self.loop_count,
            update_file=self.update_file,
        )

        # All callbacks should be set (exit kills subprocess + halts worker)
        self.assertIsNotNone(app.on_halt_callback)
        self.assertIsNotNone(app.on_edit_callback)
        self.assertIsNotNone(app.on_exit_callback)

    def test_halt_request_callback(self) -> None:
        """Test that halt callback sets halt flag."""
        app = LoopWorkerApp(
            args=self.args,
            claude_path=self.claude_path,
            loop_count=self.loop_count,
            update_file=self.update_file,
        )

        # Mock log_message to avoid Textual query issues
        app.log_message = MagicMock()

        # Initially not halted
        self.assertFalse(app._halt_requested)

        # Trigger halt callback
        if app.on_halt_callback:
            app.on_halt_callback()

        # Should now be halted
        self.assertTrue(app._halt_requested)
        app.log_message.assert_called_once_with("> Loop halt requested by user")

    def test_edit_callback(self) -> None:
        """Test that edit callback logs message."""
        app = LoopWorkerApp(
            args=self.args,
            claude_path=self.claude_path,
            loop_count=self.loop_count,
            update_file=self.update_file,
        )

        # Mock log_message to avoid Textual query issues
        app.log_message = MagicMock()

        # Trigger edit callback
        if app.on_edit_callback:
            app.on_edit_callback()

        # Should log message
        app.log_message.assert_called_once_with("> UPDATE.md edit completed")

    def test_exit_code_initial_value(self) -> None:
        """Test that exit code starts at 0."""
        app = LoopWorkerApp(
            args=self.args,
            claude_path=self.claude_path,
            loop_count=self.loop_count,
            update_file=self.update_file,
        )

        self.assertEqual(app._exit_code, 0)

    def test_exit_code_can_be_set(self) -> None:
        """Test that exit code can be changed."""
        app = LoopWorkerApp(
            args=self.args,
            claude_path=self.claude_path,
            loop_count=self.loop_count,
            update_file=self.update_file,
        )

        # Change exit code
        app._exit_code = 1
        self.assertEqual(app._exit_code, 1)

        app._exit_code = 130
        self.assertEqual(app._exit_code, 130)

    def test_exit_with_code_method(self) -> None:
        """Test that _exit_with_code sets exit code and exits."""
        app = LoopWorkerApp(
            args=self.args,
            claude_path=self.claude_path,
            loop_count=self.loop_count,
            update_file=self.update_file,
        )

        # Mock exit method
        app.exit = MagicMock()

        # Call _exit_with_code
        app._exit_with_code(42)

        # Should set exit code and call exit
        self.assertEqual(app._exit_code, 42)
        app.exit.assert_called_once()

    def test_inherits_from_clud_loop_tui(self) -> None:
        """Test that LoopWorkerApp inherits from CludLoopTUI."""
        from clud.loop_tui.app import CludLoopTUI

        app = LoopWorkerApp(
            args=self.args,
            claude_path=self.claude_path,
            loop_count=self.loop_count,
            update_file=self.update_file,
        )

        self.assertIsInstance(app, CludLoopTUI)

    def test_has_run_loop_worker_method(self) -> None:
        """Test that LoopWorkerApp has run_loop_worker method."""
        app = LoopWorkerApp(
            args=self.args,
            claude_path=self.claude_path,
            loop_count=self.loop_count,
            update_file=self.update_file,
        )

        self.assertTrue(hasattr(app, "run_loop_worker"))
        self.assertTrue(callable(app.run_loop_worker))

    def test_menu_inheritance(self) -> None:
        """Test that LoopWorkerApp inherits menu structure."""
        app = LoopWorkerApp(
            args=self.args,
            claude_path=self.claude_path,
            loop_count=self.loop_count,
            update_file=self.update_file,
        )

        # Should inherit menu constants from parent
        self.assertEqual(app.MAIN_MENU, ["Options", "Exit"])
        self.assertEqual(app.OPTIONS_MENU, ["<- Back", "Edit UPDATE.md", "Copy All", "Halt", "Help"])

    def test_halt_flag_persists(self) -> None:
        """Test that halt flag persists after being set."""
        app = LoopWorkerApp(
            args=self.args,
            claude_path=self.claude_path,
            loop_count=self.loop_count,
            update_file=self.update_file,
        )

        # Mock log_message
        app.log_message = MagicMock()

        # Set halt flag multiple times
        if app.on_halt_callback:
            app.on_halt_callback()
            app.on_halt_callback()
            app.on_halt_callback()

        # Should still be halted
        self.assertTrue(app._halt_requested)

    def test_args_stored_correctly(self) -> None:
        """Test that args parameter is stored correctly."""
        app = LoopWorkerApp(
            args=self.args,
            claude_path=self.claude_path,
            loop_count=self.loop_count,
            update_file=self.update_file,
        )

        # Args should be stored as instance variable
        self.assertEqual(app.args, self.args)

    def test_claude_path_stored_correctly(self) -> None:
        """Test that claude_path parameter is stored correctly."""
        app = LoopWorkerApp(
            args=self.args,
            claude_path=self.claude_path,
            loop_count=self.loop_count,
            update_file=self.update_file,
        )

        self.assertEqual(app.claude_path, self.claude_path)

    def test_loop_count_stored_correctly(self) -> None:
        """Test that loop_count parameter is stored correctly."""
        app = LoopWorkerApp(
            args=self.args,
            claude_path=self.claude_path,
            loop_count=self.loop_count,
            update_file=self.update_file,
        )

        self.assertEqual(app.loop_count, self.loop_count)


if __name__ == "__main__":
    unittest.main()
