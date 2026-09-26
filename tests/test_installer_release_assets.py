"""Standalone clud release asset generation."""

from __future__ import annotations

from io import BytesIO
from zipfile import ZipFile

from installer.catalog import TARGETS
from installer.release_assets import extract


def test_release_assets_extract_one_clud_per_target(tmp_path) -> None:
    wheels = tmp_path / "wheels"
    wheels.mkdir()
    for platform, (os_name, _, executable) in TARGETS.items():
        signature = (
            b"MZ"
            if os_name == "windows"
            else (b"\xcf\xfa\xed\xfe" if os_name == "darwin" else b"\x7fELF")
        )
        data = BytesIO()
        with ZipFile(data, "w") as archive:
            archive.writestr(f"clud-2.9.0.data/scripts/{executable}", signature + platform.encode())
        (wheels / f"clud-2.9.0-py3-none-{platform}.whl").write_bytes(data.getvalue())
    output = tmp_path / "standalone"
    paths = extract(wheels, output, "2.9.0")
    assert len(paths) == 6
    assert all(path.read_bytes() for path in paths)
    assert sum(path.suffix == ".exe" for path in paths) == 2

