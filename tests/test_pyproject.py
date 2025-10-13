"""Tests for pyproject.toml configuration."""

from pathlib import Path

import tomllib


def test_minimal_python_version() -> None:
    """Test that the minimal Python version requirement is 3.10."""
    pyproject_path = Path(__file__).parent.parent / "pyproject.toml"

    with open(pyproject_path, "rb") as f:
        pyproject_data = tomllib.load(f)

    requires_python = pyproject_data["project"]["requires-python"]

    # Assert that the requirement is >=3.10
    assert requires_python == ">=3.10", f"Expected minimal Python version to be >=3.10, got {requires_python}"
