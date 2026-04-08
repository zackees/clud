"""Codex backend adapter."""

from __future__ import annotations

from ..interfaces import AgentArgs, ContinueMode, InvocationMode, LaunchPlan
from .base import BaseBackendAdapter


class CodexBackend(BaseBackendAdapter):
    """Adapter for Codex CLI."""

    name = "codex"
    executable_name = "codex"

    def install_help(self) -> list[str]:
        return [
            "Codex is not installed or not in PATH.",
            "Install Codex CLI and ensure `codex` is available on PATH.",
        ]

    def resolve_model_display(self, args: AgentArgs) -> str | None:
        return args.model

    def build_launch_plan(self, args: AgentArgs) -> LaunchPlan:
        plan = self._base_plan(args)
        argv = ["--dangerously-bypass-approvals-and-sandbox", "-C", plan.cwd or "."]

        if args.continue_mode == ContinueMode.CONTINUE_LAST:
            argv.extend(["resume", "--last"])
        elif args.continue_mode == ContinueMode.RESUME:
            argv.append("resume")
            if args.resume_target:
                argv.append(args.resume_target)
        elif args.invocation_mode == InvocationMode.PROMPT and args.input_text:
            argv.append("exec")

        if args.model:
            argv.extend(["--model", args.model])

        if args.invocation_mode == InvocationMode.PROMPT and args.input_text:
            argv.extend(args.normalized_unknown_flags())
            argv.append(args.input_text)
        else:
            if args.input_text:
                argv.append(args.input_text)
            argv.extend(args.normalized_unknown_flags())

        plan.argv = argv
        plan.interactive = args.invocation_mode == InvocationMode.INTERACTIVE
        plan.supports_streaming_output = args.invocation_mode == InvocationMode.PROMPT and not args.plain
        plan.model_display = self.resolve_model_display(args)
        plan.notes.append("Codex backend adapter")
        return plan


__all__ = ["CodexBackend"]
