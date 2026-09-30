"""Process tests for the in-session git/gh clone redirect (#1486).

Inside a valid session the `git` alias refuses `clone` / `worktree add` and
the `gh` alias refuses `repo clone` and the `--clone` forms of fork/create,
each with a message naming `safe-gh-clone` / `safe-gh-worktree` and a path
that already exists under `~/.clud/tmp-wt`. Everything else reaches the real
binary unchanged. HOME is a tempdir, so no test touches the real home.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import sys
from pathlib import Path

import pytest

from tests import process
from tests.shim_env import session_env, session_key_names

POSIX = pytest.mark.skipif(sys.platform == "win32", reason="POSIX recording fixtures")
RESERVED = re.compile(r"^\s*-> (.+?)  \(reserved now", re.MULTILINE)


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
    """A fake real binary: echoes argv (one per line) and appends it to $REAL_LOG."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        '#!/bin/sh\nprintf "%s\\n" "$@"\n'
        'printf "%s\\n" "$*" >> "$REAL_LOG"\n'
        f"exit {exit_code}\n",
        encoding="utf-8",
    )
    path.chmod(0o755)
    return path


def _env(tmp_path: Path, alias: Path, **targets: Path) -> dict[str, str]:
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    env = os.environ.copy() | session_env(_binary("clud-shim"), alias.parent)
    env.update(HOME=str(home), USERPROFILE=str(home), REAL_LOG=str(tmp_path / "real.log"))
    env.update({key: str(value) for key, value in targets.items()})
    return env


def _run(argv: list[str], env: dict[str, str], cwd: Path):
    return process.run(
        argv, env=env, cwd=str(cwd), capture_output=True, text=True, timeout=30
    )


def _tmp_wt(tmp_path: Path) -> Path:
    return tmp_path / "home" / ".clud" / "tmp-wt"


def _reserved(stderr: str) -> Path:
    match = RESERVED.search(stderr)
    assert match, stderr
    return Path(match.group(1))


# ---- pass-through (acceptance 4) ----


@POSIX
@pytest.mark.parametrize(
    "args",
    [
        ["status"],
        ["push", "--force-with-lease"],
        ["-C", ".", "worktree", "list"],
        ["worktree", "remove", "../x"],
        ["submodule", "update", "--init"],
        ["commit", "-m", "clone a b"],
    ],
)
def test_git_passes_everything_else_through_unchanged(tmp_path: Path, args: list[str]) -> None:
    alias = _alias(tmp_path, "git")
    real = _recorder(tmp_path / "real" / "git")
    env = _env(tmp_path, alias, CLUD_GIT_SHIM_TARGET=real)
    result = _run([str(alias), *args], env, tmp_path)
    assert result.returncode == 37, result
    assert result.stdout.splitlines() == args
    assert result.stderr == ""


@POSIX
@pytest.mark.parametrize(
    "args", [["issue", "list"], ["pr", "view", "--json", "state"], ["repo", "fork", "a/b"]]
)
def test_gh_passes_everything_else_through_unchanged(tmp_path: Path, args: list[str]) -> None:
    alias = _alias(tmp_path, "gh")
    real = _recorder(tmp_path / "real" / "gh")
    env = _env(tmp_path, alias, CLUD_GH_SHIM_TARGET=real)
    result = _run([str(alias), *args], env, tmp_path)
    assert result.returncode == 37, result
    assert result.stdout.splitlines() == args


# ---- refusals (acceptance 1, 2, 3) ----


@POSIX
def test_git_clone_is_refused_with_a_reserved_path(tmp_path: Path) -> None:
    alias = _alias(tmp_path, "git")
    real = _recorder(tmp_path / "real" / "git")
    env = _env(tmp_path, alias, CLUD_GIT_SHIM_TARGET=real)
    work = tmp_path / "work"
    work.mkdir()
    result = _run(
        [str(alias), "clone", "https://github.com/zackees/mimalloc-pprof"], env, work
    )
    assert result.returncode != 0, result
    assert result.stderr.splitlines()[0] == "git clone is redirected inside a clud session"
    assert "safe-gh-clone" in result.stderr
    assert not (tmp_path / "real.log").exists(), "the command must never run"
    assert list(work.iterdir()) == [], "no clone directory was created"
    reserved = _reserved(result.stderr)
    assert reserved.is_dir(), "the printed path exists"
    assert reserved.parent == _tmp_wt(tmp_path)
    assert reserved.name == "mimalloc-pprof-wt-clone"


@POSIX
def test_git_worktree_add_is_refused_with_a_reserved_wt_path(tmp_path: Path) -> None:
    alias = _alias(tmp_path, "git")
    real = _recorder(tmp_path / "real" / "git")
    env = _env(tmp_path, alias, CLUD_GIT_SHIM_TARGET=real)
    repo = tmp_path / "clud"
    (repo / ".git").mkdir(parents=True)
    result = _run(
        [str(alias), "worktree", "add", "./x", "-b", "feat/432-b", "origin/main"], env, repo
    )
    assert result.returncode != 0, result
    assert "safe-gh-worktree" in result.stderr
    assert "-wt-" in result.stderr
    assert not (tmp_path / "real.log").exists()
    assert not (repo / "x").exists()
    reserved = _reserved(result.stderr)
    assert reserved.is_dir()
    assert reserved == _tmp_wt(tmp_path) / "clud-wt-432"
    assert "safe-gh-worktree clud 432 --path" in result.stderr
    assert "-b feat/432-b origin/main" in result.stderr


@POSIX
@pytest.mark.parametrize(
    ("args", "headline"),
    [
        (["repo", "clone", "zackees/clud"], "gh repo clone is redirected"),
        (["repo", "fork", "zackees/clud", "--clone"], "gh repo fork --clone is redirected"),
        (["repo", "create", "thing", "--clone"], "gh repo create --clone is redirected"),
    ],
)
def test_gh_clone_forms_are_refused(tmp_path: Path, args: list[str], headline: str) -> None:
    alias = _alias(tmp_path, "gh")
    real = _recorder(tmp_path / "real" / "gh")
    env = _env(tmp_path, alias, CLUD_GH_SHIM_TARGET=real)
    result = _run([str(alias), *args], env, tmp_path)
    assert result.returncode != 0, result
    assert result.stderr.startswith(headline), result.stderr
    assert "safe-gh-clone" in result.stderr
    assert not (tmp_path / "real.log").exists()
    assert _reserved(result.stderr).is_dir()


# ---- helpers (acceptance 6) ----


@POSIX
def test_safe_gh_worktree_takes_ordinals_and_prints_existing_paths(tmp_path: Path) -> None:
    alias = _alias(tmp_path, "safe-gh-worktree")
    real = _recorder(tmp_path / "real" / "git", exit_code=0)
    env = _env(tmp_path, alias, CLUD_GIT_SHIM_TARGET=real)
    printed = []
    for _ in range(2):
        result = _run(
            [str(alias), "clud", "432", "-b", "feat/x", "origin/main"], env, tmp_path
        )
        assert result.returncode == 0, result
        path = Path(result.stdout.splitlines()[-1])
        assert path.exists(), "valid at the time it is printed"
        printed.append(path)
    root = _tmp_wt(tmp_path)
    assert printed == [root / "clud-wt-432", root / "clud-wt-432-2"]
    calls = (tmp_path / "real.log").read_text(encoding="utf-8").splitlines()
    assert calls == [f"worktree add {p} -b feat/x origin/main" for p in printed]


@POSIX
def test_safe_gh_worktree_uses_the_path_a_refusal_reserved(tmp_path: Path) -> None:
    git = _alias(tmp_path, "git")
    helper = _alias(tmp_path, "safe-gh-worktree")
    real = _recorder(tmp_path / "real" / "git", exit_code=0)
    env = _env(tmp_path, git, CLUD_GIT_SHIM_TARGET=real)
    repo = tmp_path / "clud"
    (repo / ".git").mkdir(parents=True)
    refused = _run([str(git), "worktree", "add", "../x", "-b", "feat/7-y"], env, repo)
    reserved = _reserved(refused.stderr)
    result = _run(
        [str(helper), "clud", "7", "--path", str(reserved), "-b", "feat/7-y"], env, repo
    )
    assert result.returncode == 0, result
    assert Path(result.stdout.splitlines()[-1]) == reserved.resolve()
    assert sorted(p.name for p in _tmp_wt(tmp_path).iterdir()) == ["clud-wt-7"]


@POSIX
def test_safe_gh_clone_runs_the_real_git_or_gh(tmp_path: Path) -> None:
    alias = _alias(tmp_path, "safe-gh-clone")
    git = _recorder(tmp_path / "realgit" / "git", exit_code=0)
    gh = _recorder(tmp_path / "realgh" / "gh", exit_code=0)
    env = _env(tmp_path, alias, CLUD_GIT_SHIM_TARGET=git, CLUD_GH_SHIM_TARGET=gh)
    url = _run([str(alias), "https://github.com/zackees/clud", "--depth", "1"], env, tmp_path)
    assert url.returncode == 0, url
    first = Path(url.stdout.splitlines()[-1])
    slug = _run([str(alias), "zackees/clud"], env, tmp_path)
    assert slug.returncode == 0, slug
    second = Path(slug.stdout.splitlines()[-1])
    root = _tmp_wt(tmp_path)
    assert (first, second) == (root / "clud-wt-clone", root / "clud-wt-clone-2")
    assert first.exists()
    assert second.exists()
    assert (tmp_path / "real.log").read_text(encoding="utf-8").splitlines() == [
        f"clone --depth 1 https://github.com/zackees/clud {first}",
        f"repo clone zackees/clud {second}",
    ]


@POSIX
def test_a_failed_helper_releases_its_own_reservation(tmp_path: Path) -> None:
    alias = _alias(tmp_path, "safe-gh-worktree")
    real = _recorder(tmp_path / "real" / "git", exit_code=128)
    env = _env(tmp_path, alias, CLUD_GIT_SHIM_TARGET=real)
    result = _run([str(alias), "clud", "9"], env, tmp_path)
    assert result.returncode == 128, result
    assert not (_tmp_wt(tmp_path) / "clud-wt-9").exists()


# ---- outside a session (acceptance 7) ----


@POSIX
def test_outside_a_session_git_clone_is_the_real_git(tmp_path: Path) -> None:
    alias = _alias(tmp_path, "git")
    real = _recorder(tmp_path / "real" / "git")
    names = session_key_names(_binary("clud-shim"))
    env = {k: v for k, v in os.environ.items() if k not in names}
    env.update(
        HOME=str(tmp_path / "home"),
        REAL_LOG=str(tmp_path / "real.log"),
        PATH=os.pathsep.join((str(alias.parent), str(real.parent))),
    )
    result = _run([str(alias), "clone", "https://github.com/zackees/clud"], env, tmp_path)
    assert result.returncode == 37, result
    assert result.stderr == ""
    assert not _tmp_wt(tmp_path).exists(), "no reservation outside a session"
    env["PATH"] = str(alias.parent)
    missing = _run([str(alias), "status"], env, tmp_path)
    assert missing.returncode == 127, missing
    assert missing.stderr.strip() == "git: command not found"


# ---- Windows (acceptance 8) ----


@pytest.mark.skipif(sys.platform != "win32", reason="Windows-native alias dispatch")
def test_windows_git_alias_relays_and_refuses(tmp_path: Path) -> None:
    shim = _alias(tmp_path, "git.exe")
    recorder = tmp_path / "real-git.exe"
    shutil.copy2(Path(os.environ["CLUD_TEST_MOCK_AGENT_BINARY"]), recorder)
    recorded = tmp_path / "argv.json"
    env = _env(tmp_path, shim, CLUD_GIT_SHIM_TARGET=recorder)
    env["MOCK_RM_STUB_LOG"] = str(recorded)
    plain = _run([str(shim), "status", "--short"], env, tmp_path)
    assert plain.returncode == 0, plain
    assert json.loads(recorded.read_text(encoding="utf-8")) == ["status", "--short"]
    recorded.unlink()
    refused = _run([str(shim), "clone", "https://github.com/zackees/clud"], env, tmp_path)
    assert refused.returncode != 0, refused
    assert "safe-gh-clone" in refused.stderr
    assert not recorded.exists(), "the refused clone never ran"
    reserved = _reserved(refused.stderr)
    assert reserved.is_dir()
    assert "\\" in str(reserved), "native separators"
    assert "/" not in str(reserved), "native separators"
