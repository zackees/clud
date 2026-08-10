"""Process helpers for CI scripts, backed exclusively by running-process."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from running_process import CompletedProcess, RunningProcess


def run(*args: Any, **kwargs: Any) -> CompletedProcess[Any]:
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
