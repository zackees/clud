"""Executable identity guard for the legacy block-bad-cmd Python shim."""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path


def _load_shim():
    source = (
        Path(__file__).resolve().parents[1]
        / "crates/clud-bin/assets/tools/hooks/block-bad-cmd.py"
    )
    spec = importlib.util.spec_from_file_location("clud_block_bad_cmd_shim", source)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_legacy_shim_runs_the_launching_clud_not_a_path_stub(tmp_path: Path, monkeypatch) -> None:
    """#1551: the scanner is `clud __cmd-scan`; a PATH `clud-cmd-scan` is never used."""
    shim = _load_shim()
    install = tmp_path / "install"
    poison = tmp_path / "poison"
    install.mkdir()
    poison.mkdir()
    launching = install / ("clud.exe" if os.name == "nt" else "clud")
    launching.touch()
    (poison / ("clud-cmd-scan.exe" if os.name == "nt" else "clud-cmd-scan")).touch()
    monkeypatch.setenv("CLUD_EXE", str(launching))
    monkeypatch.setenv("PATH", str(poison))

    assert shim._native_path() == launching


def test_legacy_shim_never_falls_back_to_path(tmp_path: Path, monkeypatch) -> None:
    shim = _load_shim()
    poison = tmp_path / "poison"
    poison.mkdir()
    (poison / ("clud-cmd-scan.exe" if os.name == "nt" else "clud-cmd-scan")).touch()
    monkeypatch.delenv("CLUD_EXE", raising=False)
    monkeypatch.setenv("PATH", str(poison))

    assert shim._native_path() is None
    monkeypatch.setenv("CLUD_EXE", str(tmp_path / "missing" / "clud"))
    assert shim._native_path() is None
