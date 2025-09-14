#!/usr/bin/env -S uv run python
"""Integration test for Docker build process."""

import os
import subprocess
import sys
from pathlib import Path


class DockerBuildError(Exception):
    """Exception raised when Docker build fails."""

    pass


def run_docker_build():
    """Build the Docker image and capture output."""
    project_root = Path(__file__).parent.parent.parent
    os.chdir(project_root)

    print(f"Building Docker image from: {project_root}")
    print("=" * 60)

    # Run docker build with plain text output
    cmd = ["docker", "build", "-t", "clud-dev:latest", "--progress=plain", "."]

    try:
        subprocess.run(
            cmd,
            check=True,
            text=True,
            capture_output=False,  # Let output go directly to console
            timeout=1200,  # 20 minute timeout
        )

        print("=" * 60)
        print("[SUCCESS] Docker build completed successfully!")
        return True

    except subprocess.CalledProcessError as e:
        print("=" * 60)
        print(f"[ERROR] Docker build failed with exit code: {e.returncode}")
        raise DockerBuildError(f"Docker build failed with exit code {e.returncode}") from e

    except subprocess.TimeoutExpired as e:
        print("=" * 60)
        print("[ERROR] Docker build timed out after 10 minutes")
        raise DockerBuildError("Docker build timed out") from e

    except FileNotFoundError as e:
        print("[ERROR] Docker command not found. Is Docker installed?")
        raise DockerBuildError("Docker command not found") from e


def main():
    """Main test function."""
    print("Starting Docker build integration test...")

    try:
        run_docker_build()
        print("\n[SUCCESS] All tests passed! Docker image built successfully.")
        return 0

    except DockerBuildError as e:
        print(f"\n[FAILED] Test failed: {e}")
        return 1

    except Exception as e:
        print(f"\n[ERROR] Unexpected error: {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
