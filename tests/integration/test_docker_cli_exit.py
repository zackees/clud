#!/usr/bin/env -S uv run python
"""Integration test for Claude Docker CLI exit functionality."""

import contextlib
import os
import subprocess
import sys
import time
from pathlib import Path


class DockerCliExitError(Exception):
    """Exception raised when Docker CLI exit test fails."""

    pass


def test_docker_container_exit():
    """Test that Claude can be exited properly from the Docker CLI."""
    project_root = Path(__file__).parent.parent.parent

    print("Testing Docker container exit functionality...")
    print(f"Project root: {project_root}")
    print("=" * 60)

    # Build the Docker image first
    print("Building Docker image...")
    build_cmd = ["docker", "build", "-t", "clud-dev:latest", str(project_root)]

    try:
        subprocess.run(build_cmd, check=True, timeout=600)
        print("OK Docker image built successfully")
    except subprocess.CalledProcessError as e:
        raise DockerCliExitError(f"Failed to build Docker image: {e}") from e
    except subprocess.TimeoutExpired as e:
        raise DockerCliExitError("Docker build timed out") from e

    # Start container in detached mode
    print("\nStarting Docker container...")
    container_name = "clud-test-exit"

    # Remove existing container if it exists
    with contextlib.suppress(BaseException):
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    run_cmd = ["docker", "run", "-d", "--name", container_name, "-v", f"{project_root}:/home/coder/project", "clud-dev:latest"]

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


def test_docker_compose_exit():
    """Test that Docker Compose services can be properly stopped."""
    project_root = Path(__file__).parent.parent.parent
    compose_file = project_root / "docker-compose.yml"

    if not compose_file.exists():
        print("! docker-compose.yml not found, skipping compose test")
        return True

    print("\nTesting Docker Compose exit functionality...")

    original_dir = os.getcwd()
    try:
        os.chdir(project_root)

        # Start services with docker-compose
        print("Starting Docker Compose services...")
        up_cmd = ["docker-compose", "up", "-d"]
        subprocess.run(up_cmd, check=True, timeout=120)
        print("OK Docker Compose services started")

        # Wait for services to be ready
        time.sleep(5)

        # Check if services are running
        ps_cmd = ["docker-compose", "ps", "-q"]
        ps_result = subprocess.run(ps_cmd, capture_output=True, text=True, check=True)

        if not ps_result.stdout.strip():
            raise DockerCliExitError("No Docker Compose services are running")

        print("OK Docker Compose services verified running")

        # Test graceful shutdown
        print("Testing graceful shutdown...")
        down_cmd = ["docker-compose", "down"]
        subprocess.run(down_cmd, check=True, timeout=30)
        print("OK Docker Compose services stopped gracefully")

        # Verify services are stopped
        ps_result = subprocess.run(ps_cmd, capture_output=True, text=True, check=True)
        if ps_result.stdout.strip():
            raise DockerCliExitError("Services still running after docker-compose down")

        print("OK Docker Compose exit verified")

    except subprocess.CalledProcessError as e:
        print(f"Docker Compose command failed: {e}")
        # Attempt cleanup
        with contextlib.suppress(BaseException):
            subprocess.run(["docker-compose", "down", "-v"], capture_output=True, check=False, timeout=30)
        raise DockerCliExitError(f"Docker Compose test failed: {e}") from e

    except subprocess.TimeoutExpired as e:
        # Cleanup on timeout
        with contextlib.suppress(BaseException):
            subprocess.run(["docker-compose", "down", "-v"], capture_output=True, check=False, timeout=30)
        raise DockerCliExitError("Docker Compose command timed out") from e

    finally:
        os.chdir(original_dir)


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
        # Test basic container exit
        test_docker_container_exit()
        print("\nOK Docker container exit test passed")

        # Test Docker Compose exit
        test_docker_compose_exit()
        print("OK Docker Compose exit test passed")

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
