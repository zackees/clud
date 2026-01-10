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

---

# Agent Module Consolidation: Remove agent/foreground.py Duplicate

## Status: ✅ COMPLETED (2025-10-14)

**Created:** 2025-10-14
**Completed:** 2025-10-14 (Iteration 1)
**Priority:** High
**Effort:** Medium (~2-4 hours)

## Executive Summary

`src/clud/agent/foreground.py` is **legacy code** that should be deleted. It contains ~800 lines of duplicated code from `agent_cli.py`. Commit `29c95c5` ("feat(agent): consolidate agent execution into single module") created `agent_cli.py` to replace the distributed agent logic, but `foreground.py` was never removed, creating maintenance debt and code divergence.

## Problem Statement

### Code Duplication
- **agent_cli.py**: 1,638 lines, 40+ functions
- **agent/foreground.py**: 992 lines, 24 functions
- **Duplication**: ~800 lines (~80% overlap)

### Current Usage
- **agent_cli.py**: ✅ Active - main entry point via `cli.py:5`
- **agent/foreground.py**: ⚠️ Nearly unused - only 1 function imported by `foreground_args.py:123`

### Critical Issues

1. **Divergent Implementations**: The two files have drifted apart:
   - `_inject_completion_prompt()` differs (foreground.py has broken version)
   - `_run_loop()` differs (DONE.md validation logic)
   - Creates confusion about which file is authoritative

2. **Bug Duplication**: The loop prompt bug exists in foreground.py but was partially fixed in agent_cli.py

3. **Lost Features**: Only foreground.py has TaskInfo JSON tracking for iterations

## Detailed Analysis

### Duplicated Functions (22/24 are identical)

All credential management, API key functions, Claude path detection, command building, loop logic, and file editing functions are duplicated identically between both files. The only differences are:

- `_inject_completion_prompt()` - **DIFFERENT** (foreground.py has broken complex version)
- `_run_loop()` - **DIFFERENT** (foreground.py uses TaskInfo, skips lint-test validation)

### Unique to agent_cli.py (Keep These)

**Hook System:**
- `register_hooks_from_config()` - Hook system integration
- `trigger_hook_sync()` - Hook system integration

**Terminal UX:**
- `set_terminal_title()` - Sets terminal title to "clud: {parent_dir}"

**Special Command Handlers:**
- 15+ command handlers (lint, test, codeup, kanban, telegram, webui, api-server, code-server, fix, init-loop)
- `run_clud_subprocess()` - Subprocess execution helper
- GitHub URL handling functions

**Main Entry Point:**
- `run_agent()` - Main agent execution with full hook support
- `main()` - CLI entry point with command routing

### Unique to foreground.py (Port These)

1. **TaskInfo Integration** - Creates `.loop/info.json` with session metadata and iteration history
2. **Streaming JSON Callback** - `_create_streaming_json_callback()` (currently unused)
3. **Simpler DONE.md Logic** - No lint-test validation (keep agent_cli.py's validation instead)

### Critical Differences

#### Loop Prompt Injection Bug

**agent_cli.py (lines 827-843)** - OLD, working prompt:
```python
parts.append(f"Before finishing this iteration, create a summary file named .loop/ITERATION_{iteration}.md documenting what you accomplished.")
```

**foreground.py (lines 341-381)** - NEW, BROKEN prompt:
```
ITERATION PROTOCOL:
1. Before starting work, check for error signals...
2. During your work: ...
3. Before finishing this iteration:
   - Create .loop/ITERATION_{iteration}.md documenting what you accomplished
...
```

**Issue**: The foreground.py version buries the ITERATION file requirement in a complex 5-step protocol, causing agents to skip creating ITERATION files.

#### DONE.md Validation

**agent_cli.py** validates DONE.md with `lint-test` before accepting (lines 1165-1180).
**foreground.py** accepts DONE.md immediately without validation (lines 773-778).

**Decision**: Keep lint-test validation to prevent false completions.

## Dependencies

### What imports foreground.py?
- `src/clud/agent/foreground_args.py:123` - Imports `load_telegram_credentials()`

### What does foreground.py import?
```python
from .completion import detect_agent_completion
from .foreground_args import Args, parse_args
from .task_info import TaskInfo
from ..telegram_bot import TelegramBot
```

## Refactoring Plan

### Phase 1: Port TaskInfo to agent_cli.py

**Location**: `agent_cli.py` - update `_run_loop()` function (lines 1106-1189)

**Add import at top**:
```python
from .agent.task_info import TaskInfo
import uuid
```

**Add after line 1118** (after `done_file = Path("DONE.md")`):
```python
info_file = loop_dir / "info.json"

# Initialize or load task info
user_prompt = args.prompt if args.prompt else args.message
task_info = TaskInfo.load(info_file)

if task_info is None:
    # Create new task info for fresh session
    task_info = TaskInfo(
        session_id=str(uuid.uuid4()),
        start_time=time.time(),
        prompt=user_prompt,
        total_iterations=loop_count,
    )
    task_info.save(info_file)
else:
    # Update existing task info for continuation
    task_info.total_iterations = loop_count
    task_info.save(info_file)
```

**Add before line 1132** (before printing prompt):
```python
# Mark iteration start
task_info.start_iteration(iteration_num)
task_info.save(info_file)
```

**Add after line 1160** (after returncode is obtained):
```python
# Mark iteration end
error_msg = f"Exit code: {returncode}" if returncode != 0 else None
task_info.end_iteration(returncode, error_msg)
task_info.save(info_file)
```

**Add after line 1179** (inside DONE.md exists block):
```python
task_info.mark_completed()
task_info.save(info_file)
```

**Add after line 1182** (after "All iterations complete"):
```python
# Mark completion if all iterations finish without DONE.md
if not done_file.exists():
    task_info.mark_completed(error="Completed all iterations without DONE.md")
    task_info.save(info_file)
```

### Phase 2: Update Import in foreground_args.py

**File**: `src/clud/agent/foreground_args.py`

**Line 123** - Change:
```python
# BEFORE:
from .foreground import load_telegram_credentials

# AFTER:
from ..agent_cli import load_telegram_credentials
```

### Phase 3: Delete Legacy File

```bash
git rm src/clud/agent/foreground.py
git commit -m "refactor(agent): remove legacy foreground.py duplicate

- foreground.py was 80% duplicate of agent_cli.py
- Created in original consolidation (29c95c5) but never deleted
- Only 1 function (load_telegram_credentials) was imported
- Import updated in foreground_args.py to use agent_cli
- TaskInfo integration ported to agent_cli.py

Net reduction: 960 lines of duplicate code"
```

### Phase 4: Verify No References

```bash
grep -r "agent\.foreground\|from.*foreground import\|agent/foreground" src/ tests/
# Should return zero results
```

## Testing Checklist

- [ ] **Loop Mode**: `clud --loop 3 -p "test task"`
  - [ ] ITERATION_1.md is created
  - [ ] ITERATION_2.md is created
  - [ ] ITERATION_3.md is created
  - [ ] .loop/info.json is created

- [ ] **TaskInfo Content**: Verify info.json contains:
  - [ ] session_id (UUID)
  - [ ] start_time and start_time_readable
  - [ ] prompt (user's original prompt)
  - [ ] total_iterations (3)
  - [ ] iterations array with timing/exit codes

- [ ] **DONE.md Validation**: `clud --loop 10 -p "write DONE.md immediately"`
  - [ ] DONE.md is created by agent
  - [ ] lint-test validation runs
  - [ ] Loop halts if validation passes

- [ ] **Import Test**: `python -c "from clud.agent.foreground_args import parse_args"`
  - [ ] No ImportError

- [ ] **Full Test Suite**: `bash test --full`
  - [ ] All tests pass

- [ ] **Search**: `grep -r "foreground\.py" src/ tests/`
  - [ ] Zero results

## Benefits

### Code Quality
- ✅ Eliminates 800+ lines of duplicate code
- ✅ Single source of truth for agent logic
- ✅ Reduces maintenance burden
- ✅ Clarifies architecture

### Bug Fixes
- ✅ Fixes divergent loop prompt implementations
- ✅ Prevents future code drift
- ✅ Consolidates DONE.md validation logic

### Features
- ✅ Adds TaskInfo JSON tracking to main implementation
- ✅ Creates `.loop/info.json` with iteration history
- ✅ Better debugging and progress tracking

## Files Modified

### Updated
- `src/clud/agent_cli.py` - Add TaskInfo integration (~30 lines)
- `src/clud/agent/foreground_args.py` - Update import path (1 line)

### Deleted
- `src/clud/agent/foreground.py` - Remove entire file (992 lines)

### Net Change
- **Lines added**: ~30
- **Lines removed**: ~992
- **Net reduction**: ~960 lines (-60% of duplicate code)

## Risk Assessment

### Low Risk ✅
- foreground.py essentially unused (only 1 function import)
- All functionality exists in agent_cli.py
- Changes well-isolated

### Medium Risk ⚠️
- TaskInfo integration new to agent_cli.py
  - **Mitigation**: Port exactly as-is, test loop mode
- Import path change
  - **Mitigation**: Simple change, add import test

### High Risk ❌
- None identified

## Related Issues

- Loop mode not creating ITERATION_1.md files (foreground.py broken prompt)
- Code duplication between agent_cli.py and agent/foreground.py
- Inconsistent DONE.md validation between implementations

## References

- Commit `29c95c5`: "feat(agent): consolidate agent execution into single module"
- Commit `dbdab3b`: "refactor(foreground): build conditional prompts for loop mode" (introduced broken prompt)
- `src/clud/agent/task_info.py`: TaskInfo JSON tracking implementation

## Completion Summary (Iteration 1)

All phases successfully completed:
- ✅ Phase 1: TaskInfo integration ported to agent_cli.py (~50 lines added)
- ✅ Phase 2: Import updated in foreground_args.py (1 line changed)
- ✅ Phase 3: Legacy foreground.py deleted (992 lines removed)
- ✅ Phase 4: All references verified removed
- ✅ Test mocks updated in test_cli_yolo_integration.py
- ✅ All linting passed (ruff + pyright: 0 errors)

**Net Result:** 940 lines of code removed, single source of truth established.

---

**Last Updated**: 2025-10-14