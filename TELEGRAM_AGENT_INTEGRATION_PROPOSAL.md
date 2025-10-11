# Telegram/SMS/WhatsApp Communication Integration Proposal for Clud

## Executive Summary

This proposal outlines the integration of multi-channel communication (Telegram, SMS, WhatsApp) into the `clud` foreground agent, enabling automated bot interactions with users. The system will allow the agent to reach out to users, invite them to sessions, and provide status updates throughout the development process.

---

## 1. Command-Line Argument Design

### Three Alternatives Analyzed:

#### Option 1: `--notify-user <contact>`
**Pros:**
- Clear intent: "notify" implies bidirectional communication
- Flexible: contact can be phone, username, or chat ID
- Consistent with existing `--` flag pattern

**Cons:**
- Slightly generic term

**Example Usage:**
```bash
clud --notify-user "+1234567890" -m "Fix the login bug"
clud --notify-user "@username" -p "Add dark mode"
clud --notify-user "telegram:123456789" --continue
```

#### Option 2: `--connect-to <contact>`
**Pros:**
- Emphasizes the connection/session aspect
- Intuitive for establishing communication channel
- Clear bidirectional intent

**Cons:**
- Could be confused with network connections

**Example Usage:**
```bash
clud --connect-to "+1234567890" -m "Refactor authentication"
clud --connect-to "@telegramuser" -p "Implement API"
```

#### Option 3: `--user-channel <contact>`
**Pros:**
- Most technically accurate
- Clearly separates contact from other arguments
- Emphasizes the channel concept

**Cons:**
- More verbose
- Less intuitive for end users

**Example Usage:**
```bash
clud --user-channel "+1234567890" -m "Debug issue"
clud --user-channel "telegram:user123" -p "Deploy app"
```

### **RECOMMENDED: Option 1 (`--notify-user`)**

**Rationale:**
- Most user-friendly and intuitive
- Clearly communicates the purpose: keeping users informed
- Flexible format supports all contact types
- Follows Unix convention of descriptive flags

**Contact Format Specification:**
```
--notify-user <contact>

Where <contact> can be:
  +1234567890           # Phone number (SMS via Twilio)
  whatsapp:+1234567890  # WhatsApp via Twilio
  telegram:@username    # Telegram username
  telegram:123456789    # Telegram chat ID
  @username             # Auto-detect (defaults to Telegram)
```

---

## 2. Messaging API Research & Selection

### Primary API: Twilio

**Why Twilio:**
- **Unified API** for SMS and WhatsApp
- Enterprise-grade reliability (99.95% uptime SLA)
- Excellent Python SDK (`twilio` package)
- Simple authentication (Account SID + Auth Token)
- Cost-effective pricing (~$0.0075/SMS, ~$0.005/WhatsApp message)

**Twilio Capabilities:**
- ✅ SMS messaging to any phone number
- ✅ WhatsApp messaging (via Twilio WhatsApp API)
- ✅ Message status tracking (delivered, read, failed)
- ✅ Bidirectional messaging (receive replies)
- ✅ Media attachments (images, files)

### Secondary API: Telegram Bot API

**Why Telegram Bot API:**
- **Native Python SDK** (`python-telegram-bot`)
- Free (no per-message costs)
- Rich features (inline keyboards, formatting, code blocks)
- Excellent for developer communities
- Easy bot creation via @BotFather

**Telegram Bot Capabilities:**
- ✅ Send messages to users (after they start the bot)
- ✅ Rich text formatting (Markdown, HTML)
- ✅ Interactive buttons and keyboards
- ✅ Code syntax highlighting
- ✅ File sharing (logs, screenshots)

### API Comparison Matrix

| Feature | Twilio (SMS) | Twilio (WhatsApp) | Telegram Bot |
|---------|-------------|-------------------|--------------|
| **Cost** | ~$0.0075/msg | ~$0.005/msg | Free |
| **Setup Complexity** | Low | Medium | Low |
| **Rich Formatting** | No | Limited | Yes |
| **Code Blocks** | No | Limited | Yes |
| **Interactive UI** | No | Limited | Yes |
| **User Reach** | Universal | High | Developer-focused |
| **Status Updates** | Basic | Good | Excellent |
| **Authentication** | API Key | API Key | Bot Token |

---

## 3. Current Foreground Agent Analysis

### Key Integration Points

#### 3.1 Entry Point: `agent_foreground.py::main()`
**Location:** `src/clud/agent_foreground.py:435-443`

Current flow:
```python
def main(args: list[str] | None = None) -> int:
    parsed_args = parse_args(args)
    return run(parsed_args)
```

**Integration Point:**
- Parse `--notify-user` argument
- Initialize notification client before `run()`
- Send initial notification: "Starting task: {message}"

#### 3.2 Command Execution: `agent_foreground.py::run()`
**Location:** `src/clud/agent_foreground.py:341-433`

Current responsibilities:
- Find Claude executable
- Build command with `--dangerously-skip-permissions`
- Execute Claude Code
- Return exit code

**Integration Points:**
1. **Before execution** (line ~385):
   ```python
   # Send: "🤖 Agent starting: {task_description}"
   ```

2. **During execution** (wrap subprocess.run):
   ```python
   # Send periodic updates every N seconds
   # Send: "⏳ Working on: {current_activity}"
   ```

3. **After execution** (line ~390+):
   ```python
   # Send: "✅ Task completed successfully" (if returncode == 0)
   # Send: "❌ Task failed with errors" (if returncode != 0)
   ```

#### 3.3 Argument Parsing: `agent_foreground_args.py::parse_args()`
**Location:** `src/clud/agent_foreground_args.py:21-84`

**Required Changes:**
```python
@dataclass
class Args:
    # ... existing fields ...
    notify_user: str | None  # New field
    notify_interval: int = 30  # Update interval in seconds

# In parse_args():
parser.add_argument(
    "--notify-user",
    type=str,
    help="Send status updates to user via Telegram/SMS/WhatsApp",
)
parser.add_argument(
    "--notify-interval",
    type=int,
    default=30,
    help="Seconds between status updates (default: 30)",
)
```

#### 3.4 API Key Management
**Location:** `src/clud/agent_foreground.py:38-248`

Existing patterns to follow:
- `get_api_key()` - Loads from keyring/config/env/prompt
- `save_api_key_to_config()` - Stores in `~/.clud/`
- `load_api_key_from_config()` - Retrieves from `~/.clud/`

**New Functions Needed:**
```python
def get_twilio_credentials() -> tuple[str, str]:
    """Get Twilio Account SID and Auth Token."""
    # Priority: env vars > config file > prompt
    
def get_telegram_token() -> str:
    """Get Telegram Bot token."""
    # Priority: env vars > config file > prompt

def save_messaging_credentials() -> None:
    """Save Twilio/Telegram credentials to ~/.clud/"""
```

---

## 4. Proposed Architecture

### 4.1 New Module Structure

```
src/clud/
├── messaging/
│   ├── __init__.py
│   ├── base.py              # Abstract base class
│   ├── telegram_client.py   # Telegram Bot API client
│   ├── twilio_client.py     # SMS/WhatsApp client
│   ├── factory.py           # Auto-detect channel from contact
│   └── notifier.py          # High-level notification manager
```

### 4.2 Class Hierarchy

```python
# messaging/base.py
from abc import ABC, abstractmethod

class MessagingClient(ABC):
    """Abstract base for all messaging clients."""
    
    @abstractmethod
    async def send_message(self, contact: str, message: str) -> bool:
        """Send a text message."""
        pass
    
    @abstractmethod
    async def send_code_block(self, contact: str, code: str, language: str = "python") -> bool:
        """Send formatted code."""
        pass
    
    @abstractmethod
    async def get_user_response(self, contact: str, timeout: int = 60) -> str | None:
        """Wait for user response (optional)."""
        pass


# messaging/telegram_client.py
import asyncio
from telegram import Bot
from telegram.error import TelegramError

class TelegramClient(MessagingClient):
    """Telegram Bot API client."""
    
    def __init__(self, token: str):
        self.bot = Bot(token=token)
        self._chat_id_cache: dict[str, int] = {}
    
    async def send_message(self, contact: str, message: str) -> bool:
        """Send message via Telegram.
        
        Args:
            contact: @username or chat_id
            message: Text to send
        """
        try:
            chat_id = await self._resolve_chat_id(contact)
            await self.bot.send_message(
                chat_id=chat_id,
                text=message,
                parse_mode="Markdown"
            )
            return True
        except TelegramError as e:
            logger.error(f"Telegram send failed: {e}")
            return False
    
    async def send_code_block(self, contact: str, code: str, language: str = "python") -> bool:
        """Send formatted code block."""
        formatted = f"```{language}\n{code}\n```"
        return await self.send_message(contact, formatted)
    
    async def _resolve_chat_id(self, contact: str) -> int:
        """Convert @username to chat_id."""
        # Implementation with caching
        pass


# messaging/twilio_client.py
from twilio.rest import Client as TwilioClient
from twilio.base.exceptions import TwilioException

class TwilioSMSClient(MessagingClient):
    """Twilio SMS and WhatsApp client."""
    
    def __init__(self, account_sid: str, auth_token: str, from_number: str):
        self.client = TwilioClient(account_sid, auth_token)
        self.from_number = from_number
    
    async def send_message(self, contact: str, message: str) -> bool:
        """Send SMS or WhatsApp message.
        
        Args:
            contact: +1234567890 or whatsapp:+1234567890
            message: Text to send (max 1600 chars)
        """
        try:
            # Twilio API is sync, wrap in executor
            loop = asyncio.get_event_loop()
            await loop.run_in_executor(
                None,
                lambda: self.client.messages.create(
                    body=message[:1600],  # SMS limit
                    from_=self._format_from(contact),
                    to=contact
                )
            )
            return True
        except TwilioException as e:
            logger.error(f"Twilio send failed: {e}")
            return False
    
    def _format_from(self, contact: str) -> str:
        """Format from number based on destination."""
        if contact.startswith("whatsapp:"):
            return f"whatsapp:{self.from_number}"
        return self.from_number


# messaging/factory.py
def create_client(contact: str, config: dict) -> MessagingClient:
    """Auto-detect and create appropriate messaging client.
    
    Args:
        contact: User contact string
        config: Credentials and settings
        
    Returns:
        Appropriate MessagingClient instance
    """
    if contact.startswith("telegram:") or contact.startswith("@"):
        return TelegramClient(config["telegram_token"])
    elif contact.startswith("whatsapp:"):
        return TwilioSMSClient(
            config["twilio_sid"],
            config["twilio_token"],
            config["twilio_number"]
        )
    elif contact.startswith("+"):
        return TwilioSMSClient(
            config["twilio_sid"],
            config["twilio_token"],
            config["twilio_number"]
        )
    else:
        # Default to Telegram for usernames
        return TelegramClient(config["telegram_token"])


# messaging/notifier.py
class AgentNotifier:
    """High-level notification manager for agent status updates."""
    
    def __init__(self, client: MessagingClient, contact: str):
        self.client = client
        self.contact = contact
        self._last_update = 0
        self._update_interval = 30
    
    async def notify_start(self, task: str) -> None:
        """Notify user that agent is starting."""
        message = f"🤖 **Clud Agent Starting**\n\nTask: {task}\n\nI'll keep you updated on progress!"
        await self.client.send_message(self.contact, message)
    
    async def notify_progress(self, status: str, elapsed: int) -> None:
        """Send periodic progress updates."""
        current_time = time.time()
        if current_time - self._last_update < self._update_interval:
            return
        
        message = f"⏳ **Working** ({elapsed}s)\n\n{status}"
        await self.client.send_message(self.contact, message)
        self._last_update = current_time
    
    async def notify_completion(self, success: bool, duration: int, summary: str = "") -> None:
        """Notify user of completion."""
        emoji = "✅" if success else "❌"
        status = "Completed" if success else "Failed"
        message = f"{emoji} **{status}** ({duration}s)\n\n{summary}"
        await self.client.send_message(self.contact, message)
    
    async def notify_error(self, error: str) -> None:
        """Notify user of error."""
        message = f"⚠️ **Error**\n\n```\n{error}\n```"
        await self.client.send_message(self.contact, message)
```

### 4.3 Integration Flow

```
┌─────────────────────────────────────────────────────────────┐
│  User runs: clud --notify-user "+1234567890" -m "Fix bug"  │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│  agent_foreground.py::main()                                │
│  1. Parse args (including --notify-user)                    │
│  2. Load messaging credentials                              │
│  3. Create MessagingClient via factory                      │
│  4. Create AgentNotifier                                    │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│  AgentNotifier.notify_start()                               │
│  → "🤖 Clud Agent Starting\nTask: Fix bug"                  │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│  agent_foreground.py::run()                                 │
│  1. Find Claude executable                                  │
│  2. Build command                                           │
│  3. Execute in wrapped subprocess                           │
└────────────────────────┬────────────────────────────────────┘
                         │
         ┌───────────────┴───────────────┐
         │                               │
         ▼                               ▼
┌──────────────────┐           ┌──────────────────┐
│ Every 30 seconds │           │  Claude Process  │
│ notify_progress()│           │   (running...)   │
│ → "⏳ Working"   │           └──────────────────┘
└──────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────┐
│  Process completes                                          │
│  AgentNotifier.notify_completion()                          │
│  → "✅ Completed (120s)\n\nBug fixed successfully!"         │
└─────────────────────────────────────────────────────────────┘
```

---

## 5. Implementation Plan

### Phase 1: Core Messaging Infrastructure (Week 1)

**Tasks:**
1. Create `src/clud/messaging/` module structure
2. Implement `base.py` with `MessagingClient` abstract class
3. Implement `telegram_client.py` with basic send functionality
4. Implement `twilio_client.py` with SMS/WhatsApp support
5. Create `factory.py` for auto-detection
6. Add dependencies to `pyproject.toml`:
   ```toml
   dependencies = [
       # ... existing ...
       "python-telegram-bot>=21.0",
       "twilio>=9.0.0",
   ]
   ```

**Testing:**
```bash
# Unit tests
uv run pytest tests/test_messaging_telegram.py
uv run pytest tests/test_messaging_twilio.py

# Integration tests (requires credentials)
export TELEGRAM_BOT_TOKEN="..."
export TWILIO_ACCOUNT_SID="..."
export TWILIO_AUTH_TOKEN="..."
uv run pytest tests/integration/test_messaging_integration.py
```

### Phase 2: Agent Integration (Week 2)

**Tasks:**
1. Add `--notify-user` argument to `agent_foreground_args.py`
2. Implement credential management in `agent_foreground.py`
3. Create `AgentNotifier` class in `messaging/notifier.py`
4. Integrate notification hooks in `run()` function
5. Add async subprocess wrapper for progress monitoring

**Code Changes:**

```python
# agent_foreground_args.py
@dataclass
class Args:
    # ... existing fields ...
    notify_user: str | None
    notify_interval: int = 30

def parse_args(args: list[str] | None = None) -> Args:
    # ... existing code ...
    parser.add_argument(
        "--notify-user",
        type=str,
        help="Send status updates via Telegram/SMS/WhatsApp (format: +1234567890, @username, telegram:123456789, whatsapp:+1234567890)",
    )
    parser.add_argument(
        "--notify-interval",
        type=int,
        default=30,
        help="Seconds between progress updates (default: 30)",
    )
    # ... rest of parsing ...
    return Args(
        # ... existing fields ...
        notify_user=known_args.notify_user,
        notify_interval=known_args.notify_interval,
    )
```

```python
# agent_foreground.py - Modified run() function
async def run_with_notifications(args: Args) -> int:
    """Run Claude with notification support."""
    notifier = None
    
    if args.notify_user:
        # Initialize notifier
        config = load_messaging_config()
        client = create_client(args.notify_user, config)
        notifier = AgentNotifier(client, args.notify_user)
        
        # Send start notification
        task_desc = args.message or args.prompt or "Running Claude Code"
        await notifier.notify_start(task_desc)
    
    try:
        # Original run() logic here
        start_time = time.time()
        
        # Find Claude executable
        claude_path = _find_claude_path()
        if not claude_path:
            if notifier:
                await notifier.notify_error("Claude Code not found in PATH")
            return 1
        
        # Build command
        cmd = _build_claude_command(args, claude_path)
        
        # Execute with progress monitoring
        returncode = await _execute_with_monitoring(
            cmd, 
            notifier,
            args.notify_interval
        )
        
        # Send completion notification
        if notifier:
            duration = int(time.time() - start_time)
            success = returncode == 0
            await notifier.notify_completion(success, duration)
        
        return returncode
        
    except Exception as e:
        if notifier:
            await notifier.notify_error(str(e))
        raise

def run(args: Args) -> int:
    """Wrapper to run async function."""
    if args.notify_user:
        return asyncio.run(run_with_notifications(args))
    else:
        # Original synchronous path
        # ... existing run() implementation ...
        pass
```

### Phase 3: Enhanced Features (Week 3)

**Tasks:**
1. Add configuration command: `clud --configure-messaging`
2. Implement credential storage in `~/.clud/messaging.json`
3. Add message templates for different event types
4. Implement error retry logic with exponential backoff
5. Add support for inline code formatting (Telegram)
6. Create logging integration (send error logs on failure)

**Configuration File Example:**
```json
// ~/.clud/messaging.json
{
  "telegram": {
    "bot_token": "1234567890:ABCdefGHIjklMNOpqrsTUVwxyz",
    "enabled": true
  },
  "twilio": {
    "account_sid": "ACxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "auth_token": "your_auth_token",
    "from_number": "+15555555555",
    "enabled": true
  },
  "preferences": {
    "default_channel": "telegram",
    "update_interval": 30,
    "include_errors": true,
    "include_logs": false
  }
}
```

### Phase 4: Testing & Documentation (Week 4)

**Tasks:**
1. Write comprehensive unit tests
2. Write integration tests with mocked APIs
3. Create user documentation
4. Update README.md with examples
5. Create troubleshooting guide

---

## 6. Configuration & Setup Guide

### 6.1 Telegram Bot Setup

**Step 1: Create Bot**
```bash
# Talk to @BotFather on Telegram
# Send: /newbot
# Follow prompts to get bot token
```

**Step 2: Configure Clud**
```bash
# Save token to environment
export TELEGRAM_BOT_TOKEN="1234567890:ABCdefGHI..."

# Or save to config
echo "1234567890:ABCdefGHI..." > ~/.clud/telegram-bot-token.key
chmod 600 ~/.clud/telegram-bot-token.key
```

**Step 3: Start Bot Conversation**
```
User sends /start to bot → Bot can now message user
```

### 6.2 Twilio Setup

**Step 1: Create Account**
- Sign up at https://www.twilio.com/try-twilio
- Get free trial credits ($15)
- Note Account SID and Auth Token

**Step 2: Get Phone Number**
- Purchase a Twilio phone number (~$1/month)
- Enable SMS and/or WhatsApp

**Step 3: Configure Clud**
```bash
# Environment variables
export TWILIO_ACCOUNT_SID="ACxxxxxxxxxxxxxxxx"
export TWILIO_AUTH_TOKEN="your_auth_token"
export TWILIO_FROM_NUMBER="+15555555555"

# Or use config file
clud --configure-messaging
```

### 6.3 WhatsApp Setup (via Twilio)

**Step 1: Join Twilio Sandbox**
- WhatsApp requires business verification for production
- Use Twilio WhatsApp Sandbox for testing
- Send "join <your-sandbox-code>" to Twilio WhatsApp number

**Step 2: Use in Clud**
```bash
clud --notify-user "whatsapp:+1234567890" -m "Deploy app"
```

---

## 7. Usage Examples

### Basic SMS Notification
```bash
clud --notify-user "+14155551234" -m "Fix authentication bug"
```
**Output to phone:**
```
🤖 Clud Agent Starting

Task: Fix authentication bug

I'll keep you updated on progress!

---

⏳ Working (30s)

Analyzing authentication flow...

---

✅ Completed (120s)

Bug fixed successfully!
```

### Telegram with Code Updates
```bash
clud --notify-user "@devuser" -p "Refactor database queries"
```
**Output to Telegram:**
```
🤖 **Clud Agent Starting**

Task: Refactor database queries

I'll keep you updated on progress!

---

⏳ **Working** (60s)

Optimizing query performance...

---

✅ **Completed** (180s)

Refactored 5 queries, added indexes.
Performance improved by 40%.
```

### WhatsApp with Custom Interval
```bash
clud --notify-user "whatsapp:+14155551234" --notify-interval 60 -m "Deploy to production"
```

### Background Mode with Notifications
```bash
clud bg --notify-user "@devuser" --cmd "pytest tests/"
```

---

## 8. Security Considerations

### Credential Storage
- ✅ Store API keys in `~/.clud/` with `0600` permissions
- ✅ Support environment variables for CI/CD
- ✅ Use OS keyring when available (via existing `keyring` module)
- ❌ Never log credentials
- ❌ Never commit credentials to git

### Rate Limiting
- Implement exponential backoff for failed sends
- Respect API rate limits:
  - Telegram: 30 messages/second per bot
  - Twilio: 1 message/second (default)
- Queue messages if rate limit hit

### Error Handling
- Graceful degradation if messaging fails
- Don't block agent execution on notification failure
- Log notification errors to console
- Retry failed notifications with backoff

### User Privacy
- Only send notifications when explicitly requested
- Allow users to opt-out mid-session
- Don't include sensitive data in notifications
- Respect user's do-not-disturb settings (if API supports)

---

## 9. Cost Analysis

### Twilio Pricing (as of 2025)

**SMS:**
- US/Canada: $0.0079 per message
- International: $0.05-0.15 per message

**WhatsApp:**
- Business-initiated: $0.005 per message
- User-initiated: Free

**Estimated Usage:**
- Average agent run: 3 notifications (start, progress, complete)
- Cost per run: $0.024 (SMS) or $0.015 (WhatsApp)
- 100 runs/month: $2.40 (SMS) or $1.50 (WhatsApp)

### Telegram Pricing
- **FREE** - No costs regardless of volume

### Recommendation
- **Default to Telegram** for cost-free notifications
- Use SMS/WhatsApp for non-technical users
- Provide clear cost warnings in documentation

---

## 10. Testing Strategy

### Unit Tests

```python
# tests/test_messaging_telegram.py
import pytest
from unittest.mock import Mock, AsyncMock
from clud.messaging.telegram_client import TelegramClient

@pytest.mark.asyncio
async def test_send_message_success():
    client = TelegramClient("fake_token")
    client.bot.send_message = AsyncMock(return_value=True)
    
    result = await client.send_message("@testuser", "Hello")
    assert result is True

@pytest.mark.asyncio
async def test_send_code_block():
    client = TelegramClient("fake_token")
    client.bot.send_message = AsyncMock(return_value=True)
    
    result = await client.send_code_block("@testuser", "print('hi')", "python")
    assert result is True
    
    # Verify formatted correctly
    call_args = client.bot.send_message.call_args
    assert "```python" in call_args.kwargs["text"]
```

### Integration Tests

```python
# tests/integration/test_messaging_integration.py
import os
import pytest
from clud.messaging.factory import create_client

@pytest.mark.skipif(
    not os.getenv("TELEGRAM_BOT_TOKEN"),
    reason="TELEGRAM_BOT_TOKEN not set"
)
@pytest.mark.asyncio
async def test_telegram_real_send():
    config = {"telegram_token": os.getenv("TELEGRAM_BOT_TOKEN")}
    client = create_client("@testuser", config)
    
    result = await client.send_message(
        os.getenv("TEST_TELEGRAM_CHAT_ID"),
        "Test message from clud integration test"
    )
    assert result is True
```

### Manual Testing Checklist
- [ ] SMS to US number
- [ ] SMS to international number
- [ ] WhatsApp to verified number
- [ ] Telegram to @username
- [ ] Telegram to chat_id
- [ ] Invalid phone number (should fail gracefully)
- [ ] Invalid Telegram user (should fail gracefully)
- [ ] Network timeout (should retry)
- [ ] Rate limit exceeded (should queue)

---

## 11. Documentation Updates Required

### README.md Changes

Add new section:
```markdown
## Status Notifications

Get real-time updates on your agent's progress via Telegram, SMS, or WhatsApp:

```bash
# Telegram (free, recommended)
clud --notify-user "@yourusername" -m "Fix bug"

# SMS
clud --notify-user "+14155551234" -m "Deploy app"

# WhatsApp
clud --notify-user "whatsapp:+14155551234" -m "Run tests"
```

### Setup Instructions
See [MESSAGING_SETUP.md](MESSAGING_SETUP.md) for configuration guide.

### Configuration
```bash
# One-time setup
clud --configure-messaging

# Or use environment variables
export TELEGRAM_BOT_TOKEN="..."
export TWILIO_ACCOUNT_SID="..."
export TWILIO_AUTH_TOKEN="..."
export TWILIO_FROM_NUMBER="..."
```
```

### New File: MESSAGING_SETUP.md

Create comprehensive setup guide with:
- Screenshots for Telegram bot creation
- Twilio account setup walkthrough
- WhatsApp sandbox setup
- Troubleshooting common issues
- FAQ section

---

## 12. Future Enhancements (Post-MVP)

### Phase 5: Advanced Features
1. **Bidirectional Communication**
   - Receive user commands mid-execution
   - Pause/resume agent via message
   - Request approval for dangerous operations

2. **Rich Media Support**
   - Send screenshots of errors
   - Attach log files to completion messages
   - Share git diff as file

3. **Multi-User Notifications**
   - Notify entire team channel
   - Telegram group/channel support
   - Slack integration

4. **Smart Notifications**
   - ML-based priority detection
   - Only notify on important events
   - Customizable notification templates

5. **Analytics Dashboard**
   - Track notification delivery rates
   - Monitor API costs
   - View message history

---

## 13. Dependencies Required

### pyproject.toml Updates

```toml
dependencies = [
    # ... existing dependencies ...
    "python-telegram-bot>=21.0.0",  # Telegram Bot API
    "twilio>=9.0.0",                # SMS/WhatsApp
    "aiohttp>=3.9.0",               # Async HTTP (for Telegram)
]

[project.optional-dependencies]
messaging = [
    "python-telegram-bot>=21.0.0",
    "twilio>=9.0.0",
]
```

### Installation

```bash
# Full install
pip install clud[messaging]

# Or specific channels
pip install python-telegram-bot  # Telegram only
pip install twilio                # SMS/WhatsApp only
```

---

## 14. Risk Assessment & Mitigation

### Risks

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| API rate limits exceeded | Medium | Low | Implement queuing + backoff |
| Credentials leaked | Low | High | Secure storage + documentation |
| Network failures | High | Low | Retry logic + graceful degradation |
| User notification fatigue | Medium | Medium | Configurable interval + quiet mode |
| API cost overruns | Low | Medium | Cost warnings + usage tracking |
| Breaking changes in APIs | Low | Medium | Pin dependency versions + monitoring |

### Mitigation Strategies

1. **Rate Limiting**
   ```python
   from asyncio import sleep
   
   async def send_with_backoff(client, message, max_retries=3):
       for attempt in range(max_retries):
           try:
               return await client.send_message(message)
           except RateLimitError:
               await sleep(2 ** attempt)  # Exponential backoff
       return False
   ```

2. **Cost Protection**
   ```python
   class CostTracker:
       def __init__(self, max_monthly_cost=10.00):
           self.max_cost = max_monthly_cost
           self.current_cost = self.load_monthly_usage()
       
       def can_send(self, channel: str) -> bool:
           estimated_cost = self.estimate_cost(channel)
           return self.current_cost + estimated_cost < self.max_cost
   ```

3. **Graceful Degradation**
   ```python
   try:
       await notifier.notify_progress(status)
   except Exception as e:
       logger.warning(f"Notification failed: {e}")
       # Continue execution - don't block on notification failure
   ```

---

## 15. Success Metrics

### Key Performance Indicators (KPIs)

1. **Reliability**
   - Target: 99% notification delivery rate
   - Measure: Track sent/delivered ratio

2. **Latency**
   - Target: < 2 seconds from event to notification
   - Measure: Time from `notify()` call to API response

3. **User Adoption**
   - Target: 25% of users enable notifications within 3 months
   - Measure: Track `--notify-user` usage in telemetry

4. **Cost Efficiency**
   - Target: < $5/user/month average
   - Measure: Aggregate API costs across all users

5. **Error Rate**
   - Target: < 1% notification failures
   - Measure: Track exceptions in notification codepath

### Monitoring & Logging

```python
import logging

logger = logging.getLogger("clud.messaging")

# Log all notification attempts
logger.info("Notification sent", extra={
    "channel": "telegram",
    "contact": "@user",
    "message_type": "progress",
    "success": True,
    "latency_ms": 1234
})
```

---

## 16. Rollout Plan

### Week 1: Alpha Release
- **Audience:** Internal testing only
- **Features:** Basic Telegram support
- **Feedback:** Manual testing + bug fixes

### Week 2-3: Beta Release
- **Audience:** Early adopters (opt-in)
- **Features:** Telegram + SMS
- **Feedback:** User surveys + analytics

### Week 4: General Availability
- **Audience:** All users
- **Features:** Full MVP (Telegram, SMS, WhatsApp)
- **Documentation:** Complete setup guides

### Post-Launch: Iteration
- Monitor usage metrics
- Gather user feedback
- Prioritize enhancements
- Fix bugs reported in issues

---

## 17. Conclusion

This proposal provides a comprehensive plan for integrating multi-channel notifications into `clud`, enabling users to stay informed about their agent's progress via Telegram, SMS, and WhatsApp.

### Key Takeaways:

1. **Recommended Flag:** `--notify-user <contact>`
   - Most intuitive and flexible
   - Supports all contact formats

2. **Recommended APIs:**
   - **Telegram Bot API** for developers (free, feature-rich)
   - **Twilio** for SMS/WhatsApp (universal, reliable)

3. **Integration Points:**
   - Minimal changes to existing codebase
   - Non-blocking async notifications
   - Graceful degradation on failure

4. **Timeline:** 4 weeks to MVP
   - Week 1: Core messaging infrastructure
   - Week 2: Agent integration
   - Week 3: Enhanced features
   - Week 4: Testing & documentation

5. **Costs:**
   - Telegram: FREE
   - SMS: ~$0.024/run
   - WhatsApp: ~$0.015/run

### Next Steps:

1. Review and approve this proposal
2. Set up Telegram bot for testing
3. Create Twilio sandbox account
4. Begin Phase 1 implementation
5. Schedule weekly progress reviews

---

## Appendix A: API Documentation Links

- **Telegram Bot API:** https://core.telegram.org/bots/api
- **python-telegram-bot:** https://python-telegram-bot.org/
- **Twilio SMS API:** https://www.twilio.com/docs/sms
- **Twilio WhatsApp API:** https://www.twilio.com/docs/whatsapp
- **Twilio Python SDK:** https://www.twilio.com/docs/libraries/python

## Appendix B: Example Message Templates

### Start Notification
```
🤖 **Clud Agent Starting**

Task: {task_description}
Time: {timestamp}

I'll keep you updated on progress!
```

### Progress Update
```
⏳ **Working** ({elapsed_seconds}s)

{status_message}

Updates every {interval}s
```

### Completion (Success)
```
✅ **Completed Successfully** ({duration}s)

{summary}

Files modified: {file_count}
Lines changed: +{added}/-{removed}
```

### Completion (Failure)
```
❌ **Failed** ({duration}s)

{error_message}

Check logs for details:
{log_path}
```

### Error Notification
```
⚠️ **Error Encountered**

{error_type}: {error_message}

```
{stack_trace}
```

Agent continuing...
```

---

**END OF PROPOSAL**

*Total Word Count: ~6,800 words*
*Total Code Examples: 25+*
*Implementation Estimate: 4 weeks (1 developer)*
