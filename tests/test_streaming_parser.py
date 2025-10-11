"""Tests for streaming JSON parser."""

import json

from clud.streaming_parser import (
    AssistantMessage,
    StreamingParser,
    SystemInitEvent,
    TextContent,
    ToolResultContent,
    ToolUseContent,
    UserMessage,
)


def test_parse_system_init() -> None:
    """Test parsing system initialization event."""
    parser = StreamingParser()

    json_line = json.dumps(
        {
            "type": "system",
            "subtype": "init",
            "cwd": "/test/dir",
            "session_id": "test-session-123",
            "model": "claude-sonnet-4-5",
            "tools": ["Bash", "Read", "Write"],
            "mcp_servers": [],
            "permissionMode": "bypassPermissions",
            "slash_commands": ["test"],
            "apiKeySource": "ANTHROPIC_API_KEY",
            "output_style": "default",
            "agents": ["general-purpose"],
            "uuid": "test-uuid-123",
        }
    )

    event = parser.parse_line(json_line)

    assert isinstance(event, SystemInitEvent)
    assert event.cwd == "/test/dir"
    assert event.session_id == "test-session-123"
    assert event.model == "claude-sonnet-4-5"
    assert event.tools == ["Bash", "Read", "Write"]
    assert event.permission_mode == "bypassPermissions"
    assert parser.session.init_event == event


def test_parse_assistant_text_message() -> None:
    """Test parsing assistant text message."""
    parser = StreamingParser()

    json_line = json.dumps(
        {
            "type": "assistant",
            "message": {
                "id": "msg_123",
                "model": "claude-sonnet-4-5",
                "content": [{"type": "text", "text": "Hello, how can I help?"}],
                "usage": {"input_tokens": 100, "output_tokens": 20, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0},
            },
            "session_id": "test-session",
            "uuid": "test-uuid",
        }
    )

    event = parser.parse_line(json_line)

    assert isinstance(event, AssistantMessage)
    assert event.message_id == "msg_123"
    assert len(event.content) == 1
    assert isinstance(event.content[0], TextContent)
    assert event.content[0].text == "Hello, how can I help?"
    assert event.usage.input_tokens == 100
    assert event.usage.output_tokens == 20


def test_parse_assistant_tool_use() -> None:
    """Test parsing assistant tool use message."""
    parser = StreamingParser()

    json_line = json.dumps(
        {
            "type": "assistant",
            "message": {
                "id": "msg_123",
                "model": "claude-sonnet-4-5",
                "content": [{"type": "tool_use", "id": "tool_123", "name": "Read", "input": {"file_path": "/test/file.txt"}}],
                "usage": {"input_tokens": 50, "output_tokens": 10, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0},
            },
            "session_id": "test-session",
            "uuid": "test-uuid",
        }
    )

    event = parser.parse_line(json_line)

    assert isinstance(event, AssistantMessage)
    assert len(event.content) == 1
    assert isinstance(event.content[0], ToolUseContent)
    assert event.content[0].name == "Read"
    assert event.content[0].input == {"file_path": "/test/file.txt"}


def test_parse_user_tool_result() -> None:
    """Test parsing user tool result message."""
    parser = StreamingParser()

    json_line = json.dumps(
        {
            "type": "user",
            "message": {"content": [{"type": "tool_result", "tool_use_id": "tool_123", "content": "File contents here"}]},
            "session_id": "test-session",
            "uuid": "test-uuid",
        }
    )

    event = parser.parse_line(json_line)

    assert isinstance(event, UserMessage)
    assert len(event.content) == 1
    assert isinstance(event.content[0], ToolResultContent)
    assert event.content[0].tool_use_id == "tool_123"
    assert event.content[0].content == "File contents here"


def test_parse_stream_full_conversation() -> None:
    """Test parsing a full conversation stream."""
    parser = StreamingParser()

    lines = [
        # System init
        json.dumps(
            {
                "type": "system",
                "subtype": "init",
                "cwd": "/test",
                "session_id": "session_1",
                "model": "claude-sonnet-4-5",
                "tools": ["Read"],
                "mcp_servers": [],
                "permissionMode": "bypassPermissions",
                "slash_commands": [],
                "apiKeySource": "env",
                "output_style": "default",
                "agents": [],
                "uuid": "uuid_1",
            }
        ),
        # Assistant text
        json.dumps(
            {
                "type": "assistant",
                "message": {
                    "id": "msg_1",
                    "model": "claude-sonnet-4-5",
                    "content": [{"type": "text", "text": "Let me read that file"}],
                    "usage": {"input_tokens": 100, "output_tokens": 10, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0},
                },
                "session_id": "session_1",
                "uuid": "uuid_2",
            }
        ),
        # Assistant tool use
        json.dumps(
            {
                "type": "assistant",
                "message": {
                    "id": "msg_2",
                    "model": "claude-sonnet-4-5",
                    "content": [{"type": "tool_use", "id": "tool_1", "name": "Read", "input": {"file_path": "/test/file.txt"}}],
                    "usage": {"input_tokens": 50, "output_tokens": 20, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 50},
                },
                "session_id": "session_1",
                "uuid": "uuid_3",
            }
        ),
        # Tool result
        json.dumps(
            {
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "tool_1", "content": "File content"}]},
                "session_id": "session_1",
                "uuid": "uuid_4",
            }
        ),
    ]

    session = parser.parse_stream(lines)

    # Check init event
    assert session.init_event is not None
    assert session.init_event.session_id == "session_1"

    # Check events
    assert len(session.events) == 3  # 2 assistant + 1 user
    assert isinstance(session.events[0], AssistantMessage)
    assert isinstance(session.events[1], AssistantMessage)
    assert isinstance(session.events[2], UserMessage)

    # Check token usage accumulation
    assert session.total_usage.input_tokens == 150  # 100 + 50
    assert session.total_usage.output_tokens == 30  # 10 + 20
    assert session.total_usage.cache_read_input_tokens == 50


def test_parse_invalid_json() -> None:
    """Test parsing invalid JSON returns None."""
    parser = StreamingParser()
    event = parser.parse_line("not json at all")
    assert event is None


def test_parse_unknown_event_type() -> None:
    """Test parsing unknown event type returns None."""
    parser = StreamingParser()
    json_line = json.dumps({"type": "unknown", "data": "test"})
    event = parser.parse_line(json_line)
    assert event is None


def test_token_usage_accumulation() -> None:
    """Test that token usage is accumulated correctly."""
    parser = StreamingParser()

    # First message
    parser.parse_line(
        json.dumps(
            {
                "type": "assistant",
                "message": {
                    "id": "msg_1",
                    "model": "test",
                    "content": [],
                    "usage": {"input_tokens": 100, "output_tokens": 20, "cache_creation_input_tokens": 10, "cache_read_input_tokens": 5},
                },
                "session_id": "test",
                "uuid": "test",
            }
        )
    )

    # Second message
    parser.parse_line(
        json.dumps(
            {
                "type": "assistant",
                "message": {
                    "id": "msg_2",
                    "model": "test",
                    "content": [],
                    "usage": {"input_tokens": 50, "output_tokens": 30, "cache_creation_input_tokens": 20, "cache_read_input_tokens": 15},
                },
                "session_id": "test",
                "uuid": "test",
            }
        )
    )

    # Check accumulation
    assert parser.session.total_usage.input_tokens == 150  # 100 + 50
    assert parser.session.total_usage.output_tokens == 50  # 20 + 30
    assert parser.session.total_usage.cache_creation_input_tokens == 30  # 10 + 20
    assert parser.session.total_usage.cache_read_input_tokens == 20  # 5 + 15


def test_parse_mixed_content() -> None:
    """Test parsing message with both text and tool use."""
    parser = StreamingParser()

    json_line = json.dumps(
        {
            "type": "assistant",
            "message": {
                "id": "msg_123",
                "model": "claude-sonnet-4-5",
                "content": [{"type": "text", "text": "Let me check that"}, {"type": "tool_use", "id": "tool_1", "name": "Read", "input": {"file": "test.txt"}}],
                "usage": {"input_tokens": 100, "output_tokens": 20, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0},
            },
            "session_id": "test",
            "uuid": "test",
        }
    )

    event = parser.parse_line(json_line)

    assert isinstance(event, AssistantMessage)
    assert len(event.content) == 2
    assert isinstance(event.content[0], TextContent)
    assert isinstance(event.content[1], ToolUseContent)
