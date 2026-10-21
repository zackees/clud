"""Pre-pruning must cover the complete incoming main push."""

from __future__ import annotations

import json
from types import SimpleNamespace

from ci import cache_lockfile_changed


def test_lockfile_change_earlier_in_a_multi_commit_push_is_detected(monkeypatch, tmp_path):
    before = "1" * 40
    event = tmp_path / "event.json"
    event.write_text(json.dumps({"before": before}), encoding="utf-8")
    output = tmp_path / "output"
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event))
    monkeypatch.setenv("GITHUB_OUTPUT", str(output))

    def compare(argv, **_kwargs):
        changed = "Cargo.lock\n" if before in argv else ""
        return SimpleNamespace(returncode=0, stdout=changed, stderr="")

    monkeypatch.setattr(cache_lockfile_changed, "run", compare)
    assert cache_lockfile_changed.main() == 0
    assert output.read_text(encoding="utf-8") == "changed=true\n"


def test_unknown_push_base_conservatively_requires_pruning(monkeypatch, tmp_path):
    monkeypatch.delenv("GITHUB_EVENT_PATH", raising=False)
    output = tmp_path / "output"
    monkeypatch.setenv("GITHUB_OUTPUT", str(output))
    monkeypatch.setattr(
        cache_lockfile_changed,
        "run",
        lambda *_args, **_kwargs: SimpleNamespace(returncode=0, stdout="", stderr=""),
    )
    assert cache_lockfile_changed.main() == 0
    assert output.read_text(encoding="utf-8") == "changed=true\n"
