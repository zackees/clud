#!/usr/bin/env -S uv run python
"""Integration test for Docker build process."""

import sys
from pathlib import Path

# Add tests directory to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent))

from docker_test_utils import ensure_test_image


class DockerBuildError(Exception):
    """Exception raised when Docker build fails."""

    pass


def test_docker_build():
    """Test that Docker image can be built successfully."""
    print("Testing Docker build process...")

    try:
        # Use shared image building logic
        image_name = ensure_test_image()
        print(f"[SUCCESS] Docker image ready: {image_name}")
        return True

    except Exception as e:
        print(f"[ERROR] Docker build test failed: {e}")
        raise DockerBuildError(f"Docker build failed: {e}") from e


def main():
    """Main test function."""
    print("Starting Docker build integration test...")

    try:
        test_docker_build()
        print("\n[SUCCESS] Docker build test passed!")
        return 0

    except DockerBuildError as e:
        print(f"\n[FAILED] Test failed: {e}")
        return 1

    except Exception as e:
        print(f"\n[ERROR] Unexpected error: {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
