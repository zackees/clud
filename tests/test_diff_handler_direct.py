"""Direct test of DiffHandler path normalization."""

import logging
import unittest
from pathlib import Path

from clud.webui.api import DiffHandler

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


class TestDiffHandlerDirect(unittest.TestCase):
    """Test DiffHandler directly."""

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
