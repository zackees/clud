from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))
SPEC = importlib.util.spec_from_file_location("build_backend", ROOT / "build_backend.py")
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("failed to load build_backend.py")
build_backend = importlib.util.module_from_spec(SPEC)
sys.modules["build_backend"] = build_backend
SPEC.loader.exec_module(build_backend)


def test_build_wheel_routes_through_soldr_and_repairs(monkeypatch, tmp_path) -> None:
    calls = []
    repairs = []
    soldr = tmp_path / build_backend._script_name("soldr")

    monkeypatch.setattr(build_backend, "build_env", lambda: {"PATH": "test-bin"})
    monkeypatch.setattr(build_backend, "_soldr_executable", lambda: str(soldr))

    def fake_check_call(cmd, *, env, timeout):
        calls.append((cmd, env, timeout))
        (tmp_path / "clud-1.0.0-py3-none-any.whl").write_text("", encoding="utf-8")

    monkeypatch.setattr(build_backend.subprocess, "check_call", fake_check_call)
    monkeypatch.setattr(build_backend, "repair_windows_gnu_wheel", repairs.append)

    filename = build_backend.build_wheel(str(tmp_path))

    assert filename == "clud-1.0.0-py3-none-any.whl"
    assert repairs == [tmp_path / filename]
    assert calls == [
        (
            [
                str(soldr),
                "maturin",
                "pep517",
                "build-wheel",
                "--out",
                str(tmp_path),
                "--compatibility",
                "off",
                "--interpreter",
                sys.executable,
            ],
            {
                "PATH": str(tmp_path) + build_backend.os.pathsep + "test-bin",
                "ZCCACHE_PATH_REMAP": "auto",
                "CARGO_TARGET_DIR": str(Path.home() / ".soldr" / "cargo-target" / "wheel-build"),
            },
            build_backend._SOLDR_PEP517_TIMEOUT_SECONDS,
        )
    ]


def test_soldr_env_preserves_explicit_cache_settings(monkeypatch, tmp_path) -> None:
    target_dir = tmp_path / "target"
    monkeypatch.setattr(
        build_backend,
        "build_env",
        lambda: {
            "CARGO_TARGET_DIR": str(target_dir),
            "RUSTC_WRAPPER": "custom-wrapper",
            "SOLDR_PEP517_STABLE_TARGET_DIR": "0",
            "ZCCACHE_PATH_REMAP": "manual",
        },
    )

    env = build_backend._soldr_build_env()

    assert env["CARGO_TARGET_DIR"] == str(target_dir)
    assert env["RUSTC_WRAPPER"] == "custom-wrapper"
    assert env["ZCCACHE_PATH_REMAP"] == "manual"


def test_soldr_executable_prefers_build_env_script(monkeypatch, tmp_path) -> None:
    monkeypatch.delenv("SOLDR_BINARY", raising=False)
    scripts = tmp_path / "Scripts"
    scripts.mkdir()
    soldr = scripts / build_backend._script_name("soldr")
    soldr.write_text("", encoding="utf-8")
    python = scripts / build_backend._script_name("python")

    monkeypatch.setattr(build_backend.sys, "executable", str(python))

    assert build_backend._soldr_executable() == str(soldr)


def test_soldr_executable_prefers_setup_soldr_binary(monkeypatch, tmp_path) -> None:
    setup_soldr = tmp_path / "setup-soldr" / "bin" / build_backend._script_name("soldr")
    setup_soldr.parent.mkdir(parents=True)
    setup_soldr.write_text("", encoding="utf-8")
    scripts = tmp_path / "Scripts"
    scripts.mkdir()
    build_env_soldr = scripts / build_backend._script_name("soldr")
    build_env_soldr.write_text("", encoding="utf-8")
    python = scripts / build_backend._script_name("python")

    monkeypatch.setenv("SOLDR_BINARY", str(setup_soldr))
    monkeypatch.setattr(build_backend.sys, "executable", str(python))

    assert build_backend._soldr_executable() == str(setup_soldr)


def test_build_wheel_forwards_maturin_pep517_args(monkeypatch, tmp_path) -> None:
    calls = []

    monkeypatch.setattr(build_backend, "build_env", lambda: {})
    monkeypatch.setattr(build_backend, "_soldr_executable", lambda: "soldr")
    monkeypatch.setenv("MATURIN_PEP517_ARGS", "--profile dev --compatibility pypi")

    def fake_check_call(cmd, *, env, timeout):
        del env, timeout
        calls.append(cmd)
        (tmp_path / "clud-1.0.0-py3-none-any.whl").write_text("", encoding="utf-8")

    monkeypatch.setattr(build_backend.subprocess, "check_call", fake_check_call)
    monkeypatch.setattr(build_backend, "repair_windows_gnu_wheel", lambda path: None)

    build_backend.build_wheel(str(tmp_path))

    assert "--profile" in calls[0]
    assert "dev" in calls[0]
    assert calls[0].count("--compatibility") == 1
    assert "pypi" in calls[0]
    assert "off" not in calls[0]


def test_prepare_metadata_for_build_wheel_returns_dist_info(monkeypatch, tmp_path) -> None:
    def fake_check_call(cmd, *, env, timeout):
        del cmd, env, timeout
        (tmp_path / "clud-1.0.0.dist-info").mkdir()

    monkeypatch.setattr(build_backend, "build_env", lambda: {})
    monkeypatch.setattr(build_backend.subprocess, "check_call", fake_check_call)

    assert build_backend.prepare_metadata_for_build_wheel(str(tmp_path)) == "clud-1.0.0.dist-info"
