"""Focused tests for the bundled Docker recovery tool (issue #531)."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "docker" / "docker_recover.py"


@pytest.fixture
def recover():
    name = "clud_test_docker_recover"
    spec = importlib.util.spec_from_file_location(name, SCRIPT)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    try:
        yield module
    finally:
        sys.modules.pop(name, None)


def test_windows_storage_prefers_configured_custom_wsl_vhd(recover) -> None:
    active_vhd = r"E:\docker\wsl\disk\docker_data.vhdx"

    class FixtureProbe(recover.SystemDiskProbe):
        def exists(self, path: str) -> bool:
            return path.lower() == active_vhd.lower()

        def size_bytes(self, path: str) -> int | None:
            return 32 * 1024**3 if self.exists(path) else None

        def resolve_final(self, path: str) -> str:
            return path

        def recent_write(self, path: str, within_hours: float = 24.0) -> bool:
            return self.exists(path)

        def glob_vhdx(self, root: str) -> list[str]:
            return [active_vhd] if root.lower() == r"E:\docker\wsl".lower() else []

    resolution = recover.resolve_windows_docker_disks(
        {
            "CustomWslDistroDir": r"E:\docker\wsl",
            "DataFolder": r"C:\ProgramData\DockerDesktop\vm-data",
        },
        FixtureProbe(),
        localappdata=r"C:\Users\someone\AppData\Local",
    )

    assert resolution.chosen is not None
    assert resolution.chosen.path == active_vhd
    assert resolution.chosen.source == "CustomWslDistroDir"
    assert not resolution.used_fallback
