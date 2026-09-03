"""Focused contracts for the build-once cross-compile driver."""

from __future__ import annotations

import sys
from pathlib import Path

from running_process import PIPE, RunningProcess

from ci import bundle, xbuild

ROOT = Path(__file__).resolve().parent.parent


def test_bundle_scratch_does_not_collide_with_the_build_entrypoint() -> None:
    assert (bundle.ROOT / "build").is_file()
    assert bundle.CI_SCRATCH == bundle.ROOT / ".ci-build"
    assert bundle.CI_SCRATCH != bundle.ROOT / "build"


def test_linux_runner_wraps_cargo_with_memory_limit(monkeypatch) -> None:
    monkeypatch.setattr(xbuild.platform, "system", lambda: "Linux")
    assert xbuild._run_argv(["cargo", "clippy"]) == [
        "bash",
        "-c",
        "ulimit -v 13000000; exec \"$@\"",
        "clud-xbuild",
        "cargo",
        "clippy",
    ]


def test_non_linux_runner_does_not_wrap_cargo(monkeypatch) -> None:
    monkeypatch.setattr(xbuild.platform, "system", lambda: "Darwin")
    assert xbuild._run_argv(["cargo", "clippy"]) == ["cargo", "clippy"]


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


def test_gnu_linux_build_routes_through_the_blessed_soldr_surface() -> None:
    """soldr#2299: GNU/Linux is soldr-owned now, so its linking build goes
    through `soldr build`, never the zig cross."""
    target = "aarch64-unknown-linux-gnu"
    argv = xbuild.cargo_argv(["build", "--workspace", "--bins"], target, "soldr")
    assert argv == ["soldr", "build", "--workspace", "--bins", "--target", target]


def test_gnu_linux_zigbuild_is_refused() -> None:
    """Routing a GNU/Linux build through zigbuild is refused structurally, the
    same guard that protects Apple/MSVC (#637)."""
    import pytest

    for target in ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"):
        with pytest.raises(ValueError, match="soldr"):
            xbuild.cargo_argv(["build"], target, "zigbuild")


def test_release_linux_wheel_builds_without_zig() -> None:
    """The release GNU wheel links through soldr's blessed catalogue toolchain,
    not the old zig path. The glibc-2.17 floor comes from `soldr prepare`'s
    catalogue sysroot; maturin just tags the wheel manylinux2014. soldr#2299.
    """
    source = (ROOT / "ci" / "xbuild.py").read_text(encoding="utf-8")
    assert 'subcommand += ["--compatibility", "manylinux2014"]' in source
    assert 'subcommand += ["--zig"' not in source  # cross-lint: allow
    assert 'subcommand.append("--zig")' not in source  # cross-lint: allow
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
    # The load-bearing half: whisper-rs-sys links stdc++ statically.
    assert env["WHISPER_LINK_CXX_STATIC"] == "1"
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


def test_gnu_linux_test_verb_stays_on_prepared_cargo() -> None:
    """Non-linking verbs ride the plain cargo front door on the env `soldr
    prepare` exported -- same as Apple/MSVC under the soldr strategy."""
    target = "aarch64-unknown-linux-gnu"
    argv = xbuild.cargo_argv(["test", "--workspace", "--no-run"], target, "soldr")
    assert argv == ["cargo", "test", "--workspace", "--no-run", "--target", target]


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
        ("aarch64-unknown-linux-gnu", "soldr"),
    )
    for target, strategy in other_strategies:
        env = xbuild.whisper_env(target, strategy, {"CXXFLAGS": "-g0"})
        assert env["CXXFLAGS"] == "-g0"


def test_debuginfo_is_staged_outside_dist_so_it_cannot_reach_pypi(
    tmp_path, monkeypatch, capsys
) -> None:
    """The `.dwp` must never enter `dist/`.

    `_build-target.yml` uploads `dist/*` as the `wheels-*` artifact and
    `publish-pypi` hands that straight to twine, which rejects a non-package
    file -- the exact 400 that killed 2.7.2-2.7.4.
    """
    target = "x86_64-unknown-linux-gnu"
    release = tmp_path / "target" / target / "release"
    release.mkdir(parents=True)
    (release / "clud.dwp").write_bytes(b"dwarf")
    # Neither shim installs the crash reporter and each .dwp is ~91 MB of the
    # same DWARF, so only the main binary's symbols are published.
    (release / "clud-shim.dwp").write_bytes(b"dwarf")
    (release / "mock-agent.dwp").write_bytes(b"dwarf")
    monkeypatch.setattr(xbuild, "ROOT", tmp_path)
    monkeypatch.setattr(xbuild, "DEBUGINFO_DIR", tmp_path / "dist-debuginfo")

    staged = xbuild.collect_debuginfo(target, "release")

    assert [path.name for path in staged] == [f"clud-{target}.dwp"]
    assert all(path.parent == tmp_path / "dist-debuginfo" for path in staged)
    assert not (tmp_path / "dist").exists()
    assert f"clud-{target}.dwp (5 bytes)" in capsys.readouterr().out


def test_debuginfo_is_found_in_deps_when_cargo_does_not_uplift_it(
    tmp_path, monkeypatch, capsys
) -> None:
    """2.7.5 shipped with no sidecar because only the uplifted path was checked.

    The wheel shrank 101 MB -> 47 MB, proving the DWARF left the ELF, yet
    `release/clud.dwp` did not exist: under maturin's `cargo rustc --bin`,
    cargo uplifts the binary out of `deps/` but not reliably its `.dwp`. The
    hash suffix is matched exactly so a sibling bin is never mistaken for
    clud's own.
    """
    target = "x86_64-unknown-linux-gnu"
    deps = tmp_path / "target" / target / "release" / "deps"
    deps.mkdir(parents=True)
    (deps / "clud-2f8a1c9d4e6b7a05.dwp").write_bytes(b"dwarf")
    (deps / "clud_shim-9b3c7e1a2d4f6508.dwp").write_bytes(b"other")
    monkeypatch.setattr(xbuild, "ROOT", tmp_path)
    monkeypatch.setattr(xbuild, "DEBUGINFO_DIR", tmp_path / "dist-debuginfo")

    staged = xbuild.collect_debuginfo(target, "release")

    assert [path.name for path in staged] == [f"clud-{target}.dwp"]
    assert staged[0].read_bytes() == b"dwarf"
    assert not (tmp_path / "dist").exists()
    assert f"clud-{target}.dwp (5 bytes)" in capsys.readouterr().out


def test_missing_debuginfo_is_reported_but_never_fatal(tmp_path, monkeypatch) -> None:
    """Targets where `packed` writes no `.dwp` must not block a release."""
    target = "aarch64-pc-windows-msvc"
    (tmp_path / "target" / target / "release").mkdir(parents=True)
    monkeypatch.setattr(xbuild, "ROOT", tmp_path)
    monkeypatch.setattr(xbuild, "DEBUGINFO_DIR", tmp_path / "dist-debuginfo")

    assert xbuild.collect_debuginfo(target, "release") == []
    assert not (tmp_path / "dist-debuginfo").exists()


def test_by_path_invocation_is_rejected_with_the_module_form(tmp_path) -> None:
    """Issue #1017 blocker 1: `python ci/xbuild.py ...` used to die with

        ImportError: cannot import name 'process' from 'ci'
            (.../site-packages/ci/__init__.py)

    because running by path puts `ci/` on `sys.path[0]`, so `from ci import
    process` binds to an unrelated installed distribution. The traceback names
    the wrong package entirely, and this is a large part of why a release wheel
    could not be reproduced on a developer machine.
    """
    result = RunningProcess.run(
        [sys.executable, str(ROOT / "ci" / "xbuild.py"), "wheel", "--target", "x"],
        # Explicit pipes rather than `capture_output=True`: the latter leaves
        # `result.stderr` as None here, and the guard writes to stderr.
        stdout=PIPE,
        stderr=PIPE,
        text=True,
        encoding="utf-8",
        errors="replace",
        cwd=ROOT,
    )

    assert result.returncode == 2, result.stderr
    # Names the real cause, not a missing symbol.
    assert "cannot be run by path" in result.stderr
    assert "unrelated installed `ci` package" in result.stderr
    # Hands over the exact command, arguments preserved so it can be pasted.
    assert "python -m ci.xbuild wheel --target x" in result.stderr
    # The misleading failure is gone.
    assert "cannot import name 'process'" not in result.stderr


def test_module_form_is_untouched_by_the_guard() -> None:
    """The guard must be a no-op for the invocation CI actually uses."""
    assert xbuild.__package__ == "ci"
    # Importable as a module means the guard did not fire at import time.
    assert hasattr(xbuild, "main")
