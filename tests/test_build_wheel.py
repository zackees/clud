import struct
import zipfile

import pytest

from ci import build_wheel
from ci.kitty_wheel import KITTY_BUNDLE_FILES, KITTY_SOURCE_REVISION, add_kitty_bundle


@pytest.mark.parametrize(
    ("help_returncode", "help_output", "expected"),
    [
        (0, "--return-initial-exit-code", 0),
        (0, "Usage: wezterm-gui start", 1),
        (1, "--return-initial-exit-code", 1),
    ],
)
def test_native_windows_installed_console_help_smoke(
    monkeypatch, tmp_path, help_returncode, help_output, expected
):
    monkeypatch.setattr(build_wheel.platform, "system", lambda: "Windows")
    monkeypatch.setattr(build_wheel.platform, "machine", lambda: "AMD64")
    monkeypatch.setattr(build_wheel, "_installed_script", lambda name: tmp_path / name)
    calls = []

    class Result:
        def __init__(self, returncode=0, stdout="", stderr=""):
            self.returncode = returncode
            self.stdout = stdout
            self.stderr = stderr

    def fake_run(argv, **kwargs):
        calls.append(argv)
        if argv[-1] == "--version":
            return Result(stdout="wezterm 1.0")
        if argv[-2:] == ["start", "--help"]:
            return Result(help_returncode, help_output)
        if kwargs.get("input") and "bad" + " cmd" in kwargs["input"]:
            return Result(2, '{"permissionDecision":"deny"}')
        return Result()

    monkeypatch.setattr(build_wheel.process, "run", fake_run)
    assert build_wheel._verify_installed_smokes(env={}, target=None) == expected
    assert calls[-1] == [
        str(tmp_path / "clud-kittyterm" / "wezterm.exe"),
        "start",
        "--help",
    ]


def test_windows_soldr_wheel_packages_prebuilt_executables(monkeypatch, tmp_path):
    target_dir = tmp_path / "target"
    binaries = target_dir / "x86_64-pc-windows-msvc" / "release"
    binaries.mkdir(parents=True)
    for name in build_wheel.REQUIRED_SCRIPTS:
        (binaries / f"{name}.exe").write_bytes(f"{name}-binary".encode())
    helper = binaries / "clud-kittyterm-paste.exe"
    helper_data = bytearray(0x46)
    helper_data[:2] = b"MZ"
    struct.pack_into("<I", helper_data, 0x3C, 0x40)
    helper_data[0x40:0x44] = b"PE\0\0"
    struct.pack_into("<H", helper_data, 0x44, 0x8664)
    helper.write_bytes(helper_data)
    bundle = tmp_path / "fork-build"
    for name in KITTY_BUNDLE_FILES:
        file = bundle / name
        file.parent.mkdir(parents=True, exist_ok=True)
        if name.lower().endswith((".exe", ".dll")):
            pe = bytearray(0x46)
            pe[:2] = b"MZ"
            struct.pack_into("<I", pe, 0x3C, 0x40)
            pe[0x40:0x44] = b"PE\0\0"
            struct.pack_into("<H", pe, 0x44, 0x8664)
            if name == "wezterm-gui.exe":
                pe.extend(b"return-initial-exit-code\0")
            file.write_bytes(pe)
        else:
            file.write_bytes(name.encode())
    (bundle / "SOURCE_REVISION").write_text(f"zackees/wezterm@{KITTY_SOURCE_REVISION}\n")
    monkeypatch.setenv("CLUD_KITTYTERM_BUNDLE_DIR", str(bundle))

    wheel = build_wheel.build_windows_wheel_from_binaries(
        target="x86_64-pc-windows-msvc",
        profile="release",
        target_dir=target_dir,
        dist_dir=tmp_path / "dist",
        version="2.5.4",
    )

    with zipfile.ZipFile(wheel) as archive:
        members = set(archive.namelist())
        assert "clud/__init__.py" in members
        for name in build_wheel.REQUIRED_SCRIPTS:
            assert f"clud-2.5.4.data/scripts/{name}.exe" in members
        assert "clud-2.5.4.dist-info/METADATA" in members
        assert "clud-2.5.4.dist-info/WHEEL" in members
        assert "clud-2.5.4.dist-info/RECORD" in members
        for name in (*KITTY_BUNDLE_FILES, "clud-kittyterm.lua", "clud-kittyterm-paste.exe"):
            assert f"clud-2.5.4.data/scripts/clud-kittyterm/{name}" in members


def test_windows_soldr_wheel_fails_closed_without_bundle(monkeypatch, tmp_path):
    monkeypatch.delenv("CLUD_KITTYTERM_BUNDLE_DIR", raising=False)
    with pytest.raises(RuntimeError, match="CLUD_KITTYTERM_BUNDLE_DIR"):
        build_wheel.build_windows_wheel_from_binaries(
            target="x86_64-pc-windows-msvc",
            profile="release",
            target_dir=tmp_path / "target",
            dist_dir=tmp_path / "dist",
            version="2.5.4",
        )
    assert not (tmp_path / "dist").exists()


def test_windows_soldr_wheel_fails_before_writing_without_paste_helper(monkeypatch, tmp_path):
    monkeypatch.setattr(build_wheel, "resolve_kitty_bundle", lambda: tmp_path)
    with pytest.raises(RuntimeError, match=r"clud-kittyterm-paste\.exe"):
        build_wheel.build_windows_wheel_from_binaries(
            target="x86_64-pc-windows-msvc",
            profile="release",
            target_dir=tmp_path / "target",
            dist_dir=tmp_path / "dist",
            version="2.5.4",
        )
    assert not (tmp_path / "dist").exists()


def test_windows_arm64_wheel_does_not_require_or_ship_x64_gui(monkeypatch, tmp_path):
    monkeypatch.delenv("CLUD_KITTYTERM_BUNDLE_DIR", raising=False)
    target = "aarch64-pc-windows-msvc"
    binaries = tmp_path / "target" / target / "release"
    binaries.mkdir(parents=True)
    for name in build_wheel.REQUIRED_SCRIPTS:
        (binaries / f"{name}.exe").write_bytes(f"{name}-arm64".encode())
    wheel = build_wheel.build_windows_wheel_from_binaries(
        target=target,
        profile="release",
        target_dir=tmp_path / "target",
        dist_dir=tmp_path / "dist",
        version="2.5.4",
    )
    assert wheel.name.endswith("-win_arm64.whl")
    with zipfile.ZipFile(wheel) as archive:
        assert not any("clud-kittyterm/" in name for name in archive.namelist())


def test_windows_wheel_ships_the_cmd_scan_binary() -> None:
    """#862: 2.5.5 shipped the hook rollout pointing configs at
    `clud-cmd-scan` while the hand-packed win_amd64 wheel didn't contain the
    binary — every Bash PreToolUse call on Windows errored `command not
    found`, and the scan protection was silently off."""
    assert "clud-cmd-scan" in build_wheel.REQUIRED_SCRIPTS


def test_maturin_wheel_prunes_the_test_only_ctrlc_probe(tmp_path) -> None:
    wheel = tmp_path / "clud-2.8.7-py3-none-manylinux_2_17_x86_64.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        for name in (*build_wheel.REQUIRED_SCRIPTS, "clud-ctrlc-probe"):
            archive.writestr(f"clud-2.8.7.data/scripts/{name}", b"binary")
        archive.writestr("clud-2.8.7.dist-info/RECORD", "clud-2.8.7.dist-info/RECORD,,\n")

    assert build_wheel.prune_nonproduction_scripts(wheel)
    with zipfile.ZipFile(wheel) as archive:
        assert "clud-2.8.7.data/scripts/clud-ctrlc-probe" not in archive.namelist()
    assert build_wheel.verify_wheel_scripts(wheel) == 0


def test_release_wheel_removes_elf_debug_gdb_metadata(monkeypatch, tmp_path) -> None:
    wheel = tmp_path / "clud-2.8.7-py3-none-manylinux_2_17_x86_64.whl"
    script = "clud-2.8.7.data/scripts/clud"
    with zipfile.ZipFile(wheel, "w") as archive:
        archive.writestr(script, b"\x7fELFdebug-gdb")
        archive.writestr("clud-2.8.7.dist-info/RECORD", "clud-2.8.7.dist-info/RECORD,,\n")

    class Result:
        returncode = 0

    monkeypatch.setenv("CARGO_BUILD_TARGET", "")
    monkeypatch.delenv("CC", raising=False)
    monkeypatch.delenv("OBJCOPY", raising=False)
    monkeypatch.setattr(
        build_wheel.shutil,
        "which",
        lambda candidate: candidate == "llvm-objcopy",
    )
    calls = []
    monkeypatch.setattr(
        build_wheel.process,
        "run",
        lambda argv, **kwargs: calls.append(argv) or Result(),
    )
    assert build_wheel.remove_elf_debug_metadata(wheel)
    assert calls
    assert calls[0][:2] == ["llvm-objcopy", "--remove-section=.debug_gdb_scripts"]


def test_release_wheel_uses_target_prefixed_objcopy_when_llvm_is_absent(
    monkeypatch, tmp_path
) -> None:
    wheel = tmp_path / "clud-2.8.9-py3-none-manylinux_2_17_aarch64.whl"
    script = "clud-2.8.9.data/scripts/clud"
    with zipfile.ZipFile(wheel, "w") as archive:
        archive.writestr(script, b"\x7fELFdebug-gdb")
        archive.writestr("clud-2.8.9.dist-info/RECORD", "clud-2.8.9.dist-info/RECORD,,\n")

    cross_objcopy = "/toolchain/bin/aarch64-conda-linux-gnu-objcopy"
    # `cmd_wheel` builds maturin with a child-only target environment, then
    # strips the completed wheel in this parent process. The explicit target
    # must therefore still find the prepared cross objcopy without ambient
    # CARGO_BUILD_TARGET.
    monkeypatch.delenv("CARGO_BUILD_TARGET", raising=False)
    monkeypatch.setenv(
        "CC_aarch64_unknown_linux_gnu",
        "/toolchain/bin/aarch64-conda-linux-gnu-gcc",
    )
    monkeypatch.setattr(
        build_wheel.shutil,
        "which",
        lambda candidate: (
            candidate
            if candidate.replace("\\", "/").endswith("/aarch64-conda-linux-gnu-objcopy")
            else None
        ),
    )

    class Result:
        returncode = 0

    calls = []
    monkeypatch.setattr(
        build_wheel.process,
        "run",
        lambda argv, **kwargs: calls.append(argv) or Result(),
    )

    assert build_wheel.remove_elf_debug_metadata(wheel, target="aarch64-unknown-linux-gnu")
    assert calls[0][0].replace("\\", "/") == cross_objcopy


def test_release_wheel_falls_back_after_an_incompatible_target_objcopy(
    monkeypatch, tmp_path
) -> None:
    from pathlib import Path

    wheel = tmp_path / "clud-2.8.10-py3-none-manylinux_2_17_aarch64.whl"
    script = "clud-2.8.10.data/scripts/clud"
    with zipfile.ZipFile(wheel, "w") as archive:
        archive.writestr(script, b"\x7fELFdebug-gdb")
        archive.writestr("clud-2.8.10.dist-info/RECORD", "clud-2.8.10.dist-info/RECORD,,\n")

    first = "/toolchain/bin/aarch64-conda-linux-gnu-objcopy"
    fallback = "/toolchain/bin/objcopy"
    monkeypatch.setenv("CARGO_BUILD_TARGET", "aarch64-unknown-linux-gnu")
    monkeypatch.setenv(
        "CC_aarch64_unknown_linux_gnu",
        "/toolchain/bin/aarch64-conda-linux-gnu-gcc",
    )
    monkeypatch.setattr(
        build_wheel.shutil,
        "which",
        lambda candidate: (
            candidate
            if candidate.replace("\\", "/")
            in {first, fallback}
            else None
        ),
    )

    class Result:
        def __init__(self, returncode):
            self.returncode = returncode

    calls = []

    def fake_run(argv, **kwargs):
        calls.append(argv)
        script_path = Path(argv[2])
        if argv[0].replace("\\", "/") == first:
            script_path.write_bytes(b"corrupt")
            return Result(1)
        assert script_path.read_bytes() == b"\x7fELFdebug-gdb"
        return Result(0)

    monkeypatch.setattr(
        build_wheel.process,
        "run",
        fake_run,
    )

    assert build_wheel.remove_elf_debug_metadata(wheel)
    assert [call[0].replace("\\", "/") for call in calls] == [first, fallback]


def test_release_wheel_reports_missing_elf_objcopy(monkeypatch) -> None:
    import pytest

    monkeypatch.setenv("CARGO_BUILD_TARGET", "aarch64-unknown-linux-gnu")
    monkeypatch.delenv("CC", raising=False)
    monkeypatch.delenv("OBJCOPY", raising=False)
    monkeypatch.delenv("OBJCOPY_AARCH64_UNKNOWN_LINUX_GNU", raising=False)
    monkeypatch.delenv("CC_aarch64_unknown_linux_gnu", raising=False)
    monkeypatch.delenv("CC_AARCH64_UNKNOWN_LINUX_GNU", raising=False)
    monkeypatch.delenv("CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER", raising=False)
    monkeypatch.setattr(build_wheel.shutil, "which", lambda _candidate: None)

    with pytest.raises(RuntimeError, match="ELF objcopy"):
        build_wheel.resolve_elf_objcopy()


def test_cross_target_does_not_derive_objcopy_from_generic_cc(monkeypatch) -> None:
    import pytest

    from ci import env

    monkeypatch.setenv("CARGO_BUILD_TARGET", "aarch64-unknown-linux-gnu")
    monkeypatch.setenv("CC", "/toolchain/bin/gcc")
    monkeypatch.delenv("OBJCOPY", raising=False)
    monkeypatch.delenv("OBJCOPY_AARCH64_UNKNOWN_LINUX_GNU", raising=False)
    monkeypatch.delenv("CC_aarch64_unknown_linux_gnu", raising=False)
    monkeypatch.delenv("CC_AARCH64_UNKNOWN_LINUX_GNU", raising=False)
    monkeypatch.delenv("CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER", raising=False)
    monkeypatch.setattr(env, "host_target_triple", lambda: "x86_64-unknown-linux-gnu")
    attempted = []
    monkeypatch.setattr(
        build_wheel.shutil,
        "which",
        lambda candidate: attempted.append(candidate) or candidate == "/toolchain/bin/objcopy",
    )

    with pytest.raises(RuntimeError, match="ELF objcopy"):
        build_wheel.resolve_elf_objcopy()
    assert "/toolchain/bin/objcopy" not in attempted


def test_native_target_can_use_generic_objcopy(monkeypatch) -> None:
    from ci import env

    target = "x86_64-unknown-linux-gnu"
    monkeypatch.setenv("CARGO_BUILD_TARGET", target)
    monkeypatch.delenv("CC", raising=False)
    monkeypatch.delenv("OBJCOPY", raising=False)
    monkeypatch.delenv("OBJCOPY_X86_64_UNKNOWN_LINUX_GNU", raising=False)
    monkeypatch.delenv("CC_x86_64_unknown_linux_gnu", raising=False)
    monkeypatch.delenv("CC_X86_64_UNKNOWN_LINUX_GNU", raising=False)
    monkeypatch.delenv("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER", raising=False)
    monkeypatch.setattr(env, "host_target_triple", lambda: target)
    monkeypatch.setattr(build_wheel.shutil, "which", lambda candidate: candidate == "objcopy")

    assert build_wheel.resolve_elf_objcopy() == "objcopy"


def test_release_wheel_rejects_a_remaining_elf_debug_section(monkeypatch, tmp_path) -> None:
    import pytest

    wheel = tmp_path / "clud-2.8.7-py3-none-manylinux_2_17_x86_64.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        archive.writestr("clud-2.8.7.data/scripts/clud", b"\x7fELF")

    class Result:
        returncode = 0
        stdout = "  [1] .debug_info PROGBITS"

    monkeypatch.setattr(build_wheel.process, "run", lambda *args, **kwargs: Result())
    with pytest.raises(RuntimeError, match="retains debug"):
        build_wheel.verify_no_elf_debug_sections(wheel)


def test_local_webterm_companion_uses_the_configured_target_directory(
    monkeypatch, tmp_path
) -> None:
    target = "x86_64-pc-windows-msvc"
    companion = tmp_path / "clud-webterm" / "target" / target / "debug" / "clud-webterm.exe"
    companion.parent.mkdir(parents=True)
    companion.write_bytes(b"webterm")
    monkeypatch.setattr(build_wheel, "ROOT", tmp_path)

    class Result:
        returncode = 0

    monkeypatch.setattr(build_wheel.process, "run", lambda *args, **kwargs: Result())

    assert build_wheel.build_local_webterm_companion(mode="dev", target=target, env={}) == companion


def test_local_windows_build_explicitly_builds_paste_helper(monkeypatch, tmp_path) -> None:
    monkeypatch.setattr(build_wheel, "ROOT", tmp_path)
    helper = (
        tmp_path / "custom-target" / "x86_64-pc-windows-msvc" / "release"
        / "clud-kittyterm-paste.exe"
    )
    helper.parent.mkdir(parents=True)
    data = bytearray(0x46)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x40)
    data[0x40:0x44] = b"PE\0\0"
    struct.pack_into("<H", data, 0x44, 0x8664)
    helper.write_bytes(data)
    calls = []

    class Result:
        returncode = 0

    monkeypatch.setattr(
        build_wheel.process,
        "run",
        lambda argv, **kwargs: calls.append((argv, kwargs)) or Result(),
    )
    env = {"CARGO_TARGET_DIR": "custom-target"}
    assert build_wheel.build_local_kitty_paste_helper(mode="release", env=env) == helper
    assert calls[0][0] == [
        "soldr",
        "build",
        "--manifest-path",
        str(tmp_path / "crates" / "clud-bin" / "Cargo.toml"),
        "--bin",
        "clud-kittyterm-paste",
        "--target",
        "x86_64-pc-windows-msvc",
        "--release",
    ]


@pytest.mark.parametrize("bundle_state", ["absent", "stale"])
def test_local_windows_build_rejects_bundle_before_maturin(
    monkeypatch, tmp_path, bundle_state
) -> None:
    from ci import env as build_env_module

    monkeypatch.setattr(build_wheel, "DIST", tmp_path / "dist")
    monkeypatch.setattr(build_wheel, "local_webterm_target", lambda: build_wheel.KITTY_TARGET)
    monkeypatch.setattr(build_env_module, "build_env", lambda: {})
    if bundle_state == "absent":
        monkeypatch.delenv("CLUD_KITTYTERM_BUNDLE_DIR", raising=False)
    else:
        bundle = tmp_path / "stale-bundle"
        for name in KITTY_BUNDLE_FILES:
            file = bundle / name
            file.parent.mkdir(parents=True, exist_ok=True)
            if name.lower().endswith((".exe", ".dll")):
                data = bytearray(0x46)
                data[:2] = b"MZ"
                struct.pack_into("<I", data, 0x3C, 0x40)
                data[0x40:0x44] = b"PE\0\0"
                struct.pack_into("<H", data, 0x44, 0x8664)
                if name == "wezterm-gui.exe":
                    data.extend(b"return-initial-exit-code")
                file.write_bytes(data)
            else:
                file.write_text(name)
        (bundle / "SOURCE_REVISION").write_text("zackees/wezterm@stale\n")
        monkeypatch.setenv("CLUD_KITTYTERM_BUNDLE_DIR", str(bundle))

    calls = []
    monkeypatch.setattr(build_wheel.process, "run", lambda *args, **kwargs: calls.append(args))

    with pytest.raises(RuntimeError, match=r"CLUD_KITTYTERM_BUNDLE_DIR|SOURCE_REVISION"):
        build_wheel.run_build("release")
    assert calls == []
    assert not build_wheel.DIST.exists()


def test_local_windows_build_rejects_paste_helper_before_maturin(monkeypatch, tmp_path):
    from ci import env as build_env_module

    monkeypatch.setattr(build_wheel, "DIST", tmp_path / "dist")
    monkeypatch.setattr(build_wheel, "local_webterm_target", lambda: build_wheel.KITTY_TARGET)
    monkeypatch.setattr(build_wheel, "resolve_kitty_bundle", lambda: tmp_path)
    monkeypatch.setattr(build_env_module, "build_env", lambda: {})
    calls = []

    class FailedBuild:
        returncode = 1

    monkeypatch.setattr(
        build_wheel.process,
        "run",
        lambda argv, **kwargs: calls.append(argv) or FailedBuild(),
    )

    with pytest.raises(RuntimeError, match="failed to build clud-kittyterm-paste"):
        build_wheel.run_build("release")
    assert len(calls) == 1
    assert calls[0][:2] == ["soldr", "build"]
    assert not build_wheel.DIST.exists()


def test_hook_rollout_target_is_a_shipped_script() -> None:
    """Whatever binary the rollout migrates hook configs to MUST be in the
    wheel. Reads NEW_COMMAND from the rollout source so a future rename
    (bad-cmd -> cmd-scan -> ...) cannot repeat #862: the rename lands, this
    fails until REQUIRED_SCRIPTS is extended too."""
    import re

    source = (
        build_wheel.ROOT / "crates" / "clud-bin" / "src" / "block_bad_cmd_rollout.rs"
    ).read_text(encoding="utf-8")
    match = re.search(r'const NEW_COMMAND: &str = "([^"]+)"', source)
    assert match, "NEW_COMMAND not found in block_bad_cmd_rollout.rs"
    assert match.group(1) in build_wheel.REQUIRED_SCRIPTS, (
        f"hook rollout targets `{match.group(1)}` but the wheel does not ship it"
    )


def test_required_scripts_are_declared_crate_binaries() -> None:
    """Every shipped script must be a real `[[bin]]` — a typo here would make
    the Windows packer fail at release time instead of test time."""
    cargo = (build_wheel.ROOT / "crates" / "clud-bin" / "Cargo.toml").read_text(
        encoding="utf-8"
    )
    import re

    declared = set(re.findall(r'^name = "(clud[^"]*)"', cargo, re.MULTILINE))
    for name in build_wheel.REQUIRED_SCRIPTS:
        assert name in declared, f"{name} is not a declared [[bin]] in clud-bin"


def test_verify_windows_wheel_scripts_uses_target_not_host(monkeypatch, tmp_path):
    monkeypatch.setattr(build_wheel.platform, "system", lambda: "Linux")
    wheel = tmp_path / "clud-2.3.0-py3-none-win_amd64.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        for name in build_wheel.REQUIRED_SCRIPTS:
            archive.writestr(f"clud-2.3.0.data/scripts/{name}.exe", b"")
        archive.writestr("clud-2.3.0.data/scripts/clud-webterm.exe", b"")
        archive.writestr("clud-2.3.0.dist-info/WHEEL", "Wheel-Version: 1.0\n")
    bundle = tmp_path / "fork-build"
    for name in KITTY_BUNDLE_FILES:
        file = bundle / name
        file.parent.mkdir(parents=True, exist_ok=True)
        if name.lower().endswith((".exe", ".dll")):
            pe = bytearray(0x46)
            pe[:2] = b"MZ"
            struct.pack_into("<I", pe, 0x3C, 0x40)
            pe[0x40:0x44] = b"PE\0\0"
            struct.pack_into("<H", pe, 0x44, 0x8664)
            if name == "wezterm-gui.exe":
                pe.extend(b"return-initial-exit-code\0")
            file.write_bytes(pe)
        else:
            file.write_bytes(name.encode())
    (bundle / "SOURCE_REVISION").write_text(f"zackees/wezterm@{KITTY_SOURCE_REVISION}\n")
    config = tmp_path / "clud-kittyterm.lua"
    config.write_text("return {}\n")
    helper = tmp_path / "clud-kittyterm-paste.exe"
    helper_data = bytearray(0x46)
    helper_data[:2] = b"MZ"
    struct.pack_into("<I", helper_data, 0x3C, 0x40)
    helper_data[0x40:0x44] = b"PE\0\0"
    struct.pack_into("<H", helper_data, 0x44, 0x8664)
    helper.write_bytes(helper_data)
    add_kitty_bundle(wheel, bundle, config, "x86_64-pc-windows-msvc", helper)

    assert build_wheel.verify_wheel_scripts(wheel) == 0


def test_verify_windows_wheel_scripts_requires_the_webterm_companion(monkeypatch, tmp_path):
    monkeypatch.setattr(build_wheel.platform, "system", lambda: "Linux")
    wheel = tmp_path / "clud-2.3.0-py3-none-win_amd64.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        for name in build_wheel.REQUIRED_SCRIPTS:
            archive.writestr(f"clud-2.3.0.data/scripts/{name}.exe", b"")

    assert build_wheel.verify_wheel_scripts(wheel) == 1


def test_verify_macos_wheel_scripts_requires_the_webterm_companion(monkeypatch, tmp_path):
    monkeypatch.setattr(build_wheel.platform, "system", lambda: "Linux")
    wheel = tmp_path / "clud-2.3.0-py3-none-macosx_11_0_arm64.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        for name in build_wheel.REQUIRED_SCRIPTS:
            archive.writestr(f"clud-2.3.0.data/scripts/{name}", b"")

    assert build_wheel.verify_wheel_scripts(wheel) == 1


def test_verify_windows_wheel_scripts_rejects_missing_native_helper(monkeypatch, tmp_path):
    monkeypatch.setattr(build_wheel.platform, "system", lambda: "Linux")
    wheel = tmp_path / "clud-2.3.0-py3-none-win_amd64.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        archive.writestr("clud-2.3.0.data/scripts/clud.exe", b"")
        archive.writestr("clud-2.3.0.data/scripts/clud-shim.exe", b"")

    assert build_wheel.verify_wheel_scripts(wheel) == 1


def test_wheels_changed_since_ignores_stale_wheels(monkeypatch, tmp_path):
    monkeypatch.setattr(build_wheel, "DIST", tmp_path)
    stale = tmp_path / "clud-2.2.0-py3-none-any.whl"
    stale.write_bytes(b"old")
    before = build_wheel.wheel_snapshot()

    fresh = tmp_path / "clud-2.3.0-py3-none-any.whl"
    fresh.write_bytes(b"new")

    assert build_wheel.wheels_changed_since(before) == [fresh]
