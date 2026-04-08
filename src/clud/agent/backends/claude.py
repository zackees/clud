"""Claude backend adapter."""

from __future__ import annotations

import json

from ..interfaces import AgentArgs, ContinueMode, InvocationMode, LaunchPlan
from .base import BaseBackendAdapter


class ClaudeBackend(BaseBackendAdapter):
    """Adapter for Claude Code."""

    name = "claude"
    executable_name = "claude"

    _MODEL_FLAGS = {
        "haiku": "--haiku",
        "sonnet": "--sonnet",
        "opus": "--opus",
        "claude-3-5-sonnet": "--claude-3-5-sonnet",
        "claude-3-opus": "--claude-3-opus",
        "--haiku": "--haiku",
        "--sonnet": "--sonnet",
        "--opus": "--opus",
        "--claude-3-5-sonnet": "--claude-3-5-sonnet",
        "--claude-3-opus": "--claude-3-opus",
    }

    def find_executable(self) -> str | None:
        from ..claude_finder import _find_claude_path

        return _find_claude_path()

    def install_help(self) -> list[str]:
        return [
            "Claude Code is not installed or not in PATH.",
            "Install with: npm install -g @anthropic-ai/claude-code@latest",
        ]

    def resolve_model_display(self, args: AgentArgs) -> str | None:
        return args.model

    def _append_model(self, argv: list[str], args: AgentArgs) -> None:
        if not args.model:
            return

        model_flag = self._MODEL_FLAGS.get(args.model, args.model)
        argv.append(model_flag)

    def build_launch_plan(self, args: AgentArgs) -> LaunchPlan:
        plan = self._base_plan(args)
        argv = ["--dangerously-skip-permissions"]

        if args.continue_mode == ContinueMode.CONTINUE_LAST:
            argv.append("--continue")
        elif args.continue_mode == ContinueMode.RESUME:
            argv.append("--resume")
            if args.resume_target:
                argv.append(args.resume_target)

        if args.invocation_mode == InvocationMode.PROMPT and args.input_text:
            argv.extend(["-p", args.input_text])
            if not args.plain:
                argv.extend(["--output-format", "stream-json", "--verbose"])
        elif args.invocation_mode == InvocationMode.MESSAGE and args.input_text:
            if args.idle_timeout is not None:
                argv.extend(["-p", args.input_text])
            else:
                argv.append(args.input_text)

        self._append_model(argv, args)
        argv.extend(args.normalized_unknown_flags())

        if not args.metadata.get("disable_attribution_settings", False):
            argv.extend(["--settings", json.dumps({"attribution": {"commit": "", "pr": ""}})])

        plan.argv = argv
        plan.env = {"CLAUDE_CODE_MAX_OUTPUT_TOKENS": "64000"}
        plan.interactive = args.invocation_mode == InvocationMode.INTERACTIVE
        plan.supports_streaming_output = args.invocation_mode == InvocationMode.PROMPT and not args.plain
        plan.model_display = self.resolve_model_display(args)
        plan.notes.append("Claude backend adapter")
        return plan


__all__ = ["ClaudeBackend"]
