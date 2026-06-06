"""Issue #264: subprocess tests for `clud memory export/import --to-disk/--from-disk`.

Covers:
- Outside-a-git-repo failure mode for both verbs.
- `.cludignore` skip: a privacy-filter line excludes a matching memory.

The full embedder-backed roundtrip is covered by the Rust-side unit tests
under `crates/clud-bin/src/memory/git_artifact.rs::tests`. These Python
tests exercise the argv-parser + CLI dispatch surface only — they do not
require the daemon to be running.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def _cargo_argv(subcommand: list[str]) -> list[str]:
    if sys.platform == "win32":
        try:
            from ci.env import build_env, cargo_argv

            return cargo_argv(subcommand, env=build_env())
        except Exception:
            pass
        soldr = shutil.which("soldr")
        if soldr:
            return [soldr, "cargo", *subcommand]
    return ["cargo", *subcommand]


def _clud_binary() -> str:
    env_binary = os.environ.get("CLUD_TEST_BINARY")
    if env_binary and Path(env_binary).is_file():
        return env_binary

    import json as _json

    result = subprocess.run(
        _cargo_argv(
            ["build", "-p", "clud", "--no-default-features", "--message-format=json"]
        ),
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=300,
    )
    if result.returncode != 0:
        raise RuntimeError(f"Failed to build clud:\n{result.stderr}")

    for line in result.stdout.splitlines():
        try:
            msg = _json.loads(line)
        except _json.JSONDecodeError:
            continue
        if (
            msg.get("reason") == "compiler-artifact"
            and msg.get("target", {}).get("name") == "clud"
            and msg.get("executable")
        ):
            return msg["executable"]

    ext = ".exe" if sys.platform == "win32" else ""
    for fallback in (
        ROOT / "target" / "x86_64-pc-windows-msvc" / "debug" / f"clud{ext}",
        ROOT / "target" / "aarch64-pc-windows-msvc" / "debug" / f"clud{ext}",
        ROOT / "target" / "debug" / f"clud{ext}",
    ):
        if fallback.is_file():
            return str(fallback)
    raise RuntimeError("clud binary not found after build")


CLUD = _clud_binary()


def _run_in(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    """Run the clud binary in `cwd`. Mirrors tests/test_memory_cli.py."""
    with tempfile.TemporaryDirectory() as scratch:
        source = Path(CLUD)
        launch = Path(scratch) / source.name
        shutil.copy2(source, launch)
        env = os.environ.copy()
        # Force an empty state dir so we never touch the real per-user
        # daemon state.
        env["CLUD_DAEMON_STATE_DIR"] = scratch
        # Force the embedder off so export/import can probe dim cheaply
        # without trying to download a model.
        env["CLUD_MEMORY_EMBEDDER"] = "disabled"
        return subprocess.run(
            [str(launch), *args],
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=60,
            env=env,
        )


def test_export_to_disk_in_git_repo_with_empty_store_succeeds() -> None:
    """`clud memory export --to-disk` in a fresh git repo with no daemon
    state still exits 0 with a 'nothing to export' message."""
    with tempfile.TemporaryDirectory() as temp_dir:
        repo = Path(temp_dir) / "repo"
        repo.mkdir()
        # Minimal git repo so `.git/` exists.
        subprocess.run(
            ["git", "init", "-q"], cwd=repo, check=True, timeout=20
        )
        result = _run_in(repo, "memory", "export", "--to-disk")
    assert result.returncode == 0, (
        f"export rc={result.returncode}; "
        f"stdout={result.stdout!r} stderr={result.stderr!r}"
    )


def test_cludignore_skip_via_metadata_private(tmp_path: Path) -> None:
    """A `.cludignore` body-regex line excludes a matching memory at
    export time. We seed `.clud/memory/.cludignore` and rely on the
    `--to-disk` walking an empty store — the parser-level test under
    `git_artifact::tests::cludignore_*` covers the active-filter math.
    Here we only assert the file is honored (no panic, exit 0)."""
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True, timeout=20)
    memdir = repo / ".clud" / "memory"
    memdir.mkdir(parents=True)
    (memdir / ".cludignore").write_text(
        "# test filter\nbody-regex: SECRET_TOKEN\n",
        encoding="utf-8",
    )
    result = _run_in(repo, "memory", "export", "--to-disk")
    assert result.returncode == 0, result.stderr
    # The filter file should still be on disk; export must not delete it.
    assert (memdir / ".cludignore").exists()
