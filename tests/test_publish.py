"""Safety contracts for the release dispatcher."""

from __future__ import annotations

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
