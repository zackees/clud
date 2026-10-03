"""clud's local gate (zackees/ci.yml GATE-001..011; see local-gate.toml).

Do not call this directly before pushing. Call it through the attesting
wrapper, which runs it on a clean, committed tree and stamps HEAD:

    uvx --from git+https://github.com/zackees/ci.yml@<CI_LINT_REF> ci-lint local-gate run

Lanes:

- `ci`: the PR's minimal plan (Static checks, Dylint, Clippy linux-x64,
  Build linux-x64 and the three Linux x64 unit shards, then CI OK), replayed
  from `.github/workflows/ci.yml` by `bosn ci` under act2 in its own engine.
  This is the only sanctioned local runner for clud's lint and tests
  (CLAUDE.md); tests never run on the host (GATE-005). The lane passes only
  when bosn's receipt shows it ran a clean snapshot of this worktree at
  HEAD (the GATE-009 tree proof) and every expected job succeeded.
- `static`: the remote `static` job's checks (`ci.lint --static-only`).
  Under CI (GitHub, or act inside the `ci` lane) they run in-process; on a
  host the lane replays the `static` job through `bosn ci` instead.

The replay uses a push event: on a pull_request event the `static` job
would verify this very commit's attestation, which the gate is still
producing. Push and an unlabeled PR select the same minimal job set.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

# The zackees/ci.yml commit whose ci_lint this repository uses; ci.yml's
# `static` job runs the same SHA (tests/test_local_gate.py keeps them equal).
CI_LINT_REF = "7edeb8dc318c1530ee3bf770029cb2e8b639e6f5"
LANES = ("ci", "static")
WORKFLOW = ".github/workflows/ci.yml"
# Job display-name prefixes of the minimal plan, with the leg count each
# must report as succeeded (bosn names a called workflow's job
# `<caller name>/<called job name>`, matrix legs `<caller name>-<n>/...`).
EXPECTED_JOBS: tuple[tuple[str, int], ...] = (
    ("CI/Static checks", 1),
    ("Dylint/", 1),
    ("Clippy linux-x64/", 1),
    ("Build linux-x64/", 1),
    ("Test linux-x64 (unit)-", 3),
    ("CI/CI OK", 1),
)
WAIT_DEADLINE_MS = 2 * 60 * 60 * 1000


@dataclass(frozen=True)
class Output:
    code: int
    lines: tuple[str, ...]


@dataclass(frozen=True)
class Receipt:
    """The fields of a `bosn ci` run record this gate relies on."""

    run_id: str
    workspace: str
    sha: str
    dirty: bool
    state: str
    conclusion: str
    jobs: tuple[tuple[str, str], ...]  # (key, conclusion)


def _in_ci() -> bool:
    return os.environ.get("CI", "").lower() in {"1", "true", "yes"}


def _capture(argv: list[str]) -> Output:
    """Run `argv` from the repository root, collecting its merged output
    line by line while it runs (running-process; never capture-then-wait)."""
    from running_process import RunningProcess

    proc = RunningProcess(argv, cwd=ROOT, check=False)
    lines = tuple(str(line) for line in proc.line_iter(timeout=None))
    return Output(int(proc.wait()), lines)


def _json_record(out: Output) -> dict[str, object] | None:
    for line in reversed(out.lines):
        text = line.strip()
        if text.startswith("{"):
            try:
                value = json.loads(text)
            except json.JSONDecodeError:
                continue
            if isinstance(value, dict):
                return value
    return None


def _receipt(record: dict[str, object]) -> Receipt:
    tree = record.get("tree")
    groups = tree.get("groups", []) if isinstance(tree, dict) else []
    jobs = tuple(
        (str(job.get("key", "")), str(job.get("conclusion", "")))
        for group in groups
        if isinstance(group, dict)
        for job in group.get("jobs", [])
        if isinstance(job, dict)
    )
    return Receipt(
        run_id=str(record.get("id", "")),
        workspace=str(record.get("workspace", "")),
        sha=str(record.get("sha", "")),
        dirty=record.get("dirty") is not None,
        state=str(record.get("state", "")),
        conclusion=str(record.get("conclusion", "")),
        jobs=jobs,
    )


def _git_line(*args: str) -> str:
    out = _capture(["git", *args])
    if out.code != 0:
        raise SystemExit(f"local gate: git {' '.join(args)} failed:\n" + "\n".join(out.lines))
    return out.lines[0].strip() if out.lines else ""


def _tree_problem(receipt: Receipt, head: str) -> str | None:
    """GATE-009: the run must be a clean snapshot of this worktree at HEAD."""
    if Path(receipt.workspace).resolve() != ROOT:
        return f"bosn ran workspace {receipt.workspace}, not {ROOT}"
    if receipt.sha != head:
        return f"bosn ran {receipt.sha[:12]}, but HEAD is {head[:12]}"
    if receipt.dirty:
        return "bosn's snapshot differs from HEAD (untracked or modified files); commit or remove them"
    return None


def _job_problems(receipt: Receipt) -> list[str]:
    problems: list[str] = []
    for prefix, legs in EXPECTED_JOBS:
        matched = [c for key, c in receipt.jobs if key.startswith(prefix)]
        ok = [c for c in matched if c == "success"]
        if len(ok) != legs:
            problems.append(f"{prefix!r}: {len(ok)}/{legs} succeeded (saw {matched or 'none'})")
    failed = [key for key, c in receipt.jobs if c not in {"success", "skipped"}]
    problems.extend(f"{key}: did not succeed" for key in failed)
    return problems


def _report(run_id: str) -> None:
    out = _capture(["bosn", "ci", "report", run_id, "--tail", "200"])
    print("\n".join(out.lines), flush=True)


def _bosn_ci(job: str | None) -> int:
    """Replay ci.yml (or one root job) under `bosn ci` and prove the run."""
    status = _capture(["git", "status", "--porcelain", "--untracked-files=normal"])
    if status.lines:
        print("local gate: the worktree is not clean; bosn would test these too:", file=sys.stderr)
        print("\n".join(status.lines), file=sys.stderr)
        return 1
    head = _git_line("rev-parse", "HEAD")
    argv = ["bosn", "ci", "run", "--workspace", str(ROOT), "--workflow", WORKFLOW]
    argv += ["--trigger", "push", "--mode", "minimal", "--json"]
    if job is not None:
        argv += ["--job", job]
    submitted = _json_record(_capture(argv))
    if submitted is None or not submitted.get("id"):
        print(f"local gate: `{' '.join(argv)}` returned no run record", file=sys.stderr)
        return 1
    run_id = str(submitted["id"])
    print(f"local gate: bosn ci run {run_id} (sha {head[:12]}, job {job or 'all'})", flush=True)
    problem = _tree_problem(_receipt(submitted), head)
    if problem is not None:
        _capture(["bosn", "ci", "cancel", run_id])
        print(f"local gate: {problem}", file=sys.stderr)
        return 1
    _capture(["bosn", "ci", "wait", run_id, "--deadline-ms", str(WAIT_DEADLINE_MS), "--json"])
    record = _json_record(_capture(["bosn", "ci", "show", run_id, "--json"]))
    if record is None:
        print(f"local gate: cannot read bosn ci run {run_id}", file=sys.stderr)
        return 1
    return _verdict(_receipt(record), head, full=job is None)


def _verdict(receipt: Receipt, head: str, *, full: bool) -> int:
    problems = [p for p in (_tree_problem(receipt, head),) if p]
    if receipt.state != "done" or receipt.conclusion != "success":
        problems.append(f"run {receipt.state}/{receipt.conclusion}")
    if full:
        problems += _job_problems(receipt)
    if problems:
        _report(receipt.run_id)
        print(f"local gate: bosn ci run {receipt.run_id} FAILED:", file=sys.stderr)
        print("\n".join(f"  - {p}" for p in problems), file=sys.stderr)
        return 1
    passed = sum(1 for _, c in receipt.jobs if c == "success")
    print(f"local gate: bosn ci run {receipt.run_id} passed ({passed} jobs, sha {head[:12]})")
    return 0


def _static_in_ci() -> int:
    from ci import lint

    return lint.main(["--static-only"])


def run_lane(lane: str) -> int:
    if lane == "static":
        return _static_in_ci() if _in_ci() else _bosn_ci("static")
    if _in_ci():
        print("local gate: the `ci` lane replays ci.yml with bosn; it runs on a host, not in CI")
        return 2
    return _bosn_ci(None)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--lane", choices=("all", *LANES), default="all")
    args = parser.parse_args(argv)
    lanes = ("ci",) if args.lane == "all" else (args.lane,)
    for lane in lanes:
        code = run_lane(lane)
        if code != 0:
            return code
    return 0


if __name__ == "__main__":
    sys.exit(main())
