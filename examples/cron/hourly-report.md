# Hourly System Status Report

Generate a comprehensive system status report every hour.

## Instructions

1. **Check disk usage**: Run `df -h` to check disk space on all mounted filesystems
   - Highlight any filesystem with >90% usage
   - Format output in human-readable format

2. **List running Docker containers**: Run `docker ps --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}"`
   - If Docker is not installed or not running, note this in the report
   - If no containers are running, note "No containers running"

3. **Count active user sessions**:
   - Unix/Linux: Use `who | wc -l` to count active sessions
   - Windows: Use appropriate command or skip this check
   - Report the number of active users

4. **Check system load**: Run `uptime` to get system load average
   - Parse the load average (1min, 5min, 15min)
   - Note if load average is high (>2.0 on single-core system)

5. **Memory usage**: Run `free -h` to check memory usage
   - Report total, used, free, and available memory
   - Calculate percentage of memory used

6. **Append to log file**: Write results to `~/logs/system-status.log` with:
   - Clear timestamp header (ISO 8601 format)
   - Each section labeled clearly (Disk, Docker, Users, Load, Memory)
   - Separator line between hourly reports

7. **Rotate log file**: If log file exceeds 10MB, create new file with timestamp

## Example Output

```
=== System Status Report: 2025-01-15T14:00:00 ===

[Disk Usage]
Filesystem      Size  Used Avail Use% Mounted on
/dev/sda1       100G   45G   55G  45% /
/dev/sdb1       500G  475G   25G  95% /data  ⚠️ HIGH USAGE

[Docker Containers]
NAMES           STATUS          PORTS
web-server      Up 2 days       0.0.0.0:80->80/tcp
db-postgres     Up 2 days       0.0.0.0:5432->5432/tcp
redis-cache     Up 2 days       0.0.0.0:6379->6379/tcp

[Active Users]
3 active user sessions

[System Load]
Load average: 0.52, 0.48, 0.51 (last 1min, 5min, 15min)

[Memory Usage]
Total: 16GB, Used: 8.5GB (53%), Free: 7.5GB, Available: 9.2GB

Report completed successfully.
=============================================

```

## Error Handling

- If any command fails, note the failure in the report but continue with other checks
- If log directory `~/logs/` doesn't exist, create it
- If log file cannot be written, print error to stderr and exit

## Schedule Recommendation

Run this task every hour at :00:
```bash
clud --cron add "0 * * * *" examples/cron/hourly-report.md
```

Or run every 30 minutes:
```bash
clud --cron add "*/30 * * * *" examples/cron/hourly-report.md
```
