# Integration Test Refactoring Plan

## Current State
The codebase has multiple integration tests spread across different files:
- `test_build.py` - Tests Docker image building
- `test_docker_cli_exit.py` - Tests container exit and workspace sync
- `test_claude_plugins.py` - Tests plugin mounting functionality
- `test_web_server.py` - Tests web server functionality
- `test_simple_docker.py` - Simple Docker tests

These tests are:
1. **Redundant** - Multiple tests build the same Docker image
2. **Slow** - Each test rebuilds the image independently
3. **Resource Contention** - Separate tests compete for Docker as a singleton resource
4. **Complex** - Too many edge cases tested separately

## Proposed Solution
Consolidate ALL integration tests and edge cases into a single, comprehensive test that:
1. Builds the Docker image once
2. Tests all edge cases sequentially using the same image
3. Avoids resource contention by having one test own the Docker singleton

## Implementation Plan

### Step 1: Create New Unified Test
Create `tests/integration/test_integration.py`:
```python
#!/usr/bin/env -S uv run python
"""Single integration test for all Docker functionality and edge cases."""

def test_docker_integration():
    """Single test that verifies ALL Docker functionality and edge cases."""
    # Phase 1: Build image once
    image_id = ensure_test_image()

    # Phase 2: Basic functionality
    test_basic_execution()  # ls -al && exit 0, verify pyproject.toml

    # Phase 3: All edge cases in sequence
    test_workspace_sync()     # Multiple file sync scenarios
    test_container_exit()     # Various exit conditions
    test_plugin_mounting()    # Single file and directory mounts
    test_command_execution()  # Different --cmd scenarios
    test_background_mode()    # --bg flag behavior
    test_error_handling()     # Failed commands, missing files
    test_restart_behavior()   # Container stop/start cycles
    test_volume_mounting()    # Different mount configurations

    # All tests share the same built image - no rebuilds!
```

### Step 2: Remove Old Tests
Delete the following files:
- `tests/integration/test_build.py`
- `tests/integration/test_docker_cli_exit.py`
- `tests/integration/test_claude_plugins.py`
- `tests/integration/test_web_server.py`
- `tests/integration/test_simple_docker.py`

### Step 3: Update Test Runner
Modify `bash test` script to run the single integration test with appropriate timeout:
```bash
# Run single integration test with 10-minute timeout for Docker build
uv run pytest tests/integration/test_integration.py --timeout=600
```

## Benefits
1. **Speed**: Single Docker build instead of multiple
2. **No Resource Contention**: One test owns Docker singleton - no parallel test conflicts
3. **Comprehensive**: ALL edge cases tested in one place
4. **Maintainability**: Single test file to update
5. **Reliability**: Sequential execution eliminates race conditions

## What We're Testing
The single comprehensive test verifies EVERYTHING:
- Docker image builds successfully
- Container starts with workspace mounted
- Basic command execution (`ls -al && exit 0`)
- Workspace sync (pyproject.toml visible)
- Container exits cleanly
- Plugin mounting (single files and directories)
- Web server functionality
- Multiple exit scenarios
- Container restart behavior
- Error handling and recovery
- Volume mounting variations
- Background mode operations
- Command injection scenarios

## Why One Big Test Is Better
- **Docker is a singleton resource** - parallel tests fight over ports, names, and resources
- **Shared image** - Build once, test everything with that image
- **Sequential execution** - Each edge case runs in isolation, no race conditions
- **Easier debugging** - When it fails, you know exactly which phase failed
- **Faster overall** - No repeated image builds, no container name conflicts

## Migration Steps
1. Create new test file with single test
2. Verify new test passes
3. Remove old test files
4. Update CI/CD pipelines if needed
5. Update documentation

## Success Criteria
- Single test runs in < 3 minutes after initial image build
- Test reliably passes on all platforms (Windows/Linux/Mac)
- ALL edge cases covered (not just core functionality)
- No flaky test failures from resource contention
- Clear phase-by-phase output showing what's being tested