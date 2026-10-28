"""Pull-request context stays bounded while retaining deliberate continuation."""

import importlib.util
import json
from pathlib import Path

import pytest

TOOL = Path(__file__).resolve().parents[1] / "crates/clud-bin/assets/tools/github/pr_context.py"


@pytest.fixture
def pr_context():
    spec = importlib.util.spec_from_file_location("pr_context", TOOL)
    assert spec
    assert spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def pull_record(**overrides):
    pull = {
        "number": 42,
        "url": "https://api.github.com/repos/org/repo/pulls/42",
        "title": "bounded review",
        "state": "open",
        "draft": False,
        "merged": False,
        "user": {"login": "octocat"},
        "base": {"ref": "main"},
        "head": {"ref": "feat/x", "sha": "a" * 40},
        "mergeable_state": "clean",
        "changed_files": 7,
        "additions": 120,
        "deletions": 9,
        "body": "description " * 5000,
        "updated_at": "2026-01-01T00:00:00Z",
        "comments": 0,
    }
    pull.update(overrides)
    return pull


def snapshot_of(pull=None, reviews=(), comments=(), threads=(), checks=()):
    return {
        "schema": 1,
        "repo": "org/repo",
        "pull_number": 42,
        "hostname": "github.com",
        "pull": pull or pull_record(comments=len(comments)),
        "reviews": list(reviews),
        "review_comments": list(threads),
        "comments": list(comments),
        "checks": list(checks),
    }


def test_overview_is_bounded_and_keeps_the_big_body_available(pr_context):
    reviews = [
        {"id": i, "state": "APPROVED", "user": {"login": "rev"}, "body": "detail" * 2000}
        for i in range(1, 6)
    ]
    comments = [
        {
            "id": 100 + i,
            "user": {"login": "rev"},
            "body": "chat" * 2000,
            "updated_at": "2026-01-02T00:00:00Z",
        }
        for i in range(20)
    ]
    snapshot = snapshot_of(reviews=reviews, comments=comments)
    view = pr_context.render_snapshot(snapshot, section="overview", offset=0, limit=2048)

    assert len(view.encode()) <= 2048
    assert len(view.encode()) * 10 < len(pr_context._encoded(snapshot).encode())
    parsed = json.loads(view)
    assert parsed["number"] == 42
    assert parsed["reviews_by_state"] == {"APPROVED": 5}
    assert parsed["comment_count"] == 20
    assert parsed["comments_omitted"] == 20
    assert parsed["remaining_characters"] == 60000 - len(parsed["text"])
    assert "body offset" in parsed["continuation"]

    body = json.loads(
        pr_context.render_snapshot(snapshot, section="body", offset=59000, limit=1024)
    )
    assert 700 <= len(body["text"]) <= 1000
    assert body["remaining_characters"] == 1000 - len(body["text"])
    assert body["text"] == snapshot["pull"]["body"][59000 : 59000 + len(body["text"])]


def test_reviews_and_comments_are_listed_without_their_bodies(pr_context):
    reviews = [
        {
            "id": 7,
            "state": "CHANGES_REQUESTED",
            "user": {"login": "rev"},
            "submitted_at": "2026-01-03T00:00:00Z",
            "body": "x" * 40000,
        }
    ]
    comments = [
        {
            "id": 9,
            "user": {"login": "rev"},
            "body": "y" * 40000,
            "updated_at": "2026-01-04T00:00:00Z",
        }
    ]
    snapshot = snapshot_of(reviews=reviews, comments=comments)

    listed = json.loads(pr_context.render_snapshot(snapshot, section="reviews", limit=1024))
    assert listed["count"] == 1
    assert listed["items"][0] == {
        "id": 7,
        "author": "rev",
        "state": "CHANGES_REQUESTED",
        "submitted_at": "2026-01-03T00:00:00Z",
        "body_characters": 40000,
    }
    assert listed["continuation"] is None

    review_body = json.loads(pr_context.render_snapshot(snapshot, section="review:7", limit=1024))
    assert len(review_body["text"]) > 700
    assert set(review_body["text"]) == {"x"}
    assert review_body["remaining_characters"] > 0
    assert "review:7 offset" in review_body["continuation"]

    thread_text = pr_context.render_snapshot(snapshot, section="comment:9", limit=1024)
    assert json.loads(thread_text)["remaining_characters"] > 0


def test_thread_comments_report_path_and_line(pr_context):
    threads = [
        {
            "id": 11,
            "user": {"login": "rev"},
            "body": "b" * 500,
            "path": "src/lib.rs",
            "line": 88,
            "updated_at": "2026-01-05T00:00:00Z",
        }
    ]
    snapshot = snapshot_of(threads=threads)
    listed = json.loads(pr_context.render_snapshot(snapshot, section="review-comments", limit=1024))
    assert listed["items"][0]["path"] == "src/lib.rs"
    assert listed["items"][0]["line"] == 88


def test_check_summary_names_the_failures_but_bounds_them(pr_context):
    stored = [
        {"id": i, "name": f"job{i}", "status": "completed", "conclusion": "success"}
        for i in range(1, 31)
    ]
    stored += [
        {"id": 100 + i, "name": f"fail{i}", "status": "completed", "conclusion": "failure"}
        for i in range(1, 26)
    ]
    summary = pr_context.check_summary(stored)
    assert summary["total"] == 55
    assert summary["by_state"] == {"success": 30, "failure": 25}
    assert len(summary["not_successful"]) == pr_context.MAX_FAILING_CHECKS
    assert summary["not_successful"][0] == {"name": "fail1", "state": "failure"}


def test_in_flight_checks_are_not_reported_as_passing(pr_context):
    summary = pr_context.check_summary(
        [{"id": 1, "name": "queued", "status": "in_progress", "conclusion": None}]
    )
    assert summary["by_state"] == {"in_progress": 1}
    assert summary["not_successful"] == [{"name": "queued", "state": "in_progress"}]


class FakeGitHub:
    """Serve the fixed paths fetch_snapshot asks for, one page each."""

    def __init__(self, pull=None, reviews=(), comments=(), threads=(), checks=()):
        self.pull = pull or pull_record(comments=len(list(comments)))
        self.lists = {
            "repos/org/repo/pulls/42/reviews": list(reviews),
            "repos/org/repo/pulls/42/comments": list(threads),
            "repos/org/repo/issues/42/comments": list(comments),
            f"repos/org/repo/commits/{'a' * 40}/check-runs": list(checks),
        }
        self.calls = []

    def __call__(self, path, hostname=None):
        self.calls.append((path, hostname))
        if path == "repos/org/repo/pulls/42":
            return json.dumps(self.pull).encode()
        key = path.split("?", 1)[0]
        if key in self.lists:
            if key.endswith("check-runs"):
                return json.dumps(
                    {"total_count": len(self.lists[key]), "check_runs": self.lists[key]}
                ).encode()
            return json.dumps(self.lists[key]).encode()
        raise AssertionError(f"unexpected path {path}")


def test_snapshot_fetch_rejects_a_pull_request_that_changed_midway(pr_context):
    class Changing(FakeGitHub):
        def __init__(self):
            super().__init__()
            self.seen = 0

        def __call__(self, path, hostname=None):
            if path == "repos/org/repo/pulls/42":
                self.seen += 1
                if self.seen == 2:
                    return json.dumps(pull_record(updated_at="2026-01-09T00:00:00Z")).encode()
            return super().__call__(path, hostname)

    with pytest.raises(ValueError, match="changed during retrieval"):
        pr_context.fetch_snapshot("org/repo", 42, get=Changing())


def test_snapshot_fetch_rejects_duplicate_ids(pr_context):
    class Duplicates(FakeGitHub):
        def __init__(self):
            super().__init__()
            self.lists["repos/org/repo/pulls/42/reviews"] = [{"id": 5, "state": "APPROVED"}] * 2

    with pytest.raises(ValueError, match="incomplete or inconsistent"):
        pr_context.fetch_snapshot("org/repo", 42, get=Duplicates())


def test_check_runs_are_unwrapped_from_their_envelope(pr_context):
    checks = [
        {"id": 1, "name": "build", "status": "completed", "conclusion": "failure"},
        {"id": 2, "name": "lint", "status": "completed", "conclusion": "success"},
    ]
    snapshot = pr_context.fetch_snapshot("org/repo", 42, get=FakeGitHub(checks=checks))
    assert [check["name"] for check in snapshot["checks"]] == ["build", "lint"]
    assert pr_context.check_summary(snapshot["checks"])["by_state"] == {
        "failure": 1,
        "success": 1,
    }


def test_an_incomplete_check_run_page_fails_closed(pr_context):
    class Truncated(FakeGitHub):
        def __call__(self, path, hostname=None):
            if path.split("?", 1)[0].endswith("check-runs"):
                return json.dumps({"total_count": 5, "check_runs": []}).encode()
            return super().__call__(path, hostname)

    with pytest.raises(ValueError, match="check-run pagination was incomplete"):
        pr_context.fetch_snapshot("org/repo", 42, get=Truncated())


def test_snapshot_fetch_rejects_a_mismatched_pull_request(pr_context):
    class WrongRepo(FakeGitHub):
        def __init__(self):
            super().__init__(
                pull=pull_record(url="https://api.github.com/repos/evil/repo/pulls/42")
            )

    with pytest.raises(ValueError, match="did not return the requested pull request"):
        pr_context.fetch_snapshot("org/repo", 42, get=WrongRepo())


def test_artifact_paths_outside_the_private_directory_are_rejected(pr_context, tmp_path):
    stray = tmp_path / "pr-context-abcdefgh.json"
    stray.write_text("{}")
    with pytest.raises(ValueError, match="outside the private"):
        pr_context.load_snapshot(stray)


def test_unknown_section_fails_loudly(pr_context):
    with pytest.raises(ValueError, match="section must be"):
        pr_context.render_snapshot(snapshot_of(), section="diff", limit=1024)


def test_refresh_reports_only_what_changed(pr_context):
    old = snapshot_of(
        reviews=[{"id": 1, "state": "APPROVED"}],
        comments=[{"id": 2, "updated_at": "2026-01-01T00:00:00Z", "body": "same"}],
        threads=[{"id": 3, "body": "old"}],
    )
    new = snapshot_of(
        pull=pull_record(comments=2, updated_at="2026-01-06T00:00:00Z"),
        reviews=[{"id": 1, "state": "APPROVED"}, {"id": 4, "state": "COMMENTED"}],
        comments=[
            {"id": 2, "updated_at": "2026-01-02T00:00:00Z", "body": "edited"},
            {"id": 5, "updated_at": "2026-01-03T00:00:00Z", "body": "new"},
        ],
        threads=[{"id": 6, "body": "new thread"}],
    )
    changes = pr_context.refresh_changes(old, new)
    assert changes["comments_added"] == 1
    assert changes["comments_edited"] == 1
    assert changes["review_comments_deleted"] == 1
    assert changes["review_comments_added"] == 1
    assert changes["reviews"] == 1
    assert changes["pull_changed"] is True


def test_limit_range_and_selector_conflicts_are_rejected(pr_context, capsys):
    assert pr_context.main(["--repo", "org/repo", "--pr", "42", "--limit", "64"]) == 1
    assert "limit must be 512..8192" in capsys.readouterr().err
    assert pr_context.main(["--artifact", "/tmp/x.json", "--refresh", "/tmp/x.json"]) == 1
    assert "choose either" in capsys.readouterr().err
