"""Build a Dylint driver from the git revision used by the lint crate."""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

DYLINT_REPO = "https://github.com/trailofbits/dylint"
DYLINT_REV = "4bd91ce7729b74c7ee5664bbb588f7baf30b4a09"
TOOLCHAIN_CHANNEL = "nightly-2026-03-26"


def run(args: list[str], **kwargs) -> subprocess.CompletedProcess[str]:
    print("+", " ".join(args), flush=True)
    kwargs.setdefault("timeout", 600)
    return subprocess.run(args, check=True, text=True, **kwargs)


def rustc_host() -> str:
    output = subprocess.check_output(
        ["rustup", "run", TOOLCHAIN_CHANNEL, "rustc", "-vV"],
        text=True,
        timeout=60,
    )
    for line in output.splitlines():
        if line.startswith("host: "):
            return line.split("host: ", 1)[1]
    raise RuntimeError("could not determine rustc host triple")


def rustc_toolchain_root(full_toolchain: str) -> Path:
    rustc = subprocess.check_output(
        ["rustup", "which", "--toolchain", full_toolchain, "rustc"],
        text=True,
        timeout=30,
    ).strip()
    return Path(rustc).resolve().parent.parent


def write_driver_package(package: Path, dylint_checkout: Path, full_toolchain: str) -> None:
    src = package / "src"
    src.mkdir(parents=True)

    driver_path = str((dylint_checkout / "driver").resolve()).replace("\\", "\\\\")
    (package / "Cargo.toml").write_text(
        f"""
[package]
name = "dylint_driver-{full_toolchain}"
version = "0.1.0"
edition = "2018"

[dependencies]
anyhow = "1.0"
env_logger = "0.11"
dylint_driver = {{ path = "{driver_path}" }}
""".lstrip(),
        encoding="utf-8",
    )
    (package / "rust-toolchain.toml").write_text(
        f"""
[toolchain]
channel = "{full_toolchain}"
components = ["llvm-tools-preview", "rustc-dev"]
""".lstrip(),
        encoding="utf-8",
    )
    (src / "main.rs").write_text(
        """
#![feature(rustc_private)]

use anyhow::Result;
use std::env;

pub fn main() -> Result<()> {
    env_logger::init();

    let args: Vec<_> = env::args_os().collect();
    dylint_driver::dylint_driver(&args)
}
""".lstrip(),
        encoding="utf-8",
    )


def append_github_env(name: str, value: Path) -> None:
    github_env = os.environ.get("GITHUB_ENV")
    if github_env:
        with open(github_env, "a", encoding="utf-8") as file:
            file.write(f"{name}={value}\n")


def main() -> int:
    full_toolchain = f"{TOOLCHAIN_CHANNEL}-{rustc_host()}"
    runner_temp = Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir())).resolve()
    driver_root = runner_temp / "dylint-drivers"
    driver_dir = driver_root / full_toolchain
    driver_dir.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="clud-dylint-") as temp:
        temp_path = Path(temp)
        checkout = temp_path / "dylint"
        package = temp_path / "driver-package"

        run(["git", "clone", "--filter=blob:none", DYLINT_REPO, str(checkout)])
        run(["git", "-C", str(checkout), "checkout", DYLINT_REV])

        package.mkdir()
        write_driver_package(package, checkout, full_toolchain)

        env = os.environ.copy()
        env["RUSTUP_TOOLCHAIN"] = full_toolchain
        nightly_bin = rustc_toolchain_root(full_toolchain) / "bin"
        rustc_exe = nightly_bin / ("rustc.exe" if os.name == "nt" else "rustc")
        cargo_exe = nightly_bin / ("cargo.exe" if os.name == "nt" else "cargo")
        if rustc_exe.exists():
            env["RUSTC"] = str(rustc_exe)
        if cargo_exe.exists():
            env["CARGO"] = str(cargo_exe)
        if os.name != "nt":
            toolchain_root = rustc_toolchain_root(full_toolchain)
            rpath = f"-C link-args=-Wl,-rpath,{toolchain_root / 'lib'}"
            env["RUSTFLAGS"] = f"{env.get('RUSTFLAGS', '')} {rpath}".strip()

        run(
            ["rustup", "run", TOOLCHAIN_CHANNEL, "cargo", "build"],
            cwd=package,
            env=env,
        )

        exe_suffix = ".exe" if os.name == "nt" else ""
        built_driver = package / "target" / "debug" / f"dylint_driver-{full_toolchain}{exe_suffix}"
        installed_driver = driver_dir / "dylint-driver"
        shutil.copy2(built_driver, installed_driver)
        if os.name == "nt":
            shutil.copy2(built_driver, driver_dir / "dylint-driver.exe")

    append_github_env("DYLINT_DRIVER_PATH", driver_root)
    print(f"DYLINT_DRIVER_PATH={driver_root}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
