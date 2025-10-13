"""Demo/test for streaming UI renderer."""

import io
import json
import unittest

from clud.streaming_parser import StreamingParser
from clud.streaming_ui import StreamingUI


class TestStreamingUI(unittest.TestCase):
    """Test streaming UI renderer functionality."""

    def test_streaming_ui_demo(self) -> None:
        """Test the streaming UI with sample JSON data."""
        # Sample JSON lines from the user's example
        json_lines = [
            # System init
            json.dumps(
                {
                    "type": "system",
                    "subtype": "init",
                    "cwd": "C:\\Users\\niteris\\dev\\clud2",
                    "session_id": "957fed31-2abe-4a54-9286-61e0e5afe4eb",
                    "tools": ["Task", "Bash", "Glob", "Grep", "Read", "Edit", "Write"],
                    "mcp_servers": [],
                    "model": "claude-sonnet-4-5-20250929",
                    "permissionMode": "bypassPermissions",
                    "slash_commands": ["compact", "context", "cost"],
                    "apiKeySource": "ANTHROPIC_API_KEY",
                    "output_style": "default",
                    "agents": ["general-purpose"],
                    "uuid": "642db9ef-675d-431e-a631-2f3fadc84b97",
                }
            ),
            # Assistant text message
            json.dumps(
                {
                    "type": "assistant",
                    "message": {
                        "model": "claude-sonnet-4-5-20250929",
                        "id": "msg_01SmFbHj3bFcr7bXpD3PbpxZ",
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "text", "text": "I'll help you review and improve the README's Telegram bot instructions. Let me first locate and read the README file."}],
                        "stop_reason": None,
                        "stop_sequence": None,
                        "usage": {"input_tokens": 4, "cache_creation_input_tokens": 10517, "cache_read_input_tokens": 5432, "output_tokens": 5},
                    },
                    "parent_tool_use_id": None,
                    "session_id": "957fed31-2abe-4a54-9286-61e0e5afe4eb",
                    "uuid": "1fb1798d-e2b3-4d44-8be6-a142766ba3d7",
                }
            ),
            # Assistant tool use (Glob)
            json.dumps(
                {
                    "type": "assistant",
                    "message": {
                        "model": "claude-sonnet-4-5-20250929",
                        "id": "msg_01SmFbHj3bFcr7bXpD3PbpxZ",
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "tool_use", "id": "toolu_01RA2onbaBp3qD8G4G9wckma", "name": "Glob", "input": {"pattern": "README*"}}],
                        "stop_reason": None,
                        "stop_sequence": None,
                        "usage": {"input_tokens": 4, "cache_creation_input_tokens": 10517, "cache_read_input_tokens": 5432, "output_tokens": 79},
                    },
                    "parent_tool_use_id": None,
                    "session_id": "957fed31-2abe-4a54-9286-61e0e5afe4eb",
                    "uuid": "a319aaef-3cfd-4984-9e39-5bbc547cb097",
                }
            ),
            # Tool result
            json.dumps(
                {
                    "type": "user",
                    "message": {"role": "user", "content": [{"tool_use_id": "toolu_01RA2onbaBp3qD8G4G9wckma", "type": "tool_result", "content": "C:\\Users\\niteris\\dev\\clud2\\README.md"}]},
                    "parent_tool_use_id": None,
                    "session_id": "957fed31-2abe-4a54-9286-61e0e5afe4eb",
                    "uuid": "69828578-63f6-45fe-894f-4c86d8ce682d",
                }
            ),
            # Another assistant tool use (Read)
            json.dumps(
                {
                    "type": "assistant",
                    "message": {
                        "model": "claude-sonnet-4-5-20250929",
                        "id": "msg_01J6dAwcwAR3ewKnqULgqc47",
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "tool_use", "id": "toolu_01BXxGf4TxExFDFnqTBWbFAN", "name": "Read", "input": {"file_path": "C:\\Users\\niteris\\dev\\clud2\\README.md"}}],
                        "stop_reason": None,
                        "stop_sequence": None,
                        "usage": {"input_tokens": 7, "cache_creation_input_tokens": 105, "cache_read_input_tokens": 15949, "output_tokens": 69},
                    },
                    "parent_tool_use_id": None,
                    "session_id": "957fed31-2abe-4a54-9286-61e0e5afe4eb",
                    "uuid": "814b5bdf-b962-49d9-92db-7cdf0cc15f45",
                }
            ),
            # Tool result (truncated for demo)
            json.dumps(
                {
                    "type": "user",
                    "message": {
                        "role": "user",
                        "content": [
                            {
                                "tool_use_id": "toolu_01BXxGf4TxExFDFnqTBWbFAN",
                                "type": "tool_result",
                                "content": "     1→# clud\n     2→\n     3→Claude, but on god mode, by default.\n     4→\n     5→**A Python CLI that runs Claude Code...",
                            }
                        ],
                    },
                    "parent_tool_use_id": None,
                    "session_id": "957fed31-2abe-4a54-9286-61e0e5afe4eb",
                    "uuid": "dd39717a-0bca-4d85-800e-964e28af8a52",
                }
            ),
        ]

        # Create parser and UI
        parser = StreamingParser()
        output = io.StringIO()
        ui = StreamingUI(output=output, use_colors=False)  # Disable colors for test

        # Parse and render each line
        for line in json_lines:
            event = parser.parse_line(line)
            if event:
                ui.render_event(event)

        # Render session summary
        ui.render_session_summary(parser.session)

        # Get output
        result = output.getvalue()

        # Verify key elements are present
        self.assertIn("Session Started", result)
        self.assertIn("claude-sonnet-4-5-20250929", result)
        self.assertIn("Glob", result)
        self.assertIn("Read", result)
        self.assertIn("README*", result)
        self.assertIn("Token Usage", result)
        self.assertIn("Session Summary", result)

        # Print for visual inspection during development
        print("\n" + "=" * 70)
        print("STREAMING UI DEMO OUTPUT:")
        print("=" * 70)
        print(result)


if __name__ == "__main__":
    unittest.main()
