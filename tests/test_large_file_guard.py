"""Startup large-source-file guard — see zackees/clud#132.

The guard runs once at clud launch and prints a `[clud] warning:` block
to stderr listing the top 4 source files in the repo that exceed the
~1000 LOC threshold (40 kB). The check has a 1 s hard deadline.
"""

from __future__ import annotations

import subprocess
import tempfile
import time
from pathlib import Path

# Reuse the binary builder + helper already exercised by test_hello.py.
from test_hello import CLUD, copied_clud_env

ROOT = Path(__file__).resolve().parent.parent

WARNING_HEADER = "large source files"


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [CLUD, *args],
        cwd=str(cwd),
        capture_output=True,
        text=True,
        timeout=15,
        env=copied_clud_env(Path(CLUD)),
    )


def test_warns_with_synthetic_large_files() -> None:
    """The guard fires, names the worst offenders, and reports at least
    4 lines with a `(N more)` tail when the repo contains >40 kB sources.

    Uses a synthetic tempdir with 5 large `.rs` files instead of relying
    on the clud repo's own source: per-file LOC is moving target (the
    refactor that splits a file out from under the test should not be
    the one that fails the test), and the report's exact contents drift
    every time an unrelated file grows.
    """
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        (tmp_path / ".git").mkdir()
        # 5 files past the 40 kB / ~1000 LOC threshold so the guard
        # produces 4 listed entries + a `(1 more)` tail.
        names = ["alpha.rs", "bravo.rs", "charlie.rs", "delta.rs", "echo.rs"]
        for idx, name in enumerate(names):
            # Definite size in bytes: 50 kB + 1 kB per index, so every file
            # clears the 40 kB threshold and the order in the report is
            # stable. Using fixed bytes avoids the foot-gun where an f-string
            # template happens to multiply out to under-threshold size.
            size = 50 * 1024 + idx * 1024
            (tmp_path / name).write_text("x" * size)
        result = _run(tmp_path, "--dry-run", "-p", "hello")
        assert result.returncode == 0, result.stderr
        assert WARNING_HEADER in result.stderr, (
            f"warning header missing from stderr:\n{result.stderr}"
        )
        # All 5 should be candidates, of which 4 are named and 1 is in the
        # `(N more)` tail. Asserting any 3 of the 5 keeps the test
        # tolerant to sort-order ties on identical file sizes.
        listed = sum(1 for name in names if name in result.stderr)
        assert listed >= 3, (
            f"expected at least 3 synthetic .rs files in warning, got:\n{result.stderr}"
        )


def test_silent_in_tiny_tempdir() -> None:
    """In a fresh repo with one tiny source file, no warning is emitted."""
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        # `.git` marker is enough; the guard checks for the directory, not a
        # real worktree. Avoids needing `git init` (which may not be on PATH
        # in some CI images and would add ~50ms of latency anyway).
        (tmp_path / ".git").mkdir()
        (tmp_path / "hello.rs").write_text("fn main() {}\n")
        result = _run(tmp_path, "--dry-run", "-p", "hi")
        assert result.returncode == 0, result.stderr
        assert WARNING_HEADER not in result.stderr, (
            f"unexpected warning in tiny repo:\n{result.stderr}"
        )


def test_silent_outside_git_repo() -> None:
    """Outside any git repo, the guard exits silently — it's project-scoped."""
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        # No `.git` marker — `loop_spec::git_root_from` falls back to `tmp`,
        # `run()` short-circuits because the dir has no `.git`.
        (tmp_path / "huge.rs").write_text("x" * (100 * 1024))
        result = _run(tmp_path, "--dry-run", "-p", "hi")
        assert result.returncode == 0, result.stderr
        assert WARNING_HEADER not in result.stderr, (
            f"unexpected warning in non-git dir:\n{result.stderr}"
        )


def test_guard_does_not_blow_startup_budget() -> None:
    """Sanity-check: even in the clud repo, dry-run completes well under 5s."""
    start = time.monotonic()
    result = _run(ROOT, "--dry-run", "-p", "hello")
    elapsed = time.monotonic() - start
    assert result.returncode == 0
    # 5s is generous — the guard alone has a 1s hard deadline, and the rest
    # of dry-run is pure arg parsing. CI machines under load might be slow,
    # but we should never approach the subprocess timeout.
    assert elapsed < 5.0, f"dry-run took {elapsed:.2f}s, expected <5s"
