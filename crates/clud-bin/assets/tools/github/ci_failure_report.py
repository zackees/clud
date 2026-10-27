#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["running-process==4.10.1"]
# ///
# managed-by: clud
"""Read a failed GitHub Actions job with bounded output and a saved raw log.

  "$CLUD_EXE" tool run github/ci_failure_report.py -- --repo OWNER/REPO --run ID --job ID
  "$CLUD_EXE" tool run github/ci_failure_report.py -- --artifact PATH --start-line 1 --line-count 40

Exit 0 means the job log was retrieved, not that the job succeeded. Exit 1
means the observation is incomplete or the artifact could not be saved.
"""

from __future__ import annotations

import argparse
import io
import json
import os
import re
import shlex
import stat
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
NEXTEST_PASS = re.compile(r"^PASS\s+\[[^]]+\]\s+\S+\s+(\S+)")
NEXTEST_FAIL = re.compile(r"^FAIL\s+\[[^]]+\]\s+\S+\s+(\S+)")
ERROR = re.compile(
    r"^(?:error(?:\[E\d+\])?:|FAILED \S|FAIL\s+\[|E\s+\S|##\[error\]|Error:|Diff in )"
)
GENERIC = re.compile(r"^(?:error: could not compile|##\[error\]Process completed with exit code)")


def clean(line: str) -> str:
    line = PREFIX.sub("", ANSI.sub("", line))
    return "".join(c if c.isprintable() or c == "\t" else "?" for c in line)


def bounded_parts(lines: list[str]) -> tuple[str, int]:
    """Return bounded text and the count of source lines actually shown."""
    output: list[str] = []
    size = 0
    omitted = False
    for line in lines:
        if len(line) > MAX_LINE_CHARS:
            line = line[:MAX_LINE_CHARS] + " [line truncated]"
            omitted = True
        encoded = line.encode("utf-8")
        if len(output) >= MAX_OUTPUT_LINES - 3 or size + len(encoded) + 1 > MAX_SUMMARY_BYTES - 80:
            omitted = True
            break
        output.append(line)
        size += len(encoded) + 1
    if omitted:
        output.append("[truncated; inspect the saved artifact for more]")
    return "\n".join(output), min(len(output) - int(omitted), len(lines))


def bounded(lines: list[str]) -> str:
    return bounded_parts(lines)[0]


def log_lines(stream):
    """Yield capped lines with byte offsets while consuming even huge lines."""
    stream.seek(0)
    while True:
        position = stream.tell()
        raw = stream.readline(4096)
        if not raw:
            return
        truncated = len(raw) == 4096 and not raw.endswith(b"\n")
        if truncated:
            while True:
                tail = stream.readline(4096)
                if not tail or tail.endswith(b"\n"):
                    break
        line = raw.decode("utf-8", "replace").rstrip("\r\n")
        yield position, clean(line) + (" [line truncated]" if truncated else "")


def panic_names(stream) -> tuple[set[str], bool]:
    names: set[str] = set()
    omitted = False
    for _, line in log_lines(stream):
        match = PANIC.match(line.strip())
        if match:
            name = match.group(1)
            if len(names) < 256 and len(name) <= 256:
                names.add(name)
            else:
                omitted = True
    return names, omitted


def passed_names(stream, candidates: set[str]) -> tuple[set[str], bool]:
    passed: set[str] = set()
    failed: set[str] = set()
    for _, line in log_lines(stream):
        text = line.strip()
        match = PASS.match(text) or NEXTEST_PASS.match(text)
        if match and match.group(1) in candidates:
            passed.add(match.group(1))
        failure = NEXTEST_FAIL.match(text)
        if failure and failure.group(1) in candidates:
            failed.add(failure.group(1))
    return passed, bool(passed & failed)


def diagnostic_lines(stream, candidates: set[str], passed: set[str]) -> tuple[list[str], bool]:
    selected: list[str] = []
    generic: list[str] = []
    remaining = 0
    omitted = False
    for _, line in log_lines(stream):
        text = line.strip()
        panic = PANIC.match(text)
        candidate = bool(panic and panic.group(1) in candidates and panic.group(1) not in passed)
        candidate = candidate or bool(ERROR.match(text) and not GENERIC.match(text))
        if GENERIC.match(text):
            if not generic:
                generic.append(line)
            remaining = 0
            continue
        if panic and not candidate:
            remaining = 0
            continue
        if PASS.match(text) or NEXTEST_PASS.match(text):
            remaining = 0
            continue
        if candidate:
            remaining = 6
        elif remaining:
            remaining -= 1
        else:
            continue
        if len(selected) < MAX_OUTPUT_LINES * 2:
            selected.append(line)
        else:
            omitted = True
    return selected or generic, omitted


def summarize_stream(stream) -> str:
    candidates, panics_omitted = panic_names(stream)
    passed, ambiguous = passed_names(stream, candidates)
    lines, omitted = diagnostic_lines(stream, candidates, passed)
    if ambiguous:
        lines.append("[panic attribution is ambiguous across test binaries; inspect the artifact]")
    if not lines:
        lines = ["No actionable diagnostic recognized; inspect the saved raw log."]
    if panics_omitted or omitted:
        lines.append("[truncated: additional possible diagnostics are in the saved artifact]")
    return bounded(lines)


def summarize(raw: str) -> str:
    return summarize_stream(io.BytesIO(raw.encode("utf-8", "replace")))


def github_get(path: str, hostname: str | None = None) -> bytes:
    try:
        args = ["gh", "api", path, "--allow-escape-sequences"]
        if hostname:
            args += ["--hostname", hostname]
        result = RunningProcess.run(
            args,
            capture_output=True,
            stderr=PIPE,
            text=False,
            timeout=60,
        )
    except (OSError, TimeoutError, TimeoutExpired) as exc:
        raise RuntimeError(f"GitHub request failed: {exc}") from exc
    if b"clud: gh read broker: rate-limit reserve" in (result.stderr or b""):
        raise RuntimeError("GitHub broker returned stale data; retry later")
    if result.returncode != 0:
        message = (result.stderr or b"").decode("utf-8", "replace").strip()
        raise RuntimeError(f"GitHub request failed: {message[:500]}")
    return result.stdout or b""



def github_download(path: str, destination: Path, hostname: str | None) -> None:
    """Redirect gh stdout directly to a private file, preserving raw bytes."""
    if os.name == "nt":
        if any(c in str(destination) for c in '%!"&|<>^$`'):
            raise ValueError("artifact path contains unsupported Windows shell characters")
        env = os.environ.copy()
        env.update(CLUD_CI_API=path, CLUD_CI_LOG=str(destination))
        host_arg = ''
        if hostname:
            env["CLUD_CI_HOST"] = hostname
            host_arg = ' --hostname "%CLUD_CI_HOST%"'
        script = f'gh api "%CLUD_CI_API%"{host_arg} --allow-escape-sequences > "%CLUD_CI_LOG%"'
        command = ["cmd", "/d", "/s", "/c", script]
    else:
        env = None
        host_arg = ' --hostname "$3"' if hostname else ''
        script = f'exec gh api "$1"{host_arg} --allow-escape-sequences > "$2"'
        command = ["sh", "-c", script, "clud-ci-log", path, str(destination)]
        if hostname:
            command.append(hostname)
    try:
        result = RunningProcess.run(command, capture_output=True, stderr=PIPE,
                                    text=False, timeout=60, env=env)
    except (OSError, TimeoutError, TimeoutExpired) as exc:
        raise RuntimeError(f"GitHub log request failed: {exc}") from exc
    stderr = result.stderr or b""
    if b"clud: gh read broker: rate-limit reserve" in stderr:
        raise RuntimeError("GitHub broker returned stale data; retry later")
    if result.returncode != 0:
        message = stderr.decode("utf-8", "replace").strip()
        raise RuntimeError(f"GitHub log request failed (exit {result.returncode}): {message[:500]}")


def safe_display_path(path: Path) -> None:
    text = str(path)
    if len(os.fsencode(text)) > 300 or not all(c.isprintable() for c in text):
        raise ValueError("artifact path is too long or contains control characters")



def quote_path(path: Path, windows: bool | None = None) -> str:
    value = str(path)
    safe_display_path(path)
    if windows is None:
        windows = os.name == "nt"
    if windows:
        if any(c in value for c in '%!"&|<>^$`'):
            raise ValueError("artifact path contains unsupported Windows shell characters")
        return f'"{value}"'
    return shlex.quote(value)



def check_ancestor(info) -> None:
    if not hasattr(os, "getuid"):
        return
    if info.st_uid not in (0, os.getuid()):
        raise ValueError("artifact directory has an untrusted ancestor owner")
    if info.st_mode & 0o022 and not info.st_mode & stat.S_ISVTX:
        raise ValueError("artifact directory has a writable non-sticky ancestor")


def artifact_root() -> Path:
    if os.name == "nt":
        raise ValueError("Windows artifact privacy verification is not available yet")
    root = Path(os.environ.get("CLUD_TOOL_OUTPUT_DIR") or Path.home() / ".clud/tmp/tool-output")
    safe_display_path(root)
    if len(os.fsencode(root)) > 200:
        raise ValueError("artifact directory path is too long")
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    base = root.resolve(strict=True)
    for directory in (base, *base.parents):
        check_ancestor(directory.stat())
    private = base / "ci-failure-private"
    private.mkdir(mode=0o700, exist_ok=True)
    if private.is_symlink():
        raise ValueError("artifact directory must not be a symlink")
    resolved = private.resolve(strict=True)
    safe_display_path(resolved)
    info = resolved.stat()
    if hasattr(os, "getuid") and (info.st_uid != os.getuid() or info.st_mode & 0o077):
        raise ValueError("artifact directory must be private and owned by the current user")
    return resolved


def save_artifact(raw: bytes) -> Path:
    root = artifact_root()
    fd, name = tempfile.mkstemp(prefix="ci-failure-", suffix=".log", dir=root)
    with os.fdopen(fd, "wb") as stream:
        stream.write(raw)
    return Path(name)


def open_artifact(path: Path):
    """Open only an owned regular report inside the configured artifact root."""
    safe_display_path(path)
    root = artifact_root()
    if path.parent.resolve(strict=True) != root:
        raise ValueError("artifact is outside the tool output directory")
    if not re.fullmatch(r"ci-failure-[a-zA-Z0-9_-]{8}\.log", path.name):
        raise ValueError("artifact is not a CI failure report")
    if path.is_symlink():
        raise ValueError("artifact symlinks are not allowed")
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode):
            raise ValueError("artifact is not a regular file")
        if hasattr(os, "getuid") and (info.st_uid != os.getuid() or info.st_mode & 0o077):
            raise ValueError("artifact is not private and owned by the current user")
        return os.fdopen(fd, "rb")
    except BaseException:
        os.close(fd)
        raise


def print_range(path: Path, start: int, count: int) -> None:
    if start < 1 or count < 1 or count > 60:
        raise ValueError("start-line must be positive and line-count must be 1..60")
    lines = []
    with open_artifact(path) as stream:
        for number, (position, line) in enumerate(log_lines(stream), 1):
            if number >= start + count:
                break
            if number >= start:
                lines.append(f"{number} [byte {position}]: {line}")
    if not lines:
        raise ValueError("requested range is beyond the artifact")
    text, shown = bounded_parts(lines)
    print(text)
    next_line = start + shown
    print(f"Continue at line {next_line}: --artifact {quote_path(path)} "
          f"--start-line {next_line}")
    print("For a truncated line, use its byte offset with --byte-offset.")



def print_bytes(path: Path, offset: int, count: int) -> None:
    if offset < 0 or count < 1 or count > 80:
        raise ValueError("byte-offset must be nonnegative and byte-count must be 1..80")
    with open_artifact(path) as stream:
        stream.seek(offset)
        data = stream.read(count)
    if not data:
        raise ValueError("requested byte offset is beyond the artifact")
    print(json.dumps(data.decode("utf-8", "replace"), ensure_ascii=True))
    print(f"hex: {data.hex()}")
    print(f"Continue at byte {offset + len(data)}: --artifact {quote_path(path)} "
          f"--byte-offset {offset + len(data)}")


def read_job(repo: str, run: int, job: int) -> tuple[str, str | None]:
    parts = repo.split("/")
    if len(parts) not in (2, 3) or not all(
        re.fullmatch(r"[A-Za-z0-9_.:-]+", part) for part in parts
    ):
        raise ValueError("repository must be OWNER/REPO or HOST/OWNER/REPO")
    host = parts[0] if len(parts) == 3 else None
    owner_repo = "/".join(parts[-2:])
    if run < 1 or job < 1:
        raise ValueError("run and job IDs must be positive")
    path = f"repos/{owner_repo}/actions/jobs/{job}"
    metadata = json.loads(github_get(path, host) if host else github_get(path))
    if not isinstance(metadata, dict) or metadata.get("run_id") != run:
        raise ValueError("job does not belong to the requested run")
    if metadata.get("status") != "completed":
        raise ValueError(f"job status is {metadata.get('status')!s}; log may be incomplete")
    if metadata.get("conclusion") not in ("failure", "cancelled", "timed_out", "startup_failure"):
        raise ValueError("job is not a completed failed job")
    return path + "/logs", host


def nonempty_log(stream) -> bool:
    while chunk := stream.read(65536):
        if chunk.strip():
            return True
    return False



class BoundedParser(argparse.ArgumentParser):
    def error(self, message: str) -> None:
        raise ValueError(clean(message)[:700])


def main(argv: list[str] | None = None) -> int:
    parser = BoundedParser(prog="ci_failure_report", description=__doc__)
    parser.add_argument("--repo")
    parser.add_argument("--run", type=int)
    parser.add_argument("--job", type=int)
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--start-line", type=int, default=1)
    parser.add_argument("--line-count", type=int, default=40)
    parser.add_argument("--byte-offset", type=int)
    parser.add_argument("--byte-count", type=int, default=80)
    try:
        args = parser.parse_args(argv)
        if args.artifact:
            if args.repo or args.run or args.job:
                raise ValueError("artifact continuation cannot be combined with GitHub selectors")
            if args.byte_offset is not None:
                print_bytes(args.artifact, args.byte_offset, args.byte_count)
            else:
                print_range(args.artifact, args.start_line, args.line_count)
            return 0
        if not args.repo or args.run is None or args.job is None:
            raise ValueError("provide --repo, --run, and --job")
        log_path, host = read_job(args.repo, args.run, args.job)
        try:
            artifact = save_artifact(b"")
            github_download(log_path, artifact, host)
            with open_artifact(artifact) as stream:
                if not nonempty_log(stream):
                    raise ValueError("job log is empty or still uploading; retry later")
                summary = summarize_stream(stream)
        except OSError as exc:
            raise ValueError(f"could not save full log artifact: {exc}") from exc
        print(summary)
        print(f"Full raw log: {artifact}")
        print(f"Continue with --artifact {quote_path(artifact)} "
              "--start-line 1 --line-count 40")
        return 0
    except (RuntimeError, ValueError, OSError) as exc:
        print(f"ci_failure_report: {clean(str(exc))[:700]}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
