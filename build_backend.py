from __future__ import annotations

import contextlib
import os
from collections.abc import Iterator, Mapping
from pathlib import Path
from typing import Any

import maturin

from ci.env import build_env
from ci.wheel_repair import repair_windows_gnu_wheel


@contextlib.contextmanager
def _build_environment() -> Iterator[None]:
    updates = build_env()
    original = os.environ.copy()
    os.environ.clear()
    os.environ.update(updates)
    try:
        yield
    finally:
        os.environ.clear()
        os.environ.update(original)


def build_wheel(
    wheel_directory: str,
    config_settings: Mapping[str, Any] | None = None,
    metadata_directory: str | None = None,
) -> str:
    with _build_environment():
        filename = maturin.build_wheel(wheel_directory, config_settings, metadata_directory)
    repair_windows_gnu_wheel(Path(wheel_directory) / filename)
    return filename


def build_editable(
    wheel_directory: str,
    config_settings: Mapping[str, Any] | None = None,
    metadata_directory: str | None = None,
) -> str:
    with _build_environment():
        filename = maturin.build_editable(wheel_directory, config_settings, metadata_directory)
    repair_windows_gnu_wheel(Path(wheel_directory) / filename)
    return filename


def build_sdist(sdist_directory: str, config_settings: Mapping[str, Any] | None = None) -> str:
    with _build_environment():
        return maturin.build_sdist(sdist_directory, config_settings)


def get_requires_for_build_wheel(config_settings: Mapping[str, Any] | None = None) -> list[str]:
    return maturin.get_requires_for_build_wheel(config_settings)


def get_requires_for_build_editable(config_settings: Mapping[str, Any] | None = None) -> list[str]:
    return maturin.get_requires_for_build_editable(config_settings)


def get_requires_for_build_sdist(config_settings: Mapping[str, Any] | None = None) -> list[str]:
    return maturin.get_requires_for_build_sdist(config_settings)


def prepare_metadata_for_build_wheel(
    metadata_directory: str,
    config_settings: Mapping[str, Any] | None = None,
) -> str:
    with _build_environment():
        return maturin.prepare_metadata_for_build_wheel(metadata_directory, config_settings)


prepare_metadata_for_build_editable = prepare_metadata_for_build_wheel
