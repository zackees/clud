from __future__ import annotations

import base64
import csv
import hashlib
import os
import shutil
import tempfile
import zipfile
from pathlib import Path, PurePosixPath

_LIBSTDCPP = "libstdc++-6.dll"
_LIBGCC_CANDIDATES = (
    "libgcc_s_seh-1.dll",
    "libgcc_s_dw2-1.dll",
    "libgcc_s_sjlj-1.dll",
)
_LIBWINPTHREAD = "libwinpthread-1.dll"


def repair_windows_gnu_wheel(wheel: Path) -> bool:
    if os.name != "nt":
        return False
    if not wheel.is_file():
        return False

    runtime_dlls = find_windows_gnu_runtime_dlls()
    if not runtime_dlls:
        return False

    with zipfile.ZipFile(wheel) as archive:
        members = archive.namelist()
        script_dir = _find_scripts_dir(members)
        record_path = _find_record_path(members)
        if script_dir is None or record_path is None:
            return False

        with tempfile.TemporaryDirectory(prefix="clud-wheel-repair-") as temp_dir:
            root = Path(temp_dir)
            archive.extractall(root)

            target_dir = root / Path(*script_dir.parts)
            target_dir.mkdir(parents=True, exist_ok=True)
            for dll in runtime_dlls:
                shutil.copy2(dll, target_dir / dll.name)

            _rewrite_record(root, record_path)
            repaired = wheel.with_suffix(".repaired.whl")
            _write_wheel(root, repaired)

    repaired.replace(wheel)
    return True


def find_windows_gnu_runtime_dlls() -> list[Path]:
    runtime_dir = _find_windows_gnu_runtime_dir()
    if runtime_dir is None:
        return []

    dlls = [runtime_dir / _LIBSTDCPP]
    gcc_dll = next(
        (runtime_dir / name for name in _LIBGCC_CANDIDATES if (runtime_dir / name).is_file()),
        None,
    )
    if gcc_dll is not None:
        dlls.append(gcc_dll)
    winpthread_dll = runtime_dir / _LIBWINPTHREAD
    if winpthread_dll.is_file():
        dlls.append(winpthread_dll)
    return [dll for dll in dlls if dll.is_file()]


def _find_windows_gnu_runtime_dir() -> Path | None:
    path_entries = [Path(entry) for entry in os.environ.get("PATH", "").split(os.pathsep) if entry]
    candidates = [
        *path_entries,
        Path(r"C:\msys64\ucrt64\bin"),
        Path(r"C:\msys64\mingw64\bin"),
        Path(r"C:\Qt\Tools\mingw1120_64\bin"),
        Path(r"C:\MinGW\bin"),
    ]
    seen: set[str] = set()
    for candidate in candidates:
        normalized = os.path.normcase(os.path.normpath(str(candidate)))
        if normalized in seen:
            continue
        seen.add(normalized)
        if (candidate / _LIBSTDCPP).is_file():
            return candidate
    return None


def _find_scripts_dir(members: list[str]) -> PurePosixPath | None:
    for member in members:
        path = PurePosixPath(member)
        if path.name == "clud.exe" and len(path.parts) >= 3 and path.parts[-2] == "scripts":
            return path.parent
    return None


def _find_record_path(members: list[str]) -> PurePosixPath | None:
    for member in members:
        path = PurePosixPath(member)
        if path.name == "RECORD" and len(path.parts) >= 2 and path.parts[-2].endswith(".dist-info"):
            return path
    return None


def _rewrite_record(root: Path, record_path: PurePosixPath) -> None:
    record_file = root / Path(*record_path.parts)
    rows: list[tuple[str, str, str]] = []
    for file_path in sorted(path for path in root.rglob("*") if path.is_file()):
        relative = file_path.relative_to(root).as_posix()
        if relative == record_path.as_posix():
            continue
        data = file_path.read_bytes()
        digest = (
            base64.urlsafe_b64encode(hashlib.sha256(data).digest())
            .rstrip(b"=")
            .decode("ascii")
        )
        rows.append((relative, f"sha256={digest}", str(len(data))))
    rows.append((record_path.as_posix(), "", ""))
    with record_file.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.writer(handle, lineterminator="\n")
        writer.writerows(rows)


def _write_wheel(root: Path, destination: Path) -> None:
    with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for file_path in sorted(path for path in root.rglob("*") if path.is_file()):
            archive.write(file_path, file_path.relative_to(root).as_posix())
