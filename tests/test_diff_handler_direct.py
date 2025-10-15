"""Direct test of DiffHandler path normalization."""

import logging
import subprocess
import unittest
from pathlib import Path

from clud.webui.api import DiffHandler

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


class TestDiffHandlerDirect(unittest.TestCase):
    """Test DiffHandler directly."""

    @classmethod
    def setUpClass(cls) -> None:
        """Create test project directory if it doesn't exist."""
        test_project_dir = Path(__file__).parent / "artifacts" / "test_diff_project"
        test_project_dir.mkdir(parents=True, exist_ok=True)

        # Initialize git repo if not already initialized
        if not (test_project_dir / ".git").exists():
            subprocess.run(
                ["git", "init"],
                cwd=str(test_project_dir),
                check=True,
                capture_output=True,
            )
            subprocess.run(
                ["git", "config", "user.email", "test@example.com"],
                cwd=str(test_project_dir),
                check=True,
                capture_output=True,
            )
            subprocess.run(
                ["git", "config", "user.name", "Test User"],
                cwd=str(test_project_dir),
                check=True,
                capture_output=True,
            )

        # Ensure README.md exists and is committed
        readme_path = test_project_dir / "README.md"
        if not readme_path.exists():
            readme_path.write_text("# Test Project\n\nThis is a test project.\n", encoding="utf-8")
            subprocess.run(
                ["git", "add", "README.md"],
                cwd=str(test_project_dir),
                check=True,
                capture_output=True,
            )
            subprocess.run(
                ["git", "commit", "-m", "Initial commit"],
                cwd=str(test_project_dir),
                check=True,
                capture_output=True,
            )

    def setUp(self) -> None:
        """Ensure there are changes to detect before each test."""
        test_project_dir = Path(__file__).parent / "artifacts" / "test_diff_project"
        readme_path = test_project_dir / "README.md"

        # Reset to committed state first (ignore errors if already clean)
        subprocess.run(
            ["git", "checkout", "README.md"],
            cwd=str(test_project_dir),
            capture_output=True,
        )

        # Make a modification to create a diff
        readme_path.write_text("# Test Project\n\nThis is a test project.\n\nModified for testing.\n", encoding="utf-8")

    def test_path_normalization(self) -> None:
        """Test that path normalization works correctly."""
        handler = DiffHandler()

        test_project_dir = Path(__file__).parent / "artifacts" / "test_diff_project"
        self.assertTrue(test_project_dir.exists(), f"Test project dir should exist: {test_project_dir}")

        # Scan for changes
        logger.info("Scanning for changes in %s", test_project_dir)
        count = handler.scan_git_changes(str(test_project_dir))
        logger.info("Scan found %d changed files", count)
        self.assertGreater(count, 0, "Should find at least one changed file")

        # Try to get tree with different path formats
        test_paths = [
            str(test_project_dir),
            str(test_project_dir.resolve()),
            str(test_project_dir.absolute()),
        ]

        for path_str in test_paths:
            logger.info("Testing path: %s", path_str)

            # All path formats should work
            tree_data = handler.get_diff_tree(path_str)
            logger.info("  Tree data keys: %s", list(tree_data.keys()))
            self.assertIn("files", tree_data, "Tree should have 'files' key")
            files = tree_data.get("files", [])
            logger.info("  Files: %s", files)
            self.assertIsInstance(files, list, "Files should be a list")
            self.assertGreater(len(files), 0, f"Should have files for path: {path_str}")


if __name__ == "__main__":
    unittest.main()
