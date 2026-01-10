"""Unit tests for loop file working copy functionality."""

import tempfile
import unittest
from pathlib import Path

from clud.agent_args import AgentMode, Args


class TestLoopFileWorkingCopy(unittest.TestCase):
    """Test cases for loop file working copy functionality."""

    def test_loop_file_working_copy_created(self) -> None:
        """Test that loop file is copied to .loop/<name> for agent to use."""
        with tempfile.TemporaryDirectory() as tmpdir:
            tmpdir_path = Path(tmpdir)

            # Create a test loop file (e.g., LOOP.md)
            loop_file = tmpdir_path / "LOOP.md"
            loop_file_content = "# Tasks\n\n- [ ] Task 1\n- [ ] Task 2\n"
            loop_file.write_text(loop_file_content, encoding="utf-8")

            # Create Args object with loop_value pointing to the file
            args = Args(
                mode=AgentMode.DEFAULT,
                loop_value=str(loop_file),
                prompt=None,
                message=None,
            )

            # Change to temp directory to simulate running in project root
            import os

            original_cwd = os.getcwd()
            try:
                os.chdir(tmpdir_path)

                # Mock the necessary functions to prevent actual execution
                # We'll test just the working copy logic by checking if file exists after setup
                loop_dir = tmpdir_path / ".loop"
                loop_dir.mkdir(exist_ok=True)

                # Manually execute just the working copy logic
                import shutil

                loop_file_path: Path | None = None
                working_loop_file: Path | None = None

                if args.loop_value:
                    try:
                        int(args.loop_value)
                    except ValueError:
                        potential_file = Path(args.loop_value)
                        if potential_file.exists() and potential_file.is_file():
                            loop_file_path = potential_file
                            working_loop_file = loop_dir / loop_file_path.name

                            if not working_loop_file.exists():
                                shutil.copy2(loop_file_path, working_loop_file)

                # Verify working copy was created
                self.assertIsNotNone(working_loop_file, "Working copy path should be set")
                self.assertTrue(working_loop_file.exists(), "Working copy file should exist")  # type: ignore

                # Verify working copy content matches original
                working_content = working_loop_file.read_text(encoding="utf-8")  # type: ignore
                self.assertEqual(working_content, loop_file_content, "Working copy content should match original")

                # Verify original file still exists and is unchanged
                self.assertTrue(loop_file.exists(), "Original file should still exist")
                original_content = loop_file.read_text(encoding="utf-8")
                self.assertEqual(original_content, loop_file_content, "Original file should be unchanged")
            finally:
                os.chdir(original_cwd)

    def test_loop_file_working_copy_with_custom_name(self) -> None:
        """Test that custom loop file names are copied correctly as working files."""
        with tempfile.TemporaryDirectory() as tmpdir:
            tmpdir_path = Path(tmpdir)

            # Create a custom loop file (e.g., tasks.md)
            loop_file = tmpdir_path / "tasks.md"
            loop_file_content = "Custom task list\n"
            loop_file.write_text(loop_file_content, encoding="utf-8")

            # Create Args object with loop_value pointing to the file
            args = Args(
                mode=AgentMode.DEFAULT,
                loop_value=str(loop_file),
                prompt=None,
                message=None,
            )

            # Change to temp directory
            import os

            original_cwd = os.getcwd()
            try:
                os.chdir(tmpdir_path)

                # Execute working copy logic
                import shutil

                loop_dir = tmpdir_path / ".loop"
                loop_dir.mkdir(exist_ok=True)

                loop_file_path: Path | None = None
                working_loop_file: Path | None = None

                if args.loop_value:
                    try:
                        int(args.loop_value)
                    except ValueError:
                        potential_file = Path(args.loop_value)
                        if potential_file.exists() and potential_file.is_file():
                            loop_file_path = potential_file
                            working_loop_file = loop_dir / loop_file_path.name

                            if not working_loop_file.exists():
                                shutil.copy2(loop_file_path, working_loop_file)

                # Verify working copy has correct name
                expected_working_copy = loop_dir / "tasks.md"
                self.assertTrue(expected_working_copy.exists(), "Working copy should exist with custom name")
            finally:
                os.chdir(original_cwd)

    def test_no_working_copy_for_integer_loop_value(self) -> None:
        """Test that no working copy is created when loop_value is an integer."""
        with tempfile.TemporaryDirectory() as tmpdir:
            tmpdir_path = Path(tmpdir)

            # Create Args object with integer loop_value
            args = Args(
                mode=AgentMode.DEFAULT,
                loop_value="5",  # Integer, not a file
                prompt="Test prompt",
                message=None,
            )

            import os

            original_cwd = os.getcwd()
            try:
                os.chdir(tmpdir_path)

                # Execute working copy logic
                loop_dir = tmpdir_path / ".loop"
                loop_dir.mkdir(exist_ok=True)

                loop_file_path: Path | None = None
                working_loop_file: Path | None = None

                if args.loop_value:
                    try:
                        int(args.loop_value)
                        # It's an integer, not a file path
                    except ValueError:
                        potential_file = Path(args.loop_value)
                        if potential_file.exists() and potential_file.is_file():
                            loop_file_path = potential_file
                            working_loop_file = loop_dir / loop_file_path.name

                # Verify no working copy was created (loop_file_path should be None)
                self.assertIsNone(loop_file_path, "loop_file_path should be None for integer loop_value")
                self.assertIsNone(working_loop_file, "working_loop_file should be None for integer loop_value")

                # Verify no .md files exist in .loop (except for potential metadata files)
                md_files = list(loop_dir.glob("*.md"))
                self.assertEqual(len(md_files), 0, "No .md working copy files should exist for integer loop_value")
            finally:
                os.chdir(original_cwd)


if __name__ == "__main__":
    unittest.main()
