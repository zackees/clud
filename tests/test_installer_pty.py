"""Exercise the compiled installer selector through a real POSIX PTY."""

from __future__ import annotations

import errno
import os
import select
import shlex
import sys
import time
from pathlib import Path

import pytest

from tests.process import run as run_process

if sys.platform != "win32":
    import pty
    import signal
    import termios


def installer_executable() -> Path:
    executable = Path(os.environ.get("CLUD_INSTALLER_EXE", "clud-installer.exe")).resolve()
    if not executable.is_file():
        pytest.skip("build installer/clud-installer.exe before running PTY acceptance")
    return executable


def home_snapshot(home: Path | None) -> tuple[tuple[str, bool, int, bytes], ...]:
    if home is None or not home.exists():
        return ()
    snapshot = []
    for path in sorted(home.rglob("*")):
        info = path.lstat()
        is_directory = path.is_dir()
        content = path.read_bytes() if path.is_file() else b""
        snapshot.append((str(path.relative_to(home)), is_directory, info.st_mode, content))
    return tuple(snapshot)


def available_versions(executable: Path) -> list[str]:
    if sys.platform == "win32":
        command = [str(executable), "--list"]
    else:
        command = ["/bin/bash", "-lc", f"{shlex.quote(str(executable))} --list"]
    result = run_process(command, capture_output=True, text=True, timeout=90, check=True)
    versions = []
    for line in result.stdout.splitlines():
        if line.startswith("[*] "):
            versions.append(line.removeprefix("[*] "))
        elif line.startswith("[ ] "):
            versions.append(line.removeprefix("[ ] "))
    return versions


def run_selector(
    input_bytes: bytes, *, preloaded: bytes = b"", home: Path | None = None
) -> tuple[bytes, int, list, list | None, bool]:
    executable = installer_executable()
    pid, master = pty.fork()
    if pid == 0:
        env = os.environ.copy()
        env.pop("BASH_ENV", None)
        if home is not None:
            home.mkdir(parents=True, exist_ok=True)
            env["HOME"] = str(home)
        if preloaded:
            time.sleep(0.2)
        os.execve("/bin/bash", ["/bin/bash", "-c", str(executable)], env)

    original = termios.tcgetattr(master)
    output = bytearray()
    sent = False
    confirmation_sent = False
    cutoff = time.monotonic() + 30
    status = None
    menu_seen_at = None
    before_input = None
    restored = None
    try:
        if preloaded:
            os.write(master, preloaded)
        while time.monotonic() < cutoff:
            ready, _, _ = select.select([master], [], [], 0.05)
            if ready:
                try:
                    chunk = os.read(master, 4096)
                except OSError as error:
                    if error.errno == errno.EIO:
                        break
                    raise
                if not chunk:
                    break
                output.extend(chunk)
            if b"Choose a clud version" in output and menu_seen_at is None:
                menu_seen_at = time.monotonic()
                before_input = home_snapshot(home)
            if menu_seen_at is not None and not sent:
                if preloaded and time.monotonic() - menu_seen_at < 0.35:
                    pass
                else:
                    os.write(master, input_bytes)
                    sent = True
            if b"Proceed? [y/N]" in output and not confirmation_sent:
                os.write(master, b"n\r")
                confirmation_sent = True
            if b"Selected clud " in output or b"Installation cancelled" in output:
                break
            waited, child_status = os.waitpid(pid, os.WNOHANG)
            if waited:
                status = child_status
                break
        if status is None:
            _, status = os.waitpid(pid, 0)
    finally:
        try:
            restored = termios.tcgetattr(master)
        except OSError:
            pass
        try:
            os.close(master)
        except OSError:
            pass
        if status is None:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
    unchanged = before_input == home_snapshot(home)
    return bytes(output), status, original, restored, unchanged


@pytest.mark.skipif(sys.platform == "win32", reason="requires a POSIX PTY")
def test_menu_scrolls_and_restores_terminal_with_crlf_output(tmp_path: Path) -> None:
    executable = installer_executable()
    versions = available_versions(executable)
    assert len(versions) > 8, "the live version catalog must contain scrollable history"
    selected_version = versions[8]
    output, status, original, restored, home_unchanged = run_selector(
        b"\x1b[B" * 8 + b"\r", preloaded=b"\r", home=tmp_path / "home"
    )
    assert os.waitstatus_to_exitcode(status) != 0
    assert f"Install clud {selected_version} for this user?".encode() in output
    assert f"[*] latest ({versions[0]})".encode() in output
    assert b"... 1 more above" in output
    assert b"Installation cancelled; no files were changed." in output
    assert all(
        index > 0 and output[index - 1] == 13
        for index, byte in enumerate(output)
        if byte == 10
    )
    assert restored == original, "terminal mode was not restored after selection"
    assert home_unchanged, "declining consent modified HOME after startup"


@pytest.mark.skipif(sys.platform != "win32", reason="requires Windows ConPTY")
def test_menu_scrolls_and_restores_terminal_through_windows_conpty() -> None:
    executable = installer_executable()
    from winpty import PtyProcess

    versions = available_versions(executable)
    assert len(versions) > 8, "the live version catalog must contain scrollable history"
    selected_version = versions[8]
    process = PtyProcess.spawn([str(executable)], dimensions=(32, 100))
    output = ""
    selection_sent = False
    confirmation_sent = False
    deadline = time.monotonic() + 60
    try:
        while time.monotonic() < deadline:
            ready, _, _ = select.select([process.fileobj], [], [], 0.1)
            if ready:
                try:
                    output += process.read(4096)
                except EOFError:
                    break
            if "Choose a clud version" in output and not selection_sent:
                process.write("\x1b[B" * 8 + "\r")
                selection_sent = True
            if "Proceed? [y/N]" in output and not confirmation_sent:
                process.write("n\r")
                confirmation_sent = True
            if "Installation cancelled" in output:
                break
        assert selection_sent, f"ConPTY selector never displayed its menu:\n{output}"
        assert confirmation_sent, f"ConPTY selector did not reach consent:\n{output}"
        assert f"Install clud {selected_version} for this user?" in output
        assert f"[*] latest ({versions[0]})" in output
        assert "... 1 more above" in output
        assert "Installation cancelled; no files were changed." in output
        assert all(
            index > 0 and output[index - 1] == "\r"
            for index, char in enumerate(output)
            if char == "\n"
        ), "ConPTY output contained a bare LF or doubled CR"
    finally:
        process.close(force=True)


@pytest.mark.skipif(sys.platform == "win32", reason="requires a POSIX PTY")
def test_escape_cancels_without_selecting_a_release(tmp_path: Path) -> None:
    home = tmp_path / "home"
    output, status, original, restored, home_unchanged = run_selector(b"\x1b", home=home)
    assert os.waitstatus_to_exitcode(status) != 0
    assert b"Installation cancelled" in output
    assert restored == original, "terminal mode was not restored after Escape"
    assert home_unchanged, "Escape cancellation modified HOME after startup"
