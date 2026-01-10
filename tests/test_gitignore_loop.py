"""Unit tests for .gitignore loop directory handling."""

import tempfile
import unittest
from pathlib import Path

from clud.agent.loop_executor import _ensure_loop_in_gitignore


class TestGitignoreLoop(unittest.TestCase):
    """Test cases for _ensure_loop_in_gitignore function."""

    def test_no_git_directory(self) -> None:
        """Test that function returns early if .git directory doesn't exist."""
        with tempfile.TemporaryDirectory() as tmpdir:
            # Create .gitignore but no .git directory
            gitignore = Path(tmpdir) / ".gitignore"
            gitignore.write_text("*.pyc\n", encoding="utf-8")

            # Change to temp directory
            import os

            old_cwd = os.getcwd()
            try:
                os.chdir(tmpdir)
                _ensure_loop_in_gitignore()

                # .gitignore should remain unchanged (no .loop added)
                content = gitignore.read_text(encoding="utf-8")
                self.assertEqual(content, "*.pyc\n")
            finally:
                os.chdir(old_cwd)

    def test_no_gitignore_file(self) -> None:
        """Test that function returns early if .gitignore doesn't exist."""
        with tempfile.TemporaryDirectory() as tmpdir:
            # Create .git directory but no .gitignore
            git_dir = Path(tmpdir) / ".git"
            git_dir.mkdir()

            # Change to temp directory
            import os

            old_cwd = os.getcwd()
            try:
                os.chdir(tmpdir)
                _ensure_loop_in_gitignore()

                # .gitignore should not be created
                gitignore = Path(tmpdir) / ".gitignore"
                self.assertFalse(gitignore.exists())
            finally:
                os.chdir(old_cwd)

    def test_loop_already_in_gitignore(self) -> None:
        """Test that .loop is not added if already present."""
        with tempfile.TemporaryDirectory() as tmpdir:
            # Create .git directory
            git_dir = Path(tmpdir) / ".git"
            git_dir.mkdir()

            # Create .gitignore with .loop already present
            gitignore = Path(tmpdir) / ".gitignore"
            original_content = "*.pyc\n.loop\n__pycache__\n"
            gitignore.write_text(original_content, encoding="utf-8")

            # Change to temp directory
            import os

            old_cwd = os.getcwd()
            try:
                os.chdir(tmpdir)
                _ensure_loop_in_gitignore()

                # .gitignore should remain unchanged
                content = gitignore.read_text(encoding="utf-8")
                self.assertEqual(content, original_content)
            finally:
                os.chdir(old_cwd)

    def test_dot_slash_loop_already_in_gitignore(self) -> None:
        """Test that .loop is not added if ./.loop is already present."""
        with tempfile.TemporaryDirectory() as tmpdir:
            # Create .git directory
            git_dir = Path(tmpdir) / ".git"
            git_dir.mkdir()

            # Create .gitignore with ./.loop already present
            gitignore = Path(tmpdir) / ".gitignore"
            original_content = "*.pyc\n./.loop\n__pycache__\n"
            gitignore.write_text(original_content, encoding="utf-8")

            # Change to temp directory
            import os

            old_cwd = os.getcwd()
            try:
                os.chdir(tmpdir)
                _ensure_loop_in_gitignore()

                # .gitignore should remain unchanged
                content = gitignore.read_text(encoding="utf-8")
                self.assertEqual(content, original_content)
            finally:
                os.chdir(old_cwd)

    def test_loop_added_to_gitignore(self) -> None:
        """Test that .loop is added to .gitignore if missing."""
        with tempfile.TemporaryDirectory() as tmpdir:
            # Create .git directory
            git_dir = Path(tmpdir) / ".git"
            git_dir.mkdir()

            # Create .gitignore without .loop
            gitignore = Path(tmpdir) / ".gitignore"
            original_content = "*.pyc\n__pycache__\n"
            gitignore.write_text(original_content, encoding="utf-8")

            # Change to temp directory
            import os

            old_cwd = os.getcwd()
            try:
                os.chdir(tmpdir)
                _ensure_loop_in_gitignore()

                # .gitignore should have .loop added
                content = gitignore.read_text(encoding="utf-8")
                self.assertEqual(content, "*.pyc\n__pycache__\n.loop\n")
            finally:
                os.chdir(old_cwd)

    def test_loop_added_with_missing_final_newline(self) -> None:
        """Test that .loop is added correctly when .gitignore lacks final newline."""
        with tempfile.TemporaryDirectory() as tmpdir:
            # Create .git directory
            git_dir = Path(tmpdir) / ".git"
            git_dir.mkdir()

            # Create .gitignore without final newline
            gitignore = Path(tmpdir) / ".gitignore"
            original_content = "*.pyc\n__pycache__"
            gitignore.write_text(original_content, encoding="utf-8")

            # Change to temp directory
            import os

            old_cwd = os.getcwd()
            try:
                os.chdir(tmpdir)
                _ensure_loop_in_gitignore()

                # .gitignore should have .loop added with proper newlines
                content = gitignore.read_text(encoding="utf-8")
                self.assertEqual(content, "*.pyc\n__pycache__\n.loop\n")
            finally:
                os.chdir(old_cwd)

    def test_slash_loop_already_in_gitignore(self) -> None:
        """Test that .loop is not added if /.loop is already present."""
        with tempfile.TemporaryDirectory() as tmpdir:
            # Create .git directory
            git_dir = Path(tmpdir) / ".git"
            git_dir.mkdir()

            # Create .gitignore with /.loop already present
            gitignore = Path(tmpdir) / ".gitignore"
            original_content = "*.pyc\n/.loop\n__pycache__\n"
            gitignore.write_text(original_content, encoding="utf-8")

            # Change to temp directory
            import os

            old_cwd = os.getcwd()
            try:
                os.chdir(tmpdir)
                _ensure_loop_in_gitignore()

                # .gitignore should remain unchanged
                content = gitignore.read_text(encoding="utf-8")
                self.assertEqual(content, original_content)
            finally:
                os.chdir(old_cwd)


if __name__ == "__main__":
    unittest.main()
