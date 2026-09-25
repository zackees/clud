"""pr_merge_watch: terminal states for empty rollups, conflicts, API failures
and orphaned watches (issue #1418)."""

from __future__ import annotations

import importlib.util
import json
import signal
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "github" / "pr_merge_watch.py"
LAND_SKILL = ROOT / "crates" / "clud-bin" / "assets" / "skills" / "grind-land" / "SKILL.md"


@pytest.fixture
def watcher():
    name = "clud_test_pr_merge_watch_1418"
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


class Clock:
    """A fake monotonic clock; each sleep advances it."""

    def __init__(self) -> None:
        self.now = 1000.0

    def monotonic(self) -> float:
        return self.now

    def sleep(self, seconds: float) -> None:
        self.now += seconds


def opts(watcher, on=frozenset({"fail", "timeout", "review", "closed"})):
    return watcher.CancelOptions(set(on), "runs", 30, False, False, True, False)


def snap(watcher, head="aaa111", mergeable="MERGEABLE", merge_state="CLEAN", state="OPEN"):
    return watcher.PRSnapshot(527, state, mergeable, head, "main", "feat", merge_state)


def gate(watcher, checks=(), *, head="aaa111", mergeable="MERGEABLE", merge_state="CLEAN",
         runs=None, check_runs=None):
    head_checks = None
    if runs is not None or check_runs is not None:
        head_checks = watcher.HeadChecks(list(check_runs or []), list(runs or []), [])
    return watcher.GateSnapshot(
        pr=snap(watcher, head, mergeable, merge_state),
        checks=list(checks),
        human_review_ids=frozenset(),
        coderabbit_probe=watcher.CodeRabbitProbe("not_detected", 0),
        coderabbit=watcher.CodeRabbitObservation("quiet"),
        head_checks=head_checks,
    )


def green_row(watcher):
    return watcher.CheckRow("linux", "pass", "SUCCESS")


@pytest.fixture
def env(watcher, tmp_path, monkeypatch):
    """Patch the watcher's network edges; tests override what they need."""
    clock = Clock()
    monkeypatch.setattr(watcher.time, "monotonic", clock.monotonic)
    monkeypatch.setattr(watcher.time, "sleep", clock.sleep)
    monkeypatch.setattr(watcher, "emit_progress_report", lambda *a, **k: None)
    monkeypatch.setattr(watcher, "fetch_required_check_names", lambda *a: None)
    monkeypatch.setattr(watcher, "_resolve_origin_repo", lambda: "zackees/clud")
    monkeypatch.setattr(watcher.PRSnapshot, "fetch", lambda *a: snap(watcher))
    monkeypatch.setattr(watcher, "fetch_workflow_presence", lambda *a: True)
    monkeypatch.setattr(watcher, "fetch_head_commit_message", lambda *a: "feat: x")
    cancels: list[tuple] = []
    monkeypatch.setattr(
        watcher, "cancel_pr_runs", lambda *a, **k: cancels.append((a, k)) or 1
    )
    log = watcher.WatchLog.create(527, "zackees/clud", root=tmp_path)
    state = {"clock": clock, "cancels": cancels, "log": log, "polls": 0}

    def set_gates(fn):
        def gates(*_a, **_k):
            state["polls"] += 1
            return fn(state["polls"])

        monkeypatch.setattr(watcher, "fetch_gate_snapshot", gates)

    state["set_gates"] = set_gates
    return state


def run(watcher, env, repo="zackees/clud", timeout=3600, **kwargs):
    with pytest.raises(SystemExit) as exc:
        watcher.watch(527, repo, 20, timeout, None, opts(watcher), env["log"], **kwargs)
    return exc.value.code


def events(env) -> list[dict]:
    return [json.loads(line) for line in env["log"].path.read_text().splitlines()]


def last(env, name: str) -> dict:
    return [e for e in events(env) if e.get("event") == name][-1]


# ---- empty rollup is terminal ----


def test_no_workflows_exits_no_checks_immediately(watcher, env, monkeypatch) -> None:
    monkeypatch.setattr(watcher, "fetch_workflow_presence", lambda *a: False)
    env["set_gates"](lambda n: gate(watcher))
    assert run(watcher, env) == watcher.EXIT_NO_CHECKS
    # One settle re-poll, well within the ~5 s budget, then out.
    assert env["polls"] == 2
    assert env["clock"].now - 1000.0 <= watcher.NO_CHECKS_SETTLE_SEC
    ev = last(env, "no_checks")
    assert ev["reason"] == "no_workflows"
    assert ev["merge_state_status"] == "CLEAN"
    assert last(env, "EXIT")["code"] == watcher.EXIT_NO_CHECKS
    assert env["cancels"] == []


@pytest.mark.parametrize(
    "marker", ["[skip ci]", "[ci skip]", "[no ci]", "[skip actions]", "[actions skip]"]
)
def test_skip_ci_marker_exits_no_checks_immediately(watcher, env, monkeypatch, marker) -> None:
    monkeypatch.setattr(
        watcher, "fetch_head_commit_message", lambda *a: f"feat: assets (#6) {marker}"
    )
    env["set_gates"](lambda n: gate(watcher))
    assert run(watcher, env) == watcher.EXIT_NO_CHECKS
    assert env["polls"] == 2
    assert last(env, "no_checks")["reason"] == "skip_ci_marker"


def test_a_check_app_registering_after_skip_ci_is_still_waited_for(
    watcher, env, monkeypatch
) -> None:
    monkeypatch.setattr(watcher, "fetch_head_commit_message", lambda *a: "x [skip ci]")
    env["set_gates"](lambda n: gate(watcher) if n == 1 else gate(watcher, [green_row(watcher)]))
    assert run(watcher, env) == watcher.EXIT_GREEN


def test_a_pr_adding_the_first_workflow_is_not_no_workflows(watcher, env, monkeypatch) -> None:
    monkeypatch.setattr(
        watcher, "fetch_workflow_presence", lambda repo, ref: ref != "main"
    )
    env["set_gates"](lambda n: gate(watcher))
    assert run(watcher, env, no_checks_grace=60) == watcher.EXIT_NO_CHECKS
    assert last(env, "no_checks")["reason"] == "empty_after_grace"


def test_a_conflicting_pr_with_no_checks_exits_conflict(watcher, env, monkeypatch) -> None:
    monkeypatch.setattr(watcher, "fetch_workflow_presence", lambda *a: False)
    env["set_gates"](lambda n: gate(watcher, mergeable="CONFLICTING", merge_state="DIRTY"))
    assert run(watcher, env) == watcher.EXIT_CONFLICT


def test_a_slow_external_required_check_is_not_cut_off_by_the_grace(
    watcher, env, monkeypatch
) -> None:
    # An external CI posts a pending status; no Actions run ever exists. The
    # required check has not reported yet, but checks clearly do report here.
    monkeypatch.setattr(watcher, "fetch_required_check_names", lambda *a: {"lint"})
    pending = [watcher.CheckRow("buildkite", "pending", "PENDING")]
    env["set_gates"](lambda n: gate(watcher, pending, runs=[], check_runs=[]))
    watcher_statuses = [{"context": "buildkite", "state": "pending", "id": 1}]
    real_gate = gate

    def with_status(n):
        g = real_gate(watcher, pending, runs=[], check_runs=[])
        g.head_checks.statuses.extend(watcher_statuses)
        return g

    env["set_gates"](with_status)
    assert run(watcher, env, timeout=300, no_checks_grace=60) == watcher.EXIT_TIMEOUT


def test_empty_rollup_exits_after_grace_not_before(watcher, env) -> None:
    env["set_gates"](lambda n: gate(watcher))
    assert run(watcher, env, no_checks_grace=60) == watcher.EXIT_NO_CHECKS
    elapsed = env["clock"].now - 1000.0
    assert 60 <= elapsed < 60 + 2 * 20
    assert last(env, "no_checks")["reason"] == "empty_after_grace"


def test_grace_default_is_overridable_from_the_environment(watcher, monkeypatch) -> None:
    assert watcher.DEFAULT_NO_CHECKS_GRACE_SEC == 60
    monkeypatch.setenv("CLUD_PR_MERGE_WATCH_NO_CHECKS_GRACE", "15")
    assert watcher.parse_args(["5"]).no_checks_grace == 15
    assert watcher.parse_args(["5", "--no-checks-grace", "90"]).no_checks_grace == 90


def test_checks_that_register_late_still_wait_then_go_green(watcher, env) -> None:
    # Empty for the first 30 s, then the check registers and passes.
    env["set_gates"](
        lambda n: gate(watcher) if env["clock"].now - 1000.0 < 30
        else gate(watcher, [green_row(watcher)])
    )
    assert run(watcher, env, no_checks_grace=60) == watcher.EXIT_GREEN


def test_required_check_with_zero_runs_after_grace_is_never_reported(
    watcher, env, monkeypatch
) -> None:
    monkeypatch.setattr(watcher, "fetch_required_check_names", lambda *a: {"lint"})
    env["set_gates"](lambda n: gate(watcher, runs=[], check_runs=[]))
    assert run(watcher, env, no_checks_grace=60) == watcher.EXIT_NEVER_REPORTED
    assert env["clock"].now - 1000.0 >= 60
    assert last(env, "never_reported")["checks"] == ["lint"]


def test_judge_reports_missing_required_when_run_grace_elapsed(watcher) -> None:
    verdict = watcher.judge_check_runs([], [], "aaa111", {"lint"}, runs_grace_elapsed=True)
    assert verdict.state == "never_reported"
    assert watcher.judge_check_runs([], [], "aaa111", {"lint"}).state == "pending"


# ---- mergeability ----


def test_green_but_conflicting_exits_conflict_within_one_poll(watcher, env) -> None:
    env["set_gates"](
        lambda n: gate(watcher, [green_row(watcher)], mergeable="CONFLICTING", merge_state="DIRTY")
    )
    assert run(watcher, env) == watcher.EXIT_CONFLICT
    assert env["polls"] == 1
    checks = last(env, "checks")
    assert checks["mergeable"] == "CONFLICTING"
    assert checks["merge_state_status"] == "DIRTY"


def test_mergeable_unknown_is_transient_only_for_a_bounded_number_of_polls(
    watcher, env
) -> None:
    env["set_gates"](
        lambda n: gate(watcher, [green_row(watcher)], mergeable="UNKNOWN", merge_state="UNKNOWN")
    )
    assert run(watcher, env) == watcher.EXIT_CONFLICT
    assert env["polls"] == watcher.MERGEABLE_UNKNOWN_MAX_POLLS
    assert last(env, "EXIT")["reason"] == "mergeable_unknown"


# ---- API failures ----


def test_initial_fetch_failure_is_unreachable_not_closed(watcher, env, monkeypatch) -> None:
    def fail(*_a):
        watcher.gh_error_note("HTTP 401: Bad credentials")
        return None

    monkeypatch.setattr(watcher.PRSnapshot, "fetch", fail)
    assert run(watcher, env) == watcher.EXIT_GITHUB_UNREACHABLE
    ev = last(env, "EXIT")
    assert ev["reason"] == "github_unreachable"
    assert "Bad credentials" in ev["stderr"]


def test_no_resolvable_repo_fails_fast(watcher, env, monkeypatch) -> None:
    monkeypatch.setattr(watcher, "_resolve_origin_repo", lambda: None)
    env["set_gates"](lambda n: gate(watcher))
    assert run(watcher, env, repo=None) == watcher.EXIT_GITHUB_UNREACHABLE
    assert env["polls"] == 0


def test_persistent_gate_failure_exits_within_three_polls(watcher, env) -> None:
    def gates(n):
        watcher.gh_error_note("API rate limit exceeded")
        return None

    env["set_gates"](gates)
    assert run(watcher, env) == watcher.EXIT_GITHUB_UNREACHABLE
    assert env["polls"] == watcher.MAX_CONSECUTIVE_API_FAILURES == 3
    assert "rate limit" in last(env, "EXIT")["stderr"]


def test_one_transient_gate_failure_recovers(watcher, env) -> None:
    env["set_gates"](lambda n: None if n == 1 else gate(watcher, [green_row(watcher)]))
    assert run(watcher, env) == watcher.EXIT_GREEN


def test_every_gh_call_is_bounded_by_default(watcher, monkeypatch) -> None:
    seen: dict = {}

    class Done:
        returncode = 0
        stdout = "{}"
        stderr = ""

    def fake_run(argv, **kwargs):
        seen.update(kwargs)
        return Done()

    monkeypatch.setattr(watcher.RunningProcess, "run", fake_run)
    watcher.gh("pr", "view", "1")
    assert seen["timeout"] == watcher.GH_CALL_TIMEOUT_SEC
    assert 0 < watcher.GH_CALL_TIMEOUT_SEC <= 60


def test_a_hung_gh_call_cannot_outlive_the_timeout(watcher, env) -> None:
    # Each gate call burns 400 s (a stalled connection bounded by gh's own
    # timeout), then fails; the watch must still stop at --timeout.
    def hung(n):
        env["clock"].now += 400
        return gate(watcher, [watcher.CheckRow("linux", "pending", "IN_PROGRESS")])

    env["set_gates"](hung)
    assert run(watcher, env, timeout=540) == watcher.EXIT_TIMEOUT
    assert env["polls"] <= 2


# ---- orphaned watches ----


def test_watch_never_cancels_runs_on_a_sha_it_did_not_start_on(watcher, env) -> None:
    pending = [watcher.CheckRow("linux", "pending", "IN_PROGRESS")]
    env["set_gates"](lambda n: gate(watcher, pending, head="aaa111" if n == 1 else "bbb222"))
    assert run(watcher, env, timeout=100) == watcher.EXIT_TIMEOUT
    assert env["cancels"] == []
    moved = last(env, "head_moved")
    assert moved["old"] == "aaa111"
    assert moved["new"] == "bbb222"
    assert last(env, "cancel_skipped")["reason"] == "head_moved"


def test_timeout_on_the_started_sha_still_cancels(watcher, env) -> None:
    pending = [watcher.CheckRow("linux", "pending", "IN_PROGRESS")]
    env["set_gates"](lambda n: gate(watcher, pending))
    assert run(watcher, env, timeout=100) == watcher.EXIT_TIMEOUT
    assert len(env["cancels"]) == 1


@pytest.mark.parametrize(("sig", "code"), [(signal.SIGTERM, 143), (signal.SIGINT, 130)])
def test_kill_writes_a_final_exit_event_and_does_not_cancel(
    watcher, env, monkeypatch, tmp_path, sig, code
) -> None:
    handlers: dict = {}
    monkeypatch.setattr(watcher.signal, "signal", lambda s, h: handlers.__setitem__(s, h))
    monkeypatch.setattr(watcher, "_watch_root", lambda: tmp_path / "kill")
    created: list = []
    real_create = watcher.WatchLog.create
    monkeypatch.setattr(
        watcher.WatchLog, "create",
        lambda *a, **k: created.append(real_create(*a, **k)) or created[-1],
    )

    def killed_mid_poll(*_a, **_k):
        handlers[sig](sig, None)

    monkeypatch.setattr(watcher, "watch", killed_mid_poll)
    with pytest.raises(SystemExit) as exc:
        watcher.main(["527", "--repo", "zackees/clud"])
    assert exc.value.code == code
    lines = [json.loads(x) for x in created[0].path.read_text().splitlines()]
    assert lines[-1]["event"] == "EXIT"
    assert lines[-1]["reason"] == "killed"
    assert lines[-1]["code"] == code
    assert env["cancels"] == []


def test_timeout_default_reads_the_callers_cap_from_the_environment(
    watcher, monkeypatch
) -> None:
    assert watcher.parse_args(["5"]).timeout == 3600
    monkeypatch.setenv("CLUD_PR_MERGE_WATCH_TIMEOUT", "540")
    assert watcher.parse_args(["5"]).timeout == 540


# ---- queued too long ----


def test_max_queued_exits_naming_jobs_and_labels(watcher, env, monkeypatch) -> None:
    runs = [{
        "id": 77, "status": "queued", "head_sha": "aaa111", "event": "pull_request",
        "path": ".github/workflows/ci.yml", "name": "ci",
        "created_at": "2000-01-01T00:00:00Z",
    }]
    check_runs = [{
        "id": 1, "name": "mac", "status": "queued", "conclusion": None, "head_sha": "aaa111",
        "details_url": "https://github.com/o/r/actions/runs/77/job/9",
    }]
    monkeypatch.setattr(
        watcher, "fetch_queued_jobs",
        lambda repo, run_id: [{"name": "mac", "labels": ["macos-15-xlarge"]}],
    )
    env["set_gates"](lambda n: gate(watcher, runs=runs, check_runs=check_runs))
    assert run(watcher, env, max_queued=600) == watcher.EXIT_QUEUED
    ev = last(env, "queued_too_long")
    assert ev["jobs"] == [{"name": "mac", "labels": ["macos-15-xlarge"], "run_id": 77}]


def test_queued_without_max_queued_keeps_waiting(watcher, env) -> None:
    runs = [{
        "id": 77, "status": "queued", "head_sha": "aaa111", "event": "pull_request",
        "path": ".github/workflows/ci.yml", "name": "ci", "created_at": "2000-01-01T00:00:00Z",
    }]
    check_runs = [{
        "id": 1, "name": "mac", "status": "queued", "conclusion": None, "head_sha": "aaa111",
        "details_url": "https://github.com/o/r/actions/runs/77/job/9",
    }]
    env["set_gates"](lambda n: gate(watcher, runs=runs, check_runs=check_runs))
    assert run(watcher, env, timeout=100) == watcher.EXIT_TIMEOUT


# ---- the lander contract ----


def test_grind_land_documents_the_new_exit_codes_and_a_timeout_below_its_cap() -> None:
    body = LAND_SKILL.read_text()
    assert "--timeout 540" in body
    for code in ("`8`", "`9`", "`10`", "`11`", "NO_CHECKS", "mergeStateStatus"):
        assert code in body, code
