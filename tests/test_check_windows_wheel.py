"""Unit tests for ci/check_windows_wheel.py.

The checker parses real PE binaries; to avoid shelling out to cargo here,
the tests synthesize two in-memory wheels and run them through the same
entry point the CI step uses.
"""

from __future__ import annotations

import struct
import zipfile
from pathlib import Path

from ci.check_windows_wheel import check_wheel, forbidden_imports, iter_imported_dll_names

# A minimal PE32+ executable synthesized in-memory. The layout is chosen
# so the import table is parseable but still tiny; the test doesn't care
# about anything other than the import directory content.
DOS_STUB = b"MZ" + b"\x00" * 0x3A + struct.pack("<I", 0x40)
PE_SIG = b"PE\x00\x00"


def _make_pe_with_imports(dll_names: list[str]) -> bytes:
    """Build a bare-minimum PE32+ binary whose import table lists the given DLLs.

    Only the fields the checker reads are filled in: headers, import directory
    RVA + size, section table, and the embedded DLL name strings. Everything
    else is zeroed.
    """
    # Section layout: headers at start, one .idata section containing the
    # import descriptors + name strings, mapped at a fixed RVA.
    section_rva = 0x1000
    # Layout of the .idata section content:
    #   [IMAGE_IMPORT_DESCRIPTOR x (N+1)]  -- N descriptors + terminating null
    #   [dll name string x N]              -- zero-terminated ASCII
    import_desc_size = 20
    num_descriptors = len(dll_names) + 1  # +1 null terminator
    desc_table_size = num_descriptors * import_desc_size
    name_offsets_in_section: list[int] = []
    name_blob = bytearray()
    cursor = desc_table_size
    for name in dll_names:
        name_offsets_in_section.append(cursor)
        encoded = name.encode("ascii") + b"\x00"
        name_blob.extend(encoded)
        cursor += len(encoded)

    section_content = bytearray(b"\x00" * desc_table_size) + name_blob
    for i, name_offset in enumerate(name_offsets_in_section):
        descriptor_offset = i * import_desc_size
        # Field layout (20 bytes): OriginalFirstThunk(4), TimeDateStamp(4),
        # ForwarderChain(4), Name(4 RVA), FirstThunk(4). Only Name matters
        # here.
        struct.pack_into(
            "<IIIII",
            section_content,
            descriptor_offset,
            0,  # OriginalFirstThunk
            0,  # TimeDateStamp
            0,  # ForwarderChain
            section_rva + name_offset,  # Name RVA (absolute)
            0,  # FirstThunk
        )

    # File layout with raw offsets the checker can rva_to_offset.
    # DOS header + PE sig + COFF header + optional header + section header.
    pe_header_offset = 0x40
    coff_offset = pe_header_offset + 4
    optional_header_size = 240  # PE32+ has 240 bytes before NumberOfRvaAndSizes
    opt_offset = coff_offset + 20
    section_header_offset = opt_offset + optional_header_size
    section_raw_offset = 0x200  # start of section data in file

    total_size = section_raw_offset + len(section_content)
    pe = bytearray(b"\x00" * total_size)
    pe[0:2] = b"MZ"
    struct.pack_into("<I", pe, 0x3C, pe_header_offset)
    pe[pe_header_offset : pe_header_offset + 4] = PE_SIG

    # COFF header: Machine(2), NumberOfSections(2), TimeDateStamp(4),
    # PointerToSymbolTable(4), NumberOfSymbols(4), SizeOfOptionalHeader(2),
    # Characteristics(2).
    struct.pack_into(
        "<HHIIIHH",
        pe,
        coff_offset,
        0x8664,  # AMD64
        1,  # NumberOfSections
        0,
        0,
        0,
        optional_header_size,
        0,
    )
    # Optional header magic (PE32+).
    struct.pack_into("<H", pe, opt_offset, 0x20B)
    # NumberOfRvaAndSizes is at opt_offset + 108 for PE32+.
    struct.pack_into("<I", pe, opt_offset + 108, 16)
    # Data directories start right after. Index 1 = Import Table.
    data_dirs_start = opt_offset + 112
    struct.pack_into(
        "<II",
        pe,
        data_dirs_start + 8,  # dir[1]
        section_rva,  # Import Table RVA
        desc_table_size,  # Import Table Size
    )
    # Section header (40 bytes).
    section_name = b".idata\x00\x00"
    pe[section_header_offset : section_header_offset + 8] = section_name
    struct.pack_into("<I", pe, section_header_offset + 8, len(section_content))  # VirtualSize
    struct.pack_into("<I", pe, section_header_offset + 12, section_rva)  # VirtualAddress
    struct.pack_into("<I", pe, section_header_offset + 16, len(section_content))  # SizeOfRawData
    struct.pack_into(
        "<I", pe, section_header_offset + 20, section_raw_offset
    )  # PointerToRawData

    # Section content.
    pe[section_raw_offset : section_raw_offset + len(section_content)] = section_content
    return bytes(pe)


def _make_wheel(tmp_path: Path, wheel_name: str, imports: list[str]) -> Path:
    exe_bytes = _make_pe_with_imports(imports)
    wheel_path = tmp_path / wheel_name
    with zipfile.ZipFile(wheel_path, "w") as archive:
        archive.writestr("clud-2.0.0.data/scripts/clud.exe", exe_bytes)
        archive.writestr("clud-2.0.0.dist-info/RECORD", "")
    return wheel_path


def test_parses_imports_from_synthetic_pe():
    pe = _make_pe_with_imports(["VCRUNTIME140.dll", "KERNEL32.dll"])
    names = iter_imported_dll_names(pe)
    assert "VCRUNTIME140.dll" in names
    assert "KERNEL32.dll" in names


def test_clean_wheel_passes(tmp_path: Path):
    wheel = _make_wheel(tmp_path, "clud-2.0.0-py3-none-win_amd64.whl", ["VCRUNTIME140.dll"])
    assert check_wheel(wheel) == []


def test_mingw_wheel_flags_every_forbidden_dll(tmp_path: Path):
    wheel = _make_wheel(
        tmp_path,
        "clud-2.0.0-py3-none-win_amd64.whl",
        ["libstdc++-6.dll", "libgcc_s_seh-1.dll", "libwinpthread-1.dll", "KERNEL32.dll"],
    )
    errors = check_wheel(wheel)
    assert len(errors) == 1
    msg = errors[0]
    assert "libstdc++-6.dll" in msg
    assert "libgcc_s_seh-1.dll" in msg
    assert "libwinpthread-1.dll" in msg
    # Non-MinGW imports aren't flagged by themselves.
    assert "KERNEL32.dll" not in forbidden_imports(["KERNEL32.dll"])


def test_non_windows_wheel_is_skipped(tmp_path: Path):
    wheel = tmp_path / "clud-2.0.0-py3-none-manylinux_2_17_x86_64.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        archive.writestr("clud-2.0.0.data/scripts/clud", b"not-a-pe-binary")
        archive.writestr("clud-2.0.0.dist-info/RECORD", "")
    assert check_wheel(wheel) == []


def test_forbidden_imports_is_case_insensitive():
    assert forbidden_imports(["LIBSTDC++-6.DLL"]) == ["LIBSTDC++-6.DLL"]
    assert forbidden_imports(["libgcc_s_dw2-1.dll"]) == ["libgcc_s_dw2-1.dll"]
    assert forbidden_imports(["libgcc_s_sjlj-1.dll"]) == ["libgcc_s_sjlj-1.dll"]
    assert forbidden_imports(["MSVCP140.dll"]) == []
