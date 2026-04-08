"""Backend adapter registry."""

from __future__ import annotations

from functools import lru_cache

from .base import BackendAdapter
from .claude import ClaudeBackend
from .codex import CodexBackend


@lru_cache(maxsize=1)
def get_backend_registry() -> dict[str, BackendAdapter]:
    """Return the available backend adapters."""
    registry: dict[str, BackendAdapter] = {
        "claude": ClaudeBackend(),
        "codex": CodexBackend(),
    }
    return registry


def get_backend(name: str) -> BackendAdapter:
    """Look up a backend adapter by name."""
    registry = get_backend_registry()
    try:
        return registry[name]
    except KeyError as exc:
        raise KeyError(f"Unknown backend: {name}") from exc


def list_backends() -> list[str]:
    """Return registered backend names."""
    return sorted(get_backend_registry())


__all__ = [
    "get_backend",
    "get_backend_registry",
    "list_backends",
]
