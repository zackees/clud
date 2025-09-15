Your task is to audit rsync and determine if .git is part of that rsync. It absolutely must be. However it must not be sync'd back to the host at this point.

host: .git -> container:/workspace/.git (ONE WAY)

## Audit Results ✅ COMPLETED

**CONFIRMED: The rsync configuration is correctly implemented according to the requirements.**

### Key Findings:

1. **Host → Container sync (ONE WAY)**: ✅ CORRECT
   - Uses `RSYNC_EXCLUSIONS_COMMON` which does NOT include `.git`
   - Line 111: `exclusions = RSYNC_EXCLUSIONS_COMMON` for host-to-workspace sync
   - **Result**: `.git` directory IS synced from host to container

2. **Container → Host sync exclusion**: ✅ CORRECT
   - Uses `RSYNC_EXCLUSIONS_TO_HOST` which includes `"/.git"`
   - Line 44: `"/.git", # .git must NOT be synced back to host`
   - Line 107: Uses this exclusion list for workspace-to-host sync
   - **Result**: `.git` directory is NOT synced back to host

## Integration Test Design

### Test Objective
Verify that `.git` directory syncs one-way from host to container but never back to host.

### Test Location
Add `test_git_sync_behavior()` function to existing `tests/integration/test_simple_docker.py`

### Test Design

```python
def test_git_sync_behavior():
    """Test that .git directory syncs from host to container but not back to host."""
    print("\nTesting .git directory sync behavior...")
    print("=" * 60)

    container_name = f"clud-git-sync-test-{uuid.uuid4().hex[:8]}"

    # Create temporary test directory with .git structure
    import tempfile
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
            # Start container with volume mount (host -> container)
            run_cmd = [
                "docker", "run", "-d", "--name", container_name,
                f"--volume={temp_path}:/host:rw",
                "clud-test:latest",
                "sleep", "300"
            ]

            result = subprocess.run(run_cmd, check=True, capture_output=True, text=True)
            container_id = result.stdout.strip()
            print(f"OK Container started: {container_id[:12]}")

            # Wait for container to be ready
            time.sleep(2)

            # Test 1: Verify initial sync from host to workspace includes .git
            print("Testing host -> workspace sync includes .git...")

            exec_cmd = ["docker", "exec", container_name, "python", "/usr/local/bin/container-sync", "init"]
            sync_result = subprocess.run(exec_cmd, capture_output=True, text=True)

            if sync_result.returncode != 0:
                raise SimpleDockerError(f"Initial sync failed: {sync_result.stderr}")

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

            breadcrumb_cmd = [
                "docker", "exec", container_name, "sh", "-c",
                "echo 'test-breadcrumb' > /workspace/.git/breadcrumb.txt"
            ]
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
            create_file_cmd = [
                "docker", "exec", container_name, "sh", "-c",
                "echo 'workspace change' > /workspace/workspace_file.txt"
            ]
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
```

### Test Verification Points

1. **.git sync to workspace**: Verifies `.git` directory and files are copied from host to workspace during initial sync
2. **.git accessibility**: Confirms `.git` files are readable and contain expected content in workspace
3. **Breadcrumb isolation**: Creates a test file in workspace `.git` to verify it doesn't sync back
4. **Exclusion enforcement**: Confirms `.git` changes in workspace do NOT propagate to host
5. **Normal file sync**: Ensures regular files still sync bidirectionally as expected

### Benefits of This Test Design

- **Minimal changes**: Adds single function to existing test file
- **Comprehensive coverage**: Tests both directions of sync with specific focus on `.git`
- **Security validation**: Confirms that `.git` modifications in container don't affect host repository
- **Integration approach**: Uses real container and sync commands rather than unit tests
- **Self-contained**: Uses temporary directory, doesn't affect actual project state

### Implementation Requirements

- Add necessary imports to test file: `tempfile`
- Use existing test infrastructure (container naming, cleanup patterns)
- Follow existing error handling patterns with `SimpleDockerError`
- Use clud-test image which contains the container-sync script
- Update main() function to call the new test

### Review Notes

**Test Design is Clean and Minimal:**
- ✅ Reuses existing test patterns and infrastructure
- ✅ Single function addition to existing file
- ✅ Comprehensive coverage of the security requirement
- ✅ Uses temporary directory to avoid side effects
- ✅ Proper cleanup in finally block
- ✅ Clear verification points for both sync directions

**Security Focus:**
- ✅ Tests the critical security requirement: .git must not sync back to host
- ✅ Uses "breadcrumb" pattern to detect if .git content leaks back
- ✅ Verifies normal files still work to ensure no over-blocking

**Integration Approach:**
- ✅ Tests actual container sync commands rather than unit testing
- ✅ Uses real Docker container with proper volume mounts
- ✅ Exercises the full sync path from host → workspace → host