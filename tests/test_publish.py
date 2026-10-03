"""Safety contracts for the release dispatcher."""

from __future__ import annotations

import pytest

from ci import publish


def test_dry_run_never_starts_a_workflow(monkeypatch, capsys) -> None:
    monkeypatch.setattr(publish, "read_project_meta", lambda: ("clud", "1.2.3"))
    monkeypatch.setattr(
        publish,
        "trigger",
        lambda *_args: (_ for _ in ()).throw(AssertionError("workflow started")),
    )

    assert publish.main(["--dry-run"]) == 0
    assert "no workflow was started" in capsys.readouterr().err


def test_release_dispatch_uses_the_tag_input(monkeypatch) -> None:
    commands: list[list[str]] = []
    responses = iter(["[]", '[{"databaseId": 42, "status": "queued"}]'])
    monkeypatch.setattr(publish, "detect_publish_ref", lambda: "main")
    monkeypatch.setattr(publish.time, "sleep", lambda _seconds: None)
    monkeypatch.setattr(publish, "run", lambda command: commands.append(command))
    monkeypatch.setattr(publish, "run_capture", lambda _command: next(responses))

    assert publish.trigger("owner/repo", "1.2.3") == 42
    dispatch = commands[0]
    assert "build-mode=release" not in dispatch
    assert "tag=1.2.3" in dispatch


def test_waiting_for_the_release_run_revalidates_with_its_etag(monkeypatch) -> None:
    """GHAPI-001 (#1743): the wait re-reads the run conditionally; an
    unchanged run is a free 304, which gh reports with exit 1."""
    seen: list[list[str]] = []

    def response(status: str, etag: str, body: str, code: int = 0):
        stdout = f"HTTP/2.0 {status}\r\nEtag: {etag}\r\n\r\n{body}"
        return publish.process.CompletedProcess([], code, stdout, "")

    replies = iter(
        [
            response("200 OK", '"e1"', '{"status":"in_progress"}'),
            response("304 Not Modified", '"e1"', "", code=1),
            response("200 OK", '"e2"', '{"status":"completed","conclusion":"success"}'),
        ]
    )

    def fake(command):
        seen.append(command)
        return next(replies)

    monkeypatch.setattr(publish, "run_capture_allow_failure", fake)
    monkeypatch.setattr(publish.time, "sleep", lambda _seconds: None)
    publish.wait_for_run("owner/repo", 7)
    assert seen[0] == ["gh", "api", "-i", "repos/owner/repo/actions/runs/7"]
    assert seen[1][-2:] == ["-H", 'If-None-Match: "e1"']
    assert seen[2][-2:] == ["-H", 'If-None-Match: "e1"']


def test_a_failed_release_run_is_reported(monkeypatch) -> None:
    stdout = 'HTTP/2.0 200 OK\r\n\r\n{"status":"completed","conclusion":"failure"}'
    monkeypatch.setattr(
        publish,
        "run_capture_allow_failure",
        lambda _command: publish.process.CompletedProcess([], 0, stdout, ""),
    )
    with pytest.raises(SystemExit, match="release failed: failure"):
        publish.wait_for_run("owner/repo", 7)
