"""Integration and unit tests for agent completion detection."""

import os
import subprocess
import sys
import time
import unittest


class TestAgentCompletionIntegration(unittest.TestCase):
    """Test agent completion detection with real commands."""

    def test_agent_completion_timeout(self) -> None:
        """Test that agent completion detection works with real clud command."""
        # This should timeout quickly since "respond with 0" should complete fast
        start_time = time.time()

        try:
            result = subprocess.run(
                [
                    "uv",
                    "run",
                    "--no-sync",
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

    def test_simple_command_without_detection(self) -> None:
        """Test that regular commands work without detection flag."""
        try:
            # Use encoding handling to deal with output that might contain binary data
            result = subprocess.run(
                ["uv", "run", "--no-sync", "python", "-m", "clud.cli", ".", "--cmd", "echo 'hello world'"],
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
    def test_foreground_idle_detection(self) -> None:
        """Test idle detection in foreground mode with Claude message."""
        start_time = time.time()

        try:
            result = subprocess.run(
                ["uv", "run", "--no-sync", "clud", "-m", "respond with hi", "--idle-timeout", "10"],
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
    def test_foreground_without_idle_timeout(self) -> None:
        """Test that foreground mode without --idle-timeout preserves status quo behavior."""
        # This test just verifies that -p mode works normally without idle detection
        try:
            result = subprocess.run(
                ["uv", "run", "--no-sync", "clud", "-p", "respond with just the word 'ok'"],
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


class TestAgentCompletionCapacityRetry(unittest.TestCase):
    """Focused tests for Codex capacity retry handling."""

    def test_capacity_retry_controller_waits_for_idle_then_sends_continue(self) -> None:
        """The retry controller should defer recovery input until the PTY is quiet."""
        from clud.agent.completion import _CapacityRetryController

        controller = _CapacityRetryController(idle_timeout=3.0)
        sent: list[str] = []

        controller.observe_output("prefix\n⚠ Selected model is at capacity. Please try a different model.\n")

        self.assertTrue(controller.pending_retry)
        self.assertIsNone(controller.maybe_retry(10.0, True, lambda: sent.append("continue"), now=12.0))
        self.assertEqual(sent, [])

        retry_time = controller.maybe_retry(10.0, True, lambda: sent.append("continue"), now=13.5)

        self.assertEqual(retry_time, 13.5)
        self.assertEqual(sent, ["continue"])
        self.assertFalse(controller.pending_retry)
        self.assertEqual(controller.retry_count, 1)

    def test_fallback_detection_injects_continue_after_capacity_marker(self) -> None:
        """Fallback subprocess mode should recover from the Codex capacity marker."""
        from clud.agent.completion import _fallback_subprocess_detection

        script = (
            "import sys\nprint(chr(9888) + ' Selected model is at capacity. Please try a different model.', flush=True)\nline = sys.stdin.readline().strip()\nprint('received:' + line, flush=True)\n"
        )
        command = [sys.executable, "-c", script]
        output: list[str] = []

        result = _fallback_subprocess_detection(command, idle_timeout=0.2, output_callback=output.append)

        self.assertFalse(result.idle_detected)
        self.assertEqual(result.returncode, 0)
        combined_output = "".join(output)
        self.assertIn("Selected model is at capacity", combined_output)
        self.assertIn("received:continue", combined_output)


class TestAgentCompletionIdleGating(unittest.TestCase):
    """Idle shutdown should only happen after real agent activity."""

    def test_fallback_detection_does_not_idle_terminate_before_any_meaningful_output(self) -> None:
        """A silent startup period should not be mistaken for a completed Codex turn."""
        from clud.agent.completion import _fallback_subprocess_detection

        script = "import time; time.sleep(0.3)"
        command = [sys.executable, "-c", script]

        result = _fallback_subprocess_detection(command, idle_timeout=0.1, output_callback=None)

        self.assertFalse(result.idle_detected)
        self.assertEqual(result.returncode, 0)

    def test_fallback_detection_can_idle_terminate_after_meaningful_output(self) -> None:
        """Once the agent has emitted real output, quiet time can count as stop."""
        from clud.agent.completion import _fallback_subprocess_detection

        script = "import time; print('ready', flush=True); time.sleep(0.3)"
        command = [sys.executable, "-c", script]
        output: list[str] = []

        result = _fallback_subprocess_detection(command, idle_timeout=0.1, output_callback=output.append)

        self.assertTrue(result.idle_detected)
        self.assertEqual(result.returncode, 0)
        self.assertIn("ready", "".join(output))


if __name__ == "__main__":
    unittest.main()
