"""JSON formatter for Claude Code stream-json output."""

import json
import sys
from typing import Any, cast


class StreamJsonFormatter:
    """Format Claude Code's stream-json output to show only relevant information."""

    def __init__(self, show_system: bool = False, show_usage: bool = True, show_cache: bool = False, verbose: bool = False) -> None:
        """Initialize the formatter.

        Args:
            show_system: Show system messages (init, etc.)
            show_usage: Show token usage information
            show_cache: Show cache creation/read tokens
            verbose: Show all fields including debug info
        """
        self.show_system = show_system
        self.show_usage = show_usage
        self.show_cache = show_cache
        self.verbose = verbose
        self.seen_content: set[str] = set()  # Track content we've already shown

    def format_line(self, line: str) -> str | None:
        """Format a single line of stream-json output.

        Args:
            line: A line of JSON output from Claude Code

        Returns:
            Formatted string to display, or None if line should be filtered out
        """
        try:
            # Parse JSON
            data = json.loads(line.strip())

            # Get type and subtype
            msg_type = data.get("type", "unknown")
            subtype = data.get("subtype")

            # Filter system messages unless verbose
            if msg_type == "system":
                if not self.show_system and not self.verbose:
                    return None
                return self._format_system(data, subtype)

            # Format assistant messages
            if msg_type == "assistant":
                return self._format_assistant(data)

            # For unknown types in verbose mode, show raw JSON
            if self.verbose:
                return f"[UNKNOWN TYPE: {msg_type}]\n{json.dumps(data, indent=2)}\n"

            return None

        except json.JSONDecodeError:
            # Not JSON, pass through as-is if verbose
            if self.verbose:
                return line
            return None
        except Exception as e:
            # Error parsing, show in verbose mode
            if self.verbose:
                return f"[ERROR PARSING]: {e}\n{line}\n"
            return None

    def _format_system(self, data: dict[str, Any], subtype: str | None) -> str:
        """Format a system message."""
        if subtype == "init":
            # Extract key info from init message
            cwd = data.get("cwd", "unknown")
            model = data.get("model", "unknown")
            mode = data.get("permissionMode", "unknown")
            return f"🤖 Initialized: {model} in {cwd} (mode: {mode})\n"

        # For other system messages, show type and subtype
        return f"[SYSTEM:{subtype or 'unknown'}]\n"

    def _format_assistant(self, data: dict[str, Any]) -> str:
        """Format an assistant message."""
        message = cast(dict[str, Any], data.get("message", {}))
        output_lines: list[str] = []

        # Extract content
        content = cast(list[Any], message.get("content", []))
        for item in content:
            if isinstance(item, dict):
                item_dict = cast(dict[str, Any], item)
                item_type = cast(str, item_dict.get("type"))

                # Text content
                if item_type == "text":
                    text = cast(str, item_dict.get("text", ""))
                    if text.strip():
                        # Use a hash of the text to avoid duplicates
                        content_hash = f"text:{text}"
                        if content_hash not in self.seen_content:
                            self.seen_content.add(content_hash)
                            output_lines.append(f"💬 {text}")

                # Tool use
                elif item_type == "tool_use":
                    tool_id = cast(str, item_dict.get("id", ""))
                    # Use tool ID to avoid duplicates
                    if tool_id and tool_id in self.seen_content:
                        continue
                    if tool_id:
                        self.seen_content.add(tool_id)

                    tool_name = cast(str, item_dict.get("name", "unknown"))
                    tool_input_raw = item_dict.get("input", {})
                    tool_input = cast(dict[str, Any], tool_input_raw if isinstance(tool_input_raw, dict) else {})
                    description = cast(str, tool_input.get("description", ""))

                    if description:
                        output_lines.append(f"🔧 {tool_name}: {description}")
                    else:
                        # Show abbreviated tool input
                        if tool_name == "Bash":
                            cmd = cast(str, tool_input.get("command", ""))
                            output_lines.append(f"🔧 Bash: {cmd[:100]}")
                        elif tool_name == "Read":
                            file_path = cast(str, tool_input.get("file_path", ""))
                            output_lines.append(f"🔧 Read: {file_path}")
                        elif tool_name == "Edit":
                            file_path = cast(str, tool_input.get("file_path", ""))
                            old_string = cast(str, tool_input.get("old_string", ""))
                            new_string = cast(str, tool_input.get("new_string", ""))
                            replace_all = tool_input.get("replace_all", False)

                            # Format the edit details
                            edit_info = self._format_edit_details(file_path, old_string, new_string, replace_all)
                            output_lines.append(f"🔧 Edit: {edit_info}")
                        elif tool_name == "Write":
                            file_path = cast(str, tool_input.get("file_path", ""))
                            output_lines.append(f"🔧 Write: {file_path}")
                        elif tool_name == "Glob":
                            pattern = cast(str, tool_input.get("pattern", ""))
                            output_lines.append(f"🔧 Glob: {pattern}")
                        elif tool_name == "Grep":
                            pattern = cast(str, tool_input.get("pattern", ""))
                            output_lines.append(f"🔧 Grep: {pattern}")
                        else:
                            output_lines.append(f"🔧 {tool_name}")

        # Show usage if enabled
        usage = message.get("usage", {})
        if self.show_usage and usage:
            input_tokens = usage.get("input_tokens", 0)
            output_tokens = usage.get("output_tokens", 0)
            total_tokens = input_tokens + output_tokens

            usage_parts = [f"tokens: {total_tokens}"]

            if self.show_cache:
                cache_creation = usage.get("cache_creation_input_tokens", 0)
                cache_read = usage.get("cache_read_input_tokens", 0)
                if cache_creation > 0:
                    usage_parts.append(f"cache_create: {cache_creation}")
                if cache_read > 0:
                    usage_parts.append(f"cache_read: {cache_read}")

            output_lines.append(f"📊 {', '.join(usage_parts)}")

        return "\n".join(output_lines) + "\n" if output_lines else ""

    def _format_edit_details(self, file_path: str, old_string: str, new_string: str, replace_all: bool) -> str:
        """Format edit details for display.

        Args:
            file_path: Path to the file being edited
            old_string: The old string being replaced
            new_string: The new string replacing it
            replace_all: Whether this is a replace_all operation

        Returns:
            Formatted string with edit details
        """
        import os

        # Use just the filename if path is long
        filename = os.path.basename(file_path)

        # Count lines in old and new strings
        old_lines = old_string.count("\n") + (1 if old_string and not old_string.endswith("\n") else 0)
        new_lines = new_string.count("\n") + (1 if new_string and not new_string.endswith("\n") else 0)

        # Determine format based on edit characteristics
        max_inline_length = 50

        # For replace_all, show that info
        if replace_all:
            char_count = len(old_string)
            return f"{filename} (replace all, {char_count} chars)"

        # For short single-line edits, show inline preview
        if old_lines == 1 and new_lines == 1 and len(old_string) <= max_inline_length and len(new_string) <= max_inline_length:
            old_preview = old_string.strip()[:max_inline_length]
            new_preview = new_string.strip()[:max_inline_length]
            return f'{filename} | "{old_preview}" → "{new_preview}"'

        # For multi-line edits or longer edits, show line count changes
        if old_lines != new_lines:
            return f"{filename} ({old_lines}→{new_lines} lines)"
        else:
            char_diff = len(new_string) - len(old_string)
            char_info = f"+{char_diff}" if char_diff > 0 else str(char_diff)
            return f"{filename} ({old_lines} lines, {char_info} chars)"


def create_formatter_callback(formatter: StreamJsonFormatter | None = None, output_file: Any = None) -> Any:
    """Create a callback function for RunningProcess that formats JSON output.

    Args:
        formatter: StreamJsonFormatter instance to use (creates default if None)
        output_file: File to write to (defaults to sys.stdout)

    Returns:
        Callback function that can be passed to RunningProcess
    """
    if formatter is None:
        formatter = StreamJsonFormatter()
    if output_file is None:
        output_file = sys.stdout

    def callback(line: str) -> None:
        """Format and print a line of JSON output."""
        formatted = formatter.format_line(line)
        if formatted:
            output_file.write(formatted)
            output_file.flush()

    return callback
