"""Compatibility contracts for the repository's running-process adapters."""

from __future__ import annotations

import sys

import pytest

from ci import process as ci_process
from tests import process as test_process


@pytest.mark.parametrize("adapter", [ci_process, test_process])
def test_capture_output_keeps_stdout_and_stderr_separate(adapter) -> None:
    result = adapter.run(
        [
            sys.executable,
            "-c",
            "import sys; print('stdout'); print('stderr', file=sys.stderr)",
        ],
        capture_output=True,
        text=True,
        check=True,
    )

    assert result.stdout == "stdout"
    assert result.stderr == "stderr"
