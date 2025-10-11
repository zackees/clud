# Messaging Quick Start Guide

Get Claude agent notifications in 5 minutes! 🚀

## Choose Your Platform

### 🥇 Telegram (Easiest & Free)

**1. Create Bot (2 minutes):**
```
1. Open Telegram, search for @BotFather
2. Send: /newbot
3. Follow prompts
4. Save bot token: 123456:ABC-DEF...
```

**2. Get Chat ID (1 minute):**
```
1. Search for @userinfobot
2. Send: /start
3. Note your ID: 123456789
```

**3. Launch Agent (30 seconds):**
```bash
clud bg --messaging telegram \
  --telegram-bot-token "123456:ABC-DEF..." \
  --telegram-chat-id "123456789"
```

**4. Enjoy! 🎉**
You'll receive:
- "🚀 Agent launched!" when starting
- "✅ Agent complete!" when finishing

---

### 📱 SMS via Twilio

**1. Sign Up:**
- https://www.twilio.com/try-twilio
- Get $15 free credit

**2. Get Credentials:**
- Account SID: ACxxxxxxxxxxxx
- Auth Token: your_token
- Phone number: +1234567890

**3. Launch:**
```bash
export TWILIO_ACCOUNT_SID="ACxxxx..."
export TWILIO_AUTH_TOKEN="your_token"
export TWILIO_FROM_NUMBER="+1234567890"
export TWILIO_TO_NUMBER="+0987654321"

clud bg --messaging sms
```

---

### 💬 WhatsApp Business API

**Note:** More complex setup, requires business verification

**1. Create Meta Developer Account**
- https://developers.facebook.com

**2. Get Credentials**
- Phone Number ID
- Access Token

**3. Launch:**
```bash
clud bg --messaging whatsapp \
  --whatsapp-phone-id "123..." \
  --whatsapp-access-token "token" \
  --whatsapp-to-number "+1234567890"
```

---

## Pro Tips

### Use Environment Variables

Create `.env` file:
```bash
# Telegram
TELEGRAM_BOT_TOKEN=123456:ABC-DEF...
TELEGRAM_CHAT_ID=123456789

# Twilio
TWILIO_ACCOUNT_SID=ACxxxx...
TWILIO_AUTH_TOKEN=your_token
TWILIO_FROM_NUMBER=+1234567890
TWILIO_TO_NUMBER=+0987654321
```

Load and use:
```bash
source .env
clud bg --messaging telegram
```

### Use Config File

Create `.clud` in your project:
```json
{
  "messaging": {
    "enabled": true,
    "platform": "telegram",
    "telegram": {
      "bot_token": "${TELEGRAM_BOT_TOKEN}",
      "chat_id": "${TELEGRAM_CHAT_ID}"
    }
  }
}
```

Launch:
```bash
clud bg  # Auto-loads config
```

---

## What You'll Receive

### Launch Notification
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
```
✅ Agent Cleanup Complete

Agent: clud-dev
Duration: 0:15:23
Tasks Completed: 0
Files Modified: 0
Errors: 0

Status: 🔴 Offline
```

---

## Installation

```bash
# Install messaging dependencies
pip install python-telegram-bot twilio requests

# Or from requirements
pip install -r src/clud/messaging/requirements.txt
```

---

## Troubleshooting

**Telegram: "Bot not sending messages"**
- Solution: Send `/start` to your bot first

**SMS: "Invalid phone number"**
- Solution: Use E.164 format: +1234567890

**WhatsApp: "Template not found"**
- Solution: WhatsApp requires pre-approved templates

---

## Next Steps

- Read full guide: [MESSAGING_INTEGRATION_GUIDE.md](MESSAGING_INTEGRATION_GUIDE.md)
- Read feasibility report: [MESSAGING_AGENT_FEASIBILITY_REPORT.md](MESSAGING_AGENT_FEASIBILITY_REPORT.md)
- View implementation: [IMPLEMENTATION_SUMMARY.md](IMPLEMENTATION_SUMMARY.md)

---

## Cost Comparison

| Platform | Cost | Setup Time |
|----------|------|------------|
| Telegram | FREE | 3 minutes |
| SMS | $13-16/month | 10 minutes |
| WhatsApp | $0-2/month | 30+ minutes |

**Recommendation:** Start with Telegram!

---

Made with ❤️ for the CLUD community
