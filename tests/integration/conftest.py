"""Pytest configuration for integration tests."""

import contextlib
import subprocess
import time
import uuid
from pathlib import Path

import pytest

from clud.testing.docker_test_utils import ensure_test_image


@pytest.fixture(scope="session")
def shared_test_container():
    """Create a single long-running container for all integration tests.

    This dramatically speeds up tests by reusing the same container
    instead of creating/destroying a new one for each test.
    """
    # Ensure image is built
    image_name = ensure_test_image()

    # Create unique container name for this test session
    container_name = f"clud-integration-shared-{uuid.uuid4().hex[:8]}"
    project_root = Path(__file__).parent.parent.parent

    print(f"\n[Setup] Starting shared test container: {container_name}")

    # Clean up any existing container with this name
    with contextlib.suppress(BaseException):
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    try:
        # Start container in detached mode with long sleep
        run_cmd = [
            "docker", "run",
            "-d",
            "--name", container_name,
            "-v", f"{project_root}:/host:rw",
            "-v", f"{project_root}:/home/coder/project:rw",
            image_name,
            "sleep", "3600"  # Keep alive for 1 hour
        ]

        result = subprocess.run(
            run_cmd,
            check=True,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace"
        )

        # Wait for container to be ready
        time.sleep(2)

        # Verify container is running
        check_cmd = ["docker", "ps", "-q", "-f", f"name={container_name}"]
        check_result = subprocess.run(
            check_cmd,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace"
        )

        if not check_result.stdout.strip():
            raise RuntimeError(f"Container {container_name} failed to start")

        print(f"[Setup] Shared container ready: {container_name}")

        # Yield container info to tests
        yield {
            "name": container_name,
            "image": image_name,
            "project_root": project_root,
        }

    finally:
        # Cleanup: destroy container after all tests
        print(f"\n[Teardown] Removing shared test container: {container_name}")
        subprocess.run(
            ["docker", "rm", "-f", container_name],
            capture_output=True,
            check=False
        )


@pytest.fixture(scope="function")
def clean_container_workspace(shared_test_container):
    """Clean the container workspace before each test.

    This ensures test isolation without recreating containers.
    """
    container_name = shared_test_container["name"]

    # Clean /workspace directory if it exists
    cleanup_cmd = ["docker", "exec", container_name, "sh", "-c", "rm -rf /workspace/* /workspace/.[!.]* 2>/dev/null || true"]
    subprocess.run(cleanup_cmd, capture_output=True, check=False)

    yield shared_test_container
