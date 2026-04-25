"""Lint rule: ban direct subprocess/PTY calls in Rust source.

All process execution must go through running-process-core.
This script scans .rs files (excluding testbins/) for banned patterns
and fails the build if any are found.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Patterns that indicate direct subprocess usage (banned in crates/)
BANNED_PATTERNS: list[tuple[str, str]] = [
    (r"\bstd::process::Command\b", "use running_process_core::NativeProcess instead"),
    (r"\bprocess::Command\b", "use running_process_core::NativeProcess instead"),
    (r"\bCommand::new\b", "use running_process_core::NativeProcess instead"),
    (r"\bstd::process::Stdio\b", "use running_process_core StdinMode/StderrMode instead"),
    (r"\bstd::process::Child\b", "use running_process_core::NativeProcess instead"),
    (r"\bstd::process::Output\b", "use running_process_core::NativeProcess instead"),
    (r"\buse std::process::\{", "use running_process_core instead of std::process"),
    # Tokio's async process API is also banned — running-process-core is the
    # single chokepoint. If async is needed, extend running-process-core.
    (r"\btokio::process\b", "use running_process_core::NativeProcess instead"),
    (r"\buse tokio::process\b", "use running_process_core instead of tokio::process"),
]

# Only std::process::exit is allowed (it's not subprocess spawning)
ALLOWED_PATTERNS: list[str] = [
    r"std::process::exit",
    r"process::exit",
]


def is_allowed(line: str) -> bool:
    """Check if the line only uses allowed std::process items."""
    return any(re.search(pat, line) for pat in ALLOWED_PATTERNS)


def scan_file(path: Path) -> list[tuple[int, str, str]]:
    """Scan a single file for banned patterns. Returns (line_num, line, reason)."""
    violations: list[tuple[int, str, str]] = []
    try:
        content = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return violations

    for line_num, line in enumerate(content.splitlines(), start=1):
        stripped = line.strip()
        # Skip comments
        if stripped.startswith("//"):
            continue
        # Skip if it's an allowed usage
        if is_allowed(stripped):
            continue
        for pattern, reason in BANNED_PATTERNS:
            if re.search(pattern, stripped):
                violations.append((line_num, stripped, reason))
                break  # One violation per line is enough

    return violations


def main() -> int:
    # Scan all .rs files in crates/ (not testbins/ — mocks can use std::process)
    crates_dir = ROOT / "crates"
    if not crates_dir.is_dir():
        print("No crates/ directory found, skipping banned import check.", file=sys.stderr)
        return 0

    # trampoline.rs is exempt — it must use std::process::Command to re-exec
    # before running-process-core is involved.
    exempt = {"trampoline.rs"}
    rs_files = sorted(crates_dir.rglob("*.rs"))
    total_violations = 0

    for path in rs_files:
        if path.name in exempt:
            continue
        violations = scan_file(path)
        for line_num, line, reason in violations:
            rel = path.relative_to(ROOT)
            print(f"{rel}:{line_num}: BANNED — {reason}", file=sys.stderr)
            print(f"  {line}", file=sys.stderr)
            total_violations += 1

    if total_violations > 0:
        print(
            f"\n{total_violations} banned import(s) found. "
            "All subprocess execution must use running-process-core.",
            file=sys.stderr,
        )
        return 1

    print("No banned subprocess imports found.", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
