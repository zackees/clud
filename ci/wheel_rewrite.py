"""The one wheel writer in ci/ (#1545).

Every post-processor that edits a built wheel goes through `rewrite_wheel`,
and the one builder that creates a wheel from scratch goes through
`write_wheel`. Nothing else in ci/ may open a zip for writing or call
`extractall` -- `ci/banned_wheel_writes.py` enforces that under `bash lint`.

Why one primitive: 2.8.14 and 2.8.20 shipped every `.data/scripts/*` binary
as 0644 (#1544) because each rewriter repacked the wheel its own way.
`ZipFile.extractall` drops Unix modes, and the repack then derived the mode
from the extracted file on disk. Here a kept or replaced entry always reuses
its source entry's metadata (mode, compression, date), and an added entry
must state its mode, with `.data/scripts/*` defaulting to 0755.
"""

from __future__ import annotations

import base64
import hashlib
import stat
import tempfile
import zipfile
from collections.abc import Callable, Iterable
from pathlib import Path

SCRIPT_MODE = 0o755

#: `transform(info, data)` returns the bytes to keep (the same or new) or
#: `None` to drop the entry. The entry keeps its original ZipInfo either way.
Transform = Callable[[zipfile.ZipInfo, bytes], bytes | None]


def is_script(name: str) -> bool:
    return ".data/scripts/" in name


def is_record(name: str) -> bool:
    return name.endswith(".dist-info/RECORD") and name.count("/") == 1


def record_line(name: str, data: bytes) -> str:
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
    return f"{name},sha256={digest},{len(data)}"


def new_entry(name: str, data: bytes, mode: int | None = None) -> tuple[zipfile.ZipInfo, bytes]:
    """An added entry with an explicit Unix mode (scripts default to 0755)."""
    if mode is None:
        if not is_script(name):
            raise ValueError(f"added wheel entry needs an explicit mode: {name}")
        mode = SCRIPT_MODE
    info = zipfile.ZipInfo(name)
    info.compress_type = zipfile.ZIP_DEFLATED
    info.create_system = 3  # Unix, so pip honours external_attr
    info.external_attr = (stat.S_IFREG | mode) << 16
    return info, data


def _copy_info(source: zipfile.ZipInfo) -> zipfile.ZipInfo:
    """Clone the metadata that matters, without stale sizes/offsets/extras."""
    info = zipfile.ZipInfo(source.filename, date_time=source.date_time)
    info.compress_type = source.compress_type
    info.external_attr = source.external_attr
    info.create_system = source.create_system
    info.comment = source.comment
    return info


def dist_info_dir(names: Iterable[str]) -> str:
    """The `<name>-<version>.dist-info` directory of a wheel's member names."""
    for name in names:
        if name.endswith(".dist-info/WHEEL") and name.count("/") == 1:
            return name.split("/", 1)[0]
    raise RuntimeError("wheel has no dist-info/WHEEL entry")


def wheel_dist_info_dir(wheel: Path) -> str:
    with zipfile.ZipFile(wheel) as archive:
        try:
            return dist_info_dir(archive.namelist())
        except RuntimeError as error:
            raise RuntimeError(f"{error}: {wheel}") from None


def _write(
    destination: Path,
    entries: list[tuple[zipfile.ZipInfo, bytes]],
    record_info: zipfile.ZipInfo,
    compresslevel: int | None = None,
) -> None:
    rows = [record_line(info.filename, data) for info, data in entries if not info.is_dir()]
    rows.append(f"{record_info.filename},,")
    record = ("\n".join(rows) + "\n").encode()
    with tempfile.NamedTemporaryFile(
        dir=destination.parent, suffix=".whl", delete=False
    ) as handle:
        temporary = Path(handle.name)
    try:
        with zipfile.ZipFile(
            temporary, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=compresslevel
        ) as output:
            for info, data in entries:
                output.writestr(info, data)
            output.writestr(record_info, record)
        temporary.replace(destination)
    finally:
        temporary.unlink(missing_ok=True)


def rewrite_wheel(
    wheel: Path,
    transform: Transform | None = None,
    *,
    add: Iterable[tuple[zipfile.ZipInfo, bytes]] = (),
    compresslevel: int | None = None,
) -> None:
    """Rewrite `wheel` in place entry by entry and regenerate RECORD once.

    `add` entries (build them with `new_entry`) replace any existing entry of
    the same name and are appended after the kept ones. `compresslevel` only
    trades size for speed (CI dev wheels pass 1); entry bytes are unchanged.
    """
    added = list(add)
    added_names = {info.filename for info, _ in added}
    kept: list[tuple[zipfile.ZipInfo, bytes]] = []
    record_info: zipfile.ZipInfo | None = None
    with zipfile.ZipFile(wheel) as source:
        names = source.namelist()
        for info in source.infolist():
            if is_record(info.filename):
                record_info = _copy_info(info)
                continue
            if info.filename in added_names:
                continue
            data = source.read(info)
            result = data if transform is None else transform(info, data)
            if result is None:
                continue
            kept.append((_copy_info(info), result))
    if record_info is None:
        record_name = f"{dist_info_dir([*names, *added_names])}/RECORD"
        record_info = new_entry(record_name, b"", 0o644)[0]
    _write(wheel, [*kept, *added], record_info, compresslevel)


def write_wheel(
    destination: Path, members: Iterable[tuple[str, bytes, int | None]]
) -> None:
    """Create a wheel from `(name, data, mode)` members; RECORD is generated."""
    entries = [new_entry(name, data, mode) for name, data, mode in members]
    record_name = f"{dist_info_dir(info.filename for info, _ in entries)}/RECORD"
    _write(destination, entries, new_entry(record_name, b"", 0o644)[0])
