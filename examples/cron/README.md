# Cron Task Examples

This directory contains example task files for `clud --cron`. These examples demonstrate common use cases and best practices for scheduled tasks.

## Available Examples

### 1. `daily-backup.md` - Daily Backup Task
**Purpose**: Create daily backups of project files
**Schedule**: Daily at 2:00 AM
**Use case**: Automated backups, file archiving, disaster recovery

**Features**:
- Backs up all Python files from projects directory
- Creates timestamped tar.gz archives
- Automatically cleans up old backups (>30 days)
- Logs backup results with size and file count

**Setup**:
```bash
# Schedule the backup
clud --cron add "0 2 * * *" examples/cron/daily-backup.md

# Start the daemon
clud --cron start
```

### 2. `hourly-report.md` - Hourly System Status Report
**Purpose**: Monitor system health and resource usage
**Schedule**: Every hour at :00
**Use case**: System monitoring, health checks, capacity planning

**Features**:
- Checks disk usage with warnings for >90% full
- Lists running Docker containers
- Counts active user sessions
- Reports system load average
- Tracks memory usage

**Setup**:
```bash
# Schedule hourly monitoring
clud --cron add "0 * * * *" examples/cron/hourly-report.md

# Or every 30 minutes
clud --cron add "*/30 * * * *" examples/cron/hourly-report.md
```

### 3. `weekly-cleanup.md` - Weekly Cleanup Task
**Purpose**: Automated system maintenance and cleanup
**Schedule**: Weekly on Sunday at 3:00 AM
**Use case**: Disk space management, cache cleanup, maintenance

**Features**:
- Removes old temporary files (>7 days)
- Empties trash/recycle bin
- Cleans up unused Docker images
- Removes old log files (>90 days)
- Reports total space freed

**Setup**:
```bash
# Schedule weekly cleanup
clud --cron add "0 3 * * 0" examples/cron/weekly-cleanup.md
```

## Quick Start

1. **Install clud** (if not already installed):
   ```bash
   pip install clud
   ```

2. **Copy and customize an example**:
   ```bash
   cp examples/cron/daily-backup.md my-backup.md
   # Edit my-backup.md to match your needs
   ```

3. **Schedule the task**:
   ```bash
   clud --cron add "0 2 * * *" my-backup.md
   ```

4. **Start the daemon**:
   ```bash
   clud --cron start
   ```

5. **Verify it's working**:
   ```bash
   clud --cron status
   clud --cron list
   ```

6. **(Optional) Enable autostart**:
   ```bash
   clud --cron install
   ```

## Customizing Examples

These examples are templates - customize them for your specific needs:

### Changing Paths
Replace example paths with your actual directories:
- `~/projects/` → Your actual project directory
- `~/backups/` → Your backup location
- `~/logs/` → Your log directory

### Adjusting Schedules
Modify cron expressions to match your schedule:
- `0 2 * * *` - Daily at 2:00 AM
- `*/15 * * * *` - Every 15 minutes
- `30 8 * * 1-5` - Weekdays at 8:30 AM
- `0 0 1 * *` - First day of every month at midnight

Use [crontab.guru](https://crontab.guru/) to build custom schedules.

### Adding Error Notifications
Add notification steps to your tasks:
```markdown
7. **Send notification on error**:
   - If any step fails, send email or webhook notification
   - Include error details and timestamp
   - Use curl to POST to webhook: curl -X POST https://hooks.example.com/notify
```

### Integrating with Your Tools
Extend examples with your specific tools:
- Database backups (PostgreSQL, MySQL, MongoDB)
- Cloud storage sync (AWS S3, Google Cloud Storage)
- Git repository operations (pull, commit, push)
- API health checks
- Performance metric collection

## Testing Tasks

Before scheduling a task, test it manually:

```bash
# Test with clud directly
clud -f examples/cron/daily-backup.md

# Or with Claude Code directly
claude -f examples/cron/daily-backup.md
```

## Monitoring Tasks

After scheduling, monitor execution:

```bash
# Check daemon status
clud --cron status

# List all tasks
clud --cron list

# View daemon logs
tail -f ~/.clud/logs/cron-daemon.log

# View task execution logs
ls -lt ~/.clud/logs/cron/*/
cat ~/.clud/logs/cron/task-abc-123/2025-01-15_020000.log
```

## Common Patterns

### Conditional Execution
```markdown
1. Check if condition is met (file exists, service is up, etc.)
2. If condition false, log "Skipping task" and exit
3. If condition true, proceed with task execution
```

### Error Recovery
```markdown
1. Try primary operation
2. If fails, try alternative approach
3. If still fails, log error and send notification
4. Always log final status (success/failure)
```

### Chained Tasks
Schedule multiple tasks in sequence:
```bash
clud --cron add "0 1 * * *" task-1-fetch.md    # 1:00 AM
clud --cron add "15 1 * * *" task-2-process.md # 1:15 AM
clud --cron add "30 1 * * *" task-3-report.md  # 1:30 AM
```

### Parallel Execution
Schedule multiple independent tasks at same time:
```bash
clud --cron add "0 2 * * *" backup-db.md
clud --cron add "0 2 * * *" backup-files.md
clud --cron add "0 2 * * *" backup-config.md
# All run at 2:00 AM concurrently
```

## Best Practices

1. **Use absolute paths**: Always use full paths (`~/projects/`, not `./projects/`)
2. **Test thoroughly**: Test tasks manually before scheduling
3. **Handle errors**: Always include error handling in tasks
4. **Log results**: Log success/failure for debugging
5. **Clean up logs**: Periodically remove old log files
6. **Monitor execution**: Check logs regularly for failures
7. **Use meaningful names**: Name task files descriptively
8. **Document dependencies**: Note required tools (docker, git, etc.)
9. **Set expectations**: Document expected runtime and output
10. **Version control**: Keep task files in git for history

## Troubleshooting

**Task not executing?**
```bash
# Check task status
clud --cron list

# Verify daemon is running
clud --cron status

# Check logs for errors
cat ~/.clud/logs/cron-daemon.log
```

**Task failing repeatedly?**
```bash
# View task execution log
cat ~/.clud/logs/cron/task-abc-123/latest.log

# Task may be auto-disabled after 3 failures
clud --cron list  # Look for "disabled" status

# Fix issue and reschedule
clud --cron remove task-abc-123
# Fix the task file
clud --cron add "0 2 * * *" fixed-task.md
```

**Need help?**
- Read full documentation in `CLAUDE.md`
- Check troubleshooting section in docs
- Review task execution logs for error details

## Contributing

Have a useful task example? Consider sharing it!

1. Create a new `.md` file with clear instructions
2. Test the task thoroughly
3. Document the schedule, use case, and dependencies
4. Add entry to this README
5. Submit a pull request

## License

These examples are provided as-is for educational and practical use. Customize and use them freely in your projects.
