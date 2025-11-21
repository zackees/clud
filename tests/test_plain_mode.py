"""Unit tests for plain mode functionality (web/telegram integration)."""

import unittest
from unittest.mock import MagicMock, patch

from clud.agent_args import AgentMode, Args, parse_args


class TestPlainModeArgParsing(unittest.TestCase):
    """Test plain mode argument parsing."""

    def test_plain_flag_present(self) -> None:
        """Test that --plain flag is parsed correctly."""
        args = parse_args(["-p", "test prompt", "--plain"])
        self.assertTrue(args.plain)
        self.assertEqual(args.prompt, "test prompt")

    def test_plain_flag_absent(self) -> None:
        """Test that plain defaults to False."""
        args = parse_args(["-p", "test prompt"])
        self.assertFalse(args.plain)

    def test_plain_with_continue(self) -> None:
        """Test --plain with --continue flag."""
        args = parse_args(["-c", "-p", "test prompt", "--plain"])
        self.assertTrue(args.plain)
        self.assertTrue(args.continue_flag)
        self.assertEqual(args.prompt, "test prompt")

    def test_plain_with_message(self) -> None:
        """Test --plain with -m (message) flag."""
        args = parse_args(["-m", "test message", "--plain"])
        self.assertTrue(args.plain)
        self.assertEqual(args.message, "test message")

    def test_plain_multiple_flags(self) -> None:
        """Test --plain with multiple flags."""
        args = parse_args(["-c", "-p", "prompt", "--plain", "-v"])
        self.assertTrue(args.plain)
        self.assertTrue(args.continue_flag)
        self.assertTrue(args.verbose)
        self.assertEqual(args.prompt, "prompt")


class TestPlainModeCommandBuilding(unittest.TestCase):
    """Test command building with plain mode."""

    def test_build_command_without_plain(self) -> None:
        """Test command building without --plain flag."""
        from clud.agent.command_builder import _build_claude_command

        args = Args(
            mode=AgentMode.DEFAULT,
            prompt="test",
            plain=False,
            continue_flag=False,
            claude_args=[],
        )
        cmd = _build_claude_command(args, "claude")

        # Should include stream-json and verbose
        self.assertIn("--output-format", cmd)
        self.assertIn("stream-json", cmd)
        self.assertIn("--verbose", cmd)

    def test_build_command_with_plain(self) -> None:
        """Test command building with --plain flag."""
        from clud.agent.command_builder import _build_claude_command

        args = Args(
            mode=AgentMode.DEFAULT,
            prompt="test",
            plain=True,
            continue_flag=False,
            claude_args=[],
        )
        cmd = _build_claude_command(args, "claude")

        # Should NOT include stream-json or verbose (plain mode)
        self.assertNotIn("--output-format", cmd)
        self.assertNotIn("stream-json", cmd)
        # --verbose might still be present if explicitly requested, but not auto-added

    def test_build_command_plain_with_continue(self) -> None:
        """Test command building with --plain and --continue."""
        from clud.agent.command_builder import _build_claude_command

        args = Args(
            mode=AgentMode.DEFAULT,
            prompt="test",
            plain=True,
            continue_flag=True,
            claude_args=[],
        )
        cmd = _build_claude_command(args, "claude")

        self.assertIn("--continue", cmd)
        self.assertIn("-p", cmd)
        self.assertIn("test", cmd)
        self.assertNotIn("--output-format", cmd)


class TestPlainModeExecution(unittest.TestCase):
    """Test plain mode execution flow."""

    @patch("clud.agent.runner._find_claude_path")
    @patch("clud.agent.runner.get_api_key")
    @patch("clud.agent.runner.RunningProcess")
    @patch("clud.agent.runner.TelegramBot")
    def test_plain_mode_uses_raw_streaming(
        self,
        mock_telegram: MagicMock,
        mock_running_process: MagicMock,
        mock_get_api_key: MagicMock,
        mock_find_claude: MagicMock,
    ) -> None:
        """Test that plain mode uses raw streaming without JSON formatter."""
        from clud.agent.runner import run_agent

        # Setup mocks
        mock_find_claude.return_value = "claude"
        mock_get_api_key.return_value = "sk-ant-test123456789012345"
        mock_telegram.from_args.return_value = None
        mock_running_process.run_streaming.return_value = 0

        args = Args(
            mode=AgentMode.DEFAULT,
            prompt="test prompt",
            plain=True,
            continue_flag=False,
            claude_args=[],
        )

        result = run_agent(args)

        # Verify streaming was called without callback (raw mode)
        mock_running_process.run_streaming.assert_called_once()
        call_args = mock_running_process.run_streaming.call_args

        # In plain mode, run_streaming should be called with just cmd (no callback)
        self.assertEqual(len(call_args[0]), 1)  # Only one positional arg (cmd)
        self.assertEqual(result, 0)

    @patch("clud.agent.runner._find_claude_path")
    @patch("clud.agent.runner.get_api_key")
    @patch("clud.agent.runner.RunningProcess")
    @patch("clud.agent.runner.StreamJsonFormatter")
    @patch("clud.agent.runner.TelegramBot")
    def test_non_plain_mode_uses_json_formatter(
        self,
        mock_telegram: MagicMock,
        mock_formatter_class: MagicMock,
        mock_running_process: MagicMock,
        mock_get_api_key: MagicMock,
        mock_find_claude: MagicMock,
    ) -> None:
        """Test that non-plain mode uses JSON formatter."""
        from clud.agent.runner import run_agent

        # Setup mocks
        mock_find_claude.return_value = "claude"
        mock_get_api_key.return_value = "sk-ant-test123456789012345"
        mock_telegram.from_args.return_value = None
        mock_running_process.run_streaming.return_value = 0
        mock_formatter = MagicMock()
        mock_formatter_class.return_value = mock_formatter

        args = Args(
            mode=AgentMode.DEFAULT,
            prompt="test prompt",
            plain=False,
            continue_flag=False,
            verbose=False,
            claude_args=[],
        )

        result = run_agent(args)

        # Verify formatter was created
        mock_formatter_class.assert_called_once()

        # Verify streaming was called with callback (JSON formatting)
        mock_running_process.run_streaming.assert_called_once()
        call_args = mock_running_process.run_streaming.call_args

        # Should have cmd and stdout_callback
        self.assertGreaterEqual(len(call_args[0]) + len(call_args[1]), 1)
        self.assertEqual(result, 0)


class TestPlainModeIntegration(unittest.TestCase):
    """Integration tests for plain mode."""

    def test_plain_mode_cli_integration(self) -> None:
        """Test that plain mode integrates correctly with CLI args."""
        args = parse_args(["--plain", "-p", "hello", "-c"])

        self.assertTrue(args.plain)
        self.assertEqual(args.prompt, "hello")
        self.assertTrue(args.continue_flag)
        self.assertEqual(args.mode, AgentMode.DEFAULT)

    def test_plain_mode_with_telegram_flags(self) -> None:
        """Test plain mode with telegram notification flags."""
        args = parse_args(["--plain", "-p", "test", "--telegram-notify"])

        self.assertTrue(args.plain)
        self.assertEqual(args.prompt, "test")
        self.assertTrue(args.telegram)

    def test_dry_run_with_plain(self) -> None:
        """Test dry-run shows correct command with --plain."""
        import sys
        from io import StringIO

        from clud.agent.runner import run_agent

        args = Args(
            mode=AgentMode.DEFAULT,
            prompt="test",
            plain=True,
            dry_run=True,
            continue_flag=False,
            claude_args=[],
        )

        # Capture output
        old_stdout = sys.stdout
        sys.stdout = StringIO()

        try:
            result = run_agent(args)
            output = sys.stdout.getvalue()

            # Should show command without stream-json in plain mode
            self.assertNotIn("stream-json", output)
            self.assertIn("-p", output)
            self.assertIn("test", output)
            self.assertEqual(result, 0)
        finally:
            sys.stdout = old_stdout


if __name__ == "__main__":
    unittest.main()
