"""Translate legacy parsed args into the backend-neutral agent contract."""

from __future__ import annotations

from ..agent_args import Args
from .interfaces import AgentArgs, ContinueMode, InvocationMode


def to_agent_args(args: Args, *, resolved_backend: str | None = None, cwd: str | None = None) -> AgentArgs:
    """Convert the current parsed Args object into backend-neutral AgentArgs."""
    invocation_mode = InvocationMode.INTERACTIVE
    input_text = None
    if args.prompt:
        invocation_mode = InvocationMode.PROMPT
        input_text = args.prompt
    elif args.message:
        invocation_mode = InvocationMode.MESSAGE
        input_text = args.message

    continue_mode = ContinueMode.NONE
    resume_target = None
    if args.continue_flag:
        continue_mode = ContinueMode.CONTINUE_LAST
    elif args.resume_flag:
        continue_mode = ContinueMode.RESUME
        resume_target = args.resume_value

    unknown_flags = list(args.unknown_flags if args.unknown_flags is not None else args.claude_args or [])
    known_flags = {
        "plain": args.plain,
        "verbose": args.verbose,
        "dry_run": args.dry_run,
        "idle_timeout": args.idle_timeout,
        "hook_debug": args.hook_debug,
        "no_stop_hook": args.no_stop_hook,
        "no_skills": args.no_skills,
    }

    return AgentArgs(
        backend=resolved_backend or args.backend or args.session_model or args.agent_backend,
        persist_backend=args.persist_backend or args.agent_backend is not None,
        invocation_mode=invocation_mode,
        input_text=input_text,
        continue_mode=continue_mode,
        resume_target=resume_target,
        model=args.model,
        known_flags=known_flags,
        unknown_flags=unknown_flags,
        plain=args.plain,
        verbose=args.verbose,
        dry_run=args.dry_run,
        idle_timeout=args.idle_timeout,
        cwd=cwd,
        metadata={},
    )
