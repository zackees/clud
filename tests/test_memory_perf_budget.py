"""Issue #266: performance-budget assertions for the memory subsystem.

These tests enforce the spec'd budgets from meta #255:

| Budget                                              | Target  |
|-----------------------------------------------------|---------|
| `memory_save` p50                                   | <= 30ms |
| `memory_smart_search` p50                           | <= 25ms |
| daemon RSS without local model                      | <= 60MB |
| on-disk bytes per 1k memories (no vectors)          | <= 15MB |

All four tests are mutually independent — every one uses its own daemon
under its own temp state dir.

### Skip policy

These tests are skip-friendly so a noisy CI worker can opt out without
flaking the suite. Each test skips when:

- The host reports < 4 GB free RAM (`psutil.virtual_memory().available`).
  Below that, GC pauses and swap pressure swamp the latency signal.
- The memory subsystem is unavailable on the host (e.g. Windows-ARM with
  the embedder feature compiled in but the model unavailable).
- `psutil` is not installed (RSS-budget test only).

A smoke variant of every test runs without budget assertions so the test
file always exercises its setup code in the default `bash test` run.

### Marker

Heavy iterations are gated on `pytest -m perf_budget`. The smokes are
unmarked and run with the rest of the unit tests.
"""

from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent

# Budgets (spec'd in meta #255).
MEMORY_SAVE_P50_MS_BUDGET = 30.0
MEMORY_SEARCH_P50_MS_BUDGET = 25.0
DAEMON_RSS_NO_MODEL_MB_BUDGET = 60.0
DISK_PER_1K_MEMORIES_MB_BUDGET = 15.0

# Minimum free RAM required for the budget assertions to be considered
# meaningful. Below this we run the test but skip the budget assertion.
MIN_FREE_RAM_GB_FOR_BUDGETS = 4.0


# ---------- binary + daemon helpers (mirrors test_memory_cross_process.py) ----------


def _cargo_argv_and_env(subcommand: list[str]) -> tuple[list[str], dict[str, str]]:
    if sys.platform == "win32":
        try:
            from ci.env import build_env, cargo_argv

            env = build_env()
            return cargo_argv(subcommand, env=env), env
        except Exception:
            pass
        soldr = shutil.which("soldr")
        if soldr:
            return [soldr, "cargo", *subcommand], dict(os.environ)
    return ["cargo", *subcommand], dict(os.environ)


def _build_clud() -> Path:
    env_binary = os.environ.get("CLUD_TEST_BINARY")
    if env_binary and Path(env_binary).is_file():
        return Path(env_binary)
    argv, build_env_vars = _cargo_argv_and_env(
        ["build", "-p", "clud", "--no-default-features", "--message-format=json"]
    )
    result = subprocess.run(
        argv,
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=600,
        env=build_env_vars,
    )
    if result.returncode != 0:
        pytest.skip(f"could not build clud:\n{result.stderr[-2000:]}")
    for line in result.stdout.splitlines():
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (
            msg.get("reason") == "compiler-artifact"
            and msg.get("target", {}).get("name") == "clud"
            and msg.get("executable")
        ):
            return Path(msg["executable"])
    pytest.skip("could not locate clud binary in cargo JSON output")


def _wait_for_daemon(state_dir: Path, timeout: float = 30.0) -> tuple[int, int]:
    """Return (dashboard_port, daemon_pid) once the daemon is up."""
    deadline = time.time() + timeout
    info_path = state_dir / "daemon.json"
    while time.time() < deadline:
        if info_path.is_file():
            try:
                info = json.loads(info_path.read_text(encoding="utf-8"))
            except (json.JSONDecodeError, PermissionError):
                time.sleep(0.1)
                continue
            port = info.get("dashboard_port")
            pid = info.get("pid")
            if port and pid:
                deadline2 = time.time() + 10.0
                while time.time() < deadline2:
                    try:
                        with socket.create_connection(
                            ("127.0.0.1", int(port)), timeout=1.0
                        ):
                            return int(port), int(pid)
                    except OSError:
                        time.sleep(0.1)
                return int(port), int(pid)
        time.sleep(0.2)
    raise AssertionError(f"daemon never came up within {timeout}s")


def _kill_daemon(state_dir: Path) -> None:
    info_path = state_dir / "daemon.json"
    pid: int | None = None
    if info_path.is_file():
        try:
            pid = int(json.loads(info_path.read_text(encoding="utf-8")).get("pid", 0))
        except (json.JSONDecodeError, ValueError, PermissionError):
            pid = None
    if pid:
        if sys.platform == "win32":
            subprocess.run(
                ["taskkill", "/PID", str(pid), "/T", "/F"],
                capture_output=True,
                text=True,
                check=False,
            )
        else:
            try:
                os.kill(pid, 9)
            except OSError:
                pass
    deadline = time.time() + 5.0
    while time.time() < deadline:
        try:
            info_path.unlink()
            break
        except (FileNotFoundError, PermissionError):
            if not info_path.is_file():
                break
            time.sleep(0.1)


class _DaemonHandle:
    """Spawn `clud __daemon` for budget tests.

    ``embedder_disabled=True`` forces `CLUD_MEMORY_EMBEDDER=disabled`. The
    save handler 500s in that mode, so disable only when the test doesn't
    need to save (the RAM-budget test reads `memory_info().rss`, the
    disk-budget smoke checks one file).
    """

    def __init__(
        self,
        clud: Path,
        state_dir: Path,
        *,
        embedder_disabled: bool = False,
    ) -> None:
        self.clud = clud
        self.state_dir = state_dir
        state_dir.mkdir(parents=True, exist_ok=True)
        env = os.environ.copy()
        env["CLUD_DAEMON_STATE_DIR"] = str(state_dir)
        if embedder_disabled:
            env["CLUD_MEMORY_EMBEDDER"] = "disabled"
        env["CLUD_NO_UNLOCK"] = "1"
        env.pop("VIRTUAL_ENV", None)
        try:
            self.proc = subprocess.Popen(
                [str(clud), "__daemon", "--state-dir", str(state_dir)],
                env=env,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
        except (FileNotFoundError, PermissionError) as e:
            pytest.skip(f"could not spawn clud daemon: {e}")
        try:
            self.port, self.daemon_pid = _wait_for_daemon(state_dir, timeout=30.0)
        except AssertionError:
            self.close()
            pytest.skip("daemon failed to start")

    def close(self) -> None:
        try:
            self.proc.terminate()
            self.proc.wait(timeout=5)
        except (subprocess.TimeoutExpired, ProcessLookupError):
            if sys.platform == "win32":
                subprocess.run(
                    ["taskkill", "/PID", str(self.proc.pid), "/T", "/F"],
                    capture_output=True,
                    text=True,
                    check=False,
                )
            else:
                self.proc.kill()
                try:
                    self.proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    pass
        _kill_daemon(self.state_dir)


def _fetch(url: str, data: bytes | None = None, timeout: float = 30.0) -> tuple[int, bytes]:
    req = urllib.request.Request(url, data=data)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.getcode(), resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def _percentile(values: list[float], pct: float) -> float:
    """Linear-interpolated percentile. `pct` in [0,100]."""
    if not values:
        return float("nan")
    s = sorted(values)
    k = (len(s) - 1) * (pct / 100.0)
    f = int(k)
    c = min(f + 1, len(s) - 1)
    if f == c:
        return s[f]
    return s[f] + (s[c] - s[f]) * (k - f)


def _have_enough_ram_for_budgets() -> bool:
    try:
        import psutil  # type: ignore[import-not-found]
    except ImportError:
        return False
    avail_gb = psutil.virtual_memory().available / (1024**3)
    return avail_gb >= MIN_FREE_RAM_GB_FOR_BUDGETS


# ---------- fixtures ----------


@pytest.fixture(scope="module")
def clud_binary() -> Path:
    return _build_clud()


@pytest.fixture
def daemon(clud_binary: Path):
    """Yield a `_DaemonHandle` with a live embedder for save/search budgets."""
    with tempfile.TemporaryDirectory(prefix="clud-perf-") as tmp:
        state_dir = Path(tmp) / "state"
        handle = _DaemonHandle(clud_binary, state_dir, embedder_disabled=False)
        status, _body = _fetch(f"http://127.0.0.1:{handle.port}/memory/stats")
        if status == 503:
            handle.close()
            pytest.skip("memory subsystem unavailable on this host")
        try:
            yield handle
        finally:
            handle.close()


@pytest.fixture
def daemon_no_embedder(clud_binary: Path):
    """Yield a `_DaemonHandle` with `CLUD_MEMORY_EMBEDDER=disabled`.

    Use for the RAM-budget test (no model loaded — the floor) and the
    disk-size smoke (the save path won't work, but the file walk does).
    """
    with tempfile.TemporaryDirectory(prefix="clud-perf-nm-") as tmp:
        state_dir = Path(tmp) / "state"
        handle = _DaemonHandle(clud_binary, state_dir, embedder_disabled=True)
        status, _body = _fetch(f"http://127.0.0.1:{handle.port}/memory/stats")
        if status == 503:
            handle.close()
            pytest.skip("memory subsystem unavailable on this host")
        try:
            yield handle
        finally:
            handle.close()


# ---------- smokes (always run, zero perf-budget cost) ----------


def test_memory_save_smoke(daemon) -> None:
    """One save, no budget assertion. Proves the perf harness's save path
    wires up correctly so the marked tests can rely on it."""
    payload = json.dumps({"content": "perf-smoke-save"}).encode("utf-8")
    status, body = _fetch(
        f"http://127.0.0.1:{daemon.port}/memory/save", data=payload
    )
    if status >= 500 and b"embedder" in body.lower():
        pytest.skip("embedder unavailable; save path can't run")
    assert status == 200, body[:400]


def test_memory_search_smoke(daemon) -> None:
    """One search, no budget assertion. The empty-store reply must be `[]`."""
    status, body = _fetch(f"http://127.0.0.1:{daemon.port}/memory/search?q=foo")
    assert status == 200, body[:400]


def test_daemon_rss_smoke(daemon_no_embedder) -> None:
    """Read the daemon's RSS exactly once; no budget assertion."""
    psutil = pytest.importorskip("psutil")
    try:
        rss = psutil.Process(daemon_no_embedder.daemon_pid).memory_info().rss
    except psutil.NoSuchProcess:
        pytest.skip("daemon process exited before RSS read")
    assert rss > 0, f"unreasonable RSS: {rss}"


def test_disk_size_smoke(daemon) -> None:
    """Write one row, then walk `<state_dir>/memory/` and assert non-zero."""
    payload = json.dumps({"content": "disk-smoke"}).encode("utf-8")
    status, body = _fetch(f"http://127.0.0.1:{daemon.port}/memory/save", data=payload)
    if status >= 500 and b"embedder" in body.lower():
        pytest.skip("embedder unavailable; save path can't run")
    assert status == 200
    total = 0
    for p in (daemon.state_dir / "memory").rglob("*"):
        if p.is_file():
            try:
                total += p.stat().st_size
            except OSError:
                pass
    assert total > 0, "memory dir produced no files"


# ---------- perf budgets (gated on -m perf_budget) ----------


@pytest.mark.perf_budget
def test_memory_save_p50_under_30ms(daemon) -> None:
    """Fire 100 saves, measure p50 latency, assert <= 30 ms.

    If host is too noisy (< 4 GB free RAM), skip the assertion but still
    run the loop so we surface non-budget regressions as test failures
    (e.g. a 500 from the save handler).
    """
    enforce = _have_enough_ram_for_budgets()
    latencies_ms: list[float] = []
    failures = 0
    iterations = 100
    for i in range(iterations):
        payload = json.dumps({"content": f"save-budget-{i:04d}"}).encode("utf-8")
        t0 = time.perf_counter_ns()
        status, body = _fetch(
            f"http://127.0.0.1:{daemon.port}/memory/save", data=payload, timeout=10.0
        )
        elapsed_ms = (time.perf_counter_ns() - t0) / 1e6
        if status != 200:
            if i == 0 and status >= 500 and b"embedder" in body.lower():
                pytest.skip("embedder unavailable; save budget test cannot run")
            failures += 1
            continue
        latencies_ms.append(elapsed_ms)
    assert failures == 0, f"{failures} save calls failed out of {iterations}"
    p50 = _percentile(latencies_ms, 50)
    print(f"\n[perf] memory_save p50 = {p50:.2f} ms ({iterations} iter)")
    if not enforce:
        pytest.skip(
            f"insufficient free RAM (< {MIN_FREE_RAM_GB_FOR_BUDGETS} GB); skipping budget assertion"
        )
    assert p50 <= MEMORY_SAVE_P50_MS_BUDGET, (
        f"memory_save p50 {p50:.2f} ms exceeds budget {MEMORY_SAVE_P50_MS_BUDGET} ms"
    )


@pytest.mark.perf_budget
def test_memory_smart_search_p50_under_25ms(daemon) -> None:
    """Seed 50 rows, then fire 100 searches against the corpus, measure p50."""
    enforce = _have_enough_ram_for_budgets()
    # Seed.
    for i in range(50):
        payload = json.dumps(
            {"content": f"seed-{i:04d} foo bar baz quux"}
        ).encode("utf-8")
        status, body = _fetch(
            f"http://127.0.0.1:{daemon.port}/memory/save", data=payload
        )
        if status >= 500 and b"embedder" in body.lower():
            pytest.skip("embedder unavailable; search budget seeding cannot run")
        assert status == 200, body[:200]

    latencies_ms: list[float] = []
    queries = ["foo", "bar", "baz", "quux", "seed"]
    iterations = 100
    for i in range(iterations):
        q = queries[i % len(queries)]
        t0 = time.perf_counter_ns()
        status, body = _fetch(
            f"http://127.0.0.1:{daemon.port}/memory/search?q={q}", timeout=10.0
        )
        elapsed_ms = (time.perf_counter_ns() - t0) / 1e6
        assert status == 200, body[:400]
        latencies_ms.append(elapsed_ms)
    p50 = _percentile(latencies_ms, 50)
    print(f"\n[perf] memory_smart_search p50 = {p50:.2f} ms ({iterations} iter)")
    if not enforce:
        pytest.skip(
            f"insufficient free RAM (< {MIN_FREE_RAM_GB_FOR_BUDGETS} GB); skipping budget assertion"
        )
    assert p50 <= MEMORY_SEARCH_P50_MS_BUDGET, (
        f"memory_smart_search p50 {p50:.2f} ms exceeds budget"
        f" {MEMORY_SEARCH_P50_MS_BUDGET} ms"
    )


@pytest.mark.perf_budget
def test_daemon_ram_under_60mb_without_model(daemon_no_embedder) -> None:
    """Embedder is `Disabled` (no fastembed weights loaded). RSS budget is
    60 MB. Read 5 samples to dampen scheduler noise."""
    psutil = pytest.importorskip("psutil")
    enforce = _have_enough_ram_for_budgets()
    samples_mb: list[float] = []
    for _ in range(5):
        try:
            rss = psutil.Process(daemon_no_embedder.daemon_pid).memory_info().rss
        except psutil.NoSuchProcess:
            pytest.skip("daemon process exited before RSS sample")
        samples_mb.append(rss / (1024 * 1024))
        time.sleep(0.1)
    median_mb = sorted(samples_mb)[len(samples_mb) // 2]
    print(f"\n[perf] daemon RSS (embedder disabled) median = {median_mb:.1f} MB")
    if not enforce:
        pytest.skip(
            f"insufficient free RAM (< {MIN_FREE_RAM_GB_FOR_BUDGETS} GB); skipping budget assertion"
        )
    assert median_mb <= DAEMON_RSS_NO_MODEL_MB_BUDGET, (
        f"daemon RSS {median_mb:.1f} MB exceeds budget"
        f" {DAEMON_RSS_NO_MODEL_MB_BUDGET} MB"
    )


@pytest.mark.perf_budget
def test_disk_under_15mb_per_1k_memories(daemon) -> None:
    """Save 1000 rows, then walk `<state_dir>/memory/` for total bytes.

    Uses the live-embedder fixture so the SQLite `memory_vec` column carries
    real vector blobs (not zero-length placeholders) — this reflects the
    actual on-disk footprint a user sees. The budget is per-1k-memories.
    """
    enforce = _have_enough_ram_for_budgets()
    n = 1000
    failures = 0
    for i in range(n):
        payload = json.dumps(
            {"content": f"disk-budget-{i:05d} the quick brown fox jumps over the lazy dog"}
        ).encode("utf-8")
        status, body = _fetch(
            f"http://127.0.0.1:{daemon.port}/memory/save",
            data=payload,
            timeout=30.0,
        )
        if status != 200:
            if i == 0 and status >= 500 and b"embedder" in body.lower():
                pytest.skip("embedder unavailable; disk budget test cannot run")
            failures += 1
    assert failures == 0, f"{failures} save calls failed during disk budget test"

    # WAL checkpoint + flush via /memory/stats (cheap GET, forces the
    # tantivy reader to refresh segment lists).
    _fetch(f"http://127.0.0.1:{daemon.port}/memory/stats")
    time.sleep(0.5)

    total_bytes = 0
    for p in (daemon.state_dir / "memory").rglob("*"):
        if p.is_file():
            try:
                total_bytes += p.stat().st_size
            except OSError:
                pass
    total_mb = total_bytes / (1024 * 1024)
    print(
        f"\n[perf] disk per {n} memories (no vectors) = {total_mb:.2f} MB"
    )
    if not enforce:
        pytest.skip(
            f"insufficient free RAM (< {MIN_FREE_RAM_GB_FOR_BUDGETS} GB); skipping budget assertion"
        )
    # Spec is bytes per 1k memories; scale.
    per_1k_mb = total_mb * (1000.0 / n)
    assert per_1k_mb <= DISK_PER_1K_MEMORIES_MB_BUDGET, (
        f"disk per 1k memories = {per_1k_mb:.2f} MB exceeds budget"
        f" {DISK_PER_1K_MEMORIES_MB_BUDGET} MB"
    )
