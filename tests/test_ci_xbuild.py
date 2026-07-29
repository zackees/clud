"""Focused contracts for the build-once cross-compile driver."""

from __future__ import annotations

from pathlib import Path

from ci import bundle, xbuild

ROOT = Path(__file__).resolve().parent.parent


def test_bundle_scratch_does_not_collide_with_the_build_entrypoint() -> None:
    assert (bundle.ROOT / "build").is_file()
    assert bundle.CI_SCRATCH == bundle.ROOT / ".ci-build"
    assert bundle.CI_SCRATCH != bundle.ROOT / "build"


def test_soldr_strategy_routes_linking_builds_through_blessed_surface() -> None:
    target = "x86_64-pc-windows-msvc"
    assert xbuild.cargo_argv(["build", "--workspace", "--bins"], target, "soldr") == [
        "soldr",
        "build",
        "--workspace",
        "--bins",
        "--target",
        target,
    ]


def test_soldr_strategy_keeps_non_build_verbs_on_prepared_cargo_env() -> None:
    target = "aarch64-apple-darwin"
    assert xbuild.cargo_argv(["test", "--workspace", "--no-run"], target, "soldr") == [
        "cargo",
        "test",
        "--workspace",
        "--no-run",
        "--target",
        target,
    ]


def test_soldr_setup_installs_target_for_plain_cargo_verbs() -> None:
    setup = ROOT / ".github" / "actions" / "setup-build" / "action.yml"
    text = setup.read_text(encoding="utf-8")
    prepare = '"${SOLDR_BINARY:-soldr}" prepare'
    install = 'rustup target add "${{ inputs.target }}"'

    assert prepare in text
    assert install in text
    assert text.index(prepare) < text.index(install)


def test_zigbuild_strategy_uses_the_build_subcommand_interface() -> None:
    target = "aarch64-unknown-linux-gnu"
    argv = xbuild.cargo_argv(["build", "--workspace", "--bins"], target, "zigbuild")
    assert argv[:2] == [xbuild.PACKAGE_MANAGER, "zigbuild"]
    assert argv[2:] == ["--workspace", "--bins", "--target", target]


def test_zigbuild_strategy_builds_test_targets_without_running_them() -> None:
    target = "aarch64-unknown-linux-gnu"
    argv = xbuild.cargo_argv(
        ["test", "--workspace", "--no-run", "--message-format=json"],
        target,
        "zigbuild",
    )
    assert argv[:3] == [xbuild.PACKAGE_MANAGER, "zigbuild", "--tests"]
    assert argv[3:] == ["--workspace", "--message-format=json", "--target", target]


def test_darwin_soldr_env_forwards_the_prepared_sdk_to_cmake() -> None:
    env = xbuild.whisper_env(
        "aarch64-apple-darwin",
        "soldr",
        {"SDKROOT": "/opt/soldr/MacOSX.sdk"},
    )
    assert env["SDKROOT"] == "/opt/soldr/MacOSX.sdk"
    assert env["CMAKE_OSX_SYSROOT"] == "/opt/soldr/MacOSX.sdk"
    assert env["CMAKE_SYSTEM_NAME"] == "Darwin"
    assert env["CMAKE_INSTALL_NAME_TOOL"] == "llvm-install-name-tool"
    assert env["GGML_BLAS"] == "OFF"
    assert env["GGML_METAL"] == "OFF"


def test_windows_soldr_env_keeps_msvc_cmake_and_advapi_contract() -> None:
    env = xbuild.whisper_env(
        "x86_64-pc-windows-msvc",
        "soldr",
        {"RUSTFLAGS": "-C debuginfo=0"},
    )
    assert env["CMAKE_SYSTEM_NAME"] == "Windows"
    assert env["CC"] == "clang-cl"
    assert env["CXX"] == "clang-cl"
    assert env["CMAKE_TRY_COMPILE_TARGET_TYPE"] == "STATIC_LIBRARY"
    assert "RC" not in env
    assert "CMAKE_RC_COMPILER" not in env
    assert "CMAKE_MT" not in env
    assert env["RUSTFLAGS"] == "-C debuginfo=0 -C link-arg=advapi32.lib"


def test_msvc_exceptions_are_enabled_only_for_windows_soldr() -> None:
    windows = xbuild.whisper_env(
        "x86_64-pc-windows-msvc",
        "soldr",
        {"CXXFLAGS": "-g0"},
    )
    assert windows["CXXFLAGS"] == "-g0 /EHsc"
    assert "CMAKE_CXX_FLAGS" not in windows

    other_strategies = (
        ("x86_64-pc-windows-msvc", "native"),
        ("aarch64-apple-darwin", "soldr"),
        ("aarch64-unknown-linux-gnu", "zigbuild"),
    )
    for target, strategy in other_strategies:
        env = xbuild.whisper_env(target, strategy, {"CXXFLAGS": "-g0"})
        assert env["CXXFLAGS"] == "-g0"
