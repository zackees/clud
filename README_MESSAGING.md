# Messaging Integration - Quick Start Guide

**Get real-time notifications from your clud agent via Telegram, SMS, or WhatsApp!**

---

## ⚡ 5-Minute Setup

### Step 1: Create Telegram Bot (2 minutes)

1. Open Telegram → Search `@BotFather` → START
2. Send: `/newbot`
3. Name: "My Clud Bot"
4. Username: "my_clud_bot" (must end with 'bot')
5. **Copy token:** `1234567890:ABC...`

### Step 2: Get Your Chat ID (1 minute)

1. Search `@userinfobot` → Send any message
2. **Copy ID:** `123456789`

### Step 3: Configure clud (1 minute)

```bash
pip install clud[messaging]
clud --configure-messaging
# Paste token when prompted
```

### Step 4: Test It (1 minute)

```bash
clud --notify-user "123456789" --cmd "echo Hello from clud!"
```

**Check your Telegram!** You should receive:
```
🤖 Clud Agent Starting
Task: echo Hello from clud!
✅ Completed Successfully (1s)
```

---

## 📱 Usage

### Telegram (Free, Recommended)
```bash
clud --notify-user "123456789" -m "Deploy to production"
clud --notify-user "@username" -m "Run tests"  # If user started your bot
```

### SMS (Costs ~$0.0075/msg)
```bash
clud --notify-user "+14155551234" -m "Build Docker image"
```

### WhatsApp (Costs ~$0.005/msg)
```bash
clud --notify-user "whatsapp:+14155551234" -m "Run integration tests"
```

### Custom Update Interval
```bash
# Update every 60 seconds (default: 30)
clud --notify-user "123456789" --notify-interval 60 -m "Long task"
```

---

## 🔒 How Credentials Are Stored

### Secure (Recommended):
- **Encrypted:** `~/.clud/credentials.enc` (Fernet AES-128)
- **Permissions:** 0600 (owner only)
- **OS Integration:** Uses Keychain (macOS), Credential Manager (Windows)

### Priority Order:
1. Environment variables (highest)
2. Encrypted credential store ← clud saves here
3. Individual .key files (legacy)
4. Plain JSON (deprecated, warns)

**Setup saves to encrypted store automatically!** ✅

---

## 📖 Full Documentation

### For Setup:
- **[TELEGRAM_BOT_SETUP_GUIDE.md](./TELEGRAM_BOT_SETUP_GUIDE.md)** - Complete BotFather walkthrough
- **[MESSAGING_SETUP.md](./MESSAGING_SETUP.md)** - Full setup guide (Telegram, SMS, WhatsApp)

### For Usage:
- **[EXAMPLES.md](./EXAMPLES.md)** - 23 real-world examples
- **[MESSAGING_INDEX.md](./MESSAGING_INDEX.md)** - Navigate all docs

### For Technical Details:
- **[FINAL_IMPLEMENTATION_REPORT.md](./FINAL_IMPLEMENTATION_REPORT.md)** - Complete overview
- **[CREDENTIAL_INTEGRATION_REPORT.md](./CREDENTIAL_INTEGRATION_REPORT.md)** - Security details

---

## 🎯 Common Scenarios

### Development Notifications:
```bash
clud --notify-user "@dev" -m "Fix authentication bug"
```

### Production Deployments:
```bash
clud --notify-user "+14155551234" -m "Deploy v2.5.0 to production"
```

### Long-Running Tasks:
```bash
clud --notify-user "123456789" --notify-interval 60 -m "Migrate database schema"
```

### Team Collaboration:
```bash
# Notify reviewer when done
clud --notify-user "@code-reviewer" -m "Implement OAuth and create PR"
```

### CI/CD Integration:
```yaml
# .github/workflows/deploy.yml
- name: Deploy with notifications
  env:
    TELEGRAM_BOT_TOKEN: ${{ secrets.TELEGRAM_BOT_TOKEN }}
  run: |
    clud --notify-user "${{ secrets.CHAT_ID }}" -m "Deploy ${{ github.sha }}"
```

---

## ❓ FAQ

**Q: Which channel should I use?**  
A: Telegram (free, best for devs), SMS (universal), WhatsApp (international)

**Q: How much does it cost?**  
A: Telegram is free. SMS ~$0.03/run, WhatsApp ~$0.02/run

**Q: Is it secure?**  
A: Yes! Credentials encrypted with Fernet, stored with 0600 permissions

**Q: Where are credentials stored?**  
A: `~/.clud/credentials.enc` (encrypted) or OS keyring

**Q: Can I use environment variables?**  
A: Yes! They have highest priority (good for CI/CD)

**Q: What if I lose my bot token?**  
A: Message @BotFather, send `/token`, select your bot

**Q: Can I use multiple bots?**  
A: Yes! Use different env vars or config files

---

## 🆘 Troubleshooting

**Can't find BotFather:**
- Search `@BotFather` (with @) in Telegram

**Bot doesn't send messages:**
- Verify token with: `curl https://api.telegram.org/bot{TOKEN}/getMe`
- Check chat ID with @userinfobot
- Make sure you sent /start to your bot

**Credentials not loading:**
- Run: `clud --configure-messaging` again
- Or use env vars: `export TELEGRAM_BOT_TOKEN="..."`

**Migration issues:**
- Backup exists at: `~/.clud/messaging.json.backup`
- Can manually configure: `clud --configure-messaging`

**For more:** See [MESSAGING_SETUP.md](./MESSAGING_SETUP.md)#troubleshooting

---

## 🎉 You're Ready!

```bash
# Create bot (Telegram)
@BotFather → /newbot

# Get chat ID (Telegram)  
@userinfobot

# Configure (terminal)
clud --configure-messaging

# Use it! (terminal)
clud --notify-user "YOUR_CHAT_ID" -m "Your task here"
```

**Need help?** Read [MESSAGING_INDEX.md](./MESSAGING_INDEX.md) for complete documentation navigation.

---

**Happy coding with real-time notifications! 🚀**
