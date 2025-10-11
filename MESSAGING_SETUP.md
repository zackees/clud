# Messaging Setup Guide

Get real-time status updates from your clud agent via Telegram, SMS, or WhatsApp!

## Quick Start

```bash
# Configure messaging credentials (one-time setup)
clud --configure-messaging

# Start using notifications
clud --notify-user "@yourusername" -m "Fix the login bug"
clud --notify-user "+14155551234" -m "Deploy to production"
clud --notify-user "whatsapp:+14155551234" -m "Run all tests"
```

---

## Telegram Setup

**Recommended for developers** - Free, rich formatting, supports code blocks

### Step 1: Create a Bot

1. Open Telegram and search for `@BotFather`
2. Send `/newbot` command
3. Follow the prompts to choose a name and username
4. Copy the **bot token** (looks like: `1234567890:ABCdefGHI...`)

### Step 2: Get Your Chat ID

**Option A: Use a helper bot**
1. Search for `@userinfobot` on Telegram
2. Send any message to it
3. It will reply with your chat ID (a number like `123456789`)

**Option B: Start conversation with your bot**
1. Search for your bot by the username you created
2. Send `/start` to your bot
3. Your bot will now be able to message you

### Step 3: Configure Clud

```bash
# Interactive configuration
clud --configure-messaging

# Or set environment variable
export TELEGRAM_BOT_TOKEN="1234567890:ABCdefGHI..."

# Or save to config file
echo "1234567890:ABCdefGHI..." > ~/.clud/telegram-bot-token.key
chmod 600 ~/.clud/telegram-bot-token.key
```

### Step 4: Use It!

```bash
# With @username (requires user to /start bot first)
clud --notify-user "@username" -m "Build Docker image"

# With chat ID (more reliable)
clud --notify-user "123456789" -m "Run integration tests"

# With telegram: prefix
clud --notify-user "telegram:123456789" -m "Deploy app"
```

### Example Notification

```
🤖 **Clud Agent Starting**

Task: Build Docker image

I'll keep you updated on progress!

---

⏳ **Working** (45s)

Building image layer 5/8...

---

✅ **Completed Successfully** (120s)

Docker image built successfully!
Size: 1.2GB
Layers: 8
```

---

## SMS Setup

**Best for non-technical users** - Works on any phone

### Step 1: Create Twilio Account

1. Sign up at https://www.twilio.com/try-twilio
2. Get free trial credits ($15)
3. Verify your phone number

### Step 2: Get a Phone Number

1. Go to Phone Numbers → Buy a Number
2. Choose a number with SMS capability (~$1/month)
3. Note your number (format: `+15555555555`)

### Step 3: Get Credentials

1. Go to https://www.twilio.com/console
2. Copy your **Account SID** (starts with `AC...`)
3. Copy your **Auth Token**

### Step 4: Configure Clud

```bash
# Interactive configuration
clud --configure-messaging

# Or set environment variables
export TWILIO_ACCOUNT_SID="ACxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
export TWILIO_AUTH_TOKEN="your_auth_token"
export TWILIO_FROM_NUMBER="+15555555555"
```

### Step 5: Use It!

```bash
# Send SMS notifications
clud --notify-user "+14155551234" -m "Fix authentication bug"
clud --notify-user "+14155551234" --notify-interval 60 -m "Long running task"
```

### Example SMS

```
🤖 Clud Agent Starting

Task: Fix authentication bug

I'll keep you updated on progress!

---

⏳ Working (30s)

Analyzing auth flow...

---

✅ Completed (90s)

Bug fixed successfully!
```

**Note:** SMS messages are plain text (no Markdown formatting)

---

## WhatsApp Setup

**Best for international users** - Lower cost than SMS

### Step 1: Use Twilio WhatsApp Sandbox

For testing, use the WhatsApp Sandbox (free):

1. Go to https://www.twilio.com/console/sms/whatsapp/sandbox
2. Follow instructions to join the sandbox:
   - Send "join `<your-code>`" to Twilio WhatsApp number
   - Example: Send "join `winter-cloud-1234`" to +1 415 123 4567

### Step 2: For Production (Optional)

For production use, you need:
1. Facebook Business account
2. WhatsApp Business API access approval (takes ~1 week)
3. Verified business profile

**For most users, the sandbox is sufficient!**

### Step 3: Configure Clud

Same as SMS setup - use your Twilio credentials:

```bash
export TWILIO_ACCOUNT_SID="ACxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
export TWILIO_AUTH_TOKEN="your_auth_token"
export TWILIO_FROM_NUMBER="+15555555555"
```

### Step 4: Use It!

```bash
# Send WhatsApp notifications
clud --notify-user "whatsapp:+14155551234" -m "Deploy to staging"
```

### Example WhatsApp

```
🤖 Clud Agent Starting

Task: Deploy to staging

I'll keep you updated on progress!

---

⏳ Working (60s)

Pushing Docker image...

---

✅ Completed (180s)

Deployed successfully!
URL: https://staging.example.com
```

---

## Configuration Management

### Configuration File

Clud stores credentials in `~/.clud/messaging.json`:

```json
{
  "telegram": {
    "bot_token": "1234567890:ABCdefGHI...",
    "enabled": true
  },
  "twilio": {
    "account_sid": "ACxxxxxxxxxxxxxxxx",
    "auth_token": "your_auth_token",
    "from_number": "+15555555555",
    "enabled": true
  },
  "preferences": {
    "default_channel": "telegram",
    "update_interval": 30
  }
}
```

### Environment Variables

Environment variables take precedence over config file:

```bash
# Telegram
export TELEGRAM_BOT_TOKEN="..."

# Twilio (SMS + WhatsApp)
export TWILIO_ACCOUNT_SID="..."
export TWILIO_AUTH_TOKEN="..."
export TWILIO_FROM_NUMBER="..."
```

### Per-Command Credentials

You can also specify credentials per command (not recommended for security):

```bash
# Use config file or environment variables instead!
```

---

## Usage Examples

### Basic Notification

```bash
clud --notify-user "+14155551234" -m "Fix bug in payment flow"
```

### Custom Update Interval

```bash
# Send updates every 60 seconds (default is 30)
clud --notify-user "@devuser" --notify-interval 60 -m "Long build process"
```

### With Prompt Mode

```bash
clud --notify-user "telegram:123456789" -p "Refactor the authentication module"
```

### Background Mode with Notifications

```bash
clud bg --notify-user "@devuser" --cmd "pytest tests/"
```

### Multiple Channels (Run Multiple Commands)

```bash
# Notify on Telegram
clud --notify-user "@devuser" -m "Starting deployment" &

# Notify on SMS
clud --notify-user "+14155551234" -m "Starting deployment" &

wait
```

---

## Contact Format Reference

### Telegram

| Format | Example | Description |
|--------|---------|-------------|
| `@username` | `@devuser` | Telegram username (requires /start) |
| `chat_id` | `123456789` | Numeric chat ID (more reliable) |
| `telegram:@username` | `telegram:@devuser` | Explicit Telegram prefix |
| `telegram:chat_id` | `telegram:123456789` | Explicit with chat ID |

### SMS

| Format | Example | Description |
|--------|---------|-------------|
| `+1XXXXXXXXXX` | `+14155551234` | E.164 international format |

### WhatsApp

| Format | Example | Description |
|--------|---------|-------------|
| `whatsapp:+1XXXXXXXXXX` | `whatsapp:+14155551234` | WhatsApp with phone number |

---

## Notification Types

### Start Notification

Sent immediately when agent starts:

```
🤖 **Clud Agent Starting**

Task: {your_task_description}

I'll keep you updated on progress!
```

### Progress Updates

Sent every N seconds (configurable, default 30):

```
⏳ **Working** ({elapsed_time}s)

{last_output_from_claude}
```

### Completion Notification

Sent when agent finishes:

```
✅ **Completed Successfully** ({total_time}s)

{summary_if_available}
```

or

```
❌ **Failed** ({total_time}s)

{error_message}
```

### Error Notification

Sent if critical error occurs:

```
⚠️ **Error**

{error_details}
```

---

## Troubleshooting

### Telegram Issues

**Problem:** "Cannot resolve Telegram username to chat_id"

**Solution:** 
- Use numeric chat ID instead of @username
- Make sure user has sent `/start` to your bot first
- Get chat ID from @userinfobot

**Problem:** "python-telegram-bot not installed"

**Solution:**
```bash
pip install python-telegram-bot
# or
pip install clud[messaging]
```

### Twilio Issues

**Problem:** "Twilio configuration missing"

**Solution:**
```bash
# Make sure all three are set
export TWILIO_ACCOUNT_SID="ACxxxxx..."
export TWILIO_AUTH_TOKEN="your_token"
export TWILIO_FROM_NUMBER="+15555555555"
```

**Problem:** "twilio package not installed"

**Solution:**
```bash
pip install twilio
# or
pip install clud[messaging]
```

**Problem:** SMS not received

**Solution:**
- Check Twilio console for delivery status
- Verify destination number is correct
- Check your Twilio account balance
- For trial accounts, verify destination number is registered

### WhatsApp Issues

**Problem:** WhatsApp message not delivered

**Solution:**
- Verify you've joined the Twilio WhatsApp Sandbox
- Send "join {your-code}" to Twilio WhatsApp number
- Check sandbox is active in Twilio console
- For production, verify WhatsApp Business API is approved

### General Issues

**Problem:** Notifications not sending

**Solution:**
- Run `clud --configure-messaging` to verify credentials
- Check `~/.clud/messaging.json` exists and is valid JSON
- Try with verbose mode: `clud -v --notify-user ...`
- Check environment variables are set correctly

**Problem:** Rate limiting errors

**Solution:**
- Increase `--notify-interval` (default 30s)
- Telegram: Max 30 messages/second per bot
- Twilio: Default 1 message/second

---

## Security Best Practices

### Credential Storage

✅ **DO:**
- Use environment variables in CI/CD
- Store credentials in `~/.clud/messaging.json` with 600 permissions
- Use OS keyring when available
- Rotate credentials periodically

❌ **DON'T:**
- Commit credentials to git
- Share credentials in plain text
- Use credentials in command line arguments (visible in process list)
- Store credentials in unencrypted files

### API Key Protection

```bash
# Good: Environment variable
export TELEGRAM_BOT_TOKEN="..."
clud --notify-user "@user" -m "task"

# Good: Config file with restricted permissions
chmod 600 ~/.clud/messaging.json

# Bad: Command line (visible in ps aux)
clud --api-key "sk-ant-..." --notify-user "@user"  # Don't do this!
```

---

## Cost Estimation

### Telegram
- **Cost:** FREE
- **Limits:** 30 messages/second per bot
- **Recommendation:** Use for all developer notifications

### Twilio SMS
- **Cost:** ~$0.0075 per message (US)
- **Example:** 100 agent runs = $2.25/month
- **Recommendation:** Use for non-technical stakeholders

### Twilio WhatsApp
- **Cost:** ~$0.005 per message
- **Example:** 100 agent runs = $1.50/month
- **Recommendation:** Use for international users

### Typical Agent Run
- Start notification: 1 message
- Progress updates: 2-4 messages (every 30s for 1-2 minutes)
- Completion: 1 message
- **Total:** 4-6 messages per run

### Cost Comparison
| Channel | 100 runs/month | 500 runs/month | 1000 runs/month |
|---------|----------------|----------------|-----------------|
| Telegram | $0 | $0 | $0 |
| SMS | $3.00 | $15.00 | $30.00 |
| WhatsApp | $2.00 | $10.00 | $20.00 |

---

## Advanced Features

### Custom Notification Interval

```bash
# Update every minute instead of every 30 seconds
clud --notify-user "@user" --notify-interval 60 -m "Long task"
```

### Quiet Mode (Completion Only)

```bash
# Only send start and completion (set very high interval)
clud --notify-user "@user" --notify-interval 999999 -m "Quick task"
```

### Multiple Recipients (Future)

Currently not supported in single command. Workaround:

```bash
# Notify multiple users with different messages
clud --notify-user "@dev1" -m "Deploying..." &
clud --notify-user "+1234567890" -m "Deploying..." &
wait
```

---

## FAQ

**Q: Which channel should I use?**

A: Telegram for developers (free, rich formatting), SMS for non-technical users, WhatsApp for international users.

**Q: How much does it cost?**

A: Telegram is free. SMS/WhatsApp cost $0.005-0.0075 per message. Typical agent run sends 4-6 messages.

**Q: Can I disable notifications mid-run?**

A: Not currently supported. Terminate and restart without `--notify-user`.

**Q: Can multiple agents notify the same user?**

A: Yes! Each agent is independent.

**Q: What if my credentials are invalid?**

A: Agent will continue without notifications (graceful degradation) and print a warning.

**Q: Can I use multiple Telegram bots?**

A: Yes, specify different tokens per command or use different config files.

**Q: How do I get my Telegram chat ID?**

A: Message @userinfobot or check your bot's getUpdates endpoint.

**Q: Does this work in Docker containers?**

A: Yes! Use `clud bg --notify-user ...` or pass environment variables.

**Q: Can I customize notification messages?**

A: Not yet. This is a future enhancement.

---

## Support

- **Documentation:** https://github.com/zackees/clud
- **Issues:** https://github.com/zackees/clud/issues
- **Telegram API Docs:** https://core.telegram.org/bots/api
- **Twilio Docs:** https://www.twilio.com/docs

---

## Next Steps

1. Set up your preferred channel (Telegram recommended)
2. Run `clud --configure-messaging` to save credentials
3. Test with a simple task: `clud --notify-user "@you" -m "echo hello"`
4. Integrate into your workflow!

Happy coding! 🚀
