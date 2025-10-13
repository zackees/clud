"""Tests for pyproject.toml configuration."""

import unittest
from pathlib import Path

import tomllib


class TestPyprojectConfig(unittest.TestCase):
    """Test pyproject.toml configuration."""

    def test_minimal_python_version(self) -> None:
        """Test that the minimal Python version requirement is 3.10."""
        pyproject_path = Path(__file__).parent.parent / "pyproject.toml"

        with open(pyproject_path, "rb") as f:
            pyproject_data = tomllib.load(f)

        requires_python = pyproject_data["project"]["requires-python"]

        # Assert that the requirement is >=3.10
        self.assertEqual(requires_python, ">=3.10", f"Expected minimal Python version to be >=3.10, got {requires_python}")


if __name__ == "__main__":
    unittest.main()
