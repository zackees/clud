"""#1687: target reporting reads the daemon snapshot without scanning checkouts."""

import json
import time
from pathlib import Path

from tests.test_hello import _run


def test_gc_targets_reports_cached_sizes_without_live_directories(tmp_path: Path) -> None:
    state = tmp_path / "state"
    state.mkdir()
    snapshot = {
        "checked_unix": int(time.time()),
        "rows": [
            {
                "repo_root": str(tmp_path / "repo"),
                "worktree": str(tmp_path / "repo feature"),
                "target": str(tmp_path / "repo feature" / "target"),
                "bytes": 123456,
                "status": "complete",
            }
        ],
    }
    (state / "target-sizes.json").write_text(json.dumps(snapshot), encoding="utf-8")
    result = _run(
        "--no-daemon",
        "gc",
        "list",
        "--targets",
        "--json",
        env_overrides={"CLUD_DAEMON_STATE_DIR": str(state)},
    )
    assert result.returncode == 0, result.stderr
    data = json.loads(result.stdout)
    assert data["rows"] == snapshot["rows"]
    assert data["stale"] is False
    assert data["checked_unix"] == snapshot["checked_unix"]
    assert not Path(snapshot["rows"][0]["worktree"]).exists()


def test_gc_targets_help_and_missing_snapshot(tmp_path: Path) -> None:
    help_result = _run("gc", "list", "--help")
    assert "--targets" in help_result.stdout
    result = _run(
        "gc",
        "list",
        "--targets",
        env_overrides={"CLUD_DAEMON_STATE_DIR": str(tmp_path)},
    )
    assert result.returncode == 0, result.stderr
    assert "No cached target sizes" in result.stdout


def test_gc_targets_rejects_kind_filter_and_corrupt_cache(tmp_path: Path) -> None:
    conflict = _run("gc", "list", "--targets", "--kind", "trash")
    assert conflict.returncode == 2
    (tmp_path / "target-sizes.json").write_text("invalid", encoding="utf-8")
    result = _run(
        "gc",
        "list",
        "--targets",
        env_overrides={"CLUD_DAEMON_STATE_DIR": str(tmp_path)},
    )
    assert result.returncode == 1
    assert "cannot read target size cache" in result.stderr
