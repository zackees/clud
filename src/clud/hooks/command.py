"""Command-based hook handler compatibility for Claude-style hook configs."""

from __future__ import annotations

import asyncio
import logging
import os
import subprocess
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

from clud.hooks import HookContext

logger = logging.getLogger(__name__)

POST_HOOK_FAILURE_FILENAME = "POST_HOOK_FAILURE.txt"


@dataclass(slots=True)
class CommandHookSpec:
    """A single shell command bound to a logical hook event."""

    event_name: str
    command: str
    source_path: str | None = None


@dataclass(slots=True)
class CommandHookResult:
    """Captured result for a single hook command execution."""

    spec: CommandHookSpec
    cwd: Path
    returncode: int
    stdout: str
    stderr: str
    error_message: str | None = None

    @property
    def failed(self) -> bool:
        return self.error_message is not None or self.returncode != 0


class CommandHookHandler:
    """Execute user-defined shell commands for hook events."""

    def __init__(self, commands: list[CommandHookSpec], timeout_seconds: float = 1800.0) -> None:
        self._commands = list(commands)
        self._timeout_seconds = timeout_seconds

    async def handle(self, context: HookContext) -> None:
        for spec in self._commands:
            await asyncio.to_thread(self._run_command, spec, context)

    def _run_command(self, spec: CommandHookSpec, context: HookContext) -> None:
        result = run_command_hook(spec, context, timeout_seconds=self._timeout_seconds)

        if result.stdout:
            print(result.stdout, end="" if result.stdout.endswith("\n") else "\n", file=sys.stderr)
        if result.stderr:
            print(result.stderr, end="" if result.stderr.endswith("\n") else "\n", file=sys.stderr)

        if result.error_message:
            print(result.error_message, file=sys.stderr)
            return

        if result.returncode != 0:
            print(f"Hook `{spec.event_name}` exited with status {result.returncode}: {spec.command}", file=sys.stderr)

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


def run_command_hook(spec: CommandHookSpec, context: HookContext, timeout_seconds: float = 1800.0) -> CommandHookResult:
    """Execute a hook command and capture its result."""
    handler = CommandHookHandler([], timeout_seconds=timeout_seconds)
    cwd = handler._resolve_cwd(context)
    env = handler._build_env(spec, context, cwd)

    try:
        completed = subprocess.run(
            spec.command,
            shell=True,
            cwd=str(cwd),
            env=env,
            text=True,
            capture_output=True,
            timeout=timeout_seconds,
        )
    except subprocess.TimeoutExpired:
        return CommandHookResult(
            spec=spec,
            cwd=cwd,
            returncode=124,
            stdout="",
            stderr="",
            error_message=f"Hook `{spec.event_name}` timed out after {timeout_seconds:.0f}s: {spec.command}",
        )
    except Exception as exc:
        return CommandHookResult(
            spec=spec,
            cwd=cwd,
            returncode=1,
            stdout="",
            stderr="",
            error_message=f"Hook `{spec.event_name}` failed to start: {spec.command} ({exc})",
        )

    return CommandHookResult(
        spec=spec,
        cwd=cwd,
        returncode=completed.returncode,
        stdout=completed.stdout,
        stderr=completed.stderr,
    )


def write_failure_artifact(result: CommandHookResult, filename: str = POST_HOOK_FAILURE_FILENAME) -> Path:
    """Persist a failed hook execution to a workspace file."""
    output_path = result.cwd / filename
    timestamp = datetime.now(timezone.utc).isoformat()
    source = result.spec.source_path or "<unknown>"
    content = (
        "Post-edit hook failure\n"
        f"Timestamp: {timestamp}\n"
        f"Event: {result.spec.event_name}\n"
        f"Command: {result.spec.command}\n"
        f"Source: {source}\n"
        f"Return code: {result.returncode}\n\n"
        "STDOUT\n"
        f"{result.stdout or '<empty>'}\n\n"
        "STDERR\n"
        f"{result.stderr or '<empty>'}\n"
    )
    if result.error_message:
        content += f"\nERROR\n{result.error_message}\n"
    output_path.write_text(content, encoding="utf-8")
    return output_path


__all__ = [
    "CommandHookHandler",
    "CommandHookResult",
    "CommandHookSpec",
    "POST_HOOK_FAILURE_FILENAME",
    "run_command_hook",
    "write_failure_artifact",
]
