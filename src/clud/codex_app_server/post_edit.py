"""Codex app-server state for write detection and post-edit hook emulation."""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

from clud.hooks import HookContext, HookEvent
from clud.hooks.command import CommandHookResult, CommandHookSpec, run_command_hook, write_failure_artifact


def _failure_list() -> list[PostEditHookFailure]:
    return []


@dataclass(frozen=True, slots=True)
class TurnKey:
    """Identify a Codex turn within a thread."""

    thread_id: str
    turn_id: str


@dataclass(slots=True)
class _PendingTurnState:
    """Mutable session state for a turn."""

    write_requested: bool = False
    latest_diff: str = ""


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
    diff: str
    hook_count: int
    failures: list[PostEditHookFailure] = field(default_factory=_failure_list)

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

    def observe_turn_diff_updated(self, thread_id: str, turn_id: str, diff: str) -> None:
        """Record the latest aggregated diff for a turn."""
        state = self._ensure_turn(thread_id, turn_id)
        state.latest_diff = diff
        if diff.strip():
            state.write_requested = True

    def complete_turn(
        self,
        thread_id: str,
        turn_id: str,
        *,
        cwd: Path,
        hook_specs: list[CommandHookSpec],
    ) -> PostEditHookOutcome | None:
        """Finalize a turn and run post-edit hooks if it produced a diff."""
        key = TurnKey(thread_id=thread_id, turn_id=turn_id)
        state = self._turns.pop(key, None)
        if state is None:
            return None
        if not state.write_requested or not state.latest_diff.strip():
            return None

        outcome = PostEditHookOutcome(
            turn=key,
            diff=state.latest_diff,
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

        return outcome

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
