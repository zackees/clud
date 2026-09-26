"""The Kitty-compatible Windows GUI must ship as one intact bundle."""

from __future__ import annotations

import base64
import hashlib
import struct
import zipfile
from pathlib import Path

import pytest

from ci import process
from ci.kitty_wheel import (
    KITTY_BUNDLE_FILES,
    KITTY_SOURCE_REVISION,
    add_kitty_bundle,
    check_kitty_wheel,
    resolve_kitty_bundle,
)


def _wheel(tmp_path):
    wheel = tmp_path / "clud-2.7.1-py3-none-win_amd64.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        archive.writestr(
            "clud-2.7.1.dist-info/WHEEL",
            "Wheel-Version: 1.0\nRoot-Is-Purelib: false\nTag: py3-none-win_amd64\n",
        )
        archive.writestr(
            "clud-2.7.1.dist-info/METADATA",
            "Metadata-Version: 2.1\nName: clud\nVersion: 2.7.1\n",
        )
        archive.writestr("clud-2.7.1.data/scripts/clud.exe", b"clud")
        archive.writestr("clud-2.7.1.dist-info/RECORD", "stale")
    return wheel


def _bundle(tmp_path):
    bundle = tmp_path / "fork-build"
    for name in KITTY_BUNDLE_FILES:
        file = bundle / name
        file.parent.mkdir(parents=True, exist_ok=True)
        if name.lower().endswith((".exe", ".dll")):
            pe = bytearray(0x46)
            pe[:2] = b"MZ"
            struct.pack_into("<I", pe, 0x3C, 0x40)
            pe[0x40:0x44] = b"PE\0\0"
            struct.pack_into("<H", pe, 0x44, 0x8664)
            if name == "wezterm-gui.exe":
                pe.extend(b"--return-initial-exit-code\0")
            file.write_bytes(pe)
        else:
            file.write_bytes(name.encode())
    (bundle / "SOURCE_REVISION").write_text("zackees/wezterm@" + KITTY_SOURCE_REVISION + "\n")
    config = tmp_path / "clud-kittyterm.lua"
    config.write_text("return { enable_kitty_keyboard = true }\n")
    return bundle, config


def _paste_helper(tmp_path, machine=0x8664):
    helper = tmp_path / "clud-kittyterm-paste.exe"
    data = bytearray(0x46)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x40)
    data[0x40:0x44] = b"PE\0\0"
    struct.pack_into("<H", data, 0x44, machine)
    helper.write_bytes(data)
    return helper


def test_kitty_bundle_preserves_layout_and_record(tmp_path) -> None:
    wheel = _wheel(tmp_path)
    bundle, config = _bundle(tmp_path)

    helper = _paste_helper(tmp_path)
    add_kitty_bundle(wheel, bundle, config, "x86_64-pc-windows-msvc", helper)

    with zipfile.ZipFile(wheel) as archive:
        prefix = "clud-2.7.1.data/data/clud-kittyterm/"
        names = set(archive.namelist())
        assert {prefix + name for name in KITTY_BUNDLE_FILES} <= names
        assert prefix + "clud-kittyterm.lua" in names
        assert archive.read(prefix + "clud-kittyterm-paste.exe") == helper.read_bytes()
        assert archive.read(prefix + "mesa/opengl32.dll") == (
            bundle / "mesa/opengl32.dll"
        ).read_bytes()
        assert archive.read("clud-2.7.1.data/scripts/clud.exe") == b"clud"
        record = archive.read("clud-2.7.1.dist-info/RECORD").decode()
        for name in (*KITTY_BUNDLE_FILES, "clud-kittyterm-paste.exe"):
            member = prefix + name
            data = (helper if name == "clud-kittyterm-paste.exe" else bundle / name).read_bytes()
            digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
            assert f"{member},sha256={digest},{len(data)}" in record
    assert check_kitty_wheel(wheel) == []


def test_kitty_bundle_installs_with_uv_windows_scheme(tmp_path: Path) -> None:
    wheel = _wheel(tmp_path)
    bundle, config = _bundle(tmp_path)
    add_kitty_bundle(wheel, bundle, config, "x86_64-pc-windows-msvc", _paste_helper(tmp_path))
    installed = tmp_path / "installed"
    result = process.run(
        [
            "uv", "pip", "install", "--target", str(installed),
            "--python-platform", "x86_64-pc-windows-msvc", "--no-deps", str(wheel),
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert (installed / "clud-kittyterm" / "wezterm-gui.exe").is_file()
    assert (installed / "clud-kittyterm" / "conpty.dll").is_file()


def test_checker_rejects_bundle_in_wheel_scripts_scheme(tmp_path: Path) -> None:
    wheel = _wheel(tmp_path)
    with zipfile.ZipFile(wheel, "a") as archive:
        archive.writestr("clud-2.7.1.data/scripts/clud-kittyterm/wezterm-gui.exe", b"bad")
    assert any("cannot be under wheel scripts" in error for error in check_kitty_wheel(wheel))


def test_kitty_bundle_rejects_missing_runtime_files(tmp_path) -> None:
    wheel = _wheel(tmp_path)
    bundle, config = _bundle(tmp_path)
    (bundle / "conpty.dll").unlink()

    with pytest.raises(RuntimeError, match=r"conpty\.dll"):
        add_kitty_bundle(wheel, bundle, config, "x86_64-pc-windows-msvc")


def test_kitty_bundle_rejects_non_windows_target(tmp_path) -> None:
    wheel = _wheel(tmp_path)
    bundle, config = _bundle(tmp_path)

    with pytest.raises(ValueError, match="supports only"):
        add_kitty_bundle(wheel, bundle, config, "x86_64-unknown-linux-gnu")


def test_kitty_bundle_rejects_arm64_target(tmp_path) -> None:
    wheel = _wheel(tmp_path)
    bundle, config = _bundle(tmp_path)
    with pytest.raises(ValueError, match="supports only"):
        add_kitty_bundle(wheel, bundle, config, "aarch64-pc-windows-msvc")


def test_arm64_checker_rejects_foreign_kitty_bundle(tmp_path) -> None:
    wheel = tmp_path / "clud-2.7.1-py3-none-win_arm64.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        archive.writestr("clud-2.7.1.dist-info/WHEEL", "Wheel-Version: 1.0\n")
    assert check_kitty_wheel(wheel) == []
    with zipfile.ZipFile(wheel, "a") as archive:
        archive.writestr("clud-2.7.1.data/data/clud-kittyterm/wezterm-gui.exe", b"x64")
    assert any("forbidden in ARM64" in error for error in check_kitty_wheel(wheel))


def test_kitty_bundle_requires_configured_artifact(monkeypatch) -> None:
    monkeypatch.delenv("CLUD_KITTYTERM_BUNDLE_DIR", raising=False)
    monkeypatch.delenv("CLUD_KITTYTERM_SOURCE_REVISION", raising=False)
    with pytest.raises(RuntimeError, match="CLUD_KITTYTERM_BUNDLE_DIR"):
        resolve_kitty_bundle()


def test_kitty_bundle_rejects_wrong_revision(tmp_path) -> None:
    wheel = _wheel(tmp_path)
    bundle, config = _bundle(tmp_path)
    (bundle / "SOURCE_REVISION").write_text("zackees/wezterm@" + "b" * 40 + "\n")
    with pytest.raises(RuntimeError, match="SOURCE_REVISION"):
        add_kitty_bundle(wheel, bundle, config, "x86_64-pc-windows-msvc")


def test_kitty_bundle_rejects_foreign_architecture(tmp_path) -> None:
    wheel = _wheel(tmp_path)
    bundle, config = _bundle(tmp_path)
    data = bytearray((bundle / "wezterm-gui.exe").read_bytes())
    struct.pack_into("<H", data, 0x44, 0xAA64)
    (bundle / "wezterm-gui.exe").write_bytes(data)
    with pytest.raises(RuntimeError, match=r"non-x64 PE wezterm-gui\.exe"):
        add_kitty_bundle(wheel, bundle, config, "x86_64-pc-windows-msvc")


def test_kitty_bundle_rejects_gui_without_exit_code_flag(tmp_path) -> None:
    wheel = _wheel(tmp_path)
    bundle, config = _bundle(tmp_path)
    (bundle / "wezterm-gui.exe").write_bytes(
        (bundle / "wezterm-gui.exe")
        .read_bytes()
        .replace(b"--return-initial-exit-code", b"--obsolete-gui-option-----")
    )
    with pytest.raises(RuntimeError, match="return-initial-exit-code"):
        add_kitty_bundle(wheel, bundle, config, "x86_64-pc-windows-msvc", _paste_helper(tmp_path))


def test_kitty_wheel_check_rejects_gui_without_exit_code_flag(tmp_path) -> None:
    wheel = _wheel(tmp_path)
    bundle, config = _bundle(tmp_path)
    add_kitty_bundle(wheel, bundle, config, "x86_64-pc-windows-msvc", _paste_helper(tmp_path))
    member = "clud-2.7.1.data/data/clud-kittyterm/wezterm-gui.exe"
    with pytest.warns(UserWarning, match="Duplicate name"):
        with zipfile.ZipFile(wheel, "a") as archive:
            archive.writestr(member, (bundle / "wezterm-gui.exe").read_bytes().replace(
                b"--return-initial-exit-code", b"--obsolete-gui-option-----"
            ))
    assert any("return-initial-exit-code" in error for error in check_kitty_wheel(wheel))


def test_kitty_wheel_check_rejects_incomplete_bundle(tmp_path) -> None:
    wheel = _wheel(tmp_path)
    assert any("wezterm.exe" in error for error in check_kitty_wheel(wheel))


def test_kitty_bundle_rejects_missing_or_foreign_paste_helper(tmp_path) -> None:
    wheel = _wheel(tmp_path)
    bundle, config = _bundle(tmp_path)
    with pytest.raises(RuntimeError, match=r"clud-kittyterm-paste\.exe"):
        add_kitty_bundle(wheel, bundle, config, "x86_64-pc-windows-msvc")
    with pytest.raises(RuntimeError, match="non-x64 PE"):
        add_kitty_bundle(
            wheel, bundle, config, "x86_64-pc-windows-msvc", _paste_helper(tmp_path, 0xAA64)
        )


def test_kitty_wheel_check_rejects_stale_record(tmp_path) -> None:
    wheel = _wheel(tmp_path)
    bundle, config = _bundle(tmp_path)
    add_kitty_bundle(wheel, bundle, config, "x86_64-pc-windows-msvc", _paste_helper(tmp_path))
    with pytest.warns(UserWarning, match="Duplicate name"):
        with zipfile.ZipFile(wheel, "a") as archive:
            archive.writestr("clud-2.7.1.data/data/clud-kittyterm/conpty.dll", b"changed")
    assert any("invalid RECORD" in error for error in check_kitty_wheel(wheel))
