"""Integration tests for agent completion detection."""

import subprocess
import time
import unittest


class TestAgentCompletionIntegration(unittest.TestCase):
    """Test agent completion detection with real commands."""

    def test_agent_completion_timeout(self):
        """Test that agent completion detection works with real clud command."""
        # This should timeout quickly since "respond with 0" should complete fast
        start_time = time.time()

        try:
            result = subprocess.run(
                [
                    "python",
                    "-m",
                    "clud.cli",
                    ".",
                    "--cmd",
                    "echo '0'",  # Simple command that outputs and exits
                    "--detect-completion",
                    "--idle-timeout",
                    "2.0",
                ],
                capture_output=True,
                text=True,
                timeout=10,  # Should complete well under 10 seconds
            )

            elapsed_time = time.time() - start_time

            # Command should complete quickly (under 5 seconds for a simple echo)
            self.assertLess(elapsed_time, 5.0)

            # Should have some output
            self.assertTrue(result.stdout or result.stderr)

        except subprocess.TimeoutExpired:
            self.fail("Command took longer than 10 seconds - agent completion detection may not be working")

    def test_simple_command_without_detection(self):
        """Test that regular commands work without detection flag."""
        result = subprocess.run(["python", "-m", "clud.cli", ".", "--cmd", "echo 'hello world'"], capture_output=True, text=True, timeout=10)

        # Should complete successfully
        self.assertEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
