"""Hot-path latency guard for the command-scan hook."""

from __future__ import annotations

import json
import os
import sys
import time
from pathlib import Path

import pytest

from tests import process


@pytest.mark.skipif(sys.platform != "linux", reason="hook latency budget is measured on Linux")
def test_mixed_hook_payloads_have_sub_20ms_p99(tmp_path: Path) -> None:
    if os.environ.get("RM_HOOK_BENCH") != "1":
        pytest.skip("latency budget runs in the dedicated bosn benchmark lane")
    binary = Path(os.environ["CLUD_TEST_BINARY"]).with_name("clud-cmd-scan")
    home = tmp_path / "home"
    home.mkdir()
    env = os.environ.copy()
    env.update(HOME=str(home), USERPROFILE=str(home), CLUD_SKIP_RM_IDENTITY="1")
    commands = [
        "echo ready",
        "r" + "m -rf build",
        "safe-rm -rf build",
        "grep -n 'rm |mktemp' script.sh",
    ]
    payloads = [
        json.dumps({"tool_name": "Bash", "tool_input": {"command": command}, "cwd": str(tmp_path)})
        for command in commands
    ]
    samples_ms = []
    for index in range(1032):
        started = time.perf_counter_ns()
        result = process.run(
            [str(binary)],
            input=payloads[index % len(payloads)],
            cwd=tmp_path,
            env=env,
            capture_output=True,
            text=True,
            timeout=5,
        )
        elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
        assert result.returncode == 0, result
        if index >= 32:
            samples_ms.append(elapsed_ms)
    samples_ms.sort()
    p99_ms = samples_ms[989]
    assert p99_ms < 20, f"command-scan p99={p99_ms:.2f}ms"
