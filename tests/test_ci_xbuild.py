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


def test_release_linux_wheel_builds_without_zig() -> None:
    """The release GNU wheel links through soldr's blessed catalogue toolchain,
    not `maturin --zig`. The glibc-2.17 floor comes from `soldr prepare`'s
    catalogue sysroot; maturin just tags the wheel manylinux2014. soldr#2299.
    """
    source = (ROOT / "ci" / "xbuild.py").read_text(encoding="utf-8")
    assert 'subcommand += ["--compatibility", "manylinux2014"]' in source
    assert 'subcommand += ["--zig"' not in source
    assert 'subcommand.append("--zig")' not in source
    assert "CARGO_ZIGBUILD_PYTHON_PATH" not in source


def test_static_cxx_runtime_env_appends_to_encoded_rustflags() -> None:
    """gcc-13's libstdc++ is too new for manylinux_2_17, so the C++ runtime is
    linked statically. The flags append to soldr's exported
    CARGO_ENCODED_RUSTFLAGS so its sysroot flags survive.
    """
    prepared = {"CARGO_ENCODED_RUSTFLAGS": "-Clink-arg=--sysroot=/soldr/sysroot"}
    env = xbuild.static_cxx_runtime_env("x86_64-unknown-linux-gnu", prepared)
    parts = env["CARGO_ENCODED_RUSTFLAGS"].split("\x1f")
    assert "-Clink-arg=--sysroot=/soldr/sysroot" in parts, "soldr's sysroot flag must survive"
    assert "-Clink-arg=-static-libstdc++" in parts
    assert "-Clink-arg=-static-libgcc" in parts
    # The caller's dict is untouched.
    assert prepared["CARGO_ENCODED_RUSTFLAGS"] == "-Clink-arg=--sysroot=/soldr/sysroot"


def test_static_cxx_runtime_env_falls_back_to_target_rustflags() -> None:
    """With no encoded rustflags set, the flags land on the target-scoped
    RUSTFLAGS in spelled `-C link-arg=` form.
    """
    env = xbuild.static_cxx_runtime_env("x86_64-unknown-linux-gnu", {})
    value = env["CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS"]
    assert "-C link-arg=-static-libstdc++" in value
    assert "-C link-arg=-static-libgcc" in value


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
    rustflags_key = "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS"
    env = xbuild.whisper_env(
        "x86_64-pc-windows-msvc",
        "soldr",
        {
            "RUSTFLAGS": "-C debuginfo=0 -C opt-level=3",
            rustflags_key: ("-C link-arg=/LIBPATH:/opt/xwin/sdk/lib/um/x86_64 -C opt-level=1"),
        },
    )
    assert env["CMAKE_SYSTEM_NAME"] == "Windows"
    assert env["CC"] == "clang-cl"
    assert env["CXX"] == "clang-cl"
    assert env["CMAKE_TRY_COMPILE_TARGET_TYPE"] == "STATIC_LIBRARY"
    assert "RC" not in env
    assert "CMAKE_RC_COMPILER" not in env
    assert "CMAKE_MT" not in env
    assert "RUSTFLAGS" not in env
    assert env[rustflags_key] == (
        "-C link-arg=/LIBPATH:/opt/xwin/sdk/lib/um/x86_64 "
        "-C opt-level=1 "
        "-C debuginfo=0 "
        "-C opt-level=3 "
        "-C link-arg=advapi32.lib"
    )


def test_windows_soldr_env_preserves_encoded_sdk_paths_from_soldr_0_8_30() -> None:
    """soldr >= 0.8.30 exports the MSVC link config in the encoded variable.

    This previously raised, which was correct while the SDK paths lived only
    in the target-scoped variable that CARGO_ENCODED_RUSTFLAGS shadows. Once
    soldr moved them into the winning variable, refusing to proceed rejected
    the correct configuration and reddened every Windows lane.
    """
    encoded = "\x1f".join(
        [
            "-Clinker-flavor=lld-link",
            "-Clink-arg=/NODEFAULTLIB:libucrt.lib",
            "-Clink-arg=/LIBPATH:/opt/xwin/sdk/lib/um/x86_64",
        ]
    )
    env = xbuild.whisper_env(
        "x86_64-pc-windows-msvc",
        "soldr",
        {"CARGO_ENCODED_RUSTFLAGS": encoded},
    )
    parts = env["CARGO_ENCODED_RUSTFLAGS"].split("\x1f")
    # The prepared link configuration survives, in order...
    assert parts[:3] == encoded.split("\x1f")
    # ...and our own library is appended rather than replacing it.
    assert parts[-1] == "-Clink-arg=advapi32.lib"


def test_windows_soldr_env_folds_shadowed_rustflags_into_encoded() -> None:
    """Anything the encoded variable shadows must be merged, not dropped.

    CARGO_ENCODED_RUSTFLAGS outranks both RUSTFLAGS and
    target.<triple>.rustflags, so a toolchain that sets it without the SDK
    paths would otherwise silently lose them.
    """
    rustflags_key = "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS"
    env = xbuild.whisper_env(
        "x86_64-pc-windows-msvc",
        "soldr",
        {
            "CARGO_ENCODED_RUSTFLAGS": "-Clinker-flavor=lld-link",
            "RUSTFLAGS": "-C debuginfo=0",
            rustflags_key: "-C link-arg=/LIBPATH:/opt/xwin/sdk/lib/um/x86_64",
        },
    )
    parts = env["CARGO_ENCODED_RUSTFLAGS"].split("\x1f")
    assert "/LIBPATH:/opt/xwin/sdk/lib/um/x86_64" in " ".join(parts)
    assert "debuginfo=0" in " ".join(parts)
    assert parts[-1] == "-Clink-arg=advapi32.lib"
    # Both shadowed sources are removed so nothing reads a stale value.
    assert "RUSTFLAGS" not in env
    assert rustflags_key not in env


def test_windows_soldr_bindgen_uses_target_sdk_headers_when_available() -> None:
    cflags_key = "CFLAGS_x86_64_pc_windows_msvc"
    bindgen_key = "BINDGEN_EXTRA_CLANG_ARGS_x86_64_pc_windows_msvc"
    windows = xbuild.whisper_env(
        "x86_64-pc-windows-msvc",
        "soldr",
        {
            cflags_key: (
                "/imsvc/opt/xwin/crt/include "
                "/imsvc/opt/xwin/sdk/include/ucrt "
                "/imsvc/opt/xwin/sdk/include/cppwinrt"
            )
        },
    )
    assert "WHISPER_DONT_GENERATE_BINDINGS" not in windows
    assert windows[bindgen_key] == (
        "-I/opt/xwin/crt/include -I/opt/xwin/sdk/include/ucrt -I/opt/xwin/sdk/include/cppwinrt"
    )

    no_sdk = xbuild.whisper_env("x86_64-pc-windows-msvc", "soldr", {})
    assert no_sdk["WHISPER_DONT_GENERATE_BINDINGS"] == "1"
    assert bindgen_key not in no_sdk

    darwin = xbuild.whisper_env("aarch64-apple-darwin", "soldr", {})
    assert darwin["WHISPER_DONT_GENERATE_BINDINGS"] == "1"
    assert bindgen_key not in darwin


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
