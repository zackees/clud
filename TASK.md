# TASK: Background Agent Workspace Sync Architecture

## Objective
Design and implement a robust rsync-based synchronization system for the `/workspace` folder in the background agent (`bg.py`), enabling bidirectional code synchronization between host and Docker container.

## Current State Analysis

### Existing Infrastructure
- **Container Sync Module**: `src/clud/container_sync.py` already implements comprehensive rsync functionality
  - Class-based architecture with `ContainerSync`
  - Bidirectional sync methods: `sync_host_to_workspace()` and `sync_workspace_to_host()`
  - Built-in error handling, retry logic, and permission validation
  - .gitignore integration for selective syncing
  - Structured logging and metrics collection

- **Entrypoint Integration**: Current `entrypoint.sh` delegates to Python script
  - Simple bash entrypoint calls `python3 /usr/local/bin/container-sync init`
  - Minimal logic in shell script (good practice)

- **Volume Mapping**: Currently mounts directly to `/workspace`
  - Direct mount strategy: `--volume={docker_path}:/workspace:rw`
  - No intermediate `/host` directory yet

## Proposed Architecture

### 1. Dual Mount Strategy
```
Host Directory → /host (read-write mount)
     ↓ (rsync on startup)
/workspace (container working directory)
     ↓ (rsync on demand)
Host Directory ← /host (bidirectional sync)
```

### 2. Background Agent Integration (`bg.py`)

#### Design Decisions
- **Leverage existing `container_sync.py`** rather than creating new implementation
- **Extend functionality** for background agent specific needs:
  - Continuous monitoring mode
  - Event-driven sync triggers
  - Background sync scheduling
  - Integration with agent lifecycle

#### Key Components

```python
# bg.py structure
class BackgroundAgent:
    def __init__(self):
        self.sync_handler = ContainerSync()
        self.sync_interval = 300  # 5 minutes default
        self.watch_mode = False

    def initial_sync(self):
        """Perform initial host → workspace sync"""
        return self.sync_handler.sync_host_to_workspace()

    def schedule_periodic_sync(self):
        """Background sync task"""
        # Implementation for periodic sync

    def watch_for_changes(self):
        """File system watcher for auto-sync"""
        # Implementation for file watching
```

### 3. Helper Script Architecture (`workspace_sync.py`)

Instead of putting functionality in `entrypoint.sh`, create a dedicated helper:

```python
#!/usr/bin/env python3
"""Workspace sync helper for container initialization and management."""

import sys
import os
from pathlib import Path
from clud.container_sync import ContainerSync

class WorkspaceSyncHelper:
    """Helper class for entrypoint sync operations."""

    def __init__(self):
        self.sync = ContainerSync()

    def entrypoint_init(self):
        """Called by entrypoint.sh for initial setup."""
        # 1. Validate environment
        # 2. Perform initial sync
        # 3. Configure code-server
        # 4. Set up background sync if needed

    def setup_background_agent(self):
        """Initialize background agent for continuous sync."""
        # Start bg.py with sync capabilities
```

### 4. Enhanced Entrypoint Strategy

Keep `entrypoint.sh` minimal:

```bash
#!/bin/bash
set -e

# Delegate all logic to Python helper
exec python3 /usr/local/bin/workspace_sync.py init "$@"
```

## Implementation Plan

### Phase 1: Refactor Existing Infrastructure

0. **Create a integration test for this task**
   - tests/integration/test_workspace_sync.py
   - builds image (if necessary)
     * runs command --cmd "cat pyproject.toml | grep clud || echo "failed" && exit 1"
       * fix up this command if it's not exactly right. it will search for clud in pyproject.toml to verify that the workspace sync worked as expected.

1. **Update CLI volume mapping**
   - Change from `/workspace:rw` to `/host:rw`
   - Add environment variable for sync mode

2. **Create `workspace_sync.py` helper**
   - Import and utilize existing `ContainerSync` class
   - Add entrypoint-specific initialization logic
   - Handle environment configuration

3. **Update Dockerfile**
   - Copy new helper script
   - Ensure proper permissions
   - Update entrypoint to use helper

### Phase 2: Background Agent Development
1. **Create `bg.py` foundation**
   - Agent lifecycle management
   - Integration with ContainerSync
   - Error recovery mechanisms

2. **Implement sync strategies**
   - On-demand sync API
   - Periodic background sync
   - Event-driven sync (file watchers)

3. **Add monitoring and logging**
   - Sync metrics collection
   - Performance monitoring
   - Debug capabilities

### Phase 3: Advanced Features
1. **Selective sync patterns**
   - Per-project sync configurations
   - Dynamic exclusion rules
   - Binary file optimization

2. **Conflict resolution**
   - Detect concurrent modifications
   - Backup before destructive sync
   - Merge strategies

3. **Performance optimization**
   - Parallel sync for large projects
   - Incremental sync algorithms
   - Compression for network transfers

## Design Considerations

### Advantages of Python Helper Approach
✅ **Code reuse**: Leverages existing `ContainerSync` class
✅ **Maintainability**: Python easier to maintain than complex bash
✅ **Testing**: Can unit test Python code
✅ **Type safety**: Can use type hints and validation
✅ **Cross-platform**: Better Windows compatibility
✅ **Error handling**: Superior exception handling

### Potential Issues and Mitigations

#### Issue 1: Startup Performance
**Problem**: Python interpreter adds overhead to container startup
**Mitigation**:
- Pre-compile Python files to .pyc
- Use minimal imports in critical path
- Consider async initialization

#### Issue 2: Permission Conflicts
**Problem**: File ownership differences between host and container
**Mitigation**:
- Use `--no-owner` rsync flag where appropriate
- Implement permission mapping layer
- Document permission requirements

#### Issue 3: Large File Handling
**Problem**: Rsync memory usage with large files
**Mitigation**:
- Implement file size thresholds
- Use `--inplace` for large files
- Add streaming support for huge files

#### Issue 4: Windows Path Issues
**Problem**: Path separator and encoding differences
**Mitigation**:
- Use `pathlib.Path` for all path operations
- Normalize paths at entry points
- Test extensively on Windows

#### Issue 5: Concurrent Access
**Problem**: Race conditions during bidirectional sync
**Mitigation**:
- Implement file locking mechanism
- Use atomic operations
- Add sync queue management

## Security Considerations

### 1. Input Validation
- Validate all paths before rsync operations
- Prevent directory traversal attacks
- Sanitize user-provided patterns

### 2. Permission Model
- Run sync with minimal required permissions
- Drop privileges after initial setup
- Audit file access patterns

### 3. Secret Management
- Never sync sensitive files (.env, keys)
- Implement secret detection
- Add warning for credential files

## Testing Strategy

### Unit Tests
```python
def test_sync_empty_directory():
    """Test syncing empty directories."""

def test_sync_with_gitignore():
    """Test .gitignore pattern filtering."""

def test_permission_validation():
    """Test permission checking logic."""

def test_retry_mechanism():
    """Test retry on sync failure."""
```

### Integration Tests
- Test full sync workflow
- Verify bidirectional sync
- Test error recovery
- Validate Windows compatibility

### Performance Tests
- Benchmark sync times for various project sizes
- Memory usage profiling
- Network bandwidth utilization
- CPU usage during sync

## Monitoring and Observability

### Metrics to Track
- Sync duration
- Files transferred count
- Bytes transferred
- Error rates
- Retry counts

### Logging Strategy
- Structured JSON logging
- Log levels: DEBUG, INFO, WARN, ERROR
- Correlation IDs for sync operations
- Audit trail for file modifications

## Documentation Requirements

### User Documentation
- How to use sync commands
- Troubleshooting guide
- Performance tuning tips
- Migration from direct mount

### Developer Documentation
- Architecture overview
- API reference
- Extension points
- Contributing guide

## Success Criteria

1. **Functional**: Bidirectional sync works reliably
2. **Performance**: < 5 second overhead for typical projects
3. **Compatibility**: Works on Linux, macOS, and Windows
4. **Security**: No privilege escalation vulnerabilities
5. **Maintainability**: Clear separation of concerns
6. **Testability**: > 80% code coverage

## Next Steps

1. Review and approve this design
2. Create feature branch for implementation
3. Implement Phase 1 (basic infrastructure)
4. Test and iterate
5. Implement Phase 2 (background agent)
6. Performance testing and optimization
7. Documentation and rollout

## Open Questions

1. Should we support multiple sync profiles per project?
2. How should we handle symbolic links?
3. What's the preferred conflict resolution strategy?
4. Should sync be synchronous or asynchronous by default?
5. How do we handle partial sync failures?

## Risk Assessment

| Risk | Probability | Impact | Mitigation |
|------|------------|--------|------------|
| Data loss during sync | Low | High | Implement backup mechanism |
| Performance degradation | Medium | Medium | Add performance monitoring |
| Windows incompatibility | Low | High | Extensive Windows testing |
| Permission conflicts | Medium | Low | Clear documentation |
| Network interruptions | Low | Low | Retry mechanisms |

## Conclusion

This architecture leverages existing `container_sync.py` functionality while extending it for background agent requirements. By using a Python helper script instead of complex bash logic, we ensure maintainability, testability, and cross-platform compatibility. The phased implementation approach allows for iterative development and testing while maintaining backward compatibility.

---

## AUDIT FINDINGS: Functionality Changes and Issues

### Summary
The last agent implemented a significant architectural change from the original approach, replacing the entrypoint.sh logic completely. While this implements the planned workspace sync architecture, it has introduced **critical functionality gaps** that will prevent the container from working properly.

### Critical Issues Found

#### 1. **MAJOR: Missing User Context Switch**
- **Old behavior**: `exec sudo -u coder bash -c "cd /workspace && code-server ..."`
- **New behavior**: `os.execvp("code-server", [...])` runs as root
- **Impact**: Code-server now runs as root instead of the `coder` user, which will cause:
  - Permission issues with file access
  - Security concerns (running IDE as root)
  - Potential conflicts with code-server's expected user context

#### 2. **MAJOR: Missing Working Directory Setup**
- **Old behavior**: Explicitly changes to `/workspace` before starting code-server
- **New behavior**: No explicit working directory change, relies on process inheritance
- **Impact**: Code-server may not start in the correct working directory

#### 3. **MAJOR: Volume Mounting Strategy Changed**
- **Old behavior**: Direct mount `{docker_path}:/home/coder/project`
- **New behavior**: Indirect mount `{docker_path}:/host:rw` with rsync to `/workspace`
- **Impact**: This is intentional per the design, but creates a dependency on the sync working correctly

### Architecture Changes Implemented

#### Positive Changes
1. ✅ **Created comprehensive sync infrastructure**:
   - `workspace_sync.py`: Handles entrypoint initialization with proper environment validation
   - `bg.py`: Background agent for continuous sync with async operations and signal handling
   - Enhanced `container_sync.py` with better error handling and configuration

2. ✅ **Improved CLI integration**:
   - Added environment variables for background sync control
   - Changed volume mounting to support rsync architecture
   - Added sync-related environment variables

3. ✅ **Better separation of concerns**:
   - Moved complex logic out of bash into Python
   - Modular architecture with clear responsibilities
   - Comprehensive logging and error handling

#### Functionality Gaps
1. ❌ **User context not preserved** in code-server startup
2. ❌ **Working directory not explicitly set** for code-server
3. ❌ **No fallback mechanism** if sync fails during initialization
4. ❌ **No validation** that `coder` user exists before attempting to start code-server

### Specific Code Issues

#### In `workspace_sync.py:127-158` (`start_code_server` method):
```python
# This runs as root, but should run as coder user
os.execvp("code-server", [...])
```

**Should be:**
```python
# Need to switch to coder user like the original
os.setuid(coder_uid)  # or use subprocess with sudo -u coder
```

#### In `entrypoint.sh`:
```bash
# Old: Two-step process with explicit user switch
python3 /usr/local/bin/container-sync init
exec sudo -u coder bash -c "cd /workspace && code-server ..."

# New: Single Python process handles everything
exec python3 /usr/local/bin/workspace_sync.py init "$@"
```

### Impact Assessment

#### High Risk Issues:
1. **Container may fail to start properly** due to user permission issues
2. **Code-server running as root** poses security risks
3. **File ownership conflicts** between root and coder user

#### Medium Risk Issues:
1. **Working directory issues** may cause code-server to open wrong location
2. **Sync dependency** means container won't work if rsync fails

#### Low Risk Issues:
1. **Missing error handling** for edge cases in user switching
2. **No backward compatibility** with existing container expectations

### Recommendations

#### Immediate Fixes Required:
1. **Fix user context in `workspace_sync.py`**:
   - Add user switching logic before starting code-server
   - Ensure proper working directory is set
   - Handle case where `coder` user doesn't exist

2. **Add validation in `entrypoint_init()`**:
   - Verify `coder` user exists
   - Check that sync completed successfully before starting code-server
   - Add fallback mechanism if sync fails

3. **Test the new architecture**:
   - Verify container starts properly
   - Confirm code-server runs as expected user
   - Validate that sync operations work bidirectionally

#### Future Considerations:
1. Add integration tests to prevent regression of critical functionality
2. Consider phased rollout with feature flags
3. Document the architectural change for users

### Testing Status
The new code includes comprehensive test files:
- `tests/test_workspace_sync.py`
- `tests/test_bg.py`
- `tests/integration/test_workspace_sync_integration.py`

However, **critical functionality (user switching) is not properly implemented** in the main code, so these tests may not catch the real-world issues.

### Conclusion
While the agent successfully implemented the planned workspace sync architecture and created a more maintainable codebase, it introduced critical regressions in container startup functionality. The missing user context switch is a **blocking issue** that will prevent the container from working correctly in most environments.

## Phase 2: Update CLUD CLI
Modify `src/clud/cli.py` to:
- Add `--ui` flag to launch code-server
- Add `--port` flag (default: 8743, auto-detect if occupied)
- Add `--api-key` flag for Anthropic API key
- Remove any Docker image fetching logic
- Build image locally if not exists: `docker build -t niteris/clud:latest .`
- Launch container with proper mounts and environment:
  ```bash
  docker run -d \
    --name niteris-clud \
    -p <port>:8080 \
    -e ANTHROPIC_API_KEY=<key> \
    -e PASSWORD="" \
    -v $(pwd):/home/coder/project \
    -v ~/.config:/home/coder/.config \
    -v ~/.local:/home/coder/.local \
    niteris/clud:latest
  ```
- Auto-open browser to `http://localhost:<port>`

## Phase 3: MCP Server Configuration
Implement easy MCP server management:
- Default set: filesystem, git, fetch (always enabled)
- Optional servers via environment variable or config file
- Future: `--mcp-servers` flag to enable specific servers

## Notes
- Container name: `niteris-clud` (consider making configurable later)
- Data persistence via volume mounts to host directories
- No authentication by default (PASSWORD="") but keep capability for future
- Focus on developer experience - everything should "just work"
- **IMPORTANT:** code-server upstream uses Debian 12, but we'll use Ubuntu 25.04 for consistency with CLAUD_DOCKER.dockerfile
- code-server will be installed via their install script or .deb package on Ubuntu
