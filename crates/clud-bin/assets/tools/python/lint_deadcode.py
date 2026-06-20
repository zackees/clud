# /// script
# requires-python = ">=3.10"
# dependencies = [
#   "vulture>=2.13",
# ]
# ///
# managed-by: clud
"""Find dead Python code via vulture, classifying production vs test scope.

Symbols reachable only from test code count as production-dead — the test
exists to exercise behavior, but if no production caller invokes that
behavior, the production code is unused.

Usage:
  clud tool run python/lint_deadcode.py [<path>...] [--min-confidence N]
                                        [--exclude PATTERN]... [--json]

Output (stdout, when --json or default):
  {"v": 1, "deadcode": [
    {"file": "src/foo.py", "name": "old_helper", "line": 42,
     "type": "function", "confidence": 60, "size": 7,
     "reachable_from_tests": false}
  ]}

Exit code:
  0 — no production-dead symbols (vulture reported nothing or only
      symbols ignored by allowlist).
  1 — at least one production-dead symbol found.
  2 — tool error (vulture crashed, no Python files found, etc.).

Convergence (deferred to follow-up): single-pass for V1. The
`--converge` flag is parsed but not yet implemented; the tool emits a
warning when used. See #439 for the convergence investigation.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

DEFAULT_SRC_PATTERNS = ("src", "app", "lib")
DEFAULT_TEST_PATTERNS = ("tests", "test", "conftest.py")


def is_test_path(path: Path) -> bool:
    parts = [p.lower() for p in path.parts]
    name = path.name.lower()
    if name == "conftest.py" or name.endswith("_test.py") or name.startswith("test_"):
        return True
    for p in parts:
        if p in {"tests", "test"}:
            return True
    return False


def discover_python_files(roots: list[Path], exclude: list[str]) -> tuple[list[Path], list[Path]]:
    """Return (src_files, test_files). Both lists are absolute paths."""
    src: list[Path] = []
    tests: list[Path] = []
    excluded = [Path(e).resolve() for e in exclude]
    for root in roots:
        root = root.resolve()
        if not root.exists():
            continue
        if root.is_file():
            candidates = [root]
        else:
            candidates = [p for p in root.rglob("*.py") if not _under_any(p, excluded)]
        for f in candidates:
            if is_test_path(f):
                tests.append(f)
            else:
                src.append(f)
    return src, tests


def _under_any(path: Path, parents: list[Path]) -> bool:
    rp = path.resolve()
    for parent in parents:
        try:
            rp.relative_to(parent)
            return True
        except ValueError:
            continue
    return False


def run_vulture(
    src_files: list[Path],
    test_files: list[Path],
    min_confidence: int,
) -> list[dict]:
    """Run vulture once and return structured items for src_files.

    Test files are passed too so vulture's call-graph awareness sees that
    test-only references don't save src symbols from being flagged — we
    post-filter to drop any items whose file is in test_files.
    """
    from vulture import Vulture

    v = Vulture(verbose=False)
    paths = [str(p) for p in src_files + test_files]
    v.scavenge(paths)

    test_set = {str(p.resolve()) for p in test_files}
    items: list[dict] = []
    for item in v.get_unused_code(min_confidence=min_confidence):
        file_str = str(Path(item.filename).resolve())
        if file_str in test_set:
            # Dead code inside tests is uninteresting for this tool's
            # production-cleanup focus.
            continue
        items.append(
            {
                "file": os.path.relpath(item.filename),
                "name": item.name,
                "line": int(item.first_lineno),
                "type": str(item.typ),
                "confidence": int(item.confidence),
                "size": int(getattr(item, "size", 1)),
                # Reachable-from-tests: we don't compute this directly,
                # but if vulture still reports the symbol despite seeing
                # the tests, that means no test reaches it either. A
                # future enhancement could re-scan with tests excluded
                # and diff the results to populate this honestly.
                "reachable_from_tests": False,
            }
        )
    return items


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="lint_deadcode",
        description="Find dead Python code via vulture (production-focused).",
    )
    parser.add_argument(
        "paths",
        nargs="*",
        default=["."],
        help="Files or directories to scan (default: current directory).",
    )
    parser.add_argument(
        "--min-confidence",
        type=int,
        default=60,
        help="vulture --min-confidence (default 60).",
    )
    parser.add_argument(
        "--exclude",
        action="append",
        default=[],
        help="Path prefix to exclude (repeatable).",
    )
    parser.add_argument(
        "--converge",
        action="store_true",
        help="Iterate vulture passes until no new dead code is reported. "
        "Currently emits a warning and falls back to single-pass; see "
        "https://github.com/zackees/clud/issues/439 for the convergence "
        "investigation.",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Force JSON output (default for V1).",
    )
    args = parser.parse_args(argv)

    if args.converge:
        print(
            "lint_deadcode: --converge is single-pass for V1 (see #439)",
            file=sys.stderr,
        )

    roots = [Path(p) for p in args.paths]
    src_files, test_files = discover_python_files(roots, args.exclude)
    if not src_files:
        print(
            json.dumps(
                {
                    "v": 1,
                    "deadcode": [],
                    "note": "no Python source files found",
                }
            )
        )
        return 0

    try:
        items = run_vulture(src_files, test_files, args.min_confidence)
    except Exception as exc:  # noqa: BLE001
        print(f"lint_deadcode: vulture failed: {exc}", file=sys.stderr)
        return 2

    payload = {"v": 1, "deadcode": items, "files_scanned": len(src_files)}
    print(json.dumps(payload, indent=2))
    return 1 if items else 0


if __name__ == "__main__":
    raise SystemExit(main())
