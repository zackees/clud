"""UI renderer for Claude streaming output."""

import sys
from typing import Any, TextIO

from clud.streaming_parser import (
    AssistantMessage,
    ParsedSession,
    StreamEvent,
    SystemInitEvent,
    TextContent,
    ToolResultContent,
    ToolUseContent,
    UserMessage,
)


class Colors:
    """ANSI color codes."""

    RESET = "\033[0m"
    BOLD = "\033[1m"
    DIM = "\033[2m"

    # Foreground colors
    BLACK = "\033[30m"
    RED = "\033[31m"
    GREEN = "\033[32m"
    YELLOW = "\033[33m"
    BLUE = "\033[34m"
    MAGENTA = "\033[35m"
    CYAN = "\033[36m"
    WHITE = "\033[37m"

    # Bright colors
    BRIGHT_BLACK = "\033[90m"
    BRIGHT_RED = "\033[91m"
    BRIGHT_GREEN = "\033[92m"
    BRIGHT_YELLOW = "\033[93m"
    BRIGHT_BLUE = "\033[94m"
    BRIGHT_MAGENTA = "\033[95m"
    BRIGHT_CYAN = "\033[96m"
    BRIGHT_WHITE = "\033[97m"


class Icons:
    """Unicode icons for UI."""

    ROBOT = "🤖"
    TOOL = "🔧"
    SUCCESS = "✅"
    ERROR = "❌"
    RUNNING = "⏱"
    INFO = "ℹ️"
    CHART = "📊"
    PACKAGE = "📦"


class StreamingUI:
    """UI renderer for streaming events."""

    def __init__(self, output: TextIO = sys.stdout, use_colors: bool = True) -> None:
        """Initialize the UI renderer.

        Args:
            output: Output stream to write to
            use_colors: Whether to use ANSI colors
        """
        self.output = output
        self.use_colors = use_colors
        self.active_tools: dict[str, str] = {}  # tool_id -> tool_name

    def _colorize(self, text: str, color: str) -> str:
        """Colorize text if colors are enabled.

        Args:
            text: Text to colorize
            color: ANSI color code

        Returns:
            Colorized text or plain text
        """
        if not self.use_colors:
            return text
        return f"{color}{text}{Colors.RESET}"

    def _print(self, text: str = "") -> None:
        """Print text to output.

        Args:
            text: Text to print
        """
        print(text, file=self.output)

    def _format_json_compact(self, data: Any, max_width: int = 60) -> str:
        """Format JSON data compactly.

        Args:
            data: Dictionary or list to format
            max_width: Maximum width before truncation

        Returns:
            Formatted JSON string
        """
        import json

        json_str = json.dumps(data, ensure_ascii=False)
        if len(json_str) > max_width:
            json_str = json_str[: max_width - 3] + "..."
        return json_str

    def render_system_init(self, event: SystemInitEvent) -> None:
        """Render system initialization event.

        Args:
            event: System init event to render
        """
        self._print()
        self._print(self._colorize("=" * 70, Colors.BRIGHT_BLUE))
        self._print(self._colorize(f"{Icons.PACKAGE} Session Started", Colors.BRIGHT_BLUE + Colors.BOLD))
        self._print(self._colorize("=" * 70, Colors.BRIGHT_BLUE))
        self._print()
        self._print(f"{self._colorize('Session ID:', Colors.BRIGHT_BLACK)} {event.session_id[:16]}...")
        self._print(f"{self._colorize('Model:', Colors.BRIGHT_BLACK)} {self._colorize(event.model, Colors.BRIGHT_CYAN)}")
        self._print(f"{self._colorize('Working Dir:', Colors.BRIGHT_BLACK)} {event.cwd}")
        self._print(f"{self._colorize('Tools:', Colors.BRIGHT_BLACK)} {len(event.tools)} available")
        self._print(f"{self._colorize('Mode:', Colors.BRIGHT_BLACK)} {self._colorize(event.permission_mode, Colors.BRIGHT_YELLOW)}")
        self._print()

    def render_text_message(self, event: AssistantMessage, content: TextContent) -> None:
        """Render text content from assistant.

        Args:
            event: Assistant message event
            content: Text content to render
        """
        if content.text.strip():
            self._print(self._colorize(f"{Icons.ROBOT} Claude:", Colors.BRIGHT_GREEN + Colors.BOLD))
            self._print(f"  {content.text}")
            self._print()

    def render_tool_use(self, event: AssistantMessage, content: ToolUseContent) -> None:
        """Render tool use invocation.

        Args:
            event: Assistant message event
            content: Tool use content to render
        """
        # Track active tool
        self.active_tools[content.id] = content.name

        self._print(self._colorize(f"{Icons.TOOL} Tool: {content.name}", Colors.BRIGHT_YELLOW + Colors.BOLD))
        self._print(self._colorize(f"  ID: {content.id[:16]}...", Colors.BRIGHT_BLACK))

        # Format input parameters
        if content.input:
            self._print(self._colorize("  Input:", Colors.BRIGHT_BLACK))
            for key, value in content.input.items():
                if isinstance(value, dict | list):
                    value_str = self._format_json_compact(value)
                else:
                    value_str = str(value)
                    if len(value_str) > 80:
                        value_str = value_str[:77] + "..."
                self._print(f"    {self._colorize(key, Colors.CYAN)}: {value_str}")

        self._print(self._colorize(f"  {Icons.RUNNING} Running...", Colors.YELLOW))
        self._print()

    def render_tool_result(self, event: UserMessage, content: ToolResultContent) -> None:
        """Render tool result.

        Args:
            event: User message event
            content: Tool result content to render
        """
        tool_name = self.active_tools.get(content.tool_use_id, "Unknown")

        # Remove from active tools
        self.active_tools.pop(content.tool_use_id, None)

        self._print(self._colorize(f"{Icons.SUCCESS} Result: {tool_name}", Colors.BRIGHT_GREEN + Colors.BOLD))
        self._print(self._colorize(f"  ID: {content.tool_use_id[:16]}...", Colors.BRIGHT_BLACK))

        # Format result content
        result_str = content.content
        if len(result_str) > 500:
            # Show first 500 chars
            preview = result_str[:500] + "..."
            line_count = result_str.count("\n") + 1
            self._print(self._colorize(f"  Output: [{line_count} lines, showing preview]", Colors.BRIGHT_BLACK))
            self._print(self._colorize(f"  {preview}", Colors.DIM))
        else:
            self._print(self._colorize("  Output:", Colors.BRIGHT_BLACK))
            # Indent each line
            for line in result_str.split("\n")[:20]:  # Max 20 lines
                self._print(f"  {line}")

        self._print()

    def render_token_usage(self, event: AssistantMessage) -> None:
        """Render token usage statistics.

        Args:
            event: Assistant message event
        """
        usage = event.usage

        # Only show if there's actual token usage
        if usage.input_tokens == 0 and usage.output_tokens == 0:
            return

        self._print(self._colorize(f"{Icons.CHART} Token Usage", Colors.BRIGHT_BLUE))
        self._print(f"  {self._colorize('Input:', Colors.BRIGHT_BLACK)} {usage.input_tokens:,}")

        if usage.cache_read_input_tokens > 0:
            self._print(f"  {self._colorize('Cache Read:', Colors.BRIGHT_BLACK)} {usage.cache_read_input_tokens:,} {self._colorize('(saved)', Colors.GREEN)}")

        if usage.cache_creation_input_tokens > 0:
            self._print(f"  {self._colorize('Cache Write:', Colors.BRIGHT_BLACK)} {usage.cache_creation_input_tokens:,}")

        self._print(f"  {self._colorize('Output:', Colors.BRIGHT_BLACK)} {usage.output_tokens:,}")

        total = usage.input_tokens + usage.output_tokens
        self._print(f"  {self._colorize('Total:', Colors.BRIGHT_BLACK)} {self._colorize(f'{total:,}', Colors.BRIGHT_CYAN)}")
        self._print()

    def render_event(self, event: StreamEvent) -> None:
        """Render a single event.

        Args:
            event: Event to render
        """
        if isinstance(event, SystemInitEvent):
            self.render_system_init(event)

        elif isinstance(event, AssistantMessage):
            for content in event.content:
                if isinstance(content, TextContent):
                    self.render_text_message(event, content)
                elif isinstance(content, ToolUseContent):
                    self.render_tool_use(event, content)

            # Show token usage after content
            self.render_token_usage(event)

        elif isinstance(event, UserMessage):
            for content in event.content:
                if isinstance(content, ToolResultContent):
                    self.render_tool_result(event, content)

    def render_session_summary(self, session: ParsedSession) -> None:
        """Render session summary.

        Args:
            session: Parsed session to summarize
        """
        self._print()
        self._print(self._colorize("=" * 70, Colors.BRIGHT_BLUE))
        self._print(self._colorize(f"{Icons.CHART} Session Summary", Colors.BRIGHT_BLUE + Colors.BOLD))
        self._print(self._colorize("=" * 70, Colors.BRIGHT_BLUE))
        self._print()

        # Count events
        assistant_msgs = sum(1 for e in session.events if isinstance(e, AssistantMessage))
        tool_results = sum(1 for e in session.events if isinstance(e, UserMessage))

        self._print(f"{self._colorize('Total Events:', Colors.BRIGHT_BLACK)} {len(session.events)}")
        self._print(f"{self._colorize('Assistant Messages:', Colors.BRIGHT_BLACK)} {assistant_msgs}")
        self._print(f"{self._colorize('Tool Executions:', Colors.BRIGHT_BLACK)} {tool_results}")
        self._print()

        # Token usage
        usage = session.total_usage
        self._print(self._colorize("Total Token Usage:", Colors.BRIGHT_CYAN + Colors.BOLD))
        self._print(f"  {self._colorize('Input:', Colors.BRIGHT_BLACK)} {usage.input_tokens:,}")

        if usage.cache_read_input_tokens > 0:
            cache_savings = usage.cache_read_input_tokens
            self._print(f"  {self._colorize('Cache Read:', Colors.BRIGHT_BLACK)} {cache_savings:,} {self._colorize('(saved)', Colors.GREEN)}")

        if usage.cache_creation_input_tokens > 0:
            self._print(f"  {self._colorize('Cache Write:', Colors.BRIGHT_BLACK)} {usage.cache_creation_input_tokens:,}")

        self._print(f"  {self._colorize('Output:', Colors.BRIGHT_BLACK)} {usage.output_tokens:,}")

        total = usage.input_tokens + usage.output_tokens
        self._print(f"  {self._colorize('Total:', Colors.BRIGHT_BLACK)} {self._colorize(f'{total:,}', Colors.BRIGHT_CYAN + Colors.BOLD)}")
        self._print()

    def render_session(self, session: ParsedSession) -> None:
        """Render entire session.

        Args:
            session: Parsed session to render
        """
        # Render init event first
        if session.init_event:
            self.render_event(session.init_event)

        # Render all events
        for event in session.events:
            self.render_event(event)

        # Render summary
        self.render_session_summary(session)
