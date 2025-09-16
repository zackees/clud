#!/usr/bin/env -S uv run python
"""Integration test for Claude Docker CLI exit functionality."""

import contextlib
import subprocess
import sys
import time
import uuid
from pathlib import Path

from clud.testing.docker_test_utils import ensure_test_image


class DockerCliExitError(Exception):
    """Exception raised when Docker CLI exit test fails."""

    pass


def test_workspace_sync_verification():
    """Test that workspace sync works by checking pyproject.toml contains 'clud'."""
    project_root = Path(__file__).parent.parent.parent

    print("Testing workspace sync functionality...")
    print(f"Project root: {project_root}")
    print("=" * 60)

    # Use shared image building logic
    image_name = ensure_test_image()
    print(f"Using Docker image: {image_name}")

    # Run container with command to check pyproject.toml contains 'clud'
    print("\nTesting workspace sync with container command...")
    container_name = f"clud-test-sync-{uuid.uuid4().hex[:8]}"

    # Remove existing container if it exists
    with contextlib.suppress(BaseException):
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    # Run container with command to cat pyproject.toml and check for 'clud'
    run_cmd = ["docker", "run", "--name", container_name, "-v", f"{project_root}:/host:rw", image_name, "--cmd", "cat pyproject.toml && exit 0"]

    try:
        result = subprocess.run(run_cmd, check=True, capture_output=True, text=True, timeout=60)
        output = result.stdout
        print(f"Container output:\n{output}")

        # Check that 'clud' appears in the output (from pyproject.toml)
        if "clud" not in output:
            raise DockerCliExitError(f"'clud' not found in pyproject.toml output. Workspace sync failed. Output: {output}")

        print("OK Workspace sync verification successful - 'clud' found in pyproject.toml")

        # Verify container exited cleanly
        inspect_cmd = ["docker", "inspect", container_name, "--format", "{{.State.ExitCode}}"]
        inspect_result = subprocess.run(inspect_cmd, capture_output=True, text=True, check=True)

        exit_code = inspect_result.stdout.strip()
        if exit_code != "0":
            raise DockerCliExitError(f"Container exited with non-zero code: {exit_code}")

        print("OK Container exited cleanly")

    except subprocess.CalledProcessError as e:
        print(f"Command failed: {e}")
        if e.stderr:
            print(f"Error output: {e.stderr}")
        # Get container logs for debugging
        try:
            logs_cmd = ["docker", "logs", container_name]
            logs_result = subprocess.run(logs_cmd, capture_output=True, text=True)
            print(f"Container logs:\n{logs_result.stdout}\n{logs_result.stderr}")
        except Exception:
            pass
        raise DockerCliExitError(f"Workspace sync test failed: {e}") from e

    except subprocess.TimeoutExpired as e:
        # Cleanup on timeout
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)
        raise DockerCliExitError("Workspace sync test timed out") from e

    finally:
        # Cleanup
        try:
            subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)
            print("OK Container cleanup completed")
        except Exception:
            pass


def test_docker_container_exit():
    """Test that Claude can be exited properly from the Docker CLI."""
    project_root = Path(__file__).parent.parent.parent

    print("Testing Docker container exit functionality...")
    print(f"Project root: {project_root}")
    print("=" * 60)

    # Use shared image building logic
    image_name = ensure_test_image()
    print(f"Using Docker image: {image_name}")

    # Start container in detached mode
    print("\nStarting Docker container...")
    container_name = f"clud-test-exit-{uuid.uuid4().hex[:8]}"

    # Remove existing container if it exists
    with contextlib.suppress(BaseException):
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    run_cmd = ["docker", "run", "-d", "--name", container_name, "-v", f"{project_root}:/home/coder/project", image_name]

    try:
        result = subprocess.run(run_cmd, check=True, capture_output=True, text=True)
        container_id = result.stdout.strip()
        print(f"OK Container started: {container_id[:12]}")

        # Wait for container to fully start
        time.sleep(5)

        # Check if container is running
        check_cmd = ["docker", "ps", "-q", "-f", f"name={container_name}"]
        check_result = subprocess.run(check_cmd, capture_output=True, text=True)

        if not check_result.stdout.strip():
            # Get container logs to see what happened
            logs_cmd = ["docker", "logs", container_name]
            logs_result = subprocess.run(logs_cmd, capture_output=True, text=True)
            print(f"Container logs:\n{logs_result.stdout}\n{logs_result.stderr}")
            raise DockerCliExitError("Container is not running")

        print("OK Container is running")

        # Test graceful exit with docker stop
        print("\nTesting graceful exit with docker stop...")
        stop_cmd = ["docker", "stop", "-t", "10", container_name]
        subprocess.run(stop_cmd, check=True, capture_output=True, text=True, timeout=15)
        print("OK Container stopped gracefully")

        # Verify container is stopped
        check_result = subprocess.run(check_cmd, capture_output=True, text=True)
        if check_result.stdout.strip():
            raise DockerCliExitError("Container still running after stop command")

        print("OK Container exit verified")

        # Test that the container can be restarted
        print("\nTesting container restart...")
        restart_cmd = ["docker", "start", container_name]
        subprocess.run(restart_cmd, check=True, capture_output=True)

        time.sleep(3)

        # Check if restarted container is running
        check_result = subprocess.run(check_cmd, capture_output=True, text=True)
        if not check_result.stdout.strip():
            raise DockerCliExitError("Container failed to restart")

        print("OK Container restarted successfully")

        # Final cleanup - force stop and remove
        subprocess.run(["docker", "stop", container_name], capture_output=True, check=False)
        subprocess.run(["docker", "rm", container_name], capture_output=True, check=False)

        print("OK Container cleanup completed")

    except subprocess.CalledProcessError as e:
        print(f"Command failed: {e}")
        if e.stderr:
            print(f"Error output: {e.stderr}")
        raise DockerCliExitError(f"Docker command failed: {e}") from e

    except subprocess.TimeoutExpired as e:
        # Cleanup on timeout
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)
        raise DockerCliExitError("Docker command timed out") from e


def main():
    """Main test function."""
    print("Starting Docker CLI exit integration tests...")

    # Check Docker availability
    try:
        subprocess.run(["docker", "version"], capture_output=True, check=True, timeout=10)
        print("OK Docker is available")
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, FileNotFoundError):
        print("X Docker is not available")
        return 1

    try:
        # Test workspace sync verification first
        test_workspace_sync_verification()
        print("\nOK Workspace sync verification test passed")

        # Test basic container exit
        test_docker_container_exit()
        print("\nOK Docker container exit test passed")

        print("\n" + "=" * 60)
        print("SUCCESS: All Docker CLI exit tests passed!")
        return 0

    except DockerCliExitError as e:
        print(f"\nFAILED: {e}")
        return 1

    except Exception as e:
        print(f"\nERROR: Unexpected error: {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
