from __future__ import annotations

import os
import zipfile
from pathlib import Path, PurePosixPath

from ci.wheel_rewrite import new_entry, rewrite_wheel

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

    rewrite_wheel(
        wheel,
        add=[
            new_entry(f"{script_dir.as_posix()}/{dll.name}", dll.read_bytes())
            for dll in runtime_dlls
        ],
    )
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
