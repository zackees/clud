"""Base types for backend adapters."""

from __future__ import annotations

from abc import ABC, abstractmethod
from pathlib import Path
from typing import Protocol

from ..interfaces import AgentArgs, LaunchPlan


class BackendAdapter(Protocol):
    """Protocol for backend adapters."""

    name: str

    def is_installed(self) -> bool: ...
    def find_executable(self) -> str | None: ...
    def install_help(self) -> list[str]: ...
    def resolve_model_display(self, args: AgentArgs) -> str | None: ...
    def build_launch_plan(self, args: AgentArgs) -> LaunchPlan: ...


class BaseBackendAdapter(ABC):
    """Shared behavior for backend adapters."""

    name: str
    executable_name: str

    def find_executable(self) -> str | None:
        """Find the backend executable on PATH."""
        import shutil

        return shutil.which(self.executable_name)

    def is_installed(self) -> bool:
        """Return whether the backend executable is available."""
        return self.find_executable() is not None

    def _resolve_cwd(self, args: AgentArgs) -> str:
        """Resolve a working directory for the launch plan."""
        return args.cwd or str(Path.cwd())

    def _base_plan(self, args: AgentArgs) -> LaunchPlan:
        """Create a launch plan with shared metadata populated."""
        executable = self.find_executable() or self.executable_name
        return LaunchPlan(
            backend=self.name,
            executable=executable,
            cwd=self._resolve_cwd(args),
            display_name=self.name.title(),
        )

    @abstractmethod
    def install_help(self) -> list[str]:
        """Return user-facing install guidance."""

    @abstractmethod
    def resolve_model_display(self, args: AgentArgs) -> str | None:
        """Return the display string for the effective model, if any."""

    @abstractmethod
    def build_launch_plan(self, args: AgentArgs) -> LaunchPlan:
        """Translate standardized args into a backend-native execution plan."""


__all__ = [
    "BackendAdapter",
    "BaseBackendAdapter",
]
