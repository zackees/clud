"""pr_merge_watch: page reviews, review threads and thread comments (issue #1617).

#1612 paged the check contexts only. `fetch_gate_snapshot` still returned no
snapshot when the reviews (100), review threads (100) or a thread's comments
(20) reported another page, so a large PR stalled the watcher. Each connection
must now be paged, pinned to the same pull request / thread node, and any
failed or malformed page must yield no snapshot: a partial list is never green.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "github" / "pr_merge_watch.py"
PR_NODE = "PR_node_1617"
CR = "coderabbitai[bot]"


@pytest.fixture
def watcher():
    name = "clud_test_pr_merge_watch_1617"
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


def conn(nodes: list[dict], cursor: str | None) -> dict:
    return {"nodes": nodes, "pageInfo": {"hasNextPage": cursor is not None, "endCursor": cursor}}


def review(i: int, login: str = "human", state: str = "COMMENTED") -> dict:
    return {"databaseId": i, "state": state, "author": {"login": login}}


def comment(i: int, login: str = "human") -> dict:
    return {"databaseId": i, "body": f"c{i}", "author": {"login": login}}


def thread(tid: str, comments: dict, resolved: bool = False) -> dict:
    return {"id": tid, "isResolved": resolved, "comments": comments}


def first_page(*, reviews: dict, threads: dict | None = None) -> dict:
    pull = {
        "id": PR_NODE,
        "number": 7,
        "state": "OPEN",
        "mergeable": "MERGEABLE",
        "mergeStateStatus": "CLEAN",
        "headRefOid": "abc",
        "baseRefName": "main",
        "headRefName": "feat",
        "reviews": reviews,
        "commits": {"nodes": [{"commit": {"statusCheckRollup": None}}]},
    }
    repository: dict = {"pullRequest": pull}
    if threads is not None:
        pull["reviewThreads"] = threads
        pull["comments"] = {"nodes": [], "pageInfo": {"hasPreviousPage": False}}
        repository["recent"] = {"nodes": []}
    return {"data": {"repository": repository}}


def node_page(field: str, connection: object) -> dict:
    return {"data": {"node": {field: connection}}}


def arg_value(args: tuple[str, ...], key: str) -> str | None:
    for arg in args:
        if arg.startswith(f"{key}="):
            return arg.split("=", 1)[1]
    return None


def kind(args: tuple[str, ...]) -> str:
    query = arg_value(args, "query") or ""
    if arg_value(args, "after") is None:
        return "first"
    for name in ("reviewThreads", "reviews", "comments"):
        if f"{name}(first:100,after:$after)" in query:
            return name
    return "unknown"


def fake_gh(pages: dict[tuple[str, str | None, str | None], object], calls: list):
    """Serve by (connection, pinned node id, cursor)."""

    def gh_json(*args: str):
        calls.append(args)
        if arg_value(args, "query") is None:
            return None  # REST head-check lookups: unavailable is fine
        key = (kind(args), arg_value(args, "id"), arg_value(args, "after"))
        return pages.get(key)

    return gh_json


def snapshot(watcher, monkeypatch, pages, *, coderabbit: bool, calls: list | None = None):
    monkeypatch.setattr(watcher, "gh_json", fake_gh(pages, [] if calls is None else calls))
    return watcher.fetch_gate_snapshot("o/r", 7, include_coderabbit=coderabbit)


def test_reviews_beyond_first_page_are_fetched(watcher, monkeypatch):
    calls: list = []
    pages = {
        ("first", None, None): first_page(
            reviews=conn([review(i, "bot[bot]") for i in range(100)], "r1")
        ),
        ("reviews", PR_NODE, "r1"): node_page("reviews", conn([review(500)], None)),
    }
    snap = snapshot(watcher, monkeypatch, pages, coderabbit=False, calls=calls)
    assert snap is not None, "a >100-review PR must still produce a snapshot"
    assert snap.human_review_ids == frozenset({500})


def test_review_threads_beyond_first_page_are_fetched(watcher, monkeypatch):
    resolved = [thread(f"T{i}", conn([comment(i, CR)], None), True) for i in range(100)]
    pages = {
        ("first", None, None): first_page(reviews=conn([], None), threads=conn(resolved, "t1")),
        ("reviewThreads", PR_NODE, "t1"): node_page(
            "reviewThreads", conn([thread("T500", conn([comment(500, CR)], None))], None)
        ),
    }
    snap = snapshot(watcher, monkeypatch, pages, coderabbit=True)
    assert snap is not None, "a >100-thread PR must still produce a snapshot"
    assert snap.coderabbit.actionable
    assert snap.coderabbit.ids == frozenset({500})


def test_thread_comments_beyond_first_page_are_fetched(watcher, monkeypatch):
    humans = [comment(i) for i in range(20)]
    pages = {
        ("first", None, None): first_page(
            reviews=conn([], None), threads=conn([thread("T1", conn(humans, "k1"))], None)
        ),
        ("comments", "T1", "k1"): node_page("comments", conn([comment(99, CR)], None)),
    }
    snap = snapshot(watcher, monkeypatch, pages, coderabbit=True)
    assert snap is not None, "a thread with >20 comments must still produce a snapshot"
    assert snap.coderabbit.actionable
    assert snap.coderabbit.ids == frozenset({99})


BAD_PAGES = [None, {"data": {"node": None}}, "more-then-fail"]
BAD_IDS = ["call-failed", "no-node", "third-page-failed"]


def _second(field: str, bad: object, node: dict) -> object:
    if bad == "more-then-fail":
        return node_page(field, conn([node], "next"))
    return bad


@pytest.mark.parametrize("bad", BAD_PAGES, ids=BAD_IDS)
def test_reviews_pagination_failure_is_never_green(watcher, monkeypatch, bad):
    pages = {
        ("first", None, None): first_page(reviews=conn([review(1)], "r1")),
        ("reviews", PR_NODE, "r1"): _second("reviews", bad, review(2)),
    }
    assert snapshot(watcher, monkeypatch, pages, coderabbit=False) is None


@pytest.mark.parametrize("bad", BAD_PAGES, ids=BAD_IDS)
def test_threads_pagination_failure_is_never_green(watcher, monkeypatch, bad):
    t = thread("T1", conn([], None))
    pages = {
        ("first", None, None): first_page(reviews=conn([], None), threads=conn([t], "t1")),
        ("reviewThreads", PR_NODE, "t1"): _second("reviewThreads", bad, thread("T2", conn([], None))),
    }
    assert snapshot(watcher, monkeypatch, pages, coderabbit=True) is None


@pytest.mark.parametrize("bad", BAD_PAGES, ids=BAD_IDS)
def test_thread_comments_pagination_failure_is_never_green(watcher, monkeypatch, bad):
    pages = {
        ("first", None, None): first_page(
            reviews=conn([], None), threads=conn([thread("T1", conn([comment(1)], "k1"))], None)
        ),
        ("comments", "T1", "k1"): _second("comments", bad, comment(2)),
    }
    assert snapshot(watcher, monkeypatch, pages, coderabbit=True) is None


def test_missing_thread_id_is_never_green(watcher, monkeypatch):
    t = {"isResolved": False, "comments": conn([comment(1)], "k1")}
    pages = {("first", None, None): first_page(reviews=conn([], None), threads=conn([t], None))}
    assert snapshot(watcher, monkeypatch, pages, coderabbit=True) is None


def test_missing_pr_node_id_is_never_green(watcher, monkeypatch):
    page = first_page(reviews=conn([review(1)], "r1"))
    del page["data"]["repository"]["pullRequest"]["id"]
    assert snapshot(watcher, monkeypatch, {("first", None, None): page}, coderabbit=False) is None
