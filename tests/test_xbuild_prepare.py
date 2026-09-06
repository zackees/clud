"""Unit tests for `ci/xbuild.py`'s local `soldr prepare` step.

#1017 blocker 2: CI persists the target toolchain env via `soldr prepare
--github-env "$GITHUB_ENV"`, and nothing did that locally, so a local release
wheel died inside `ring`. `prepare_toolchain_locally` is the local
counterpart: same command, same exported variables, applied in-process.
"""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

import pytest

from ci.xbuild import parse_github_env, prepare_toolchain_locally


def test_parse_single_line_and_heredoc_values() -> None:
    text = (
        "CC_x86_64_unknown_linux_gnu=/tc/bin/gcc\n"
        "\n"
        "PATH=/tc/bin:/usr/bin\n"
        "FLAGS<<ghadelimiter\n"
        "-a\n"
        "-b=c\n"
        "ghadelimiter\n"
        "EMPTY=\n"
    )
    assert parse_github_env(text) == {
        "CC_x86_64_unknown_linux_gnu": "/tc/bin/gcc",
        "PATH": "/tc/bin:/usr/bin",
        "FLAGS": "-a\n-b=c",
        "EMPTY": "",
    }


def test_parse_rejects_a_line_that_is_neither_shape() -> None:
    with pytest.raises(ValueError, match="unparseable"):
        parse_github_env("just words\n")


def _fake_soldr(monkeypatch: pytest.MonkeyPatch, *, returncode: int = 0) -> list[list[str]]:
    """Stand in for `soldr prepare`: record argv and write a canned env file."""
    calls: list[list[str]] = []

    def run(command: list[str], **_kwargs: object) -> SimpleNamespace:
        calls.append(command)
        env_file = Path(command[command.index("--github-env") + 1])
        env_file.write_text(
            "CC_aarch64_unknown_linux_gnu=/tc/bin/aarch64-linux-gnu-gcc\nPATH=/tc/bin:/usr/bin\n",
            encoding="utf-8",
        )
        return SimpleNamespace(returncode=returncode)

    monkeypatch.setattr("ci.xbuild.process.run", run)
    return calls


def test_it_runs_soldr_prepare_and_applies_the_exported_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls = _fake_soldr(monkeypatch)
    env = {"PATH": "/usr/bin", "SOLDR_BINARY": "/opt/soldr"}

    applied = prepare_toolchain_locally("aarch64-unknown-linux-gnu", env)

    assert calls[0][:4] == ["/opt/soldr", "prepare", "--target", "aarch64-unknown-linux-gnu"]
    assert "--github-env" in calls[0]
    assert applied == ["CC_aarch64_unknown_linux_gnu", "PATH"]
    # The same variables CI's later steps see, now visible to every child.
    assert env["CC_aarch64_unknown_linux_gnu"] == "/tc/bin/aarch64-linux-gnu-gcc"
    assert env["PATH"] == "/tc/bin:/usr/bin"


def test_it_is_inert_in_ci(monkeypatch: pytest.MonkeyPatch) -> None:
    """setup-build already ran prepare into $GITHUB_ENV; running it again
    per verb would just repeat work and could not persist anyway."""
    calls = _fake_soldr(monkeypatch)
    env = {"GITHUB_ACTIONS": "true", "SOLDR_BINARY": "/opt/soldr"}

    assert prepare_toolchain_locally("x86_64-unknown-linux-gnu", env) is None
    assert calls == []
    assert "CC_x86_64_unknown_linux_gnu" not in env


def test_it_can_be_skipped_explicitly(monkeypatch: pytest.MonkeyPatch) -> None:
    calls = _fake_soldr(monkeypatch)
    env = {"CLUD_XBUILD_SKIP_PREPARE": "1", "SOLDR_BINARY": "/opt/soldr"}

    assert prepare_toolchain_locally("x86_64-unknown-linux-gnu", env) is None
    assert calls == []


def test_without_soldr_it_defers_to_the_preflight(monkeypatch: pytest.MonkeyPatch) -> None:
    calls = _fake_soldr(monkeypatch)
    monkeypatch.setattr("ci.xbuild.shutil.which", lambda _name, path=None: None)
    env = {"PATH": "/nowhere"}

    assert prepare_toolchain_locally("x86_64-unknown-linux-gnu", env) is None
    assert calls == []


def test_a_failed_prepare_stops_the_build_with_a_reason(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A half-prepared toolchain must not fall through to the cargo failure
    the issue was filed about; the prepare failure is the real one."""
    _fake_soldr(monkeypatch, returncode=3)
    env = {"SOLDR_BINARY": "/opt/soldr"}

    with pytest.raises(SystemExit) as excinfo:
        prepare_toolchain_locally("x86_64-unknown-linux-gnu", env)
    assert "soldr prepare --target x86_64-unknown-linux-gnu" in str(excinfo.value)
    assert "exited 3" in str(excinfo.value)
    assert "PATH" not in env
