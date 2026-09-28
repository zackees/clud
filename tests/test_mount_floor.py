"""Real bind-mount regression in bosn's disposable, read-only mount fixture."""

from __future__ import annotations

import os
import shutil
from pathlib import Path

import pytest

from tests import process
from tests.shim_env import session_env


def test_bind_mount_is_not_deleted() -> None:
    root = Path("/mount-probe")
    parent = root / "parent"
    mounted = parent / "mounted"
    sentinel = mounted / "CLAUDE.md"
    if not root.is_dir():
        pytest.skip("bind-mount probe runs in the dedicated Docker lane")
    try:
        is_mount = mounted.is_mount()
    except NotImplementedError:
        pytest.skip("mount-point detection is unavailable on this platform")
    if not is_mount and os.environ.get("RM_REQUIRE_MOUNT_PROBE") != "1":
        pytest.skip("bind-mount probe runs in the dedicated Docker lane")
    assert is_mount
    assert sentinel.is_file()
    shim_dir = root / "shim"
    shim_dir.mkdir(exist_ok=True)
    shim = shim_dir / ("r" + "m")
    shutil.copy2(Path("/build/target/debug/clud-shim"), shim)
    home = root / "home"
    home.mkdir(exist_ok=True)
    env = os.environ.copy()
    env.update(HOME=str(home), USERPROFILE=str(home), PATH=f"{shim_dir}:/usr/bin:/bin")
    env.update(session_env(Path("/build/target/debug/clud-shim"), shim_dir))

    stub_dir = root / "stub"
    stub_dir.mkdir(exist_ok=True)
    stub_log = root / "stub-invoked"
    stub = stub_dir / ("r" + "m")
    stub.write_text("#!/bin/sh\n/usr/bin/touch /mount-probe/stub-invoked\n", encoding="utf-8")
    stub.chmod(0o755)
    refusal_env = env.copy()
    refusal_env["PATH"] = f"{shim_dir}:{stub_dir}:/usr/bin:/bin"

    direct = process.run(
        [str(shim), "-" + "rf", str(mounted)],
        cwd=str(root), env=refusal_env, capture_output=True, text=True, timeout=30,
    )
    assert direct.returncode == 2, direct
    assert not stub_log.exists()
    assert sentinel.is_file()

    ancestor = process.run(
        [str(shim), "-" + "rf", str(parent)],
        cwd=str(root), env=env, capture_output=True, text=True, timeout=30,
    )
    assert ancestor.returncode != 0, ancestor
    assert sentinel.is_file()
