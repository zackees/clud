from __future__ import annotations

import json
from pathlib import Path

from bench.idle_cpu.harness import ProcessIdentity, _discard_reused_pids, _identity_matches
from bench.idle_cpu.report import EVENT_LINE_SLACK, assemble_report, budget_violations


def _report() -> dict:
    return assemble_report(
        head="abc123",
        timestamp="2026-07-22T00:00:00+00:00",
        sessions=1,
        window_secs=60,
        roles={10: "daemon", 20: "client-root"},
        before={
            10: {"cpu_seconds": 4.0, "ctx_switches": 100},
            20: {"cpu_seconds": 1.0, "ctx_switches": 10},
        },
        after={
            10: {"cpu_seconds": 4.2, "ctx_switches": 120},
            20: {"cpu_seconds": 1.5, "ctx_switches": 13},
        },
        event_lines_before=4,
        event_lines_after=9,
    )


def test_report_assembles_deltas_from_samples() -> None:
    report = _report()
    assert report["per_process"] == [
        {
            "role": "daemon",
            "pid": 10,
            "cpu_seconds": 0.2,
            "ctx_switches": 20,
            "missing_at_end": False,
        },
        {
            "role": "client-root",
            "pid": 20,
            "cpu_seconds": 0.5,
            "ctx_switches": 3,
            "missing_at_end": False,
        },
    ]
    assert report["totals"] == {
        "client_cpu_seconds": 0.5,
        "daemon_cpu_seconds": 0.2,
        "event_lines_appended": 5,
    }


def test_budget_pass_and_fail_boundaries() -> None:
    baseline = _report()
    baseline["totals"] = {
        "client_cpu_seconds": 1.0,
        "daemon_cpu_seconds": 1.0,
        "event_lines_appended": 10,
    }
    passing = _report()
    passing["totals"] = {
        "client_cpu_seconds": 1.19,
        "daemon_cpu_seconds": 1.19,
        "event_lines_appended": 10 + EVENT_LINE_SLACK,
    }
    failing = _report()
    failing["totals"] = {
        "client_cpu_seconds": 1.21,
        "daemon_cpu_seconds": 1.21,
        "event_lines_appended": 11 + EVENT_LINE_SLACK,
    }
    assert budget_violations(passing, baseline) == []
    assert len(budget_violations(failing, baseline)) == 3


def test_report_json_shape_is_stable() -> None:
    report = _report()
    assert set(report) == {
        "head",
        "mode",
        "timestamp",
        "sessions",
        "window_secs",
        "per_process",
        "totals",
    }
    assert report["mode"] == "daemon"
    assert set(report["per_process"][0]) == {
        "role",
        "pid",
        "cpu_seconds",
        "ctx_switches",
        "missing_at_end",
    }
    assert set(report["totals"]) == {
        "client_cpu_seconds",
        "daemon_cpu_seconds",
        "event_lines_appended",
    }


def test_reused_pid_is_discarded_from_second_sample() -> None:
    before = {10: {"create_time": 1.0, "cpu_seconds": 1.0, "ctx_switches": 1}}
    after = {10: {"create_time": 2.0, "cpu_seconds": 99.0, "ctx_switches": 99}}
    assert _discard_reused_pids(before, after) == {}


def test_process_identity_requires_matching_creation_time(monkeypatch) -> None:
    current = ProcessIdentity(pid=10, create_time=2.0)
    monkeypatch.setattr("bench.idle_cpu.harness._process_identity", lambda _pid: current)
    assert _identity_matches(ProcessIdentity(pid=10, create_time=2.0))
    assert not _identity_matches(ProcessIdentity(pid=10, create_time=1.0))


def test_baseline_files_parse() -> None:
    root = Path(__file__).resolve().parents[1] / "bench" / "idle_cpu"
    for sessions in (1, 8):
        baseline = json.loads((root / f"baseline_n{sessions}.json").read_text(encoding="utf-8"))
        assert baseline["sessions"] == sessions
        assert set(baseline["totals"]) == {
            "client_cpu_seconds",
            "daemon_cpu_seconds",
            "event_lines_appended",
        }


def test_pty_mode_has_its_own_baselines() -> None:
    from bench.idle_cpu.harness import _baseline_name

    assert _baseline_name("daemon", 1) == "baseline_n1.json"
    assert _baseline_name("pty", 6) == "baseline_pty_n6.json"
    root = Path(__file__).resolve().parents[1] / "bench" / "idle_cpu"
    for sessions in (1, 6):
        path = root / _baseline_name("pty", sessions)
        baseline = json.loads(path.read_text(encoding="utf-8"))
        assert baseline["mode"] == "pty"
        assert baseline["sessions"] == sessions


def test_isolated_env_never_points_at_the_real_home(tmp_path: Path) -> None:
    import os

    from bench.idle_cpu.harness import _isolated_env

    env = _isolated_env(tmp_path, tmp_path / "mock", tmp_path / "state")
    for key in ("HOME", "USERPROFILE", "CLUD_HOOK_HOME"):
        assert env[key] == str(tmp_path / "home")
        assert env[key] != os.environ.get(key)
    assert env["PATH"].startswith(str(tmp_path / "mock"))
