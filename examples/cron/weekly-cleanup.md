# Weekly Cleanup Task

Perform system maintenance and cleanup tasks weekly.

## Instructions

1. **Clean temporary files**:
   - Find and delete files older than 7 days in `~/temp/` directory
   - Use command: `find ~/temp -type f -mtime +7 -delete`
   - Count number of files deleted
   - Calculate space freed

2. **Empty trash/recycle bin**:
   - Unix/Linux: `rm -rf ~/.local/share/Trash/*`
   - macOS: `rm -rf ~/.Trash/*`
   - Windows: Skip this step (or use PowerShell Clear-RecycleBin if available)
   - Log space freed

3. **Remove Docker unused images**:
   - Run: `docker image prune -a -f`
   - If Docker is not installed, skip this step
   - Parse output to determine space freed
   - Log number of images removed

4. **Clean package manager cache** (platform-specific):
   - Linux (apt): `sudo apt clean` (if sudo available)
   - Linux (dnf): `sudo dnf clean all` (if sudo available)
   - macOS (brew): `brew cleanup`
   - Skip if package manager not available or no sudo access

5. **Remove old log files**:
   - Find log files older than 90 days in `~/logs/`
   - Delete files matching `*.log` older than 90 days
   - Preserve recent logs (last 90 days)
   - Count files deleted and space freed

6. **Calculate total space freed**:
   - Sum up space freed from all cleanup operations
   - Format in human-readable format (MB or GB)

7. **Generate cleanup report**:
   - Create detailed report in `~/logs/cleanup-report-YYYY-MM-DD.log`
   - Include:
     - Timestamp of cleanup
     - Each cleanup operation and results
     - Total space freed
     - Any errors encountered
   - Also print summary to stdout

8. **Optional: Optimize databases**:
   - If you have databases that need maintenance (PostgreSQL VACUUM, MySQL OPTIMIZE, etc.)
   - Add commands here based on your setup

## Example Output

```
=== Weekly Cleanup Report: 2025-01-15T03:00:00 ===

[Temporary Files Cleanup]
Deleted 47 files older than 7 days from ~/temp/
Space freed: 125 MB

[Trash/Recycle Bin]
Emptied trash: 8 items
Space freed: 342 MB

[Docker Image Cleanup]
Removed 5 unused images
Space freed: 1.2 GB

[Package Manager Cache]
Cleaned apt cache
Space freed: 89 MB

[Old Log Files]
Deleted 12 log files older than 90 days from ~/logs/
Space freed: 67 MB

=== Summary ===
Total space freed: 1.82 GB
Cleanup completed successfully in 23 seconds

Full report saved to: ~/logs/cleanup-report-2025-01-15.log
=============================================
```

## Error Handling

- If a directory doesn't exist, skip that cleanup step and log warning
- If insufficient permissions for an operation, skip and log warning
- If Docker is not available, skip Docker cleanup silently
- If no files found for cleanup, note "Nothing to clean" for that step
- Continue with all steps even if some fail

## Pre-Cleanup Checks

- Verify sufficient disk space before starting (at least 1GB free)
- Don't delete files if disk is critically low (<500MB) - log warning and skip

## Schedule Recommendation

Run this task weekly on Sunday at 3:00 AM:
```bash
clud --cron add "0 3 * * 0" examples/cron/weekly-cleanup.md
```

Or monthly on the 1st at midnight:
```bash
clud --cron add "0 0 1 * *" examples/cron/weekly-cleanup.md
```
