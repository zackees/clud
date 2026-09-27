"""Check the six direct release downloads before hashing and publication."""

from __future__ import annotations

import hashlib
import sys
from pathlib import Path

from installer.catalog import verify_direct_executable

TARGETS = {
    "x86_64-pc-windows-msvc.exe": ("pe", "x86_64"),
    "aarch64-pc-windows-msvc.exe": ("pe", "aarch64"),
    "x86_64-apple-darwin": ("macho", "x86_64"),
    "aarch64-apple-darwin": ("macho", "aarch64"),
    "x86_64-unknown-linux-musl": ("elf", "x86_64"),
    "aarch64-unknown-linux-musl": ("elf", "aarch64"),
}


def verify_format(data: bytes, kind: str, arch: str) -> None:
    os_name = {"elf": "linux", "pe": "windows", "macho": "darwin"}.get(kind)
    if os_name is None:
        raise ValueError(f"unknown native format: {kind}")
    verify_direct_executable(data, os_name, arch, "static-musl" if kind == "elf" else None)


def verify(directory: Path, version: str) -> dict[str, tuple[int, str]]:
    expected = {
        f"clud-{version}-{suffix}": (kind, arch)
        for suffix, (kind, arch) in TARGETS.items()
    }
    actual = {path.name for path in directory.iterdir()}
    if actual != set(expected):
        missing = set(expected) - actual
        extra = actual - set(expected)
        raise ValueError(f"native asset set mismatch: missing={missing}, extra={extra}")
    results = {}
    for name, (kind, arch) in expected.items():
        path = directory / name
        if not path.is_file() or path.is_symlink():
            raise ValueError(f"native asset is not a regular file: {name}")
        data = path.read_bytes()
        verify_format(data, kind, arch)
        results[name] = len(data), hashlib.sha256(data).hexdigest()
        print(f"{name}: {results[name][0]} bytes sha256={results[name][1]} arch={arch}")
    return results


def verify_one(directory: Path, version: str, target: str) -> tuple[int, str]:
    """Verify the exact uploaded musl artifact before native ARM execution."""
    if target not in ("x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"):
        raise ValueError(f"unsupported standalone target: {target}")
    path = directory / f"clud-{version}-{target}"
    if not path.is_file() or path.is_symlink():
        raise ValueError(f"missing standalone asset: {path.name}")
    data = path.read_bytes()
    verify_format(data, "elf", TARGETS[target][1])
    digest = hashlib.sha256(data).hexdigest()
    print(f"{path.name}: {len(data)} bytes sha256={digest}")
    return len(data), digest


if __name__ == "__main__":
    if len(sys.argv) == 4:
        verify_one(Path(sys.argv[2]), sys.argv[1], sys.argv[3])
    else:
        verify(Path(sys.argv[2]), sys.argv[1])
