"""Git Bash lookup must follow the actual Git for Windows installation layout."""

from pathlib import Path

import pytest

from tests.installer_windows_shells import git_bash_executable


@pytest.mark.parametrize("layout", [
    "cmd/git.exe", "bin/git.exe", "mingw32/bin/git.exe", "mingw64/bin/git.exe",
    "ucrt64/bin/git.exe", "clangarm64/bin/git.exe", "usr/bin/git.exe",
])
def test_git_bash_resolves_standard_and_nested_git_layouts(tmp_path: Path, layout: str) -> None:
    root = tmp_path / "Git with spaces and ünicode"
    git = root / layout
    git.parent.mkdir(parents=True)
    git.touch()
    bash = root / "bin" / "bash.exe"
    bash.parent.mkdir(parents=True, exist_ok=True)
    bash.touch()
    assert git_bash_executable(git) == bash


def test_git_installation_named_like_a_toolchain_keeps_its_own_bash(tmp_path: Path) -> None:
    root = tmp_path / "ucrt64"
    git = root / "bin" / "git.exe"
    git.parent.mkdir(parents=True)
    git.touch()
    bash = root / "bin" / "bash.exe"
    bash.touch()
    assert git_bash_executable(git) == bash


def test_missing_git_bash_does_not_select_an_unrelated_ancestor(tmp_path: Path) -> None:
    git = tmp_path / "Git" / "cmd" / "git.exe"
    git.parent.mkdir(parents=True)
    git.touch()
    unrelated = tmp_path / "bin" / "bash.exe"
    unrelated.parent.mkdir()
    unrelated.touch()
    with pytest.raises(AssertionError, match="Git Bash is missing"):
        git_bash_executable(git)


def test_persisted_environment_excludes_inherited_path_spellings(monkeypatch) -> None:
    import json
    from types import SimpleNamespace

    from tests import installer_windows_shells as shells

    inherited = {"Path": "C:\\shadow", "PATH": "C:\\other", "SystemRoot": "C:\\Windows"}
    original = inherited.copy()
    persisted = "C:\\Windows;C:\\User with ünicode\\bin"
    monkeypatch.setattr(shells.shutil, "which", lambda _name: "C:\\PowerShell\\pwsh.exe")

    def read_registry(_command, **kwargs):
        assert kwargs["env"] == original
        return SimpleNamespace(
            returncode=0, stdout=json.dumps(persisted).encode(), stderr=b""
        )

    monkeypatch.setattr(shells, "run_process", read_registry)
    fresh = shells.persisted_windows_environment(inherited)
    assert fresh == {"PATH": persisted, "SystemRoot": "C:\\Windows"}
    assert inherited == original
