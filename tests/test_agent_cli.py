"""Unit tests for agent_cli module."""

import unittest
from unittest.mock import patch

from clud.agent.command_builder import _wrap_command_for_git_bash


class TestWrapCommandForGitBash(unittest.TestCase):
    """Test _wrap_command_for_git_bash function."""

    def test_non_windows_returns_unchanged(self) -> None:
        """Test that non-Windows systems return command unchanged."""
        cmd = ["claude", "--dangerously-skip-permissions", "-p", "test"]

        # Mock platform.system() to return non-Windows
        with patch("clud.agent.command_builder.platform.system", return_value="Linux"):
            result = _wrap_command_for_git_bash(cmd)

        self.assertEqual(result, cmd)

    def test_windows_without_git_bash_returns_unchanged(self) -> None:
        """Test that Windows without git-bash returns command unchanged."""
        cmd = ["claude", "--dangerously-skip-permissions", "-p", "test"]

        # Mock platform.system() to return Windows, but detect_git_bash to return None
        with (
            patch("clud.agent.command_builder.platform.system", return_value="Windows"),
            patch("clud.agent.command_builder.detect_git_bash", return_value=None),
        ):
            result = _wrap_command_for_git_bash(cmd)

        self.assertEqual(result, cmd)

    def test_windows_with_git_bash_wraps_command(self) -> None:
        """Test that Windows with git-bash wraps command properly."""
        cmd = [
            r"C:\Users\user\.clud\npm\node_modules\.bin\claude.cmd",
            "--dangerously-skip-permissions",
            "-p",
            "test message",
        ]
        git_bash_path = r"C:\Program Files\Git\bin\bash.exe"

        # Mock platform.system() to return Windows, and detect_git_bash to return path
        with (
            patch("clud.agent.command_builder.platform.system", return_value="Windows"),
            patch("clud.agent.command_builder.detect_git_bash", return_value=git_bash_path),
        ):
            result = _wrap_command_for_git_bash(cmd)

        # Verify structure: [git_bash_path, "-c", "command string"]
        self.assertEqual(len(result), 3)
        self.assertEqual(result[0], git_bash_path)
        self.assertEqual(result[1], "-c")

        # Verify command string has forward slashes (bash-compatible paths)
        cmd_str = result[2]
        self.assertIn("'C:/Users/user/.clud/npm/node_modules/.bin/claude.cmd'", cmd_str)
        self.assertIn("'--dangerously-skip-permissions'", cmd_str)
        self.assertIn("'-p'", cmd_str)
        self.assertIn("'test message'", cmd_str)

        # Verify no backslashes in the command string (should be converted to forward slashes)
        self.assertNotIn("\\", cmd_str)

    def test_windows_paths_converted_to_forward_slashes(self) -> None:
        """Test that Windows paths are converted to forward slashes for bash."""
        cmd = [r"C:\path\to\file.exe", "--arg", r"D:\another\path\file.txt"]
        git_bash_path = r"C:\Program Files\Git\bin\bash.exe"

        with (
            patch("clud.agent.command_builder.platform.system", return_value="Windows"),
            patch("clud.agent.command_builder.detect_git_bash", return_value=git_bash_path),
        ):
            result = _wrap_command_for_git_bash(cmd)

        cmd_str = result[2]

        # Verify paths are converted to forward slashes
        self.assertIn("'C:/path/to/file.exe'", cmd_str)
        self.assertIn("'D:/another/path/file.txt'", cmd_str)

        # Verify no backslashes remain
        self.assertNotIn("\\", cmd_str)

    def test_single_quotes_escaped_properly(self) -> None:
        """Test that single quotes in arguments are escaped properly."""
        cmd = ["command", "--message", "It's a test with 'quotes'"]
        git_bash_path = r"C:\Program Files\Git\bin\bash.exe"

        with (
            patch("clud.agent.command_builder.platform.system", return_value="Windows"),
            patch("clud.agent.command_builder.detect_git_bash", return_value=git_bash_path),
        ):
            result = _wrap_command_for_git_bash(cmd)

        cmd_str = result[2]

        # Single quotes should be escaped as '\''
        self.assertIn("'It'\\''s a test with '\\''quotes'\\'''", cmd_str)

    def test_urls_in_prompt_preserved(self) -> None:
        """Test that URLs in prompts are NOT converted to Windows paths.

        This tests the MSYS path conversion issue where:
        - /gh-debug -> C:\\Program Files\\Git\\gh-debug
        - https://github.com/... -> https;\\\\github.com\\...

        The fix is to set MSYS_NO_PATHCONV=1 in the environment.
        """
        # This prompt contains a URL that should NOT be path-converted
        prompt = "/gh-debug https://github.com/FastLED/FastLED/actions/runs/123"
        cmd = [
            r"C:\Users\user\.clud\npm\node_modules\.bin\claude.cmd",
            "--dangerously-skip-permissions",
            "-p",
            prompt,
        ]
        git_bash_path = r"C:\Program Files\Git\bin\bash.exe"

        with (
            patch("clud.agent.command_builder.platform.system", return_value="Windows"),
            patch("clud.agent.command_builder.detect_git_bash", return_value=git_bash_path),
        ):
            result = _wrap_command_for_git_bash(cmd)

        cmd_str = result[2]

        # Verify the URL is preserved with forward slashes (not converted to backslashes)
        # The prompt should contain the original URL structure
        self.assertIn("https://github.com/FastLED/FastLED/actions/runs/123", cmd_str)
        self.assertIn("/gh-debug", cmd_str)

        # Verify the URL was NOT converted to Windows path format
        self.assertNotIn("https;\\\\", cmd_str)  # MSYS converts :// to ;\\
        self.assertNotIn("github.com\\", cmd_str)  # Forward slashes should remain


if __name__ == "__main__":
    unittest.main()
