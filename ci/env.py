"""Build environment setup for clud CI."""

from __future__ import annotations

import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path

import tomllib


def cargo_home() -> Path:
    if os.environ.get("CARGO_HOME"):
        return Path(os.environ["CARGO_HOME"]).expanduser()
    return Path.home() / ".cargo"


def cargo_bin() -> Path:
    return cargo_home() / "bin"


def rustup_home() -> Path:
    if os.environ.get("RUSTUP_HOME"):
        return Path(os.environ["RUSTUP_HOME"]).expanduser()
    return Path.home() / ".rustup"


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def toolchain_file() -> Path:
    return repo_root() / "rust-toolchain.toml"


def load_toolchain_channel() -> str:
    with toolchain_file().open("rb") as handle:
        data = tomllib.load(handle)
    toolchain = data.get("toolchain")
    if not isinstance(toolchain, dict):
        raise RuntimeError(f"missing [toolchain] in {toolchain_file()}")
    channel = toolchain.get("channel")
    if not isinstance(channel, str) or not channel:
        raise RuntimeError(f"missing toolchain.channel in {toolchain_file()}")
    return channel


def host_target_triple() -> str:
    system = platform.system()
    machine = platform.machine().lower()
    arch = {
        "amd64": "x86_64",
        "x86_64": "x86_64",
        "arm64": "aarch64",
        "aarch64": "aarch64",
    }.get(machine)
    if arch is None:
        raise RuntimeError(f"unsupported architecture: {machine}")
    if system == "Windows":
        # This repo builds Windows artifacts with the rust-toolchain override,
        # which points at MSVC. `cargo -Vv` can still report the ambient GNU
        # rustup proxy host, and using that value here causes wheel builds to
        # pick the wrong target triple.
        return f"{arch}-pc-windows-msvc"
    detected = _cargo_host_triple()
    if detected:
        return detected
    if system == "Linux":
        return f"{arch}-unknown-linux-gnu"
    if system == "Darwin":
        return f"{arch}-apple-darwin"
    raise RuntimeError(f"unsupported platform: {system}")


def _cargo_host_triple() -> str | None:
    cargo = shutil.which("cargo")
    if not cargo:
        return None
    result = subprocess.run(
        [cargo, "-Vv"],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        return None
    for line in result.stdout.splitlines():
        if line.startswith("host: "):
            return line.removeprefix("host: ").strip() or None
    return None


def toolchain_name() -> str:
    return f"{load_toolchain_channel()}-{host_target_triple()}"


def toolchain_bin() -> Path:
    return rustup_home() / "toolchains" / toolchain_name() / "bin"


def _find_vswhere() -> Path | None:
    candidates = [
        Path(r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe"),
        Path(r"C:\Program Files\Microsoft Visual Studio\Installer\vswhere.exe"),
    ]
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    return None


def _find_vsdevcmd() -> Path | None:
    vswhere = _find_vswhere()
    if vswhere is None:
        return None
    result = subprocess.run(
        [
            str(vswhere),
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    installation_path = result.stdout.strip()
    if not installation_path:
        return None
    candidate = Path(installation_path) / "Common7" / "Tools" / "VsDevCmd.bat"
    if candidate.is_file():
        return candidate
    return None


def _windows_build_env() -> dict[str, str]:
    env = os.environ.copy()
    toolchain_bin_dir = toolchain_bin()
    if toolchain_bin_dir.is_dir():
        env["PATH"] = str(toolchain_bin_dir) + os.pathsep + env.get("PATH", "")
        cargo_exe = toolchain_bin_dir / "cargo.exe"
        rustc_exe = toolchain_bin_dir / "rustc.exe"
        if cargo_exe.is_file():
            env["CARGO"] = str(cargo_exe)
        if rustc_exe.is_file():
            env["RUSTC"] = str(rustc_exe)
        env["RUSTUP_TOOLCHAIN"] = toolchain_name()
        env["CARGO_BUILD_TARGET"] = host_target_triple()

    gnu_runtime = _find_windows_gnu_runtime_bin()
    if gnu_runtime is not None and env.get("CARGO_BUILD_TARGET", "").endswith("-gnu"):
        env["PATH"] = str(gnu_runtime) + os.pathsep + env.get("PATH", "")

    vsdevcmd = _find_vsdevcmd()
    if vsdevcmd is None:
        return env

    command = f'"{vsdevcmd}" -arch=x64 -host_arch=x64 >nul && set'
    result = subprocess.run(
        ["cmd", "/d", "/s", "/c", command],
        check=False,
        capture_output=True,
        text=True,
        env=env,
    )
    if result.returncode != 0:
        return env
    for line in result.stdout.splitlines():
        if "=" not in line:
            continue
        key, value = line.split("=", 1)
        env[key] = value
    return env


def _find_windows_gnu_runtime_bin() -> Path | None:
    candidates = [
        Path(r"C:\msys64\ucrt64\bin"),
        Path(r"C:\msys64\mingw64\bin"),
        Path(r"C:\Qt\Tools\mingw1120_64\bin"),
        Path(r"C:\MinGW\bin"),
    ]
    for candidate in candidates:
        if (candidate / "libstdc++-6.dll").is_file():
            return candidate
    return None


def activate() -> None:
    bin_dir = cargo_bin()
    if not bin_dir.is_dir():
        return

    current_path = os.environ.get("PATH", "")
    path_parts = current_path.split(os.pathsep) if current_path else []
    normalized_cargo_bin = os.path.normcase(os.path.normpath(str(bin_dir)))
    normalized_parts = {
        os.path.normcase(os.path.normpath(part)) for part in path_parts if part
    }
    if normalized_cargo_bin in normalized_parts:
        return
    os.environ["PATH"] = (
        str(bin_dir) if not current_path else str(bin_dir) + os.pathsep + current_path
    )


def _apply_sccache(env: dict[str, str]) -> dict[str, str]:
    if env.get("RUSTC_WRAPPER"):
        return env
    sccache = shutil.which("sccache", path=env.get("PATH"))
    if sccache:
        env["RUSTC_WRAPPER"] = sccache
    return env


def clean_env() -> dict[str, str]:
    activate()
    env = os.environ.copy()
    env.pop("VIRTUAL_ENV", None)
    env.setdefault("PYTHONUTF8", "1")
    if platform.system() == "Windows":
        env = env | _windows_build_env()
        env.pop("VIRTUAL_ENV", None)
        env.setdefault("PYTHONUTF8", "1")
    env.setdefault("RUSTUP_TOOLCHAIN", toolchain_name())
    env = _apply_sccache(env)
    return env


def build_env() -> dict[str, str]:
    return clean_env()


def soldr_path(env: dict[str, str] | None = None) -> str | None:
    """Return the path to the soldr binary, or None if not available.

    soldr (https://github.com/zackees/soldr) is pulled in as a dev dependency
    and proxies cargo/rustc/maturin invocations through the rustup-managed
    toolchain (via `rustup which`). On Windows, developer machines that have
    a chocolatey-installed GNU-host cargo first on PATH produce binaries
    that link MinGW runtime DLLs (libstdc++-6.dll etc.), which fail to load
    on stock Windows. CI runners today happen to have the MSVC rustup
    toolchain first on PATH, but that's luck — this helper pins the
    behavior by making every cargo/maturin CI call go through soldr.
    """
    path = None if env is None else env.get("PATH")
    return shutil.which("soldr", path=path)


def cargo_argv(subcommand: list[str], env: dict[str, str] | None = None) -> list[str]:
    """Return the cargo argv, preferring `soldr cargo` on every platform.

    Issue #27 pinned this on Windows; issue #68 extends it to all platforms
    so that local dev and CI (via `zackees/setup-soldr@v0`) go through the
    same rustup-resolved toolchain. Falls back to bare `cargo` when soldr
    isn't on PATH — matches `tests/integration/conftest.py::_cargo_argv`.
    """
    soldr = soldr_path(env)
    if soldr:
        return [soldr, "cargo", *subcommand]
    return ["cargo", *subcommand]


def maturin_argv(subcommand: list[str], env: dict[str, str] | None = None) -> list[str]:
    """Return the maturin argv, always using the dev-venv install.

    Originally this routed through `soldr maturin` (issues #27, #68) on the
    assumption that soldr would resolve maturin via the venv. In practice,
    `soldr <tool>` always tries to fetch the tool from GitHub Releases,
    and PyO3/maturin only publishes `x86_64-unknown-linux-musl.tar.gz` /
    `aarch64-unknown-linux-musl.tar.gz` for Linux — there is no
    `*-unknown-linux-gnu` asset. soldr's asset matcher rejects the musl
    archives on GNU Linux runners (its musl→gnu fallback rule does not
    fire here), producing `tool not found: no asset matches target
    x86_64-unknown-linux-gnu` and breaking every Linux Build job.

    maturin is already pinned in pyproject.toml dev deps and installed
    into the uv-managed venv on every runner, so `python -m maturin`
    works uniformly across platforms without any GitHub-Releases lookup.
    The Windows MSVC pin from issue #27 is preserved through the cargo
    side: `_windows_build_env` exports `CARGO`/`RUSTC`/`RUSTUP_TOOLCHAIN`
    + prepends the MSVC toolchain bin to PATH, so when this maturin
    invocation spawns cargo it picks up the same MSVC toolchain that
    `soldr cargo` would have given it.
    """
    return [sys.executable, "-m", "maturin", *subcommand]
