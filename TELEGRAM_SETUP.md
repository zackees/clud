# Telegram Notifications for Claude Agents

Get notified when your Claude agents launch and complete via Telegram! 🚀

## Quick Setup (5 Minutes)

### 1. Create a Telegram Bot (2 minutes)

1. Open Telegram and search for `@BotFather`
2. Send `/newbot` command
3. Follow prompts to name your bot
4. **Save the bot token** (looks like `123456:ABC-DEF1234ghIkl...`)

### 2. Get Your Chat ID (1 minute)

1. Search for `@userinfobot` on Telegram
2. Send `/start` command
3. **Note your chat ID** (a number like `123456789`)

### 3. Start Your Bot

1. Search for your bot by username
2. Send `/start` to activate it

### 4. Launch Agent with Notifications

```bash
clud bg --telegram \
  --telegram-bot-token "123456:ABC-DEF1234..." \
  --telegram-chat-id "123456789"
```

## Installation

Install the Telegram dependency:

```bash
pip install python-telegram-bot
```

## Usage Options

### Command Line Arguments

```bash
# Enable Telegram with credentials
clud bg --telegram \
  --telegram-bot-token "123456:ABC-DEF..." \
  --telegram-chat-id "123456789"

# Short form
clud bg --telegram --telegram-bot-token TOKEN --telegram-chat-id ID
```

### Environment Variables (Recommended)

```bash
# Set environment variables
export TELEGRAM_BOT_TOKEN="123456:ABC-DEF..."
export TELEGRAM_CHAT_ID="123456789"

# Launch with just the flag
clud bg --telegram
```

### Configuration File

Create a `.clud` file in your project:

```json
{
  "telegram": {
    "enabled": true,
    "bot_token": "${TELEGRAM_BOT_TOKEN}",
    "chat_id": "${TELEGRAM_CHAT_ID}"
  }
}
```

Then launch:

```bash
export TELEGRAM_BOT_TOKEN="your_token"
export TELEGRAM_CHAT_ID="your_chat_id"
clud bg  # Auto-loads from .clud file
```

## What You'll Receive

### Launch Notification

When your agent starts:

```
🚀 Claude Agent Launched

Agent: clud-dev
Container: abc123456789
Project: /workspace/my-project
Mode: background

Status: ✅ Online and ready

Send messages to interact with your agent!
```

### Cleanup Notification

When your agent completes:

```
✅ Agent Cleanup Complete

Agent: clud-dev
Duration: 0:15:23
Tasks Completed: 0
Files Modified: 0
Errors: 0

Status: 🔴 Offline
```

## Security Best Practices

### Environment Variables

Create a `.env` file (add to `.gitignore`):

```bash
TELEGRAM_BOT_TOKEN=123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11
TELEGRAM_CHAT_ID=123456789
```

Load with:

```bash
source .env  # or use direnv, dotenv, etc.
clud bg --telegram
```

### Never Commit Credentials

Add to `.gitignore`:

```
.env
.clud
*.key
```

## Examples

### Personal Development

```bash
# Get notified when long-running tasks complete
clud bg --telegram --cmd "pytest tests/"
```

### With Browser UI

```bash
# Notifications + VS Code server
clud bg --telegram --open
```

### Custom Project Path

```bash
# Notifications for specific project
clud bg /path/to/project --telegram
```

## Troubleshooting

### Issue: Bot not sending messages

**Solution:** Make sure you've sent `/start` to your bot first

### Issue: Invalid chat ID

**Solution:** Use `@userinfobot` to get the correct chat ID (it's a number, not your username)

### Issue: Token not found

**Solution:** Verify environment variables are set:
```bash
echo $TELEGRAM_BOT_TOKEN
echo $TELEGRAM_CHAT_ID
```

### Issue: python-telegram-bot not installed

**Solution:** Install the dependency:
```bash
pip install python-telegram-bot
```

## Features

- ✅ **Free** - No cost for bot or messages
- ✅ **Easy Setup** - 5 minutes to get started
- ✅ **Rich Formatting** - Markdown support
- ✅ **Bidirectional** - Can receive messages from users
- ✅ **Cross-platform** - Works on mobile, desktop, web
- ✅ **Real-time** - Instant notifications

## Why Telegram?

- **FREE** - No API costs
- **Easy** - Simple 5-minute setup
- **Reliable** - Well-documented API
- **Rich** - Supports formatting, files, buttons
- **Popular** - 800M+ users worldwide

## Example Workflow

```bash
# Morning routine
export TELEGRAM_BOT_TOKEN="your_token"
export TELEGRAM_CHAT_ID="your_chat_id"

# Start working on project
cd ~/projects/my-app
clud bg --telegram

# Agent sends: "🚀 Agent launched!"
# Work in container...
# Exit container

# Agent sends: "✅ Agent complete! Duration: 1:23:45"
```

## Advanced Usage

### Multiple Agents

Use different bots for different projects:

```bash
# Project A
export TELEGRAM_BOT_TOKEN="bot_token_a"
export TELEGRAM_CHAT_ID="chat_id_a"
clud bg ~/project-a --telegram

# Project B  
export TELEGRAM_BOT_TOKEN="bot_token_b"
export TELEGRAM_CHAT_ID="chat_id_b"
clud bg ~/project-b --telegram
```

### Team Collaboration

Share bot with team for group notifications:

1. Create group chat in Telegram
2. Add your bot to the group
3. Get group chat ID (negative number)
4. Use group chat ID instead of personal chat ID

## CLI Reference

```bash
# Telegram arguments
--telegram                  Enable Telegram notifications
--telegram-bot-token TOKEN  Telegram bot API token
--telegram-chat-id ID       Telegram chat ID to send to

# Environment variables
TELEGRAM_BOT_TOKEN         Bot token from @BotFather
TELEGRAM_CHAT_ID           Your chat ID from @userinfobot
```

## Support

- Check example config: `.clud.example`
- Read tests: `tests/test_messaging.py`
- File issue on GitHub

---

**Made with ❤️ for the CLUD community**
