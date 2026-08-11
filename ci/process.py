"""Process helpers for CI scripts, backed exclusively by running-process."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from running_process import PIPE, CompletedProcess, RunningProcess


def run(*args: Any, **kwargs: Any) -> CompletedProcess[Any]:
    """Run a command while preserving `subprocess.run` capture semantics."""
    kwargs.setdefault(
        "text",
        bool(
            kwargs.get("encoding") is not None
            or kwargs.get("errors") is not None
            or kwargs.get("universal_newlines", False)
        ),
    )
    if kwargs.get("capture_output"):
        # `running-process` merges stderr into stdout unless this is explicit;
        # Python's `subprocess.run(capture_output=True)` captures both streams.
        kwargs.setdefault("stderr", PIPE)
    return RunningProcess.run(*args, **kwargs)


def check_output(
    args: str | list[str], *, cwd: Path | str | None = None, text: bool = True, **kwargs: Any
) -> str | bytes:
    return RunningProcess.run(
        args,
        cwd=cwd,
        capture_output=True,
        text=text,
        check=True,
        **kwargs,
    ).stdout
