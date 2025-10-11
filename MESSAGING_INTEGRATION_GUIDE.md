# Messaging Integration Guide

This guide explains how to use the messaging features in CLUD to receive notifications when Claude agents launch and clean up.

## Supported Platforms

- **Telegram** - Free, easy setup, rich features (Recommended)
- **SMS** - Universal reach via Twilio
- **WhatsApp** - High engagement via Meta Cloud API

## Installation

### Base Installation

Install the optional messaging dependencies:

```bash
pip install python-telegram-bot twilio requests
```

Or using requirements file:

```bash
pip install -r src/clud/messaging/requirements.txt
```

## Platform Setup

### Telegram Setup (Recommended)

1. **Create a Bot**:
   - Open Telegram and search for `@BotFather`
   - Send `/newbot` command
   - Follow prompts to name your bot
   - Save the bot token (e.g., `123456:ABC-DEF1234...`)

2. **Get Your Chat ID**:
   - Search for `@userinfobot` on Telegram
   - Send `/start` command
   - Note your chat ID (e.g., `123456789`)

3. **Start Your Bot**:
   - Search for your bot by username
   - Send `/start` to activate it

4. **Use with CLUD**:

```bash
# Using command line arguments
clud bg --messaging telegram \
  --telegram-bot-token "123456:ABC-DEF1234..." \
  --telegram-chat-id "123456789"

# Using environment variables
export TELEGRAM_BOT_TOKEN="123456:ABC-DEF1234..."
export TELEGRAM_CHAT_ID="123456789"
clud bg --messaging telegram
```

### SMS Setup (Twilio)

1. **Create Twilio Account**:
   - Sign up at https://www.twilio.com/try-twilio
   - Get free trial credits

2. **Get Credentials**:
   - Find your Account SID and Auth Token in console
   - Get a phone number (or use trial number)

3. **Use with CLUD**:

```bash
# Using command line arguments
clud bg --messaging sms \
  --sms-account-sid "ACxxxxxxxxxxxx" \
  --sms-auth-token "your_auth_token" \
  --sms-from-number "+1234567890" \
  --sms-to-number "+1987654321"

# Using environment variables
export TWILIO_ACCOUNT_SID="ACxxxxxxxxxxxx"
export TWILIO_AUTH_TOKEN="your_auth_token"
export TWILIO_FROM_NUMBER="+1234567890"
export TWILIO_TO_NUMBER="+1987654321"
clud bg --messaging sms
```

### WhatsApp Setup

1. **Create Meta Developer Account**:
   - Go to https://developers.facebook.com
   - Create app and add WhatsApp product

2. **Get Credentials**:
   - Get Phone Number ID from WhatsApp settings
   - Generate access token

3. **Use with CLUD**:

```bash
# Using command line arguments
clud bg --messaging whatsapp \
  --whatsapp-phone-id "123456789012345" \
  --whatsapp-access-token "your_access_token" \
  --whatsapp-to-number "+1987654321"

# Using environment variables
export WHATSAPP_PHONE_ID="123456789012345"
export WHATSAPP_ACCESS_TOKEN="your_access_token"
export WHATSAPP_TO_NUMBER="+1987654321"
clud bg --messaging whatsapp
```

## Usage Examples

### Basic Usage

```bash
# Launch agent with Telegram notifications
clud bg --messaging telegram --telegram-chat-id 123456789

# Launch with SMS notifications
clud bg --messaging sms --sms-to-number +1234567890

# Launch with WhatsApp notifications
clud bg --messaging whatsapp --whatsapp-to-number +1234567890
```

### With Environment Variables

```bash
# Set credentials in environment
export TELEGRAM_BOT_TOKEN="123456:ABC-DEF..."
export TELEGRAM_CHAT_ID="123456789"

# Launch agent
clud bg --messaging telegram
```

### Combined with Other Features

```bash
# Messaging + browser UI
clud bg --messaging telegram --telegram-chat-id 123456789 --open

# Messaging + specific command
clud bg --messaging telegram --telegram-chat-id 123456789 --cmd "pytest tests/"

# Messaging + custom project path
clud bg /path/to/project --messaging telegram --telegram-chat-id 123456789
```

## Notification Types

### Launch Invitation

When your agent starts, you'll receive:

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

## Configuration File

You can store messaging configuration in a `.clud` file in your project:

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

Note: Config file support is planned for future release.

## Security

### Best Practices

1. **Use Environment Variables**: Don't commit tokens to git
2. **Restrict Bot Access**: Only share bot with trusted users
3. **Rotate Tokens**: Periodically regenerate API tokens
4. **Use .gitignore**: Add credential files to .gitignore

### Environment Variables

Create a `.env` file (and add to .gitignore):

```bash
# Telegram
TELEGRAM_BOT_TOKEN=123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11
TELEGRAM_CHAT_ID=123456789

# Twilio SMS
TWILIO_ACCOUNT_SID=ACxxxxxxxxxxxx
TWILIO_AUTH_TOKEN=your_auth_token
TWILIO_FROM_NUMBER=+1234567890
TWILIO_TO_NUMBER=+1987654321

# WhatsApp
WHATSAPP_PHONE_ID=123456789012345
WHATSAPP_ACCESS_TOKEN=your_access_token
WHATSAPP_TO_NUMBER=+1987654321
```

Load with:

```bash
source .env  # or use direnv, dotenv, etc.
clud bg --messaging telegram
```

## Troubleshooting

### Telegram

**Issue**: Bot not sending messages
- **Solution**: Make sure you've sent `/start` to the bot first

**Issue**: Invalid chat ID
- **Solution**: Use `@userinfobot` to get correct chat ID

### SMS

**Issue**: Messages not sending
- **Solution**: Verify Twilio credentials and phone numbers are in E.164 format (+1234567890)

**Issue**: Trial account restrictions
- **Solution**: Verify recipient phone number in Twilio console for trial accounts

### WhatsApp

**Issue**: Template not approved
- **Solution**: WhatsApp requires pre-approved templates for proactive messages

**Issue**: 24-hour window restriction
- **Solution**: Can only send free-form messages within 24h of user message

## Cost Comparison

| Platform | Setup Cost | Monthly Cost (150 msgs) | Features |
|----------|-----------|------------------------|----------|
| Telegram | $0 | $0 | Rich, Free |
| SMS | $1 (number) | $13-16 | Universal |
| WhatsApp | $0 | $0-2 | Rich, Popular |

## CLI Reference

### Messaging Arguments

```bash
--messaging PLATFORM          Enable messaging (telegram, sms, or whatsapp)

# Telegram
--telegram-bot-token TOKEN    Telegram bot token
--telegram-chat-id ID         Telegram chat ID

# SMS (Twilio)
--sms-account-sid SID         Twilio account SID
--sms-auth-token TOKEN        Twilio auth token
--sms-from-number NUMBER      Phone number to send from
--sms-to-number NUMBER        Phone number to send to

# WhatsApp
--whatsapp-phone-id ID        WhatsApp phone number ID
--whatsapp-access-token TOKEN WhatsApp access token
--whatsapp-to-number NUMBER   Phone number to send to
```

### Environment Variables

```bash
# Telegram
TELEGRAM_BOT_TOKEN
TELEGRAM_CHAT_ID

# SMS
TWILIO_ACCOUNT_SID
TWILIO_AUTH_TOKEN
TWILIO_FROM_NUMBER
TWILIO_TO_NUMBER

# WhatsApp
WHATSAPP_PHONE_ID
WHATSAPP_ACCESS_TOKEN
WHATSAPP_TO_NUMBER
```

## Examples

### Personal Development

```bash
# Get notified when long-running tasks complete
clud bg --cmd "pytest tests/" --messaging telegram --telegram-chat-id 123456789
```

### Team Collaboration

```bash
# Multiple developers can receive notifications from shared bot
clud bg --messaging telegram --telegram-bot-token $TEAM_BOT_TOKEN --telegram-chat-id $TEAM_CHAT_ID
```

### CI/CD Integration

```bash
# Get notified of CI/CD container status
clud bg --cmd "./deploy.sh" --messaging sms --sms-to-number +1234567890 --detect-completion
```

## Support

For issues or questions:
- Check the feasibility report: `MESSAGING_AGENT_FEASIBILITY_REPORT.md`
- File an issue on GitHub
- Check Telegram Bot API docs: https://core.telegram.org/bots/api
- Check Twilio docs: https://www.twilio.com/docs/sms
- Check WhatsApp Cloud API docs: https://developers.facebook.com/docs/whatsapp/cloud-api
