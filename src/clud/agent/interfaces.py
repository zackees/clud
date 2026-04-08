"""Backend-neutral agent execution interfaces."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any


def _any_dict() -> dict[str, Any]:
    """Return a typed empty metadata/flags mapping."""
    return {}


def _str_list() -> list[str]:
    """Return a typed empty list of strings."""
    return []


def _str_dict() -> dict[str, str]:
    """Return a typed empty environment mapping."""
    return {}


class InvocationMode(str, Enum):
    """How the agent should receive the user's input."""

    INTERACTIVE = "interactive"
    MESSAGE = "message"
    PROMPT = "prompt"


class ContinueMode(str, Enum):
    """How a backend should continue or resume a prior conversation."""

    NONE = "none"
    CONTINUE_LAST = "continue_last"
    RESUME = "resume"


@dataclass(slots=True)
class AgentArgs:
    """Standardized agent parameters plus preserved backend-native flags."""

    backend: str | None = None
    persist_backend: bool = False
    invocation_mode: InvocationMode = InvocationMode.INTERACTIVE
    input_text: str | None = None
    continue_mode: ContinueMode = ContinueMode.NONE
    resume_target: str | None = None
    model: str | None = None
    known_flags: dict[str, Any] = field(default_factory=_any_dict)
    unknown_flags: list[str] = field(default_factory=_str_list)
    plain: bool = False
    verbose: bool = False
    dry_run: bool = False
    idle_timeout: float | None = None
    cwd: str | None = None
    metadata: dict[str, Any] = field(default_factory=_any_dict)

    def normalized_unknown_flags(self) -> list[str]:
        """Return a copy of passthrough flags in their original order."""
        return list(self.unknown_flags)


@dataclass(slots=True)
class LaunchPlan:
    """Concrete execution plan returned by a backend adapter."""

    backend: str
    executable: str
    argv: list[str] = field(default_factory=_str_list)
    env: dict[str, str] = field(default_factory=_str_dict)
    cwd: str | None = None
    display_name: str | None = None
    interactive: bool = False
    supports_streaming_output: bool = False
    model_display: str | None = None
    notes: list[str] = field(default_factory=_str_list)

    @property
    def command(self) -> list[str]:
        """Return the full command including executable."""
        return [self.executable, *self.argv]


__all__ = [
    "AgentArgs",
    "ContinueMode",
    "InvocationMode",
    "LaunchPlan",
]
