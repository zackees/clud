"""Tests for CI environment command selection."""

from __future__ import annotations

import os

from ci import env as ci_env


def test_cargo_argv_routes_through_soldr_when_no_shim(monkeypatch) -> None:
    """#1158: soldr present, no shim, explicit `CARGO` -> `soldr cargo`.

    This is the local developer. soldr's cache hardlinks `target/` artifacts
    read-only, and only soldr's own driver can rewrite them; a plain cargo
    (which is what an explicit `CARGO` names) fails with "not writeable".
    """
    monkeypatch.setattr(ci_env, "soldr_path", lambda env=None: "/opt/soldr")

    assert ci_env.cargo_argv(["check"], env={"CARGO": r"C:\rust\bin\cargo.exe"}) == [
        "/opt/soldr",
        "cargo",
        "check",
    ]


def test_cargo_argv_prefers_explicit_cargo_without_soldr(monkeypatch) -> None:
    monkeypatch.setattr(ci_env, "soldr_path", lambda env=None: None)

    assert ci_env.cargo_argv(["check"], env={"CARGO": r"C:\rust\bin\cargo.exe"}) == [
        r"C:\rust\bin\cargo.exe",
        "check",
    ]


def test_cargo_argv_prefers_soldr_even_with_shims(monkeypatch) -> None:
    monkeypatch.setattr(ci_env, "soldr_path", lambda env=None: "/opt/soldr")
    assert ci_env.cargo_argv(
        ["check"], env={"CARGO": r"C:\rust\bin\cargo.exe", "CLUD_USE_SOLDR_SHIMS": "1"}
    ) == ["/opt/soldr", "cargo", "check"]


def test_cargo_argv_uses_shim_when_driver_missing(monkeypatch) -> None:
    monkeypatch.setattr(ci_env, "soldr_path", lambda env=None: None)
    assert ci_env.cargo_argv(["check"], env={"CLUD_USE_SOLDR_SHIMS": "1"}) == ["cargo", "check"]


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


def test_cargo_argv_uses_soldr_when_no_explicit_cargo(monkeypatch) -> None:
    monkeypatch.setattr(ci_env, "soldr_path", lambda env=None: "soldr")

    assert ci_env.cargo_argv(["check"], env={}) == ["soldr", "cargo", "check"]


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


def test_host_triple_uses_soldr_when_available(monkeypatch) -> None:
    from types import SimpleNamespace

    monkeypatch.setattr(ci_env, "soldr_path", lambda env=None: "/opt/soldr")
    calls = []

    def run(argv, **kwargs):
        calls.append(argv)
        return SimpleNamespace(returncode=0, stdout="host: x86_64-unknown-linux-gnu\n")

    monkeypatch.setattr(ci_env.process, "run", run)
    assert ci_env._cargo_host_triple() == "x86_64-unknown-linux-gnu"
    assert calls == [["/opt/soldr", "cargo", "-Vv"]]
