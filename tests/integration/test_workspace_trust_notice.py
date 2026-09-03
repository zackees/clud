"""Integration: the untrusted-workspace notice (issue #1102).

`clud grind` in a directory the harness has never been trusted in printed
claude's own red "this workspace has not been trusted" banner at the top of
every one of up to 200 unattended iterations, and nothing else. clud now says
it once, before the first iteration, and only when the repo actually ships a
`.claude/settings*.json` for that decision to suppress.
"""

from __future__ import annotations

import json
import tempfile
from pathlib import Path

import pytest

from tests import process

from ._daemon_helpers import copy_launcher, run_clud

pytestmark = pytest.mark.integration

# 30s for the same reason as `test_mock_agents` (#994): these launches spawn a
# real clud through the mock agent on shared runners, and 15s is close enough
# to the observed spread to fail on load alone. This file inherited the 15
# from that one when it was written.
_TIMEOUT = 30
_NOTICE = "does not have this workspace trusted"


def _run_in(
    clud: Path, *args: str, env: dict[str, str], cwd: Path
) -> process.CompletedProcess[str]:
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as temp_dir:
        launch = Path(temp_dir) / clud.name
        copy_launcher(clud, launch)
        return run_clud([str(launch), *args], timeout=_TIMEOUT, env=env, cwd=cwd)


def _workspace(root: Path) -> Path:
    """A repo directory carrying project settings the trust decision gates."""
    repo = root / "repo"
    (repo / ".claude").mkdir(parents=True)
    (repo / ".claude" / "settings.local.json").write_text(
        json.dumps({"permissions": {"allow": ["Bash(ls:*)"]}}), encoding="utf-8"
    )
    return repo


def _write_state(env: dict[str, str], projects: dict) -> None:
    Path(env["HOME"], ".claude.json").write_text(
        json.dumps({"projects": projects}), encoding="utf-8"
    )


def _loop(clud: Path, env: dict[str, str], cwd: Path, *extra: str):
    """A 3-iteration loop — the unattended shape the notice exists for."""
    return _run_in(clud, "loop", "--loop-count", "3", "task", *extra, env=env, cwd=cwd)


def test_untrusted_workspace_warns_exactly_once(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    repo = _workspace(tmp_path)
    _write_state(mock_env, {})

    result = _loop(clud_binary, mock_env, repo)

    # Once, not once per iteration — the whole point of hoisting the call out
    # of both launch modes' loops.
    assert "iteration 3/3" in result.stderr, result.stderr
    assert result.stderr.count(_NOTICE) == 1, result.stderr
    assert "settings.local.json" in result.stderr


def test_trusted_workspace_is_silent(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    repo = _workspace(tmp_path)
    _write_state(mock_env, {str(repo.resolve()): {"hasTrustDialogAccepted": True}})

    result = _loop(clud_binary, mock_env, repo)

    assert _NOTICE not in result.stderr, result.stderr


def test_workspace_without_project_settings_is_silent(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    # Untrusted, but there is no `.claude/settings*.json` to ignore, so the
    # trust decision changes nothing and the notice would be pure noise.
    bare = tmp_path / "bare"
    bare.mkdir()
    _write_state(mock_env, {})

    result = _loop(clud_binary, mock_env, bare)

    assert _NOTICE not in result.stderr, result.stderr


def test_single_interactive_launch_is_silent(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    # One iteration shows the harness's own banner on screen where it is
    # perfectly readable. A second banner would help nobody.
    repo = _workspace(tmp_path)
    _write_state(mock_env, {})

    result = _run_in(clud_binary, "-p", "hello", env=mock_env, cwd=repo)

    assert _NOTICE not in result.stderr, result.stderr


def test_dry_run_stays_clean(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    # `--dry-run` is consumed by tooling; it must not grow a new stderr line.
    repo = _workspace(tmp_path)
    _write_state(mock_env, {})

    result = _loop(clud_binary, mock_env, repo, "--dry-run")

    assert _NOTICE not in result.stderr, result.stderr
