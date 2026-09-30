"""pr_merge_watch: PRs with more than 100 check contexts (issue #1604).

`fetch_gate_snapshot` used to return None whenever the rollup's
`contexts(first:100)` page reported `hasNextPage`, so a PR with >100 checks
never produced a snapshot and the loop exited 10 (GITHUB-UNREACHABLE). It must
page the connection instead, and must never treat a partial rollup as the
whole one.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "github" / "pr_merge_watch.py"
HEAD = "abc123"


@pytest.fixture
def watcher():
    name = "clud_test_pr_merge_watch_1604"
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


def check_run(i: int, conclusion: str = "SUCCESS") -> dict:
    return {
        "__typename": "CheckRun",
        "name": f"job-{i:03d}",
        "status": "COMPLETED",
        "conclusion": conclusion,
        "detailsUrl": f"https://example.invalid/{i}",
    }


def connection(nodes: list[dict], cursor: str | None) -> dict:
    return {
        "nodes": nodes,
        "pageInfo": {"hasNextPage": cursor is not None, "endCursor": cursor},
    }


def first_page(nodes: list[dict], cursor: str | None) -> dict:
    return {
        "data": {
            "repository": {
                "pullRequest": {
                    "number": 581,
                    "state": "OPEN",
                    "mergeable": "MERGEABLE",
                    "mergeStateStatus": "CLEAN",
                    "headRefOid": HEAD,
                    "baseRefName": "main",
                    "headRefName": "feat",
                    "reviews": connection([], None),
                    "commits": {
                        "nodes": [
                            {
                                "commit": {
                                    "statusCheckRollup": {"contexts": connection(nodes, cursor)}
                                }
                            }
                        ]
                    },
                }
            }
        }
    }


def later_page(nodes: list[dict], cursor: str | None) -> dict:
    return {
        "data": {
            "repository": {
                "object": {"statusCheckRollup": {"contexts": connection(nodes, cursor)}}
            }
        }
    }


def arg_value(args: tuple[str, ...], key: str) -> str | None:
    for arg in args:
        if arg.startswith(f"{key}="):
            return arg.split("=", 1)[1]
    return None


def fake_gh(pages: dict[str | None, object], calls: list[tuple[str, ...]]):
    """Serve page by `after` cursor; the first query carries no cursor."""

    def gh_json(*args: str):
        calls.append(args)
        return pages.get(arg_value(args, "after"))

    return gh_json


def test_contexts_beyond_first_page_are_fetched(watcher, monkeypatch):
    calls: list[tuple[str, ...]] = []
    pages = {
        None: first_page([check_run(i) for i in range(100)], "c1"),
        "c1": later_page([check_run(i) for i in range(100, 138)], None),
    }
    monkeypatch.setattr(watcher, "gh_json", fake_gh(pages, calls))

    snapshot = watcher.fetch_gate_snapshot("zackees/mimalloc-pprof", 581, include_coderabbit=False)

    assert snapshot is not None, "a >100-context rollup must still produce a snapshot"
    assert len(snapshot.checks) == 138
    assert {row.name for row in snapshot.checks} == {f"job-{i:03d}" for i in range(138)}
    # The follow-up page is pinned to the head commit that the first page saw.
    assert arg_value(calls[1], "oid") == HEAD


def test_failure_on_a_later_page_is_visible(watcher, monkeypatch):
    """A failing check past the first 100 must reach the gate."""
    pages = {
        None: first_page([check_run(i) for i in range(100)], "c1"),
        "c1": later_page([check_run(100, "FAILURE")], None),
    }
    monkeypatch.setattr(watcher, "gh_json", fake_gh(pages, []))

    snapshot = watcher.fetch_gate_snapshot("o/r", 581, include_coderabbit=False)

    assert snapshot is not None
    assert [row.name for row in snapshot.checks if row.bucket == "fail"] == ["job-100"]


@pytest.mark.parametrize(
    "second",
    [
        None,  # gh call failed
        {"data": {"repository": {"object": None}}},  # commit vanished
        later_page([check_run(100)], "c2"),  # claims more, but page 3 fails
    ],
    ids=["call-failed", "no-object", "third-page-failed"],
)
def test_pagination_failure_is_never_green(watcher, monkeypatch, second):
    pages: dict[str | None, object] = {
        None: first_page([check_run(i) for i in range(100)], "c1"),
        "c1": second,
    }
    monkeypatch.setattr(watcher, "gh_json", fake_gh(pages, []))

    assert watcher.fetch_gate_snapshot("o/r", 581, include_coderabbit=False) is None


def test_missing_cursor_is_never_green(watcher, monkeypatch):
    page = first_page([check_run(i) for i in range(100)], "c1")
    contexts = page["data"]["repository"]["pullRequest"]["commits"]["nodes"][0]["commit"][
        "statusCheckRollup"
    ]["contexts"]
    contexts["pageInfo"] = {"hasNextPage": True}
    monkeypatch.setattr(watcher, "gh_json", fake_gh({None: page}, []))

    assert watcher.fetch_gate_snapshot("o/r", 581, include_coderabbit=False) is None
