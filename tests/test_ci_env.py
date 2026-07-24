"""Tests for CI environment command selection."""

from __future__ import annotations

import os
from pathlib import Path

from ci import env as ci_env


def test_cargo_argv_prefers_explicit_cargo(monkeypatch) -> None:
    monkeypatch.setattr(ci_env, "soldr_path", lambda env=None: "soldr")

    assert ci_env.cargo_argv(["check"], env={"CARGO": r"C:\rust\bin\cargo.exe"}) == [
        r"C:\rust\bin\cargo.exe",
        "check",
    ]


def test_cargo_argv_uses_path_when_soldr_shims_requested() -> None:
    assert ci_env.cargo_argv(
        ["check"],
        env={"CARGO": r"C:\rust\bin\cargo.exe", "CLUD_USE_SOLDR_SHIMS": "1"},
    ) == ["cargo", "check"]


def test_activate_preserves_soldr_shims_ahead_of_cargo_bin(monkeypatch, tmp_path) -> None:
    cargo_home = tmp_path / "cargo"
    cargo_bin = cargo_home / "bin"
    cargo_bin.mkdir(parents=True)
    shims_dir = tmp_path / "setup-soldr" / "shims"
    shims_dir.mkdir(parents=True)

    monkeypatch.setenv("CARGO_HOME", str(cargo_home))
    monkeypatch.setenv("CLUD_USE_SOLDR_SHIMS", "1")
    monkeypatch.setenv("PATH", str(shims_dir))

    ci_env.activate()

    path_parts = os.environ["PATH"].split(os.pathsep)
    assert path_parts[0] == str(shims_dir)
    assert path_parts[-1] == str(cargo_bin)


def test_activate_moves_existing_cargo_bin_behind_soldr_shims(monkeypatch, tmp_path) -> None:
    cargo_home = tmp_path / "cargo"
    cargo_bin = cargo_home / "bin"
    cargo_bin.mkdir(parents=True)
    shims_dir = tmp_path / "setup-soldr" / "shims"
    shims_dir.mkdir(parents=True)

    monkeypatch.setenv("CARGO_HOME", str(cargo_home))
    monkeypatch.setenv("CLUD_USE_SOLDR_SHIMS", "1")
    monkeypatch.setenv("PATH", os.pathsep.join([str(cargo_bin), str(shims_dir)]))

    ci_env.activate()

    path_parts = os.environ["PATH"].split(os.pathsep)
    assert path_parts == [str(shims_dir), str(cargo_bin)]


def test_cargo_argv_uses_bare_cargo_when_no_explicit_cargo(monkeypatch) -> None:
    monkeypatch.setattr(ci_env, "soldr_path", lambda env=None: "soldr")

    assert ci_env.cargo_argv(["check"], env={}) == ["cargo", "check"]


def test_cargo_argv_falls_back_to_bare_cargo(monkeypatch) -> None:
    monkeypatch.setattr(ci_env, "soldr_path", lambda env=None: None)

    assert ci_env.cargo_argv(["check"], env={}) == ["cargo", "check"]


def test_clean_env_never_auto_configures_sccache(monkeypatch) -> None:
    monkeypatch.setattr(ci_env, "activate", lambda: None)
    monkeypatch.setattr(ci_env.platform, "system", lambda: "Linux")
    monkeypatch.setattr(ci_env, "toolchain_name", lambda: "1.94.1")
    monkeypatch.delenv("RUSTC_WRAPPER", raising=False)
    monkeypatch.setattr(
        ci_env.shutil,
        "which",
        lambda command, **_kwargs: "/tools/sccache" if command == "sccache" else None,
    )

    assert "RUSTC_WRAPPER" not in ci_env.clean_env()


def test_vendored_whisper_disables_native_sccache_autodetection() -> None:
    build_script = (
        Path(__file__).resolve().parents[1] / "vendor" / "whisper-rs-sys" / "build.rs"
    ).read_text(encoding="utf-8")

    assert '.define("GGML_CCACHE", "OFF")' in build_script
