"""End-to-end coverage for the `CwdChanged` backstop (#967 Phase 5).

The handler (`block_bad_cmd_cwd_changed.rs`) is the reactive detection of
drift the PreToolUse scanner cannot see — an alias or a script that chdirs.
These tests exercise it as a real process: the drift warning against the
payload's new cwd, the always-exit-0 contract, and the Tier-B dispatch of
repo-declared `CwdChanged` hooks with a deny downgraded to a warning.
"""

from __future__ import annotations

import json
import os
import shutil
import sys
from pathlib import Path

from tests import process

HOOKS_JSON = json.dumps(
    {
        "hooks": {
            "CwdChanged": [
                {"command": "touch marker", "timeout": 30},
            ]
        }
    }
)


def _binary_name(name: str) -> str:
    return f"{name}.exe" if sys.platform == "win32" else name


def _cmd_scan_binary() -> Path:
    clud_binary = os.environ.get("CLUD_TEST_BINARY")
    if clud_binary:
        sibling = Path(clud_binary).with_name(_binary_name("clud-cmd-scan"))
        if sibling.is_file():
            return sibling

    resolved = shutil.which(_binary_name("clud-cmd-scan"))
    if resolved:
        return Path(resolved)

    raise AssertionError("clud-cmd-scan test binary not found")


def _make_repo(tmp_path: Path, *, hooks_json: str | None = None) -> Path:
    repo = tmp_path / "repo"
    (repo / ".git").mkdir(parents=True)
    (repo / "src").mkdir()
    if hooks_json is not None:
        clud = repo / ".clud"
        clud.mkdir()
        (clud / "hooks.json").write_text(hooks_json, encoding="utf-8")
    return repo


def _payload(new_cwd: Path) -> str:
    # The harness's documented CwdChanged payload: `new_cwd` is the new
    # directory, `cwd` documents the same.
    return json.dumps(
        {
            "hook_event_name": "CwdChanged",
            "session_id": "s1",
            "transcript_path": "/t.jsonl",
            "old_cwd": "/start",
            "cwd": str(new_cwd),
            "new_cwd": str(new_cwd),
        }
    )


def _run(
    tmp_path: Path,
    repo: Path,
    new_cwd: Path,
    *,
    payload: str | None = None,
    extra_env: dict[str, str] | None = None,
) -> process.CompletedProcess[str]:
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    env = os.environ.copy()
    # An isolated HOME keeps the developer's own settings out of the
    # resolution; CLAUDE_PROJECT_DIR is the session root a real harness
    # exports on every spawned hook.
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    env["CLAUDE_PROJECT_DIR"] = str(repo)
    if extra_env:
        env.update(extra_env)
    return process.run(
        [str(_cmd_scan_binary()), "--event", "CwdChanged"],
        input=payload if payload is not None else _payload(new_cwd),
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
    )


def test_an_escape_the_scanner_cannot_see_warns(tmp_path: Path) -> None:
    # A migrated repo resolves "auto" to relaxed; a chdir out of the
    # registered roots is exactly the drift an alias or script would cause.
    repo = _make_repo(tmp_path, hooks_json=HOOKS_JSON)
    outside = tmp_path / "elsewhere"
    outside.mkdir()

    result = _run(tmp_path, repo, outside)

    assert result.returncode == 0, result.stdout + result.stderr
    # The path is rendered with forward slashes on every platform, so assert
    # the stable prefix rather than the platform spelling.
    assert "[clud] CwdChanged: the session cwd moved to" in result.stderr


def test_a_move_within_the_registered_root_stays_silent(tmp_path: Path) -> None:
    repo = _make_repo(tmp_path, hooks_json=HOOKS_JSON)
    subdir = repo / "src"

    result = _run(tmp_path, repo, subdir)

    assert result.returncode == 0, result.stdout + result.stderr
    assert "CwdChanged" not in result.stderr


def test_block_cd_false_silences_the_backstop(tmp_path: Path) -> None:
    repo = _make_repo(tmp_path, hooks_json=HOOKS_JSON)
    (repo / ".clud" / "settings.json").write_text(
        json.dumps({"bash": {"block_cd": False}}), encoding="utf-8"
    )
    outside = tmp_path / "elsewhere"
    outside.mkdir()

    result = _run(tmp_path, repo, outside)

    assert result.returncode == 0, result.stdout + result.stderr
    assert "CwdChanged" not in result.stderr


def test_a_garbage_payload_still_exits_zero(tmp_path: Path) -> None:
    # The backstop is hygiene, never correctness: no payload shape may turn
    # it into a wall.
    repo = _make_repo(tmp_path, hooks_json=HOOKS_JSON)

    result = _run(tmp_path, repo, repo, payload="not json")

    assert result.returncode == 0, result.stdout + result.stderr


def test_a_declared_cwd_changed_hook_runs_rooted_at_the_repo(tmp_path: Path) -> None:
    # Tier B fires the repo's declared CwdChanged hooks for the repo the
    # session moved within, rooted at that repo.
    repo = _make_repo(tmp_path, hooks_json=HOOKS_JSON)
    subdir = repo / "src"

    result = _run(tmp_path, repo, subdir)

    assert result.returncode == 0, result.stdout + result.stderr
    assert (repo / "marker").exists(), "the hook ran rooted at the repo"


def test_a_refusing_cwd_changed_hook_is_downgraded_to_a_warning(
    tmp_path: Path,
) -> None:
    # The cwd has already changed and the harness gives CwdChanged no
    # decision control, so an exit 2 surfaces as a warning, never a block.
    repo = _make_repo(
        tmp_path,
        hooks_json=json.dumps(
            {"hooks": {"CwdChanged": [{"command": "exit 2", "timeout": 30}]}}
        ),
    )
    subdir = repo / "src"

    result = _run(tmp_path, repo, subdir)

    assert result.returncode == 0, result.stdout + result.stderr
    assert "cannot be enforced" in result.stderr
