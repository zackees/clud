"""pr_merge_watch backs off on GitHub secondary rate limits (#2368)."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "github" / "pr_merge_watch.py"

LIMIT = "gh: You have exceeded a secondary rate limit. Please wait a few minutes (HTTP 403)"


@pytest.fixture
def watcher():
    name = "clud_test_pr_merge_watch_2368"
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


def test_detects_secondary_limit_only(watcher):
    assert watcher.is_secondary_rate_limit(LIMIT)
    assert watcher.is_secondary_rate_limit("API rate limit exceeded (HTTP 429)")
    assert not watcher.is_secondary_rate_limit("HTTP 403: Resource not accessible")
    assert not watcher.is_secondary_rate_limit("connection reset")


def test_delay_honors_retry_after_and_caps(watcher):
    assert watcher.rate_limit_delay("Retry-After: 42", 1) == 42
    assert watcher.rate_limit_delay("retry-after: 99999", 1) == watcher.RATE_LIMIT_MAX_SEC
    assert watcher.rate_limit_delay(LIMIT, 1) == watcher.RATE_LIMIT_BASE_SEC
    assert watcher.rate_limit_delay(LIMIT, 2) == 2 * watcher.RATE_LIMIT_BASE_SEC
    assert watcher.rate_limit_delay(LIMIT, 20) == watcher.RATE_LIMIT_MAX_SEC


def test_backoff_prints_actionable_line_and_stays_within_deadline(watcher, monkeypatch, capsys):
    slept: list[float] = []
    monkeypatch.setattr(watcher.time, "sleep", slept.append)
    watcher.note_rate_limit(LIMIT)
    deadline = watcher.time.monotonic() + 5
    assert watcher.rate_limit_backoff(deadline, None) is True
    assert 0 <= slept[0] <= 5
    assert "GitHub secondary rate limit; retrying after" in capsys.readouterr().err


def test_backoff_is_noop_without_a_limit_and_resets_streak(watcher, monkeypatch):
    monkeypatch.setattr(watcher.time, "sleep", lambda s: None)
    watcher.note_rate_limit(LIMIT)
    watcher.rate_limit_backoff(watcher.time.monotonic() + 100, None)
    assert watcher.RATE_LIMIT_STREAK == 1
    assert watcher.rate_limit_backoff(watcher.time.monotonic() + 100, None) is False
    assert watcher.RATE_LIMIT_STREAK == 0


def test_successful_gh_call_clears_a_stale_rate_limit(watcher, monkeypatch):
    class Res:
        def __init__(self, code, err):
            self.returncode, self.stdout, self.stderr = code, "", err

    results = iter([Res(1, LIMIT), Res(0, "")])
    monkeypatch.setattr(watcher.RunningProcess, "run", lambda *a, **k: next(results))
    watcher.gh("api", "x")
    assert watcher.PENDING_RATE_LIMIT is not None
    watcher.gh("api", "y")
    assert watcher.PENDING_RATE_LIMIT is None
