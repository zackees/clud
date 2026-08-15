"""Bundle the separately-built web terminal next to clud in desktop wheels."""

from __future__ import annotations

import base64
import hashlib
import tempfile
import zipfile
from pathlib import Path


def desktop_target(target: str) -> bool:
    return "windows" in target or "apple-darwin" in target


def companion_name(target: str) -> str:
    return "clud-webterm.exe" if "windows" in target else "clud-webterm"


def _record_line(name: str, data: bytes) -> str:
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
    return f"{name},sha256={digest},{len(data)}"


def add_companion(wheel: Path, companion: Path, target: str) -> None:
    """Atomically add a companion script and regenerate the wheel RECORD."""
    if not companion.is_file():
        raise RuntimeError(f"web terminal binary is missing: {companion}")
    with zipfile.ZipFile(wheel) as source:
        entries = [
            (info.filename, source.read(info.filename), info.external_attr)
            for info in source.infolist()
            if not info.filename.endswith(".dist-info/RECORD")
            and not info.filename.endswith(f".data/scripts/{companion_name(target)}")
        ]
    dist_info = next(
        (name.split("/", 1)[0] for name, _, _ in entries if name.endswith(".dist-info/WHEEL")),
        None,
    )
    if dist_info is None:
        raise RuntimeError(f"wheel has no dist-info/WHEEL entry: {wheel}")
    script = f"{dist_info.removesuffix('.dist-info')}.data/scripts/{companion_name(target)}"
    external_attr = (0o755 << 16) if "windows" not in target else 0
    entries.append((script, companion.read_bytes(), external_attr))
    records = [_record_line(name, data) for name, data, _ in entries]
    record = f"{dist_info}/RECORD"
    records.append(f"{record},,")
    with tempfile.NamedTemporaryFile(dir=wheel.parent, suffix=".whl", delete=False) as handle:
        temporary = Path(handle.name)
    try:
        with zipfile.ZipFile(temporary, "w", compression=zipfile.ZIP_DEFLATED) as output:
            for name, data, external_attr in entries:
                info = zipfile.ZipInfo(name)
                info.external_attr = external_attr
                output.writestr(info, data)
            output.writestr(record, "\n".join(records) + "\n")
        temporary.replace(wheel)
    finally:
        temporary.unlink(missing_ok=True)


def wheel_has_companion(wheel: Path, target: str) -> bool:
    needle = f".data/scripts/{companion_name(target)}"
    with zipfile.ZipFile(wheel) as archive:
        return any(name.endswith(needle) for name in archive.namelist())
