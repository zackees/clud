"""#1741: without `--repo`, the watcher must watch the PR on `origin`.

In a fork with no `gh repo set-default`, a bare `gh pr view N` resolves to the
*parent* repository. A checkout of zackees/act2 (a fork of nektos/act) watched
nektos/act#1, printed `PR-STATE CLOSED` and exited as if that were its own PR.
"""

from __future__ import annotations

import importlib.util
import json
import sys
from dataclasses import dataclass
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "github" / "pr_merge_watch.py"

# gh's own default for a fork without `gh repo set-default`.
GH_DEFAULT_REPO = "nektos/act"


@pytest.fixture
def watcher():
    name = "clud_test_pr_merge_watch_1741"
    spec = importlib.util.spec_from_file_location(name, SCRIPT)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    try:
        yield module
    finally:
        sys.modules.pop(name, None)


@dataclass
class Result:
    returncode: int
    stdout: str
    stderr: str


class FakeProcesses:
    """Stands in for `RunningProcess`: a git with one origin, and a gh that
    resolves any call without `--repo` to the fork's parent, as gh does."""

    def __init__(self, origin: str | None) -> None:
        self.origin = origin
        self.gh_calls: list[list[str]] = []

    def run(self, argv, **_kwargs) -> Result:
        argv = [str(a) for a in argv]
        if argv[:1] == ["git"]:
            if argv[1:] == ["remote", "get-url", "origin"]:
                if self.origin is None:
                    return Result(2, "", "error: No such remote 'origin'\n")
                return Result(0, self.origin + "\n", "")
            return Result(128, "", "fatal: not a git repository\n")
        if argv[:1] != ["gh"]:
            return Result(127, "", f"unexpected command {argv}\n")
        args = argv[1:]
        self.gh_calls.append(args)
        repo = args[args.index("--repo") + 1] if "--repo" in args else GH_DEFAULT_REPO
        if args[:2] == ["pr", "view"]:
            # The parent's #1 is long closed; the fork's own #1 was merged.
            state = "CLOSED" if repo == GH_DEFAULT_REPO else "MERGED"
            return Result(0, json.dumps(_pr(state)), "")
        if args[:2] == ["repo", "view"]:
            return Result(0, json.dumps({"nameWithOwner": GH_DEFAULT_REPO}), "")
        return Result(1, "", f"unexpected gh call {args}\n")


def _pr(state: str) -> dict:
    return {
        "number": 1,
        "state": state,
        "mergeable": "UNKNOWN",
        "headRefOid": "abc123",
        "baseRefName": "main",
        "headRefName": "feat",
        "mergeStateStatus": "UNKNOWN",
    }


def _opts(watcher):
    return watcher.CancelOptions(set(), "none", 30, False, False, True, False)


def _watch(watcher, monkeypatch, origin: str | None) -> tuple[int, FakeProcesses]:
    fake = FakeProcesses(origin)
    monkeypatch.setattr(watcher, "RunningProcess", fake)
    with pytest.raises(SystemExit) as exc:
        watcher.watch(1, None, 1, 60, None, _opts(watcher), None)
    return exc.value.code, fake


def test_fork_without_repo_watches_origins_pr_not_the_parents(watcher, monkeypatch) -> None:
    code, fake = _watch(watcher, monkeypatch, "https://github.com/zackees/act2.git")

    assert fake.gh_calls, "the watcher made no gh call"
    for args in fake.gh_calls:
        assert "--repo" in args, args
        assert args[args.index("--repo") + 1] == "zackees/act2", args
    # zackees/act2#1 merged; nektos/act#1 (closed) is somebody else's PR.
    assert code == watcher.EXIT_GREEN


def test_non_github_origin_falls_back_to_ghs_repo(watcher, monkeypatch) -> None:
    code, fake = _watch(watcher, monkeypatch, "https://gitlab.com/zackees/act2.git")

    assert ["repo", "view", "--json", "nameWithOwner"] in fake.gh_calls
    pr_views = [args for args in fake.gh_calls if args[:2] == ["pr", "view"]]
    assert pr_views
    for args in pr_views:
        assert args[args.index("--repo") + 1] == GH_DEFAULT_REPO, args
    assert code == watcher.EXIT_PR_CLOSED


@pytest.mark.parametrize(
    "url",
    [
        "https://github.com/zackees/act2.git",
        "https://github.com/zackees/act2",
        "https://github.com/zackees/act2/",
        "https://token@github.com/zackees/act2.git",
        "http://github.com/zackees/act2.git",
        "git@github.com:zackees/act2.git",
        "git@github.com:zackees/act2",
        "ssh://git@github.com/zackees/act2.git",
        "ssh://git@github.com:22/zackees/act2.git",
        "git://github.com/zackees/act2.git",
        "https://GitHub.com/zackees/act2.git",
    ],
)
def test_github_remote_urls_name_their_repo(watcher, url: str) -> None:
    assert watcher.github_repo_from_url(url) == "zackees/act2"


@pytest.mark.parametrize(
    "url",
    [
        "",
        "https://gitlab.com/zackees/act2.git",
        "https://github.com.evil.example/zackees/act2.git",
        "https://github.com/zackees",
        "/srv/git/act2.git",
        "https://github.com/zackees/act2/extra",
    ],
)
def test_other_remote_urls_are_not_github(watcher, url: str) -> None:
    assert watcher.github_repo_from_url(url) is None


def test_help_says_origin_is_the_default(watcher, capsys) -> None:
    with pytest.raises(SystemExit):
        watcher.parse_args(["--help"])
    out = " ".join(capsys.readouterr().out.split())
    assert "git remote get-url origin" in out
