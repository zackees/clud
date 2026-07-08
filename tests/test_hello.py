"""Verify core clud CLI behavior: --help, --version, --dry-run, pipe mode."""

from __future__ import annotations

import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Don't use tomllib: it's py311-only and `requires-python = ">=3.10"` promises
# py310 works. A regex is sufficient for the trivial `version = "x.y.z"` line
# in [project] and avoids the tomli backport dep.
_VERSION_RE = re.compile(r'^version\s*=\s*"([^"]+)"', re.MULTILINE)


def _project_version() -> str:
    text = (ROOT / "pyproject.toml").read_text(encoding="utf-8")
    match = _VERSION_RE.search(text)
    if match is None:
        raise RuntimeError("project.version missing from pyproject.toml")
    return match.group(1)


def _cargo_argv(subcommand: list[str]) -> list[str]:
    """Return the cargo argv, pinning the MSVC toolchain on Windows.

    Windows rustc installations from chocolatey ship a GNU-host rustc which
    links C/C++ deps against MinGW runtime DLLs (libstdc++-6.dll,
    libgcc_s_seh-1.dll, libwinpthread-1.dll). Those DLLs aren't on stock
    Windows, so a test that launches the resulting binary fails with
    STATUS_ENTRYPOINT_NOT_FOUND (0xC0000139). `ci.env.build_env()` forces the
    MSVC target via the rustup-managed toolchain. Mirrors
    `tests/integration/conftest.py::_cargo_argv`.
    """
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
    """Build the current repo's clud binary and return its path."""
    env_binary = os.environ.get("CLUD_TEST_BINARY")
    if env_binary and Path(env_binary).is_file():
        return env_binary

    result = subprocess.run(
        _cargo_argv(["build", "-p", "clud", "--message-format=json"]),
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=120,
    )
    if result.returncode != 0:
        raise RuntimeError(f"Failed to build clud:\n{result.stderr}")

    for line in result.stdout.splitlines():
        msg = json.loads(line)
        if (
            msg.get("reason") == "compiler-artifact"
            and msg.get("target", {}).get("name") == "clud"
            and msg.get("executable")
        ):
            return msg["executable"]

    ext = ".exe" if sys.platform == "win32" else ""
    # The MSVC-pinned env lands artifacts in a triple-qualified subdir; bare
    # cargo lands in target/debug. Check both.
    for fallback in (
        ROOT / "target" / "x86_64-pc-windows-msvc" / "debug" / f"clud{ext}",
        ROOT / "target" / "aarch64-pc-windows-msvc" / "debug" / f"clud{ext}",
        ROOT / "target" / "debug" / f"clud{ext}",
    ):
        if fallback.is_file():
            return str(fallback)
    raise RuntimeError("clud binary not found after build")


CLUD = _clud_binary()
PROJECT_VERSION = _project_version()


def copied_clud_env(_source: Path) -> dict[str, str]:
    """Return an environment that can launch a copied clud binary."""
    env = os.environ.copy()
    # Suppress unlock_exe()'s in-place rename trampoline when the
    # launched binary lives in a tmpdir (every _run() invocation).
    # Without this, the trampoline renames `<tmpdir>/clud.exe` to
    # `<tmpdir>/clud.exe.old.<rand>` and races against
    # `TemporaryDirectory.__exit__`'s shutil.rmtree on Windows —
    # SECTION release on process exit is async, so the tmpdir cleanup
    # gets WinError 32 on the still-locked `.old.<rand>` file. The
    # trampoline brings zero benefit here (the tmpdir binary is
    # never `pip install`'d at this path). See issues #331, #333.
    env["CLUD_NO_UNLOCK"] = "1"
    return env


def _copied_clud_tempdir() -> tempfile.TemporaryDirectory:
    """Return a tempdir safe for copied Windows executable launches."""
    return tempfile.TemporaryDirectory(ignore_cleanup_errors=sys.platform == "win32")


def _copy_clud_for_test(temp_dir: str) -> Path:
    source = Path(CLUD)
    launch = Path(temp_dir) / source.name
    shutil.copy2(source, launch)
    return launch


def _fake_claude_on_path(bin_dir: Path) -> None:
    bin_dir.mkdir(parents=True, exist_ok=True)
    if sys.platform == "win32":
        fake = bin_dir / "claude.cmd"
        fake.write_text("@echo off\r\nexit /b 0\r\n", encoding="utf-8")
    else:
        fake = bin_dir / "claude"
        fake.write_text("#!/usr/bin/env sh\nexit 0\n", encoding="utf-8")
        fake.chmod(0o755)


def _run(*args: str, input_data: str | None = None) -> subprocess.CompletedProcess[str]:
    with _copied_clud_tempdir() as temp_dir:
        source = Path(CLUD)
        launch = _copy_clud_for_test(temp_dir)
        return subprocess.run(
            [str(launch), *args],
            capture_output=True,
            text=True,
            timeout=10,
            input=input_data,
            env=copied_clud_env(source),
        )


def _isolated_clud_env(source: Path, home: Path, state_dir: Path) -> dict[str, str]:
    env = copied_clud_env(source)
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    env["LOCALAPPDATA"] = str(home / "local-app-data")
    env["XDG_STATE_HOME"] = str(home / ".local" / "state")
    env["XDG_CACHE_HOME"] = str(home / ".cache")
    env["CLUD_HOOK_HOME"] = str(home)
    env["CLUD_DAEMON_STATE_DIR"] = str(state_dir)
    env["CLUD_DATA_DB"] = str(state_dir / "data.redb")
    return env


def _shutdown_daemon(state_dir: Path) -> None:
    info_path = state_dir / "daemon.json"
    if not info_path.exists():
        return
    info = json.loads(info_path.read_text(encoding="utf-8"))
    pid = int(info.get("pid") or 0)
    try:
        with socket.create_connection(("127.0.0.1", int(info["port"])), timeout=2) as sock:
            sock.sendall(b'{"op":"shutdown"}\n')
            sock.recv(4096)
    except OSError:
        return
    deadline = time.monotonic() + 10
    while (
        (info_path.exists() or _pid_is_alive(pid))
        and time.monotonic() < deadline
    ):
        time.sleep(0.05)


def _pid_is_alive(pid: int) -> bool:
    if pid <= 0:
        return False
    if sys.platform == "win32":
        try:
            result = subprocess.run(
                ["tasklist", "/FI", f"PID eq {pid}", "/FO", "CSV", "/NH"],
                capture_output=True,
                text=True,
                timeout=5,
            )
        except subprocess.TimeoutExpired:
            return True
        return result.returncode == 0 and f'"{pid}"' in result.stdout
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _snapshot_tree(root: Path) -> dict[str, tuple[bool, int, int | None, bytes | None]]:
    snapshot: dict[str, tuple[bool, int, int | None, bytes | None]] = {}
    for path in sorted(root.rglob("*")):
        stat = path.stat()
        rel = path.relative_to(root).as_posix()
        if path.is_file():
            snapshot[rel] = (False, stat.st_mtime_ns, stat.st_size, path.read_bytes())
        else:
            snapshot[rel] = (True, stat.st_mtime_ns, None, None)
    return snapshot


def test_help() -> None:
    result = _run("--help")
    assert result.returncode == 0
    assert "YOLO" in result.stdout
    assert "--prompt" in result.stdout
    assert "--safe" in result.stdout
    assert "loop" in result.stdout


def test_version() -> None:
    result = _run("--version")
    assert result.returncode == 0
    assert result.stdout.strip() == f"clud {PROJECT_VERSION}"


def test_gc_bare_prints_help_without_touching_clud_dir() -> None:
    with _copied_clud_tempdir() as temp_dir:
        source = Path(CLUD)
        launch = _copy_clud_for_test(temp_dir)
        home = Path(temp_dir) / "home"
        state_dir = Path(temp_dir) / "daemon-state"
        clud_dir = home / ".clud"
        marker = clud_dir / "sentinel.txt"
        marker.parent.mkdir(parents=True)
        marker.write_text("do not touch\n", encoding="utf-8")
        before = _snapshot_tree(clud_dir)

        result = subprocess.run(
            [str(launch), "gc"],
            capture_output=True,
            text=True,
            timeout=10,
            env=_isolated_clud_env(source, home, state_dir),
        )

        assert result.returncode == 0, result.stderr
        assert "Commands:" in result.stdout or "SUBCOMMANDS:" in result.stdout
        assert "KINDS:" in result.stdout
        assert "uv-cache" in result.stdout
        assert "all" in result.stdout
        assert _snapshot_tree(clud_dir) == before


def test_gc_all_prunes_uv_cache_and_registered_trash() -> None:
    with _copied_clud_tempdir() as temp_dir:
        source = Path(CLUD)
        launch = _copy_clud_for_test(temp_dir)
        home = Path(temp_dir) / "home"
        state_dir = Path(temp_dir) / "daemon-state"
        env = _isolated_clud_env(source, home, state_dir)

        uv_env = home / ".clud" / "cache" / "uv" / "environments-v2" / "stale-env"
        uv_env.mkdir(parents=True)
        (uv_env / "pyvenv.cfg").write_text("stale\n", encoding="utf-8")
        old = time.time() - 8 * 24 * 60 * 60
        os.utime(uv_env, (old, old))

        victim = home / "victim.txt"
        victim.parent.mkdir(parents=True, exist_ok=True)
        victim.write_text("trash me\n", encoding="utf-8")
        trash = subprocess.run(
            [str(launch), "trash", "--cross-volume", str(victim)],
            capture_output=True,
            text=True,
            timeout=20,
            env=env,
        )
        try:
            assert trash.returncode == 0, trash.stderr
            assert not victim.exists()
            trash_root = home / ".clud" / "trash"
            assert any(trash_root.iterdir())

            result = subprocess.run(
                [str(launch), "gc", "all"],
                capture_output=True,
                text=True,
                timeout=30,
                env=env,
            )

            assert result.returncode == 0, result.stderr
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                trash_empty = not trash_root.exists() or not any(trash_root.iterdir())
                if not uv_env.exists() and trash_empty:
                    break
                time.sleep(0.1)
            assert not uv_env.exists(), result.stdout
            assert not trash_root.exists() or not any(trash_root.iterdir()), result.stdout
        finally:
            _shutdown_daemon(state_dir)


def test_top_once_json_arg_surface() -> None:
    with _copied_clud_tempdir() as temp_dir:
        source = Path(CLUD)
        launch = _copy_clud_for_test(temp_dir)
        home = Path(temp_dir) / "home"
        state_dir = Path(temp_dir) / "daemon-state"
        env = _isolated_clud_env(source, home, state_dir)
        try:
            result = subprocess.run(
                [
                    str(launch),
                    "top",
                    "--once",
                    "--json",
                    "--flat",
                    "--sort",
                    "rss",
                    "--limit",
                    "5",
                    "--since",
                    "5s",
                    "--originator",
                    "CLUD:0",
                ],
                capture_output=True,
                text=True,
                timeout=20,
                env=env,
            )

            assert result.returncode == 0, result.stderr
            data = json.loads(result.stdout)
            assert data["schema_version"] == 1
            assert data["interval_ms"] >= 250
            assert isinstance(data["rows"], list)
            assert "summary" in data
        finally:
            _shutdown_daemon(state_dir)


def test_linux_binary_does_not_require_libasound_at_startup() -> None:
    if not sys.platform.startswith("linux"):
        return
    ldd = shutil.which("ldd")
    if ldd is None:
        return

    result = subprocess.run(
        [ldd, CLUD],
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert result.returncode == 0, result.stderr
    assert "libasound" not in (result.stdout + result.stderr).lower()


def test_dry_run_prompt() -> None:
    result = _run("--dry-run", "-p", "hello")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert data["backend"] == "claude"
    assert data["launch_mode"] == "subprocess"
    assert "--dangerously-skip-permissions" in data["command"]
    assert "-p" in data["command"]
    assert "hello" in data["command"]
    assert data["iterations"] == 1


def test_dry_run_codex() -> None:
    result = _run("--dry-run", "--codex", "-p", "hello")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert data["backend"] == "codex"
    assert data["launch_mode"] == "subprocess"


def test_dry_run_codex_reports_project_doc_fallback(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    home = tmp_path / "home"
    repo.mkdir()
    home.mkdir()
    (repo / "CODEX.md").write_text("codex fallback", encoding="utf-8")

    with _copied_clud_tempdir() as temp_dir:
        source = Path(CLUD)
        launch = _copy_clud_for_test(temp_dir)
        env = copied_clud_env(source)
        env["HOME"] = str(home)
        env["USERPROFILE"] = str(home)
        env["CLUD_HOOK_HOME"] = str(home)
        result = subprocess.run(
            [str(launch), "--dry-run", "--codex", "-p", "hello"],
            cwd=repo,
            env=env,
            capture_output=True,
            text=True,
            timeout=10,
        )

    assert result.returncode == 0, result.stderr
    data = json.loads(result.stdout)
    assert (
        'project_doc_fallback_filenames=["CODEX.md"]'
        in data["command"]
    )


def test_dry_run_pty_override() -> None:
    result = _run("--dry-run", "--pty", "-p", "hello")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert data["backend"] == "claude"
    assert data["launch_mode"] == "pty"


def test_dry_run_safe_mode() -> None:
    result = _run("--dry-run", "--safe", "-p", "hello")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert "--dangerously-skip-permissions" not in data["command"]


def test_dry_run_model() -> None:
    result = _run("--dry-run", "--model", "opus", "-p", "hello")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert "--model" in data["command"]
    assert "opus" in data["command"]


def test_dry_run_continue() -> None:
    result = _run("--dry-run", "-c")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert "--continue" in data["command"]


def test_dry_run_message() -> None:
    result = _run("--dry-run", "-m", "fix bug")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert "-m" in data["command"]
    assert "fix bug" in data["command"]


def test_dry_run_up() -> None:
    result = _run("--dry-run", "up")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    prompt = data["command"][-1]
    assert "lint" in prompt.lower()
    assert "codeup" in prompt.lower()


def test_dry_run_up_with_message() -> None:
    result = _run("--dry-run", "up", "-m", "bump version")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    prompt = data["command"][-1]
    assert 'codeup -m "bump version"' in prompt


def test_dry_run_up_with_publish() -> None:
    result = _run("--dry-run", "up", "--publish")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    prompt = data["command"][-1]
    assert "-p" in prompt.split("codeup")[1]


def test_dry_run_rebase() -> None:
    result = _run("--dry-run", "rebase")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    prompt = data["command"][-1]
    assert "git fetch" in prompt
    assert "rebase" in prompt.lower()


def test_dry_run_fix() -> None:
    result = _run("--dry-run", "fix")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    prompt = data["command"][-1].lower()
    assert "linting" in prompt
    assert "unit tests" in prompt


def test_dry_run_fix_with_url() -> None:
    result = _run("--dry-run", "fix", "https://github.com/user/repo/actions/runs/123")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    prompt = data["command"][-1]
    assert "github.com/user/repo/actions/runs/123" in prompt
    assert "gh run view" in prompt


def test_dry_run_loop() -> None:
    result = _run("--dry-run", "loop", "--loop-count", "5", "do stuff")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert data["iterations"] == 5
    # Prompt is the last arg (Claude's `-p <prompt>`), with the DONE-marker
    # contract appended. Original task is preserved at the start.
    prompt = data["command"][-1]
    assert prompt.startswith("do stuff")
    # Issue #95: contract now embeds the absolute marker path; the relative
    # suffix is still present with platform-native separators.
    normalized_prompt = prompt.replace("\\", "/")
    assert ".clud/loop/DONE" in normalized_prompt
    assert ".clud/loop/BLOCKED" in normalized_prompt
    assert data["loop_markers"] is not None
    assert data["loop_markers"]["done_path"].replace("\\", "/").endswith(".clud/loop/DONE")
    assert data["loop_markers"]["blocked_path"].replace("\\", "/").endswith(
        ".clud/loop/BLOCKED"
    )


def test_dry_run_loop_no_done() -> None:
    result = _run("--dry-run", "loop", "--no-done", "do stuff")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert data["command"][-1] == "do stuff"
    assert data["loop_markers"] is None


def test_dry_run_loop_repeat_implies_no_done() -> None:
    result = _run("--dry-run", "loop", "--repeat", "1h", "do stuff")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert data["command"][-1] == "do stuff"
    assert data["loop_markers"] is None
    assert data["repeat_interval_secs"] == 3600


def test_dry_run_loop_repeat_emits_warning_to_stderr() -> None:
    # Issue #61 acceptance: when `--repeat` is supplied without `--done`, the
    # CLI must tell the user it's auto-disabling DONE-marker injection.
    result = _run("--dry-run", "loop", "--repeat", "1h", "do stuff")
    assert result.returncode == 0
    assert "--repeat" in result.stderr
    assert "--no-done" in result.stderr
    assert "DONE marker" in result.stderr


def test_dry_run_loop_repeat_with_done_path_no_warning() -> None:
    # Issue #61: --done <path> overrides the implicit --no-done; no warning.
    result = _run("--dry-run", "loop", "--repeat", "1h", "--done", "DONE.md", "do stuff")
    assert result.returncode == 0
    assert "implies `--no-done`" not in result.stderr


def test_dry_run_loop_repeat_with_explicit_no_done_no_warning() -> None:
    # Issue #61: explicit --no-done already opted out; don't badger the user.
    result = _run("--dry-run", "loop", "--repeat", "1h", "--no-done", "do stuff")
    assert result.returncode == 0
    assert "implies `--no-done`" not in result.stderr


def test_dry_run_loop_no_repeat_no_warning() -> None:
    # Issue #61: plain `clud loop` without --repeat must not emit the warning.
    result = _run("--dry-run", "loop", "do stuff")
    assert result.returncode == 0
    assert "implies `--no-done`" not in result.stderr


def test_dry_run_loop_repeat_with_done_override() -> None:
    result = _run("--dry-run", "loop", "--repeat", "1h", "--done", "DONE.md", "do stuff")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    prompt = data["command"][-1]
    assert "DONE.md" in prompt
    assert "BLOCKED.md" in prompt
    assert data["loop_markers"]["done_path"].replace("\\", "/").endswith("DONE.md")
    assert data["loop_markers"]["blocked_path"].replace("\\", "/").endswith("BLOCKED.md")


def test_dry_run_loop_repeat_invalid_duration_errors() -> None:
    # Issue #61: bogus duration values must fail with a clear error and a
    # non-zero exit code, not crash or silently succeed.
    result = _run("--dry-run", "loop", "--repeat", "30d", "do stuff")
    assert result.returncode != 0
    assert "invalid --repeat" in result.stderr or "unsupported" in result.stderr.lower()


def test_dry_run_loop_default_count() -> None:
    result = _run("--dry-run", "loop", "task")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert data["iterations"] == 50


def test_dry_run_passthrough_flags() -> None:
    result = _run("--dry-run", "--unknown-flag", "-p", "hello")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert "--unknown-flag" in data["command"]


def test_pipe_mode() -> None:
    result = _run("--dry-run", input_data="piped prompt")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert "-p" in data["command"]
    assert "piped prompt" in data["command"]


def test_startup_refreshes_stale_managed_bundled_tool(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    home = tmp_path / "home"
    fake_bin = tmp_path / "bin"
    repo.mkdir()
    home.mkdir()
    _fake_claude_on_path(fake_bin)

    hook = home / ".clud" / "tools" / "hooks" / "block-bad-cmd.py"
    hook.parent.mkdir(parents=True)
    hook.write_text("# managed-by: clud\nprint('stale hook')\n", encoding="utf-8")

    with _copied_clud_tempdir() as temp_dir:
        source = Path(CLUD)
        launch = _copy_clud_for_test(temp_dir)
        env = copied_clud_env(source)
        env["HOME"] = str(home)
        env["USERPROFILE"] = str(home)
        env["CLUD_HOOK_HOME"] = str(home)
        env["PATH"] = str(fake_bin) + os.pathsep + env.get("PATH", "")
        result = subprocess.run(
            [
                str(launch),
                "--no-daemon",
                "--no-fix-hooks",
                "--no-cpu-banner",
                "--no-dnd",
                "--subprocess",
                "-p",
                "hello",
            ],
            cwd=repo,
            env=env,
            capture_output=True,
            text=True,
            timeout=15,
        )

    assert result.returncode == 0, (
        f"startup launch failed (rc={result.returncode}): "
        f"stdout={result.stdout!r} stderr={result.stderr!r}"
    )
    updated = hook.read_text(encoding="utf-8")
    assert "clud-block-bad-cmd" in updated
    assert "compatibility shim" in updated.lower()
    assert "stale hook" not in updated


def test_dry_run_does_not_refresh_stale_managed_bundled_tool(tmp_path: Path) -> None:
    home = tmp_path / "home"
    home.mkdir()
    hook = home / ".clud" / "tools" / "hooks" / "block-bad-cmd.py"
    hook.parent.mkdir(parents=True)
    stale = "# managed-by: clud\nprint('stale hook')\n"
    hook.write_text(stale, encoding="utf-8")

    with _copied_clud_tempdir() as temp_dir:
        source = Path(CLUD)
        launch = _copy_clud_for_test(temp_dir)
        env = copied_clud_env(source)
        env["HOME"] = str(home)
        env["USERPROFILE"] = str(home)
        env["CLUD_HOOK_HOME"] = str(home)
        result = subprocess.run(
            [str(launch), "--dry-run", "-p", "hello"],
            env=env,
            capture_output=True,
            text=True,
            timeout=10,
        )

    assert result.returncode == 0, result.stderr
    assert hook.read_text(encoding="utf-8") == stale


def test_clean_worktrees_dry_run_smoke() -> None:
    """Issue #83: `clud --clean-worktrees --dry-run` must enumerate worktrees
    without crashing and without removing anything. We don't assert on the
    exact set of worktrees (the host running the test may have any number),
    only that the binary returns successfully and prints the dry-run banner.
    The binary itself lives inside a git repo (this one), so the worktree
    list is non-empty in practice — but even on a fresh clone with zero
    extra worktrees the command must succeed.
    """
    result = _run("--clean-worktrees", "--dry-run", "--yes")
    assert result.returncode == 0, (
        f"--clean-worktrees --dry-run failed (rc={result.returncode}): "
        f"stdout={result.stdout!r} stderr={result.stderr!r}"
    )
    # Must mention worktrees in the output — sanity check that we ran the
    # worktrees code path and not e.g. the launcher path.
    combined = result.stdout + result.stderr
    assert "Worktrees" in combined or "worktree" in combined.lower()


def test_clean_worktrees_rejects_invalid_stale_after() -> None:
    """Bogus `--stale-after` values must fail with a clear error before any
    git invocation happens."""
    result = _run("--clean-worktrees", "--stale-after", "30x", "--dry-run")
    assert result.returncode != 0
    assert "invalid --stale-after" in result.stderr or "unsupported" in result.stderr.lower()


def test_fix_hooks_dry_run_plans_without_writing(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    home = tmp_path / "home"
    (repo / ".git").mkdir(parents=True)
    (repo / ".claude").mkdir()
    (repo / ".claude" / "settings.json").write_text(
        json.dumps(
            {
                "hooks": {
                    "PreToolUse": [
                        {
                            "matcher": "Bash",
                            "hooks": [{"type": "command", "command": "python check.py"}],
                        }
                    ]
                }
            }
        ),
        encoding="utf-8",
    )
    home.mkdir()

    with _copied_clud_tempdir() as temp_dir:
        source = Path(CLUD)
        launch = Path(temp_dir) / source.name
        shutil.copy2(source, launch)
        env = copied_clud_env(source)
        env["HOME"] = str(home)
        env["USERPROFILE"] = str(home)
        env["CLUD_HOOK_HOME"] = str(home)
        result = subprocess.run(
            [str(launch), "--dry-run", "--fix-hooks"],
            cwd=repo,
            env=env,
            capture_output=True,
            text=True,
            timeout=10,
        )

    assert result.returncode == 0
    assert "hook health dry-run" in result.stdout
    assert "Claude PreToolUse hooks exist" in result.stdout
    assert "claude->codex" in result.stdout
    assert "matcher `Bash`" in result.stdout
    assert "add Codex project trust" not in result.stdout
    assert not (home / ".codex" / "config.toml").exists()
