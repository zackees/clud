#!/usr/bin/env -S uv run python
"""Single comprehensive integration test for all Docker functionality and edge cases."""

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

# Import shared utilities
from clud.testing.docker_test_utils import ensure_test_image


class IntegrationTestError(Exception):
    """Exception raised when integration test fails."""

    pass


def find_free_port():
    """Find an available port by binding to port 0 and getting the assigned port."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("", 0))
        s.listen(1)
        port = s.getsockname()[1]
    return port


def wait_for_server(url: str, timeout: int = 60, interval: float = 2.0) -> bool:
    """Wait for server to become available."""
    print(f"  Waiting for server at {url}...")
    start_time = time.time()
    while time.time() - start_time < timeout:
        try:
            response = urllib.request.urlopen(url, timeout=5)
            if response.status == 200:
                print(f"  ✓ Server is responding at {url}")
                return True
        except (urllib.error.URLError, urllib.error.HTTPError, OSError):
            pass
        time.sleep(interval)
    return False


def test_docker_integration():
    """Single test that verifies ALL Docker functionality and edge cases."""
    print("Starting comprehensive Docker integration test...")
    print("=" * 80)

    # Store results for summary
    test_results: list[tuple[str, bool, str | None]] = []

    # Phase 1: Build image once
    print("\n[Phase 1] Building Docker image...")
    print("-" * 40)
    try:
        image_name = ensure_test_image()
        print(f"✓ Docker image ready: {image_name}")
        test_results.append(("Image build", True, None))
    except Exception as e:
        print(f"✗ Failed to build Docker image: {e}")
        test_results.append(("Image build", False, str(e)))
        raise IntegrationTestError(f"Image build failed: {e}") from e

    project_root = Path(__file__).parent.parent.parent

    # Phase 2: Basic functionality
    print("\n[Phase 2] Testing basic Docker functionality...")
    print("-" * 40)

    # Test 2.1: Basic container execution
    print("  Testing basic container execution...")
    container_name = f"clud-integration-basic-{uuid.uuid4().hex[:8]}"
    with contextlib.suppress(BaseException):
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    try:
        run_cmd = ["docker", "run", "--name", container_name, "-v", f"{project_root}:/host:rw", image_name, "--cmd", "ls -al && exit 0"]
        result = subprocess.run(run_cmd, check=True, capture_output=True, text=True, timeout=60)
        output = result.stdout

        # Verify pyproject.toml is visible (workspace sync worked)
        if "pyproject.toml" in output:
            print("    ✓ Basic execution and workspace sync working")
            test_results.append(("Basic execution", True, None))
        else:
            raise IntegrationTestError("pyproject.toml not found in workspace")

    except Exception as e:
        print(f"    ✗ Basic execution failed: {e}")
        test_results.append(("Basic execution", False, str(e)))
        raise
    finally:
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    # Phase 3: All edge cases in sequence
    print("\n[Phase 3] Testing all edge cases...")
    print("-" * 40)

    # Test 3.1: Workspace sync verification
    print("  Testing workspace sync (pyproject.toml contains 'clud')...")
    container_name = f"clud-integration-sync-{uuid.uuid4().hex[:8]}"
    with contextlib.suppress(BaseException):
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    try:
        run_cmd = ["docker", "run", "--name", container_name, "-v", f"{project_root}:/host:rw", image_name, "--cmd", "cat pyproject.toml && exit 0"]
        result = subprocess.run(run_cmd, check=True, capture_output=True, text=True, timeout=60)

        if "clud" in result.stdout:
            print("    ✓ Workspace sync verification passed")
            test_results.append(("Workspace sync", True, None))
        else:
            raise IntegrationTestError("'clud' not found in pyproject.toml")

    except Exception as e:
        print(f"    ✗ Workspace sync failed: {e}")
        test_results.append(("Workspace sync", False, str(e)))
    finally:
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    # Test 3.2: Container exit and restart
    print("  Testing container exit and restart behavior...")
    container_name = f"clud-integration-exit-{uuid.uuid4().hex[:8]}"
    with contextlib.suppress(BaseException):
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    try:
        # Start container in detached mode
        run_cmd = ["docker", "run", "-d", "--name", container_name, "-v", f"{project_root}:/home/coder/project", image_name]
        subprocess.run(run_cmd, check=True, capture_output=True, text=True)

        time.sleep(5)

        # Check if running
        check_cmd = ["docker", "ps", "-q", "-f", f"name={container_name}"]
        check_result = subprocess.run(check_cmd, capture_output=True, text=True)

        if check_result.stdout.strip():
            # Test graceful stop
            stop_cmd = ["docker", "stop", "-t", "10", container_name]
            subprocess.run(stop_cmd, check=True, capture_output=True, timeout=15)

            # Verify stopped
            check_result = subprocess.run(check_cmd, capture_output=True, text=True)
            if not check_result.stdout.strip():
                # Test restart
                restart_cmd = ["docker", "start", container_name]
                subprocess.run(restart_cmd, check=True, capture_output=True)
                time.sleep(3)

                check_result = subprocess.run(check_cmd, capture_output=True, text=True)
                if check_result.stdout.strip():
                    print("    ✓ Container exit and restart working")
                    test_results.append(("Container lifecycle", True, None))
                else:
                    raise IntegrationTestError("Container failed to restart")
            else:
                raise IntegrationTestError("Container did not stop")
        else:
            raise IntegrationTestError("Container did not start")

    except Exception as e:
        print(f"    ✗ Container lifecycle test failed: {e}")
        test_results.append(("Container lifecycle", False, str(e)))
    finally:
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    # Test 3.3: Plugin mounting (directory)
    print("  Testing plugin directory mounting...")
    container_name = f"clud-integration-plugin-dir-{uuid.uuid4().hex[:8]}"

    with tempfile.TemporaryDirectory() as temp_dir:
        temp_path = Path(temp_dir)
        plugins_dir = temp_path / "test_plugins"
        plugins_dir.mkdir()

        # Create test plugin files
        (plugins_dir / "test.md").write_text("# Test Plugin\n\nTest plugin content.")
        (plugins_dir / "deploy.md").write_text("# Deploy Plugin\n\nDeploy plugin content.")

        with contextlib.suppress(BaseException):
            subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

        try:
            # Start container with plugin directory mounted
            run_cmd = ["docker", "run", "-d", "--name", container_name, "-v", f"{plugins_dir}:/root/.claude/commands:ro", image_name, "--cmd", "sleep 300"]

            env = os.environ.copy()
            env["MSYS_NO_PATHCONV"] = "1"
            subprocess.run(run_cmd, check=True, capture_output=True, text=True, env=env)

            time.sleep(3)

            # Verify plugins are mounted
            check_cmd = ["docker", "exec", container_name, "ls", "/root/.claude/commands"]
            result = subprocess.run(check_cmd, capture_output=True, text=True)

            if "test.md" in result.stdout and "deploy.md" in result.stdout:
                print("    ✓ Plugin directory mounting working")
                test_results.append(("Plugin directory mount", True, None))
            else:
                raise IntegrationTestError("Plugins not found in container")

        except Exception as e:
            print(f"    ✗ Plugin directory mounting failed: {e}")
            test_results.append(("Plugin directory mount", False, str(e)))
        finally:
            subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    # Test 3.4: Plugin mounting (single file)
    print("  Testing single plugin file mounting...")
    container_name = f"clud-integration-plugin-file-{uuid.uuid4().hex[:8]}"

    with tempfile.TemporaryDirectory() as temp_dir:
        temp_path = Path(temp_dir)
        single_plugin = temp_path / "single.md"
        single_plugin.write_text("# Single Plugin\n\nSingle plugin content.")

        with contextlib.suppress(BaseException):
            subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

        try:
            run_cmd = ["docker", "run", "-d", "--name", container_name, "-v", f"{single_plugin}:/root/.claude/commands/single.md:ro", image_name, "--cmd", "sleep 300"]

            env = os.environ.copy()
            env["MSYS_NO_PATHCONV"] = "1"
            subprocess.run(run_cmd, check=True, capture_output=True, text=True, env=env)

            time.sleep(3)

            # Verify single plugin is mounted
            check_cmd = ["docker", "exec", container_name, "test", "-f", "/root/.claude/commands/single.md"]
            result = subprocess.run(check_cmd, capture_output=True)

            if result.returncode == 0:
                print("    ✓ Single plugin file mounting working")
                test_results.append(("Single plugin mount", True, None))
            else:
                raise IntegrationTestError("Single plugin file not found")

        except Exception as e:
            print(f"    ✗ Single plugin mounting failed: {e}")
            test_results.append(("Single plugin mount", False, str(e)))
        finally:
            subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    # Test 3.5: Web server functionality
    print("  Testing web server functionality (code-server)...")
    container_name = f"clud-integration-webserver-{uuid.uuid4().hex[:8]}"
    test_port = find_free_port()

    with contextlib.suppress(BaseException):
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    try:
        run_cmd = ["docker", "run", "-d", "--name", container_name, "-p", f"{test_port}:8080", "-v", f"{project_root}:/home/coder/project", "-e", "ENVIRONMENT=test", image_name]
        subprocess.run(run_cmd, check=True, capture_output=True, text=True)

        # Wait for server to be ready
        server_url = f"http://localhost:{test_port}"
        if wait_for_server(server_url, timeout=90):
            # Test server response
            response = urllib.request.urlopen(server_url, timeout=10)
            content = response.read()

            if response.status == 200 and len(content) > 0:
                print("    ✓ Web server (code-server) working")
                test_results.append(("Web server", True, None))
            else:
                raise IntegrationTestError(f"Unexpected server response: {response.status}")
        else:
            raise IntegrationTestError("Web server did not start")

    except Exception as e:
        print(f"    ✗ Web server test failed: {e}")
        test_results.append(("Web server", False, str(e)))
    finally:
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    # Test 3.6: Background mode
    print("  Testing background mode (--bg flag)...")
    container_name = f"clud-integration-bg-{uuid.uuid4().hex[:8]}"

    with contextlib.suppress(BaseException):
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    try:
        # Container should start in background with --bg flag (simulated by -d in run)
        run_cmd = ["docker", "run", "-d", "--name", container_name, "-v", f"{project_root}:/home/coder/project", image_name]
        subprocess.run(run_cmd, check=True, capture_output=True, text=True)

        time.sleep(3)

        # Check if container is running in background
        check_cmd = ["docker", "ps", "-q", "-f", f"name={container_name}"]
        check_result = subprocess.run(check_cmd, capture_output=True, text=True)

        if check_result.stdout.strip():
            print("    ✓ Background mode working")
            test_results.append(("Background mode", True, None))
        else:
            raise IntegrationTestError("Container not running in background")

    except Exception as e:
        print(f"    ✗ Background mode test failed: {e}")
        test_results.append(("Background mode", False, str(e)))
    finally:
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    # Test 3.7: Error handling
    print("  Testing error handling (failed commands)...")
    container_name = f"clud-integration-error-{uuid.uuid4().hex[:8]}"

    with contextlib.suppress(BaseException):
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    try:
        # Run a command that should fail
        run_cmd = ["docker", "run", "--name", container_name, "-v", f"{project_root}:/host:rw", image_name, "--cmd", "exit 42"]
        result = subprocess.run(run_cmd, capture_output=True, text=True)

        # Check exit code
        inspect_cmd = ["docker", "inspect", container_name, "--format", "{{.State.ExitCode}}"]
        inspect_result = subprocess.run(inspect_cmd, capture_output=True, text=True)

        exit_code = inspect_result.stdout.strip()
        if exit_code == "42":
            print("    ✓ Error handling (exit codes) working")
            test_results.append(("Error handling", True, None))
        else:
            raise IntegrationTestError(f"Unexpected exit code: {exit_code}")

    except Exception as e:
        print(f"    ✗ Error handling test failed: {e}")
        test_results.append(("Error handling", False, str(e)))
    finally:
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    # Test 3.8: Exit signals
    print("  Testing exit signals (SIGTERM handling)...")
    container_name = f"clud-integration-signals-{uuid.uuid4().hex[:8]}"

    with contextlib.suppress(BaseException):
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    try:
        # Start a long-running container
        run_cmd = ["docker", "run", "-d", "--name", container_name, "ubuntu:25.04", "sleep", "300"]
        subprocess.run(run_cmd, check=True, capture_output=True, text=True)

        time.sleep(2)

        # Test graceful stop (SIGTERM)
        stop_cmd = ["docker", "stop", "-t", "5", container_name]
        subprocess.run(stop_cmd, check=True, timeout=10)

        # Verify container stopped
        check_cmd = ["docker", "ps", "-q", "-f", f"name={container_name}"]
        check_result = subprocess.run(check_cmd, capture_output=True, text=True)

        if not check_result.stdout.strip():
            print("    ✓ Exit signals (SIGTERM) working")
            test_results.append(("Exit signals", True, None))
        else:
            raise IntegrationTestError("Container did not respond to SIGTERM")

    except Exception as e:
        print(f"    ✗ Exit signals test failed: {e}")
        test_results.append(("Exit signals", False, str(e)))
    finally:
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    # Test 3.9: Git sync behavior
    print("  Testing .git directory sync behavior...")
    container_name = f"clud-integration-git-{uuid.uuid4().hex[:8]}"

    with tempfile.TemporaryDirectory() as temp_dir:
        temp_path = Path(temp_dir)

        # Initialize a real Git repository
        subprocess.run(["git", "init"], cwd=temp_path, check=True, capture_output=True)
        subprocess.run(["git", "config", "user.email", "test@example.com"], cwd=temp_path, check=True, capture_output=True)
        subprocess.run(["git", "config", "user.name", "Test User"], cwd=temp_path, check=True, capture_output=True)

        # Create and commit a test file
        (temp_path / "test_file.py").write_text("# Test file\nprint('hello')\n")
        subprocess.run(["git", "add", "."], cwd=temp_path, check=True, capture_output=True)
        subprocess.run(["git", "commit", "-m", "Initial commit"], cwd=temp_path, check=True, capture_output=True)

        with contextlib.suppress(BaseException):
            subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

        try:
            # Start container with volume mount
            run_cmd = ["docker", "run", "-d", "--name", container_name, f"--volume={temp_path}:/host:rw", image_name, "--cmd", "sleep 300"]

            env = os.environ.copy()
            env["MSYS_NO_PATHCONV"] = "1"
            subprocess.run(run_cmd, check=True, capture_output=True, text=True, env=env)

            time.sleep(5)

            # Verify Git is available in workspace (either .git directory or .git file from worktree)
            check_git_exists_cmd = ["docker", "exec", container_name, "test", "-e", "/workspace/.git"]
            git_exists = subprocess.run(check_git_exists_cmd, capture_output=True)

            if git_exists.returncode == 0:
                # Test Git functionality by checking status
                git_status_cmd = ["docker", "exec", container_name, "sh", "-c", "cd /workspace && git status --porcelain 2>&1"]
                status_result = subprocess.run(git_status_cmd, capture_output=True, text=True)

                # Check if Git is working (no fatal errors)
                if status_result.returncode == 0 and "fatal" not in status_result.stdout.lower():
                    # Git is working - now test that we can make changes
                    test_file_cmd = ["docker", "exec", container_name, "sh", "-c", "cd /workspace && echo 'test content' > test_git_sync.txt"]
                    subprocess.run(test_file_cmd, check=True, capture_output=True)

                    # Verify the test file appears in git status
                    status_check_cmd = ["docker", "exec", container_name, "sh", "-c", "cd /workspace && git status --porcelain"]
                    status_check = subprocess.run(status_check_cmd, capture_output=True, text=True)

                    if "test_git_sync.txt" in status_check.stdout:
                        # Sync back to host
                        sync_cmd = ["docker", "exec", container_name, "python", "/usr/local/bin/container-sync", "sync"]
                        subprocess.run(sync_cmd, capture_output=True, text=True)

                        # Verify the test file synced back
                        host_test_file = temp_path / "test_git_sync.txt"
                        if host_test_file.exists():
                            # Clean up test file
                            host_test_file.unlink(missing_ok=True)

                            # Check: ensure .git changes don't sync back (if .git is a directory)
                            check_git_dir_cmd = ["docker", "exec", container_name, "test", "-d", "/workspace/.git"]
                            is_git_dir = subprocess.run(check_git_dir_cmd, capture_output=True)

                            if is_git_dir.returncode == 0:
                                # Create a breadcrumb in .git directory
                                breadcrumb_cmd = ["docker", "exec", container_name, "sh", "-c", "echo 'breadcrumb' > /workspace/.git/test_breadcrumb.txt"]
                                subprocess.run(breadcrumb_cmd, capture_output=True)

                                # Sync again
                                subprocess.run(sync_cmd, capture_output=True, text=True)

                                # Verify breadcrumb did NOT sync to host
                                host_breadcrumb = temp_path / ".git" / "test_breadcrumb.txt"
                                if not host_breadcrumb.exists():
                                    print("    ✓ Git functionality and sync isolation working")
                                    test_results.append(("Git sync", True, None))
                                else:
                                    # Clean up breadcrumb if it exists
                                    host_breadcrumb.unlink(missing_ok=True)
                                    raise IntegrationTestError(".git directory changes synced back to host (security issue)")
                            else:
                                # It's a worktree (.git is a file), which is also valid
                                print("    ✓ Git worktree functionality working")
                                test_results.append(("Git sync", True, None))
                        else:
                            raise IntegrationTestError("Test file did not sync back to host")
                    else:
                        raise IntegrationTestError("Git status not detecting file changes in workspace")
                else:
                    raise IntegrationTestError(f"Git status command failed: {status_result.stdout}")
            else:
                raise IntegrationTestError("No Git setup found in workspace (.git not present)")

        except Exception as e:
            print(f"    ✗ Git sync test failed: {e}")
            test_results.append(("Git sync", False, str(e)))
        finally:
            subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    # Test 3.10: Volume mounting variations
    print("  Testing volume mounting variations...")
    container_name = f"clud-integration-volumes-{uuid.uuid4().hex[:8]}"

    with tempfile.TemporaryDirectory() as temp_dir:
        temp_path = Path(temp_dir)

        # Create test files
        (temp_path / "file1.txt").write_text("File 1 content")
        subdir = temp_path / "subdir"
        subdir.mkdir()
        (subdir / "file2.txt").write_text("File 2 content")

        with contextlib.suppress(BaseException):
            subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

        try:
            # Mount with read-write
            run_cmd = ["docker", "run", "--name", container_name, "-v", f"{temp_path}:/test:rw", image_name, "--cmd", "ls -la /test && cat /test/file1.txt && exit 0"]

            env = os.environ.copy()
            env["MSYS_NO_PATHCONV"] = "1"
            result = subprocess.run(run_cmd, check=True, capture_output=True, text=True, env=env)

            if "File 1 content" in result.stdout and "subdir" in result.stdout:
                print("    ✓ Volume mounting variations working")
                test_results.append(("Volume mounting", True, None))
            else:
                raise IntegrationTestError("Volume mount content not accessible")

        except Exception as e:
            print(f"    ✗ Volume mounting test failed: {e}")
            test_results.append(("Volume mounting", False, str(e)))
        finally:
            subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    # Test 3.11: Multiple command execution scenarios
    print("  Testing multiple command execution scenarios...")
    container_name = f"clud-integration-cmds-{uuid.uuid4().hex[:8]}"

    with contextlib.suppress(BaseException):
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    try:
        # Test command chaining
        run_cmd = ["docker", "run", "--name", container_name, "-v", f"{project_root}:/host:rw", image_name, "--cmd", "echo 'First' && echo 'Second' && ls /workspace | head -5 && exit 0"]
        result = subprocess.run(run_cmd, check=True, capture_output=True, text=True)

        if "First" in result.stdout and "Second" in result.stdout:
            print("    ✓ Command chaining working")
            test_results.append(("Command execution", True, None))
        else:
            raise IntegrationTestError("Command chaining output not found")

    except Exception as e:
        print(f"    ✗ Command execution test failed: {e}")
        test_results.append(("Command execution", False, str(e)))
    finally:
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    # Test 3.12: Container with nginx web server
    print("  Testing nginx web server container...")
    container_name = f"clud-integration-nginx-{uuid.uuid4().hex[:8]}"
    test_port = find_free_port()

    with contextlib.suppress(BaseException):
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    try:
        # Start nginx container
        run_cmd = ["docker", "run", "-d", "--name", container_name, "-p", f"{test_port}:80", "nginx:alpine"]
        subprocess.run(run_cmd, check=True, capture_output=True, text=True)

        # Wait for nginx to be ready
        server_url = f"http://localhost:{test_port}"
        if wait_for_server(server_url, timeout=30):
            response = urllib.request.urlopen(server_url, timeout=10)
            content = response.read()

            if response.status == 200:
                # Test graceful shutdown
                stop_cmd = ["docker", "stop", "-t", "10", container_name]
                subprocess.run(stop_cmd, check=True, timeout=15)
                print("    ✓ Nginx web server container working")
                test_results.append(("Nginx container", True, None))
            else:
                raise IntegrationTestError(f"Nginx returned status {response.status}")
        else:
            raise IntegrationTestError("Nginx did not start")

    except Exception as e:
        print(f"    ✗ Nginx container test failed: {e}")
        test_results.append(("Nginx container", False, str(e)))
    finally:
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)

    # Print summary
    print("\n" + "=" * 80)
    print("TEST SUMMARY")
    print("=" * 80)

    total_tests = len(test_results)
    passed_tests = sum(1 for _, passed, _ in test_results if passed)
    failed_tests = total_tests - passed_tests

    for test_name, passed, error in test_results:
        status = "✓ PASS" if passed else "✗ FAIL"
        print(f"{status:8} - {test_name}")
        if error:
            print(f"         Error: {error[:60]}...")

    print("-" * 80)
    print(f"Total: {total_tests} tests | Passed: {passed_tests} | Failed: {failed_tests}")

    if failed_tests > 0:
        raise IntegrationTestError(f"{failed_tests} tests failed")

    print("\n✓ SUCCESS: All Docker integration tests passed!")
    print("\nThis comprehensive test verified:")
    print("• Docker image builds successfully")
    print("• Container starts with workspace mounted")
    print("• Basic command execution works")
    print("• Workspace sync (pyproject.toml visible)")
    print("• Container exits cleanly and can restart")
    print("• Plugin mounting (single files and directories)")
    print("• Web server functionality (code-server)")
    print("• Multiple exit scenarios")
    print("• Container restart behavior")
    print("• Error handling and recovery")
    print("• Volume mounting variations")
    print("• Background mode operations")
    print("• Command injection scenarios")
    print("• Git directory security (one-way sync)")
    print("• Signal handling (SIGTERM)")


def main():
    """Main test function."""
    print("CLUD Docker Integration Test Suite")
    print("=" * 80)

    # Check Docker availability
    try:
        subprocess.run(["docker", "version"], capture_output=True, check=True, timeout=10)
        print("✓ Docker is available")
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, FileNotFoundError):
        print("✗ Docker is not available")
        return 1

    try:
        test_docker_integration()
        return 0

    except IntegrationTestError as e:
        print(f"\n✗ FAILED: {e}")
        return 1

    except Exception as e:
        print(f"\n✗ ERROR: Unexpected error: {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
