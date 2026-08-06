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


def test_release_linux_wheel_avoids_duplicate_zig_flag() -> None:
    source = (ROOT / "ci" / "xbuild.py").read_text(encoding="utf-8")
    assert 'args.strategy == "zigbuild" and "--zig" not in subcommand' in source


def test_release_linux_wheel_uses_an_isolated_target_dir() -> None:
    env = xbuild.manylinux_wheel_env("aarch64-unknown-linux-gnu", {})
    assert env["CARGO_TARGET_DIR"] == str(
        ROOT / "target" / "release-wheel" / "aarch64-unknown-linux-gnu"
    )
    assert env["SOLDR_LINKER"] == "default"


def test_release_linux_wheel_keeps_the_glibc_floor_off_the_target_triple() -> None:
    """maturin parses `--target` with target-lexicon; a `.2.17` suffix is fatal.

    The floor comes from `--compatibility manylinux2014 --zig`, which maturin
    turns into the zig target `<triple>.2.17` itself.
    """
    source = (ROOT / "ci" / "xbuild.py").read_text(encoding="utf-8")
    assert "wheel_target" not in source
    assert 'subcommand += ["--zig", "--compatibility", "manylinux2014"]' in source


def test_release_linux_wheel_lets_zig_own_the_c_toolchain() -> None:
    """`soldr prepare` exports the same variables cargo-zigbuild sets.

    cargo-zigbuild installs its shims with `add_env_if_missing`, so anything
    left here silently wins and the C objects miss the 2.17 floor -- which is
    exactly how the wheel failed the manylinux audit.
    """
    target = "aarch64-unknown-linux-gnu"
    prepared = {
        "CC_aarch64_unknown_linux_gnu": "/soldr/linux-cross/cc",
        "CXX_aarch64_unknown_linux_gnu": "/soldr/linux-cross/cxx",
        "AR_aarch64_unknown_linux_gnu": "/soldr/linux-cross/ar",
        "RANLIB_aarch64_unknown_linux_gnu": "/soldr/linux-cross/ranlib",
        "CC_aarch64-unknown-linux-gnu": "/soldr/linux-cross/cc",
        "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER": "/soldr/linux-cross/linker",
        "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS": "-C link-self-contained=no",
        "CARGO_ENCODED_RUSTFLAGS": "-Clink-self-contained=no",
        "CC": "clang",
        "TARGET_CC": "clang",
        "KEEP_ME": "yes",
    }
    env = xbuild.manylinux_wheel_env(target, prepared)

    for key in prepared:
        if key != "KEEP_ME":
            assert key not in env, f"{key} must not survive into the manylinux build"
    assert env["KEEP_ME"] == "yes"
    # The caller's dict is untouched.
    assert prepared["CC_aarch64_unknown_linux_gnu"] == "/soldr/linux-cross/cc"


def test_release_linux_wheel_names_the_interpreter_that_has_ziglang() -> None:
    """cargo-zigbuild probes `which(python3) -m ziglang`, then `which(zig)`.

    On the native x86_64 lane neither resolves -- `python3` is the hosted-tool
    interpreter and `soldr prepare`, which puts a zig on PATH, is skipped --
    so the release wheel died with "Failed to find zig".
    """
    import sys

    env = xbuild.manylinux_wheel_env("x86_64-unknown-linux-gnu", {})
    assert env["CARGO_ZIGBUILD_PYTHON_PATH"] == sys.executable


def test_release_linux_wheel_does_not_bake_in_the_builder_cpu() -> None:
    env = xbuild.manylinux_wheel_env("x86_64-unknown-linux-gnu", {})
    assert env["GGML_NATIVE"] == "OFF"


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
