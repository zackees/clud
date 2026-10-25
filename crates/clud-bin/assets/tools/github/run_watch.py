#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "running-process==4.10.1",
# ]
# ///
# managed-by: clud
"""run_watch.py — wait on one GitHub Actions run that has no PR (#1941).

Usage:
  run_watch.py <run-id> [--repo OWNER/REPO] [--timeout SECS]
               [--interval SECS] [--cancel]

The sanctioned waiter for a `workflow_dispatch`, `schedule` or `push` run,
which `pr_merge_watch.py` cannot reach because it takes a PR. Start it once
in the background and wait for its exit; it prints one line per job when the
run settles.

Fails fast: the first job that concludes failure, cancelled or timed_out ends
the watch, naming each failed job, its failed step(s) and the tail of its
failed-step log. `--cancel` then also cancels the rest of the run.
`--interval` is clamped to at least 30 seconds to spare the API rate limit.

Run through clud's `tool run` subcommand:
  "$CLUD_EXE" tool run github/run_watch.py -- 38310141872 --repo OWNER/REPO

Exit codes:
  0    the run concluded success (or skipped/neutral)
  1    a job failed, or the run concluded failure/cancelled/timed_out
  3    gh could not read the run
  64   usage error
  124  timed out while the run was still in progress (resumable: re-run with
       the same arguments)
"""

from __future__ import annotations

import argparse
import json
import sys
import time

from running_process import PIPE, RunningProcess, TimeoutExpired

GH_TIMEOUT = 60.0
FAILED = {"failure", "cancelled", "timed_out", "startup_failure", "action_required"}
EXIT_OK = 0
EXIT_FAILED = 1
EXIT_GH = 3
EXIT_USAGE = 64
EXIT_TIMEOUT = 124
MIN_INTERVAL = 30.0
LOG_TAIL_LINES = 40


def gh(*args: str) -> tuple[int, str, str]:
    """Run gh; return (exit code, stdout, stderr)."""
    try:
        # stderr=PIPE: running-process merges stderr into stdout otherwise (#1175).
        res = RunningProcess.run(
            ["gh", *args], capture_output=True, stderr=PIPE, text=True, timeout=GH_TIMEOUT
        )
    except (TimeoutError, TimeoutExpired):
        return 124, "", f"gh {' '.join(args)} timed out after {GH_TIMEOUT}s"
    return res.returncode, res.stdout or "", res.stderr or ""


def evaluate(run: dict) -> tuple[str, list[dict]]:
    """Pure: classify one `gh run view --json status,conclusion,jobs` sample.

    Returns `("failed", failed_jobs)` as soon as any job failed, even while
    others still run; `("success", [])` when the run completed cleanly;
    `("failed", [])` when the run itself concluded badly with no failed job
    to name; otherwise `("pending", [])`.
    """
    jobs = run.get("jobs") or []
    failed = [job for job in jobs if (job.get("conclusion") or "") in FAILED]
    if failed:
        return "failed", failed
    if run.get("status") != "completed":
        return "pending", []
    if (run.get("conclusion") or "") in FAILED:
        return "failed", []
    return "success", []


def render_jobs(jobs: list[dict]) -> str:
    lines = []
    for job in jobs:
        verdict = job.get("conclusion") or job.get("status") or "?"
        lines.append(f"  {verdict:<12} {job.get('name', '?')}")
    return "\n".join(lines)


def failed_steps(job: dict) -> list[str]:
    """Pure: names of the steps in `job` that concluded badly."""
    return [
        step.get("name", "?")
        for step in job.get("steps") or []
        if (step.get("conclusion") or "") in FAILED
    ]


def log_tail(log: str, lines: int = LOG_TAIL_LINES) -> str:
    """Pure: the last `lines` non-empty lines of a `--log-failed` dump."""
    kept = [line for line in log.splitlines() if line.strip()]
    return "\n".join(kept[-lines:])


def report_failed_job(job: dict, repo: str | None) -> None:
    """Print a failed job's failed steps and the tail of its failed-step log."""
    steps = failed_steps(job)
    print(f"FAILED job: {job.get('name', '?')}")
    print(f"  failed step(s): {', '.join(steps) if steps else '(none reported)'}")
    job_id = job.get("databaseId")
    if not job_id:
        return
    args = ["run", "view", "--job", str(job_id), "--log-failed"]
    if repo:
        args += ["--repo", repo]
    code, out, err = gh(*args)
    tail = log_tail(out) if code == 0 else ""
    if tail:
        print(f"  --- last {LOG_TAIL_LINES} lines of the failed-step log ---")
        print(tail)
    else:
        print(f"  (failed-step log unavailable: {err.strip() or f'gh exit {code}'})")


class _UsageParser(argparse.ArgumentParser):
    def error(self, message: str) -> None:  # type: ignore[override]
        self.print_usage(sys.stderr)
        sys.stderr.write(f"run_watch.py: error: {message}\n")
        sys.exit(EXIT_USAGE)


def parse_args(argv: list[str]) -> argparse.Namespace:
    p = _UsageParser(prog="run_watch.py", description=__doc__.splitlines()[0])
    p.add_argument("run_id", type=int)
    p.add_argument("--repo", default=None)
    p.add_argument("--timeout", type=float, default=3300.0)
    p.add_argument("--interval", type=float, default=30.0)
    p.add_argument("--cancel", action="store_true", help="cancel the run on the first failed job")
    ns = p.parse_args(argv)
    ns.interval = max(MIN_INTERVAL, ns.interval)
    return ns


def fetch(run_id: int, repo: str | None) -> dict | None:
    args = ["run", "view", str(run_id), "--json", "status,conclusion,jobs,url,event"]
    if repo:
        args += ["--repo", repo]
    code, out, err = gh(*args)
    if code != 0:
        sys.stderr.write(f"run_watch: gh run view failed (exit {code}): {err.strip()}\n")
        return None
    try:
        data = json.loads(out)
    except json.JSONDecodeError:
        return None
    return data if isinstance(data, dict) else None


def main(argv: list[str] | None = None) -> int:
    ns = parse_args(sys.argv[1:] if argv is None else argv)
    deadline = time.monotonic() + ns.timeout
    misses = 0
    while True:
        run = fetch(ns.run_id, ns.repo)
        if run is None:
            misses += 1
            if misses >= 3:
                return EXIT_GH
        else:
            misses = 0
            state, failed = evaluate(run)
            if state != "pending":
                print(f"run {ns.run_id} ({run.get('event', '?')}) {run.get('url', '')}")
                print(render_jobs(run.get("jobs") or []))
                if state == "success":
                    print(f"GREEN  run {ns.run_id} succeeded")
                    return EXIT_OK
                names = ", ".join(job.get("name", "?") for job in failed) or "the run"
                print(f"RED    run {ns.run_id}: {names} failed")
                for job in failed:
                    report_failed_job(job, ns.repo)
                if ns.cancel and run.get("status") != "completed":
                    cancel = ["run", "cancel", str(ns.run_id)]
                    if ns.repo:
                        cancel += ["--repo", ns.repo]
                    gh(*cancel)
                    print(f"cancelled run {ns.run_id}")
                return EXIT_FAILED
        if time.monotonic() >= deadline:
            print(f"TIMEOUT run {ns.run_id} still in progress; re-run with the same arguments")
            return EXIT_TIMEOUT
        time.sleep(min(ns.interval, max(1.0, deadline - time.monotonic())))


if __name__ == "__main__":
    sys.exit(main())
