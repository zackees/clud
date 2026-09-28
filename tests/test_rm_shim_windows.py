"""Windows shim refusals with a recording executable as the only handoff."""

from __future__ import annotations

import json
import os
import shutil
import sys
from pathlib import Path

import pytest

from tests import process
from tests.shim_env import session_env

pytestmark = pytest.mark.skipif(sys.platform != "win32", reason="Windows-only shim cases")


def test_catastrophe_floor_never_reaches_the_recording_handoff(tmp_path: Path) -> None:
    shim_dir = tmp_path / "shim"
    stub_dir = tmp_path / "stub"
    home = tmp_path / "home"
    for directory in [shim_dir, stub_dir, home]:
        directory.mkdir()
    binary = Path(os.environ["CLUD_TEST_BINARY"])
    stub_binary = Path(os.environ["CLUD_TEST_MOCK_AGENT_BINARY"])
    shim = shim_dir / ("r" + "m.exe")
    shutil.copy2(binary.with_name("clud-shim.exe"), shim)
    shutil.copy2(stub_binary, stub_dir / ("r" + "m.exe"))
    recorded = tmp_path / "handoff.json"
    env = os.environ.copy()
    env.update(
        HOME=str(home),
        USERPROFILE=str(home),
        PATH=os.pathsep.join([str(shim_dir), str(stub_dir), env["PATH"]]),
        MOCK_RM_STUB_LOG=str(recorded),
    )
    env.update(session_env(binary.with_name("clud-shim.exe"), shim_dir))
    protected = [
        "C:/",
        "C:\\",
        "C:/Windows",
        "C:/Users",
        "/c",
        "/c/Windows",
        "//server/share",
        "%USERPROFILE%",
        str(home),
    ]
    for operand in protected:
        result = process.run(
            [str(shim), "-rf", operand],
            cwd=tmp_path,
            env=env,
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert result.returncode == 2, (operand, result)
        assert json.loads(result.stdout)["decision"] == "deny"
        assert not recorded.exists(), operand
    allowed = tmp_path / "allowed.txt"
    allowed.write_text("still here", encoding="utf-8")
    handed_off = process.run(
        [str(shim), "-f", str(allowed)],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert handed_off.returncode == 0, handed_off
    assert allowed.read_text(encoding="utf-8") == "still here"
    assert json.loads(recorded.read_text(encoding="utf-8")) == ["-f", str(allowed)]
