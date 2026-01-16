"""Unit tests for command_builder._print_launch_banner behavior."""

import io
import sys
import unittest

from clud.agent.command_builder import _print_launch_banner


class TestLaunchBannerDisplay(unittest.TestCase):
    """Test cases for environment variable display in launch banner."""

    def test_does_not_mask_max_output_tokens(self) -> None:
        """Test that CLAUDE_CODE_MAX_OUTPUT_TOKENS is NOT masked in the banner."""
        # Redirect stderr to capture output
        captured_output = io.StringIO()
        old_stderr = sys.stderr
        sys.stderr = captured_output

        try:
            test_env = {
                "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "64000",
            }

            _print_launch_banner(
                cmd=["claude", "-p", "test"],
                cwd="/test",
                env_vars=test_env,
            )

            output = captured_output.getvalue()

            # Max output tokens should NOT be masked
            self.assertIn("CLAUDE_CODE_MAX_OUTPUT_TOKENS=64000", output)
            self.assertNotIn("CLAUDE_CODE_MAX_OUTPUT_TOKENS=****", output)

        finally:
            sys.stderr = old_stderr


if __name__ == "__main__":
    unittest.main()
