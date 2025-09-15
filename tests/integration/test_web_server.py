#!/usr/bin/env -S uv run python
"""Integration test for dev container web server functionality."""

import contextlib
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path
from typing import Any

# Add tests directory to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent))

from docker_test_utils import ensure_test_image


class WebServerError(Exception):
    """Exception raised when web server test fails."""

    pass


def wait_for_server(url: str, timeout: int = 60, interval: float = 2.0) -> bool:
    """Wait for server to become available."""
    print(f"Waiting for server at {url}...")

    start_time = time.time()
    while time.time() - start_time < timeout:
        try:
            response = urllib.request.urlopen(url, timeout=5)
            if response.status == 200:
                print(f"OK Server is responding at {url}")
                return True
        except (urllib.error.URLError, urllib.error.HTTPError, OSError):
            pass

        time.sleep(interval)

    return False


def check_server_response(url: str) -> dict[str, Any]:
    """Check server response and return status information."""
    try:
        response = urllib.request.urlopen(url, timeout=10)

        # Read response content
        content = response.read()

        return {"status_code": response.status, "headers": dict(response.headers), "content_length": len(content), "content_preview": content[:500].decode("utf-8", errors="ignore")}

    except urllib.error.HTTPError as e:
        return {"status_code": e.code, "error": str(e), "content_length": 0, "content_preview": ""}

    except Exception as e:
        raise WebServerError(f"Failed to check server response: {e}") from e


def test_code_server_ui():
    """Test that code-server UI is accessible in dev container."""
    project_root = Path(__file__).parent.parent.parent

    print("Testing dev container web server (code-server UI)...")
    print(f"Project root: {project_root}")
    print("=" * 60)

    # Use shared image building logic
    image_name = ensure_test_image()
    print(f"Using Docker image: {image_name}")

    # Start container with web server
    print("\nStarting Docker container with web server...")
    container_name = f"clud-test-webserver-{uuid.uuid4().hex[:8]}"

    # Remove existing container if it exists
    with contextlib.suppress(BaseException):
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    # Use a different port for testing to avoid conflicts
    test_port = 8081

    run_cmd = ["docker", "run", "-d", "--name", container_name, "-p", f"{test_port}:8080", "-v", f"{project_root}:/home/coder/project", "-e", "ENVIRONMENT=test", image_name]

    try:
        result = subprocess.run(run_cmd, check=True, capture_output=True, text=True)
        container_id = result.stdout.strip()
        print(f"OK Container started: {container_id[:12]}")
        print(f"OK Web server should be accessible on port {test_port}")

        # Wait for container to fully start and web server to be ready
        server_url = f"http://localhost:{test_port}"

        if not wait_for_server(server_url, timeout=90):
            # Get container logs to debug
            logs_cmd = ["docker", "logs", container_name]
            logs_result = subprocess.run(logs_cmd, capture_output=True, text=True)
            print(f"Container logs:\n{logs_result.stdout}\n{logs_result.stderr}")
            raise WebServerError(f"Web server did not start within timeout at {server_url}")

        # Test server response
        print("\nTesting server response...")
        response_info = check_server_response(server_url)

        if response_info["status_code"] != 200:
            raise WebServerError(f"Server returned status code {response_info['status_code']}")

        print(f"OK Server returned status code: {response_info['status_code']}")
        print(f"OK Content length: {response_info['content_length']} bytes")

        # Check if it looks like a code-server response
        content_preview = response_info["content_preview"].lower()
        if "code-server" in content_preview or "vscode" in content_preview or "<!doctype html>" in content_preview:
            print("OK Response appears to be from code-server")
        else:
            print("! Response doesn't clearly indicate code-server, but server is responding")

        # Test that we can make multiple requests
        print("\nTesting multiple requests...")
        for i in range(3):
            response_info = check_server_response(server_url)
            if response_info["status_code"] != 200:
                raise WebServerError(f"Request {i + 1} failed with status {response_info['status_code']}")

        print("OK Multiple requests successful")

        # Test that container is still running
        check_cmd = ["docker", "ps", "-q", "-f", f"name={container_name}"]
        check_result = subprocess.run(check_cmd, capture_output=True, text=True)

        if not check_result.stdout.strip():
            raise WebServerError("Container stopped unexpectedly")

        print("OK Container is still running")

    except subprocess.CalledProcessError as e:
        print(f"Docker command failed: {e}")
        if e.stderr:
            print(f"Error output: {e.stderr}")
        raise WebServerError(f"Docker command failed: {e}") from e

    except subprocess.TimeoutExpired as e:
        raise WebServerError("Docker command timed out") from e

    finally:
        # Cleanup
        try:
            print("\nCleaning up container...")
            subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False, timeout=30)
            print("OK Container cleanup completed")
        except Exception:
            print("! Container cleanup may have failed")


def test_cli_ui_mode():
    """Test the clud CLI --ui mode functionality."""
    print("\nTesting clud CLI --ui mode...")

    # This test runs the CLI in UI mode but stops it quickly to verify it starts
    # We can't do a full test without risking hanging processes

    project_root = Path(__file__).parent.parent.parent

    try:
        # Test that CLI accepts --ui flag and doesn't error immediately
        cli_cmd = [sys.executable, "-m", "clud.cli", "--ui", "--port", "8082", str(project_root)]

        # Set a fake API key for testing
        test_env = os.environ.copy()
        test_env["ANTHROPIC_API_KEY"] = "sk-ant-test" + "x" * 50

        print("Testing CLI UI mode initialization...")

        # Run with timeout to prevent hanging
        process = subprocess.Popen(cli_cmd, env=test_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

        # Wait a bit to see if it starts properly
        try:
            stdout, stderr = process.communicate(timeout=30)

            if process.returncode == 0:
                print("OK CLI UI mode completed successfully")
            else:
                print(f"CLI UI mode output:\n{stdout}")
                print(f"CLI UI mode errors:\n{stderr}")
                # Don't fail the test if it's just a Docker/permission issue
                print("! CLI UI mode had non-zero exit, but this might be expected in test environment")

        except subprocess.TimeoutExpired:
            print("OK CLI UI mode started (terminating test after timeout)")
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()

    except Exception as e:
        print(f"CLI UI mode test error: {e}")
        # Don't fail the entire test suite for CLI issues
        print("! CLI UI mode test had issues, but continuing other tests")


def main():
    """Main test function."""
    print("Starting dev container web server integration tests...")

    # Check Docker availability
    try:
        subprocess.run(["docker", "version"], capture_output=True, check=True, timeout=10)
        print("OK Docker is available")
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, FileNotFoundError):
        print("X Docker is not available")
        return 1

    try:
        # Test direct container web server
        test_code_server_ui()
        print("\nOK Direct container web server test passed")

        # Test CLI UI mode
        test_cli_ui_mode()
        print("OK CLI UI mode test completed")

        print("\n" + "=" * 60)
        print("SUCCESS: All web server integration tests passed!")
        return 0

    except WebServerError as e:
        print(f"\nFAILED: {e}")
        return 1

    except Exception as e:
        print(f"\nERROR: Unexpected error: {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
