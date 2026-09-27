"""The native clud installer entry must gate writes and restore its terminal."""

from __future__ import annotations

import errno
import hashlib
import os
import select
import shutil
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


@pytest.fixture
def installer_target(tmp_path: Path):
    if sys.platform != "win32":
        home = tmp_path / "home"
        home.mkdir(mode=0o700)
        env = isolated_env(home)
        env["SHELL"] = shutil.which("bash") or "/bin/bash"
        yield env, home / ".local" / "bin" / "clud"
        return

    if not (
        os.environ.get("GITHUB_ACTIONS") == "true"
        and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted"
    ):
        pytest.skip("Windows User environment test requires a GitHub-hosted runner")
    import winreg

    local = os.environ.get("LOCALAPPDATA")
    if not local:
        pytest.skip("Windows User environment lacks LOCALAPPDATA")
    destination = Path(local) / "Programs" / "clud" / "bin" / "clud.exe"
    programs = destination.parents[2]
    install_tree = destination.parents[1]
    programs_existed = programs.exists()
    if install_tree.exists():
        pytest.skip("Windows User profile already has a clud install tree")
    key = winreg.OpenKey(
        winreg.HKEY_CURRENT_USER,
        "Environment",
        0,
        winreg.KEY_READ | winreg.KEY_WRITE,
    )
    try:
        prior = winreg.QueryValueEx(key, "Path")
    except FileNotFoundError:
        prior = None
    try:
        yield os.environ.copy(), destination
    finally:
        if prior is None:
            try:
                winreg.DeleteValue(key, "Path")
            except FileNotFoundError:
                pass
        else:
            winreg.SetValueEx(key, "Path", 0, prior[1], prior[0])
        key.Close()
        if not programs_existed and programs.exists():
            programs.replace(tmp_path / "installed-programs-tree")
        elif install_tree.exists():
            install_tree.replace(tmp_path / "installed-clud-tree")


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


def test_no_tty_refusal_preserves_existing_startup_file(tmp_path: Path) -> None:
    home = tmp_path / "home"
    home.mkdir()
    profile = home / ".bashrc"
    profile.write_text("# user settings\n")
    env = isolated_env(home)
    result = run_process(
        [str(clud_binary()), "--installer", "--install-current"],
        env=env,
        capture_output=True,
        timeout=15,
        check=False,
    )
    assert result.returncode == 2, result.stderr.decode(errors="replace")
    assert profile.read_text() == "# user settings\n"
    assert not (home / ".local").exists()


@pytest.mark.skipif(sys.platform != "win32", reason="requires Windows User registry")
def test_no_tty_refusal_preserves_windows_user_path() -> None:
    import winreg

    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, "Environment") as key:
        try:
            before = winreg.QueryValueEx(key, "Path")
        except FileNotFoundError:
            before = None
        result = run_process(
            [str(clud_binary()), "--installer", "--install-current"],
            env=os.environ.copy(),
            capture_output=True,
            timeout=15,
            check=False,
        )
        assert result.returncode == 2, result.stderr.decode(errors="replace")
        try:
            after = winreg.QueryValueEx(key, "Path")
        except FileNotFoundError:
            after = None
        assert after == before


def test_explicit_current_installs_verified_copy_offline(installer_target) -> None:
    env, destination = installer_target
    env["HTTPS_PROXY"] = "http://127.0.0.1:1"
    env["HTTP_PROXY"] = "http://127.0.0.1:1"
    result = run_process(
        [str(clud_binary()), "--installer", "--install-current", "--yes"],
        env=env,
        capture_output=True,
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, result.stderr.decode(errors="replace")
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
    assert repeat.returncode == 0, repeat.stderr.decode(errors="replace")
    assert destination.read_bytes() == clud_binary().read_bytes()
    assert not list(destination.parent.glob(".clud-install-*"))
    assert not destination.with_suffix(".clud-backup").exists()
    if sys.platform != "win32":
        for profile in (
            destination.parents[2] / ".bash_profile",
            destination.parents[2] / ".bashrc",
        ):
            assert profile.read_text().count("# >>> clud installer PATH >>>") == 1


def test_interrupted_commit_restores_backup_before_reinstall(installer_target) -> None:
    env, destination = installer_target
    command = [str(clud_binary()), "--installer", "--install-current", "--yes"]
    first = run_process(command, env=env, capture_output=True, timeout=30, check=False)
    assert first.returncode == 0, first.stderr.decode(errors="replace")
    backup = destination.with_suffix(".clud-backup")
    backup.write_bytes(destination.read_bytes())
    backup.chmod(destination.stat().st_mode)
    destination.write_bytes(b"interrupted commit")
    recovered = run_process(command, env=env, capture_output=True, timeout=30, check=False)
    assert recovered.returncode == 0, recovered.stderr.decode(errors="replace")
    assert destination.read_bytes() == clud_binary().read_bytes()
    assert not backup.exists()


@pytest.mark.skipif(sys.platform == "win32", reason="requires POSIX bash startup files")
def test_bash_fresh_login_and_interactive_lookup(tmp_path: Path) -> None:
    shell = shutil.which("bash")
    if shell is None:
        pytest.skip("bash is unavailable")
    home = tmp_path / "home with spaces and 'quotes' $dollars"
    home.mkdir(mode=0o700)
    env = isolated_env(home)
    env["SHELL"] = shell
    env["PATH"] = "/usr/bin:/bin"
    installed = home / ".local" / "bin" / "clud"
    source_version = run_process(
        [str(clud_binary()), "--version"],
        env=env,
        capture_output=True,
        timeout=15,
        check=True,
    ).stdout.strip()
    result = run_process(
        [str(clud_binary()), "--installer", "--install-current", "--yes"],
        env=env,
        capture_output=True,
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, result.stderr.decode(errors="replace")
    for mode in ("-lc", "-ic", "-lic"):
        fresh = run_process(
            [shell, mode, "command -v clud; clud --version"],
            env=env,
            capture_output=True,
            timeout=15,
            check=False,
        )
        assert fresh.returncode == 0, fresh.stderr.decode(errors="replace")
        assert fresh.stdout.splitlines() == [
            os.fsencode(installed),
            source_version,
        ]


@pytest.mark.skipif(sys.platform == "win32", reason="requires POSIX bash startup files")
def test_existing_user_bin_and_first_bash_login_file(tmp_path: Path) -> None:
    shell = shutil.which("bash")
    if shell is None:
        pytest.skip("bash is unavailable")
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    bin_dir = home / "bin"
    bin_dir.mkdir(mode=0o700)
    login = home / ".bash_login"
    login.write_text("# existing login settings\n")
    login.chmod(0o600)
    env = isolated_env(home)
    env["SHELL"] = shell
    env["PATH"] = f"{bin_dir}:/usr/bin:/bin"
    result = run_process(
        [str(clud_binary()), "--installer", "--install-current", "--yes"],
        env=env,
        capture_output=True,
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, result.stderr.decode(errors="replace")
    assert (bin_dir / "clud").is_file()
    assert login.read_text().startswith("# existing login settings\n")
    assert login.read_text().count("# >>> clud installer PATH >>>") == 1
    assert not (home / ".bash_profile").exists()


@pytest.mark.skipif(sys.platform == "win32", reason="requires POSIX fish startup files")
def test_fish_custom_xdg_uses_nonuniversal_snippet(tmp_path: Path) -> None:
    shell = shutil.which("fish")
    if shell is None:
        pytest.skip("fish is unavailable")
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    config = home / "custom config"
    env = isolated_env(home)
    env["SHELL"] = shell
    env["XDG_CONFIG_HOME"] = str(config)
    env["PATH"] = "/usr/bin:/bin"
    result = run_process(
        [str(clud_binary()), "--installer", "--install-current", "--yes"],
        env=env,
        capture_output=True,
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, result.stderr.decode(errors="replace")
    snippet = config / "fish" / "conf.d" / "clud-path.fish"
    assert snippet.is_file()
    assert "fish_add_path --path --move --" in snippet.read_text()
    assert not (config / "fish" / "fish_variables").exists()


@pytest.mark.skipif(sys.platform == "win32", reason="requires POSIX zsh startup files")
def test_zsh_custom_zdotdir_activation(tmp_path: Path) -> None:
    shell = shutil.which("zsh")
    if shell is None:
        pytest.skip("zsh is unavailable")
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    dotdir = home / "custom zsh"
    env = isolated_env(home)
    env["SHELL"] = shell
    env["ZDOTDIR"] = str(dotdir)
    env["PATH"] = "/usr/bin:/bin"
    result = run_process(
        [str(clud_binary()), "--installer", "--install-current", "--yes"],
        env=env,
        capture_output=True,
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, result.stderr.decode(errors="replace")
    assert (dotdir / ".zprofile").is_file()
    assert (dotdir / ".zshrc").is_file()


def test_exact_older_release_downloads_selected_executable(installer_target) -> None:
    env, destination = installer_target
    result = run_process(
        [str(clud_binary()), "--installer", "--install-version", "2.8.13", "--yes"],
        env=env,
        capture_output=True,
        timeout=120,
        check=False,
    )
    assert result.returncode == 0, result.stderr.decode(errors="replace")
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
def test_running_windows_target_remains_valid(installer_target) -> None:
    env, destination = installer_target
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
