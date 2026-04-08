"""Tests for Codex app-server post-edit hook emulation."""

from __future__ import annotations

import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from clud.codex_app_server.post_edit import CodexAppServerSessionState
from clud.hooks.command import CommandHookSpec


class TestCodexAppServerPostEdit(unittest.TestCase):
    """Verify write detection and post-edit hook behavior."""

    def test_complete_turn_without_write_request_returns_none(self) -> None:
        state = CodexAppServerSessionState()
        state.observe_turn_diff_updated("thread-1", "turn-1", "")

        outcome = state.complete_turn(
            "thread-1",
            "turn-1",
            cwd=Path.cwd(),
            hook_specs=[],
        )

        self.assertIsNone(outcome)

    def test_complete_turn_with_write_request_and_diff_runs_no_hooks(self) -> None:
        state = CodexAppServerSessionState()
        state.observe_file_change_request("thread-1", "turn-1")
        state.observe_turn_diff_updated("thread-1", "turn-1", "diff --git a/foo.py b/foo.py\n")

        outcome = state.complete_turn(
            "thread-1",
            "turn-1",
            cwd=Path.cwd(),
            hook_specs=[],
        )

        self.assertIsNotNone(outcome)
        assert outcome is not None
        self.assertEqual(outcome.hook_count, 0)
        self.assertFalse(outcome.has_failures)
        self.assertIn("foo.py", outcome.diff)

    def test_complete_turn_writes_failure_artifact_for_failed_hook(self) -> None:
        state = CodexAppServerSessionState()
        state.observe_file_change_request("thread-1", "turn-1")
        state.observe_turn_diff_updated("thread-1", "turn-1", "diff --git a/foo.py b/foo.py\n")

        with TemporaryDirectory() as tmp:
            outcome = state.complete_turn(
                "thread-1",
                "turn-1",
                cwd=Path(tmp),
                hook_specs=[
                    CommandHookSpec(
                        event_name="PostEdit",
                        command="python -c \"import sys; print('lint failed'); print('stderr failed', file=sys.stderr); sys.exit(5)\"",
                    )
                ],
            )

            self.assertIsNotNone(outcome)
            assert outcome is not None
            self.assertTrue(outcome.has_failures)
            self.assertEqual(len(outcome.failures), 1)
            failure = outcome.failures[0]
            self.assertEqual(failure.result.returncode, 5)
            self.assertTrue(failure.artifact_path.exists())
            content = failure.artifact_path.read_text(encoding="utf-8")
            self.assertIn("lint failed", content)
            self.assertIn("stderr failed", content)
            self.assertIn("Return code: 5", content)

    def test_complete_turn_clears_state_after_completion(self) -> None:
        state = CodexAppServerSessionState()
        state.observe_file_change_request("thread-1", "turn-1")
        state.observe_turn_diff_updated("thread-1", "turn-1", "diff --git a/foo.py b/foo.py\n")

        first = state.complete_turn(
            "thread-1",
            "turn-1",
            cwd=Path.cwd(),
            hook_specs=[],
        )
        second = state.complete_turn(
            "thread-1",
            "turn-1",
            cwd=Path.cwd(),
            hook_specs=[],
        )

        self.assertIsNotNone(first)
        self.assertIsNone(second)


if __name__ == "__main__":
    unittest.main()
