#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["running-process==4.10.1"]
# ///
# managed-by: clud
"""Read a failed GitHub Actions job with bounded output and a saved raw log.

  clud tool run github/ci_failure_report.py -- --repo OWNER/REPO --run ID --job ID
  clud tool run github/ci_failure_report.py -- --artifact PATH --start-line 1 --line-count 40

Exit 0 means the job log was retrieved, not that the job succeeded. Exit 1
means the observation is incomplete or the artifact could not be saved.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import sys
import tempfile
from pathlib import Path

from running_process import PIPE, RunningProcess, TimeoutExpired

MAX_OUTPUT_BYTES = 8192
MAX_OUTPUT_LINES = 80
MAX_SUMMARY_BYTES = 4096
MAX_LINE_CHARS = 500
PREFIX = re.compile(r"^(?:[^\t]*\t[^\t]*\t)?\d{4}-\d\d-\d\dT[\d:.]+Z\s?")
ANSI = re.compile(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\))")
PANIC = re.compile(r"^thread '([^']+)'(?: \(\d+\))? panicked at")
PASS = re.compile(r"^test (\S+) \.\.\. ok\b")
ERROR = re.compile(r"^(?:error(?:\[E\d+\])?:|FAILED \S|E\s+\S|##\[error\]|Error:|Diff in )")
GENERIC = re.compile(r"^(?:error: could not compile|##\[error\]Process completed with exit code)")


def clean(line: str) -> str:
    line = PREFIX.sub("", ANSI.sub("", line))
    return "".join(c if c.isprintable() or c == "\t" else "?" for c in line)


def bounded(lines: list[str]) -> str:
    """Cap by bytes and lines, always signalling omitted material."""
    output: list[str] = []
    size = 0
    omitted = False
    for line in lines:
        if len(line) > MAX_LINE_CHARS:
            line = line[:MAX_LINE_CHARS] + " [line truncated]"
            omitted = True
        encoded = line.encode("utf-8")
        if len(output) >= MAX_OUTPUT_LINES - 1 or size + len(encoded) + 1 > MAX_SUMMARY_BYTES - 80:
            omitted = True
            break
        output.append(line)
        size += len(encoded) + 1
    if omitted:
        output.append("[truncated; inspect the saved artifact for more]")
    return "\n".join(output)


def summarize(raw: str) -> str:
    lines = [clean(line) for line in raw.splitlines()]
    passed = {m.group(1) for line in lines if (m := PASS.match(line.strip()))}
    selected: set[int] = set()
    candidates = []
    for index, line in enumerate(lines):
        stripped = line.strip()
        panic = PANIC.match(stripped)
        if panic and panic.group(1) in passed:
            continue
        if (panic or ERROR.match(stripped)) and not GENERIC.match(stripped):
            candidates.append(index)
    if not candidates:
        candidates = [i for i, line in enumerate(lines) if GENERIC.match(line.strip())]
    for index in candidates:
        selected.add(index)
        for offset in range(index + 1, min(index + 7, len(lines))):
            following = lines[offset].strip()
            if PANIC.match(following) or PASS.match(following) or GENERIC.match(following):
                break
            if ERROR.match(following):
                break
            selected.add(offset)
    if not selected:
        return "No actionable diagnostic recognized; inspect the saved raw log."
    return bounded([lines[i] for i in sorted(selected)])


def github_get(path: str) -> bytes:
    try:
        result = RunningProcess.run(
            ["gh", "api", path, "--allow-escape-sequences"],
            capture_output=True,
            stderr=PIPE,
            text=False,
            timeout=60,
        )
    except (OSError, TimeoutError, TimeoutExpired) as exc:
        raise RuntimeError(f"GitHub request failed: {exc}") from exc
    if b"clud: gh read broker: rate-limit reserve" in (result.stderr or b""):
        raise RuntimeError("GitHub broker returned stale data; retry later")
    if result.returncode:
        message = (result.stderr or b"").decode("utf-8", "replace").strip()
        raise RuntimeError(f"GitHub request failed: {message[:500]}")
    return result.stdout or b""


def save_artifact(raw: bytes) -> Path:
    root = Path(os.environ.get("CLUD_TOOL_OUTPUT_DIR") or Path.home() / ".clud/tmp/tool-output")
    if len(os.fsencode(root)) > 800:
        raise OSError("artifact directory path is too long to report safely")
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix="ci-failure-", suffix=".log", dir=root)
    with os.fdopen(fd, "wb") as stream:
        stream.write(raw)
    return Path(name)


def print_range(path: Path, start: int, count: int) -> None:
    if start < 1 or count < 1 or count > 60:
        raise ValueError("start-line must be positive and line-count must be 1..60")
    lines = []
    with path.open("r", encoding="utf-8", errors="replace") as stream:
        for number, line in enumerate(stream, 1):
            if number >= start + count:
                break
            if number >= start:
                lines.append(f"{number}: {clean(line.rstrip())}")
    if not lines:
        raise ValueError("requested range is beyond the artifact")
    print(bounded(lines))
    print(f"Continue at line {start + count}: --artifact {shlex.quote(str(path))} "
          f"--start-line {start + count}")


def read_job(repo: str, run: int, job: int) -> bytes:
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo):
        raise ValueError("repository must be OWNER/REPO")
    if run < 1 or job < 1:
        raise ValueError("run and job IDs must be positive")
    path = f"repos/{repo}/actions/jobs/{job}"
    metadata = json.loads(github_get(path))
    if not isinstance(metadata, dict) or metadata.get("run_id") != run:
        raise ValueError("job does not belong to the requested run")
    if metadata.get("status") != "completed":
        raise ValueError(f"job status is {metadata.get('status')!s}; log may be incomplete")
    if metadata.get("conclusion") not in ("failure", "cancelled", "timed_out", "startup_failure"):
        raise ValueError("job is not a completed failed job")
    raw = github_get(path + "/logs")
    if not raw.strip():
        raise ValueError("job log is empty or still uploading; retry later")
    return raw


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo")
    parser.add_argument("--run", type=int)
    parser.add_argument("--job", type=int)
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--start-line", type=int, default=1)
    parser.add_argument("--line-count", type=int, default=40)
    args = parser.parse_args(argv)
    try:
        if args.artifact:
            if args.repo or args.run or args.job:
                raise ValueError("artifact continuation cannot be combined with GitHub selectors")
            print_range(args.artifact, args.start_line, args.line_count)
            return 0
        if not args.repo or args.run is None or args.job is None:
            raise ValueError("provide --repo, --run, and --job")
        raw = read_job(args.repo, args.run, args.job)
        try:
            artifact = save_artifact(raw)
        except OSError as exc:
            raise ValueError(f"could not save full log artifact: {exc}") from exc
        print(summarize(raw.decode("utf-8", "replace")))
        print(f"Full raw log: {artifact}")
        print("Continue: clud tool run github/ci_failure_report.py -- "
              f"--artifact {shlex.quote(str(artifact))} --start-line 1 --line-count 40")
        return 0
    except (RuntimeError, ValueError, OSError, json.JSONDecodeError) as exc:
        print(f"ci_failure_report: {str(exc)[:700]}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
