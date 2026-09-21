"""Build clud Rust binary and package as a Python wheel via maturin."""

from __future__ import annotations

import argparse
import base64
import contextlib
import hashlib
import json
import os
import platform
import shutil
import sys
import tempfile
import zipfile
from pathlib import Path
from typing import Literal

from ci import process
from ci.webterm_wheel import add_companion, companion_name, desktop_target
from ci.wheel_repair import repair_windows_gnu_wheel

ROOT = Path(__file__).resolve().parent.parent
DIST = ROOT / "dist"

BuildMode = Literal["dev", "release"]
# Every binary a shipped wheel must carry. This tuple is the single list the
# Windows wheel packer, the wheel verifier, and the installed-scripts verifier
# all iterate — extending the crate's `[[bin]]` set without extending this
# ships a wheel missing the new binary ON WINDOWS ONLY, because the manylinux
# wheels are maturin-built (all bins) while Windows wheels are hand-packed
# from exactly this list. That is how 2.5.5 shipped a hook rollout pointing
# configs at `clud-cmd-scan` while the win_amd64 wheel didn't contain it
# (#862). `test_hook_rollout_target_is_a_shipped_script` pins the invariant.
REQUIRED_SCRIPTS = ("clud", "clud-shim", "clud-block-bad-cmd", "clud-cmd-scan")


def prune_nonproduction_scripts(wheel: Path) -> bool:
    """Remove maturin's test-only executables from a binary wheel.

    Maturin's ``bindings = "bin"`` packages every enabled Cargo ``[[bin]]``.
    That is useful for the production helpers, but also used to publish
    ``clud-ctrlc-probe``, whose only callers are integration tests. Windows is
    hand-packed from ``REQUIRED_SCRIPTS`` already; this closes the equivalent
    native/maturin path without hiding the probe from test bundles.
    """
    with zipfile.ZipFile(wheel) as archive:
        members = archive.namelist()
        stale = [
            name
            for name in members
            if ".data/scripts/clud-" in name
            and Path(name).name.removesuffix(".exe") not in REQUIRED_SCRIPTS
        ]
        record = next((name for name in members if name.endswith(".dist-info/RECORD")), None)
        if not stale or record is None:
            return False
        with tempfile.TemporaryDirectory(prefix="clud-wheel-prune-") as temp_dir:
            root = Path(temp_dir)
            archive.extractall(root)
            for name in stale:
                (root / name).unlink()
            from ci.wheel_repair import _rewrite_record, _write_wheel

            _rewrite_record(root, Path(record))
            replacement = wheel.with_suffix(".pruned.whl")
            _write_wheel(root, replacement)
    replacement.replace(wheel)
    return True


def resolve_elf_objcopy() -> str:
    """Find an objcopy that can edit the configured Linux target's ELF files.

    The native developer environment normally exposes llvm-objcopy. Soldr's
    catalogue GNU cross toolchain instead exports a target-prefixed GNU binary
    next to its compiler (for example aarch64-conda-linux-gnu-objcopy).
    A cross target deliberately never derives a tool from generic ``CC`` or
    falls back to generic ``objcopy``: either can be a host-only GNU binary.
    """
    target = os.environ.get("CARGO_BUILD_TARGET", "")
    if target:
        from ci.env import host_target_triple

        is_cross_target = target != host_target_triple()
    else:
        is_cross_target = False
    target_key = target.upper().replace("-", "_")
    target_cc_key = target.lower().replace("-", "_")
    candidates = [
        os.environ.get("OBJCOPY"),
        os.environ.get(f"OBJCOPY_{target_key}") if target_key else None,
    ]

    target_compilers = (
        os.environ.get(f"CARGO_TARGET_{target_key}_LINKER") if target_key else None,
        # cc-rs and Soldr use the lowercase target spelling for CC_<target>.
        os.environ.get(f"CC_{target_cc_key}") if target_cc_key else None,
        os.environ.get(f"CC_{target_key}") if target_key else None,
    )
    compilers = (
        target_compilers
        if is_cross_target
        else (*target_compilers, os.environ.get("CC"))
    )
    for compiler in compilers:
        if not compiler:
            continue
        compiler_path = Path(compiler)
        for suffix in ("gcc", "clang", "cc"):
            if compiler_path.name.endswith(suffix):
                candidates.append(
                    str(
                        compiler_path.with_name(
                            f"{compiler_path.name.removesuffix(suffix)}objcopy"
                        )
                    )
                )
                break

    # LLVM is cross-capable; generic GNU objcopy is safe only for a native build.
    candidates.append("llvm-objcopy")
    if not is_cross_target:
        candidates.append("objcopy")
    for candidate in candidates:
        if candidate and shutil.which(candidate):
            return candidate
    raise RuntimeError(
        "no compatible ELF objcopy found; set OBJCOPY or expose the target toolchain"
    )


def remove_elf_debug_metadata(wheel: Path) -> bool:
    """Remove the residual ELF debug-GDB section from release wheel scripts.

    Cargo's ``strip = "debuginfo"`` removes DWARF but intentionally retains
    ``.debug_gdb_scripts``. It is not symbol data, yet its debug-prefixed name
    violates the artifact contract and confused the original audit. objcopy
    removes that one section without touching ``.symtab``.
    """
    with zipfile.ZipFile(wheel) as archive:
        members = archive.namelist()
        scripts = [name for name in members if ".data/scripts/" in name]
        record = next((name for name in members if name.endswith(".dist-info/RECORD")), None)
        if record is None:
            return False
        with tempfile.TemporaryDirectory(prefix="clud-wheel-strip-") as temp_dir:
            root = Path(temp_dir)
            archive.extractall(root)
            elf_scripts = [
                root / name for name in scripts if (root / name).read_bytes()[:4] == b"\x7fELF"
            ]
            if not elf_scripts:
                return False
            objcopy = resolve_elf_objcopy()
            for script in elf_scripts:
                result = process.run(
                    [objcopy, "--remove-section=.debug_gdb_scripts", str(script)],
                    check=False,
                )
                if result.returncode != 0:
                    raise RuntimeError(f"failed to remove debug metadata from {script}")
            from ci.wheel_repair import _rewrite_record, _write_wheel

            _rewrite_record(root, Path(record))
            replacement = wheel.with_suffix(".stripped.whl")
            _write_wheel(root, replacement)
    replacement.replace(wheel)
    return True


def verify_no_elf_debug_sections(wheel: Path) -> None:
    """Fail a release build if any shipped ELF script has `.debug_*` data."""
    with zipfile.ZipFile(wheel) as archive:
        scripts = [name for name in archive.namelist() if ".data/scripts/" in name]
        with tempfile.TemporaryDirectory(prefix="clud-wheel-verify-") as temp_dir:
            root = Path(temp_dir)
            archive.extractall(root)
            for name in scripts:
                script = root / name
                if script.read_bytes()[:4] != b"\x7fELF":
                    continue
                result = process.run(
                    ["readelf", "-SW", str(script)], capture_output=True, text=True, check=False
                )
                if result.returncode != 0:
                    raise RuntimeError(f"readelf failed for shipped script {name}")
                if ".debug_" in result.stdout:
                    raise RuntimeError(f"shipped script retains debug sections: {name}")


def local_webterm_target() -> str | None:
    from ci.env import host_target_triple

    target = host_target_triple()
    return target if desktop_target(target) else None


def build_local_webterm_companion(
    *, mode: BuildMode, target: str, env: dict[str, str]
) -> Path:
    """Build the native Tauri companion for a local desktop wheel."""
    command = [
        "soldr",
        "build",
        "--manifest-path",
        str(ROOT / "clud-webterm" / "Cargo.toml"),
    ]
    if mode == "release":
        command.append("--release")
    result = process.run(command, cwd=ROOT, check=False, env=env)
    if result.returncode != 0:
        raise RuntimeError("failed to build clud-webterm companion")
    profile = "release" if mode == "release" else "debug"
    # build_env sets CARGO_BUILD_TARGET on desktop hosts, so Cargo places the
    # artifact under the target triple even though this local invocation does
    # not repeat `--target` on its command line.
    companion = ROOT / "clud-webterm" / "target" / target / profile / companion_name(target)
    if not companion.is_file():
        raise RuntimeError(f"web terminal build produced no companion: {companion}")
    return companion


def build_command(mode: BuildMode, env: dict[str, str] | None = None) -> list[str]:
    from ci.env import maturin_argv

    subcommand = [
        "build",
        "--interpreter",
        sys.executable,
        "--out",
        str(DIST),
    ]
    if mode == "dev":
        subcommand.extend(["--profile", "dev"])
    else:
        subcommand.append("--release")
        if platform.system() == "Linux":
            # Local, non-shipping wheel: no zig (banned everywhere, soldr#2299).
            # The manylinux_2_17 release wheel is produced by CI's blessed path
            # (ci/xbuild.py: soldr's catalogue toolchain + static libstdc++);
            # a bare local build cannot meet that floor, so tag it `linux` and
            # skip the audit rather than claim a floor it did not enforce.
            subcommand.extend(["--compatibility", "linux"])
        else:
            subcommand.extend(["--compatibility", "pypi"])
    # Use the dev-venv maturin via `python -m maturin`. setup-soldr shims keep
    # maturin-spawned cargo in the soldr/zccache path; routing maturin itself
    # through soldr fails on Linux because PyO3/maturin only publishes musl
    # Linux release assets.
    return maturin_argv(subcommand, env=env)


def build_windows_wheel_from_binaries(
    *,
    target: str,
    profile: str,
    target_dir: Path,
    dist_dir: Path,
    version: str,
) -> Path:
    """Package executables already built by soldr into a Windows wheel.

    Maturin's Linux-to-MSVC path brings its own xwin downloader, duplicating
    the SDK preparation that soldr already owns. The binaries in this wheel
    have therefore been built exclusively by the preceding soldr invocation.
    """
    platform_tag = {"x86_64": "win_amd64", "aarch64": "win_arm64"}[target.split("-", 1)[0]]
    distribution = f"clud-{version}"
    wheel = dist_dir / f"{distribution}-py3-none-{platform_tag}.whl"
    binaries = target_dir / target / profile
    scripts = []
    for name in REQUIRED_SCRIPTS:
        binary = binaries / f"{name}.exe"
        if not binary.is_file():
            raise RuntimeError(f"soldr-built Windows executable is missing: {binary}")
        scripts.append((f"{distribution}.data/scripts/{name}.exe", binary.read_bytes()))

    metadata = (
        "Metadata-Version: 2.1\n"
        "Name: clud\n"
        f"Version: {version}\n"
        "Summary: Fast Rust CLI for running Claude Code and Codex in YOLO mode\n"
    ).encode()
    wheel_metadata = (
        "Wheel-Version: 1.0\n"
        "Generator: clud ci.build_wheel\n"
        "Root-Is-Purelib: false\n"
        f"Tag: py3-none-{platform_tag}\n"
    ).encode()
    package_source = ROOT / "src" / "clud" / "__init__.py"
    members = [
        ("clud/__init__.py", package_source.read_bytes()),
        *scripts,
        (f"{distribution}.dist-info/METADATA", metadata),
        (f"{distribution}.dist-info/WHEEL", wheel_metadata),
    ]
    records = [
        f"{name},sha256={base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b'=').decode()},{len(data)}"
        for name, data in members
    ]
    records.append(f"{distribution}.dist-info/RECORD,,")
    dist_dir.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(wheel, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, data in members:
            archive.writestr(name, data)
        archive.writestr(f"{distribution}.dist-info/RECORD", "\n".join(records) + "\n")
    return wheel


def built_wheels() -> list[Path]:
    return sorted(DIST.glob("clud-*.whl"), key=lambda path: path.stat().st_mtime)


def wheel_snapshot() -> dict[str, int]:
    return {path.name: path.stat().st_mtime_ns for path in built_wheels()}


def wheels_changed_since(snapshot: dict[str, int]) -> list[Path]:
    return [path for path in built_wheels() if snapshot.get(path.name) != path.stat().st_mtime_ns]


def latest_wheel() -> Path:
    wheels = built_wheels()
    if not wheels:
        raise RuntimeError(f"no built wheel found in {DIST}")
    return wheels[-1]


def install_wheel(wheel: Path, *, env: dict[str, str]) -> int:
    install = process.run(
        [
            "uv",
            "pip",
            "install",
            "--python",
            sys.executable,
            "--reinstall",
            "--no-deps",
            str(wheel),
        ],
        cwd=ROOT,
        check=False,
        env=env,
    )
    if install.returncode != 0:
        return install.returncode

    for pth in (ROOT / ".venv").glob("**/site-packages/clud.pth"):
        with contextlib.suppress(OSError):
            pth.unlink()
    return verify_installed_scripts(env=env)


def _script_name(name: str) -> str:
    return f"{name}.exe" if platform.system() == "Windows" else name


def _wheel_script_name(wheel: Path, name: str) -> str:
    """Return the script filename for the wheel target, not the build host."""
    platform_tag = wheel.stem.rsplit("-", 1)[-1].lower()
    is_windows = any(tag.startswith("win") for tag in platform_tag.split("."))
    return f"{name}.exe" if is_windows else name


def _installed_script(name: str) -> Path:
    return Path(sys.executable).parent / _script_name(name)


def verify_installed_scripts(*, env: dict[str, str]) -> int:
    required = list(REQUIRED_SCRIPTS)
    target = local_webterm_target()
    if target is not None:
        required.append(companion_name(target).removesuffix(".exe"))
    missing = [name for name in required if not _installed_script(name).is_file()]
    if missing:
        print(
            "installed wheel is missing scripts: " + ", ".join(missing),
            file=sys.stderr,
            flush=True,
        )
        return 1

    # Hook smoke tests need the same trusted rm PATH contract as a session.
    # Never invoke this alias: the only payloads below are inert hook JSON.
    with tempfile.TemporaryDirectory(prefix="clud-wheel-rm-") as directory:
        alias = Path(directory) / _script_name("rm")
        shutil.copyfile(_installed_script("clud-shim"), alias)
        alias.chmod(0o755)
        smoke_env = env | {"PATH": directory + os.pathsep + env.get("PATH", "")}
        return _verify_installed_smokes(env=smoke_env, target=target)


def _verify_installed_smokes(*, env: dict[str, str], target: str | None) -> int:
    guard = _installed_script("clud-block-bad-cmd")
    deny_payload = json.dumps(
        {
            "tool_name": "Bash",
            "tool_input": {"command": "bad" + " cmd"},
        }
    )
    deny = process.run(
        [str(guard)],
        input=deny_payload,
        text=True,
        capture_output=True,
        check=False,
        timeout=5,
        env=env,
    )
    if deny.returncode != 2 or "permissionDecision" not in deny.stdout or "deny" not in deny.stdout:
        print(
            "installed clud-block-bad-cmd deny smoke failed: "
            f"rc={deny.returncode} stdout={deny.stdout!r} stderr={deny.stderr!r}",
            file=sys.stderr,
            flush=True,
        )
        return 1

    allow_payload = json.dumps(
        {
            "tool_name": "Bash",
            "tool_input": {"command": "echo ok"},
        }
    )
    allow = process.run(
        [str(guard)],
        input=allow_payload,
        text=True,
        capture_output=True,
        check=False,
        timeout=5,
        env=env,
    )
    if allow.returncode != 0:
        print(
            "installed clud-block-bad-cmd allow smoke failed: "
            f"rc={allow.returncode} stdout={allow.stdout!r} stderr={allow.stderr!r}",
            file=sys.stderr,
            flush=True,
        )
        return 1

    if target is not None:
        webterm = _installed_script(companion_name(target).removesuffix(".exe"))
        startup = process.run(
            [str(webterm), "--startup-check"],
            text=True,
            capture_output=True,
            check=False,
            timeout=5,
            env=env,
        )
        if startup.returncode != 0:
            print(
                "installed clud-webterm startup smoke failed: "
                f"rc={startup.returncode} stdout={startup.stdout!r} stderr={startup.stderr!r}",
                file=sys.stderr,
                flush=True,
            )
            return 1

    return 0


def verify_wheel_scripts(wheel: Path) -> int:
    with zipfile.ZipFile(wheel) as archive:
        members = {name.replace("\\", "/") for name in archive.namelist()}
    required = list(REQUIRED_SCRIPTS)
    platform_tag = wheel.stem.rsplit("-", 1)[-1].lower()
    if any(tag.startswith(("win", "macosx")) for tag in platform_tag.split(".")):
        required.append("clud-webterm")
    missing = []
    for name in required:
        script = _wheel_script_name(wheel, name)
        if not any(member.endswith(f".data/scripts/{script}") for member in members):
            missing.append(script)
    if missing:
        print(
            f"built wheel {wheel.name} is missing scripts: " + ", ".join(missing),
            file=sys.stderr,
            flush=True,
        )
        return 1
    unexpected = [
        member
        for member in members
        if ".data/scripts/clud-" in member
        and Path(member).name.removesuffix(".exe") not in required
    ]
    if unexpected:
        print(
            f"built wheel {wheel.name} contains non-production scripts: " + ", ".join(unexpected),
            file=sys.stderr,
            flush=True,
        )
        return 1
    return 0


def run_build(mode: BuildMode) -> int:
    from ci.env import build_env

    env = build_env()
    DIST.mkdir(parents=True, exist_ok=True)
    before = wheel_snapshot()
    cmd = build_command(mode, env=env)
    print(f"build mode: {mode}", file=sys.stderr, flush=True)
    result = process.run(cmd, cwd=ROOT, check=False, env=env)
    if result.returncode != 0:
        return result.returncode
    changed_wheels = wheels_changed_since(before)
    if not changed_wheels:
        print("build completed but produced no wheel", file=sys.stderr, flush=True)
        return 1
    target = local_webterm_target()
    companion = (
        build_local_webterm_companion(mode=mode, target=target, env=env)
        if target is not None
        else None
    )
    for wheel in changed_wheels:
        prune_nonproduction_scripts(wheel)
        if mode == "release":
            remove_elf_debug_metadata(wheel)
            verify_no_elf_debug_sections(wheel)
        repair_windows_gnu_wheel(wheel)
        if companion is not None and target is not None:
            add_companion(wheel, companion, target)
        verify = verify_wheel_scripts(wheel)
        if verify != 0:
            return verify
    if mode != "dev":
        return 0

    wheel = latest_wheel()
    action = "reinstalling existing dev wheel" if wheel.name in before else "installing dev wheel"
    print(f"{action}: {wheel.name}", file=sys.stderr, flush=True)
    return install_wheel(wheel, env=env)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Build clud")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--dev", action="store_true", help="build dev-profile wheel and reinstall")
    mode.add_argument("--release", action="store_true", help="build release wheel(s) into dist/")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None, *, default_mode: BuildMode = "release") -> int:
    args = parse_args(argv)
    mode: BuildMode = default_mode
    if args.dev:
        mode = "dev"
    if args.release:
        mode = "release"
    return run_build(mode)


if __name__ == "__main__":
    sys.exit(main())
