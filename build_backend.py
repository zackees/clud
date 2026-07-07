from __future__ import annotations

import os
import platform
import shlex
import shutil
import struct
import subprocess
import sys
from collections.abc import Mapping
from pathlib import Path
from typing import Any

from ci.env import build_env
from ci.wheel_repair import repair_windows_gnu_wheel

_SOLDR_PEP517_TIMEOUT_SECONDS = 600
_FALSE_VALUES = {"0", "false", "no", "off"}


def _script_name(name: str) -> str:
    return f"{name}.exe" if platform.system() == "Windows" else name


def _soldr_executable() -> str:
    build_env_soldr = Path(sys.executable).parent / _script_name("soldr")
    if build_env_soldr.is_file():
        return str(build_env_soldr)
    return shutil.which("soldr") or "soldr"


def _soldr_build_env() -> dict[str, str]:
    env = build_env()
    soldr = _soldr_executable()
    soldr_dir = Path(soldr).parent
    if soldr_dir != Path("."):
        env["PATH"] = str(soldr_dir) + os.pathsep + env.get("PATH", "")
    env.setdefault("RUSTC_WRAPPER", soldr)
    env.setdefault("ZCCACHE_PATH_REMAP", "auto")

    knob = env.get("SOLDR_PEP517_STABLE_TARGET_DIR", "").strip().lower()
    if knob not in _FALSE_VALUES:
        env.setdefault(
            "CARGO_TARGET_DIR",
            str(Path.home() / ".soldr" / "cargo-target" / "wheel-build"),
        )
    return env


def _maturin_pep517(
    subcommand: str,
    *args: str,
    env_overrides: Mapping[str, str] | None = None,
) -> None:
    cmd = [_soldr_executable(), "maturin", "pep517", subcommand, *args]
    env = _soldr_build_env()
    if env_overrides:
        env.update(env_overrides)
    try:
        subprocess.check_call(cmd, env=env, timeout=_SOLDR_PEP517_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(
            "soldr maturin pep517 exceeded 600s; suspect zccache daemon wedge - "
            "try `soldr status` to inspect."
        ) from exc


def _additional_pep517_args() -> list[str]:
    if platform.system().lower() == "windows" and platform.machine().lower() == "amd64":
        pointer_width = struct.calcsize("P") * 8
        if pointer_width == 32:
            return ["--target", "i686-pc-windows-msvc"]
    return []


def _maturin_pep517_args(config_settings: Mapping[str, Any] | None) -> list[str]:
    build_args = None
    if config_settings:
        build_args = config_settings.get("maturin.build-args", config_settings.get("build-args"))
    if build_args is None:
        return shlex.split(os.getenv("MATURIN_PEP517_ARGS", ""))
    if isinstance(build_args, str):
        return shlex.split(build_args)
    return [str(arg) for arg in build_args]


def _has_interpreter_arg(args: list[str]) -> bool:
    for arg in args:
        if arg in {"--interpreter", "-i"} or arg.startswith("--interpreter="):
            return True
    return False


def _has_compatibility_arg(args: list[str]) -> bool:
    for arg in args:
        if arg in {"--compatibility", "--manylinux"}:
            return True
        if arg.startswith("--compatibility=") or arg.startswith("--manylinux="):
            return True
    return False


def _wheel_options(
    config_settings: Mapping[str, Any] | None,
    *,
    editable: bool = False,
) -> list[str]:
    options = _additional_pep517_args()
    if editable:
        options.append("--editable")
    options.extend(_maturin_pep517_args(config_settings))
    if not _has_compatibility_arg(options):
        options = ["--compatibility", "off", *options]
    if not _has_interpreter_arg(options):
        options.extend(["--interpreter", sys.executable])
    return options


def _newest_entry(directory: str, suffix: str, *, want_dir: bool) -> str:
    entries: list[tuple[float, str]] = []
    for name in os.listdir(directory):
        if not name.endswith(suffix):
            continue
        path = Path(directory, name)
        if want_dir and not path.is_dir():
            continue
        if not want_dir and not path.is_file():
            continue
        entries.append((path.stat().st_mtime, name))
    if not entries:
        kind = "directory" if want_dir else "file"
        raise RuntimeError(f"build backend: no {suffix} {kind} produced in {directory}")
    entries.sort(reverse=True)
    return entries[0][1]


def build_wheel(
    wheel_directory: str,
    config_settings: Mapping[str, Any] | None = None,
    metadata_directory: str | None = None,
) -> str:
    env_overrides = {}
    if metadata_directory is not None:
        env_overrides["MATURIN_PEP517_METADATA_DIR"] = metadata_directory
    _maturin_pep517(
        "build-wheel",
        "--out",
        wheel_directory,
        *_wheel_options(config_settings),
        env_overrides=env_overrides,
    )
    filename = _newest_entry(wheel_directory, ".whl", want_dir=False)
    repair_windows_gnu_wheel(Path(wheel_directory) / filename)
    return filename


def build_editable(
    wheel_directory: str,
    config_settings: Mapping[str, Any] | None = None,
    metadata_directory: str | None = None,
) -> str:
    env_overrides = {}
    if metadata_directory is not None:
        env_overrides["MATURIN_PEP517_METADATA_DIR"] = metadata_directory
    _maturin_pep517(
        "build-wheel",
        "--out",
        wheel_directory,
        *_wheel_options(config_settings, editable=True),
        env_overrides=env_overrides,
    )
    filename = _newest_entry(wheel_directory, ".whl", want_dir=False)
    repair_windows_gnu_wheel(Path(wheel_directory) / filename)
    return filename


def build_sdist(sdist_directory: str, config_settings: Mapping[str, Any] | None = None) -> str:
    del config_settings
    _maturin_pep517(
        "write-sdist",
        "--sdist-directory",
        sdist_directory,
    )
    return _newest_entry(sdist_directory, ".tar.gz", want_dir=False)


def get_requires_for_build_wheel(config_settings: Mapping[str, Any] | None = None) -> list[str]:
    del config_settings
    return []


def get_requires_for_build_editable(config_settings: Mapping[str, Any] | None = None) -> list[str]:
    del config_settings
    return []


def get_requires_for_build_sdist(config_settings: Mapping[str, Any] | None = None) -> list[str]:
    del config_settings
    return []


def prepare_metadata_for_build_wheel(
    metadata_directory: str,
    config_settings: Mapping[str, Any] | None = None,
) -> str:
    options = _additional_pep517_args()
    options.extend(_maturin_pep517_args(config_settings))
    _maturin_pep517(
        "write-dist-info",
        "--metadata-directory",
        metadata_directory,
        *options,
        *([] if _has_interpreter_arg(options) else ["--interpreter", sys.executable]),
    )
    return _newest_entry(metadata_directory, ".dist-info", want_dir=True)


prepare_metadata_for_build_editable = prepare_metadata_for_build_wheel
