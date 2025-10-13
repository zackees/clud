"""Tests for streaming JSON parser."""

import json
import unittest

from clud.streaming_parser import (
    AssistantMessage,
    StreamingParser,
    SystemInitEvent,
    TextContent,
    ToolResultContent,
    ToolUseContent,
    UserMessage,
)


class TestStreamingParser(unittest.TestCase):
    """Test streaming JSON parser functionality."""

    def test_parse_system_init(self) -> None:
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

        self.assertIsInstance(event, SystemInitEvent)
        assert isinstance(event, SystemInitEvent)  # Type assertion for pyright
        self.assertEqual(event.cwd, "/test/dir")
        self.assertEqual(event.session_id, "test-session-123")
        self.assertEqual(event.model, "claude-sonnet-4-5")
        self.assertEqual(event.tools, ["Bash", "Read", "Write"])
        self.assertEqual(event.permission_mode, "bypassPermissions")
        self.assertEqual(parser.session.init_event, event)

    def test_parse_assistant_text_message(self) -> None:
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

        self.assertIsInstance(event, AssistantMessage)
        assert isinstance(event, AssistantMessage)  # Type assertion for pyright
        self.assertEqual(event.message_id, "msg_123")
        self.assertEqual(len(event.content), 1)
        self.assertIsInstance(event.content[0], TextContent)
        assert isinstance(event.content[0], TextContent)  # Type assertion for pyright
        self.assertEqual(event.content[0].text, "Hello, how can I help?")
        self.assertEqual(event.usage.input_tokens, 100)
        self.assertEqual(event.usage.output_tokens, 20)

    def test_parse_assistant_tool_use(self) -> None:
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

        self.assertIsInstance(event, AssistantMessage)
        assert isinstance(event, AssistantMessage)  # Type assertion for pyright
        self.assertEqual(len(event.content), 1)
        self.assertIsInstance(event.content[0], ToolUseContent)
        assert isinstance(event.content[0], ToolUseContent)  # Type assertion for pyright
        self.assertEqual(event.content[0].name, "Read")
        self.assertEqual(event.content[0].input, {"file_path": "/test/file.txt"})

    def test_parse_user_tool_result(self) -> None:
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

        self.assertIsInstance(event, UserMessage)
        assert isinstance(event, UserMessage)  # Type assertion for pyright
        self.assertEqual(len(event.content), 1)
        self.assertIsInstance(event.content[0], ToolResultContent)
        assert isinstance(event.content[0], ToolResultContent)  # Type assertion for pyright
        self.assertEqual(event.content[0].tool_use_id, "tool_123")
        self.assertEqual(event.content[0].content, "File contents here")

    def test_parse_stream_full_conversation(self) -> None:
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
        self.assertIsNotNone(session.init_event)
        assert session.init_event is not None  # Type assertion for pyright
        self.assertEqual(session.init_event.session_id, "session_1")

        # Check events
        self.assertEqual(len(session.events), 3)  # 2 assistant + 1 user
        self.assertIsInstance(session.events[0], AssistantMessage)
        self.assertIsInstance(session.events[1], AssistantMessage)
        self.assertIsInstance(session.events[2], UserMessage)

        # Check token usage accumulation
        self.assertEqual(session.total_usage.input_tokens, 150)  # 100 + 50
        self.assertEqual(session.total_usage.output_tokens, 30)  # 10 + 20
        self.assertEqual(session.total_usage.cache_read_input_tokens, 50)

    def test_parse_invalid_json(self) -> None:
        """Test parsing invalid JSON returns None."""
        parser = StreamingParser()
        event = parser.parse_line("not json at all")
        self.assertIsNone(event)

    def test_parse_unknown_event_type(self) -> None:
        """Test parsing unknown event type returns None."""
        parser = StreamingParser()
        json_line = json.dumps({"type": "unknown", "data": "test"})
        event = parser.parse_line(json_line)
        self.assertIsNone(event)

    def test_token_usage_accumulation(self) -> None:
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
        self.assertEqual(parser.session.total_usage.input_tokens, 150)  # 100 + 50
        self.assertEqual(parser.session.total_usage.output_tokens, 50)  # 20 + 30
        self.assertEqual(parser.session.total_usage.cache_creation_input_tokens, 30)  # 10 + 20
        self.assertEqual(parser.session.total_usage.cache_read_input_tokens, 20)  # 5 + 15

    def test_parse_mixed_content(self) -> None:
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

        self.assertIsInstance(event, AssistantMessage)
        assert isinstance(event, AssistantMessage)  # Type assertion for pyright
        self.assertEqual(len(event.content), 2)
        self.assertIsInstance(event.content[0], TextContent)
        self.assertIsInstance(event.content[1], ToolUseContent)


if __name__ == "__main__":
    unittest.main()
