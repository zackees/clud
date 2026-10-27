"""Issue context stays bounded while retaining deliberate continuation (#2229)."""

import importlib.util
import sys
from pathlib import Path

import pytest

TOOL = Path(__file__).resolve().parents[1] / "crates/clud-bin/assets/tools/github/issue_context.py"


@pytest.fixture
def issue_context():
    spec = importlib.util.spec_from_file_location("issue_context", TOOL)
    assert spec
    assert spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_large_issue_and_comments_are_bounded_with_attribution(issue_context):
    issue = {
        "number": 42,
        "title": "test issue",
        "state": "open",
        "updated_at": "2026-01-01T00:00:00Z",
        "body": "å" * 10000,
    }
    comments = [
        {"id": i, "updated_at": f"2026-01-{i:02}T00:00:00Z", "body": "detail" * 2000}
        for i in range(1, 21)
    ]
    snapshot = {"issue": issue, "comments": comments}
    first = issue_context.render_snapshot(snapshot, section="overview", offset=0, limit=2048)
    assert len(first.encode()) <= 2048
    assert len(first.encode()) * 10 < len(issue_context._encoded(snapshot).encode())
    assert "test issue" in first
    assert "open" in first
    assert "20" in first
    assert "continu" in first.lower()
    parsed = issue_context.json.loads(first)
    assert parsed["comments_omitted"] == 20
    assert parsed["remaining_characters"] == 10000 - len(parsed["text"])
    page = issue_context.render_snapshot(snapshot, section="comment:7", offset=0, limit=1024)
    assert len(page.encode()) <= 1024
    assert "7" in page
    assert "2026-01-07" in page
    assert "detail" in page
    assert "continu" in page.lower()
    body = issue_context.render_snapshot(snapshot, section="body", offset=100, limit=1024)
    assert len(body.encode()) <= 1024
    assert "å" in issue_context.json.loads(body)["text"]


def issue_record(count=0, updated="2026-01-01T00:00:00Z", host="api.github.com"):
    return {
        "number": 42,
        "url": f"https://{host}/repos/org/repo/issues/42",
        "title": "report",
        "state": "open",
        "updated_at": updated,
        "body": "body",
        "comments": count,
    }


def test_pagination_and_incomplete_snapshot_fail_closed(issue_context):
    pages = [
        [{"id": n, "body": "x", "updated_at": "t"} for n in range(1, 101)],
        [{"id": 101, "body": "last", "updated_at": "t"}],
    ]
    paths = []

    def get(path, host):
        paths.append((path, host))
        if "/comments?" in path:
            page = int(path.rsplit("=", 1)[1])
            return issue_context._encoded(pages[page - 1]).encode()
        return issue_context._encoded(issue_record(101, host="enterprise.example")).encode()

    snapshot = issue_context.fetch_snapshot("org/repo", 42, "enterprise.example", get)
    assert len(snapshot["comments"]) == 101
    assert len([p for p, _ in paths if "/comments?" in p]) == 2
    assert all(host == "enterprise.example" for _, host in paths)
    pages[1] = []
    with pytest.raises(ValueError, match="incomplete"):
        issue_context.fetch_snapshot("org/repo", 42, "enterprise.example", get=get)


def test_api_failure_and_changed_issue_are_not_success(issue_context):
    def failed(_path, _host):
        raise RuntimeError("API unavailable")

    with pytest.raises(RuntimeError, match="unavailable"):
        issue_context.fetch_snapshot("org/repo", 42, get=failed)
    seen = 0

    def changed(path, _host):
        nonlocal seen
        if "/comments?" in path:
            return b"[]"
        seen += 1
        return issue_context._encoded(issue_record(updated=f"2026-01-0{seen}T00:00:00Z")).encode()

    with pytest.raises(ValueError, match="changed during retrieval"):
        issue_context.fetch_snapshot("org/repo", 42, get=changed)


def test_comment_listing_cursor_and_missing_comment(issue_context):
    snapshot = {
        "issue": issue_record(80),
        "comments": [
            {"id": n, "updated_at": "2026-01-01T00:00:00Z", "body": "x"} for n in range(80)
        ],
    }
    first = issue_context.render_snapshot(snapshot, "comments", 0, 512)
    parsed = issue_context.json.loads(first)
    assert len(first.encode()) <= 512
    assert parsed["continuation"]
    next_offset = len(parsed["items"])
    second = issue_context.json.loads(
        issue_context.render_snapshot(snapshot, "comments", next_offset, 512)
    )
    assert second["items"][0]["id"] == next_offset
    with pytest.raises(ValueError, match="not found"):
        issue_context.render_snapshot(snapshot, "comment:999")


@pytest.mark.skipif(sys.platform == "win32", reason="private Windows artifacts tracked in #2246")
def test_snapshot_roundtrip_refresh_and_failure(issue_context, monkeypatch, tmp_path, capsys):
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(tmp_path))
    old = {
        "schema": 1,
        "repo": "org/repo",
        "issue_number": 42,
        "hostname": None,
        "issue": issue_record(2),
        "comments": [
            {"id": 1, "updated_at": "old", "body": "private old"},
            {"id": 2, "updated_at": "old", "body": "deleted"},
        ],
    }
    artifact = issue_context.save_snapshot(old)
    assert artifact.stat().st_mode & 0o077 == 0
    assert issue_context.load_snapshot(artifact) == old
    updated = {
        **old,
        "comments": [
            {"id": 1, "updated_at": "new", "body": "edited"},
            {"id": 3, "updated_at": "new", "body": "added"},
        ],
    }
    monkeypatch.setattr(issue_context, "fetch_snapshot", lambda *args: updated)
    assert issue_context.main(["--refresh", str(artifact)]) == 0
    out = capsys.readouterr().out
    assert '"added":1' in out
    assert '"deleted":1' in out
    assert '"edited":1' in out
    assert (
        issue_context.main(
            ["--artifact", str(artifact), "--section", "comment:1", "--limit", "1024"]
        )
        == 0
    )
    continuation = capsys.readouterr().out
    assert "private old" in continuation
    assert len(continuation.encode()) <= 1024
    assert (
        issue_context.main(["--artifact", str(artifact), "--section", "body", "--limit", "512"])
        == 0
    )
    assert len(capsys.readouterr().out.encode()) <= 512
    monkeypatch.setattr(
        issue_context, "save_snapshot", lambda _: (_ for _ in ()).throw(OSError("disk full"))
    )
    assert issue_context.main(["--refresh", str(artifact)]) == 1
    captured = capsys.readouterr()
    assert "I/O failed" in captured.err
    assert "private old" not in captured.err


@pytest.mark.skipif(sys.platform == "win32", reason="private Windows artifacts tracked in #2246")
def test_artifact_symlink_is_rejected(issue_context, monkeypatch, tmp_path):
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(tmp_path))
    snapshot = {
        "schema": 1,
        "repo": "org/repo",
        "issue_number": 42,
        "issue": issue_record(),
        "comments": [],
    }
    artifact = issue_context.save_snapshot(snapshot)
    link = artifact.parent / "issue-context-aaaaaaaa.json"
    link.symlink_to(artifact)
    with pytest.raises((OSError, ValueError)):
        issue_context.load_snapshot(link)


def test_invalid_cli_arguments_are_bounded(issue_context, capsys):
    assert issue_context.main(["--issue", "x" * 50000]) == 1
    assert len(capsys.readouterr().err.encode()) < 200


def test_hostile_github_stderr_is_not_exposed(issue_context, monkeypatch):
    class Result:
        returncode = 1
        stderr = b"\x1b[31mPRIVATE\nhttps://example.test/?token=secret"
        stdout = b""

    monkeypatch.setattr(issue_context.RunningProcess, "run", lambda *a, **kw: Result())
    with pytest.raises(RuntimeError) as exc:
        issue_context.github_get("repos/org/repo/issues/42")
    assert "PRIVATE" not in str(exc.value)
    assert "secret" not in str(exc.value)
    assert "\x1b" not in str(exc.value)


def test_comment_author_and_url_are_attributed(issue_context):
    snapshot = {
        "issue": issue_record(1),
        "comments": [
            {
                "id": 7,
                "body": "text",
                "updated_at": "now",
                "user": {"login": "alice"},
                "html_url": "https://github.com/org/repo/issues/42#issuecomment-7",
            }
        ],
    }
    listing = issue_context.json.loads(issue_context.render_snapshot(snapshot, "comments", 0, 1024))
    detail = issue_context.json.loads(issue_context.render_snapshot(snapshot, "comment:7", 0, 1024))
    assert listing["items"][0]["author"] == "alice"
    assert detail["url"].endswith("#issuecomment-7")
    snapshot["comments"][0]["html_url"] = "javascript:alert(1)"
    assert (
        issue_context.json.loads(issue_context.render_snapshot(snapshot, "comment:7", 0, 1024))[
            "url"
        ]
        is None
    )


def test_comment_listing_cannot_stall_on_oversized_metadata(issue_context):
    snapshot = {
        "issue": issue_record(1),
        "comments": [
            {
                "id": 5,
                "body": "x",
                "updated_at": "x" * 10000,
                "user": {"login": "a" * 10000},
                "html_url": "https://example.test/" + "x" * 10000,
            }
        ],
    }
    page = issue_context.json.loads(issue_context.render_snapshot(snapshot, "comments", 0, 512))
    assert page["items"][0]["id"] == 5
    assert page["items"][0]["updated_at"] == "unknown"
    assert page["continuation"] is None


def test_render_escapes_terminal_and_bidirectional_controls(issue_context):
    snapshot = {"issue": {**issue_record(), "body": "a\x9b\u202eb"}, "comments": []}
    result = issue_context.render_snapshot(snapshot, "body", 0, 512)
    assert "\x9b" not in result
    assert "\u202e" not in result
    assert issue_context.json.loads(result)["text"] == "a\x9b\u202eb"


def test_host_is_pinned_and_mismatched_response_is_rejected(issue_context, monkeypatch):
    monkeypatch.setenv("GH_HOST", "enterprise.example")
    seen = []

    def get(path, host):
        seen.append(host)
        if "/comments?" in path:
            return b"[]"
        return issue_context._encoded(issue_record(host="enterprise.example")).encode()

    snapshot = issue_context.fetch_snapshot("org/repo", 42, get=get)
    assert snapshot["hostname"] == "enterprise.example"
    monkeypatch.setenv("GH_HOST", "other.example")
    issue_context.fetch_snapshot("org/repo", 42, snapshot["hostname"], get=get)
    assert all(host == "enterprise.example" for host in seen)

    def wrong(_path, _host):
        return issue_context._encoded(issue_record()).encode()

    with pytest.raises(ValueError, match="requested issue"):
        issue_context.fetch_snapshot("org/repo", 42, "enterprise.example", get=wrong)


def test_github_reads_request_fresh_broker_data(issue_context, monkeypatch):
    class Result:
        returncode = 0
        stderr = b""
        stdout = b"{}"

    calls = []

    def run(*args, **kwargs):
        calls.append(kwargs["env"])
        return Result()

    monkeypatch.setattr(issue_context.RunningProcess, "run", run)
    assert issue_context.github_get("repos/org/repo/issues/42") == b"{}"
    assert calls[0]["CLUD_GH_FRESH"] == "1"


def test_missing_or_invalid_issue_timestamp_fails(issue_context):
    def get(_path, _host):
        record = issue_record()
        record["updated_at"] = "9999-99-99T99:99:99Z"
        return issue_context._encoded(record).encode()

    with pytest.raises(ValueError, match="timestamp"):
        issue_context.fetch_snapshot("org/repo", 42, get=get)


def test_github_capture_is_bounded_before_return(issue_context, monkeypatch):
    class Result:
        returncode = 141
        stderr = b""
        stdout = b"x" * (issue_context.MAX_SNAPSHOT + 1)

    calls = []

    def run(args, **kwargs):
        calls.append(args)
        return Result()

    monkeypatch.setattr(issue_context.RunningProcess, "run", run)
    with pytest.raises(ValueError, match="exceeds"):
        issue_context.github_get("repos/org/repo/issues/42")
    assert "head -c 33554433" in calls[0][4]
    assert "head -c 4096" in calls[0][4]


def test_text_continuation_never_reuses_unchanged_offset(issue_context):
    snapshot = {
        "issue": issue_record(1),
        "comments": [
            {
                "id": 1,
                "body": "😀",
                "updated_at": "2026-01-01T00:00:00Z",
                "user": {"login": "a" * 100},
                "html_url": "https://example.test/" + "a" * 275,
            }
        ],
    }
    with pytest.raises(ValueError, match="no room"):
        issue_context.render_snapshot(snapshot, "comment:1", 0, 512)


@pytest.mark.skipif(sys.platform != "win32", reason="Windows-only privacy gate")
def test_windows_fails_before_fetch(issue_context, monkeypatch, capsys):
    calls = []
    monkeypatch.setattr(issue_context, "fetch_snapshot", lambda *args: calls.append(args))
    assert issue_context.main(["--repo", "org/repo", "--issue", "42"]) == 1
    assert calls == []
    assert "privacy" in capsys.readouterr().err
