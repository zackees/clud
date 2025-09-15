#!/usr/bin/env -S uv run python
"""Simple Docker integration tests that don't depend on project setup."""

import contextlib
import os
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

# Add tests directory to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent))


class SimpleDockerError(Exception):
    """Exception raised when simple Docker test fails."""

    pass


def find_free_port():
    """Find an available port by binding to port 0 and getting the assigned port."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("", 0))
        s.listen(1)
        port = s.getsockname()[1]
    return port


def test_docker_container_basic():
    """Test basic Docker container creation and exit."""
    print("Testing basic Docker container functionality...")
    print("=" * 60)

    # Use a simple Ubuntu container for testing
    # Add unique suffix to prevent collisions when running tests in parallel
    container_name = f"clud-simple-test-{uuid.uuid4().hex[:8]}"

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
        rm_result = subprocess.run(rm_cmd, capture_output=True)
        if rm_result.returncode == 0:
            print("OK Container removed successfully")
        elif b"No such container" in rm_result.stderr:
            print("OK Container was already removed (auto-removed)")
        else:
            raise SimpleDockerError(f"Failed to remove container: {rm_result.stderr.decode()}")

    except subprocess.CalledProcessError as e:
        print(f"Command failed: {e}")
        if e.stderr:
            print(f"Error output: {e.stderr}")
        raise SimpleDockerError(f"Docker command failed: {e}") from e


def test_docker_web_server_nginx():
    """Test a simple web server container."""
    print("\nTesting simple web server container...")
    print("=" * 60)

    container_name = f"clud-nginx-test-{uuid.uuid4().hex[:8]}"
    test_port = find_free_port()

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

    container_name = f"clud-signal-test-{uuid.uuid4().hex[:8]}"

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


def test_git_sync_behavior():
    """Test that .git directory syncs from host to container but not back to host."""
    print("\nTesting .git directory sync behavior...")
    print("=" * 60)

    container_name = f"clud-git-sync-test-{uuid.uuid4().hex[:8]}"

    # Create temporary test directory with .git structure
    with tempfile.TemporaryDirectory() as temp_dir:
        temp_path = Path(temp_dir)

        # Create mock .git directory structure
        git_dir = temp_path / ".git"
        git_dir.mkdir()

        # Create test files in .git directory
        (git_dir / "HEAD").write_text("ref: refs/heads/main\n")
        (git_dir / "config").write_text("[core]\n\trepositoryformatversion = 0\n")

        # Create test project file
        (temp_path / "test_file.py").write_text("# Test file\nprint('hello')\n")

        try:
            # Start container with volume mount (host -> container) - let entrypoint handle initial sync
            run_cmd = ["docker", "run", "-d", "--name", container_name, f"--volume={temp_path}:/host:rw", "clud-test:latest", "--cmd", "sleep 300"]

            # Use MSYS_NO_PATHCONV to prevent Git Bash from converting paths
            env = os.environ.copy()
            env["MSYS_NO_PATHCONV"] = "1"
            result = subprocess.run(run_cmd, check=True, capture_output=True, text=True, env=env)
            container_id = result.stdout.strip()
            print(f"OK Container started: {container_id[:12]}")

            # Wait for container to be ready and initial sync to complete
            time.sleep(5)

            # Test 1: Verify initial sync from host to workspace includes .git
            print("Testing host -> workspace sync includes .git...")

            # Verify .git directory exists in workspace
            check_git_cmd = ["docker", "exec", container_name, "test", "-d", "/workspace/.git"]
            git_check = subprocess.run(check_git_cmd, capture_output=True)

            if git_check.returncode != 0:
                raise SimpleDockerError(".git directory was not synced to workspace")

            print("OK .git directory synced from host to workspace")

            # Test 2: Verify .git files are accessible in workspace
            check_head_cmd = ["docker", "exec", container_name, "cat", "/workspace/.git/HEAD"]
            head_result = subprocess.run(check_head_cmd, capture_output=True, text=True)

            if "refs/heads/main" not in head_result.stdout:
                raise SimpleDockerError("git files were not properly synced")

            print("OK .git files are accessible in workspace")

            # Test 3: Create breadcrumb file in workspace .git directory
            print("Testing workspace -> host sync excludes .git...")

            breadcrumb_cmd = ["docker", "exec", container_name, "sh", "-c", "echo 'test-breadcrumb' > /workspace/.git/breadcrumb.txt"]
            subprocess.run(breadcrumb_cmd, check=True, capture_output=True)

            # Verify breadcrumb exists in workspace
            check_breadcrumb_cmd = ["docker", "exec", container_name, "cat", "/workspace/.git/breadcrumb.txt"]
            breadcrumb_result = subprocess.run(check_breadcrumb_cmd, capture_output=True, text=True)

            if "test-breadcrumb" not in breadcrumb_result.stdout:
                raise SimpleDockerError("Failed to create breadcrumb in workspace .git")

            print("OK Breadcrumb created in workspace .git directory")

            # Test 4: Sync workspace back to host and verify .git exclusion
            sync_back_cmd = ["docker", "exec", container_name, "python", "/usr/local/bin/container-sync", "sync"]
            sync_back_result = subprocess.run(sync_back_cmd, capture_output=True, text=True)

            if sync_back_result.returncode != 0:
                raise SimpleDockerError(f"Workspace to host sync failed: {sync_back_result.stderr}")

            # Verify breadcrumb did NOT sync back to host
            host_breadcrumb_path = temp_path / ".git" / "breadcrumb.txt"
            if host_breadcrumb_path.exists():
                raise SimpleDockerError("SECURITY VIOLATION: .git breadcrumb synced back to host!")

            print("OK .git directory properly excluded from workspace -> host sync")

            # Test 5: Verify other files still sync normally
            # Create a regular file in workspace
            create_file_cmd = ["docker", "exec", container_name, "sh", "-c", "echo 'workspace change' > /workspace/workspace_file.txt"]
            subprocess.run(create_file_cmd, check=True, capture_output=True)

            # Sync again
            subprocess.run(sync_back_cmd, check=True, capture_output=True)

            # Verify regular file DID sync back
            host_file_path = temp_path / "workspace_file.txt"
            if not host_file_path.exists():
                raise SimpleDockerError("Regular files failed to sync back to host")

            if "workspace change" not in host_file_path.read_text():
                raise SimpleDockerError("Regular file content not synced correctly")

            print("OK Regular files sync correctly from workspace -> host")

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

        # Test git sync behavior
        test_git_sync_behavior()
        print("OK Git sync behavior test passed")

        print("\n" + "=" * 60)
        print("SUCCESS: All simple Docker integration tests passed!")
        print("This proves that:")
        print("- Docker containers can be started and stopped")
        print("- Web servers can be run in containers and accessed")
        print("- Containers respond properly to exit signals")
        print("- Container lifecycle management works correctly")
        print("- Git directories sync one-way from host to container correctly")
        return 0

    except SimpleDockerError as e:
        print(f"\nFAILED: {e}")
        return 1

    except Exception as e:
        print(f"\nERROR: Unexpected error: {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
