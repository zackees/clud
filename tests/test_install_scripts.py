"""Sanity checks for the user-facing install scripts (issue #163).

Covers what the AC calls out:
- URL construction (the install URL the script bootstraps points at uv
  via the Astral domain, not an arbitrary third party).
- Version pinning (CLUD_VERSION → ``clud==<ver>`` spec, otherwise plain
  ``clud``).
- PATH mutation logic (POSIX surfaces the tool dir; PowerShell mutates
  the User-scope PATH idempotently).

These are static-content tests because the scripts shell out to network
installers we can't exercise inline. The point is to guard the contract
that a copy/paste install line keeps working after refactors.
"""

from __future__ import annotations

from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
SH = (ROOT / "install.sh").read_text(encoding="utf-8")
PS1 = (ROOT / "install.ps1").read_text(encoding="utf-8")


# ---- POSIX (install.sh) ---------------------------------------------------


def test_sh_has_posix_shebang() -> None:
    # `/usr/bin/env sh` keeps the script portable across distros where
    # /bin/sh may be a dash → bash → ksh symlink.
    assert SH.startswith("#!/usr/bin/env sh\n")


def test_sh_bootstraps_uv_from_astral() -> None:
    # The uv installer URL is the single network dependency. Pin it to
    # the official Astral domain so a refactor can't redirect it through
    # a malicious mirror.
    assert "https://astral.sh/uv/install.sh" in SH


def test_sh_supports_version_pin() -> None:
    # Default (latest) install path.
    assert 'spec="clud"' in SH
    # CLUD_VERSION env var produces an `==` spec.
    assert 'spec="clud==$CLUD_VERSION"' in SH
    assert 'CLUD_VERSION="${CLUD_VERSION:-}"' in SH


def test_sh_installs_via_uv_tool_with_force() -> None:
    # --force makes re-running the script upgrade in place, which the
    # issue's AC requires ("Re-running the installer upgrades or
    # confirms the existing install without leaving duplicate PATH
    # entries").
    assert 'uv tool install --force "$spec"' in SH


def test_sh_verifies_native_helper_after_install() -> None:
    assert "verify_clud_install" in SH
    assert "clud-block-bad-cmd" in SH
    assert "permissionDecision" in SH
    assert 'deny_command="bad"' in SH


def test_sh_updates_shell_path() -> None:
    # POSIX hand-off to uv's own shell-profile editor.
    assert "uv tool update-shell" in SH


def test_sh_has_clud_no_path_escape_hatch() -> None:
    # Users in CI / containers should be able to skip the profile edit
    # but still get clud installed.
    assert 'CLUD_NO_PATH' in SH


# ---- Windows (install.ps1) ------------------------------------------------


def test_ps1_has_requires_directive() -> None:
    # Pin a minimum PowerShell version so users hit a clear error rather
    # than a parser failure on 2.x / nano server.
    assert "#Requires -Version 5.1" in PS1


def test_ps1_bootstraps_uv_from_astral() -> None:
    assert "https://astral.sh/uv/install.ps1" in PS1


def test_ps1_supports_version_pin() -> None:
    # Default (latest) install path.
    assert "$spec = 'clud'" in PS1
    # CLUD_VERSION env var produces an `==` spec.
    assert '"clud==$($env:CLUD_VERSION)"' in PS1


def test_ps1_installs_via_uv_tool_with_force() -> None:
    assert "uv tool install --force $spec" in PS1


def test_ps1_verifies_native_helper_after_install() -> None:
    assert "Verify-CludInstall" in PS1
    assert "clud-block-bad-cmd.exe" in PS1
    assert "permissionDecision" in PS1
    assert "$denyCommand = 'bad'" in PS1


def test_ps1_path_mutation_is_idempotent() -> None:
    # The User PATH update reads the existing entries, checks membership,
    # and only writes when missing. Without this gate, every re-run
    # would append the same dir, growing PATH unbounded.
    assert "GetEnvironmentVariable('PATH', 'User')" in PS1
    assert "if ($entries -contains $dir)" in PS1
    assert "SetEnvironmentVariable('PATH', $newPath, 'User')" in PS1


def test_ps1_has_clud_no_path_escape_hatch() -> None:
    assert "CLUD_NO_PATH" in PS1


# ---- Cross-script invariants ---------------------------------------------


@pytest.mark.parametrize(
    ("script", "label"),
    [(SH, "install.sh"), (PS1, "install.ps1")],
    ids=["install.sh", "install.ps1"],
)
def test_script_does_not_call_pipx_or_pip(script: str, label: str) -> None:
    # Both AC-mandated installers route through uv, not pipx/pip, because
    # uv's tool-install path has the most reliable cross-platform PATH
    # behavior. If a future change adds a pipx/pip fallback, the test
    # should be relaxed deliberately rather than slipping in unnoticed.
    assert "pipx install" not in script, f"{label} unexpectedly calls pipx"
    assert "pip install" not in script, f"{label} unexpectedly calls pip"


@pytest.mark.parametrize(
    ("script", "label"),
    [(SH, "install.sh"), (PS1, "install.ps1")],
    ids=["install.sh", "install.ps1"],
)
def test_script_references_only_official_uv_origin(script: str, label: str) -> None:
    # Both scripts must source uv from astral.sh — nothing else. A bare
    # `curl ... | sh` from an attacker-controlled host would be the most
    # severe regression possible here, so make it loud if it changes.
    import re

    urls = re.findall(r"https?://[^\s'\")]+", script)
    third_party = [
        u
        for u in urls
        if u.startswith(("http://", "https://"))
        and "astral.sh/uv/install" not in u
        and "docs.astral.sh" not in u
        and "raw.githubusercontent.com/zackees/clud" not in u
    ]
    assert not third_party, f"{label} references unexpected URLs: {third_party}"
