"""#2122: `clud session stats` prints the verify-ratio hook's per-kind breakdown."""

import hashlib
import json
from pathlib import Path

from tests.test_hello import _run


def _ledger_path(state: Path, session_id: str) -> Path:
    digest = hashlib.sha256(f"clud-verify-ratio-v1\0session\0{session_id}".encode()).digest()
    return state / "verify-ratio" / f"{digest[:8].hex()}.json"


def test_session_stats_prints_the_recorded_breakdown(tmp_path: Path) -> None:
    state = tmp_path / "state"
    path = _ledger_path(state, "s-123")
    path.parent.mkdir(parents=True)
    ledger = {
        "window": "evv",
        "quiet": 0,
        "totals": {
            "edit": {"calls": 1, "ms": 0},
            "test": {"calls": 2, "ms": 90000},
            "other": {"calls": 1, "ms": 30000},
        },
    }
    path.write_text(json.dumps(ledger), encoding="utf-8")
    result = _run(
        "session",
        "stats",
        "--session",
        "s-123",
        env_overrides={"CLUD_DAEMON_STATE_DIR": str(state)},
    )
    assert result.returncode == 0, result.stderr
    assert "test        2      1m30s     75%" in result.stdout, result.stdout
    assert "bash wall time: 2m00s" in result.stdout, result.stdout


def test_session_stats_reports_an_unknown_or_missing_session(tmp_path: Path) -> None:
    state = tmp_path / "state"
    state.mkdir()
    env = {"CLUD_DAEMON_STATE_DIR": str(state), "CLAUDE_CODE_SESSION_ID": ""}
    unknown = _run("session", "stats", "--session", "nobody", env_overrides=env)
    assert unknown.returncode == 1, unknown.stderr
    assert "nothing recorded" in unknown.stderr
    missing = _run("session", "stats", env_overrides=env)
    assert missing.returncode == 2, missing.stderr
    assert "no session id" in missing.stderr
