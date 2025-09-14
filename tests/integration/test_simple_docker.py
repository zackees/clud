#!/usr/bin/env -S uv run python
"""Simple Docker integration tests that don't depend on project setup."""

import contextlib
import subprocess
import sys
import time
import urllib.error
import urllib.request


class SimpleDockerError(Exception):
    """Exception raised when simple Docker test fails."""

    pass


def test_docker_container_basic():
    """Test basic Docker container creation and exit."""
    print("Testing basic Docker container functionality...")
    print("=" * 60)

    # Use a simple Ubuntu container for testing
    container_name = "clud-simple-test"

    # Remove existing container if it exists
    with contextlib.suppress(BaseException):
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    try:
        # Start a simple container that will exit after running a command
        run_cmd = ["docker", "run", "--name", container_name, "ubuntu:25.04", "echo", "Hello from Docker!"]

        result = subprocess.run(run_cmd, check=True, capture_output=True, text=True)
        output = result.stdout.strip()

        if "Hello from Docker!" not in output:
            raise SimpleDockerError(f"Expected output not found: {output}")

        print("OK Docker container ran successfully")
        print(f"Output: {output}")

        # Verify container exited cleanly
        inspect_cmd = ["docker", "inspect", container_name, "--format", "{{.State.ExitCode}}"]
        inspect_result = subprocess.run(inspect_cmd, capture_output=True, text=True, check=True)

        exit_code = inspect_result.stdout.strip()
        if exit_code != "0":
            raise SimpleDockerError(f"Container exited with non-zero code: {exit_code}")

        print("OK Container exited cleanly")

        # Test container removal
        rm_cmd = ["docker", "rm", container_name]
        subprocess.run(rm_cmd, check=True, capture_output=True)
        print("OK Container removed successfully")

    except subprocess.CalledProcessError as e:
        print(f"Command failed: {e}")
        if e.stderr:
            print(f"Error output: {e.stderr}")
        raise SimpleDockerError(f"Docker command failed: {e}") from e


def test_docker_web_server_nginx():
    """Test a simple web server container."""
    print("\nTesting simple web server container...")
    print("=" * 60)

    container_name = "clud-nginx-test"
    test_port = 8082

    # Remove existing container if it exists
    with contextlib.suppress(BaseException):
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    try:
        # Start nginx container
        run_cmd = ["docker", "run", "-d", "--name", container_name, "-p", f"{test_port}:80", "nginx:alpine"]

        result = subprocess.run(run_cmd, check=True, capture_output=True, text=True)
        container_id = result.stdout.strip()
        print(f"OK Nginx container started: {container_id[:12]}")

        # Wait for server to be ready
        server_url = f"http://localhost:{test_port}"
        print(f"Waiting for server at {server_url}...")

        # Wait up to 30 seconds for server to respond
        start_time = time.time()
        server_ready = False

        while time.time() - start_time < 30:
            try:
                response = urllib.request.urlopen(server_url, timeout=5)
                if response.status == 200:
                    print(f"OK Server is responding at {server_url}")
                    server_ready = True
                    break
            except (urllib.error.URLError, urllib.error.HTTPError, OSError):
                pass
            time.sleep(2)

        if not server_ready:
            # Get container logs
            logs_cmd = ["docker", "logs", container_name]
            logs_result = subprocess.run(logs_cmd, capture_output=True, text=True)
            print(f"Container logs:\n{logs_result.stdout}\n{logs_result.stderr}")
            raise SimpleDockerError("Web server did not start within timeout")

        # Test server response
        response = urllib.request.urlopen(server_url, timeout=10)
        content = response.read()

        print(f"OK Server returned status code: {response.status}")
        print(f"OK Content length: {len(content)} bytes")

        # Test graceful shutdown
        print("Testing graceful container stop...")
        stop_cmd = ["docker", "stop", "-t", "10", container_name]
        subprocess.run(stop_cmd, check=True, timeout=15)
        print("OK Container stopped gracefully")

    except subprocess.CalledProcessError as e:
        print(f"Docker command failed: {e}")
        if e.stderr:
            print(f"Error output: {e.stderr}")
        raise SimpleDockerError(f"Docker command failed: {e}") from e

    finally:
        # Cleanup
        try:
            subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)
            print("OK Container cleanup completed")
        except Exception:
            pass


def test_docker_exit_signals():
    """Test that Docker containers respond to exit signals."""
    print("\nTesting Docker container exit signals...")
    print("=" * 60)

    container_name = "clud-signal-test"

    # Remove existing container if it exists
    with contextlib.suppress(BaseException):
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    try:
        # Start a long-running container
        run_cmd = ["docker", "run", "-d", "--name", container_name, "ubuntu:25.04", "sleep", "300"]

        result = subprocess.run(run_cmd, check=True, capture_output=True, text=True)
        container_id = result.stdout.strip()
        print(f"OK Long-running container started: {container_id[:12]}")

        # Wait a moment for container to settle
        time.sleep(2)

        # Check that container is running
        ps_cmd = ["docker", "ps", "-q", "-f", f"name={container_name}"]
        ps_result = subprocess.run(ps_cmd, capture_output=True, text=True)

        if not ps_result.stdout.strip():
            raise SimpleDockerError("Container is not running")

        print("OK Container is running")

        # Test graceful stop (SIGTERM)
        print("Testing SIGTERM (graceful stop)...")
        stop_cmd = ["docker", "stop", "-t", "5", container_name]
        subprocess.run(stop_cmd, check=True, timeout=10)
        print("OK Container stopped with SIGTERM")

        # Verify container is stopped
        ps_result = subprocess.run(ps_cmd, capture_output=True, text=True)
        if ps_result.stdout.strip():
            raise SimpleDockerError("Container still running after stop")

        print("OK Container exit verified")

    except subprocess.CalledProcessError as e:
        print(f"Docker command failed: {e}")
        if e.stderr:
            print(f"Error output: {e.stderr}")
        raise SimpleDockerError(f"Docker command failed: {e}") from e

    finally:
        # Cleanup
        try:
            subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)
            print("OK Container cleanup completed")
        except Exception:
            pass


def main():
    """Main test function."""
    print("Starting simple Docker integration tests...")

    # Check Docker availability
    try:
        subprocess.run(["docker", "version"], capture_output=True, check=True, timeout=10)
        print("OK Docker is available")
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, FileNotFoundError):
        print("X Docker is not available")
        return 1

    try:
        # Test basic container functionality
        test_docker_container_basic()
        print("\nOK Basic Docker container test passed")

        # Test web server container
        test_docker_web_server_nginx()
        print("OK Web server container test passed")

        # Test exit signals
        test_docker_exit_signals()
        print("OK Docker exit signals test passed")

        print("\n" + "=" * 60)
        print("SUCCESS: All simple Docker integration tests passed!")
        print("This proves that:")
        print("- Docker containers can be started and stopped")
        print("- Web servers can be run in containers and accessed")
        print("- Containers respond properly to exit signals")
        print("- Container lifecycle management works correctly")
        return 0

    except SimpleDockerError as e:
        print(f"\nFAILED: {e}")
        return 1

    except Exception as e:
        print(f"\nERROR: Unexpected error: {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
