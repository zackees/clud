"""Unit tests for `ci/xbuild.py`'s cross-toolchain preflight.

#1017 blocker 2: a local cross `wheel` dies minutes in with

    error occurred in cc-rs: failed to find tool "x86_64-linux-gnu-gcc"

inside `ring`, which sent the issue looking at soldr and at clud's env
exports. The cause is neither: CI runs `soldr prepare --github-env`, and
nothing does that locally. The preflight makes that legible at entry.
"""

from __future__ import annotations

import pytest

from ci.xbuild import cross_toolchain_preflight


@pytest.fixture(autouse=True)
def _not_in_ci(monkeypatch: pytest.MonkeyPatch) -> None:
    """The preflight is deliberately inert in CI; test the local path."""
    monkeypatch.delenv("GITHUB_ACTIONS", raising=False)
    monkeypatch.delenv("TARGET_CC", raising=False)


def test_a_cross_gnu_target_without_a_toolchain_explains_itself(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("CC_aarch64_unknown_linux_gnu", raising=False)
    monkeypatch.setattr("ci.xbuild.shutil.which", lambda _name: None)
    monkeypatch.setattr("ci.xbuild._host_triple", lambda: "x86_64-unknown-linux-gnu")

    message = cross_toolchain_preflight("aarch64-unknown-linux-gnu")

    assert message is not None
    # Names the missing tool, the variable that would satisfy it, and where CI
    # gets it -- the three things whose absence made #1017 a two-place hunt.
    assert "aarch64-linux-gnu-gcc" in message
    assert "CC_aarch64_unknown_linux_gnu" in message
    assert "soldr prepare" in message
    assert "#1017" in message


def test_it_is_silent_in_ci(monkeypatch: pytest.MonkeyPatch) -> None:
    """CI prepares the toolchain in a prior step, so warning there is noise.

    This matters more than it looks: a preflight that fired in CI would fail
    every release build, since the env arrives via `$GITHUB_ENV` rather than
    anything this process can see at dispatch time."""
    monkeypatch.setenv("GITHUB_ACTIONS", "true")
    monkeypatch.setattr("ci.xbuild.shutil.which", lambda _name: None)
    monkeypatch.setattr("ci.xbuild._host_triple", lambda: "x86_64-unknown-linux-gnu")

    assert cross_toolchain_preflight("aarch64-unknown-linux-gnu") is None


def test_a_host_build_is_not_a_cross_build(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("ci.xbuild.shutil.which", lambda _name: None)
    monkeypatch.setattr("ci.xbuild._host_triple", lambda: "x86_64-unknown-linux-gnu")

    assert cross_toolchain_preflight("x86_64-unknown-linux-gnu") is None


def test_non_gnu_targets_are_out_of_scope(monkeypatch: pytest.MonkeyPatch) -> None:
    """Apple and MSVC fail earlier on their own SDK checks; musl is static.

    Claiming to cover them would be asserting a check that was never written."""
    monkeypatch.setattr("ci.xbuild.shutil.which", lambda _name: None)
    monkeypatch.setattr("ci.xbuild._host_triple", lambda: "x86_64-unknown-linux-gnu")

    for target in (
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
        "x86_64-unknown-linux-musl",
    ):
        assert cross_toolchain_preflight(target) is None, target


def test_an_explicit_cc_satisfies_it(monkeypatch: pytest.MonkeyPatch) -> None:
    """Someone who exported the variable has already solved the problem."""
    monkeypatch.setattr("ci.xbuild.shutil.which", lambda _name: None)
    monkeypatch.setattr("ci.xbuild._host_triple", lambda: "x86_64-unknown-linux-gnu")
    monkeypatch.setenv("CC_aarch64_unknown_linux_gnu", "/opt/cross/bin/gcc")

    assert cross_toolchain_preflight("aarch64-unknown-linux-gnu") is None


def test_a_toolchain_on_path_satisfies_it(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("CC_aarch64_unknown_linux_gnu", raising=False)
    monkeypatch.setattr("ci.xbuild._host_triple", lambda: "x86_64-unknown-linux-gnu")
    monkeypatch.setattr(
        "ci.xbuild.shutil.which",
        lambda name: "/usr/bin/" + name if name.endswith("-gcc") else None,
    )

    assert cross_toolchain_preflight("aarch64-unknown-linux-gnu") is None
