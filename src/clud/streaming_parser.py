"""JSON parser for Claude streaming output."""

import json
from dataclasses import dataclass, field
from typing import Any, Literal


@dataclass
class TokenUsage:
    """Token usage statistics."""

    input_tokens: int = 0
    output_tokens: int = 0
    cache_creation_input_tokens: int = 0
    cache_read_input_tokens: int = 0


@dataclass
class SystemInitEvent:
    """System initialization event."""

    cwd: str
    session_id: str
    model: str
    tools: list[str]
    mcp_servers: list[str]
    permission_mode: str
    slash_commands: list[str]
    api_key_source: str
    output_style: str
    agents: list[str]
    uuid: str


@dataclass
class TextContent:
    """Text content in a message."""

    type: Literal["text"]
    text: str


@dataclass
class ToolUseContent:
    """Tool use content in a message."""

    type: Literal["tool_use"]
    id: str
    name: str
    input: dict[str, Any]


@dataclass
class ToolResultContent:
    """Tool result content in a message."""

    type: Literal["tool_result"]
    tool_use_id: str
    content: str


@dataclass
class AssistantMessage:
    """Assistant message event."""

    message_id: str
    model: str
    content: list[TextContent | ToolUseContent]
    usage: TokenUsage
    session_id: str
    uuid: str
    parent_tool_use_id: str | None = None


@dataclass
class UserMessage:
    """User message event (tool results)."""

    content: list[ToolResultContent]
    session_id: str
    uuid: str
    parent_tool_use_id: str | None = None


StreamEvent = SystemInitEvent | AssistantMessage | UserMessage


@dataclass
class ParsedSession:
    """Parsed streaming session."""

    init_event: SystemInitEvent | None = None
    events: list[StreamEvent] = field(default_factory=lambda: [])
    total_usage: TokenUsage = field(default_factory=TokenUsage)


class StreamingParser:
    """Parser for Claude streaming JSON output."""

    def __init__(self) -> None:
        """Initialize the parser."""
        self.session = ParsedSession()

    def parse_line(self, line: str) -> StreamEvent | None:
        """Parse a single JSON line.

        Args:
            line: JSON string to parse

        Returns:
            Parsed event or None if parsing fails
        """
        try:
            data = json.loads(line)
        except json.JSONDecodeError:
            return None

        event_type = data.get("type")

        if event_type == "system":
            return self._parse_system_event(data)
        elif event_type == "assistant":
            return self._parse_assistant_event(data)
        elif event_type == "user":
            return self._parse_user_event(data)

        return None

    def _parse_system_event(self, data: dict[str, Any]) -> SystemInitEvent | None:
        """Parse system initialization event."""
        if data.get("subtype") != "init":
            return None

        event = SystemInitEvent(
            cwd=data.get("cwd", ""),
            session_id=data.get("session_id", ""),
            model=data.get("model", ""),
            tools=data.get("tools", []),
            mcp_servers=data.get("mcp_servers", []),
            permission_mode=data.get("permissionMode", ""),
            slash_commands=data.get("slash_commands", []),
            api_key_source=data.get("apiKeySource", ""),
            output_style=data.get("output_style", ""),
            agents=data.get("agents", []),
            uuid=data.get("uuid", ""),
        )

        self.session.init_event = event
        return event

    def _parse_assistant_event(self, data: dict[str, Any]) -> AssistantMessage | None:
        """Parse assistant message event."""
        message_data = data.get("message", {})
        content_list = message_data.get("content", [])

        # Parse content items
        parsed_content: list[TextContent | ToolUseContent] = []
        for item in content_list:
            content_type = item.get("type")
            if content_type == "text":
                parsed_content.append(TextContent(type="text", text=item.get("text", "")))
            elif content_type == "tool_use":
                parsed_content.append(
                    ToolUseContent(
                        type="tool_use",
                        id=item.get("id", ""),
                        name=item.get("name", ""),
                        input=item.get("input", {}),
                    )
                )

        # Parse usage
        usage_data = message_data.get("usage", {})
        usage = TokenUsage(
            input_tokens=usage_data.get("input_tokens", 0),
            output_tokens=usage_data.get("output_tokens", 0),
            cache_creation_input_tokens=usage_data.get("cache_creation_input_tokens", 0),
            cache_read_input_tokens=usage_data.get("cache_read_input_tokens", 0),
        )

        # Update total usage
        self.session.total_usage.input_tokens += usage.input_tokens
        self.session.total_usage.output_tokens += usage.output_tokens
        self.session.total_usage.cache_creation_input_tokens += usage.cache_creation_input_tokens
        self.session.total_usage.cache_read_input_tokens += usage.cache_read_input_tokens

        event = AssistantMessage(
            message_id=message_data.get("id", ""),
            model=message_data.get("model", ""),
            content=parsed_content,
            usage=usage,
            session_id=data.get("session_id", ""),
            uuid=data.get("uuid", ""),
            parent_tool_use_id=data.get("parent_tool_use_id"),
        )

        self.session.events.append(event)
        return event

    def _parse_user_event(self, data: dict[str, Any]) -> UserMessage | None:
        """Parse user message event (tool results)."""
        message_data = data.get("message", {})
        content_list = message_data.get("content", [])

        # Parse tool results
        parsed_content: list[ToolResultContent] = []
        for item in content_list:
            if item.get("type") == "tool_result":
                parsed_content.append(
                    ToolResultContent(
                        type="tool_result",
                        tool_use_id=item.get("tool_use_id", ""),
                        content=item.get("content", ""),
                    )
                )

        event = UserMessage(
            content=parsed_content,
            session_id=data.get("session_id", ""),
            uuid=data.get("uuid", ""),
            parent_tool_use_id=data.get("parent_tool_use_id"),
        )

        self.session.events.append(event)
        return event

    def parse_stream(self, lines: list[str]) -> ParsedSession:
        """Parse multiple JSON lines.

        Args:
            lines: List of JSON strings

        Returns:
            Parsed session data
        """
        for line in lines:
            line = line.strip()
            if line:
                self.parse_line(line)

        return self.session
