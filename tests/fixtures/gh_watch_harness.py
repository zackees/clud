"""Offline child harness for exercising the real watcher through the gh shim."""

from __future__ import annotations

import importlib.util
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WATCHER = ROOT / "crates/clud-bin/assets/tools/github/pr_merge_watch.py"
spec = importlib.util.spec_from_file_location("gh_watch_harness_watcher", WATCHER)
assert spec is not None
assert spec.loader is not None
watcher = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = watcher
spec.loader.exec_module(watcher)

scenario = os.environ["GH_WATCH_SCENARIO"]
cancel_log = Path(os.environ["GH_WATCH_CANCEL_LOG"])
snapshot = watcher.PRSnapshot(123, "OPEN", "MERGEABLE", "abc123", "main")
watcher.PRSnapshot.fetch = lambda *_args: snapshot
watcher.fetch_required_check_names = lambda *_args: None if scenario == "no_checks" else {"linux"}
watcher.emit_progress_report = lambda *_args, **_kwargs: None
watcher.fetch_workflow_presence = lambda *_args: False
watcher.fetch_head_commit_message = lambda *_args: "feat: shim"
watcher._build_failure_report = lambda check, *_args: watcher.FailureReport(
    check, None, "boom", "test failure"
)


def cancel(pr, _repo, sha, _opts, _log=None, **_scope):
    cancel_log.write_text(f"{pr} {sha}\n", encoding="utf-8")
    return 1


watcher.cancel_pr_runs = cancel
watcher._rerun_since_verdict = lambda *_args: None


def _check(check_id, name, status, conclusion):
    return {
        "id": check_id,
        "name": name,
        "head_sha": "abc123",
        "status": status,
        "conclusion": conclusion,
        "details_url": f"https://github.com/zackees/clud/actions/runs/900/job/{check_id}",
    }


# A failure verdict needs REST check runs (#1742): the rollup alone never decides one.
FAILING_HEAD = watcher.HeadChecks(
    [_check(1, "linux", "completed", "failure"), _check(2, "macos", "in_progress", None)],
    [
        {
            "id": 900,
            "run_number": 1,
            "path": ".github/workflows/ci.yml",
            "head_sha": "abc123",
            "event": "pull_request",
            "status": "in_progress",
            "conclusion": None,
            "created_at": "2026-10-02T00:00:00Z",
        }
    ],
)


def gate(*_args, **_kwargs):
    if scenario == "fail":
        checks = [
            watcher.CheckRow("linux", "fail", "FAILURE"),
            watcher.CheckRow("macos", "pending", "IN_PROGRESS"),
        ]
        coderabbit = watcher.CodeRabbitObservation("quiet")
    elif scenario == "review":
        checks = [watcher.CheckRow("linux", "pass", "SUCCESS")]
        coderabbit = watcher.CodeRabbitObservation(
            "actionable", actionable=True, unresolved_threads=1, ids=frozenset({91})
        )
    elif scenario in {"green", "scope"}:
        checks = [watcher.CheckRow("linux", "pass", "SUCCESS")]
        if scenario == "scope":
            checks.append(watcher.CheckRow("macos", "pending", "IN_PROGRESS"))
        coderabbit = watcher.CodeRabbitObservation("quiet")
    elif scenario == "timeout":
        checks = [watcher.CheckRow("linux", "pending", "IN_PROGRESS")]
        coderabbit = watcher.CodeRabbitObservation("quiet")
    else:
        checks = []
        coderabbit = watcher.CodeRabbitObservation("quiet")
    return watcher.GateSnapshot(
        pr=snapshot,
        checks=checks,
        human_review_ids=frozenset(),
        coderabbit_probe=watcher.CodeRabbitProbe("not_detected", 0),
        coderabbit=coderabbit,
        head_checks=FAILING_HEAD if scenario == "fail" else None,
    )


watcher.fetch_gate_snapshot = gate

# No remote edge is permitted in this fixture, even if production adds a new read.
def unexpected_remote(*_args, **_kwargs):
    raise AssertionError("offline watcher fixture attempted an unexpected GitHub read")


watcher.gh_json = unexpected_remote
watcher.api_get = unexpected_remote

if scenario in {"scope", "timeout"}:
    clock = [0.0]
    watcher.time.monotonic = lambda: clock[0]

    def advance_clock(*_args):
        clock[0] += 600.0

    watcher._sleep_remaining_interval = advance_clock

# #2004: deterministic live-session handshake; independent of watcher's outcomes.
if os.environ.get("GH_WATCH_CONTROLLED_SESSION") == "1":
    print("CONTROLLED_SESSION_READY", flush=True)
    if sys.stdin.readline().strip() != "continue":
        raise SystemExit("missing test controller release")

def actual_watchdog_result():
    """Exercise clud's real resumable-tool wrapper with a local blocked uv fixture."""
    sys.path.insert(0, str(ROOT))
    from tests import process

    fake_bin = Path.cwd() / "watchdog-bin"
    fake_bin.mkdir()
    uv = fake_bin / "uv"
    uv.write_text("#!/bin/sh\nsleep 2\n", encoding="utf-8")
    uv.chmod(0o755)
    env = dict(os.environ, PATH=f"{fake_bin}{os.pathsep}{os.environ['PATH']}",
               CLUD_TEST_TOOL_COMMAND_TIMEOUT_MS="100")
    result = process.run(
        [os.environ["GH_WATCH_CLUD_BINARY"], "tool", "run", "github/pr_merge_watch.py",
         "--", "123", "--repo", "zackees/clud"],
        env=env, capture_output=True, text=True, timeout=10,
    )
    sys.stdout.write(result.stdout)
    sys.stderr.write(result.stderr)
    return result.returncode


raise SystemExit(actual_watchdog_result() if scenario == "watchdog"
                 else watcher.main(sys.argv[1:]))
