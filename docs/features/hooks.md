# Hook System and Message Handler API

The hook system provides an event-based architecture for intercepting and forwarding execution events to external systems (Telegram, webhooks, etc.).

## Hook System (`src/clud/hooks/`)

### Core Components

- **HookManager**: Singleton that manages hook registration and event triggering
- **HookHandler Protocol**: Interface for implementing custom hook handlers
- **TelegramHookHandler**: Built-in handler for streaming output to Telegram
- **WebhookHandler**: Built-in handler for HTTP webhook notifications

### Hook Events

- **PRE_EXECUTION**: Before Claude Code starts execution
- **POST_EXECUTION**: After Claude Code completes execution
- **OUTPUT_CHUNK**: Real-time output streaming chunks
- **ERROR**: When an error occurs during execution
- **AGENT_START**: When agent subprocess starts
- **AGENT_STOP**: When agent subprocess stops

### Implementation

```python
from clud.hooks import HookManager, HookHandler, HookEvent, HookContext

# Create custom hook handler
class MyHookHandler(HookHandler):
    def on_event(self, event: HookEvent, context: HookContext) -> None:
        if event == HookEvent.OUTPUT_CHUNK:
            print(f"Output: {context.data}")

# Register handler
hook_manager = HookManager.get_instance()
hook_manager.register_handler(MyHookHandler())

# Trigger event
hook_manager.trigger(HookEvent.OUTPUT_CHUNK, {"data": "Hello, world!"})
```

## Message Handler API (`src/clud/api/`)

### Purpose

Unified API for routing messages from multiple client types (Telegram, Web UI, etc.) to clud instances.

### Core Components

#### MessageHandler

Core routing logic with session management.

- Routes messages to appropriate clud instance
- Manages session state
- Handles instance lifecycle

#### InstancePool

Manages lifecycle of clud subprocess instances.

- **Automatic instance reuse** per session
- **Idle timeout and cleanup** (default: 30 minutes)
- **Max instances limit** (default: 100)
- **Resource cleanup** on shutdown

### FastAPI Server Endpoints

#### REST Endpoints

- `POST /api/message` - Send message to clud instance
- `GET /api/instances` - List all active instances
- `DELETE /api/instances/{id}` - Delete an instance

#### WebSocket Endpoints

- `WebSocket /ws/{instance_id}` - Real-time output streaming

### Usage Example

```python
from clud.api import MessageHandler, MessageRequest

# Create message handler
handler = MessageHandler()

# Send message
request = MessageRequest(
    session_id="user123",
    message="Help me write a Python function",
    project_path="/home/user/project"
)

response = await handler.handle_message(request)
print(response.output)
```

## Testing

- `tests/test_hooks.py` - Hook system unit tests
- `tests/test_api_models.py` - API models unit tests
- `tests/test_message_handler.py` - Message handler unit tests
- `tests/test_instance_manager.py` - Instance manager unit tests
- `tests/test_webui_e2e.py` - End-to-end Playwright tests for Web UI (run with `bash test --full`)

## Architecture

### Hook System Data Flow

```
Execution Event → HookManager → Registered Handlers
                                    ↓
                          Telegram / Webhook / Custom
```

### Message Handler Data Flow

```
Client (Telegram/Web) → MessageHandler → InstancePool
                                             ↓
                                      Claude Code Instance
                                             ↓
                                    Output → WebSocket → Client
```

## Configuration

### Hook Configuration (`hooks/config.py`)

```python
from clud.hooks.config import HookConfig

config = HookConfig(
    telegram_enabled=True,
    webhook_enabled=True,
    webhook_url="https://example.com/webhook"
)
```

### API Configuration

```python
from clud.api import APIConfig

config = APIConfig(
    max_instances=100,
    idle_timeout_minutes=30,
    port=8000
)
```

## Related Documentation

- [Telegram API](telegram-api.md)
- [Web UI](webui.md)
- [Development Setup](../development/setup.md)
- [Architecture](../development/architecture.md)
