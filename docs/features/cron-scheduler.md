# Cron Scheduler (`clud --cron`)

The `clud --cron` feature enables automated execution of tasks on recurring schedules using standard cron expressions. Run tasks unattended for backups, reports, monitoring, maintenance, and more.

## Key Features

- Standard cron syntax (5-field expressions)
- Cross-platform daemon (Linux, macOS, Windows)
- Automatic retry with exponential backoff (handles transient failures)
- Comprehensive logging (`~/.clud/logs/cron/`)
- Crash recovery (validates tasks, recalculates stale times on restart)
- Autostart on system boot (systemd, launchd, Task Scheduler)
- Zero configuration (works out-of-box with sensible defaults)
- No admin/root permissions required

## Quick Start

```bash
# 1. Create a task file
echo "Generate daily sales report from database" > daily-report.md

# 2. Schedule it (daily at 9 AM)
clud --cron add "0 9 * * *" daily-report.md

# 3. Start the scheduler daemon
clud --cron start

# 4. (Optional) Enable autostart on system boot
clud --cron install

# 5. Check status
clud --cron status
```

## Commands

| Command | Purpose | Example |
|---------|---------|---------|
| `add` | Schedule new task | `clud --cron add "0 9 * * *" task.md` |
| `list` | Show all tasks | `clud --cron list` |
| `remove` | Delete task | `clud --cron remove <task-id>` |
| `start` | Start daemon | `clud --cron start` |
| `stop` | Stop daemon | `clud --cron stop` |
| `status` | Show daemon/tasks | `clud --cron status` |
| `install` | Enable autostart | `clud --cron install` |

## Cron Expression Syntax

Cron expressions use 5 fields: `minute hour day_of_month month day_of_week`

### Field Ranges

- Minute: 0-59
- Hour: 0-23
- Day of month: 1-31
- Month: 1-12 (or JAN-DEC)
- Day of week: 0-6 (0=Sunday, or SUN-SAT)

### Special Characters

- `*` - Any value (every minute/hour/day/etc.)
- `,` - List of values (e.g., `1,15,30`)
- `-` - Range of values (e.g., `1-5`)
- `/` - Step values (e.g., `*/15` = every 15 minutes)

### Common Examples

```bash
# Every minute
* * * * *

# Every hour at 30 minutes past
30 * * * *

# Every day at 9:00 AM
0 9 * * *

# Every weekday (Mon-Fri) at 8:30 AM
30 8 * * 1-5

# Every 15 minutes
*/15 * * * *

# First day of every month at midnight
0 0 1 * *

# Every Sunday at 2:00 AM
0 2 * * 0
```

## Task Files

Task files are simple markdown files containing instructions for `clud` to execute. The file describes what you want Claude Code to do.

### Example Task Files

```markdown
# daily-backup.md
Create a backup of all Python files in ~/projects to ~/backups/YYYY-MM-DD.tar.gz
Include timestamp in the backup filename
Log the backup size and file count
```

```markdown
# hourly-report.md
Generate a status report:
1. Check disk usage with df -h
2. List running Docker containers
3. Count active user sessions
4. Append results to ~/logs/system-status.log with timestamp
```

```markdown
# weekly-cleanup.md
Clean up temporary files:
1. Delete files older than 7 days from ~/temp/
2. Empty trash/recycle bin
3. Remove Docker unused images (docker image prune -a -f)
4. Report space freed
```

### Best Practices

- Keep instructions clear and specific
- Include validation steps (e.g., "verify backup was created")
- Specify absolute paths (avoid relative paths like `./data`)
- Add error handling instructions (e.g., "if backup fails, send notification")
- Use meaningful filenames (e.g., `hourly-db-backup.md` not `task1.md`)

## Daemon Management

### Starting the Daemon

```bash
# Start daemon in background
clud --cron start

# Verify it's running
clud --cron status
```

The daemon runs in the background and checks for due tasks every minute. It automatically detaches from your terminal, so you can close your shell without stopping the scheduler.

### Stopping the Daemon

```bash
# Graceful shutdown
clud --cron stop

# Verify it stopped
clud --cron status
```

### Daemon Logs

- Main log: `~/.clud/logs/cron-daemon.log` (daemon lifecycle, task scheduling)
- Task logs: `~/.clud/logs/cron/{task-id}/{timestamp}.log` (output from each task execution)

### Log Rotation

- Daemon log: 10MB max, 5 backups (total ~50MB)
- Task logs: Not rotated (one log file per execution)

## Autostart Configuration

Enable the daemon to start automatically when your system boots:

```bash
# Install autostart configuration
clud --cron install

# Verify installation
clud --cron status  # Shows "Autostart: Enabled"
```

### Platform-Specific Methods

| Platform | Primary Method | Fallback Method |
|----------|---------------|-----------------|
| **Linux** | systemd user unit (`~/.config/systemd/user/clud-cron.service`) | crontab `@reboot` entry |
| **macOS** | launchd user agent (`~/Library/LaunchAgents/com.clud.cron.plist`) | Login Items (AppleScript) |
| **Windows** | Task Scheduler (user-level task) | Registry Run key (`HKCU\...\Run`) |

### Installation Behavior

1. Tries primary method first (systemd, launchd, Task Scheduler)
2. If primary fails, automatically tries fallback method
3. Reports which method was used
4. No admin/root permissions required (user-level only)

### Verification

```bash
# Linux - check systemd unit
systemctl --user status clud-cron

# macOS - check launchd job
launchctl list | grep clud

# Windows - check Task Scheduler
schtasks /query /tn CludCron
```

### Manual Uninstall

```bash
# Linux (systemd)
systemctl --user stop clud-cron
systemctl --user disable clud-cron
rm ~/.config/systemd/user/clud-cron.service

# Linux (crontab fallback)
crontab -e  # Remove the @reboot line

# macOS (launchd)
launchctl unload ~/Library/LaunchAgents/com.clud.cron.plist
rm ~/Library/LaunchAgents/com.clud.cron.plist

# Windows (Task Scheduler)
schtasks /delete /tn CludCron /f

# Windows (Registry fallback)
# Open regedit, delete: HKCU\Software\Microsoft\Windows\CurrentVersion\Run\CludCron
```

## Error Handling and Retry

### Automatic Retry

Tasks that fail are automatically retried up to 3 times with exponential backoff:
1. Initial attempt fails → Wait 2 seconds
2. Retry 1 fails → Wait 4 seconds
3. Retry 2 fails → Wait 8 seconds
4. Retry 3 fails → Mark as failed, log error

### Failure Tracking

- Each failure increments the task's `consecutive_failures` counter
- After 3 consecutive failures, the task is automatically disabled
- A successful execution resets the counter to 0
- Disabled tasks remain in the schedule but don't execute

### Re-enabling Failed Tasks

```bash
# Remove the disabled task
clud --cron remove task-abc-123

# Fix the issue (update task file, fix permissions, etc.)
# ...

# Re-add the task (fresh start, counters reset)
clud --cron add "0 9 * * *" fixed-task.md
```

### Common Failure Causes

- Task file deleted or moved
- Network connectivity issues
- Permission denied (file/directory access)
- Disk full (can't write logs)
- Invalid task instructions (Claude Code can't understand)

### Crash Recovery

When the daemon restarts (after crash or reboot):
1. Validates all task files exist (logs warnings for missing files)
2. Recalculates `next_run` times for tasks that are in the past
3. Skips disabled tasks (preserves disable state)
4. Resumes normal scheduling

This prevents "execution bursts" where missed tasks all run at once after downtime.

## Monitoring and Logs

### Check Daemon Status

```bash
clud --cron status
```

Output shows:
- Daemon state (running/stopped/stale)
- Daemon PID and uptime
- Number of tasks (enabled/disabled)
- Autostart configuration status
- Recent activity summary

### List Scheduled Tasks

```bash
clud --cron list
```

Output shows for each task:
- Task ID (unique identifier)
- Cron expression (schedule)
- Task file path
- Next run time (absolute and relative)
- Status (enabled/disabled)
- Consecutive failures count

### View Logs

```bash
# Daemon logs (startup, scheduling, errors)
cat ~/.clud/logs/cron-daemon.log
tail -f ~/.clud/logs/cron-daemon.log  # Follow live

# Task execution logs (per task, per execution)
ls ~/.clud/logs/cron/task-abc-123/
cat ~/.clud/logs/cron/task-abc-123/2025-01-15_090000.log

# Find recent task executions
ls -lt ~/.clud/logs/cron/*/  # Sorted by modification time
```

### Log Format

```
2025-01-15 09:00:00 [INFO] Daemon starting main loop...
2025-01-15 09:00:00 [INFO] Performing crash recovery checks...
2025-01-15 09:00:00 [INFO] Crash recovery complete
2025-01-15 09:00:15 [INFO] Checking for due tasks...
2025-01-15 09:00:15 [INFO] Found 1 task(s) due for execution
2025-01-15 09:00:15 [INFO] [Task task-abc-123] Starting execution: daily-report.md
2025-01-15 09:00:42 [INFO] [Task task-abc-123] ✓ Completed successfully (duration: 27.3s)
```

## Troubleshooting

### Daemon won't start

```bash
# Check if already running
clud --cron status

# Check daemon logs for errors
cat ~/.clud/logs/cron-daemon.log

# Try stopping and restarting
clud --cron stop
clud --cron start

# Verify PID file isn't stale
rm ~/.clud/cron.pid  # If daemon is definitely not running
clud --cron start
```

### Task not executing

```bash
# Verify task is enabled
clud --cron list  # Look for "disabled" status

# Check next run time
clud --cron list  # Ensure next_run is in the future

# Verify daemon is running
clud --cron status

# Check for consecutive failures
clud --cron list  # Task may be auto-disabled after 3 failures

# Remove and re-add task to reset
clud --cron remove task-abc-123
clud --cron add "0 9 * * *" daily-report.md
```

### Invalid cron expression

```bash
# Error: "Invalid cron expression: '0 25 * * *' - hour must be 0-23"
# Fix: Use valid hour (0-23)
clud --cron add "0 9 * * *" task.md

# Use online tools to validate expressions:
# - https://crontab.guru/
# - https://crontab.cronhub.io/
```

### Permission errors

```bash
# Error: "Permission denied: /path/to/task.md"
# Fix: Ensure task file is readable
chmod +r /path/to/task.md

# Error: "Permission denied: ~/.clud/logs/cron/"
# Fix: Ensure log directory is writable
chmod +w ~/.clud/logs/cron/
```

### Autostart not working

```bash
# Reboot system and check status
clud --cron status

# Linux - verify systemd unit
systemctl --user status clud-cron
journalctl --user -u clud-cron  # Check logs

# macOS - verify launchd job
launchctl list | grep clud
cat ~/Library/LaunchAgents/com.clud.cron.plist  # Check config

# Windows - verify Task Scheduler task
schtasks /query /tn CludCron /v

# If autostart failed, check which method was used
cat ~/.clud/logs/cron-daemon.log | grep -i "autostart\|install"
```

### Stale PID file

```bash
# Daemon reports "running" but isn't
clud --cron status  # Shows "stale" status

# Automatically cleaned up by `clud --cron start`
clud --cron start  # Detects stale PID, cleans up, starts fresh
```

### Disk full

```bash
# Check disk space
df -h ~/.clud/logs/

# Remove old task logs
rm -rf ~/.clud/logs/cron/task-*/  # Keep only recent logs

# Daemon logs are auto-rotated (10MB max, 5 backups)
# Task logs are NOT rotated (manual cleanup required)
```

## Configuration Files

### Config Location

`~/.clud/cron.json`

### Config Structure

```json
{
  "tasks": [
    {
      "id": "task-abc-123",
      "cron_expression": "0 9 * * *",
      "task_file_path": "/home/user/daily-report.md",
      "enabled": true,
      "created_at": 1705305600.0,
      "last_run": 1705392000.0,
      "next_run": 1705478400.0,
      "consecutive_failures": 0,
      "last_failure_time": null
    }
  ],
  "daemon_pid": 12345
}
```

### Manual Editing

- NOT recommended (use `clud --cron` commands instead)
- If you must edit: Stop daemon first (`clud --cron stop`)
- Restart daemon after editing (`clud --cron start`)
- Invalid JSON will prevent daemon from starting

### Backup Config

```bash
# Before making changes
cp ~/.clud/cron.json ~/.clud/cron.json.backup

# Restore if needed
mv ~/.clud/cron.json.backup ~/.clud/cron.json
```

## Performance Characteristics

### Resource Usage (measured with 10 scheduled tasks, optimized in v1.0.34+)

- **CPU (idle)**: <0.1% (daemon sleeps intelligently until next task)
- **Memory (idle)**: ~40-50MB (Python interpreter + dependencies + psutil)
- **Disk I/O**: Minimal (writes logs only during task execution and every 5 minutes for resource profiling)

### Scheduler Behavior (optimized in v1.0.34+)

- **Intelligent sleep**: Daemon sleeps until next scheduled task (max 1 hour for responsiveness)
- **No wasteful polling**: Previous versions checked every 60 seconds regardless of schedule
- **Immediate execution**: Task execution starts within 1 second of scheduled time (was 60 seconds)
- **Resource profiling**: Logs CPU/memory usage every 5 minutes for monitoring
- **Concurrent execution**: Multiple tasks can run concurrently (no queue limit)
- **No timeouts**: Tasks run until completion or failure (no artificial time limits)

### Resource Monitoring

- Daemon logs initial resource usage on startup
- Periodic profiling every 5 minutes (CPU %, Memory MB, Uptime, Cycle count)
- Final resource usage logged on shutdown
- View current resource usage with `clud --cron status` (shows CPU % and Memory MB)

### Scaling Limits (not enforced, for reference)

- Recommended: <100 tasks (config load/save is O(n))
- Recommended: <10 concurrent executions (system resource limits)
- Log storage: Plan for ~1-10MB per task execution (depends on output)

### Performance Improvements (v1.0.34+)

- ✨ Daemon sleeps until next task instead of fixed 60-second polling (up to 60x reduction in wake cycles)
- ✨ Task execution responsiveness improved from ~60s to ~1s
- ✨ Resource profiling with psutil (CPU, memory, uptime tracking)
- ✨ Progress spinners for long operations (daemon start, autostart install)
- ✨ Next run time displayed as both absolute and relative ("in 2 hours")

## Example Workflows

### Daily Database Backup

```bash
# 1. Create task file
cat > db-backup.md << 'EOF'
Create a backup of the PostgreSQL database:
1. Run: pg_dump mydatabase > ~/backups/db-$(date +%Y%m%d).sql
2. Compress: gzip ~/backups/db-$(date +%Y%m%d).sql
3. Verify the backup file exists and is >1MB
4. Delete backups older than 30 days from ~/backups/
5. Log the backup size and timestamp to ~/logs/backup.log
EOF

# 2. Schedule for 2 AM daily
clud --cron add "0 2 * * *" db-backup.md

# 3. Start daemon (if not running)
clud --cron start

# 4. Enable autostart
clud --cron install
```

### Hourly System Monitoring

```bash
# 1. Create task file
cat > system-monitor.md << 'EOF'
Check system health and log status:
1. Check CPU usage: top -bn1 | head -5
2. Check memory: free -h
3. Check disk space: df -h
4. Check load average: uptime
5. Append results to ~/logs/system-health.log with timestamp
6. If disk usage >90%, highlight in the log
EOF

# 2. Schedule for every hour
clud --cron add "0 * * * *" system-monitor.md

# 3. Start daemon
clud --cron start
```

### Weekday Morning Standup Report

```bash
# 1. Create task file
cat > standup-report.md << 'EOF'
Generate daily standup report:
1. Git: List commits from yesterday in ~/projects/ repos
2. Jira: Count open tickets assigned to me (use API)
3. Calendar: List today's meetings (parse .ics file)
4. Format as markdown and email to team@company.com
5. Save copy to ~/reports/standup-$(date +%Y%m%d).md
EOF

# 2. Schedule for weekdays at 8:30 AM
clud --cron add "30 8 * * 1-5" standup-report.md

# 3. Start daemon
clud --cron start
```

### Weekly Cleanup

```bash
# 1. Create task file
cat > weekly-cleanup.md << 'EOF'
Perform weekly system cleanup:
1. Delete temp files older than 7 days: find ~/temp -mtime +7 -delete
2. Clear browser cache: rm -rf ~/.cache/google-chrome/
3. Remove Docker unused images: docker image prune -a -f
4. Empty trash: rm -rf ~/.local/share/Trash/*
5. Run apt autoremove (if Linux): sudo apt autoremove -y
6. Log space freed to ~/logs/cleanup.log
EOF

# 2. Schedule for Sunday at 3 AM
clud --cron add "0 3 * * 0" weekly-cleanup.md

# 3. Start daemon
clud --cron start
```

### Every 15 Minutes: API Health Check

```bash
# 1. Create task file
cat > api-health.md << 'EOF'
Check API health and log status:
1. Ping API endpoint: curl -s https://api.example.com/health
2. Parse JSON response and extract status field
3. If status != "ok", send alert notification (email or webhook)
4. Log response time and status to ~/logs/api-health.log
5. Keep only last 24 hours of logs (delete older entries)
EOF

# 2. Schedule every 15 minutes
clud --cron add "*/15 * * * *" api-health.md

# 3. Start daemon
clud --cron start
```

## Advanced Usage

### Multiple Schedules for Same Task

```bash
# Run backup at 2 AM and 2 PM
clud --cron add "0 2 * * *" backup.md   # Morning backup
clud --cron add "0 14 * * *" backup.md  # Afternoon backup

# Both tasks execute independently with separate logs
```

### Conditional Execution

```markdown
# conditional-task.md
Execute task only if conditions are met:
1. Check if file ~/data/input.csv exists
2. If not exists, skip and log "No input file found"
3. If exists, process the CSV and generate report
4. Move processed file to ~/data/archive/
5. Log completion status
```

### Chained Tasks

```markdown
# task-1-fetch.md
Download data from API and save to ~/data/raw.json

# task-2-process.md
Process ~/data/raw.json and generate ~/data/processed.csv

# task-3-report.md
Generate report from ~/data/processed.csv and email results
```

```bash
# Schedule in sequence (staggered times)
clud --cron add "0 1 * * *" task-1-fetch.md    # 1:00 AM
clud --cron add "15 1 * * *" task-2-process.md # 1:15 AM
clud --cron add "30 1 * * *" task-3-report.md  # 1:30 AM
```

### Environment Variables

```markdown
# task-with-env.md
Execute task with specific environment:
1. Export API_KEY from ~/.secrets/api-key.txt
2. Run script: python ~/scripts/fetch-data.py
3. Unset API_KEY after completion
4. Log results to ~/logs/fetch-data.log
```

## Integration with Other Tools

### Git Operations

```markdown
# git-sync.md
Sync repositories and push changes:
1. cd ~/projects/repo1 && git pull origin main
2. cd ~/projects/repo2 && git pull origin main
3. If changes detected, commit with message: "Auto-sync $(date)"
4. git push origin main (for both repos)
5. Log sync status to ~/logs/git-sync.log
```

### Docker Management

```markdown
# docker-cleanup.md
Clean up Docker resources:
1. docker system prune -a -f --volumes
2. docker image prune -a -f
3. docker container prune -f
4. docker volume prune -f
5. Log space freed (compare df -h before/after)
```

### Webhook Notifications

```markdown
# notify-webhook.md
Send webhook notification with system status:
1. Collect system metrics (CPU, memory, disk, load)
2. Format as JSON payload
3. POST to webhook URL: https://hooks.example.com/system-status
4. Log response status code
5. If webhook fails, log error (don't retry)
```

### Email Reports

```markdown
# email-report.md
Generate and email weekly report:
1. Query database for weekly statistics
2. Generate markdown report with charts (use matplotlib)
3. Convert markdown to HTML (use pandoc)
4. Send email via SMTP (use ~/.secrets/smtp.conf)
5. Save copy to ~/reports/weekly-$(date +%Y%m%d).html
```

## Security Considerations

### Task File Permissions

- Task files should be readable only by your user (chmod 600)
- Don't store sensitive data (passwords, API keys) in task files
- Use external secrets management (e.g., `~/.secrets/` directory)

### Log File Sensitivity

- Task logs may contain sensitive output (API responses, database data)
- Review log retention policy (delete old logs regularly)
- Consider encrypting sensitive logs (use gpg or similar)

### Daemon Privileges

- Daemon runs as your user (no elevated privileges)
- Tasks execute with your user's permissions
- Cannot modify system files or other users' files

### Autostart Security

- Autostart uses absolute paths to prevent hijacking
- systemd/launchd units are user-level (not system-wide)
- Task Scheduler tasks run as your user (not SYSTEM)

### Network Operations

- Tasks can make network requests (use with caution)
- Consider firewall rules for scheduled tasks
- Validate external data before processing

## Migration and Backup

### Export Configuration

```bash
# Backup entire cron setup
tar -czf clud-cron-backup.tar.gz ~/.clud/cron.json ~/.clud/logs/cron/

# Copy to another machine
scp clud-cron-backup.tar.gz user@remote:~/
```

### Import Configuration

```bash
# On new machine
tar -xzf clud-cron-backup.tar.gz -C ~/

# Verify tasks
clud --cron list

# Update paths if needed (different home directory)
# Edit ~/.clud/cron.json and update task_file_path fields

# Start daemon
clud --cron start

# Install autostart
clud --cron install
```

### Reset Everything

```bash
# Stop daemon
clud --cron stop

# Remove all config and logs
rm -rf ~/.clud/cron.json ~/.clud/cron.pid ~/.clud/logs/cron/

# Remove autostart (platform-specific)
# See "Manual Uninstall" section above

# Start fresh
clud --cron add "0 9 * * *" new-task.md
clud --cron start
```

## Related Documentation

- [Pipe Mode](pipe-mode.md)
- [Web UI](webui.md)
- [Development Setup](../development/setup.md)
