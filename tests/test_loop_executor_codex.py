"""Targeted regression tests for Codex loop execution paths."""

from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from clud.agent.loop_executor import _run_loop
from clud.agent_args import AgentMode, Args


class TestLoopExecutorCodex(unittest.TestCase):
    """Codex loop mode should launch directly without git-bash wrapping."""

    def test_codex_loop_skips_git_bash_wrapper(self) -> None:
        args = Args(
            mode=AgentMode.DEFAULT,
            prompt="keep working",
            agent_backend="codex",
        )

        with tempfile.TemporaryDirectory() as tmp:
            original_cwd = os.getcwd()
            os.chdir(tmp)
            try:
                with (
                    patch("clud.agent.loop_executor._handle_existing_loop", return_value=(True, 1)),
                    patch("clud.agent.loop_executor.write_motivation_file"),
                    patch("clud.agent.loop_executor._print_loop_banner"),
                    patch("clud.agent.loop_executor._build_claude_command", return_value=["codex", "resume", "--last"]),
                    patch("clud.agent.loop_executor._get_effective_backend", return_value="codex"),
                    patch("clud.agent.loop_executor._get_model_from_args", return_value=None),
                    patch("clud.agent.loop_executor._print_model_message"),
                    patch("clud.agent.loop_executor._print_debug_info"),
                    patch("clud.agent.loop_executor.run_claude_process", return_value=0),
                    patch("clud.agent.loop_executor._wrap_command_for_git_bash", side_effect=AssertionError("wrapper should not be used")),
                ):
                    result = _run_loop(args, "codex", loop_count=1)
            finally:
                os.chdir(original_cwd)

        self.assertEqual(result, 0)
        self.assertFalse(Path(tmp, "DONE.md").exists())


if __name__ == "__main__":
    unittest.main()
