"""Fixtures for integration tests with mock agents."""

from __future__ import annotations

import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent.parent

# Make ci.env importable so we can reuse its MSVC-forcing env on Windows.
sys.path.insert(0, str(ROOT))


def _cargo_argv(subcommand: list[str]) -> list[str]:
    """Return the cargo argv, preferring `soldr cargo` on Windows.

    Windows rustc installations from chocolatey ship a GNU-host rustc which
    links C++ deps (whisper-rs) against MinGW runtime DLLs
    (libstdc++-6.dll, libgcc_s_seh-1.dll, libwinpthread-1.dll). Those DLLs
    aren't present on stock Windows, so the resulting debug binary fails
    with STATUS_ENTRYPOINT_NOT_FOUND when launched as a subprocess.

    `soldr cargo ...` forces the MSVC target, which links against
    VCRUNTIME140.dll / MSVCP140.dll — both ship with Windows 10+.
    """
    if sys.platform == "win32":
        soldr = shutil.which("soldr")
        if soldr:
            return [soldr, "cargo", *subcommand]
    return ["cargo", *subcommand]


def _cargo_build_env() -> dict[str, str]:
    """Return the env used for building clud/mock-agent in tests.

    On Windows, reuse ci.env.build_env() which pins RUSTUP_TOOLCHAIN and
    CARGO_BUILD_TARGET to the MSVC variants — the same env the wheel build
    uses. This is a fallback for systems without `soldr`.
    """
    if sys.platform != "win32":
        return os.environ.copy()
    try:
        from ci.env import build_env  # type: ignore[import-not-found]

        return build_env()
    except Exception:
        return os.environ.copy()


def _build_env_without_sccache() -> dict[str, str]:
    """Cargo build env with sccache disabled.

    Why: when RUSTC_WRAPPER=sccache (CI default), cargo invokes rustc via
    sccache, which lazily starts a persistent server daemon. That server
    **inherits the subprocess stdio pipe handles** and keeps them open for
    its whole idle lifetime — so `capture_output=True` never sees EOF and
    `communicate()` hangs indefinitely on Windows runners. Stripping the
    wrapper for the fixture's cargo call avoids the inheritance entirely.
    The outer workflow's `Dev build` step already warmed the cache; this
    fixture call is usually a no-op incremental build anyway. See #37.
    """
    env = _cargo_build_env()
    env.pop("RUSTC_WRAPPER", None)
    env.pop("SCCACHE_GHA_ENABLED", None)
    return env


def _target_search_dirs() -> list[Path]:
    """Directories where a built binary might land, ordered by preference."""
    ext = ".exe" if sys.platform == "win32" else ""
    dirs = []
    if sys.platform == "win32":
        dirs.extend(
            [
                ROOT / "target" / "x86_64-pc-windows-msvc" / "debug",
                ROOT / "target" / "aarch64-pc-windows-msvc" / "debug",
            ]
        )
    dirs.append(ROOT / "target" / "debug")
    return [d for d in dirs if (d / f"probe{ext}") or d.is_dir()]


def _find_built_binary(name: str) -> Path | None:
    ext = ".exe" if sys.platform == "win32" else ""
    for d in _target_search_dirs():
        candidate = d / f"{name}{ext}"
        if candidate.is_file():
            return candidate
    return None


def _cargo_build_inherit_stdio(package: str) -> None:
    """Build a workspace package, streaming cargo output to our own stdout
    and stderr rather than capturing via pipes.

    Capturing cargo's output through pipes on Windows GHA runners caused an
    indefinite hang (#37): `communicate()` waited for pipe EOF but a
    grand-child sccache server daemon — or some other long-lived descendant
    — kept the inheritable pipe handles open. Letting cargo's output go
    straight to the CI log sidesteps the issue entirely and still produces
    the artifacts at a deterministic path.
    """
    result = subprocess.run(
        _cargo_argv(["build", "-p", package]),
        cwd=ROOT,
        env=_build_env_without_sccache(),
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"cargo build -p {package} exited with code {result.returncode}"
        )


def _find_clud() -> Path:
    """Build the current repo's clud binary and return its path."""
    _cargo_build_inherit_stdio("clud")
    binary = _find_built_binary("clud")
    if binary is None:
        raise RuntimeError("clud binary not found after build")
    return binary


def _build_mock_agent() -> Path:
    """Build the mock-agent binary and return its path."""
    _cargo_build_inherit_stdio("mock-agent")
    binary = _find_built_binary("mock-agent")
    if binary is None:
        raise RuntimeError("mock-agent binary not found after build")
    return binary


@pytest.fixture(scope="session")
def mock_agent_binary() -> Path:
    """Build mock-agent once per test session."""
    return _build_mock_agent()


@pytest.fixture
def mock_env(mock_agent_binary: Path, tmp_path: Path) -> dict[str, str]:
    """Create a temp directory with mock claude/codex binaries on PATH.

    Returns an environment dict with PATH set so that `claude` and `codex`
    resolve to the mock-agent binary.
    """
    ext = ".exe" if platform.system() == "Windows" else ""

    # Copy mock-agent as both claude and codex
    claude_path = tmp_path / f"claude{ext}"
    codex_path = tmp_path / f"codex{ext}"
    shutil.copy2(mock_agent_binary, claude_path)
    shutil.copy2(mock_agent_binary, codex_path)

    if platform.system() != "Windows":
        claude_path.chmod(0o755)
        codex_path.chmod(0o755)

    # Build env with mock dir first on PATH
    env = os.environ.copy()
    env["PATH"] = str(tmp_path) + os.pathsep + env.get("PATH", "")
    # Prevent any VIRTUAL_ENV interference
    env.pop("VIRTUAL_ENV", None)

    # Enable PID logging for zombie detection
    pid_log = tmp_path / "child_pids.log"
    env["RUNNING_PROCESS_CHILD_PID_LOG_PATH"] = str(pid_log)

    # Skip the Windows exe-unlock dance (rename+copy+GC on every start).
    # The unlock path is under investigation in #37 as the suspected cause
    # of a pipe-EOF hang on Windows CI where subprocess.run cannot flush
    # stdout/stderr of a `clud --version` subprocess. Tests don't need the
    # hot-reload safety net anyway; the repo isn't pip-installing over a
    # running clud during tests.
    env["CLUD_NO_UNLOCK"] = "1"

    return env


@pytest.fixture
def clud_binary() -> Path:
    """Return the path to the current repo's clud binary."""
    return _find_clud()


def _scan_for_clud_zombies() -> list[dict]:
    """Scan the system for orphaned CLUD-spawned processes."""
    ext = ".exe" if sys.platform == "win32" else ""
    candidates = [
        # soldr / explicit --target put artifacts under target/<triple>/debug.
        ROOT
        / "target"
        / "x86_64-pc-windows-msvc"
        / "debug"
        / "examples"
        / f"scan_zombies{ext}",
        ROOT / "target" / "debug" / "examples" / f"scan_zombies{ext}",
    ]
    scan_bin = next((p for p in candidates if p.is_file()), None)
    if scan_bin is None:
        return []
    try:
        result = subprocess.run(
            [str(scan_bin)],
            capture_output=True,
            text=True,
            timeout=10,
        )
        orphans = []
        for line in result.stdout.splitlines():
            if "ORPHAN" in line:
                orphans.append({"line": line.strip()})
        return orphans
    except Exception:
        return []


@pytest.fixture(autouse=True)
def _check_no_zombies_after_test():
    """After each test, verify no orphaned CLUD processes were leaked."""
    yield
    import time

    time.sleep(0.2)  # brief settle time for process cleanup
    orphans = _scan_for_clud_zombies()
    if orphans:
        msg = "CLUD zombie processes detected after test:\n"
        for o in orphans:
            msg += f"  {o['line']}\n"
        pytest.fail(msg)
