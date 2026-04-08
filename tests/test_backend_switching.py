"""Tests for backend switching between Claude and Codex."""

import unittest
from io import StringIO
from unittest.mock import patch

from clud.agent.command_builder import _build_claude_command, _get_effective_backend
from clud.agent.runner import run_agent
from clud.agent_args import AgentMode, Args, parse_args


class TestBackendArgParsing(unittest.TestCase):
    """Test CLI parsing for backend switches."""

    def test_parse_codex_global_flag(self) -> None:
        """`--codex` sets the persistent backend flag."""
        args = parse_args(["--codex"])
        self.assertEqual(args.agent_backend, "codex")

    def test_parse_claude_global_flag(self) -> None:
        """`--claude` sets the persistent backend flag."""
        args = parse_args(["--claude"])
        self.assertEqual(args.agent_backend, "claude")

    def test_parse_session_model_equals_syntax(self) -> None:
        """`--session-model=codex` sets a one-run backend override."""
        args = parse_args(["--session-model=codex"])
        self.assertEqual(args.session_model, "codex")

    def test_parse_invalid_session_model_raises(self) -> None:
        """Invalid backend values should fail fast."""
        with self.assertRaises(ValueError):
            parse_args(["--session-model=invalid"])

    def test_parse_conflicting_backend_flags_raises(self) -> None:
        """Conflicting global backend switches should fail fast."""
        with self.assertRaises(ValueError):
            parse_args(["--codex", "--claude"])


class TestBackendResolution(unittest.TestCase):
    """Test effective backend resolution order."""

    def test_session_override_wins(self) -> None:
        """Session override should beat saved backend."""
        args = Args(mode=AgentMode.DEFAULT, session_model="codex")
        with patch("clud.agent.command_builder.get_agent_backend", return_value="claude"):
            self.assertEqual(_get_effective_backend(args), "codex")

    def test_explicit_global_flag_beats_saved_backend(self) -> None:
        """Current CLI global switch should beat saved backend for this run."""
        args = Args(mode=AgentMode.DEFAULT, agent_backend="codex")
        with patch("clud.agent.command_builder.get_agent_backend", return_value="claude"):
            self.assertEqual(_get_effective_backend(args), "codex")

    def test_saved_backend_used_when_no_override(self) -> None:
        """Saved backend should be used when there is no override."""
        args = Args(mode=AgentMode.DEFAULT)
        with patch("clud.agent.command_builder.get_agent_backend", return_value="codex"):
            self.assertEqual(_get_effective_backend(args), "codex")


class TestCodexCommandBuilding(unittest.TestCase):
    """Test Codex command construction."""

    def test_build_codex_prompt_command(self) -> None:
        """Codex prompt mode should use `codex exec` with dangerous flags."""
        args = Args(
            mode=AgentMode.DEFAULT,
            prompt="test prompt",
            agent_backend="codex",
            claude_args=["--model", "gpt-5.4"],
        )
        cmd = _build_claude_command(args, "codex")
        self.assertEqual(cmd[0], "codex")
        self.assertIn("--dangerously-bypass-approvals-and-sandbox", cmd)
        self.assertNotIn("--full-auto", cmd)
        self.assertIn("exec", cmd)
        self.assertIn("test prompt", cmd)
        self.assertIn("--model", cmd)
        self.assertIn("gpt-5.4", cmd)


class TestCodexDryRun(unittest.TestCase):
    """Test dry-run output for Codex backend."""

    def test_codex_dry_run_prints_codex_command(self) -> None:
        """Dry-run should reflect the Codex backend command."""
        args = Args(
            mode=AgentMode.DEFAULT,
            prompt="say hello",
            dry_run=True,
            agent_backend="codex",
            claude_args=["--model", "gpt-5.4"],
        )

        captured_output = StringIO()
        with patch("sys.stdout", captured_output):
            result = run_agent(args)

        self.assertEqual(result, 0)
        output = captured_output.getvalue().strip()
        self.assertIn("Would execute: codex", output)
        self.assertIn("--dangerously-bypass-approvals-and-sandbox", output)
        self.assertNotIn("--full-auto", output)
        self.assertIn("exec", output)


if __name__ == "__main__":
    unittest.main()
