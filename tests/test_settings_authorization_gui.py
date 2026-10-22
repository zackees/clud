"""Real Linux settings approval windows, driven by synthetic desktop input."""

from __future__ import annotations

import json
import shutil
import sys
from pathlib import Path

import pytest

from tests import process
from tests.test_settings_authorization import _world


@pytest.mark.skipif(sys.platform != "linux", reason="X11 native approval acceptance")
@pytest.mark.parametrize("action", ["approve", "cancel", "roots", "changed"])
def test_settings_authorization_native_window(tmp_path: Path, action: str) -> None:
    binary, env, repo, settings = _world(tmp_path)
    assert shutil.which("xvfb-run"), "settings approval acceptance requires Xvfb"
    assert shutil.which("xdotool"), "settings approval acceptance requires xdotool"
    original = settings.read_bytes()
    expected = json.loads(original)
    mutation = ["set", "/web_term/enabled", "false", "--reason", "Test-owned preference"]
    if action == "roots":
        root = tmp_path / "approved-data"
        root.mkdir()
        roots = [{"path": str(root.resolve()), "reason": "Owned test data"}]
        mutation = ["roots", json.dumps(roots), "--reason", "Test-owned deletion root"]
        expected["safe_rm"]["extra_roots"] = roots
    else:
        expected["web_term"] = {"enabled": False}
    env.pop("WAYLAND_DISPLAY", None)
    # The driver inherits only this test-owned HOME and talks to a fresh X server.
    result = process.run(
        ["xvfb-run", "-a", "-s", "-screen 0 1024x768x24", sys.executable,
         "-m", "tests.settings_approval_driver", binary, str(repo), str(settings),
         action, json.dumps(mutation)],
        env=env, capture_output=True, text=True, timeout=70,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    outcome = json.loads(result.stdout.strip().splitlines()[-1])
    if action == "cancel":
        assert outcome["returncode"] == 3, outcome
        assert settings.read_bytes() == original
    elif action == "changed":
        assert outcome["returncode"] != 0, outcome
        assert json.loads(settings.read_bytes()) == {"concurrent": True}
    else:
        assert outcome["returncode"] == 0, outcome
        assert json.loads(settings.read_bytes()) == expected
        audit = (settings.parent / "settings-audit.jsonl").read_text(encoding="utf-8")
        assert '"saved"' in audit
        assert "preserved" not in audit
