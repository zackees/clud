"""Integration tests for the session-registry concurrency fix (issue #138).

The original bug: `redb::Database::create()` takes an exclusive per-process
file lock; two `clud` processes launching simultaneously would race, the
loser would print "Database already open. Cannot acquire lock.", and the
issue-#73 fork-bomb cap would silently no-op for every concurrent launch
past the first.

These tests pin two contracts of the lockfile-based fix:

1. **No warning under concurrency.** N concurrent launches must all
   succeed without printing the `Database already open` warning. The
   `sessions.lock` advisory lock serializes redb opens so the redb file
   lock is only ever held by one process at a time.

2. **Cap actually refuses.** With `CLUD_MAX_INSTANCES=1` and one live
   `clud`, a second concurrent launch is refused with the fork-bomb
   guardrail message — *not* silently let through with a warning.
"""

from __future__ import annotations

import shutil
import subprocess
import tempfile
import threading
import time
from pathlib import Path

import pytest

pytestmark = pytest.mark.integration


_WARNING_FRAGMENT = "Database already open"
_REFUSE_FRAGMENT = "fork-bomb guardrail"


def _launch_clud(
    clud: Path,
    env: dict[str, str],
    extra_args: list[str] | None = None,
    sleep_ms: int = 300,
) -> subprocess.CompletedProcess[str]:
    """Run clud once with the mock backend; default workload is a short sleep
    so the session registry row is held in the DB while the test inspects
    a sibling launch.
    """
    with tempfile.TemporaryDirectory() as temp_dir:
        # Copy the binary into a private dir so per-test launches don't
        # collide on Windows file locks (mirrors the helper in
        # `test_mock_agents.py::_run`).
        launch = Path(temp_dir) / clud.name
        shutil.copy2(clud, launch)
        args = [str(launch), "-p", "hello"]
        if extra_args:
            args.extend(extra_args)
        args.extend(["--", "--mock-sleep-ms", str(sleep_ms)])
        return subprocess.run(
            args,
            capture_output=True,
            text=True,
            timeout=30,
            env=env,
        )


def _registry_env(
    base: dict[str, str], tmp_path: Path, max_instances: int = 8
) -> dict[str, str]:
    """Build an env that isolates the test's session registry from the host's.

    Pointing both `CLUD_SESSION_DB` and `CLUD_SESSION_LOCK` at the test's
    temp dir means the test doesn't pollute (or get polluted by) the
    user's real `%LOCALAPPDATA%\\clud\\sessions.redb`. The lockfile lives
    next to the redb file anyway, but pinning both is explicit.

    Also disables the gc daemon (`CLUD_NO_DAEMON=1`) so the test doesn't
    leave a long-lived `clud __gc-daemon` child wedged behind it.
    """
    env = dict(base)
    env["CLUD_SESSION_DB"] = str(tmp_path / "sessions.redb")
    env["CLUD_SESSION_LOCK"] = str(tmp_path / "sessions.lock")
    env["CLUD_MAX_INSTANCES"] = str(max_instances)
    env["CLUD_NO_DAEMON"] = "1"
    return env


class TestConcurrentLaunchesNoWarning:
    """Acceptance: N concurrent `clud` launches do not print the redb
    `Database already open` warning that issue #138 reported."""

    def test_three_concurrent_launches_all_succeed_without_redb_warning(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        env = _registry_env(mock_env, tmp_path, max_instances=8)

        results: list[subprocess.CompletedProcess[str]] = []
        errors: list[BaseException] = []
        lock = threading.Lock()

        def run_one() -> None:
            try:
                r = _launch_clud(clud_binary, env, sleep_ms=400)
                with lock:
                    results.append(r)
            except BaseException as e:  # surface any failure to the test thread
                with lock:
                    errors.append(e)

        threads = [threading.Thread(target=run_one) for _ in range(3)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=45)

        assert not errors, f"thread errors: {errors}"
        assert len(results) == 3, f"expected 3 results, got {len(results)}"
        for i, r in enumerate(results):
            assert r.returncode == 0, (
                f"launch {i} failed: rc={r.returncode}\nstderr={r.stderr}"
            )
            assert _WARNING_FRAGMENT not in r.stderr, (
                f"launch {i} printed the redb warning issue #138 was supposed to fix:\n"
                f"stderr={r.stderr}"
            )

    def test_cap_warn_threshold_does_not_leak_redb_warning(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        """A second concurrent launch at the warn threshold prints the
        warn-band message — but **never** the redb warning. Distinguishes
        the intended "you have many sessions" guidance from the redb
        contention bug that #138 fixes."""
        env = _registry_env(mock_env, tmp_path, max_instances=4)
        env["CLUD_WARN_INSTANCES"] = "1"  # warn the moment a sibling exists

        # First launch sleeps long enough that the second sees it in the
        # cap count.
        first_done = threading.Event()

        def run_first() -> subprocess.CompletedProcess[str]:
            r = _launch_clud(clud_binary, env, sleep_ms=2000)
            first_done.set()
            return r

        first_result_box: list[subprocess.CompletedProcess[str]] = []

        def first_target() -> None:
            first_result_box.append(run_first())

        t = threading.Thread(target=first_target)
        t.start()
        # Give the first launch time to register itself.
        time.sleep(0.6)
        second = _launch_clud(clud_binary, env, sleep_ms=100)
        t.join(timeout=30)

        assert first_result_box, "first launch never produced a result"
        assert _WARNING_FRAGMENT not in second.stderr
        assert _WARNING_FRAGMENT not in first_result_box[0].stderr


class TestCapActuallyRefuses:
    """Acceptance: with `CLUD_MAX_INSTANCES=1`, a second concurrent
    launch is refused — not silently allowed by the redb-contention
    bypass."""

    def test_cap_one_refuses_second_concurrent_launch(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        env = _registry_env(mock_env, tmp_path, max_instances=1)

        first_started = threading.Event()
        first_result_box: list[subprocess.CompletedProcess[str]] = []

        def first_target() -> None:
            first_started.set()
            r = _launch_clud(clud_binary, env, sleep_ms=2500)
            first_result_box.append(r)

        t = threading.Thread(target=first_target)
        t.start()
        first_started.wait(timeout=5)
        # Give the first launch a moment to register before we probe.
        time.sleep(0.8)

        second = _launch_clud(clud_binary, env, sleep_ms=100)
        t.join(timeout=30)

        # The second launch must be refused (issue #73 fork-bomb guardrail)
        # rather than slipping through with the redb warning (issue #138).
        assert _WARNING_FRAGMENT not in second.stderr, (
            f"second launch silently bypassed cap due to redb warning:\n"
            f"stderr={second.stderr}"
        )
        assert second.returncode != 0, (
            f"second launch should have been refused but exited 0\n"
            f"stderr={second.stderr}"
        )
        assert _REFUSE_FRAGMENT in second.stderr, (
            f"second launch exited non-zero but without the fork-bomb message\n"
            f"stderr={second.stderr}"
        )

        # First launch should still finish cleanly.
        assert first_result_box, "first launch never produced a result"
        assert first_result_box[0].returncode == 0
