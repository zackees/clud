"""Offline child harness for exercising the real watcher through the gh shim."""

from __future__ import annotations

import importlib.util
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WATCHER = ROOT / "crates/clud-bin/assets/tools/github/pr_merge_watch.py"
spec = importlib.util.spec_from_file_location("gh_watch_harness_watcher", WATCHER)
assert spec is not None and spec.loader is not None
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


def cancel(pr, _repo, sha, _opts, _log=None):
    cancel_log.write_text(f"{pr} {sha}\n", encoding="utf-8")
    return 1


watcher.cancel_pr_runs = cancel


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
    else:
        checks = []
        coderabbit = watcher.CodeRabbitObservation("quiet")
    return watcher.GateSnapshot(
        pr=snapshot,
        checks=checks,
        human_review_ids=frozenset(),
        coderabbit_probe=watcher.CodeRabbitProbe("not_detected", 0),
        coderabbit=coderabbit,
    )


watcher.fetch_gate_snapshot = gate
raise SystemExit(watcher.main(sys.argv[1:]))
