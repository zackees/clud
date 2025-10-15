# Advanced Telegram Integration Guide

## Overview

The Advanced Telegram Integration for `clud` provides a unified interface for interacting with Claude Code through both Telegram and a web dashboard. Messages sent to your Telegram bot are automatically synchronized with a real-time web interface, allowing you to monitor and interact with your bot from anywhere.

## Features

- **Real-time Synchronization**: Messages sent to the Telegram bot appear instantly in the web dashboard
- **Multi-Session Support**: Handle multiple concurrent Telegram users, each with their own isolated session
- **Session Persistence**: Conversation history is maintained across web client reconnections
- **WebSocket Streaming**: Real-time message updates with minimal latency
- **SvelteKit Frontend**: Modern, responsive web interface with dark/light theme support
- **Session Management**: View all active sessions, switch between users, and monitor activity
- **Instance Pooling**: Efficient resource management with automatic instance lifecycle management

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Telegram Platform                         │
│                 (t.me/clud_ckl_bot)                         │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
            ┌────────────────────────┐
            │ TelegramBotHandler     │
            │ (python-telegram-bot)  │
            └───────────┬────────────┘
                        │
                        ▼
┌───────────────────────────────────────────────────────────┐
│                   SessionManager                           │
│  - Maps Telegram user_id → session_id                     │
│  - Manages message history per session                    │
│  - Broadcasts events to web clients                       │
└──────────┬────────────────────────────────┬──────────────┘
           │                                 │
           ▼                                 ▼
  ┌────────────────┐              ┌──────────────────┐
  │ InstancePool   │              │  WebSocket Server│
  │ (clud agents)  │              │  (FastAPI)       │
  └────────────────┘              └────────┬─────────┘
                                           │
                                           ▼
                              ┌─────────────────────────┐
                              │   Web Client (Browser)  │
                              │   - SvelteKit UI        │
                              │   - Real-time updates   │
                              │   - Session selector    │
                              └─────────────────────────┘
```

## Getting Started

### Prerequisites

1. **Python 3.13+**: Ensure you have Python 3.13 or later installed
2. **Telegram Bot Token**: Create a bot through [@BotFather](https://t.me/BotFather) on Telegram
3. **Dependencies**: Install clud with all dependencies:
   ```bash
   uv pip install -e ".[dev]"
   ```

### Quick Start

1. **Set your Telegram bot token**:
   ```bash
   export TELEGRAM_BOT_TOKEN="your_bot_token_here"
   ```

2. **Start the server**:
   ```bash
   clud --telegram-server
   ```

3. **Open the web dashboard**:
   - The browser should automatically open to `http://127.0.0.1:8889`
   - If not, manually navigate to that URL

4. **Start chatting**:
   - Send a message to your Telegram bot
   - Watch it appear in real-time on the web dashboard
   - The bot's responses will be sent to both Telegram and the web interface

## Configuration

### Environment Variables

The simplest way to configure the Telegram integration is through environment variables:

```bash
# Required: Telegram Bot Token
export TELEGRAM_BOT_TOKEN="123456789:ABCdefGHIjklMNOpqrsTUVwxyz"

# Optional: Web Interface Configuration
export TELEGRAM_WEB_PORT=8889
export TELEGRAM_WEB_AUTH="my_secret_token"  # Enable authentication

# Optional: Session Configuration
export TELEGRAM_SESSION_TIMEOUT=3600        # Session timeout in seconds (default: 1 hour)
export TELEGRAM_MAX_SESSIONS=50            # Maximum concurrent sessions
export TELEGRAM_MESSAGE_HISTORY_LIMIT=1000  # Maximum messages per session

# Optional: Telegram Bot Configuration
export TELEGRAM_ALLOWED_USERS="123456,789012"  # Comma-separated user IDs (empty = allow all)
```

### Configuration File

For more complex setups, create a `telegram_config.yaml` file:

```yaml
telegram:
  bot_token: ${TELEGRAM_BOT_TOKEN}  # Can reference environment variables
  webhook_url: null                  # Use polling by default
  allowed_users: []                  # Empty list = allow all users

web:
  port: 8889
  host: "127.0.0.1"
  auth_required: false
  bidirectional: false  # Allow web-initiated messages

sessions:
  timeout_seconds: 3600
  max_sessions: 50
  message_history_limit: 1000
  cleanup_interval: 300  # Cleanup check interval (5 minutes)

logging:
  level: INFO
  file: "logs/telegram.log"
```

Then start the server with the config file:
```bash
clud --telegram-server --telegram-config telegram_config.yaml
```

## CLI Commands

### Basic Usage

```bash
# Start with default settings (port 8889)
clud --telegram-server

# Short form
clud --tg-server
```

### Custom Port

```bash
# Start on port 9000
clud --telegram-server 9000
```

### With Configuration File

```bash
# Use custom config file
clud --telegram-server --telegram-config my_config.yaml

# Combine port and config
clud --telegram-server 9000 --telegram-config my_config.yaml
```

### Help

```bash
# Show all telegram server options
clud --help | grep telegram
```

## Web Interface Guide

### Session List (Left Sidebar)

The session list shows all active Telegram users:

- **Green indicator (●)**: Active session (recent activity)
- **Gray indicator (○)**: Idle session (no recent activity)
- **Username**: Telegram username (e.g., @johndoe)
- **Full Name**: User's first and last name from Telegram
- **Last Activity**: Time since last message (e.g., "2 min ago")

Click on a session to view the conversation.

### Chat View (Main Panel)

The chat view displays the conversation with the selected user:

- **User Messages**: Messages sent by the Telegram user
- **Bot Messages**: Responses from Claude Code
- **Timestamps**: Time each message was sent
- **Typing Indicator**: Shows when the bot is processing a response
- **Auto-scroll**: Automatically scrolls to the latest message

### Features

- **Theme Toggle**: Switch between dark and light modes
- **Connection Status**: Banner shows WebSocket connection status
- **Message History**: Full conversation history is preserved
- **Real-time Updates**: Messages appear instantly as they're sent

## Data Flow

### User Sends Message on Telegram

1. User sends message to bot on Telegram
2. Telegram Bot Handler receives the update
3. SessionManager retrieves or creates a session for the user
4. Message is added to session history
5. SessionManager routes message to InstancePool
6. Claude Code (CludInstance) processes the message
7. Response is streamed back in chunks
8. SessionManager broadcasts each chunk to:
   - Telegram Bot Handler → sends to Telegram user
   - All connected web clients via WebSocket
9. Web interface updates in real-time

### Web Client Connects

1. User opens web interface in browser
2. Frontend fetches list of active sessions via REST API
3. User selects a session to view
4. Frontend establishes WebSocket connection
5. SessionManager validates the connection
6. Full message history is sent to the client (replay)
7. Client renders the conversation
8. Real-time updates flow via WebSocket

## API Reference

### REST API Endpoints

#### Get All Sessions

```http
GET /api/telegram/sessions
```

Returns list of all active sessions with metadata.

**Response**:
```json
{
  "sessions": [
    {
      "session_id": "uuid",
      "telegram_user_id": 123456,
      "telegram_username": "johndoe",
      "telegram_first_name": "John",
      "telegram_last_name": "Doe",
      "instance_id": "instance-uuid",
      "created_at": "2025-10-14T10:30:00Z",
      "last_activity": "2025-10-14T10:35:00Z",
      "is_active": true,
      "web_client_count": 2,
      "message_count": 42
    }
  ]
}
```

#### Get Session Details

```http
GET /api/telegram/sessions/{session_id}
```

Returns detailed session information including full message history.

#### Delete Session

```http
DELETE /api/telegram/sessions/{session_id}
```

Terminates a session and cleans up resources.

#### Health Check

```http
GET /api/health
```

Returns server health status.

### WebSocket Protocol

#### Connect to Session

```javascript
ws://127.0.0.1:8889/ws/telegram/{session_id}
```

#### Client → Server Messages

**Subscribe to Session**:
```json
{
  "type": "subscribe",
  "session_id": "uuid",
  "auth_token": "optional_token"
}
```

#### Server → Client Messages

**New Message**:
```json
{
  "type": "message",
  "message": {
    "message_id": "uuid",
    "session_id": "uuid",
    "telegram_message_id": 12345,
    "sender": "user",  // or "bot"
    "content": "Hello, bot!",
    "content_type": "text",
    "timestamp": "2025-10-14T10:35:00Z",
    "metadata": {}
  }
}
```

**Message History**:
```json
{
  "type": "history",
  "messages": [/* array of messages */]
}
```

**Session Update**:
```json
{
  "type": "session_update",
  "session": {/* session object */}
}
```

**Error**:
```json
{
  "type": "error",
  "error": "Error message"
}
```

## Security Considerations

### Localhost Only (Default)

By default, the server binds to `127.0.0.1`, making it accessible only from the local machine. This is the recommended configuration for development and personal use.

### Authentication (Optional)

To require authentication for the web interface:

```bash
export TELEGRAM_WEB_AUTH="my_secret_token"
clud --telegram-server
```

Web clients must provide the auth token when connecting via WebSocket.

### User Whitelisting

To restrict bot access to specific Telegram users:

```bash
export TELEGRAM_ALLOWED_USERS="123456,789012"
clud --telegram-server
```

Only users with the specified Telegram user IDs can interact with the bot.

### Data Privacy

- **In-Memory Storage**: Message history is stored in memory by default (not persisted to disk)
- **Session Isolation**: Each user's session is completely isolated from others
- **No Logging of Messages**: By default, message content is not logged (only metadata)

### Production Deployment

For production deployments, consider:

1. **HTTPS/WSS**: Use reverse proxy (nginx, Caddy) for SSL/TLS
2. **Authentication**: Enable `TELEGRAM_WEB_AUTH` for web access
3. **Rate Limiting**: Implement rate limiting at the reverse proxy level
4. **Firewall Rules**: Restrict access to trusted IP ranges
5. **Environment Variables**: Use secrets management (not hardcoded tokens)

## Troubleshooting

### Bot Token Not Configured

**Error**: `ERROR: No Telegram bot token configured!`

**Solution**: Set the `TELEGRAM_BOT_TOKEN` environment variable:
```bash
export TELEGRAM_BOT_TOKEN="your_token_here"
```

### Port Already in Use

**Error**: `ERROR: Port 8889 is already in use`

**Solution**: Use a different port:
```bash
clud --telegram-server 9000
```

### Frontend Not Loading

**Issue**: Web interface shows "Frontend not built" message

**Solution**: The frontend is pre-built and should be included. If missing:
```bash
cd src/clud/telegram/frontend
npm install
npm run build
```

### WebSocket Connection Failed

**Issue**: Web interface shows "Disconnected" status

**Possible Causes**:
1. Server not running - restart with `clud --telegram-server`
2. Port mismatch - check console for actual port number
3. Browser console errors - check for CORS or network issues

### Bot Not Responding

**Issue**: Messages sent to Telegram bot receive no response

**Debugging Steps**:
1. Check server logs for errors
2. Verify bot token is correct: `echo $TELEGRAM_BOT_TOKEN`
3. Check if bot is running: `curl http://localhost:8889/api/health`
4. Test with `/start` command in Telegram
5. Check Telegram bot settings with @BotFather

### Session Timeout

**Issue**: Sessions are being cleaned up too quickly

**Solution**: Increase session timeout:
```bash
export TELEGRAM_SESSION_TIMEOUT=7200  # 2 hours
clud --telegram-server
```

## Advanced Usage

### Multiple Bots

To run multiple bot instances on different ports:

```bash
# Terminal 1 - Bot 1
export TELEGRAM_BOT_TOKEN="token1"
clud --telegram-server 8889

# Terminal 2 - Bot 2
export TELEGRAM_BOT_TOKEN="token2"
clud --telegram-server 8890
```

### Webhook Mode (Production)

For production deployments, use webhook mode instead of polling:

```yaml
# telegram_config.yaml
telegram:
  bot_token: ${TELEGRAM_BOT_TOKEN}
  webhook_url: "https://yourdomain.com/telegram/webhook"

web:
  port: 8889
  host: "0.0.0.0"  # Listen on all interfaces
```

Then configure your reverse proxy to forward webhook requests to the server.

### Custom Working Directory

By default, Claude Code runs in the directory where you started the server. To specify a different working directory:

```bash
cd /path/to/your/project
clud --telegram-server
```

All file operations in Claude Code will be relative to this directory.

### Integration with Other Tools

The Telegram integration can be used alongside other clud features:

```bash
# Use with custom MCP servers (if configured)
export MCP_SERVER_CONFIG="/path/to/mcp_config.json"
clud --telegram-server

# Use with specific Claude model
export ANTHROPIC_MODEL="claude-sonnet-4.5"
clud --telegram-server
```

## Performance Tips

### Session Limits

To prevent resource exhaustion, limit the number of concurrent sessions:

```bash
export TELEGRAM_MAX_SESSIONS=20
```

When the limit is reached, the oldest idle session will be cleaned up before creating a new one.

### Message History Limit

Limit the number of messages stored per session:

```bash
export TELEGRAM_MESSAGE_HISTORY_LIMIT=500
```

Older messages will be discarded when the limit is reached.

### Cleanup Interval

Control how often idle sessions are checked and cleaned up:

```yaml
sessions:
  cleanup_interval: 300  # Check every 5 minutes
```

## Monitoring and Logging

### Server Logs

The server logs important events to console by default. To log to a file:

```yaml
logging:
  level: INFO
  file: "logs/telegram.log"
```

Log levels: `DEBUG`, `INFO`, `WARNING`, `ERROR`, `CRITICAL`

### Monitoring Active Sessions

Use the REST API to monitor active sessions:

```bash
curl http://localhost:8889/api/telegram/sessions | jq
```

### Health Checks

Check server health:

```bash
curl http://localhost:8889/api/health
```

## Development

### Running in Development Mode

For development with hot-reload:

```bash
# Terminal 1 - Backend
clud --telegram-server

# Terminal 2 - Frontend (if modifying)
cd src/clud/telegram/frontend
npm run dev
```

### Running Tests

```bash
# Run all Telegram tests
pytest tests/test_telegram*.py -v

# Run with coverage
pytest tests/test_telegram*.py --cov=src/clud/telegram --cov-report=html
```

### Project Structure

```
src/clud/telegram/
├── __init__.py
├── models.py              # Data models (TelegramMessage, TelegramSession)
├── config.py             # Configuration loading and validation
├── session_manager.py    # Core session orchestration
├── bot_handler.py        # Telegram bot API integration
├── ws_server.py          # WebSocket handler for web clients
├── api.py                # REST API endpoints
├── server.py             # Main server and startup logic
└── frontend/             # SvelteKit web interface
    ├── src/
    │   ├── lib/
    │   │   ├── components/  # Svelte components
    │   │   ├── stores/      # State management
    │   │   └── services/    # API and WebSocket services
    │   ├── routes/
    │   └── app.html
    ├── package.json
    └── svelte.config.js
```

## FAQ

### Q: Can I use this in production?

A: The integration is designed for personal use and small teams. For production use with many users, consider adding:
- Persistent storage (database)
- Load balancing
- Redis for session management
- Enhanced security measures

### Q: Does this store my conversations?

A: By default, conversations are stored in memory only and are lost when the server restarts. You can implement persistent storage if needed.

### Q: Can I send messages from the web interface?

A: Not by default. Bidirectional messaging is an optional feature that can be enabled in the configuration.

### Q: How many concurrent users can it handle?

A: The default limit is 50 concurrent sessions. Each session runs its own Claude Code instance, so resource usage scales linearly with active users.

### Q: Does it work with Telegram groups?

A: Currently, the integration is designed for direct messages (DMs) with the bot. Group support would require additional implementation.

### Q: Can I customize the bot's behavior?

A: Yes! The bot runs Claude Code with full access to your project. You can customize behavior through:
- Project-specific commands
- Custom MCP servers
- Environment variables
- Configuration files

## Contributing

Found a bug or have a feature request? Please open an issue on the [GitHub repository](https://github.com/anthropics/clud).

## License

This project is part of `clud` and follows the same license.

## Support

For help with the Telegram integration:

1. Check this documentation
2. Review the troubleshooting section
3. Check existing GitHub issues
4. Open a new issue with detailed logs and steps to reproduce

## Credits

Built with:
- [python-telegram-bot](https://python-telegram-bot.org/) - Telegram Bot API wrapper
- [FastAPI](https://fastapi.tiangolo.com/) - Modern web framework
- [SvelteKit](https://kit.svelte.dev/) - Frontend framework
- [Claude Code](https://claude.com/claude-code) - AI coding assistant

---

**Document Version**: 1.0
**Last Updated**: 2025-10-14
**Author**: Claude Code Development Team
