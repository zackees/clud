# Daily Backup Task

Create a backup of all Python files in the project directory.

## Instructions

1. **Identify Python files**: Find all `.py` files in `~/projects/` recursively
2. **Create backup archive**: Archive files to `~/backups/python-backup-YYYY-MM-DD.tar.gz`
   - Use current date in format: `YYYY-MM-DD` (e.g., `2025-01-15`)
   - Include timestamp in the backup filename
3. **Verify backup**: Check that the backup file exists and is at least 1KB in size
4. **Count files**: Count the number of Python files included in the backup
5. **Log results**: Append backup summary to `~/logs/backup.log` with:
   - Timestamp of backup
   - Backup file size (human-readable format like "2.5 MB")
   - Number of files backed up
   - Success/failure status
6. **Cleanup old backups**: Delete backup files older than 30 days from `~/backups/` directory
7. **Report**: Print a summary to stdout with backup file path, size, and file count

## Example Output

```
[2025-01-15 02:00:00] Starting daily backup...
Found 247 Python files in ~/projects/
Created backup: ~/backups/python-backup-2025-01-15.tar.gz (3.2 MB)
Cleaned up 2 old backup files (older than 30 days)
Backup completed successfully!
```

## Error Handling

- If `~/projects/` doesn't exist, log error and exit with non-zero status
- If `~/backups/` doesn't exist, create it before backing up
- If backup creation fails, log error with details
- If backup file is smaller than expected, log warning but continue

## Schedule Recommendation

Run this task daily at 2:00 AM:
```bash
clud --cron add "0 2 * * *" examples/cron/daily-backup.md
```
