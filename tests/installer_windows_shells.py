"""Verify persisted installer activation through each supported Windows shell."""

from __future__ import annotations

import shutil
from pathlib import Path, PureWindowsPath

from tests.process import run as run_process


def ps_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def git_bash_executable(git: Path) -> Path:
    """Resolve Bash within the Git installation, including bundled toolchains."""
    root = git.parent.parent
    bash = root / "bin" / "bash.exe"
    if not bash.is_file() and git.parent.name.casefold() == "bin":
        if root.name.casefold() in {"mingw32", "mingw64", "ucrt64", "clangarm64", "usr"}:
            bash = root.parent / "bin" / "bash.exe"
    assert bash.is_file(), f"Git Bash is missing beside {git}: {bash}"
    return bash


def windows_shell_commands() -> dict[str, list[str]]:
    """Resolve shell executables before replacing the inherited runner PATH."""
    resolved = {name: shutil.which(name) for name in ("powershell.exe", "pwsh.exe", "cmd.exe")}
    for name, executable in resolved.items():
        assert executable, f"required installer acceptance shell is missing: {name}"
    git = shutil.which("git.exe")
    assert git, "Git for Windows is required for Git Bash installer acceptance"
    bash = git_bash_executable(Path(git))
    powershell_lookup = (
        "$c=Get-Command clud -CommandType Application -ErrorAction Stop; "
        "Write-Output $c.Source; & $c.Source --version; exit $LASTEXITCODE"
    )
    return {
        "powershell-5.1": [
            resolved["powershell.exe"], "-NoProfile", "-NonInteractive",
            "-Command", powershell_lookup,
        ],
        "powershell-7": [
            resolved["pwsh.exe"], "-NoProfile", "-NonInteractive",
            "-Command", powershell_lookup,
        ],
        "cmd": [resolved["cmd.exe"], "/d", "/c", "where.exe clud && clud --version"],
        "git-bash": [
            str(bash), "--login", "-c",
            'cygpath -w "$(command -v clud)"; clud --version',
        ],
    }


def assert_windows_shell_activation(
    env: dict[str, str], destination: Path, version: str
) -> dict[str, str]:
    """Launch shells with machine/user PATH reconstructed from persisted state."""
    launcher = shutil.which("pwsh.exe")
    assert launcher, "PowerShell 7 is required for installer acceptance"
    evidence = {}
    for shell, command in windows_shell_commands().items():
        invocation = "& " + " ".join(ps_literal(argument) for argument in command)
        script = (
            "$ErrorActionPreference='Stop'; "
            "[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); "
            '$m=[Environment]::GetEnvironmentVariable("Path","Machine"); '
            '$u=[Environment]::GetEnvironmentVariable("Path","User"); '
            '$env:Path=[Environment]::ExpandEnvironmentVariables("$m;$u"); '
            + invocation
            + "; exit $LASTEXITCODE"
        )
        result = run_process(
            [launcher, "-NoProfile", "-NonInteractive", "-Command", script],
            env=env,
            capture_output=True,
            timeout=30,
            check=False,
        )
        assert result.returncode == 0, (shell, result.stderr.decode(errors="replace"))
        lines = result.stdout.decode("utf-8-sig").strip().splitlines()
        assert len(lines) >= 2, (shell, lines)
        if shell != "cmd":
            assert len(lines) == 2, (shell, lines)
        assert PureWindowsPath(lines[0]) == PureWindowsPath(destination), (shell, lines)
        assert lines[-1] == f"clud {version}", (shell, lines)
        evidence[shell] = lines[0]
    return evidence
