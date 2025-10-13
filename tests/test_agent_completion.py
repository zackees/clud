"""Integration tests for agent completion detection."""

import os
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
                    "uv",
                    "run",
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
                timeout=60,  # Allow time for process startup
            )

            elapsed_time = time.time() - start_time

            # Command should complete reasonably quickly (under 30 seconds including process startup)
            self.assertLess(elapsed_time, 30.0)

            # Should have some output
            self.assertTrue(result.stdout or result.stderr)

        except subprocess.TimeoutExpired:
            self.fail("Command took longer than 60 seconds - agent completion detection may not be working")

    def test_simple_command_without_detection(self):
        """Test that regular commands work without detection flag."""
        try:
            # Use encoding handling to deal with output that might contain binary data
            result = subprocess.run(
                ["uv", "run", "python", "-m", "clud.cli", ".", "--cmd", "echo 'hello world'"],
                capture_output=True,
                text=True,
                timeout=30,  # Increased timeout for process startup
                encoding="utf-8",
                errors="replace",  # Replace undecodable characters
            )
            # Command should now succeed with the fixed entrypoint behavior
            self.assertEqual(result.returncode, 0, f"Command failed with returncode {result.returncode}. stderr: {result.stderr or ''}")
            # Should have some output including the echo command result
            stdout_output = result.stdout or ""
            stderr_output = result.stderr or ""
            combined_output = stdout_output + stderr_output
            self.assertIn("hello world", combined_output, f"Expected 'hello world' in output. Got stdout length: {len(stdout_output)}, stderr length: {len(stderr_output)}")
        except subprocess.TimeoutExpired:
            self.fail("Command took longer than 30 seconds - this should complete quickly now")

    @unittest.skipUnless(os.getenv("RUN_API_TESTS") == "1", "Requires API key and RUN_API_TESTS=1")
    def test_foreground_idle_detection(self):
        """Test idle detection in foreground mode with Claude message."""
        start_time = time.time()

        try:
            result = subprocess.run(
                ["uv", "run", "clud", "-m", "respond with hi", "--idle-timeout", "10"],
                capture_output=True,
                text=True,
                timeout=30,  # Max timeout including Claude startup
                encoding="utf-8",
                errors="replace",
            )

            elapsed_time = time.time() - start_time

            # Should complete successfully
            self.assertEqual(result.returncode, 0, f"Command failed with returncode {result.returncode}")

            # Should complete within reasonable time (Claude startup + idle timeout + buffer)
            self.assertLess(elapsed_time, 30.0, f"Command took {elapsed_time}s, expected < 30s")

            # Should have some output from Claude
            combined_output = (result.stdout or "") + (result.stderr or "")
            self.assertTrue(len(combined_output) > 0, "Expected some output from Claude")

        except subprocess.TimeoutExpired:
            self.fail("Command timed out - idle detection may not be working")

    @unittest.skipUnless(os.getenv("RUN_API_TESTS") == "1", "Requires API key and RUN_API_TESTS=1")
    def test_foreground_without_idle_timeout(self):
        """Test that foreground mode without --idle-timeout preserves status quo behavior."""
        # This test just verifies that -p mode works normally without idle detection
        try:
            result = subprocess.run(
                ["uv", "run", "clud", "-p", "respond with just the word 'ok'"],
                capture_output=True,
                text=True,
                timeout=20,
                encoding="utf-8",
                errors="replace",
            )

            # Should complete successfully
            if result.returncode != 0:
                print(f"STDOUT: {result.stdout}")
                print(f"STDERR: {result.stderr}")
            self.assertEqual(result.returncode, 0, f"Command failed with returncode {result.returncode}")

            # Should have output
            combined_output = (result.stdout or "") + (result.stderr or "")
            self.assertTrue(len(combined_output) > 0, "Expected output from Claude")

        except subprocess.TimeoutExpired:
            self.fail("Command timed out - basic Claude functionality may be broken")


if __name__ == "__main__":
    unittest.main()
