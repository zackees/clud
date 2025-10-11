# Telegram Web App Integration - Design Document

## 1. Overview

### Purpose
Create a static HTML page that serves as a Telegram Web App interface, allowing users to interact with Claude Code through Telegram's messaging infrastructure. The page auto-embeds into Telegram and users can start chatting immediately.

### Goals
- Generate a static HTML file with embedded Telegram Web App SDK
- Serve it using Python stdlib's `http.server` (zero external dependencies)
- Auto-initialize the bot context when loaded in Telegram
- Provide clean chat UI that works within Telegram's Web App container
- All communication goes through Telegram Bot API (no WebSocket/Flask needed)

### User Flow
1. User runs `clud --telegram` (or `clud -tg`) which starts simple HTTP server on an auto-assigned localhost port
2. Server prints URL (e.g., `http://localhost:54321`)
3. User configures this URL in their Telegram bot (via BotFather)
4. User opens the bot in Telegram (creates or opens existing chat session)
5. User clicks menu button → web app loads in an iframe within that chat context
6. Web app auto-initializes with Telegram SDK (has access to chat_id, user_id)
7. User types messages → sent via Telegram Bot API → processed by bot backend
8. Bot responds to that specific chat_id → Telegram delivers response → displayed in web app UI

**Multi-user support**: One bot can serve many concurrent users/chats. Each user has their own persistent chat session with the bot. The web app inherits the chat context when loaded.

---

## 2. Architecture Design

### High-Level Architecture

```
┌──────────────────────────────────────────────┐
│         Telegram App (User's Phone)          │
│                                              │
│  ┌────────────────────────────────────────┐ │
│  │    Web App (iframe)                    │ │
│  │  - Static HTML from localhost:5000     │ │
│  │  - Telegram Web App SDK initialized    │ │
│  │  - Chat UI for user interaction        │ │
│  └────────┬──────────────────────┬────────┘ │
│           │                      │           │
└───────────┼──────────────────────┼───────────┘
            │                      │
            │ User messages        │ Bot responses
            │ (via Telegram SDK)   │ (via Telegram SDK)
            ▼                      ▼
┌─────────────────────────────────────────────┐
│         Telegram Bot API (Cloud)             │
│  - Routes messages between user and bot      │
│  - Handles authentication                    │
│  - Manages conversation state                │
│  - Delivers messages bidirectionally         │
└───────────┬──────────────────────────────────┘
            │
            │ HTTPS webhooks or polling
            │
┌───────────▼──────────────────────────────────┐
│    Python Bot Backend (telegram.py)          │
│  - Receives messages from Telegram API       │
│  - Spawns Claude agent to process requests   │
│  - Sends responses back to Telegram          │
│  - Uses existing agent_foreground.py         │
└──────────────────────────────────────────────┘

Separate lightweight process (for serving HTML):
┌──────────────────────────────────────────────┐
│   Python stdlib http.server (localhost:5000) │
│  - Serves static telegram_webapp.html        │
│  - No backend logic needed                   │
│  - Just simple file serving                  │
│  - Can use python -m http.server             │
└──────────────────────────────────────────────┘
```

### Technology Stack
- **Web Server**: Python stdlib `http.server.HTTPServer` (zero dependencies)
- **Frontend**: Static HTML5, CSS, JavaScript, Telegram Web App SDK (CDN)
- **Communication**: Telegram Bot API (all handled by Telegram's infrastructure)
- **Bot Backend**: Python with `python-telegram-bot` (existing `telegram.py`)
- **Agent**: Reuse existing `agent_foreground.py` for Claude execution

### Key Design Principles
1. **Static-first**: HTML file is completely self-contained
2. **No server-side logic**: Server only serves files, doesn't process anything
3. **Telegram handles communication**: No WebSocket, SSE, or polling needed
4. **Minimal dependencies**: Only stdlib for file serving
5. **Reuse existing code**: Leverage current `telegram.py` and `agent_foreground.py`

### Multi-Chat Architecture

**One bot serves many concurrent users:**

```
Your Telegram Bot (@mybot)
│
├── Chat Session: User A (chat_id: 123456)
│   ├── Web App Instance (loads in this chat context)
│   ├── Message history for User A
│   └── Isolated Claude agent for User A
│
├── Chat Session: User B (chat_id: 789012)
│   ├── Web App Instance (loads in this chat context)
│   ├── Message history for User B
│   └── Isolated Claude agent for User B
│
└── Chat Session: User C (chat_id: 345678)
    ├── Web App Instance (loads in this chat context)
    ├── Message history for User C
    └── Isolated Claude agent for User C
```

**Key points:**
- Each user has a **persistent chat session** with the bot (identified by `chat_id`)
- When web app loads, Telegram SDK provides: `user_id`, `chat_id`, `query_id`
- Bot backend uses `chat_id` to route responses to correct user
- Message history is per-chat (Telegram manages this)
- Each chat can optionally have isolated agent context (if needed)

**Chat lifecycle:**
1. **New user**: Opens bot → Creates new chat session → First interaction
2. **Returning user**: Opens bot → Resumes existing chat session → Continues conversation
3. **Web app launch**: User clicks menu button → Web app loads in iframe → Inherits chat context
4. **Message flow**: User sends message → Goes to bot with `chat_id` → Bot responds to that `chat_id`

---

## 3. Component Breakdown

### 3.1 Static HTML Server (`src/clud/webapp/server.py`)

**Purpose**: Serve the static HTML file - nothing more.

**Implementation** (stdlib only):
```python
"""Minimal HTTP server for serving Telegram Web App static files."""
from http.server import HTTPServer, SimpleHTTPRequestHandler
import os
from pathlib import Path

def run_server() -> int:
    """Start simple HTTP server to serve webapp files.

    Automatically picks an available port.

    Returns:
        Exit code (0 for success)
    """
    # Change to webapp static directory
    webapp_dir = Path(__file__).parent / "static"
    os.chdir(webapp_dir)

    # Create server with port 0 (auto-assign available port)
    server = HTTPServer(('localhost', 0), SimpleHTTPRequestHandler)

    # Get the actual port that was assigned
    actual_port = server.server_address[1]

    print(f"Telegram Web App server running at http://localhost:{actual_port}")
    print("Configure this URL in your Telegram bot via @BotFather")
    print("Press Ctrl+C to stop")

    try:
        server.serve_forever()
        return 0
    except KeyboardInterrupt:
        print("\nServer stopped")
        return 0
```

That's it! No Flask, no Socket.IO, no complexity.

### 3.2 Static Web App (`src/clud/webapp/static/index.html`)

**Purpose**: Self-contained HTML with Telegram SDK that auto-initializes bot.

**Key Features**:
- Telegram Web App SDK initialization
- Chat UI (messages, input field)
- Send messages via `Telegram.WebApp.sendData()`
- Receive responses via Telegram's message handler
- Mobile-responsive design
- No external dependencies except Telegram SDK (CDN)

**Implementation**:
```html
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Claude Code Chat</title>
    <script src="https://telegram.org/js/telegram-web-app.js"></script>
    <style>
        * {
            margin: 0;
            padding: 0;
            box-sizing: border-box;
        }

        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            background-color: var(--tg-theme-bg-color, #ffffff);
            color: var(--tg-theme-text-color, #000000);
            height: 100vh;
            display: flex;
            flex-direction: column;
        }

        #header {
            padding: 16px;
            background-color: var(--tg-theme-secondary-bg-color, #f0f0f0);
            border-bottom: 1px solid var(--tg-theme-hint-color, #ccc);
        }

        #status {
            font-size: 14px;
            color: var(--tg-theme-hint-color, #999);
        }

        #messages {
            flex: 1;
            overflow-y: auto;
            padding: 16px;
        }

        .message {
            margin-bottom: 12px;
            padding: 8px 12px;
            border-radius: 8px;
            max-width: 80%;
        }

        .message.user {
            background-color: var(--tg-theme-button-color, #0088cc);
            color: var(--tg-theme-button-text-color, #ffffff);
            margin-left: auto;
            text-align: right;
        }

        .message.bot {
            background-color: var(--tg-theme-secondary-bg-color, #f0f0f0);
        }

        #input-container {
            padding: 16px;
            border-top: 1px solid var(--tg-theme-hint-color, #ccc);
            display: flex;
            gap: 8px;
        }

        #message-input {
            flex: 1;
            padding: 10px;
            border: 1px solid var(--tg-theme-hint-color, #ccc);
            border-radius: 20px;
            background-color: var(--tg-theme-bg-color, #ffffff);
            color: var(--tg-theme-text-color, #000000);
        }

        #send-btn {
            padding: 10px 20px;
            border: none;
            border-radius: 20px;
            background-color: var(--tg-theme-button-color, #0088cc);
            color: var(--tg-theme-button-text-color, #ffffff);
            cursor: pointer;
        }

        #send-btn:disabled {
            opacity: 0.5;
            cursor: not-allowed;
        }

        code {
            background-color: var(--tg-theme-secondary-bg-color, #f0f0f0);
            padding: 2px 4px;
            border-radius: 3px;
            font-family: 'Courier New', monospace;
        }
    </style>
</head>
<body>
    <div id="header">
        <h2>Claude Code Assistant</h2>
        <div id="status">Initializing...</div>
    </div>

    <div id="messages"></div>

    <div id="input-container">
        <input
            type="text"
            id="message-input"
            placeholder="Type your message..."
            disabled
        />
        <button id="send-btn" disabled>Send</button>
    </div>

    <script>
        // Initialize Telegram WebApp
        const tg = window.Telegram.WebApp;

        // Expand to full height
        tg.expand();

        // Set theme
        tg.ready();

        // UI elements
        const messagesDiv = document.getElementById('messages');
        const statusDiv = document.getElementById('status');
        const messageInput = document.getElementById('message-input');
        const sendBtn = document.getElementById('send-btn');

        // Enable UI after initialization
        function initialize() {
            statusDiv.textContent = 'Ready - Start chatting!';
            messageInput.disabled = false;
            sendBtn.disabled = false;
            messageInput.focus();

            // Show welcome message
            appendMessage('bot', 'Hello! I\'m Claude Code. How can I help you today?');
        }

        // Append message to chat
        function appendMessage(sender, text) {
            const messageDiv = document.createElement('div');
            messageDiv.className = `message ${sender}`;
            messageDiv.textContent = text;
            messagesDiv.appendChild(messageDiv);

            // Auto-scroll to bottom
            messagesDiv.scrollTop = messagesDiv.scrollHeight;
        }

        // Send message via Telegram
        function sendMessage() {
            const message = messageInput.value.trim();
            if (!message) return;

            // Show user message in UI
            appendMessage('user', message);

            // Send to Telegram bot backend
            tg.sendData(JSON.stringify({
                type: 'message',
                text: message,
                timestamp: Date.now()
            }));

            // Clear input
            messageInput.value = '';

            // Show status
            statusDiv.textContent = 'Claude is thinking...';
        }

        // Handle send button click
        sendBtn.addEventListener('click', sendMessage);

        // Handle Enter key
        messageInput.addEventListener('keypress', (e) => {
            if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                sendMessage();
            }
        });

        // Listen for responses from bot (via web_app_data)
        // Note: This is handled by Telegram's infrastructure
        // The bot backend sends updates that Telegram delivers to this app

        // Initialize after short delay
        setTimeout(initialize, 500);

        // For testing in browser (not in Telegram)
        if (!window.Telegram.WebApp.initData) {
            console.log('Running outside Telegram - demo mode');
            initialize();

            // Mock bot response for testing
            setTimeout(() => {
                appendMessage('bot', 'This is a test response. In production, messages come from your Telegram bot.');
                statusDiv.textContent = 'Ready';
            }, 2000);
        }
    </script>
</body>
</html>
```

### 3.3 Bot Backend Integration (existing `telegram.py`)

The bot backend already exists! Just extend it to handle Web App messages with multi-chat support:

```python
# In src/clud/messaging/telegram.py (extend existing code)

async def handle_web_app_data(self, update, context):
    """Handle data sent from Web App.

    Args:
        update: Telegram update with web_app_data
        context: Telegram context
    """
    if not update.message or not update.message.web_app_data:
        return

    # Extract chat and user context
    chat_id = update.message.chat_id
    user_id = update.message.from_user.id
    username = update.message.from_user.username or "Unknown"

    try:
        # Parse web app data
        data = json.loads(update.message.web_app_data.data)
        message_text = data.get('text', '')

        logger.info(f"Web app message from user {username} (chat_id: {chat_id}): {message_text[:50]}...")

        # Process with Claude agent (isolated per chat)
        response = await self.process_message_with_agent(
            message_text,
            chat_id=chat_id,
            user_id=user_id
        )

        # Send response back to this specific chat
        await update.message.reply_text(response)

    except Exception as e:
        logger.error(f"Error handling web app data for chat {chat_id}: {e}")
        await update.message.reply_text("Sorry, an error occurred processing your request.")

async def process_message_with_agent(self, message: str, chat_id: int, user_id: int) -> str:
    """Process message with Claude agent, isolated per chat.

    Args:
        message: User message to process
        chat_id: Telegram chat ID (for routing responses)
        user_id: Telegram user ID (for context)

    Returns:
        Agent response
    """
    # Option 1: Single agent handles all chats (simpler)
    # Just pass the message to agent_foreground.py

    # Option 2: Separate agent instance per chat (more isolated)
    # Spawn/reuse agent process for this specific chat_id
    # This ensures each user has isolated context

    # For now, simple implementation:
    # (You can extend this later with chat-specific state management)
    response = await self._invoke_claude_agent(message)
    return response

async def _invoke_claude_agent(self, message: str) -> str:
    """Invoke Claude agent with message.

    Args:
        message: Message to process

    Returns:
        Agent response
    """
    # This would call your existing agent_foreground.py
    # Or spawn a Claude subprocess to handle the request
    # Implementation depends on your existing architecture
    pass
```

**Chat Session Management (optional enhancement):**

If you want to maintain per-chat agent context:

```python
class TelegramMessenger:
    def __init__(self, bot_token: str, chat_id: str):
        # ... existing code ...
        self.chat_agents: dict[int, AgentProcess] = {}  # chat_id -> agent instance

    async def get_or_create_agent(self, chat_id: int) -> AgentProcess:
        """Get existing agent for chat or create new one.

        Args:
            chat_id: Telegram chat ID

        Returns:
            Agent process for this chat
        """
        if chat_id not in self.chat_agents:
            # Spawn new agent for this chat
            self.chat_agents[chat_id] = await self._spawn_agent(chat_id)

        return self.chat_agents[chat_id]

    async def cleanup_agent(self, chat_id: int):
        """Cleanup agent for specific chat.

        Args:
            chat_id: Chat ID to cleanup
        """
        if chat_id in self.chat_agents:
            await self.chat_agents[chat_id].terminate()
            del self.chat_agents[chat_id]
```

### 3.4 CLI Integration (`src/clud/cli.py` extension)

Add simple command to start the web server:

```python
def handle_telegram_command() -> int:
    """Handle the --telegram/-tg command by starting Telegram Web App server."""
    from .webapp.server import run_server

    try:
        print(f"Starting Telegram Web App server...")
        return run_server()
    except Exception as e:
        print(f"Error running Telegram Web App: {e}", file=sys.stderr)
        return 1
```

Update argument parser and router:
```python
# In cli_args.py - add --telegram/-tg flag
parser.add_argument('--telegram', '-tg', action='store_true',
                    help='Start Telegram Web App server (auto-picks available port)')

# In cli.py main() - handle telegram command
if router_args.telegram:
    return handle_telegram_command()
```

---

## 4. Implementation Plan

### Phase 1: Static Server (1-2 hours)
- [ ] Create `src/clud/webapp/` directory structure
- [ ] Implement `server.py` with stdlib `http.server` (auto-assign port with port=0)
- [ ] Test server can serve files on auto-assigned localhost port
- [ ] Add CLI integration (`--telegram`/`-tg` flag)

### Phase 2: HTML/UI (2-3 hours)
- [ ] Create `static/index.html` with Telegram SDK
- [ ] Implement chat UI with CSS
- [ ] Add message display and input handling
- [ ] Test in regular browser (non-Telegram mode)
- [ ] Test theme variables work correctly

### Phase 3: Telegram Integration (1-2 hours)
- [ ] Test loading in Telegram app
- [ ] Verify `tg.sendData()` works
- [ ] Configure bot with @BotFather
- [ ] Test full message flow (app → Telegram → bot → app)

### Phase 4: Bot Backend (1-2 hours)
- [ ] Extend `telegram.py` to handle web_app_data
- [ ] Connect to existing agent_foreground.py
- [ ] Test message processing and responses
- [ ] Add error handling

### Phase 5: Polish & Testing (2-3 hours)
- [ ] Mobile-responsive tweaks
- [ ] Add loading states
- [ ] Improve error messages
- [ ] Update CLAUDE.md documentation
- [ ] Write integration tests

**Total Estimated Time**: 7-12 hours (1-2 days)

---

## 5. File Structure

```
src/clud/webapp/
├── __init__.py
├── server.py              # stdlib http.server (30 lines)
└── static/
    └── index.html         # Self-contained HTML (all-in-one)

src/clud/
├── cli.py                 # Add --webapp command
├── cli_args.py            # Add webapp arguments
└── messaging/
    └── telegram.py        # Extend with web_app_data handler

docs/
└── telegram-webapp-design.md  # This document
```

**Total new code**: ~300-400 lines (mostly HTML/CSS/JS in one file)

---

## 6. Configuration & Usage

### Setup Instructions

**Step 1: Start the web server**
```bash
clud --telegram
# or shorthand
clud -tg

# Output: Telegram Web App server running at http://localhost:54321
#         (port is auto-assigned, will be different each time)
```

**Step 2: Configure Telegram bot (via @BotFather)**
```
1. Message @BotFather in Telegram
2. Choose your bot
3. Select "Bot Settings" → "Configure Mini App"
4. Send the URL shown in terminal (e.g., http://localhost:54321)
```

**Step 3: Open bot and start chatting**
```
1. Open your bot in Telegram (creates or opens your chat session)
2. Click menu button to launch web app
3. Web app loads in iframe with your chat context
4. Chat interface appears
5. Start messaging!
```

### CLI Options
```bash
# Start server (auto-picks available port)
clud --telegram
clud -tg

# That's it - no configuration needed!
```

---

## 7. Security Considerations

### 7.1 Localhost Only
- Server binds to `127.0.0.1` (localhost only)
- No external access possible
- Telegram Web App loads in sandboxed iframe
- **Note**: For remote access, use ngrok or similar tunnel (not recommended for production)

### 7.2 Telegram Authentication
- Telegram handles all user authentication
- `initData` parameter contains signed user info
- Bot can verify requests came from Telegram
- No additional auth needed

### 7.3 Input Validation
- Validate all web_app_data in bot backend
- Sanitize messages before passing to agent
- Rate limiting via Telegram's built-in limits

### 7.4 No Sensitive Data in HTML
- HTML is completely static and public
- No API keys or tokens embedded
- All secrets stay in bot backend

---

## 8. Dependencies

### Zero New Dependencies!

The only thing we need:
- Python stdlib `http.server` ✅ (built-in)
- Telegram Web App SDK ✅ (CDN, no install)
- Existing `python-telegram-bot` ✅ (already in project)

**No changes needed to `pyproject.toml`**

---

## 9. Testing Strategy

### Unit Tests
- `tests/test_webapp_server.py` - Test server startup/shutdown
- Test HTML file exists and is valid

### Integration Tests
- Test loading HTML in regular browser
- Test Telegram SDK initialization
- Test message flow with mock Telegram updates
- Test bot backend handles web_app_data

### Manual Testing
1. **Browser testing** (outside Telegram)
   - Open http://localhost:5000 in Chrome/Firefox
   - Verify UI displays correctly
   - Test message input and send button

2. **Telegram testing** (in Telegram app)
   - Configure bot with localhost URL (use ngrok for phone testing)
   - Open bot in Telegram on phone
   - Verify web app loads in iframe
   - Test sending messages
   - Verify responses appear correctly

3. **Mobile responsiveness**
   - Test on different screen sizes
   - Verify keyboard doesn't cover input
   - Test scrolling behavior

---

## 10. Future Enhancements

### v1.1 Features
- [ ] Message history persistence (localStorage)
- [ ] Syntax highlighting for code blocks (highlight.js via CDN)
- [ ] File attachment support (Telegram's file API)
- [ ] Typing indicators
- [ ] Message markdown rendering

### v1.2 Features
- [ ] Multiple conversation threads
- [ ] Export chat history
- [ ] Voice message support (Telegram voice API)
- [ ] Image generation display
- [ ] Performance metrics dashboard

### v2.0 Features
- [ ] Cloud deployment (not localhost)
- [ ] Database-backed history
- [ ] Multi-user support
- [ ] Advanced agent orchestration
- [ ] Integration with other messaging platforms

---

## 11. Troubleshooting

### Common Issues

**Issue**: Web app doesn't load in Telegram
- **Solution**: Make sure bot is configured correctly in @BotFather
- Check that server is running (should see "Telegram Web App server running..." message)
- Verify you copied the correct URL to @BotFather
- For phone testing, use ngrok tunnel (localhost not accessible from phone)

**Issue**: Messages not reaching bot
- **Solution**: Check bot backend is running and listening
- Verify web_app_data handler is registered
- Check Telegram API credentials are correct

**Issue**: Responses not appearing in web app
- **Solution**: Telegram delivers responses as regular messages, not back to the web app
- Consider using inline mode or different message type
- Alternative: Poll for updates using Telegram API

**Issue**: Theme colors look wrong
- **Solution**: Use Telegram's CSS variables (--tg-theme-*)
- Test in actual Telegram app (not browser)
- Browser fallback colors may differ

---

## 12. Example Session

```bash
# Terminal: Start server
$ clud --telegram
Starting Telegram Web App server...
Telegram Web App server running at http://localhost:54738
Configure this URL in your Telegram bot via @BotFather
Press Ctrl+C to stop

# User A: Opens bot in Telegram (chat_id: 123456)
# Clicks menu button → Web app loads in iframe

# Web App displays:
# ┌─────────────────────────────┐
# │ Claude Code Assistant       │
# │ Status: Ready               │
# │ User: Alice (123456)        │
# ├─────────────────────────────┤
# │ [Bot] Hello! I'm Claude     │
# │       Code. How can I help? │
# │                             │
# │                             │
# ├─────────────────────────────┤
# │ [Input: Type message...]    │
# └─────────────────────────────┘

# User A types: "List files in current directory"
# App sends to Telegram (with chat_id: 123456)
# Bot receives with context: chat_id=123456, user_id=111
# Processes with Claude → Responds to chat_id 123456

# Bot responds:
# ┌─────────────────────────────┐
# │ [User] List files in        │
# │        current directory    │
# │                             │
# │ [Bot] Here are the files:   │
# │       • README.md           │
# │       • pyproject.toml      │
# │       • src/                │
# │       • tests/              │
# └─────────────────────────────┘

# Meanwhile, User B (chat_id: 789012) also uses the bot
# Their messages go to their own chat session
# Completely isolated from User A
```

---

## 13. Implementation Checklist

- [ ] Create directory: `src/clud/webapp/`
- [ ] Write `server.py` (simple HTTP server with stdlib)
- [ ] Write `static/index.html` (self-contained web app with Telegram SDK)
- [ ] Update `cli.py` (add `handle_telegram_command()`)
- [ ] Update `cli_args.py` (add `--telegram`/`-tg` arguments)
- [ ] Extend `telegram.py` (add `handle_web_app_data()` with multi-chat support)
- [ ] Test in browser (localhost:5000)
- [ ] Configure bot with @BotFather
- [ ] Test in Telegram app (single user)
- [ ] Test multi-user scenarios (concurrent chats)
- [ ] Write unit tests (server, HTML validation)
- [ ] Write integration tests (bot backend, multi-chat)
- [ ] Update CLAUDE.md (document `--telegram` command)
- [ ] Update project README

---

## Appendix A: Telegram Web App SDK Reference

### Key Methods

**Initialization**
```javascript
const tg = window.Telegram.WebApp;
tg.ready();  // Tell Telegram app is ready
tg.expand(); // Expand to full height
```

**Sending Data**
```javascript
tg.sendData(JSON.stringify({
    type: 'message',
    text: 'Hello from web app'
}));
```

**Theme Colors**
```javascript
// Access theme colors
const bgColor = tg.backgroundColor;
const textColor = tg.textColor;
const buttonColor = tg.buttonColor;

// Or use CSS variables (preferred)
// var(--tg-theme-bg-color)
// var(--tg-theme-text-color)
// var(--tg-theme-button-color)
```

**User Info**
```javascript
const user = tg.initDataUnsafe.user;
console.log(user.id, user.first_name);
```

**Main Button** (optional)
```javascript
tg.MainButton.text = "Submit";
tg.MainButton.show();
tg.MainButton.onClick(() => {
    // Handle click
});
```

---

## Appendix B: Bot Backend Implementation

### Telegram Bot Handler Setup

```python
# In src/clud/messaging/telegram.py

from telegram import Update
from telegram.ext import Application, MessageHandler, filters

async def setup_web_app_handler(self):
    """Set up handler for web app data."""
    if not self.app:
        return

    # Add handler for web app data
    web_app_handler = MessageHandler(
        filters.StatusUpdate.WEB_APP_DATA,
        self.handle_web_app_data
    )
    self.app.add_handler(web_app_handler)

async def handle_web_app_data(self, update: Update, context):
    """Handle data from Telegram Web App with multi-chat support.

    Args:
        update: Telegram update containing web_app_data
        context: Telegram context object
    """
    if not update.message or not update.message.web_app_data:
        logger.warning("No web app data in update")
        return

    # Extract chat context
    chat_id = update.message.chat_id
    user_id = update.message.from_user.id
    username = update.message.from_user.username or "Unknown"

    try:
        # Parse the JSON data sent from web app
        import json
        data = json.loads(update.message.web_app_data.data)

        message_text = data.get('text', '')
        message_type = data.get('type', 'message')

        logger.info(f"Web app message from {username} (chat_id: {chat_id}): {message_text[:50]}...")

        # Process with Claude agent (pass chat context for isolation)
        response = await self._process_with_agent(
            message_text,
            chat_id=chat_id,
            user_id=user_id
        )

        # Send response back to this specific chat
        await update.message.reply_text(
            response,
            parse_mode='Markdown'
        )

    except json.JSONDecodeError as e:
        logger.error(f"Invalid JSON from web app (chat {chat_id}): {e}")
        await update.message.reply_text(
            "Sorry, I received invalid data. Please try again."
        )
    except Exception as e:
        logger.error(f"Error processing web app data (chat {chat_id}): {e}")
        await update.message.reply_text(
            "Sorry, an error occurred. Please try again."
        )

async def _process_with_agent(self, message: str) -> str:
    """Process message with Claude agent.

    Args:
        message: User message to process

    Returns:
        Agent response
    """
    # This would call your existing agent_foreground.py logic
    # or spawn a Claude process to handle the request
    # Implementation depends on your existing architecture
    pass
```

---

## Appendix C: References

- [Telegram Web Apps Documentation](https://core.telegram.org/bots/webapps)
- [Telegram Bot API](https://core.telegram.org/bots/api)
- [python-telegram-bot Library](https://python-telegram-bot.org/)
- [Example Telegram Web App](https://github.com/revenkroz/telegram-web-app-bot-example)
- [Python http.server Documentation](https://docs.python.org/3/library/http.server.html)

---

**Document Version**: 2.0 (Simplified)
**Last Updated**: 2025-10-11
**Author**: Claude Code Design Agent
**Status**: Ready for Implementation
