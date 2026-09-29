"""#1545: one wheel writer, and every post-processor keeps script exec bits."""

from __future__ import annotations

import stat
import types
import zipfile
from pathlib import Path

import pytest

from ci import banned_wheel_writes, build_wheel, check_wheel_modes, wheel_repair, wheel_smoke
from ci.ci_matrix import wheel_smoke_matrix
from ci.kitty_wheel import add_kitty_bundle, check_kitty_wheel
from ci.webterm_wheel import add_companion
from ci.wheel_rewrite import new_entry, rewrite_wheel, write_wheel
from tests.test_kitty_wheel import _bundle, _paste_helper

DATE = (2024, 5, 6, 7, 8, 10)
EXEC = stat.S_IFREG | 0o755


def _synthetic(path: Path, platform_tag: str = "manylinux_2_17_x86_64") -> Path:
    """A wheel shaped like maturin's: 0755 scripts, deflated, fixed date."""
    wheel = path / f"clud-2.8.20-py3-none-{platform_tag}.whl"
    suffix = ".exe" if platform_tag.startswith("win") else ""
    with zipfile.ZipFile(wheel, "w") as archive:
        def put(name: str, data: bytes, mode: int) -> None:
            info = zipfile.ZipInfo(name, date_time=DATE)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.create_system = 3
            info.external_attr = mode << 16
            archive.writestr(info, data)

        for script in (*build_wheel.REQUIRED_SCRIPTS, "clud-ctrlc-probe"):
            put(f"clud-2.8.20.data/scripts/{script}{suffix}", b"\x7fELF" + script.encode(), EXEC)
        put("clud/__init__.py", b"", stat.S_IFREG | 0o644)
        put("clud-2.8.20.dist-info/WHEEL", b"Wheel-Version: 1.0\n", stat.S_IFREG | 0o644)
        put("clud-2.8.20.dist-info/RECORD", b"stale", stat.S_IFREG | 0o644)
    return wheel


def _infos(wheel: Path) -> dict[str, zipfile.ZipInfo]:
    with zipfile.ZipFile(wheel) as archive:
        return {info.filename: info for info in archive.infolist()}


def _assert_scripts_keep_metadata(wheel: Path) -> None:
    infos = _infos(wheel)
    scripts = [info for name, info in infos.items() if ".data/scripts/" in name]
    assert scripts
    for info in scripts:
        assert info.external_attr >> 16 == EXEC, (info.filename, oct(info.external_attr >> 16))
        assert info.compress_type == zipfile.ZIP_DEFLATED
    for name in (f"clud-2.8.20.data/scripts/{s}" for s in build_wheel.REQUIRED_SCRIPTS):
        if name in infos:
            assert infos[name].date_time == DATE
    assert check_wheel_modes.check_wheel(wheel) == []


def test_prune_round_trip_keeps_modes(tmp_path) -> None:
    wheel = _synthetic(tmp_path)
    assert build_wheel.prune_nonproduction_scripts(wheel)
    assert "clud-2.8.20.data/scripts/clud-ctrlc-probe" not in _infos(wheel)
    _assert_scripts_keep_metadata(wheel)


def test_elf_strip_round_trip_keeps_modes(monkeypatch, tmp_path) -> None:
    wheel = _synthetic(tmp_path)
    monkeypatch.setattr(build_wheel, "elf_objcopy_candidates", lambda _target=None: ["objcopy"])

    def fake_objcopy(argv, **_kwargs):
        Path(argv[-1]).write_bytes(b"\x7fELFstripped")
        return types.SimpleNamespace(returncode=0)

    monkeypatch.setattr(build_wheel.process, "run", fake_objcopy)
    assert build_wheel.remove_elf_debug_metadata(wheel, target="x86_64-unknown-linux-gnu")
    with zipfile.ZipFile(wheel) as archive:
        assert archive.read("clud-2.8.20.data/scripts/clud") == b"\x7fELFstripped"
    _assert_scripts_keep_metadata(wheel)


def test_webterm_round_trip_keeps_modes(tmp_path) -> None:
    wheel = _synthetic(tmp_path, "macosx_11_0_arm64")
    companion = tmp_path / "clud-webterm"
    companion.write_bytes(b"webterm")
    add_companion(wheel, companion, "aarch64-apple-darwin")
    assert "clud-2.8.20.data/scripts/clud-webterm" in _infos(wheel)
    _assert_scripts_keep_metadata(wheel)


def test_windows_gnu_repair_round_trip_keeps_modes(monkeypatch, tmp_path) -> None:
    wheel = _synthetic(tmp_path, "win_amd64")
    dll = tmp_path / "libstdc++-6.dll"
    dll.write_bytes(b"MZ")
    monkeypatch.setattr(wheel_repair, "os", types.SimpleNamespace(name="nt"))
    monkeypatch.setattr(wheel_repair, "find_windows_gnu_runtime_dlls", lambda: [dll])
    assert wheel_repair.repair_windows_gnu_wheel(wheel)
    assert "clud-2.8.20.data/scripts/libstdc++-6.dll" in _infos(wheel)
    _assert_scripts_keep_metadata(wheel)


def test_kitty_round_trip_keeps_modes(tmp_path) -> None:
    wheel = _synthetic(tmp_path, "win_amd64")
    bundle, config = _bundle(tmp_path)
    add_kitty_bundle(wheel, bundle, config, "x86_64-pc-windows-msvc", _paste_helper(tmp_path))
    _assert_scripts_keep_metadata(wheel)
    assert check_kitty_wheel(wheel) == []


def test_rewrite_regenerates_record_once_and_added_entries_need_a_mode(tmp_path) -> None:
    wheel = _synthetic(tmp_path)
    rewrite_wheel(wheel, add=[new_entry("clud-2.8.20.data/scripts/extra", b"x")])
    with zipfile.ZipFile(wheel) as archive:
        records = [n for n in archive.namelist() if n.endswith("RECORD")]
        assert records == ["clud-2.8.20.dist-info/RECORD"]
        record = archive.read(records[0]).decode()
    assert "clud-2.8.20.data/scripts/extra,sha256=" in record
    assert "stale" not in record
    assert _infos(wheel)["clud-2.8.20.data/scripts/extra"].external_attr >> 16 == EXEC
    with pytest.raises(ValueError, match="explicit mode"):
        new_entry("clud-2.8.20.dist-info/METADATA", b"")


def test_write_wheel_creates_executable_scripts(tmp_path) -> None:
    wheel = tmp_path / "clud-1.0-py3-none-manylinux_2_17_x86_64.whl"
    write_wheel(
        wheel,
        [
            ("clud-1.0.data/scripts/clud", b"bin", None),
            ("clud-1.0.dist-info/WHEEL", b"", 0o644),
        ],
    )
    assert check_wheel_modes.check_wheel(wheel) == []
    assert "clud-1.0.dist-info/RECORD" in _infos(wheel)


def test_mode_gate_fails_0644_scripts_like_2_8_20(tmp_path) -> None:
    """The regression shape of #1544: every script stored as 0o100644."""
    wheel = _synthetic(tmp_path)
    broken = tmp_path / "dist" / wheel.name
    broken.parent.mkdir()
    with zipfile.ZipFile(wheel) as source, zipfile.ZipFile(broken, "w") as out:
        for info in source.infolist():
            data = source.read(info)
            if ".data/scripts/" in info.filename:
                info.external_attr = (stat.S_IFREG | 0o644) << 16
            out.writestr(info, data)
    errors = check_wheel_modes.check_wheel(broken)
    assert len(errors) == len(build_wheel.REQUIRED_SCRIPTS) + 1
    assert all("is not executable" in error for error in errors)
    assert check_wheel_modes.main(["--dist-dir", str(broken.parent)]) == 1


def test_mode_gate_rejects_non_regular_and_skips_windows(tmp_path) -> None:
    wheel = tmp_path / "clud-1.0-py3-none-manylinux_2_17_x86_64.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        info = zipfile.ZipInfo("clud-1.0.data/scripts/clud")
        info.external_attr = (stat.S_IFLNK | 0o777) << 16
        archive.writestr(info, b"target")
    assert "not a regular file" in check_wheel_modes.check_wheel(wheel)[0]
    windows = tmp_path / "clud-1.0-py3-none-win_amd64.whl"
    wheel.rename(windows)
    assert check_wheel_modes.check_wheel(windows) == []


def test_mode_gate_passes_good_dist_and_fails_empty(tmp_path) -> None:
    _synthetic(tmp_path)
    assert check_wheel_modes.main(["--dist-dir", str(tmp_path)]) == 0
    assert check_wheel_modes.main(["--dist-dir", str(tmp_path / "empty")]) == 1


def test_check_wheel_modes_is_stdlib_only() -> None:
    """Copyable verbatim into other repos (#1545)."""
    source = Path(check_wheel_modes.__file__).read_text(encoding="utf-8")
    assert "from ci" not in source
    assert "import ci" not in source


def test_banned_wheel_writes_flags_extractall_and_zip_writes() -> None:
    text = "\n".join(
        [
            "archive.extractall(root)",
            'with zipfile.ZipFile(path, "w") as out:',
            "with zipfile.ZipFile(",
            "    path,",
            '    mode="a",',
            ") as out:",
            "with zipfile.ZipFile(path) as source:",
            "tar.extractall(dest)  # wheel-write-lint: allow (tar)",
        ]
    )
    assert banned_wheel_writes.scan(text) == [1, 2, 3]


def test_ci_tree_has_no_banned_wheel_writes() -> None:
    assert banned_wheel_writes.main() == 0


def test_wheel_smoke_requires_exactly_one_wheel(tmp_path) -> None:
    (tmp_path / "clud-1.0.tar.gz").write_bytes(b"")
    with pytest.raises(SystemExit):
        wheel_smoke.select_wheel(tmp_path)
    wheel = _synthetic(tmp_path)
    assert wheel_smoke.select_wheel(tmp_path) == wheel


def test_wheel_smoke_matrix_covers_every_native_linux_and_macos_release_wheel() -> None:
    include = wheel_smoke_matrix()["include"]
    assert {entry["target"] for entry in include} == {
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
    }
    assert all(entry["artifact"].startswith("wheels-") for entry in include)
