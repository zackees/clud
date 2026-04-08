"""Contract tests for the backend-neutral agent interface."""

import unittest
from pathlib import Path


class TestAgentBackendContract(unittest.TestCase):
    """Spec for the backend-neutral agent contract."""

    def test_agent_args_preserves_known_and_unknown_flags(self) -> None:
        """The param struct should carry normalized fields plus raw passthrough flags."""
        from clud.agent.interfaces import AgentArgs, ContinueMode, InvocationMode

        args = AgentArgs(
            backend="codex",
            persist_backend=True,
            invocation_mode=InvocationMode.PROMPT,
            input_text="ship it",
            continue_mode=ContinueMode.RESUME,
            resume_target="session-123",
            model="gpt-5.4",
            known_flags={"plain": False, "verbose": True, "cwd": "C:/work"},
            unknown_flags=["--model", "gpt-5.4", "--trace", "--foo=bar"],
            plain=False,
            verbose=True,
            dry_run=False,
            idle_timeout=30.0,
            cwd="C:/work",
        )

        self.assertEqual(args.backend, "codex")
        self.assertTrue(args.persist_backend)
        self.assertEqual(args.invocation_mode, InvocationMode.PROMPT)
        self.assertEqual(args.input_text, "ship it")
        self.assertEqual(args.continue_mode, ContinueMode.RESUME)
        self.assertEqual(args.resume_target, "session-123")
        self.assertEqual(args.model, "gpt-5.4")
        self.assertEqual(args.known_flags["cwd"], "C:/work")
        self.assertEqual(args.unknown_flags, ["--model", "gpt-5.4", "--trace", "--foo=bar"])
        self.assertEqual(args.normalized_unknown_flags(), ["--model", "gpt-5.4", "--trace", "--foo=bar"])

    def test_backend_registry_returns_named_adapters(self) -> None:
        """The backend registry should resolve adapters by backend name."""
        from clud.agent.backends.claude import ClaudeBackend
        from clud.agent.backends.codex import CodexBackend
        from clud.agent.backends.registry import get_backend, get_backend_registry, list_backends

        registry = get_backend_registry()
        self.assertIsInstance(registry["claude"], ClaudeBackend)
        self.assertIsInstance(registry["codex"], CodexBackend)
        self.assertIsInstance(get_backend("claude"), ClaudeBackend)
        self.assertIsInstance(get_backend("codex"), CodexBackend)
        self.assertEqual(set(list_backends()), {"claude", "codex"})

    def test_claude_launch_plan_maps_standard_args(self) -> None:
        """Claude adapter should map the standard param struct into a native argv plan."""
        from clud.agent.backends.claude import ClaudeBackend
        from clud.agent.interfaces import AgentArgs, ContinueMode, InvocationMode

        backend = ClaudeBackend()
        plan = backend.build_launch_plan(
            AgentArgs(
                backend="claude",
                invocation_mode=InvocationMode.PROMPT,
                input_text="hello",
                continue_mode=ContinueMode.NONE,
                model="sonnet",
                known_flags={"plain": False},
                unknown_flags=["--experimental-flag", "value"],
                plain=False,
                verbose=False,
                dry_run=False,
                idle_timeout=None,
                cwd="C:/work",
            )
        )

        self.assertEqual(plan.display_name, "Claude")
        self.assertFalse(plan.interactive)
        self.assertTrue(plan.supports_streaming_output)
        self.assertIn("--dangerously-skip-permissions", plan.argv)
        self.assertIn("-p", plan.argv)
        self.assertIn("hello", plan.argv)
        self.assertIn("--sonnet", plan.argv)
        self.assertIn("--experimental-flag", plan.argv)
        self.assertIn("value", plan.argv)
        self.assertIn("--settings", plan.argv)
        self.assertTrue(Path(plan.command[0]).name.lower().startswith("claude"))

    def test_codex_launch_plan_maps_standard_args(self) -> None:
        """Codex adapter should map the standard param struct into a native argv plan."""
        from clud.agent.backends.codex import CodexBackend
        from clud.agent.interfaces import AgentArgs, ContinueMode, InvocationMode

        backend = CodexBackend()
        plan = backend.build_launch_plan(
            AgentArgs(
                backend="codex",
                invocation_mode=InvocationMode.PROMPT,
                input_text="ship it",
                continue_mode=ContinueMode.CONTINUE_LAST,
                model="gpt-5.4",
                known_flags={"plain": False},
                unknown_flags=["--trace"],
                plain=False,
                verbose=True,
                dry_run=False,
                idle_timeout=None,
                cwd="C:/work",
            )
        )

        self.assertEqual(plan.display_name, "Codex")
        self.assertFalse(plan.interactive)
        self.assertTrue(plan.supports_streaming_output)
        self.assertIn("--dangerously-bypass-approvals-and-sandbox", plan.argv)
        self.assertIn("resume", plan.argv)
        self.assertIn("--last", plan.argv)
        self.assertIn("--model", plan.argv)
        self.assertIn("gpt-5.4", plan.argv)
        self.assertIn("--trace", plan.argv)
        self.assertIn("ship it", plan.argv)
        self.assertTrue(Path(plan.command[0]).name.lower().startswith("codex"))


if __name__ == "__main__":
    unittest.main()
