"""Command-based hook handler compatibility for Claude-style hook configs."""

from __future__ import annotations

import asyncio
import logging
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

from clud.hooks import HookContext

logger = logging.getLogger(__name__)


@dataclass(slots=True)
class CommandHookSpec:
    """A single shell command bound to a logical hook event."""

    event_name: str
    command: str
    source_path: str | None = None


class CommandHookHandler:
    """Execute user-defined shell commands for hook events."""

    def __init__(self, commands: list[CommandHookSpec], timeout_seconds: float = 1800.0) -> None:
        self._commands = list(commands)
        self._timeout_seconds = timeout_seconds

    async def handle(self, context: HookContext) -> None:
        for spec in self._commands:
            await asyncio.to_thread(self._run_command, spec, context)

    def _run_command(self, spec: CommandHookSpec, context: HookContext) -> None:
        cwd = self._resolve_cwd(context)
        env = self._build_env(spec, context, cwd)

        try:
            result = subprocess.run(
                spec.command,
                shell=True,
                cwd=str(cwd),
                env=env,
                text=True,
                capture_output=True,
                timeout=self._timeout_seconds,
            )
        except subprocess.TimeoutExpired:
            print(
                f"Hook `{spec.event_name}` timed out after {self._timeout_seconds:.0f}s: {spec.command}",
                file=sys.stderr,
            )
            return
        except Exception as exc:
            print(f"Hook `{spec.event_name}` failed to start: {spec.command} ({exc})", file=sys.stderr)
            return

        if result.stdout:
            print(result.stdout, end="" if result.stdout.endswith("\n") else "\n", file=sys.stderr)
        if result.stderr:
            print(result.stderr, end="" if result.stderr.endswith("\n") else "\n", file=sys.stderr)

        if result.returncode != 0:
            print(
                f"Hook `{spec.event_name}` exited with status {result.returncode}: {spec.command}",
                file=sys.stderr,
            )

    def _resolve_cwd(self, context: HookContext) -> Path:
        raw_cwd = context.metadata.get("cwd")
        if isinstance(raw_cwd, str) and raw_cwd:
            return Path(raw_cwd)
        return Path.cwd()

    def _build_env(self, spec: CommandHookSpec, context: HookContext, cwd: Path) -> dict[str, str]:
        env = os.environ.copy()
        metadata = context.metadata

        env["CLAUDE_PROJECT_DIR"] = str(cwd)
        env["CLUD_PROJECT_DIR"] = str(cwd)
        env["CLUD_HOOK_EVENT"] = spec.event_name
        env["CLUD_INTERNAL_EVENT"] = context.event.value
        env["CLUD_INSTANCE_ID"] = context.instance_id
        env["CLUD_SESSION_ID"] = context.session_id
        env["CLUD_CLIENT_TYPE"] = context.client_type
        env["CLUD_CLIENT_ID"] = context.client_id

        backend = metadata.get("backend")
        if isinstance(backend, str) and backend:
            env["CLUD_BACKEND"] = backend

        reason = metadata.get("reason")
        if isinstance(reason, str) and reason:
            env["CLUD_STOP_REASON"] = reason

        returncode = metadata.get("returncode")
        if returncode is not None:
            env["CLUD_RETURN_CODE"] = str(returncode)

        if metadata.get("idle_detected") is not None:
            env["CLUD_IDLE_DETECTED"] = "1" if metadata["idle_detected"] else "0"

        source_path = spec.source_path
        if source_path:
            env["CLUD_HOOK_SOURCE"] = source_path

        return env


__all__ = ["CommandHookHandler", "CommandHookSpec"]
