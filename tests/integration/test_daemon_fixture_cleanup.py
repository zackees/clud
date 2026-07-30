"""Regression coverage for mandatory pytest daemon cleanup (#641)."""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import psutil
import pytest

from ._daemon_helpers import (
    managed_env,
    process_identity_is_alive,
    stop_daemons_below,
)

pytestmark = pytest.mark.integration
pytest_plugins = ("pytester",)


def _start_daemon(
    clud_binary: Path, env: dict[str, str], state_dir: Path
) -> dict[str, object]:
    result = subprocess.run(
        [str(clud_binary), "daemon", "restart"],
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )
    assert result.returncode == 0, result.stderr
    return json.loads((state_dir / "daemon.json").read_text(encoding="utf-8"))


def test_stop_daemons_below_stops_recorded_identity(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = managed_env(mock_env, state_dir)
    info = _start_daemon(clud_binary, env, state_dir)

    stopped = stop_daemons_below(tmp_path, clud_binary)

    assert stopped == [state_dir]
    assert not process_identity_is_alive(
        int(info["pid"]),
        int(info["pid_start"]),
    )
    assert not (state_dir / "daemon.json").exists()


def test_cleanup_rejects_pid_only_legacy_state(tmp_path: Path) -> None:
    state_dir = tmp_path / "legacy-state"
    state_dir.mkdir()
    info_path = state_dir / "daemon.json"
    info_path.write_text(
        json.dumps({"pid": os.getpid(), "pid_start": 0}),
        encoding="utf-8",
    )

    with pytest.raises(AssertionError, match="pid_start must be positive"):
        stop_daemons_below(tmp_path, tmp_path / "must-not-be-invoked")

    assert info_path.exists()
    assert process_identity_is_alive(
        os.getpid(),
        int(psutil.Process().create_time()),
    )
    info_path.unlink()


def test_cleanup_continues_after_one_state_dir_fails(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    bad_state_dir = tmp_path / "a-bad-state"
    bad_state_dir.mkdir()
    bad_info_path = bad_state_dir / "daemon.json"
    bad_info_path.write_text(
        json.dumps({"pid": os.getpid(), "pid_start": 0}),
        encoding="utf-8",
    )

    good_state_dir = tmp_path / "z-good-state"
    env = managed_env(mock_env, good_state_dir)
    good_info = _start_daemon(clud_binary, env, good_state_dir)
    try:
        with pytest.raises(AssertionError, match="a-bad-state"):
            stop_daemons_below(tmp_path, clud_binary)

        assert not process_identity_is_alive(
            int(good_info["pid"]),
            int(good_info["pid_start"]),
        )
        assert not (good_state_dir / "daemon.json").exists()
    finally:
        bad_info_path.unlink(missing_ok=True)


def test_autouse_fixture_cleans_daemon_after_real_test_failure(
    pytester: pytest.Pytester,
) -> None:
    """Run a nested failing test through the real integration conftest."""
    marker = pytester.path / "daemon-identity.json"
    pytester.makeconftest('pytest_plugins = ("integration.conftest",)')
    pytester.makepyfile(
        f"""
        import json
        import subprocess
        from pathlib import Path

        def test_body_fails_after_starting_daemon(
            clud_binary, mock_env, tmp_path
        ):
            state_dir = tmp_path / "daemon-state"
            env = dict(mock_env)
            env["CLUD_DAEMON_STATE_DIR"] = str(state_dir)
            result = subprocess.run(
                [str(clud_binary), "daemon", "restart"],
                capture_output=True,
                text=True,
                timeout=30,
                env=env,
            )
            assert result.returncode == 0, result.stderr
            info = json.loads(
                (state_dir / "daemon.json").read_text(encoding="utf-8")
            )
            info["state_dir"] = str(state_dir)
            Path({json.dumps(str(marker))}).write_text(
                json.dumps(info),
                encoding="utf-8",
            )
            raise RuntimeError("intentional nested test failure")
        """
    )

    result = pytester.runpytest_inprocess("-q")

    result.assert_outcomes(failed=1)
    info = json.loads(marker.read_text(encoding="utf-8"))
    assert not process_identity_is_alive(
        int(info["pid"]),
        int(info["pid_start"]),
    )
    assert not (Path(info["state_dir"]) / "daemon.json").exists()
