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


def test_warns_in_clud_repo() -> None:
    """Inside the clud repo, the guard fires, names the worst offenders,
    and reports at least 4 lines with a `(N more)` tail.

    Naming exact files is brittle (a single edit on top of any file could
    flip the leaderboard), so we assert (a) the worst three known-large
    `.rs` files are all listed, and (b) the report is capped at 4 entries
    with a `(N more)` tail proving there are extras the user should look
    into.
    """
    result = _run(ROOT, "--dry-run", "-p", "hello")
    assert result.returncode == 0, result.stderr
    assert WARNING_HEADER in result.stderr, (
        f"warning header missing from stderr:\n{result.stderr}"
    )
    # `daemon.rs`, `command.rs`, and `voice.rs` have been the top 3 by
    # source-file size in clud for a long time; if any of these drops out
    # of the report a non-trivial refactor has landed.
    for expected in ("daemon.rs", "command.rs", "voice.rs"):
        assert expected in result.stderr, (
            f"expected {expected} in warning, got:\n{result.stderr}"
        )
    # Report must include a `(N more)` tail — clud has more than 4 files
    # over the threshold (Rust sources, large integration tests, etc.).
    assert "more)" in result.stderr, (
        f"expected `(N more)` tail in warning, got:\n{result.stderr}"
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
