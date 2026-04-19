"""Assert that a Windows wheel's clud.exe was built against MSVC, not MinGW.

Issue #27: the test harness and dev workflow pin the MSVC toolchain via
`ci/env.py::build_env()` and `soldr`, but nothing at the CI layer asserts
the resulting binary actually has only MSVC runtime imports. If the pin
ever slipped, we'd ship a wheel depending on `libstdc++-6.dll` /
`libgcc_s_seh-1.dll` / `libwinpthread-1.dll` — none of which ship with
Windows, so the binary would fail to start for any user who doesn't
happen to have a MinGW install on PATH.

This script opens a wheel, extracts `scripts/clud.exe`, and asserts its
PE import table has no MinGW runtime entries. It reads the PE headers
directly (no dumpbin / no VS tools needed), so it runs on any platform
with Python stdlib — useful for local verification as well as CI.

Usage:
    python -m ci.check_windows_wheel <wheel_path>
    python -m ci.check_windows_wheel --dist-dir dist/
"""

from __future__ import annotations

import argparse
import struct
import sys
import zipfile
from pathlib import Path

# MinGW runtime DLLs that MUST NOT appear in the import table of a clud.exe
# we ship. Exact casing is matched case-insensitively when scanning.
FORBIDDEN_DLL_PREFIXES = (
    "libstdc++",
    "libgcc_s",
    "libwinpthread",
)


def iter_imported_dll_names(pe_bytes: bytes) -> list[str]:
    """Return the list of DLL names in a PE file's import directory.

    Minimal PE parser — covers what we need to walk IMAGE_IMPORT_DESCRIPTOR
    and read the DLL name strings. Doesn't validate every PE field; a
    malformed binary just raises and fails the check loudly.
    """
    if pe_bytes[:2] != b"MZ":
        raise ValueError("not a PE/DOS executable (no MZ signature)")
    pe_offset = struct.unpack_from("<I", pe_bytes, 0x3C)[0]
    if pe_bytes[pe_offset : pe_offset + 4] != b"PE\x00\x00":
        raise ValueError("missing PE signature at header offset")

    coff_off = pe_offset + 4
    # IMAGE_FILE_HEADER: 20 bytes. We need SizeOfOptionalHeader to skip past
    # the optional header and find section headers.
    (_, _, _, _, _, size_of_optional_header, _) = struct.unpack_from(
        "<HHIIIHH", pe_bytes, coff_off
    )

    opt_off = coff_off + 20
    # Magic 0x10b = PE32, 0x20b = PE32+. The offset of the DataDirectories
    # and the size of RVA/Size entries are the same; what differs is where
    # NumberOfRvaAndSizes lives and the widths of some fields in between.
    magic = struct.unpack_from("<H", pe_bytes, opt_off)[0]
    if magic == 0x10B:
        num_rva_off = opt_off + 92
    elif magic == 0x20B:
        num_rva_off = opt_off + 108
    else:
        raise ValueError(f"unexpected optional header magic: 0x{magic:x}")
    num_rva_sizes = struct.unpack_from("<I", pe_bytes, num_rva_off)[0]
    data_dirs_off = num_rva_off + 4
    if num_rva_sizes < 2:
        return []  # no import directory
    import_rva, _import_size = struct.unpack_from("<II", pe_bytes, data_dirs_off + 8)
    if import_rva == 0:
        return []

    # Section headers start right after the optional header.
    num_sections = struct.unpack_from("<H", pe_bytes, coff_off + 2)[0]
    sections_off = opt_off + size_of_optional_header
    sections: list[tuple[int, int, int]] = []  # (virtual_address, virtual_size, raw_offset)
    for i in range(num_sections):
        header_off = sections_off + i * 40
        _name = pe_bytes[header_off : header_off + 8]
        virtual_size = struct.unpack_from("<I", pe_bytes, header_off + 8)[0]
        virtual_address = struct.unpack_from("<I", pe_bytes, header_off + 12)[0]
        _size_of_raw_data = struct.unpack_from("<I", pe_bytes, header_off + 16)[0]
        pointer_to_raw_data = struct.unpack_from("<I", pe_bytes, header_off + 20)[0]
        sections.append((virtual_address, virtual_size, pointer_to_raw_data))

    def rva_to_offset(rva: int) -> int | None:
        for va, size, raw in sections:
            if va <= rva < va + max(size, 1):
                return raw + (rva - va)
        return None

    import_table_off = rva_to_offset(import_rva)
    if import_table_off is None:
        return []

    names: list[str] = []
    descriptor = import_table_off
    while True:
        entry = pe_bytes[descriptor : descriptor + 20]
        if len(entry) < 20:
            break
        name_rva = struct.unpack_from("<I", entry, 12)[0]
        if name_rva == 0:
            break  # null descriptor terminates the table
        name_off = rva_to_offset(name_rva)
        if name_off is None:
            break
        end = pe_bytes.index(b"\x00", name_off)
        names.append(pe_bytes[name_off:end].decode("ascii", errors="replace"))
        descriptor += 20
    return names


def forbidden_imports(dll_names: list[str]) -> list[str]:
    hits: list[str] = []
    for name in dll_names:
        lowered = name.lower()
        for prefix in FORBIDDEN_DLL_PREFIXES:
            if lowered.startswith(prefix):
                hits.append(name)
                break
    return hits


def check_wheel(wheel_path: Path) -> list[str]:
    """Return a list of error messages; empty list means the wheel is clean."""
    errors: list[str] = []
    with zipfile.ZipFile(wheel_path) as archive:
        exe_members = [
            name
            for name in archive.namelist()
            if name.endswith("/clud.exe") or name.endswith("\\clud.exe")
        ]
        if not exe_members:
            # Not a Windows wheel — skip silently; this script is for .whl
            # files that actually carry clud.exe.
            return []
        for member in exe_members:
            try:
                pe_bytes = archive.read(member)
                names = iter_imported_dll_names(pe_bytes)
            except Exception as exc:
                errors.append(f"{wheel_path.name}::{member}: failed to parse PE: {exc}")
                continue
            bad = forbidden_imports(names)
            if bad:
                errors.append(
                    f"{wheel_path.name}::{member} has forbidden MinGW imports: {bad}. "
                    f"Full import list: {names}"
                )
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Verify built Windows wheels don't depend on MinGW runtime DLLs"
    )
    parser.add_argument("wheels", nargs="*", help="wheel paths to check")
    parser.add_argument(
        "--dist-dir",
        type=Path,
        default=None,
        help="check every *.whl in this directory",
    )
    args = parser.parse_args(argv)

    paths: list[Path] = [Path(p) for p in args.wheels]
    if args.dist_dir:
        paths.extend(sorted(args.dist_dir.glob("*.whl")))
    # Dedupe while preserving order.
    seen: set[str] = set()
    unique: list[Path] = []
    for path in paths:
        key = str(path.resolve())
        if key not in seen:
            seen.add(key)
            unique.append(path)
    if not unique:
        print("no wheels to check", file=sys.stderr)
        return 0

    all_errors: list[str] = []
    for wheel in unique:
        if not wheel.is_file():
            all_errors.append(f"{wheel}: not a file")
            continue
        print(f"checking {wheel.name}", file=sys.stderr)
        all_errors.extend(check_wheel(wheel))

    if all_errors:
        print("FAIL: Windows wheel check found MinGW imports:", file=sys.stderr)
        for error in all_errors:
            print(f"  {error}", file=sys.stderr)
        return 1
    print("OK: no MinGW imports in any checked wheel", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
