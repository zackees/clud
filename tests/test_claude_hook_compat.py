"""Tests for Claude hook compatibility support."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from clud.agent.hooks import register_hooks_from_config
from clud.hooks import reset_hook_manager
from clud.hooks.claude_compat import load_claude_compat_hooks


class TestClaudeCompatHookLoading(unittest.TestCase):
    """Tests for parsing Claude-style hook settings."""

    def test_loads_nested_stop_and_session_end_commands(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            claude_dir = root / ".claude"
            claude_dir.mkdir()
            (claude_dir / "settings.local.json").write_text(
                """
                {
                  "hooks": {
                    "Stop": [
                      {
                        "matcher": "*",
                        "hooks": [
                          {"type": "command", "command": "bash lint"},
                          {"type": "command", "command": "bash test"}
                        ]
                      }
                    ],
                    "SessionEnd": {
                      "hooks": [
                        {"type": "command", "command": "echo session-end"}
                      ]
                    }
                  }
                }
                """,
                encoding="utf-8",
            )

            hooks = load_claude_compat_hooks(root)

        self.assertEqual([spec.command for spec in hooks.stop], ["bash lint", "bash test"])
        self.assertEqual([spec.command for spec in hooks.session_end], ["echo session-end"])

    def test_register_hooks_reports_summary(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            claude_dir = root / ".claude"
            claude_dir.mkdir()
            (claude_dir / "settings.local.json").write_text(
                """
                {
                  "hooks": {
                    "Stop": [{"hooks": [{"type": "command", "command": "bash lint"}]}],
                    "SessionEnd": [{"hooks": [{"type": "command", "command": "bash test"}]}]
                  }
                }
                """,
                encoding="utf-8",
            )

            reset_hook_manager()
            try:
                summary = register_hooks_from_config(cwd=root)
            finally:
                reset_hook_manager()

        self.assertTrue(summary.has_stop_hooks)
        self.assertTrue(summary.has_session_end_hooks)

    def test_loads_settings_with_comments_and_trailing_commas(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            claude_dir = root / ".claude"
            claude_dir.mkdir()
            (claude_dir / "settings.local.json").write_text(
                """
                {
                  // line comment
                  "hooks": {
                    "Stop": [
                      {
                        "hooks": [
                          {"type": "command", "command": "bash lint"},
                        ],
                      },
                    ],
                    "SessionEnd": [
                      {
                        "hooks": [
                          {"type": "command", "command": "bash test"},
                        ],
                      },
                    ],
                  },
                }
                """,
                encoding="utf-8",
            )

            hooks = load_claude_compat_hooks(root)

        self.assertEqual([spec.command for spec in hooks.stop], ["bash lint"])
        self.assertEqual([spec.command for spec in hooks.session_end], ["bash test"])


if __name__ == "__main__":
    unittest.main()
