"""Build clud Rust binary and package as a Python wheel via maturin."""

from __future__ import annotations

import argparse
import base64
import contextlib
import hashlib
import json
import platform
import sys
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

    return 0


def verify_wheel_scripts(wheel: Path) -> int:
    with zipfile.ZipFile(wheel) as archive:
        members = {name.replace("\\", "/") for name in archive.namelist()}
    missing = []
    for name in REQUIRED_SCRIPTS:
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
