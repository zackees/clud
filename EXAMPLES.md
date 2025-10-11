# Clud Messaging Integration Examples

Real-world examples of using clud with Telegram, SMS, and WhatsApp notifications.

## Table of Contents
- [Basic Examples](#basic-examples)
- [Development Workflows](#development-workflows)
- [Production Deployments](#production-deployments)
- [Team Collaboration](#team-collaboration)
- [CI/CD Integration](#cicd-integration)

---

## Basic Examples

### Example 1: Simple Task with Telegram

```bash
# Configure once
clud --configure-messaging

# Run a simple task with notifications
clud --notify-user "123456789" -m "Add dark mode toggle to settings"
```

**What happens:**
1. Bot sends: "🤖 **Clud Agent Starting** - Task: Add dark mode toggle to settings"
2. Every 30s: "⏳ **Working** (30s) - Creating toggle component..."
3. Finally: "✅ **Completed Successfully** (120s)"

---

### Example 2: Long-Running Build with SMS

```bash
# Build Docker image and get SMS updates
clud --notify-user "+14155551234" -m "Build Docker image for production"
```

**Timeline:**
- 0s: "🤖 Clud Agent Starting - Task: Build Docker image..."
- 30s: "⏳ Working (30s) - Building layer 3/8..."
- 60s: "⏳ Working (60s) - Building layer 6/8..."
- 120s: "✅ Completed (120s) - Image built: my-app:latest"

---

### Example 3: WhatsApp for International Team

```bash
# Notify international colleague via WhatsApp (cheaper than SMS)
clud --notify-user "whatsapp:+447123456789" -m "Deploy to staging environment"
```

---

## Development Workflows

### Example 4: Bug Fix with Progress Updates

```bash
# Fix a bug with detailed progress tracking
clud --notify-user "@devlead" -m "Fix memory leak in user authentication"
```

**Sample Progress Messages:**
```
🤖 Clud Agent Starting
Task: Fix memory leak in user authentication
I'll keep you updated on progress!

---

⏳ Working (30s)
Analyzing authentication flow...

---

⏳ Working (60s)
Identified issue in session management

---

⏳ Working (90s)
Implementing fix in auth.py

---

✅ Completed Successfully (135s)
Memory leak fixed! Updated 3 files.
```

---

### Example 5: Refactoring with Custom Interval

```bash
# Long refactoring task, update every 2 minutes
clud --notify-user "@senior-dev" \
  --notify-interval 120 \
  -m "Refactor database query layer for better performance"
```

---

### Example 6: Testing Suite Execution

```bash
# Run entire test suite with notifications
clud --notify-user "telegram:987654321" \
  --cmd "pytest tests/ -v"
```

**Progress:**
```
🤖 Clud Agent Starting
Task: pytest tests/ -v

⏳ Working (45s)
test_authentication.py::test_login PASSED
test_authentication.py::test_logout PASSED
test_database.py::test_connection PASSED

✅ Completed Successfully (180s)
92 tests passed, 0 failed
```

---

## Production Deployments

### Example 7: Production Deploy with Multiple Stakeholders

```bash
# Notify tech lead on Telegram
clud --notify-user "@techlead" -m "Deploy v2.5.0 to production" &

# Notify PM on SMS
clud --notify-user "+14155551234" -m "Deploy v2.5.0 to production" &

# Notify ops team on WhatsApp
clud --notify-user "whatsapp:+442012345678" -m "Deploy v2.5.0 to production" &

wait
```

---

### Example 8: Emergency Hotfix

```bash
# Critical fix with immediate notification
clud --notify-user "@oncall-engineer" \
  --notify-interval 15 \
  -m "URGENT: Apply security patch CVE-2024-1234"
```

**Aggressive Updates:**
- Updates every 15 seconds
- Engineer gets real-time progress
- Quick confirmation when complete

---

### Example 9: Database Migration

```bash
# Long-running migration with progress tracking
clud --notify-user "@dba" \
  --notify-interval 60 \
  -m "Migrate 10M records to new schema"
```

---

## Team Collaboration

### Example 10: Code Review Request

```bash
# Implement feature and notify reviewer
clud --notify-user "@code-reviewer" \
  -m "Implement OAuth2 authentication and create PR"
```

**Workflow:**
1. Agent implements OAuth2
2. Creates tests
3. Opens PR
4. Sends notification: "✅ Completed - PR #123 ready for review"

---

### Example 11: Pair Programming Async

```bash
# Notify pair when your task is done
clud --notify-user "@pair-programmer" \
  -m "Complete my portion of API integration"
```

**Use case:** Working in different timezones, notify when your part is ready.

---

### Example 12: Daily Automation

```bash
#!/bin/bash
# daily-tasks.sh - Run daily maintenance

# Update dependencies
clud --notify-user "@devops" -m "Update all npm dependencies" &

# Run security scan
clud --notify-user "@security" -m "Run npm audit and apply fixes" &

# Generate reports
clud --notify-user "@manager" -m "Generate weekly metrics report" &

wait
```

---

## CI/CD Integration

### Example 13: GitHub Actions Integration

```yaml
# .github/workflows/deploy.yml
name: Deploy to Production

on:
  push:
    branches: [main]

jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      
      - name: Install clud
        run: pip install clud[messaging]
      
      - name: Deploy with notifications
        env:
          ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
          TELEGRAM_BOT_TOKEN: ${{ secrets.TELEGRAM_BOT_TOKEN }}
        run: |
          clud --notify-user "${{ secrets.TELEGRAM_CHAT_ID }}" \
            -m "Deploy ${{ github.sha }} to production"
```

---

### Example 14: GitLab CI with Multiple Channels

```yaml
# .gitlab-ci.yml
deploy_production:
  stage: deploy
  script:
    - pip install clud[messaging]
    - |
      # Notify on Telegram
      clud --notify-user "$TELEGRAM_CHAT_ID" \
        -m "Deploy ${CI_COMMIT_SHORT_SHA} to production" &
      
      # Notify manager on SMS
      clud --notify-user "$MANAGER_PHONE" \
        -m "Production deployment in progress" &
      
      wait
  only:
    - main
```

---

### Example 15: Cron Job Monitoring

```bash
#!/bin/bash
# /etc/cron.daily/backup

# Daily backup with notification on completion
clud --notify-user "+14155551234" \
  --cmd "/usr/local/bin/backup-databases.sh"
```

**Notification:**
```
✅ Completed Successfully (3600s)

Backup completed:
- Database: myapp_production
- Size: 2.3 GB
- Location: s3://backups/2024-10-11/
```

---

## Advanced Patterns

### Example 16: Conditional Notifications

```bash
#!/bin/bash
# Only notify if task takes longer than expected

# Start time
START=$(date +%s)

# Run task
clud --notify-user "@lead" -m "Optimize database queries"

# Calculate duration
END=$(date +%s)
DURATION=$((END - START))

# Send follow-up if took too long
if [ $DURATION -gt 600 ]; then
  # Use another clud command to send custom message
  echo "Task took ${DURATION}s - longer than expected!" | \
    clud --notify-user "@lead" -m "Performance note: query optimization"
fi
```

---

### Example 17: Multi-Step Workflow

```bash
#!/bin/bash
# multi-step-deploy.sh

set -e

# Step 1: Build
clud --notify-user "@devops" -m "Step 1/3: Build Docker image"

# Step 2: Test
clud --notify-user "@devops" -m "Step 2/3: Run integration tests"

# Step 3: Deploy
clud --notify-user "@devops" -m "Step 3/3: Deploy to production"

# Success notification
echo "✅ All steps completed successfully!" | \
  clud --notify-user "@devops" -m "Deployment complete"
```

---

### Example 18: Error Recovery with Notifications

```bash
#!/bin/bash
# deploy-with-retry.sh

MAX_RETRIES=3
ATTEMPT=1

while [ $ATTEMPT -le $MAX_RETRIES ]; do
  echo "Attempt $ATTEMPT of $MAX_RETRIES"
  
  if clud --notify-user "@oncall" -m "Deploy to production (attempt $ATTEMPT)"; then
    echo "✅ Deployment successful!"
    break
  else
    echo "❌ Deployment failed, retrying..."
    ATTEMPT=$((ATTEMPT + 1))
    sleep 60
  fi
done

if [ $ATTEMPT -gt $MAX_RETRIES ]; then
  # Final failure notification
  echo "❌ Deployment failed after $MAX_RETRIES attempts" | \
    clud --notify-user "@oncall" -m "URGENT: Manual intervention needed"
fi
```

---

## Testing Patterns

### Example 19: Test Suite with Conditional Notification

```bash
#!/bin/bash
# run-tests-notify.sh

# Run tests and capture exit code
if clud --notify-user "@qa-lead" --cmd "pytest tests/ -v"; then
  echo "✅ All tests passed!"
else
  # Send urgent notification on failure
  echo "❌ TESTS FAILED - Please review immediately" | \
    clud --notify-user "@qa-lead" -m "Test suite failure alert"
  exit 1
fi
```

---

### Example 20: Performance Benchmarking

```bash
# Run performance benchmarks with progress tracking
clud --notify-user "@performance-team" \
  --notify-interval 45 \
  --cmd "python benchmarks/run_all.py --iterations=1000"
```

**Progress:**
```
⏳ Working (45s)
Benchmark 1/10: API latency - 45ms avg

⏳ Working (90s)
Benchmark 5/10: Database queries - 12ms avg

✅ Completed (450s)
All benchmarks complete:
- API: 45ms (within SLA)
- DB: 12ms (improved 20%)
- Memory: 512MB peak
```

---

## Configuration Examples

### Example 21: Per-Project Configuration

```bash
# project-a/.env
export TELEGRAM_BOT_TOKEN="bot_token_for_project_a"
export TELEGRAM_CHAT_ID="123456789"

# Usage in project
source .env
clud --notify-user "$TELEGRAM_CHAT_ID" -m "Build project A"
```

---

### Example 22: Multiple Bots for Different Teams

```bash
# Backend team notifications
export BACKEND_BOT_TOKEN="1234567890:ABC..."
TELEGRAM_BOT_TOKEN=$BACKEND_BOT_TOKEN clud --notify-user "@backend-dev" -m "task"

# Frontend team notifications
export FRONTEND_BOT_TOKEN="9876543210:XYZ..."
TELEGRAM_BOT_TOKEN=$FRONTEND_BOT_TOKEN clud --notify-user "@frontend-dev" -m "task"
```

---

### Example 23: Scheduled Notification Summary

```bash
#!/bin/bash
# weekly-summary.sh - Run via cron every Monday

# Generate and send weekly metrics
clud --notify-user "@team-lead" \
  --cmd "python scripts/generate_weekly_report.py"
```

**Crontab:**
```cron
0 9 * * 1 /home/user/scripts/weekly-summary.sh
```

---

## Tips & Tricks

### Quiet Mode (Completion Only)

```bash
# Set very high interval to only get start/end notifications
clud --notify-user "@dev" --notify-interval 999999 -m "quick task"
```

### Verbose Progress (Every 10 seconds)

```bash
# Get updates every 10 seconds for critical tasks
clud --notify-user "@oncall" --notify-interval 10 -m "critical fix"
```

### Multiple Contacts Pattern

```bash
# Create wrapper script for team notifications
#!/bin/bash
# notify-team.sh

TEAM_CONTACTS=(
  "@alice"
  "@bob"
  "telegram:987654321"
)

for contact in "${TEAM_CONTACTS[@]}"; do
  clud --notify-user "$contact" "$@" &
done

wait
```

**Usage:**
```bash
./notify-team.sh -m "Emergency maintenance in progress"
```

---

## Cost Optimization

### Use Telegram for Development

```bash
# Free notifications during development
clud --notify-user "@dev" -m "Dev task"
```

### Reserve SMS for Production Alerts

```bash
# Paid SMS only for critical production issues
if [ "$ENVIRONMENT" = "production" ]; then
  clud --notify-user "+14155551234" -m "Production deploy"
else
  clud --notify-user "@dev" -m "Staging deploy"
fi
```

---

## Troubleshooting Examples

### Test Telegram Bot

```bash
# Quick test to verify Telegram setup
echo "Test message" | clud --notify-user "123456789" -m "echo Hello World"
```

### Test SMS Delivery

```bash
# Verify Twilio configuration
clud --notify-user "+14155551234" --cmd "echo Testing SMS delivery"
```

### Debug Mode

```bash
# Run with verbose output to see notification details
clud -v --notify-user "@dev" -m "debug this task"
```

---

## Best Practices

1. **Use Telegram for dev environments** (free)
2. **Reserve SMS for production alerts** (cost-effective)
3. **Set appropriate notify-interval** (balance updates vs noise)
4. **Store credentials in environment variables** (security)
5. **Test notifications in staging first** (avoid surprises)
6. **Use descriptive task messages** (clear notifications)
7. **Monitor notification costs** (especially SMS/WhatsApp)

---

## More Examples

For more examples and patterns, see:
- [MESSAGING_SETUP.md](MESSAGING_SETUP.md) - Setup guide
- [README.md](README.md) - Main documentation
- [TELEGRAM_AGENT_INTEGRATION_PROPOSAL.md](TELEGRAM_AGENT_INTEGRATION_PROPOSAL.md) - Technical details

---

**Happy automating! 🚀**
