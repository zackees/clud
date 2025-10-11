# Telegram Bot Setup Guide: Complete Walkthrough

**For clud Messaging Integration**

---

## What You'll Need

- Telegram app (mobile or desktop)
- 5 minutes of time
- A name for your bot

---

## Step 1: Find BotFather

**BotFather** is Telegram's official bot for creating and managing bots.

1. Open Telegram
2. In the search bar, type: `@BotFather`
3. Click on **BotFather** (verified with blue checkmark ✓)
4. Click **START** button

![BotFather Profile](https://core.telegram.org/file/811140184/1/zlN4goPTupk/9ff2f2f01c4bd1b013)

---

## Step 2: Create Your Bot

### Command: `/newbot`

Type or click: `/newbot`

BotFather will respond:
```
Alright, a new bot. How are we going to call it? 
Please choose a name for your bot.
```

---

## Step 3: Choose a Display Name

**Display Name** = What users see (can be anything)

Examples:
- `Clud Notifier`
- `My Dev Bot`
- `CI/CD Helper`
- `John's Agent`

Type your chosen name and send it.

---

## Step 4: Choose a Username

**Username** = Unique identifier (must end with `bot`)

BotFather will ask:
```
Good. Now let's choose a username for your bot. 
It must end in `bot`. Like this, for example: TetrisBot or tetris_bot.
```

### Username Rules:
- ✅ Must end with `bot` (case insensitive)
- ✅ Must be unique (not already taken)
- ✅ Can contain: letters, numbers, underscores
- ✅ Minimum 5 characters
- ❌ No spaces or special characters (except underscore)

### Good Examples:
- `cludnotifier_bot`
- `MyDevHelperBot`
- `johns_agent_bot`
- `CICD_Bot`

### Bad Examples:
- ❌ `mybot` (doesn't end with "bot")
- ❌ `my bot` (contains space)
- ❌ `my-bot` (contains hyphen)
- ❌ `bot` (too short)

Type your username and send it.

---

## Step 5: Get Your Bot Token 🎉

If successful, BotFather will respond with:

```
Done! Congratulations on your new bot. You will find it at 
t.me/your_bot_username. You can now add a description, about 
section and profile picture for your bot, see /help for a list 
of commands.

Use this token to access the HTTP API:
1234567890:ABCdefGHIjklMNOpqrsTUVwxyz-1234567

Keep your token secure and store it safely, it can be used by 
anyone to control your bot.
```

### 🔑 Your Bot Token

**Format:** `{number}:{letters_numbers_hyphens}`

**Example:** `1234567890:ABCdefGHIjklMNOpqrsTUVwxyz-1234567`

**⚠️ IMPORTANT:** 
- Copy this token immediately
- Keep it secret (like a password)
- Don't share it publicly
- Don't commit to git

---

## Step 6: Get Your Chat ID

To receive notifications, you need your **Chat ID** (your Telegram user ID).

### Method 1: Use @userinfobot (Easiest)

1. In Telegram search, type: `@userinfobot`
2. Click on **userinfobot**
3. Click **START** or send any message
4. Bot will reply with your info:

```
👤 User
Id: 123456789          ← This is your Chat ID!
First name: John
Username: @johnsmith
Language: en
```

**Copy the Id number** (e.g., `123456789`)

### Method 2: Use Your New Bot

1. Click the link BotFather sent (t.me/your_bot_username)
2. Click **START** in your bot
3. Run this command (replace `{TOKEN}` with your bot token):

```bash
curl "https://api.telegram.org/bot{TOKEN}/getUpdates"
```

**Example:**
```bash
curl "https://api.telegram.org/bot1234567890:ABCdef/getUpdates"
```

4. Look for `"chat":{"id":123456789` in the response
5. That number (`123456789`) is your Chat ID

**Example Response:**
```json
{
  "ok": true,
  "result": [{
    "update_id": 123456789,
    "message": {
      "message_id": 1,
      "from": {"id": 123456789, "is_bot": false, "first_name": "John"},
      "chat": {"id": 123456789, "type": "private"},  ← Chat ID here!
      "date": 1234567890,
      "text": "/start"
    }
  }]
}
```

---

## Step 7: Configure clud

Now you have both required pieces:
1. ✅ Bot Token: `1234567890:ABCdefGHIjklMNOpqrsTUVwxyz`
2. ✅ Chat ID: `123456789`

### Configure clud:

```bash
clud --configure-messaging
```

**Interactive prompt:**
```
=== Messaging Configuration ===
Configure Telegram, SMS, and/or WhatsApp notifications

Telegram Bot Configuration (optional)
Get token from @BotFather: https://t.me/botfather
Telegram Bot Token (or press Enter to skip): 1234567890:ABCdefGHIjklMNOpqrsTUVwxyz
✓ Telegram configured

✓ Credentials saved securely to encrypted credential store
  Location: ~/.clud/credentials.enc (encrypted)

You can now use --notify-user to receive agent status updates!
```

---

## Step 8: Test Your Setup

Send a test notification:

```bash
# Using your Chat ID (recommended)
clud --notify-user "123456789" --cmd "echo Hello from clud!"
```

**Expected Output:**

In your terminal:
```
Hello from clud!
```

In your Telegram:
```
🤖 Clud Agent Starting

Task: echo Hello from clud!

I'll keep you updated on progress!

---

✅ Completed Successfully (1s)
```

---

## Step 9: Use in Real Workflows

Now you can use notifications in your real development work:

```bash
# Get notified when deployment completes
clud --notify-user "123456789" -m "Deploy to production"

# Get updates every 60 seconds
clud --notify-user "123456789" --notify-interval 60 -m "Long build task"

# Use with background mode
clud bg --notify-user "123456789" --cmd "pytest tests/"

# Multiple tasks
clud --notify-user "123456789" -m "Fix bug in authentication" &
clud --notify-user "123456789" -m "Update documentation" &
```

---

## Bot Customization (Optional)

After creating your bot, you can customize it in BotFather:

### Change Display Name
```
/setname
@your_bot_username
New Display Name
```

### Add Description
```
/setdescription
@your_bot_username
This bot sends me notifications from my clud development agent.
```

### Add About Text
```
/setabouttext
@your_bot_username
Automated notifications from clud - Claude in YOLO mode 🚀
```

### Set Profile Picture
```
/setuserpic
@your_bot_username
[Upload an image file]
```

### Set Commands (appears in bot menu)
```
/setcommands
@your_bot_username
start - Start receiving notifications
help - Show help information
status - Check bot status
```

---

## Troubleshooting

### ❌ "Username already taken"

**Problem:** Someone else is using that username

**Solution:** Try different variations:
- Add numbers: `mybot_bot` → `mybot123_bot`
- Add underscores: `mybotbot` → `my_bot_bot`
- Be more specific: `bot` → `johns_dev_bot`

---

### ❌ "Username is invalid"

**Problem:** Username doesn't meet requirements

**Solution:** Check that your username:
- Ends with `bot` (case insensitive)
- Has at least 5 characters
- Contains only: letters, numbers, underscores
- No spaces or special characters

---

### ❌ "Cannot resolve Telegram username"

**Problem:** Using @username instead of numeric chat ID

**Solution:** Use numeric chat ID instead:
```bash
# ❌ This might not work:
clud --notify-user "@johnsmith" -m "task"

# ✅ This will work:
clud --notify-user "123456789" -m "task"
```

---

### ❌ "Bot doesn't send messages"

**Problem:** Wrong chat ID or bot token

**Solution:** Verify both credentials:

1. **Test bot token:**
   ```bash
   curl "https://api.telegram.org/bot{YOUR_TOKEN}/getMe"
   ```
   Should return bot info (not error)

2. **Verify chat ID:**
   - Make sure you sent `/start` to your bot
   - Check @userinfobot for correct ID
   - Use numeric ID, not @username

---

### ❌ "Credentials not loading"

**Problem:** Credentials not saved correctly

**Solution:** Check credential storage:

```bash
# Re-run configuration
clud --configure-messaging

# Or set environment variable (temporary)
export TELEGRAM_BOT_TOKEN="1234567890:ABC..."
clud --notify-user "123456789" -m "test"
```

---

## Security Best Practices

### ✅ DO:
- Keep bot token secret (treat like password)
- Use clud's encrypted storage (automatic)
- Store in environment variables for CI/CD
- Revoke token if compromised
- Use numeric chat ID (more reliable)

### ❌ DON'T:
- Share bot token publicly
- Commit tokens to git repos
- Post tokens in chat/email
- Store in plain text files
- Use the same bot for multiple purposes

---

## Advanced: Revoking a Compromised Token

If your bot token is exposed:

1. Open BotFather
2. Send: `/mybots`
3. Select your bot
4. Click: **API Token**
5. Click: **Revoke current token**
6. Confirm
7. Copy new token
8. Re-run: `clud --configure-messaging`

---

## Managing Multiple Bots

You can create multiple bots for different purposes:

```bash
# Development notifications
clud --notify-user "123456789" -m "dev task"
export DEV_BOT_TOKEN="1234567890:ABC..."

# Production alerts
clud --notify-user "987654321" -m "prod task"
export PROD_BOT_TOKEN="9876543210:XYZ..."

# Team notifications (different chat ID)
clud --notify-user "555555555" -m "team task"
```

---

## Useful BotFather Commands

| Command | Description |
|---------|-------------|
| `/newbot` | Create a new bot |
| `/mybots` | Manage your existing bots |
| `/setname` | Change bot's display name |
| `/setdescription` | Add description (shown in profile) |
| `/setabouttext` | Add about text |
| `/setuserpic` | Upload profile picture |
| `/setcommands` | Set bot menu commands |
| `/deletebot` | Permanently delete a bot |
| `/token` | Get bot's token (if lost) |
| `/revoke` | Generate new token (revoke old) |
| `/help` | Show all commands |

---

## Quick Reference Card

```
┌─────────────────────────────────────────────┐
│  TELEGRAM BOT QUICK REFERENCE               │
├─────────────────────────────────────────────┤
│  Create Bot:                                │
│    1. Message @BotFather                    │
│    2. Send /newbot                          │
│    3. Choose name and username              │
│    4. Copy token                            │
│                                             │
│  Get Chat ID:                               │
│    1. Message @userinfobot                  │
│    2. Copy the "Id" number                  │
│                                             │
│  Configure clud:                            │
│    $ clud --configure-messaging             │
│    Paste token when prompted                │
│                                             │
│  Test:                                      │
│    $ clud --notify-user "{CHAT_ID}" \      │
│           --cmd "echo test"                 │
│                                             │
│  Use:                                       │
│    $ clud --notify-user "{CHAT_ID}" \      │
│           -m "your task"                    │
└─────────────────────────────────────────────┘
```

---

## Example: Complete Setup (5 minutes)

```
Time: 0:00 - Open Telegram, search @BotFather
Time: 0:30 - Send /newbot
Time: 1:00 - Choose name: "My Dev Bot"
Time: 1:30 - Choose username: "my_dev_helper_bot"
Time: 2:00 - Copy token: 1234567890:ABC...
Time: 2:30 - Search @userinfobot
Time: 3:00 - Copy chat ID: 123456789
Time: 3:30 - Run: clud --configure-messaging
Time: 4:00 - Paste token
Time: 4:30 - Test: clud --notify-user "123456789" --cmd "echo test"
Time: 5:00 - ✅ Receive notification in Telegram!
```

---

## What's Next?

After setup, explore:
- [MESSAGING_SETUP.md](./MESSAGING_SETUP.md) - Full setup guide (Telegram, SMS, WhatsApp)
- [EXAMPLES.md](./EXAMPLES.md) - 23 usage examples
- [README.md](./README.md) - Main documentation

---

## Support

**Having issues?**
- Check [Troubleshooting](#troubleshooting) section above
- Review [MESSAGING_SETUP.md](./MESSAGING_SETUP.md) FAQ
- File issue: https://github.com/zackees/clud/issues

**Official Resources:**
- Telegram Bots: https://core.telegram.org/bots
- BotFather Guide: https://core.telegram.org/bots/features#botfather
- Bot API: https://core.telegram.org/bots/api

---

**✅ Setup Complete!**

You can now receive real-time notifications from your clud agent! 🚀
