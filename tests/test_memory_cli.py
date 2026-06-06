"""Issue #262: surface-level subprocess tests for `clud memory <verb>`.

These tests cover only the argv parser + dispatcher entry points. The
full save/recall/forget loop exercises real SQLite + tantivy + an
embedder and is covered by the rust-side `daemon::http::tests`
integration tests in `crates/clud-bin/src/daemon/http.rs`.
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
        _cargo_argv(["build", "-p", "clud", "--no-default-features", "--message-format=json"]),
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


def _run(
    *args: str, env_overrides: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    """Run clud in a tempdir with copied binary; mirror tests/test_hello.py."""
    with tempfile.TemporaryDirectory() as temp_dir:
        source = Path(CLUD)
        launch = Path(temp_dir) / source.name
        shutil.copy2(source, launch)
        env = os.environ.copy()
        if env_overrides:
            env.update(env_overrides)
        return subprocess.run(
            [str(launch), *args],
            capture_output=True,
            text=True,
            timeout=20,
            env=env,
        )


def test_memory_bare_prints_help() -> None:
    result = _run("memory")
    assert result.returncode == 0, result.stderr
    combined = (result.stdout + result.stderr).lower()
    assert "memory" in combined
    assert "init" in combined or "save" in combined or "search" in combined


def test_memory_help_lists_verbs() -> None:
    result = _run("memory", "--help")
    assert result.returncode == 0, result.stderr
    out = result.stdout + result.stderr
    for verb in ("init", "status", "search", "save", "forget", "reembed", "branch-isolate"):
        assert verb in out, f"missing {verb} in help output:\n{out}"


def test_memory_export_to_disk_outside_git_repo_fails() -> None:
    # Post-#264: --to-disk writes under <git-root>/.clud/memory/, so a
    # non-git cwd is a hard user error (exit 1).
    with tempfile.TemporaryDirectory() as temp_dir:
        source = Path(CLUD)
        launch = Path(temp_dir) / source.name
        shutil.copy2(source, launch)
        not_a_repo = Path(temp_dir) / "not-a-repo"
        not_a_repo.mkdir()
        result = subprocess.run(
            [str(launch), "memory", "export", "--to-disk"],
            cwd=not_a_repo,
            capture_output=True,
            text=True,
            timeout=20,
            env=os.environ.copy(),
        )
    assert result.returncode == 1, (
        f"expected 1, got {result.returncode}; stdout={result.stdout!r} stderr={result.stderr!r}"
    )
    assert "git" in result.stderr.lower() or "not a git" in result.stderr.lower()


def test_memory_import_from_disk_outside_git_repo_fails() -> None:
    with tempfile.TemporaryDirectory() as temp_dir:
        source = Path(CLUD)
        launch = Path(temp_dir) / source.name
        shutil.copy2(source, launch)
        not_a_repo = Path(temp_dir) / "not-a-repo"
        not_a_repo.mkdir()
        result = subprocess.run(
            [str(launch), "memory", "import", "--from-disk"],
            cwd=not_a_repo,
            capture_output=True,
            text=True,
            timeout=20,
            env=os.environ.copy(),
        )
    assert result.returncode == 1
    assert "git" in result.stderr.lower()


def test_memory_status_no_daemon_exits_3() -> None:
    # `--no-daemon memory status` short-circuits with the daemon-unavailable
    # exit code (3) before attempting any HTTP call.
    result = _run("--no-daemon", "memory", "status")
    assert result.returncode == 3, (
        f"expected exit 3, got rc={result.returncode}; "
        f"stdout={result.stdout!r} stderr={result.stderr!r}"
    )
    assert "daemon" in result.stderr.lower()


def test_memory_search_no_daemon_exits_3() -> None:
    result = _run("--no-daemon", "memory", "search", "foo")
    assert result.returncode == 3
    assert "daemon" in result.stderr.lower()


def test_memory_save_no_daemon_exits_3() -> None:
    result = _run("--no-daemon", "memory", "save", "hello")
    assert result.returncode == 3
    assert "daemon" in result.stderr.lower()


def test_memory_search_help_smoke() -> None:
    # Every verb's --help must exit 0 and mention its own name. Probe the
    # heaviest verb (with the most flags) as a smoke test.
    result = _run("memory", "search", "--help")
    assert result.returncode == 0, result.stderr
    out = result.stdout + result.stderr
    for opt in ("--k", "--session-id", "--tier-floor", "--scope-key", "--json"):
        assert opt in out, f"missing {opt} in search --help:\n{out}"


def test_memory_save_help_smoke() -> None:
    result = _run("memory", "save", "--help")
    assert result.returncode == 0, result.stderr
    out = result.stdout + result.stderr
    for opt in ("--tier", "--session-id", "--metadata", "--json"):
        assert opt in out, f"missing {opt} in save --help:\n{out}"


def test_memory_branch_isolate_outside_git_repo_reports_error() -> None:
    # branch-isolate touches the local working tree and never needs the
    # daemon; run it in a non-git tempdir and assert the error path.
    with tempfile.TemporaryDirectory() as temp_dir:
        source = Path(CLUD)
        launch = Path(temp_dir) / source.name
        shutil.copy2(source, launch)
        not_a_repo = Path(temp_dir) / "not-a-repo"
        not_a_repo.mkdir()
        result = subprocess.run(
            [str(launch), "memory", "branch-isolate"],
            cwd=not_a_repo,
            capture_output=True,
            text=True,
            timeout=20,
            env=os.environ.copy(),
        )
    assert result.returncode == 1
    assert "git" in result.stderr.lower() or "not a git" in result.stderr.lower()
