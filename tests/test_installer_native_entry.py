"""The native clud installer entry must gate writes and restore its terminal."""

from __future__ import annotations

import errno
import hashlib
import os
import select
import sys
import time
from pathlib import Path

import pytest

from tests.process import run as run_process

if sys.platform != "win32":
    import pty
    import termios


def clud_binary() -> Path:
    candidate = os.environ.get("CLUD_TEST_BINARY")
    if not candidate:
        pytest.skip("compiled clud bundle is required")
    path = Path(candidate)
    if not path.is_file():
        pytest.skip("compiled clud bundle is unavailable")
    return path


def isolated_env(home: Path) -> dict[str, str]:
    env = os.environ.copy()
    env["HOME"] = str(home)
    if sys.platform == "win32":
        env["USERPROFILE"] = str(home)
        env.pop("LOCALAPPDATA", None)
    return env


def assert_crlf(output: bytes) -> None:
    for index, byte in enumerate(output):
        if byte == 10:
            assert index > 0, "selector emitted a bare LF"
            assert output[index - 1] == 13, "selector emitted a bare LF"


@pytest.mark.parametrize(
    ("flags", "expected"),
    [
        (["--installer"], 2),
        (["--installer", "--install-current"], 2),
        (["--installer", "--install-version", "2.8.14"], 2),
    ],
)
def test_no_tty_never_writes(tmp_path: Path, flags: list[str], expected: int) -> None:
    home = tmp_path / "home"
    result = run_process(
        [str(clud_binary()), *flags],
        env=isolated_env(home),
        input=b"",
        capture_output=True,
        timeout=15,
        check=False,
    )
    assert result.returncode == expected, result.stderr.decode(errors="replace")
    assert not home.exists(), "installer entry wrote to the user home"


def test_explicit_current_installs_verified_copy_offline(tmp_path: Path) -> None:
    home = tmp_path / "home"
    env = isolated_env(home)
    env["HTTPS_PROXY"] = "http://127.0.0.1:1"
    env["HTTP_PROXY"] = "http://127.0.0.1:1"
    if sys.platform == "win32":
        env["LOCALAPPDATA"] = str(home / "AppData" / "Local")
        destination = (
            home / "AppData" / "Local" / "Programs" / "clud" / "bin" / "clud.exe"
        )
    else:
        destination = home / ".local" / "bin" / "clud"
    result = run_process(
        [str(clud_binary()), "--installer", "--install-current", "--yes"],
        env=env,
        capture_output=True,
        timeout=30,
        check=False,
    )
    assert result.returncode == 1, result.stderr.decode(errors="replace")
    assert destination.is_file(), result.stderr.decode(errors="replace")
    assert hashlib.sha256(destination.read_bytes()).digest() == hashlib.sha256(
        clud_binary().read_bytes()
    ).digest()
    repeat = run_process(
        [str(clud_binary()), "--installer", "--install-current", "--yes"],
        env=env,
        capture_output=True,
        timeout=30,
        check=False,
    )
    assert repeat.returncode == 1, repeat.stderr.decode(errors="replace")
    assert destination.read_bytes() == clud_binary().read_bytes()
    assert not list(destination.parent.glob(".clud-install-*"))
    assert not destination.with_suffix(".clud-backup").exists()


def test_interrupted_commit_restores_backup_before_reinstall(tmp_path: Path) -> None:
    home = tmp_path / "home"
    env = isolated_env(home)
    if sys.platform == "win32":
        env["LOCALAPPDATA"] = str(home / "AppData" / "Local")
        destination = home / "AppData" / "Local" / "Programs" / "clud" / "bin" / "clud.exe"
    else:
        destination = home / ".local" / "bin" / "clud"
    command = [str(clud_binary()), "--installer", "--install-current", "--yes"]
    first = run_process(command, env=env, capture_output=True, timeout=30, check=False)
    assert first.returncode == 1, first.stderr.decode(errors="replace")
    backup = destination.with_suffix(".clud-backup")
    backup.write_bytes(destination.read_bytes())
    backup.chmod(destination.stat().st_mode)
    destination.write_bytes(b"interrupted commit")
    recovered = run_process(command, env=env, capture_output=True, timeout=30, check=False)
    assert recovered.returncode == 1, recovered.stderr.decode(errors="replace")
    assert destination.read_bytes() == clud_binary().read_bytes()
    assert not backup.exists()


def test_exact_older_release_downloads_selected_executable(tmp_path: Path) -> None:
    home = tmp_path / "home"
    env = isolated_env(home)
    if sys.platform == "win32":
        env["LOCALAPPDATA"] = str(home / "AppData" / "Local")
        destination = (
            home / "AppData" / "Local" / "Programs" / "clud" / "bin" / "clud.exe"
        )
    else:
        destination = home / ".local" / "bin" / "clud"
    result = run_process(
        [str(clud_binary()), "--installer", "--install-version", "2.8.13", "--yes"],
        env=env,
        capture_output=True,
        timeout=120,
        check=False,
    )
    assert result.returncode == 1, result.stderr.decode(errors="replace")
    assert destination.is_file(), result.stderr.decode(errors="replace")
    selected = run_process(
        [str(destination), "--version"],
        env=env,
        capture_output=True,
        timeout=15,
        check=False,
    )
    assert selected.returncode == 0, selected.stderr.decode(errors="replace")
    assert selected.stdout.strip() == b"clud 2.8.13"


@pytest.mark.skipif(sys.platform != "win32", reason="requires a running Windows executable")
def test_running_windows_target_remains_valid(tmp_path: Path) -> None:
    home = tmp_path / "home"
    env = isolated_env(home)
    env["LOCALAPPDATA"] = str(home / "AppData" / "Local")
    destination = home / "AppData" / "Local" / "Programs" / "clud" / "bin" / "clud.exe"
    first = run_process(
        [str(clud_binary()), "--installer", "--install-current", "--yes"],
        env=env,
        capture_output=True,
        timeout=30,
        check=False,
    )
    assert destination.is_file(), first.stderr.decode(errors="replace")
    before = hashlib.sha256(destination.read_bytes()).digest()
    running = run_process(
        [str(destination), "--installer", "--install-current", "--yes"],
        env=env,
        capture_output=True,
        timeout=30,
        check=False,
    )
    assert running.returncode == 1, running.stderr.decode(errors="replace")
    assert destination.is_file()
    assert hashlib.sha256(destination.read_bytes()).digest() == before
    assert not destination.with_suffix(".clud-backup").exists()


@pytest.mark.skipif(sys.platform == "win32", reason="requires a POSIX PTY")
def test_explicit_menu_escape_restores_posix_terminal(tmp_path: Path) -> None:
    home = tmp_path / "home"
    pid, master = pty.fork()
    if pid == 0:
        binary = str(clud_binary())
        os.execve(binary, [binary, "--installer"], isolated_env(home))
    original = termios.tcgetattr(master)
    output = bytearray()
    sent = False
    status = None
    deadline = time.monotonic() + 15
    try:
        while time.monotonic() < deadline:
            readable, _, _ = select.select([master], [], [], 0.05)
            if readable:
                try:
                    output.extend(os.read(master, 4096))
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
            if b"Install clud" in output and not sent:
                os.write(master, b"\x1b")
                sent = True
            waited, child_status = os.waitpid(pid, os.WNOHANG)
            if waited:
                status = child_status
                break
        assert sent, output.decode(errors="replace")
        assert status is not None, output.decode(errors="replace")
        assert os.waitstatus_to_exitcode(status) == 130, output.decode(errors="replace")
        assert termios.tcgetattr(master) == original
        assert_crlf(output)
        assert not home.exists()
    finally:
        if status is None:
            os.kill(pid, 9)
            os.waitpid(pid, 0)
        os.close(master)


@pytest.mark.skipif(sys.platform != "win32", reason="requires Windows ConPTY")
def test_explicit_menu_escape_through_conpty(tmp_path: Path) -> None:
    from winpty import PtyProcess

    home = tmp_path / "home"
    process = PtyProcess.spawn(
        [str(clud_binary()), "--installer"],
        env=isolated_env(home),
        dimensions=(24, 80),
    )
    output = ""
    sent = False
    deadline = time.monotonic() + 15
    try:
        while time.monotonic() < deadline:
            readable, _, _ = select.select([process.fileobj], [], [], 0.05)
            if readable:
                try:
                    output += process.read(4096)
                except EOFError:
                    break
            if "Install clud" in output and not sent:
                process.write("\x1b")
                sent = True
            if not process.isalive():
                break
        assert sent, output
        assert process.exitstatus == 130, output
        assert_crlf(output.encode())
        assert not home.exists()
    finally:
        process.close(force=True)
