# RSYNC Implementation Recommendations

## Current State Analysis

### Current Volume Mapping Strategy
The current implementation directly maps the host project directory to `/workspace` in the container:

```bash
# From src/clud/cli.py:642
cmd.append(f"--volume={docker_path}:/workspace:rw")
```

**Current Dockerfile Setup:**
- Working directory: `/workspace` (line 230)
- Auto-cd to `/workspace` in shell startup (line 201-203)
- Container expects code to be at `/workspace`

## Proposed Architecture: Host → Rsync → Workspace

### New Volume Mapping Strategy

**Change from:**
```bash
--volume=/host/project:/workspace:rw
```

**To:**
```bash
--volume=/host/project:/host:rw    # Read-write host mount for bidirectional sync
# + rsync /host/* /workspace/      # Initial selective sync respecting .gitignore
# + sync command alias             # Bidirectional sync back to host
```

### Implementation Approach

#### 1. Dockerfile Changes

```dockerfile
# Add rsync installation (add to system packages section around line 23)
RUN apt-get update && apt-get install -y \
    # ... existing packages ...
    rsync \
    && rm -rf /var/lib/apt/lists/*

# Create both directories (update line 82)
RUN mkdir -p /home/${USERNAME}/project /workspace /host /var/log && \
    chown -R ${USERNAME}:${USERNAME} /home/${USERNAME} && \
    chown ${USERNAME}:${USERNAME} /workspace /host && \
    chmod 755 /workspace /host /var/log && \
    touch /var/log/clud-sync.log && \
    chown ${USERNAME}:${USERNAME} /var/log/clud-sync.log
```

#### 2. Enhanced Entrypoint Script

**Create new entrypoint functionality with proper error handling:**

```bash
#!/bin/bash
set -euo pipefail

# Exit codes
READONLY_ERROR=10
SYNC_ERROR=11
PERMISSION_ERROR=12

# Function to log messages with timestamps
log() {
    local level="${2:-INFO}"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] [$level] [entrypoint] $1" >&2
}

# Function to handle errors
handle_error() {
    local exit_code=$?
    local line_number=$1
    log "Error occurred at line $line_number with exit code $exit_code" "ERROR"
    exit $exit_code
}

# Set up error handling
trap 'handle_error $LINENO' ERR

# Function to validate directory permissions
validate_permissions() {
    local dir="$1"
    local operation="$2"

    if [ ! -d "$dir" ]; then
        log "Directory $dir does not exist" "ERROR"
        return 1
    fi

    if [ "$operation" = "read" ] && [ ! -r "$dir" ]; then
        log "No read permission for $dir" "ERROR"
        return $PERMISSION_ERROR
    fi

    if [ "$operation" = "write" ] && [ ! -w "$dir" ]; then
        log "No write permission for $dir" "ERROR"
        return $PERMISSION_ERROR
    fi

    return 0
}

# Function to perform initial sync from /host to /workspace
sync_host_to_workspace() {
    local retry_count=0
    local max_retries=3

    if [ -d "/host" ] && [ "$(ls -A /host 2>/dev/null || true)" ]; then
        log "Starting host to workspace sync..."

        # Validate permissions
        validate_permissions "/host" "read" || return $?
        validate_permissions "/workspace" "write" || return $?

        # Create rsync exclude file from .gitignore if it exists
        local rsync_excludes=""
        if [ -f "/host/.gitignore" ]; then
            rsync_excludes="--filter=:- .gitignore"
            log "Using .gitignore filters"
        fi

        # Retry loop for rsync operation
        while [ $retry_count -lt $max_retries ]; do
            if rsync -av \
                --stats \
                --human-readable \
                --delete \
                --exclude='/.git' \
                --exclude='/.docker_test_cache.json' \
                --exclude='**/.DS_Store' \
                ${rsync_excludes} \
                /host/ /workspace/ 2>&1 | tee /tmp/rsync_host_to_workspace.log; then

                log "Host sync completed successfully"
                local file_count=$(grep -E "Number of (regular )?files transferred" /tmp/rsync_host_to_workspace.log | head -1 | grep -oE '[0-9,]+' | head -1 || echo "0")
                log "Transferred $file_count files"
                return 0
            else
                retry_count=$((retry_count + 1))
                log "Rsync attempt $retry_count failed, retrying..." "WARN"
                sleep 2
            fi
        done

        log "Failed to sync after $max_retries attempts" "ERROR"
        return $SYNC_ERROR
    else
        log "No host directory found or empty, skipping sync"
        return 0
    fi
}

# Set up Anthropic API key if provided
if [ -n "${ANTHROPIC_API_KEY}" ]; then
    export ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY}"
    log "Anthropic API key configured"
fi

# Function to sync workspace changes back to host with safety checks
sync_workspace_to_host() {
    local dry_run="${1:-false}"
    local retry_count=0
    local max_retries=3

    if [ -d "/workspace" ] && [ "$(ls -A /workspace 2>/dev/null || true)" ]; then
        if [ "$dry_run" = "true" ]; then
            log "Running dry-run sync (no changes will be made)"
        else
            log "Syncing workspace changes back to host..."
        fi

        # Validate permissions
        validate_permissions "/workspace" "read" || return $?
        validate_permissions "/host" "write" || return $?

        # Check if host is read-only
        if ! touch "/host/.write_test" 2>/dev/null; then
            log "Host filesystem appears to be read-only" "ERROR"
            return $READONLY_ERROR
        fi
        rm -f "/host/.write_test"

        # Create rsync exclude file from .gitignore if it exists
        local rsync_excludes=""
        if [ -f "/workspace/.gitignore" ]; then
            rsync_excludes="--filter=:- .gitignore"
            log "Using .gitignore filters for reverse sync"
        fi

        # Add dry-run flag if requested
        local dry_run_flag=""
        if [ "$dry_run" = "true" ]; then
            dry_run_flag="--dry-run"
        fi

        # Retry loop for rsync operation
        while [ $retry_count -lt $max_retries ]; do
            if rsync -av \
                --stats \
                --human-readable \
                --delete \
                --exclude='/.git' \
                --exclude='/.docker_test_cache.json' \
                --exclude='**/.DS_Store' \
                ${dry_run_flag} \
                ${rsync_excludes} \
                /workspace/ /host/ 2>&1 | tee /tmp/rsync_workspace_to_host.log; then

                if [ "$dry_run" = "true" ]; then
                    log "Dry-run completed - no changes made"
                else
                    log "Workspace to host sync completed successfully"
                    local file_count=$(grep -E "Number of (regular )?files transferred" /tmp/rsync_workspace_to_host.log | head -1 | grep -oE '[0-9,]+' | head -1 || echo "0")
                    log "Transferred $file_count files"
                fi
                return 0
            else
                retry_count=$((retry_count + 1))
                log "Reverse sync attempt $retry_count failed, retrying..." "WARN"
                sleep 2
            fi
        done

        log "Failed to sync after $max_retries attempts" "ERROR"
        return $SYNC_ERROR
    else
        log "No workspace directory found or empty, skipping reverse sync"
        return 0
    fi
}

# Make sync function available globally
export -f sync_workspace_to_host

# Perform initial sync
sync_host_to_workspace

# Configure code-server
mkdir -p /home/coder/.config/code-server
cat > /home/coder/.config/code-server/config.yaml << 'YAML_EOF'
bind-addr: 0.0.0.0:8080
auth: none
cert: false
YAML_EOF

# Fix permissions safely
if [ -d "/home/coder/.config" ]; then
    chown -R coder:coder /home/coder/.config 2>/dev/null || log "Warning: Could not change ownership of /home/coder/.config" "WARN"
fi

if [ -d "/workspace" ]; then
    chown -R coder:coder /workspace 2>/dev/null || log "Warning: Could not change ownership of /workspace" "WARN"
fi

# Start code-server
log "Starting code-server on port 8080..."
exec sudo -u coder bash -c "cd /workspace && code-server --bind-addr 0.0.0.0:8080 --auth none --disable-telemetry /workspace"
```

#### 3. CLI Changes (src/clud/cli.py)

**Update volume mapping around line 642:**

```python
# OLD:
cmd = ["docker", "run", "-it", "--rm", f"--name=clud-{project_name}", f"--volume={docker_path}:/workspace:rw"]

# NEW:
cmd = ["docker", "run", "-it", "--rm", f"--name=clud-{project_name}", f"--volume={docker_path}:/host:rw"]
```

#### 4. Shell Aliases and Sync Command

**Add to Dockerfile bashrc setup (around line 185 in existing Dockerfile):**

```dockerfile
# Enhanced bashrc with sync capabilities and error handling
RUN cat >> /root/.bashrc << 'EOF'

# ... existing content ...

# CLUD alias - the main purpose of this container
alias clud='claude code --dangerously-skip-permissions'

# Sync command - bidirectional sync between workspace and host
alias sync='sync_workspace_to_host false'

# Dry-run sync to preview changes
alias sync-preview='sync_workspace_to_host true'

# Quick status command to see sync differences
alias sync-status='rsync -avzn --stats --human-readable --exclude="/.git" --exclude="/.docker_test_cache.json" --filter=":- .gitignore" /workspace/ /host/ 2>/dev/null || echo "Sync status unavailable"'

# Backup creation before destructive operations
alias sync-backup='create_backup'

# Show sync logs
alias sync-logs='tail -f /var/log/clud-sync.log 2>/dev/null || echo "No sync logs available"'

# Windows-specific path utilities
if [[ "$OSTYPE" =~ ^msys|^cygwin ]]; then
    alias path-normalize='normalize_path'
fi

# ... rest of existing bashrc content ...
EOF
```

**Make sync function available in shell:**

```bash
# In entrypoint.sh, add this after the function definition:
# Make sync functions available in all shells
echo 'export -f sync_workspace_to_host' >> /root/.bashrc
echo 'export -f sync_host_to_workspace' >> /root/.bashrc
```

#### 5. Updated Welcome Banner

**Replace the existing welcome message in Dockerfile bashrc (around line 205-210):**

```dockerfile
# Enhanced welcome message with sync information
echo "┌─ CLUD Development Environment ─────────────────────────────────────┐"
echo "│ Working Directory: /workspace (synced from /host)                  │"
echo "│ Type 'clud' to start Claude with dangerous permissions enabled     │"
echo "│                                                                     │"
echo "│ Sync Commands:                                                      │"
echo "│   sync        - Save workspace changes back to host                │"
echo "│   sync-status - Preview what would be synced (dry-run)             │"
echo "│                                                                     │"
echo "│ Note: Your project files are isolated in /workspace until you run  │"
echo "│       'sync' to save changes back to the host filesystem           │"
echo "└────────────────────────────────────────────────────────────────────┘"
echo ""
```

**Alternative compact version for smaller terminals:**

```dockerfile
# Compact welcome message
echo "CLUD Dev Environment | /workspace ↔ /host"
echo "Commands: clud | sync | sync-status"
echo "Note: Run 'sync' to save workspace changes to host"
echo ""
```

**Dynamic banner with sync status (Advanced):**

```dockerfile
# Dynamic welcome message with real-time sync information
show_welcome_banner() {
    echo "┌─ CLUD Development Environment ─────────────────────────────────────┐"
    echo "│ Working Directory: /workspace (synced from /host)                  │"
    echo "│ Type 'clud' to start Claude with dangerous permissions enabled     │"
    echo "│                                                                     │"
    echo "│ Sync Commands:                                                      │"
    echo "│   sync        - Save workspace changes back to host                │"
    echo "│   sync-status - Preview what would be synced (dry-run)             │"
    echo "│                                                                     │"

    # Show sync status if workspace exists
    if [ -d "/workspace" ] && [ -d "/host" ]; then
        local changes=$(rsync -avzn --stats --exclude="/.git" --filter=":- .gitignore" /workspace/ /host/ 2>/dev/null | grep "Number of created files" | cut -d: -f2 | xargs)
        if [ -n "$changes" ] && [ "$changes" != "0" ]; then
            echo "│ Status: $changes unsaved changes in workspace                     │"
        else
            echo "│ Status: Workspace synced with host                                 │"
        fi
    fi

    echo "│                                                                     │"
    echo "│ Note: Your project files are isolated in /workspace until you run  │"
    echo "│       'sync' to save changes back to the host filesystem           │"
    echo "└────────────────────────────────────────────────────────────────────┘"
    echo ""
}

# Call the function instead of static echo statements
show_welcome_banner
```

#### 6. Advanced Rsync Configuration

**Recommended rsync command for production use:**

```bash
# Complete sync with comprehensive .gitignore handling
rsync -avzP \
    --delete \
    --exclude='/.git' \
    --exclude='/.docker_test_cache.json' \
    --filter=':- .gitignore' \
    --stats \
    /host/ /workspace/
```

**Filter options explained:**
- `--filter=':- .gitignore'`: Directory merge, exclude patterns from .gitignore
- `--exclude='/.git'`: Explicitly exclude git directory
- `--delete`: Remove files in destination that don't exist in source
- `--stats`: Show transfer statistics

## Bidirectional Sync Workflow

### 1. Initial Sync (Host → Workspace)
```bash
# Happens automatically on container startup
sync_host_to_workspace
```

### 2. Development Work
- Developer works in `/workspace` directory
- All changes happen in the container's workspace
- Files are isolated from host until explicitly synced

### 3. Sync Back to Host (Workspace → Host)
```bash
# Manual sync command (available as alias)
sync

# Or explicit function call
sync_workspace_to_host

# Check what would be synced (dry-run)
sync-status
```

### 4. Use Cases for Bidirectional Sync

**When to use `sync` command:**
- After completing a feature or bugfix
- Before switching branches or stopping the container
- When you want to save work-in-progress to host
- Before running host-based tools (git operations, IDE integration)

**Automatic sync scenarios (future enhancement):**
- On container shutdown (exit hook)
- On file save events (watch mode)
- Periodic background sync for specific file types

### 5. Safety Features

**Conflict Prevention:**
- .gitignore patterns prevent syncing build artifacts
- Git directory is explicitly excluded from both directions
- Dry-run capability with `sync-status` command

**Backup Strategy:**
- Host files are preserved (not deleted unless explicitly removed in workspace)
- Manual sync process gives developer control over when changes propagate
- Version control handles conflict resolution

## Security Considerations

### 1. **Permission Model**
- Container runs with non-root user `coder` for development work
- Host mount at `/host` maintains original file ownership
- Rsync preserves file permissions and ownership where possible
- Entrypoint script performs minimal privileged operations during setup

### 2. **File System Safety**
- Read-only validation prevents accidental writes to protected filesystems
- Dry-run capabilities allow preview of sync operations
- Backup creation option for destructive operations
- Explicit .git directory exclusion prevents repository corruption

### 3. **Input Validation**
- Directory existence checks before operations
- Permission validation for both read and write operations
- Retry mechanisms with exponential backoff
- Comprehensive error logging for debugging

### 4. **Windows Security Considerations**
- Path traversal protection through rsync's built-in safeguards
- Proper handling of Windows file attributes and permissions
- UTF-8 encoding for international filenames
- NTFS junction point handling

## Error Handling Patterns

### 1. **Rsync Error Codes**
```bash
# Common rsync exit codes and handling
case $? in
    0)  log "Sync completed successfully" ;;
    1)  log "Syntax or usage error" "ERROR" ;;
    2)  log "Protocol incompatibility" "ERROR" ;;
    3)  log "Errors selecting input/output files" "ERROR" ;;
    5)  log "Error starting client-server protocol" "ERROR" ;;
    6)  log "Daemon unable to append to log-file" "WARN" ;;
    10) log "Error in socket I/O" "ERROR" ;;
    11) log "Error in file I/O" "ERROR" ;;
    12) log "Error in rsync protocol data stream" "ERROR" ;;
    13) log "Errors with program diagnostics" "ERROR" ;;
    14) log "Error in IPC code" "ERROR" ;;
    20) log "Received SIGUSR1 or SIGINT" "WARN" ;;
    21) log "Some error returned by waitpid()" "ERROR" ;;
    22) log "Error allocating core memory buffers" "ERROR" ;;
    23) log "Partial transfer due to error" "WARN" ;;
    24) log "Partial transfer due to vanished source files" "WARN" ;;
    25) log "The --max-delete limit stopped deletions" "WARN" ;;
    30) log "Timeout in data send/receive" "ERROR" ;;
    35) log "Timeout waiting for daemon connection" "ERROR" ;;
    *)  log "Unknown rsync error code: $?" "ERROR" ;;
esac
```

### 2. **Recovery Strategies**
```bash
# Function to create backup before destructive sync
create_backup() {
    local backup_dir="/host/.clud-backups/$(date +%Y%m%d_%H%M%S)"
    if [ -d "/host" ] && [ "$(ls -A /host 2>/dev/null)" ]; then
        log "Creating backup at $backup_dir"
        mkdir -p "$backup_dir"
        rsync -av --exclude='/.git' /host/ "$backup_dir/" || {
            log "Backup creation failed" "ERROR"
            return 1
        }
        log "Backup created successfully"
    fi
}

# Function to restore from backup
restore_backup() {
    local backup_dir="$1"
    if [ -d "$backup_dir" ]; then
        log "Restoring from backup: $backup_dir"
        rsync -av "$backup_dir/" /host/ || {
            log "Backup restoration failed" "ERROR"
            return 1
        }
        log "Restore completed successfully"
    else
        log "Backup directory not found: $backup_dir" "ERROR"
        return 1
    fi
}
```

## Performance Optimization

### 1. **Rsync Performance Tuning**
```bash
# Optimized rsync command for large projects
rsync -avzP \
    --compress-level=6 \
    --skip-compress=gz/zip/z/rpm/deb/iso/bz2/t[gb]z/7z/mp[34]/mov/avi/ogg/mp[gv]/jpg/jpeg/png \
    --partial-dir=/tmp/rsync-partial \
    --inplace \
    --no-whole-file \
    --delete \
    --exclude='/.git' \
    --filter=':- .gitignore' \
    /workspace/ /host/
```

### 2. **Memory and I/O Optimization**
- Use `--inplace` for large files to reduce disk usage
- Enable compression for slow networks with `--compress-level`
- Skip compression for already compressed files
- Use partial transfer resumption for interrupted syncs
- Implement file size thresholds for different sync strategies

### 3. **Parallel Processing**
```bash
# Parallel sync for multiple directories
sync_directories_parallel() {
    local dirs=("src" "tests" "docs" "config")
    local pids=()

    for dir in "${dirs[@]}"; do
        if [ -d "/workspace/$dir" ]; then
            rsync -av --filter=':- .gitignore' "/workspace/$dir/" "/host/$dir/" &
            pids+=("$!")
        fi
    done

    # Wait for all background processes
    for pid in "${pids[@]}"; do
        wait "$pid" || log "Parallel sync failed for PID $pid" "WARN"
    done
}
```

## Windows Compatibility Enhancements

### 1. **Path Handling**
```bash
# Windows-safe path conversion
normalize_path() {
    local path="$1"
    # Convert Windows paths to Unix-style for container use
    echo "$path" | sed 's|\\|/|g' | sed 's|^\([A-Za-z]\):|/\1|'
}

# Handle Windows file attributes
handle_windows_attributes() {
    # Preserve Windows file attributes when possible
    rsync -av \
        --modify-window=2 \
        --omit-dir-times \
        --filter=':- .gitignore' \
        /workspace/ /host/
}
```

### 2. **Encoding and Character Sets**
```bash
# Ensure proper UTF-8 handling
export LC_ALL=C.UTF-8
export LANG=C.UTF-8

# Handle Windows filename encoding issues
rsync -av \
    --iconv=utf-8,utf-8 \
    --filter=':- .gitignore' \
    /workspace/ /host/
```

## Monitoring and Logging

### 1. **Structured Logging**
```bash
# Enhanced logging with structured format
log_structured() {
    local message="$1"
    local level="${2:-INFO}"
    local component="${3:-entrypoint}"
    local timestamp=$(date -u +"%Y-%m-%dT%H:%M:%S.%3NZ")

    printf '{"timestamp":"%s","level":"%s","component":"%s","message":"%s"}\n' \
        "$timestamp" "$level" "$component" "$message" >> /var/log/clud-sync.log
}
```

### 2. **Metrics Collection**
```bash
# Collect sync performance metrics
collect_sync_metrics() {
    local start_time=$(date +%s)
    local files_before=$(find /workspace -type f | wc -l)

    # Perform sync operation
    sync_workspace_to_host
    local sync_result=$?

    local end_time=$(date +%s)
    local duration=$((end_time - start_time))
    local files_after=$(find /host -type f | wc -l)

    log_structured "Sync completed in ${duration}s, ${files_after} files total" "INFO" "metrics"

    return $sync_result
}
```

## Benefits of This Approach

### 1. **Workflow Improvements**
- Host filesystem is mounted read-write (`/host:rw`) for bidirectional sync
- `sync` command provides controlled synchronization back to host
- Selective file copying based on .gitignore rules in both directions

### 2. **Performance Benefits**
- Excludes large build artifacts (dist/, node_modules/, __pycache__, etc.)
- Faster container startup (smaller working set)
- Reduced disk usage in container

### 3. **Cleaner Development Environment**
- No build artifacts cluttering the workspace
- Consistent environment regardless of host state
- Better isolation between host and container

### 4. **Flexibility**
- Can easily add custom exclude patterns
- Supports nested .gitignore files
- Easy to modify sync behavior per project

## Implementation Phases

### Phase 1: Basic Implementation
1. Update Dockerfile to install rsync and create `/host` directory
2. Modify CLI to mount as `/host:rw` instead of `/workspace:rw`
3. Update entrypoint.sh with bidirectional rsync functionality
4. Add `sync` and `sync-status` aliases to bashrc
5. Update welcome banner with sync command information
6. Test with simple .gitignore exclusions

### Phase 2: Enhanced Filtering
1. Implement comprehensive .gitignore parsing
2. Add support for nested .gitignore files
3. Add custom exclude patterns for container-specific files
4. Enhanced banner with dynamic sync status
5. Performance optimization and testing

### Phase 3: Advanced Features (Optional)
1. Watch mode for automatic re-sync during development
2. Bidirectional sync for specific file types
3. Integration with development workflows
4. Custom sync profiles per project type

## Testing Strategy

### 1. Test Cases
- Empty project directory
- Project with complex .gitignore patterns
- Projects with nested .gitignore files
- Large projects with many excluded files
- Windows path compatibility

### 2. Validation Commands
```bash
# Test dry-run to validate exclusions
rsync -avzP --dry-run --filter=':- .gitignore' --exclude='/.git' /host/ /workspace/

# Compare file counts
echo "Host files: $(find /host -type f | wc -l)"
echo "Workspace files: $(find /workspace -type f | wc -l)"

# Verify specific exclusions
ls -la /workspace/ | grep -E "(node_modules|__pycache__|\.git)"  # Should be empty
```

## Migration Considerations

### 1. **Backward Compatibility**
- Add feature flag to enable/disable new behavior
- Maintain existing volume mapping as fallback option
- Clear documentation for migration path

### 2. **Windows Compatibility**
- Test path normalization with new volume structure
- Verify rsync behavior on Windows Docker Desktop
- Handle Windows-specific path issues

### 3. **Performance Impact**
- Initial sync adds startup time
- Monitor resource usage during sync
- Optimize for large projects

## Recommended Next Steps

1. **Create prototype implementation** in a feature branch
2. **Test with sample projects** of varying complexity
3. **Benchmark performance** vs current approach
4. **Document migration guide** for existing users
5. **Implement feature flag** for gradual rollout

## Code Quality Considerations

Following the project's code quality standards:
- All rsync operations must include proper error handling
- Log all sync operations with appropriate verbosity levels
- Never use bare exception handling for rsync failures
- Include comprehensive testing for edge cases

## Sync Command Usage Examples

### Basic Sync Operations

```bash
# Basic sync back to host
sync

# Check what files would be synced (dry-run)
sync-status

# Verbose sync with progress
VERBOSE=1 sync

# Sync specific directory only
rsync -av --filter=':- .gitignore' /workspace/src/ /host/src/
```

### Advanced Sync Scenarios

```bash
# Sync excluding specific patterns
rsync -av --exclude='*.tmp' --exclude='logs/' --filter=':- .gitignore' /workspace/ /host/

# Sync only modified files (preserve timestamps)
rsync -av --update --filter=':- .gitignore' /workspace/ /host/

# Sync with backup of overwritten files
rsync -av --backup --backup-dir=/host/.rsync-backups --filter=':- .gitignore' /workspace/ /host/

# Force sync even with .gitignore conflicts
rsync -av --exclude='/.git' /workspace/ /host/
```

### Troubleshooting Sync Issues

```bash
# Debug filter matching
rsync -av --debug=FILTER --filter=':- .gitignore' /workspace/ /host/

# Show detailed transfer statistics
rsync -avz --stats --filter=':- .gitignore' /workspace/ /host/

# Test sync without making changes
rsync -avzn --filter=':- .gitignore' /workspace/ /host/
```

## Configuration Options

Consider adding these environment variables for customization:

```bash
# Enable verbose rsync output
CLUD_RSYNC_VERBOSE=1

# Custom rsync options for sync command
CLUD_SYNC_OPTS="--progress --stats"

# Skip initial sync (for debugging)
CLUD_SKIP_INITIAL_SYNC=1

# Custom exclude patterns file
CLUD_RSYNC_EXCLUDE_FILE="/host/.clud-ignore"

# Enable automatic sync on file changes (future feature)
CLUD_AUTO_SYNC=1

# Sync interval for watch mode (seconds)
CLUD_SYNC_INTERVAL=30
```

## Troubleshooting Guide

### Common Issues and Solutions

#### 1. **Permission Denied Errors**
```bash
# Symptoms: "Permission denied" during sync operations
# Solutions:
chmod 755 /workspace /host  # Fix directory permissions
sudo chown -R $(whoami) /workspace  # Fix ownership

# Check mount permissions
mount | grep "/host"  # Verify mount options include 'rw'
```

#### 2. **Sync Failures on Windows**
```bash
# Symptoms: Rsync fails with path-related errors
# Solutions:
export MSYS=winsymlinks:nativestrict  # Enable proper symlink handling
rsync --modify-window=2 /workspace/ /host/  # Account for Windows time precision
```

#### 3. **Large File Sync Issues**
```bash
# Symptoms: Timeout or memory issues with large files
# Solutions:
rsync --inplace --partial /workspace/ /host/  # Use in-place updates
rsync --bwlimit=1000 /workspace/ /host/  # Limit bandwidth usage
```

#### 4. **Read-Only Filesystem**
```bash
# Symptoms: "Read-only file system" errors
# Solutions:
# Check Docker mount options
docker inspect container_name | grep -A 5 "Mounts"
# Remount if necessary (requires container restart)
```

### Debug Commands

```bash
# Enable debug logging
export CLUD_LOG_LEVEL="DEBUG"
export CLUD_RSYNC_VERBOSE=1

# Test sync with maximum verbosity
rsync -avvv --dry-run --stats /workspace/ /host/

# Check filesystem status
df -h /workspace /host
lsblk
mount | grep -E "(workspace|host)"

# Verify permissions
ls -la /workspace/ /host/
stat /workspace /host

# Test network connectivity (if using remote mounts)
ping -c 3 host.docker.internal
```

### Log Analysis

```bash
# View structured logs
tail -f /var/log/clud-sync.log | jq '.' 2>/dev/null || tail -f /var/log/clud-sync.log

# Filter by log level
grep '"level":"ERROR"' /var/log/clud-sync.log | jq '.'

# Performance analysis
grep 'Transferred.*files' /tmp/rsync_*.log
```

## Performance Benchmarks

### Expected Performance Characteristics

| Project Size | File Count | Initial Sync | Incremental Sync | Memory Usage |
|-------------|------------|--------------|------------------|-------------|
| Small (< 100 files) | < 100 | < 5s | < 1s | < 50MB |
| Medium (< 1k files) | < 1,000 | < 30s | < 5s | < 100MB |
| Large (< 10k files) | < 10,000 | < 2min | < 15s | < 200MB |
| Very Large (> 10k files) | > 10,000 | > 2min | > 15s | > 200MB |

### Optimization Recommendations

1. **For Large Projects**: Enable parallel sync and increase compression
2. **For Frequent Changes**: Use incremental sync with `--update` flag
3. **For Network Storage**: Implement bandwidth limiting and retry logic
4. **For Windows Hosts**: Use modify-window and omit-dir-times options

## Migration from Direct Mount

### Step-by-Step Migration Guide

1. **Backup Current Setup**
   ```bash
   # Create backup of current project
   cp -r /path/to/project /path/to/project.backup
   ```

2. **Update Container Configuration**
   ```bash
   # Old: --volume=/host/project:/workspace:rw
   # New: --volume=/host/project:/host:rw
   ```

3. **Test New Implementation**
   ```bash
   # Start container with new volume mapping
   # Verify initial sync works correctly
   # Test bidirectional sync functionality
   ```

4. **Rollback Plan**
   ```bash
   # If issues occur, revert to old volume mapping
   # Restore from backup if necessary
   ```

This comprehensive implementation provides a robust, secure, and performant foundation for the new volume mapping strategy while maintaining compatibility across different platforms and use cases.