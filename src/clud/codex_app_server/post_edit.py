"""Codex app-server state for post-edit hook follow-up turns."""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from clud.hooks import HookContext, HookEvent
from clud.hooks.command import CommandHookResult, CommandHookSpec, run_command_hook, write_failure_artifact


def _failure_list() -> list[PostEditHookFailure]:
    return []


def _user_input_items(text: str) -> list[dict[str, str]]:
    return [{"type": "text", "text": text}]


@dataclass(frozen=True, slots=True)
class TurnKey:
    """Identify a Codex turn within a thread."""

    thread_id: str
    turn_id: str


@dataclass(slots=True)
class _PendingTurnState:
    """Mutable session state for a turn."""

    write_requested: bool = False


@dataclass(slots=True)
class PostEditHookFailure:
    """Failure details for one post-edit hook command."""

    hook: CommandHookSpec
    result: CommandHookResult
    artifact_path: Path


@dataclass(slots=True)
class PostEditHookOutcome:
    """Outcome from evaluating post-edit hooks after a write-producing turn."""

    turn: TurnKey
    hook_count: int
    failures: list[PostEditHookFailure] = field(default_factory=_failure_list)
    follow_up_message: str | None = None

    @property
    def has_failures(self) -> bool:
        return bool(self.failures)


class CodexAppServerSessionState:
    """Track write-related app-server events and emulate a post-edit hook event."""

    def __init__(self) -> None:
        self._turns: dict[TurnKey, _PendingTurnState] = {}

    def observe_file_change_request(self, thread_id: str, turn_id: str) -> None:
        """Record that a turn requested a file change approval."""
        self._ensure_turn(thread_id, turn_id).write_requested = True

    def complete_turn(
        self,
        thread_id: str,
        turn_id: str,
        *,
        cwd: Path,
        hook_specs: list[CommandHookSpec],
    ) -> PostEditHookOutcome | None:
        """Finalize a turn and run post-edit hooks if it requested file changes."""
        key = TurnKey(thread_id=thread_id, turn_id=turn_id)
        state = self._turns.pop(key, None)
        if state is None or not state.write_requested:
            return None

        outcome = PostEditHookOutcome(
            turn=key,
            hook_count=len(hook_specs),
        )
        if not hook_specs:
            return outcome

        context = HookContext(
            event=HookEvent.POST_EXECUTION,
            instance_id=turn_id,
            session_id=thread_id,
            client_type="codex-app-server",
            client_id="codex",
            metadata={
                "backend": "codex",
                "cwd": str(cwd),
                "reason": "post_edit",
                "thread_id": thread_id,
                "turn_id": turn_id,
            },
        )

        for hook in hook_specs:
            result = run_command_hook(hook, context)
            if result.failed:
                artifact_path = write_failure_artifact(
                    result,
                    filename=self._failure_filename(turn_id=turn_id, event_name=hook.event_name),
                )
                outcome.failures.append(
                    PostEditHookFailure(
                        hook=hook,
                        result=result,
                        artifact_path=artifact_path,
                    )
                )

        if outcome.failures:
            first_failure = outcome.failures[0]
            outcome.follow_up_message = f"Post-edit hook failed. Read {first_failure.artifact_path.name} and fix the problem before continuing."

        return outcome

    @staticmethod
    def build_follow_up_turn_request(
        *,
        thread_id: str,
        message: str,
        request_id: str | int,
        cwd: Path | None = None,
    ) -> dict[str, Any]:
        """Build a JSON-RPC request that submits a follow-up user turn."""
        params: dict[str, Any] = {
            "threadId": thread_id,
            "input": _user_input_items(message),
        }
        if cwd is not None:
            params["cwd"] = str(cwd)
        return {
            "id": request_id,
            "method": "turn/start",
            "params": params,
        }

    def _ensure_turn(self, thread_id: str, turn_id: str) -> _PendingTurnState:
        key = TurnKey(thread_id=thread_id, turn_id=turn_id)
        if key not in self._turns:
            self._turns[key] = _PendingTurnState()
        return self._turns[key]

    @staticmethod
    def _failure_filename(turn_id: str, event_name: str) -> str:
        safe_turn = "".join(char if char.isalnum() or char in {"-", "_"} else "_" for char in turn_id)
        safe_event = "".join(char if char.isalnum() or char in {"-", "_"} else "_" for char in event_name)
        return f"POST_HOOK_FAILURE_{safe_event}_{safe_turn}.txt"
