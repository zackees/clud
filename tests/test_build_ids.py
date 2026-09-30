"""Contracts for ci/build_ids.py, the release side of #1016's pairing."""

from __future__ import annotations

import argparse
import struct
import zipfile
from pathlib import Path

import pytest

from ci import build_ids, build_wheel, wheel_repair, xbuild

BUILD_ID = bytes.fromhex("0123456789abcdef0123456789abcdef01234567")


def _note(name: bytes, ntype: int, desc: bytes) -> bytes:
    def pad(raw: bytes) -> bytes:
        return raw + b"\0" * (-len(raw) % 4)

    return struct.pack("<III", len(name), len(desc), ntype) + pad(name) + pad(desc)


def elf64(notes: bytes, *, extra_phdrs: int = 0) -> bytes:
    """A minimal little-endian ELF64 image: header, PT_LOAD fillers, one PT_NOTE."""
    phnum = extra_phdrs + 1
    phoff = 64
    notes_at = phoff + 56 * phnum
    header = bytearray(64)
    header[:4] = b"\x7fELF"
    header[4] = 2  # ELFCLASS64
    header[5] = 1  # little-endian
    struct.pack_into("<Q", header, 0x20, phoff)
    struct.pack_into("<HH", header, 0x36, 56, phnum)
    phdrs = b""
    for _ in range(extra_phdrs):
        phdrs += struct.pack("<IIQQQQQQ", 1, 0, 0, 0, 0, 0, 0, 0)  # PT_LOAD
    phdrs += struct.pack("<IIQQQQQQ", 4, 0, notes_at, 0, 0, len(notes), len(notes), 4)
    return bytes(header) + phdrs + notes


def elf32(notes: bytes) -> bytes:
    phoff = 52
    notes_at = phoff + 32
    header = bytearray(52)
    header[:4] = b"\x7fELF"
    header[4] = 1
    header[5] = 1
    struct.pack_into("<I", header, 0x1C, phoff)
    struct.pack_into("<HH", header, 0x2A, 32, 1)
    phdr = struct.pack("<IIIIIIII", 4, notes_at, 0, 0, len(notes), len(notes), 0, 4)
    return bytes(header) + phdr + notes


def test_reads_the_gnu_build_id_from_a_note_segment() -> None:
    image = elf64(_note(b"GNU\0", 3, BUILD_ID), extra_phdrs=2)
    assert build_ids.read_gnu_build_id(image) == BUILD_ID.hex()


def test_skips_other_notes_before_the_build_id() -> None:
    notes = _note(b"GNU\0", 1, b"\0" * 16) + _note(b"GNU\0", 3, BUILD_ID)
    assert build_ids.read_gnu_build_id(elf64(notes)) == BUILD_ID.hex()


def test_reads_elf32_too() -> None:
    assert build_ids.read_gnu_build_id(elf32(_note(b"GNU\0", 3, BUILD_ID))) == BUILD_ID.hex()


@pytest.mark.parametrize(
    "image",
    [
        b"",
        b"MZ" + b"\0" * 100,
        elf64(_note(b"GNU\0", 1, b"\0" * 16)),  # ABI tag only, no build-id
        elf64(_note(b"XYZ\0", 3, BUILD_ID)),  # right type, wrong owner
        elf64(_note(b"GNU\0", 3, BUILD_ID))[:130],  # truncated
    ],
)
def test_anything_without_a_well_formed_build_id_is_none(image: bytes) -> None:
    assert build_ids.read_gnu_build_id(image) is None


def _wheel(tmp_path: Path, scripts: dict[str, bytes]) -> Path:
    wheel = tmp_path / "clud-1.0.0-py3-none-manylinux_2_17_x86_64.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        for name, data in scripts.items():
            archive.writestr(f"clud-1.0.0.data/scripts/{name}", data)
    return wheel


def test_the_id_comes_from_the_shipped_clud_entry_not_a_sibling(tmp_path: Path) -> None:
    other = bytes.fromhex("ff" * 20)
    wheel = _wheel(
        tmp_path,
        {
            "clud-cmd-scan": elf64(_note(b"GNU\0", 3, other)),
            "clud": elf64(_note(b"GNU\0", 3, BUILD_ID)),
        },
    )
    assert build_ids.wheel_binary_build_id(wheel) == BUILD_ID.hex()


def test_record_then_combine_produces_one_sorted_manifest(tmp_path: Path) -> None:
    debuginfo = tmp_path / "debuginfo"
    arm_id = bytes.fromhex("ab" * 20)
    (tmp_path / "a").mkdir()
    (tmp_path / "b").mkdir()
    x86 = _wheel(tmp_path / "a", {"clud": elf64(_note(b"GNU\0", 3, BUILD_ID))})
    arm = _wheel(tmp_path / "b", {"clud": elf64(_note(b"GNU\0", 3, arm_id))})

    build_ids.record("x86_64-unknown-linux-gnu", [x86], debuginfo)
    build_ids.record("aarch64-unknown-linux-gnu", [arm], debuginfo)
    (debuginfo / "clud-x86_64-unknown-linux-gnu.dwp").write_bytes(b"dwarf")

    out = build_ids.combine(debuginfo)

    assert out == debuginfo / "BUILD-IDS.txt"
    assert out.read_text() == (
        f"{arm_id.hex()}  aarch64-unknown-linux-gnu\n"
        f"{BUILD_ID.hex()}  x86_64-unknown-linux-gnu\n"
    )
    # Fragments are folded away so they are not attached as assets of their own;
    # the sidecar itself is untouched.
    assert sorted(p.name for p in debuginfo.iterdir()) == [
        "BUILD-IDS.txt",
        "clud-x86_64-unknown-linux-gnu.dwp",
    ]


def test_record_without_an_id_warns_and_writes_nothing(tmp_path: Path, capsys) -> None:
    wheel = _wheel(tmp_path, {"clud": elf64(_note(b"GNU\0", 1, b"\0" * 16))})
    assert build_ids.record("x86_64-unknown-linux-gnu", [wheel], tmp_path / "d") is None
    assert not (tmp_path / "d").exists()
    assert "::warning::" in capsys.readouterr().out


def test_combine_with_no_fragments_writes_no_manifest(tmp_path: Path) -> None:
    assert build_ids.combine(tmp_path) is None
    assert not (tmp_path / "BUILD-IDS.txt").exists()


def test_combine_refuses_two_ids_for_one_triple(tmp_path: Path) -> None:
    (tmp_path / "a.build-id").write_text(f"{'aa' * 20}  x86_64-unknown-linux-gnu\n")
    (tmp_path / "b.build-id").write_text(f"{'bb' * 20}  x86_64-unknown-linux-gnu\n")
    with pytest.raises(ValueError, match="more than one build-id"):
        build_ids.combine(tmp_path)


def test_the_asset_name_matches_the_fetcher() -> None:
    source = (
        Path(__file__).resolve().parent.parent / "crates" / "clud-bin" / "src" / "symbols.rs"
    ).read_text(encoding="utf-8")
    assert f'pub const BUILD_IDS_ASSET: &str = "{build_ids.BUILD_IDS_ASSET}";' in source


def test_release_wheel_records_the_build_id_when_a_dwp_was_staged(
    monkeypatch, tmp_path: Path
) -> None:
    target = "x86_64-unknown-linux-gnu"
    wheel = _wheel(tmp_path, {"clud": elf64(_note(b"GNU\0", 3, BUILD_ID))})
    debuginfo = tmp_path / "dist-debuginfo"
    monkeypatch.setattr(xbuild, "DEBUGINFO_DIR", debuginfo)
    monkeypatch.setattr(xbuild, "build_env", lambda *_args: {})
    monkeypatch.setattr(xbuild, "run", lambda *_args, **_kwargs: 0)
    monkeypatch.setattr(build_wheel, "built_wheels", lambda: [wheel])
    monkeypatch.setattr(build_wheel, "prune_nonproduction_scripts", lambda _wheel, **_kw: False)
    monkeypatch.setattr(build_wheel, "remove_elf_debug_metadata", lambda *_a, **_kw: True)
    monkeypatch.setattr(build_wheel, "verify_no_elf_debug_sections", lambda _wheel: None)
    monkeypatch.setattr(build_wheel, "verify_wheel_scripts", lambda _wheel: 0)
    monkeypatch.setattr(xbuild, "verify_wheel_modes", lambda _wheel: 0)
    monkeypatch.setattr(wheel_repair, "repair_windows_gnu_wheel", lambda _wheel: None)
    monkeypatch.setattr(
        xbuild, "collect_debuginfo", lambda *_args: [debuginfo / f"clud-{target}.dwp"]
    )

    args = argparse.Namespace(target=target, strategy="soldr", profile="release")
    assert xbuild.cmd_wheel(args) == 0
    assert (debuginfo / f"clud-{target}.build-id").read_text() == f"{BUILD_ID.hex()}  {target}\n"


def test_the_release_job_combines_before_it_checksums() -> None:
    workflow = (
        Path(__file__).resolve().parent.parent / ".github" / "workflows" / "auto-release.yml"
    ).read_text(encoding="utf-8")
    combine_at = workflow.index("python -m ci.build_ids combine debuginfo")
    checksum_at = workflow.index("sha256sum )")
    assert combine_at < checksum_at
