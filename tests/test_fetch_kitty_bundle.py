"""Offline checks for provenance-gated Kitty CI artifact acquisition."""

from __future__ import annotations

import hashlib
import io
import struct
import zipfile
from pathlib import Path
from unittest.mock import patch

import pytest

from ci.fetch_kitty_bundle import SOURCE_REVISION, fetch_bundle
from ci.kitty_wheel import KITTY_BUNDLE_FILES

ROOT = Path(__file__).resolve().parent.parent
PINNED_URL = (
    "https://github.com/zackees/wezterm/releases/download/"
    "kitty-windows-compat-4877c4c1/WezTerm-windows-portable.zip"
)
PINNED_SHA256 = "97d04836ba5edb75425e59450d794ab564cdd7e7a124ed387ae081de9ba56f74"


def test_windows_x64_wheel_fetches_pinned_bundle_before_build() -> None:
    workflow = (ROOT / ".github/workflows/_build-target.yml").read_text(encoding="utf-8")
    assert PINNED_URL in workflow
    assert PINNED_SHA256 in workflow
    assert 'inputs.target == \'x86_64-pc-windows-msvc\'' in workflow
    assert '"$VENV_PY" -m ci.fetch_kitty_bundle' in workflow
    assert "echo \"CLUD_KITTYTERM_BUNDLE_DIR=" in workflow
    assert workflow.index("-m ci.fetch_kitty_bundle") < workflow.index("-m ci.xbuild wheel")


def test_native_windows_integration_smokes_installed_gui() -> None:
    workflow = (ROOT / ".github/workflows/_run-tests.yml").read_text(encoding="utf-8")
    assert "inputs.target == 'x86_64-pc-windows-msvc' && inputs.suite == 'integration'" in workflow
    assert "pwsh -NoProfile -File ci/kitty_windows_smoke.ps1" in workflow
    assert workflow.index("-m ci.run_bundle") < workflow.index("ci/kitty_windows_smoke.ps1")
    assert "-ScriptsDir (Join-Path $env:GITHUB_WORKSPACE '.venv/Scripts')" in workflow
    assert "-ScriptsDir (Split-Path $env:VENV_PY -Parent)" not in workflow


def _pe(machine: int = 0x8664) -> bytes:
    data = bytearray(0x46)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x40)
    data[0x40:0x44] = b"PE\0\0"
    struct.pack_into("<H", data, 0x44, machine)
    return bytes(data)


def _archive(revision: str = SOURCE_REVISION, overrides: dict[str, bytes] | None = None) -> bytes:
    overrides = overrides or {}
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w") as archive:
        for name in KITTY_BUNDLE_FILES:
            if name in overrides:
                data = overrides[name]
            elif name == "SOURCE_REVISION":
                data = revision.encode()
            elif name.lower().endswith((".exe", ".dll")):
                data = _pe() + (b"--return-initial-exit-code" if name == "wezterm-gui.exe" else b"")
            else:
                data = b"fixture"
            archive.writestr(name, data)
    return output.getvalue()


def _fetch(tmp_path: Path, archive: bytes, digest: str | None = None) -> Path:
    with patch("ci.fetch_kitty_bundle.urllib.request.urlopen", return_value=io.BytesIO(archive)):
        return fetch_bundle(
            "https://github.com/zackees/wezterm/releases/download/pin/portable.zip",
            digest or hashlib.sha256(archive).hexdigest(),
            tmp_path / "bundle",
        )


def test_fetch_verified_bundle(tmp_path: Path) -> None:
    bundle = _fetch(tmp_path, _archive())
    assert b"--return-initial-exit-code" in (bundle / "wezterm-gui.exe").read_bytes()
    assert (bundle / "SOURCE_REVISION").read_text() == SOURCE_REVISION


def test_rejects_wrong_digest_without_exposing_bundle(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="SHA-256 mismatch"):
        _fetch(tmp_path, _archive(), "0" * 64)
    assert not (tmp_path / "bundle").exists()


def test_rejects_wrong_revision_without_exposing_bundle(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="source revision mismatch"):
        _fetch(tmp_path, _archive("zackees/wezterm@wrong"))
    assert not (tmp_path / "bundle").exists()


def test_rejects_missing_portable_dependency(tmp_path: Path) -> None:
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w") as archive:
        archive.writestr("SOURCE_REVISION", SOURCE_REVISION)
    with pytest.raises(RuntimeError, match=r"missing wezterm\.exe"):
        _fetch(tmp_path, output.getvalue())
    assert not (tmp_path / "bundle").exists()


def test_rejects_zip_traversal(tmp_path: Path) -> None:
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w") as archive:
        archive.writestr("../escape", "bad")
    with pytest.raises(ValueError, match="unsafe path"):
        _fetch(tmp_path, output.getvalue())
    assert not (tmp_path / "escape").exists()


@pytest.mark.parametrize(
    ("overrides", "message"),
    [
        ({"wezterm-gui.exe": _pe()}, "return-initial-exit-code"),
        ({"wezterm-gui.exe": b"fixture"}, "invalid PE wezterm-gui.exe"),
        ({"conpty.dll": _pe(0xAA64)}, "non-x64 PE conpty.dll"),
    ],
)
def test_rejects_invalid_bundle_before_publication(
    tmp_path: Path, overrides: dict[str, bytes], message: str
) -> None:
    with pytest.raises(RuntimeError, match=message):
        _fetch(tmp_path, _archive(overrides=overrides))
    assert not (tmp_path / "bundle").exists()
