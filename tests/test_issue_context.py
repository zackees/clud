"""Unit coverage for the bundled bounded issue reader (#2229)."""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

TOOL_DIR = (
    Path(__file__).resolve().parents[1] / "crates" / "clud-bin" / "assets" / "tools" / "github"
)


def _load():
    if str(TOOL_DIR) not in sys.path:
        sys.path.insert(0, str(TOOL_DIR))
    import issue_context

    return issue_context


ic = _load()


def _comment(cid: int, body: str, author: str = "alice", updated: str = "2026-10-20T00:00:00Z"):
    return {
        "id": cid,
        "user": {"login": author},
        "created_at": "2026-10-19T00:00:00Z",
        "updated_at": updated,
        "body": body,
    }


def _issue(body: str, comment_count: int) -> str:
    return json.dumps(
        {
            "number": 7,
            "title": "make the build quieter",
            "state": "open",
            "state_reason": None,
            "user": {"login": "zackees"},
            "labels": [{"name": "enhancement"}],
            "created_at": "2026-10-18T00:00:00Z",
            "updated_at": "2026-10-21T00:00:00Z",
            "comments": comment_count,
            "html_url": "https://github.com/o/r/issues/7",
            "body": body,
        }
    )


def _snapshot(body: str, comments_json: str) -> dict:
    return {
        "v": 1,
        "fetched_at": "2026-10-27T00:00:00Z",
        "repo": "o/r",
        "number": 7,
        "issue": ic.parse_issue(_issue(body, len(ic.parse_comments(comments_json)))),
        "comments": ic.parse_comments(comments_json),
    }


class FakeGh:
    """A scripted `gh` that records every invocation."""

    def __init__(self, issue_json: str, comments_json: str | None = None):
        self.issue_json = issue_json
        self.comments_json = comments_json
        self.calls: list[tuple[str, ...]] = []
        self.fail_issue = False

    def __call__(self, *args, timeout=None):
        self.calls.append(args)
        if args[:2] == ("api", "repos/o/r/issues/7"):
            if self.fail_issue:
                return ic.GhResult(1, "", "gh: HTTP 404: Not Found")
            return ic.GhResult(0, self.issue_json, "")
        if args[:3] == ("api", "--paginate", "repos/o/r/issues/7/comments"):
            return ic.GhResult(0, self.comments_json or "", "")
        raise AssertionError(f"unexpected gh call: {args}")


@pytest.fixture
def home(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.delenv("USERPROFILE", raising=False)
    monkeypatch.delenv("CLUD_TOOL_OUTPUT_DIR", raising=False)
    return tmp_path


def _many_comments(count: int, size: int) -> str:
    return json.dumps([_comment(100 + i, f"comment {i} " + "y" * size) for i in range(count)])


def test_summary_is_bounded_and_reports_omissions():
    big = "x" * 50_000
    comments = _many_comments(40, 5_000)
    out = ic.render_text(
        _snapshot(big, comments), {"new": [], "edited": []}, max_bytes=8_000, note=""
    )
    assert len(out.encode()) <= 8_600
    assert "open" in out
    assert "make the build quieter" in out
    assert "comments: 40" in out
    assert "omitted" in out
    assert "continuation:" in out


def test_full_snapshot_is_saved_outside_the_checkout(home, monkeypatch):
    comments = json.dumps([_comment(101, "a" * 3_000), _comment(102, "b" * 3_000)])
    monkeypatch.setattr(ic, "gh", FakeGh(_issue("body", 2), comments))

    ic._load_or_fetch("o/r", 7, refresh=False, ttl=300)
    path = ic.snapshot_path("o/r", 7)
    assert path.exists()
    assert str(home) in str(path)
    assert "assets" not in str(path)
    saved = json.loads(path.read_text())
    assert [c["id"] for c in saved["comments"]] == [101, 102]
    assert saved["comments"][0]["body"].startswith("a")


def test_continuation_prints_one_comment_in_full(home, monkeypatch):
    comments = json.dumps([_comment(101, "short"), _comment(102, "z" * 9_000)])
    monkeypatch.setattr(ic, "gh", FakeGh(_issue("body", 2), comments))
    snap, _delta, _note = ic._load_or_fetch("o/r", 7, refresh=True, ttl=300)

    text = ic.render_comment(snap, 102, max_bytes=24_000)
    assert "#102" in text
    assert text.count("z") > 5_000


def test_missing_comment_is_a_hard_error_not_silence(home, monkeypatch):
    comments = json.dumps([_comment(101, "hi")])
    monkeypatch.setattr(ic, "gh", FakeGh(_issue("body", 1), comments))
    snap, _delta, _note = ic._load_or_fetch("o/r", 7, refresh=True, ttl=300)
    with pytest.raises(ic.NotFoundError):
        ic.render_comment(snap, 999, max_bytes=24_000)


def test_refresh_reports_new_and_edited_comments(home, monkeypatch):
    first = json.dumps([_comment(101, "one"), _comment(102, "two")])
    monkeypatch.setattr(ic, "gh", FakeGh(_issue("body", 2), first))
    ic._load_or_fetch("o/r", 7, refresh=True, ttl=300)

    edited = _comment(101, "one", updated="2026-10-21T00:00:00Z")
    second = json.dumps([edited, _comment(103, "three")])
    monkeypatch.setattr(ic, "gh", FakeGh(_issue("body", 2), second))
    cur, delta, _note = ic._load_or_fetch("o/r", 7, refresh=True, ttl=300)

    assert delta == {"new": [103], "edited": [101]}
    text = ic.render_text(cur, delta, max_bytes=24_000, note="")
    assert "+1 new, 1 edited" in text


def test_long_line_and_unicode_are_truncated_on_a_boundary():
    text = "e\u0301" * 100  # combining accent: multi-byte in utf-8
    exc, trunc = ic.excerpt(text, 51)
    assert trunc
    assert len(exc.encode()) <= 51
    assert "\ufffd" not in exc


def test_reads_only_use_get_api_calls(home, monkeypatch):
    comments = json.dumps([_comment(101, "hi")])
    fake = FakeGh(_issue("body", 1), comments)
    monkeypatch.setattr(ic, "gh", fake)
    ic._load_or_fetch("o/r", 7, refresh=True, ttl=300)
    for call in fake.calls:
        assert call[0] == "api"
        joined = " ".join(call)
        for banned in ("-X", "--method", "POST", "-f", "-F", "--input", "--raw-field"):
            assert banned not in joined, f"{banned} in {joined}"


def test_a_404_is_a_not_found_exit_not_an_empty_read(home, monkeypatch, capsys):
    fake = FakeGh(_issue("body", 0))
    fake.fail_issue = True
    monkeypatch.setattr(ic, "gh", fake)
    code = ic.main(["--repo", "o/r", "7"])
    assert code == ic.EXIT_NOT_FOUND
    assert "NOT-FOUND" in capsys.readouterr().err


def test_bad_issue_argument_is_a_usage_error(home):
    assert ic.main(["--repo", "o/r", "not-a-number"]) == ic.EXIT_USAGE
