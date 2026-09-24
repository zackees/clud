"""Package the pinned Windows WezTerm fork without losing its DLL layout.

The GUI and its ConPTY/graphics support files must share an installed directory.
Keeping them under wheel ``.data/data/clud-kittyterm/`` also avoids shadowing
an independently installed WezTerm on ``PATH``.
"""

from __future__ import annotations

import base64
import csv
import hashlib
import io
import os
import struct
import tempfile
import zipfile
from pathlib import Path

# Mirrors the runtime portion of the fork's Windows portable release. PDBs are
# deliberately excluded; license notices and source provenance are mandatory.
KITTY_SOURCE_REVISION = "29a4208689ed349a3e983537030c837b4a9663be"
KITTY_BUNDLE_ENV = "CLUD_KITTYTERM_BUNDLE_DIR"
KITTY_TARGET = "x86_64-pc-windows-msvc"
KITTY_PE_MACHINE = 0x8664  # IMAGE_FILE_MACHINE_AMD64
KITTY_BUNDLE_FILES = (
    "wezterm.exe",
    "wezterm-gui.exe",
    "wezterm-mux-server.exe",
    "strip-ansi-escapes.exe",
    "conpty.dll",
    "OpenConsole.exe",
    "libEGL.dll",
    "libGLESv2.dll",
    "mesa/opengl32.dll",
    "LICENSE.md",
    "LICENSE_OFL.txt",
    "LICENSE_POWERLINE_EXTRA.txt",
    "THIRD_PARTY_NOTICES.md",
    "third-party/conhost/README.md",
    "third-party/mesa/README.md",
    "third-party/ANGLE-LICENSE.txt",
    "third-party/MICROSOFT-TERMINAL-LICENSE.txt",
    "SOURCE_REVISION",
)
KITTY_PE_FILES = tuple(
    name for name in KITTY_BUNDLE_FILES if name.lower().endswith((".exe", ".dll"))
)
KITTY_PASTE_HELPER = "clud-kittyterm-paste.exe"
KITTY_GUI_REQUIRED_OPTION = b"return-initial-exit-code"


def _pe_machine(data: bytes) -> int:
    if len(data) < 0x40 or data[:2] != b"MZ":
        raise ValueError("missing DOS header")
    offset = struct.unpack_from("<I", data, 0x3C)[0]
    if offset + 6 > len(data) or data[offset : offset + 4] != b"PE\0\0":
        raise ValueError("missing PE header")
    return struct.unpack_from("<H", data, offset + 4)[0]


def check_kitty_wheel(wheel: Path) -> list[str]:
    """Validate the portable layout and every bundled member's RECORD entry."""
    errors: list[str] = []
    with zipfile.ZipFile(wheel) as archive:
        names = set(archive.namelist())
        if wheel.name.endswith("-win_arm64.whl"):
            if any(
                f".data/{scheme}/clud-kittyterm/" in name
                for name in names
                for scheme in ("data", "scripts")
            ):
                return [f"{wheel.name}: x64 Kitty GUI bundle is forbidden in ARM64 wheel"]
            return []
        if any(".data/scripts/clud-kittyterm/" in name for name in names):
            errors.append(f"{wheel.name}: Kitty bundle cannot be under wheel scripts directory")
        dist_info = next(
            (n.split("/", 1)[0] for n in names if n.endswith(".dist-info/WHEEL")), None
        )
        if dist_info is None:
            return [f"{wheel.name}: missing dist-info/WHEEL"]
        prefix = f"{dist_info.removesuffix('.dist-info')}.data/data/clud-kittyterm/"
        record_name = f"{dist_info}/RECORD"
        if record_name not in names:
            return [f"{wheel.name}: missing RECORD"]
        rows = {
            row[0]: row[1:]
            for row in csv.reader(io.StringIO(archive.read(record_name).decode()))
        }
        for name in (*KITTY_BUNDLE_FILES, "clud-kittyterm.lua", KITTY_PASTE_HELPER):
            member = prefix + name
            if member not in names:
                errors.append(f"{wheel.name}: missing {member}")
                continue
            data = archive.read(member)
            if name == "wezterm-gui.exe" and KITTY_GUI_REQUIRED_OPTION not in data:
                errors.append(
                    f"{wheel.name}: {member} lacks --return-initial-exit-code support"
                )
            if name in KITTY_PE_FILES or name == KITTY_PASTE_HELPER:
                try:
                    machine = _pe_machine(data)
                except ValueError as error:
                    errors.append(f"{wheel.name}: invalid PE for {member}: {error}")
                else:
                    if machine != KITTY_PE_MACHINE:
                        errors.append(f"{wheel.name}: non-x64 PE for {member}: 0x{machine:04x}")
            expected = _record_line(member, data).split(",", 1)[1].split(",")
            if rows.get(member) != expected:
                errors.append(f"{wheel.name}: invalid RECORD for {member}")
        source = prefix + "SOURCE_REVISION"
        if source in names:
            expected_revision = f"zackees/wezterm@{KITTY_SOURCE_REVISION}"
            if archive.read(source).decode().strip() != expected_revision:
                errors.append(f"{wheel.name}: wrong SOURCE_REVISION")
    return errors


def _record_line(name: str, data: bytes) -> str:
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
    return f"{name},sha256={digest},{len(data)}"


def resolve_kitty_bundle() -> Path:
    """Find the exact portable build supplied by the CI or local caller."""
    configured = os.environ.get(KITTY_BUNDLE_ENV)
    if not configured:
        raise RuntimeError(
            f"Windows Kitty wheel requires {KITTY_BUNDLE_ENV} pointing to the "
            f"zackees/wezterm portable build at {KITTY_SOURCE_REVISION}"
        )
    bundle = Path(configured)
    if not bundle.is_dir():
        raise RuntimeError(f"{KITTY_BUNDLE_ENV} is not a directory: {bundle}")
    validate_kitty_bundle(bundle)
    return bundle


def validate_kitty_bundle(bundle: Path) -> None:
    """Reject incomplete or unpinned portable builds before wheel creation."""
    for name in KITTY_BUNDLE_FILES:
        if not (bundle / name).is_file():
            raise RuntimeError(f"Kitty GUI bundle is missing {name}: {bundle}")
    for name in KITTY_PE_FILES:
        data = (bundle / name).read_bytes()
        try:
            machine = _pe_machine(data)
        except ValueError as error:
            raise RuntimeError(f"Kitty GUI bundle has invalid PE {name}: {error}") from error
        if machine != KITTY_PE_MACHINE:
            raise RuntimeError(f"Kitty GUI bundle has non-x64 PE {name}: 0x{machine:04x}")
        if name == "wezterm-gui.exe" and KITTY_GUI_REQUIRED_OPTION not in data:
            raise RuntimeError(
                "Kitty GUI bundle wezterm-gui.exe lacks --return-initial-exit-code support"
            )
    expected_revision = f"zackees/wezterm@{KITTY_SOURCE_REVISION}"
    actual_revision = (bundle / "SOURCE_REVISION").read_text(encoding="utf-8").strip()
    if actual_revision != expected_revision:
        raise RuntimeError(
            f"Kitty GUI SOURCE_REVISION must be {expected_revision}; got {actual_revision!r}"
        )


def read_kitty_paste_helper(paste_helper: Path | None) -> bytes:
    """Require the soldr-built x64 helper and return its validated bytes."""
    if paste_helper is None or not paste_helper.is_file():
        raise RuntimeError(f"Kitty paste helper is missing: {paste_helper or KITTY_PASTE_HELPER}")
    data = paste_helper.read_bytes()
    try:
        machine = _pe_machine(data)
    except ValueError as error:
        raise RuntimeError(f"Kitty paste helper has invalid PE: {error}") from error
    if machine != KITTY_PE_MACHINE:
        raise RuntimeError(f"Kitty paste helper has non-x64 PE: 0x{machine:04x}")
    return data


def add_kitty_bundle(
    wheel: Path, bundle: Path, config: Path, target: str, paste_helper: Path | None = None
) -> None:
    """Atomically add the Windows GUI bundle and regenerate wheel RECORD.

    The caller must produce ``bundle`` from the pinned fork revision. This
    function requires the exact checked-in fork revision in SOURCE_REVISION;
    CI remains responsible for obtaining the artifact from that revision.
    """
    if target != KITTY_TARGET:
        raise ValueError(f"Kitty GUI bundle supports only {KITTY_TARGET}: {target}")
    validate_kitty_bundle(bundle)
    if not config.is_file():
        raise RuntimeError(f"Kitty GUI config is missing: {config}")
    helper_data = read_kitty_paste_helper(paste_helper)

    with zipfile.ZipFile(wheel) as source:
        entries = [
            (info, source.read(info.filename))
            for info in source.infolist()
            if not info.filename.endswith(".dist-info/RECORD")
            and ".data/data/clud-kittyterm/" not in info.filename
            and ".data/scripts/clud-kittyterm/" not in info.filename
        ]
    dist_info = next(
        (
            info.filename.split("/", 1)[0]
            for info, _ in entries
            if info.filename.endswith(".dist-info/WHEEL")
        ),
        None,
    )
    if dist_info is None:
        raise RuntimeError(f"wheel has no dist-info/WHEEL entry: {wheel}")
    prefix = f"{dist_info.removesuffix('.dist-info')}.data/data/clud-kittyterm/"

    for name in KITTY_BUNDLE_FILES:
        info = zipfile.ZipInfo(prefix + name)
        info.compress_type = zipfile.ZIP_DEFLATED
        entries.append((info, (bundle / name).read_bytes()))
    info = zipfile.ZipInfo(prefix + "clud-kittyterm.lua")
    info.compress_type = zipfile.ZIP_DEFLATED
    entries.append((info, config.read_bytes()))
    info = zipfile.ZipInfo(prefix + KITTY_PASTE_HELPER)
    info.compress_type = zipfile.ZIP_DEFLATED
    entries.append((info, helper_data))

    record_name = f"{dist_info}/RECORD"
    record_lines = [_record_line(info.filename, data) for info, data in entries]
    record = "\n".join([*record_lines, f"{record_name},,"]) + "\n"
    with tempfile.NamedTemporaryFile(dir=wheel.parent, suffix=".whl", delete=False) as handle:
        temporary = Path(handle.name)
    try:
        with zipfile.ZipFile(temporary, "w") as output:
            for info, data in entries:
                output.writestr(info, data)
            output.writestr(record_name, record)
        temporary.replace(wheel)
    finally:
        temporary.unlink(missing_ok=True)
