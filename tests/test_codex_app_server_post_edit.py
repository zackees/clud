"""Tests for Codex app-server post-edit hook follow-up turns."""

from __future__ import annotations

import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from clud.codex_app_server.post_edit import CodexAppServerSessionState
from clud.hooks.command import CommandHookSpec


class TestCodexAppServerPostEdit(unittest.TestCase):
    """Verify file-change approvals trigger real post-edit hook handling."""

    def test_complete_turn_without_write_request_returns_none(self) -> None:
        state = CodexAppServerSessionState()

        outcome = state.complete_turn(
            "thread-1",
            "turn-1",
            cwd=Path.cwd(),
            hook_specs=[],
        )

        self.assertIsNone(outcome)

    def test_complete_turn_with_write_request_and_no_hooks_returns_outcome(self) -> None:
        state = CodexAppServerSessionState()
        state.observe_file_change_request("thread-1", "turn-1")

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
        self.assertIsNone(outcome.follow_up_message)

    def test_complete_turn_writes_failure_artifact_and_builds_follow_up_message(self) -> None:
        state = CodexAppServerSessionState()
        state.observe_file_change_request("thread-1", "turn-1")

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
            self.assertIsNotNone(outcome.follow_up_message)
            assert outcome.follow_up_message is not None
            self.assertIn(failure.artifact_path.name, outcome.follow_up_message)
            self.assertNotIn("\x15", outcome.follow_up_message)
            self.assertNotIn("\r", outcome.follow_up_message)
            self.assertNotIn("\n", outcome.follow_up_message)

    def test_complete_turn_with_successful_hook_does_not_build_follow_up_message(self) -> None:
        state = CodexAppServerSessionState()
        state.observe_file_change_request("thread-1", "turn-1")

        with TemporaryDirectory() as tmp:
            outcome = state.complete_turn(
                "thread-1",
                "turn-1",
                cwd=Path(tmp),
                hook_specs=[
                    CommandHookSpec(
                        event_name="PostEdit",
                        command="python -c \"print('lint ok')\"",
                    )
                ],
            )

        self.assertIsNotNone(outcome)
        assert outcome is not None
        self.assertFalse(outcome.has_failures)
        self.assertIsNone(outcome.follow_up_message)

    def test_build_follow_up_turn_request_uses_turn_start_text_input(self) -> None:
        request = CodexAppServerSessionState.build_follow_up_turn_request(
            thread_id="thread-1",
            message="Post-edit hook failed.",
            request_id="req-1",
            cwd=Path("C:/tmp/work"),
        )

        self.assertEqual(request["id"], "req-1")
        self.assertEqual(request["method"], "turn/start")
        self.assertEqual(request["params"]["threadId"], "thread-1")
        self.assertEqual(request["params"]["cwd"], "C:\\tmp\\work")
        self.assertEqual(
            request["params"]["input"],
            [{"type": "text", "text": "Post-edit hook failed."}],
        )
        self.assertNotIn("\x15", request["params"]["input"][0]["text"])
        self.assertNotIn("\r", request["params"]["input"][0]["text"])

    def test_complete_turn_clears_state_after_completion(self) -> None:
        state = CodexAppServerSessionState()
        state.observe_file_change_request("thread-1", "turn-1")

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
