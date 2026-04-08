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

    def test_parse_resume_sets_resume_flag(self) -> None:
        """`--resume` should be parsed as a known resume command, not continue-last."""
        args = parse_args(["--resume"])
        self.assertFalse(args.continue_flag)
        self.assertTrue(args.resume_flag)
        self.assertEqual(args.resume_value, "")
        self.assertEqual(args.claude_args, [])

    def test_parse_resume_with_codex_does_not_passthrough(self) -> None:
        """`--resume --codex` should not leak `--resume` into passthrough args."""
        args = parse_args(["--resume", "--codex"])
        self.assertFalse(args.continue_flag)
        self.assertTrue(args.resume_flag)
        self.assertEqual(args.resume_value, "")
        self.assertEqual(args.agent_backend, "codex")
        self.assertEqual(args.claude_args, [])

    def test_parse_resume_value(self) -> None:
        """`--resume VALUE` should capture the picker/session term as a known arg."""
        args = parse_args(["--resume", "search-term"])
        self.assertTrue(args.resume_flag)
        self.assertEqual(args.resume_value, "search-term")
        self.assertEqual(args.claude_args, [])

    def test_parse_continue_and_resume_conflict_raises(self) -> None:
        """Distinct resume modes should not be allowed together."""
        with self.assertRaises(ValueError):
            parse_args(["--continue", "--resume"])


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

    def test_build_codex_continue_command(self) -> None:
        """Codex continue should use the native `resume --last` command shape."""
        args = parse_args(["--continue", "--codex"])
        cmd = _build_claude_command(args, "codex")
        self.assertEqual(
            cmd,
            [
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "-C",
                "C:\\Users\\niteris\\dev\\clud",
                "resume",
                "--last",
            ],
        )

    def test_build_codex_continue_with_prompt_and_passthrough(self) -> None:
        """Codex `-c` should normalize to resume-last while preserving flags and prompt."""
        args = parse_args(["--continue", "--codex", "-p", "keep going", "--model", "gpt-5.4"])
        cmd = _build_claude_command(args, "codex")
        self.assertEqual(
            cmd,
            [
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "-C",
                "C:\\Users\\niteris\\dev\\clud",
                "resume",
                "--last",
                "--model",
                "gpt-5.4",
                "keep going",
            ],
        )

    def test_build_codex_resume_picker_command(self) -> None:
        """Codex resume without a value should open the native picker."""
        args = parse_args(["--resume", "--codex"])
        cmd = _build_claude_command(args, "codex")
        self.assertEqual(
            cmd,
            [
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "-C",
                "C:\\Users\\niteris\\dev\\clud",
                "resume",
            ],
        )

    def test_build_codex_resume_with_value_command(self) -> None:
        """Codex resume should pass a specific session/search term through."""
        args = parse_args(["--resume", "search-term", "--codex"])
        cmd = _build_claude_command(args, "codex")
        self.assertEqual(
            cmd,
            [
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "-C",
                "C:\\Users\\niteris\\dev\\clud",
                "resume",
                "search-term",
            ],
        )


class TestCodexDryRun(unittest.TestCase):
    """Test dry-run output for Codex backend."""

    def test_codex_saved_backend_continue_dry_run_uses_resume_last(self) -> None:
        """`clud -c` should resume Codex when Codex is the saved backend."""
        args = parse_args(["-c", "--dry-run"])

        captured_output = StringIO()
        with patch("clud.agent.command_builder.get_agent_backend", return_value="codex"), patch("sys.stdout", captured_output):
            result = run_agent(args)

        self.assertEqual(result, 0)
        output = captured_output.getvalue().strip()
        self.assertIn("Would execute: codex", output)
        self.assertIn("resume --last", output)
        self.assertNotIn(" exec ", output)

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
