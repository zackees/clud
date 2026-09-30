"""Process tests for the in-session git/gh telemetry pass-through (#1486).

Inside a valid session the `git` and `gh` aliases run the real binary with
the same argv, streams and exit status, and append one JSON line per
invocation to `<state>/logs/shim/git-gh.jsonl`. Nothing is ever refused.
The state dir is a tempdir (`CLUD_DAEMON_STATE_DIR`), so no test touches the
real `~/.clud`.
"""

from __future__ import annotations

import json
import os
import shutil
import signal
import sys
from pathlib import Path

import pytest

from tests import process
from tests.shim_env import session_env, session_key_names

POSIX = pytest.mark.skipif(sys.platform == "win32", reason="POSIX recording fixtures")
SENTINEL = "clud-telemetry-must-not-log-this-value"


def _binary(name: str) -> Path:
    suffix = ".exe" if sys.platform == "win32" else ""
    clud = os.environ.get("CLUD_TEST_BINARY")
    candidate = (
        Path(clud).with_name(name + suffix)
        if clud
        else Path(__file__).resolve().parents[1] / "target" / "debug" / (name + suffix)
    )
    assert candidate.is_file(), candidate
    return candidate


def _alias(tmp_path: Path, name: str) -> Path:
    shim_dir = tmp_path / "shim"
    shim_dir.mkdir(exist_ok=True)
    alias = shim_dir / name
    shutil.copy2(_binary("clud-shim"), alias)
    return alias


def _recorder(path: Path, exit_code: int = 37) -> Path:
    """A fake real binary: echoes argv one per line and exits."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f'#!/bin/sh\nprintf "%s\\n" "$@"\nexit {exit_code}\n', encoding="utf-8")
    path.chmod(0o755)
    return path


def _state(tmp_path: Path) -> Path:
    return tmp_path / "state"


def _log(tmp_path: Path) -> Path:
    return _state(tmp_path) / "logs" / "shim" / "git-gh.jsonl"


def _env(tmp_path: Path, alias: Path, **targets: Path) -> dict[str, str]:
    env = os.environ.copy() | session_env(_binary("clud-shim"), alias.parent)
    env.update(
        CLUD_DAEMON_STATE_DIR=str(_state(tmp_path)),
        CLUD_SESSION_ID="session-1486",
        CLUD_TELEMETRY_TEST_SENTINEL=SENTINEL,
    )
    env.update({key: str(value) for key, value in targets.items()})
    return env


def _run(argv: list[str], env: dict[str, str], cwd: Path):
    return process.run(
        argv, env=env, cwd=str(cwd), capture_output=True, text=True, timeout=30
    )


def _records(tmp_path: Path) -> list[dict]:
    log = _log(tmp_path)
    if not log.exists():
        return []
    return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]


# ---- pass-through: nothing is refused ----


@POSIX
@pytest.mark.parametrize(
    "args",
    [
        ["status"],
        ["push", "--force-with-lease"],
        ["clone", "https://github.com/zackees/mimalloc-pprof"],
        ["worktree", "add", "./x", "-b", "b", "origin/main"],
        ["-C", ".", "worktree", "add", "../y"],
        ["commit", "-m", "a b  c"],
    ],
)
def test_git_passes_every_command_through(tmp_path: Path, args: list[str]) -> None:
    alias = _alias(tmp_path, "git")
    real = _recorder(tmp_path / "real" / "git")
    env = _env(tmp_path, alias, CLUD_GIT_SHIM_TARGET=real)
    result = _run([str(alias), *args], env, tmp_path)
    assert result.returncode == 37, result
    assert result.stdout.splitlines() == args
    assert result.stderr == ""
    assert [r["argv"] for r in _records(tmp_path)] == [args]


@POSIX
@pytest.mark.parametrize(
    "args",
    [
        ["issue", "list"],
        ["pr", "view", "--json", "state"],
        ["repo", "clone", "zackees/clud"],
        ["repo", "fork", "zackees/clud", "--clone"],
    ],
)
def test_gh_passes_every_command_through(tmp_path: Path, args: list[str]) -> None:
    alias = _alias(tmp_path, "gh")
    real = _recorder(tmp_path / "real" / "gh")
    env = _env(tmp_path, alias, CLUD_GH_SHIM_TARGET=real)
    result = _run([str(alias), *args], env, tmp_path)
    assert result.returncode == 37, result
    assert result.stdout.splitlines() == args
    assert [(r["tool"], r["argv"]) for r in _records(tmp_path)] == [("gh", args)]


# ---- telemetry ----


@POSIX
def test_one_record_with_the_right_fields(tmp_path: Path) -> None:
    alias = _alias(tmp_path, "git")
    real = _recorder(tmp_path / "real" / "git", exit_code=3)
    env = _env(tmp_path, alias, CLUD_GIT_SHIM_TARGET=real)
    work = tmp_path / "work"
    work.mkdir()
    result = _run([str(alias), "log", "--oneline"], env, work)
    assert result.returncode == 3, result
    records = _records(tmp_path)
    assert len(records) == 1
    record = records[0]
    assert record["tool"] == "git"
    assert record["argv"] == ["log", "--oneline"]
    assert Path(record["cwd"]).resolve() == work.resolve()
    assert record["exit_code"] == 3
    assert isinstance(record["duration_ms"], int)
    assert record["ts_ms"] > 0
    assert record["session_id"] == "session-1486"
    assert "parent" in record
    assert SENTINEL not in _log(tmp_path).read_text(encoding="utf-8"), "no env values"


@POSIX
def test_an_unwritable_telemetry_path_still_passes_through(tmp_path: Path) -> None:
    alias = _alias(tmp_path, "git")
    real = _recorder(tmp_path / "real" / "git", exit_code=5)
    env = _env(tmp_path, alias, CLUD_GIT_SHIM_TARGET=real)
    _state(tmp_path).write_text("a file where the state dir should be", encoding="utf-8")
    result = _run([str(alias), "status"], env, tmp_path)
    assert result.returncode == 5, result
    assert result.stdout.splitlines() == ["status"]
    assert result.stderr == ""


@POSIX
def test_a_child_killed_by_a_signal_kills_the_shim_the_same_way(tmp_path: Path) -> None:
    alias = _alias(tmp_path, "git")
    real = tmp_path / "real" / "git"
    real.parent.mkdir()
    real.write_text("#!/bin/sh\nkill -TERM $$\n", encoding="utf-8")
    real.chmod(0o755)
    env = _env(tmp_path, alias, CLUD_GIT_SHIM_TARGET=real)
    result = _run([str(alias), "status"], env, tmp_path)
    assert result.returncode == -signal.SIGTERM, result
    assert [r["exit_code"] for r in _records(tmp_path)] == [128 + signal.SIGTERM]


# ---- no recursion, outside a session ----


@POSIX
def test_a_target_pointing_at_the_alias_never_recurses(tmp_path: Path) -> None:
    alias = _alias(tmp_path, "git")
    real = _recorder(tmp_path / "real" / "git")
    env = _env(tmp_path, alias, CLUD_GIT_SHIM_TARGET=alias)
    env["PATH"] = os.pathsep.join((str(alias.parent), str(real.parent)))
    result = _run([str(alias), "status"], env, tmp_path)
    assert result.returncode == 37, result
    assert result.stdout.splitlines() == ["status"]


@POSIX
def test_outside_a_session_git_is_the_real_git(tmp_path: Path) -> None:
    alias = _alias(tmp_path, "git")
    real = _recorder(tmp_path / "real" / "git")
    names = session_key_names(_binary("clud-shim"))
    env = {k: v for k, v in os.environ.items() if k not in names}
    env.update(
        CLUD_DAEMON_STATE_DIR=str(_state(tmp_path)),
        PATH=os.pathsep.join((str(alias.parent), str(real.parent))),
    )
    result = _run([str(alias), "clone", "https://github.com/zackees/clud"], env, tmp_path)
    assert result.returncode == 37, result
    assert result.stderr == ""
    assert _records(tmp_path) == [], "no telemetry outside a session"
    env["PATH"] = str(alias.parent)
    missing = _run([str(alias), "status"], env, tmp_path)
    assert missing.returncode == 127, missing
    assert missing.stderr.strip() == "git: command not found"


# ---- Windows ----


@pytest.mark.skipif(sys.platform != "win32", reason="Windows-native alias dispatch")
def test_windows_git_alias_relays_and_records(tmp_path: Path) -> None:
    shim = _alias(tmp_path, "git.exe")
    recorder = tmp_path / "real-git.exe"
    shutil.copy2(Path(os.environ["CLUD_TEST_MOCK_AGENT_BINARY"]), recorder)
    recorded = tmp_path / "argv.json"
    env = _env(tmp_path, shim, CLUD_GIT_SHIM_TARGET=recorder)
    env["MOCK_RM_STUB_LOG"] = str(recorded)
    result = _run([str(shim), "clone", "https://github.com/zackees/clud"], env, tmp_path)
    assert result.returncode == 0, result
    assert json.loads(recorded.read_text(encoding="utf-8")) == [
        "clone", "https://github.com/zackees/clud",
    ]
    records = _records(tmp_path)
    assert [(r["tool"], r["exit_code"]) for r in records] == [("git", 0)]
