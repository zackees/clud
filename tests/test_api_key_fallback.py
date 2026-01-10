"""Unit tests for API key fallback behavior in runner module."""

import os
import unittest
from unittest.mock import MagicMock, patch

from clud.agent_args import AgentMode, Args


class TestApiKeyFallback(unittest.TestCase):
    """Test API key fallback logic when claude -p command fails."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        # Create minimal Args object for testing
        self.args = Args(
            prompt="say hello world",
            message=None,
            claude_args=[],
            dry_run=False,
            cmd=None,
            continue_flag=False,
            verbose=False,
            plain=False,
            mode=AgentMode.DEFAULT,
            help=False,
            login=False,
            task=None,
            lint=False,
            test=False,
            kanban=False,
            telegram_web=False,
            telegram_token=None,
            telegram_server=False,
            telegram_server_port=8889,
            telegram_server_config=None,
            webui=False,
            webui_port=8888,
            api_server=False,
            api_port=8765,
            code=False,
            code_port=8080,
            fix=False,
            fix_url=None,
            init_loop=False,
            info=False,
            install_claude=False,
            cron=False,
            cron_subcommand=None,
            cron_args=[],
            track=False,
            up_publish=False,
            telegram=False,
            telegram_bot_token=None,
            telegram_chat_id=None,
            hook_debug=False,
            loop_value=None,
            idle_timeout=None,
        )

    @patch("clud.agent.runner._find_claude_path")
    @patch("clud.agent.runner.get_api_key")
    @patch("clud.agent.runner.RunningProcess.run_streaming")
    @patch("clud.agent.runner._build_claude_command")
    @patch("clud.agent.runner._wrap_command_for_git_bash")
    @patch("clud.agent.runner.register_hooks_from_config")
    @patch("clud.agent.runner.TelegramBot.from_args")
    @patch("clud.agent.runner.trigger_hook_sync")
    @patch("clud.agent.runner.load_telegram_credentials")
    @patch("sys.stdin.isatty", return_value=True)
    def test_retry_without_api_key_on_failure(
        self,
        mock_isatty: MagicMock,
        mock_load_telegram: MagicMock,
        mock_trigger_hook: MagicMock,
        mock_telegram_bot: MagicMock,
        mock_register_hooks: MagicMock,
        mock_wrap_command: MagicMock,
        mock_build_command: MagicMock,
        mock_run_streaming: MagicMock,
        mock_get_api_key: MagicMock,
        mock_find_claude: MagicMock,
    ) -> None:
        """Test that command retries without API key when first attempt fails."""
        from clud.agent.runner import run_agent

        # Setup mocks
        mock_find_claude.return_value = "/usr/bin/claude"
        mock_get_api_key.return_value = "sk-invalid-key"
        mock_build_command.return_value = ["claude", "--dangerously-skip-permissions", "-p", "say hello world"]
        mock_wrap_command.side_effect = lambda x: x  # type: ignore[misc]  # Return command unchanged
        mock_telegram_bot.return_value = None  # No telegram bot
        mock_load_telegram.return_value = (None, None)  # No saved credentials

        # First call fails (invalid API key), second call succeeds
        mock_run_streaming.side_effect = [1, 0]

        # Execute
        result = run_agent(self.args)

        # Verify behavior
        self.assertEqual(result, 0, "Should return success after retry")
        self.assertEqual(mock_run_streaming.call_count, 2, "Should call run_streaming twice (initial + retry)")

        # Verify API key was set initially
        self.assertIn("ANTHROPIC_API_KEY", os.environ)

        # Note: We can't easily verify that the API key was removed during retry
        # because the runner restores it in the finally block

    @patch("clud.agent.runner._find_claude_path")
    @patch("clud.agent.runner.get_api_key")
    @patch("clud.agent.runner.RunningProcess.run_streaming")
    @patch("clud.agent.runner._build_claude_command")
    @patch("clud.agent.runner._wrap_command_for_git_bash")
    @patch("clud.agent.runner.register_hooks_from_config")
    @patch("clud.agent.runner.TelegramBot.from_args")
    @patch("clud.agent.runner.trigger_hook_sync")
    @patch("clud.agent.runner.load_telegram_credentials")
    @patch("sys.stdin.isatty", return_value=True)
    def test_no_retry_when_first_attempt_succeeds(
        self,
        mock_isatty: MagicMock,
        mock_load_telegram: MagicMock,
        mock_trigger_hook: MagicMock,
        mock_telegram_bot: MagicMock,
        mock_register_hooks: MagicMock,
        mock_wrap_command: MagicMock,
        mock_build_command: MagicMock,
        mock_run_streaming: MagicMock,
        mock_get_api_key: MagicMock,
        mock_find_claude: MagicMock,
    ) -> None:
        """Test that command does not retry when first attempt succeeds."""
        from clud.agent.runner import run_agent

        # Setup mocks
        mock_find_claude.return_value = "/usr/bin/claude"
        mock_get_api_key.return_value = "sk-valid-key"
        mock_build_command.return_value = ["claude", "--dangerously-skip-permissions", "-p", "say hello world"]
        mock_wrap_command.side_effect = lambda x: x  # type: ignore[misc]
        mock_telegram_bot.return_value = None
        mock_load_telegram.return_value = (None, None)

        # First call succeeds
        mock_run_streaming.return_value = 0

        # Execute
        result = run_agent(self.args)

        # Verify behavior
        self.assertEqual(result, 0, "Should return success")
        self.assertEqual(mock_run_streaming.call_count, 1, "Should only call run_streaming once")

    @patch("clud.agent.runner._find_claude_path")
    @patch("clud.agent.runner.get_api_key")
    @patch("clud.agent.runner.RunningProcess.run_streaming")
    @patch("clud.agent.runner._build_claude_command")
    @patch("clud.agent.runner._wrap_command_for_git_bash")
    @patch("clud.agent.runner.register_hooks_from_config")
    @patch("clud.agent.runner.TelegramBot.from_args")
    @patch("clud.agent.runner.trigger_hook_sync")
    @patch("clud.agent.runner.load_telegram_credentials")
    @patch("sys.stdin.isatty", return_value=True)
    def test_return_error_when_both_attempts_fail(
        self,
        mock_isatty: MagicMock,
        mock_load_telegram: MagicMock,
        mock_trigger_hook: MagicMock,
        mock_telegram_bot: MagicMock,
        mock_register_hooks: MagicMock,
        mock_wrap_command: MagicMock,
        mock_build_command: MagicMock,
        mock_run_streaming: MagicMock,
        mock_get_api_key: MagicMock,
        mock_find_claude: MagicMock,
    ) -> None:
        """Test that command returns error when both attempts fail."""
        from clud.agent.runner import run_agent

        # Setup mocks
        mock_find_claude.return_value = "/usr/bin/claude"
        mock_get_api_key.return_value = "sk-invalid-key"
        mock_build_command.return_value = ["claude", "--dangerously-skip-permissions", "-p", "say hello world"]
        mock_wrap_command.side_effect = lambda x: x  # type: ignore[misc]
        mock_telegram_bot.return_value = None
        mock_load_telegram.return_value = (None, None)

        # Both calls fail
        mock_run_streaming.side_effect = [1, 1]

        # Execute
        result = run_agent(self.args)

        # Verify behavior
        self.assertEqual(result, 1, "Should return error when both attempts fail")
        self.assertEqual(mock_run_streaming.call_count, 2, "Should call run_streaming twice")


if __name__ == "__main__":
    unittest.main()
