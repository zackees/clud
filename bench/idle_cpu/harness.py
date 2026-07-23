"""Launch idle mock-agent sessions and report their CPU and daemon event cost.

Run with ``python -m bench.idle_cpu.harness``. This module intentionally stays
outside pytest: the default sample is 60 seconds, while unit tests cover the
report math without creating processes.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import psutil

from .report import assemble_report, budget_violations

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_BASELINE_DIR = ROOT / "bench" / "idle_cpu"


@dataclass(frozen=True)
class ProcessIdentity:
    """A PID paired with the creation time needed to detect PID reuse."""

    pid: int
    create_time: float


def _binary_name(name: str) -> str:
    return f"{name}.exe" if sys.platform == "win32" else name


def _target_dirs() -> list[Path]:
    dirs: list[Path] = []
    if target := os.environ.get("CARGO_BUILD_TARGET"):
        dirs.append(ROOT / "target" / target / "debug")
    if sys.platform == "win32":
        dirs.extend(
            [
                ROOT / "target" / "x86_64-pc-windows-msvc" / "debug",
                ROOT / "target" / "aarch64-pc-windows-msvc" / "debug",
            ]
        )
    dirs.append(ROOT / "target" / "debug")
    return dirs


def _find_binary(name: str, env_name: str) -> Path | None:
    if configured := os.environ.get(env_name):
        candidate = Path(configured)
        if candidate.is_file():
            return candidate
    for directory in _target_dirs():
        candidate = directory / _binary_name(name)
        if candidate.is_file():
            return candidate
    return None


def _ensure_binary(name: str, env_name: str) -> Path:
    if binary := _find_binary(name, env_name):
        return binary
    result = subprocess.run(["soldr", "cargo", "build", "-p", name], cwd=ROOT, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"soldr cargo build -p {name} failed with {result.returncode}")
    if binary := _find_binary(name, env_name):
        return binary
    raise RuntimeError(f"{name} binary not found after build")


def _read_json(path: Path, timeout: float = 10.0) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.is_file():
            try:
                return json.loads(path.read_text(encoding="utf-8"))
            except (FileNotFoundError, json.JSONDecodeError, PermissionError):
                pass
        time.sleep(0.05)
    raise RuntimeError(f"timed out waiting for valid JSON at {path}")


def _read_session_id(proc: subprocess.Popen[str], timeout: float = 10.0) -> str:
    assert proc.stderr is not None
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        line = proc.stderr.readline()
        if "daemon session" in line:
            return line.strip().rsplit(" ", 1)[-1]
        if "running in background" in line and "session " in line:
            return line.strip().split("session ", 1)[-1].split(" running")[0]
        if proc.poll() is not None:
            raise RuntimeError(f"clud exited while starting a session: {line!r}")
    raise RuntimeError("timed out waiting for detached session id")


def _start_daemon(clud_binary: Path, env: dict[str, str], state_dir: Path) -> int:
    result = subprocess.run(
        [str(clud_binary), "daemon", "restart"],
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"daemon restart failed: {result.stderr.strip()}")
    return int(_read_json(state_dir / "daemon.json")["pid"])


def _launch_session(
    clud_binary: Path, env: dict[str, str], index: int, sleep_ms: int
) -> tuple[subprocess.Popen[str], str]:
    proc = subprocess.Popen(
        [
            str(clud_binary),
            "--detach",
            "--codex",
            "-p",
            f"idle CPU benchmark session {index}",
            "--",
            "--mock-sleep-ms",
            str(sleep_ms),
        ],
        cwd=ROOT,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return proc, _read_session_id(proc)


def _count_event_lines(state_dir: Path) -> int:
    return sum(
        len(path.read_text(encoding="utf-8").splitlines())
        for path in (state_dir / "daemon-events.jsonl", state_dir / "daemon-events.jsonl.1")
        if path.is_file()
    )


def _sample(pids: list[int]) -> dict[int, dict[str, float | int | None]]:
    sample: dict[int, dict[str, float | int | None]] = {}
    for pid in pids:
        try:
            proc = psutil.Process(pid)
            cpu = proc.cpu_times()
            try:
                switches = proc.num_ctx_switches()
                context_switches: int | None = switches.voluntary + switches.involuntary
            except (psutil.AccessDenied, AttributeError):
                context_switches = None
            sample[pid] = {
                "cpu_seconds": cpu.user + cpu.system,
                "ctx_switches": context_switches,
                "create_time": proc.create_time(),
            }
        except (psutil.AccessDenied, psutil.NoSuchProcess):
            continue
    return sample


def _process_identity(pid: int) -> ProcessIdentity | None:
    try:
        process = psutil.Process(pid)
        return ProcessIdentity(pid=pid, create_time=process.create_time())
    except (psutil.AccessDenied, psutil.NoSuchProcess):
        return None


def _identity_matches(identity: ProcessIdentity) -> bool:
    return _process_identity(identity.pid) == identity


def _kill_tree(identity: ProcessIdentity) -> None:
    if not _identity_matches(identity):
        return
    if sys.platform == "win32":
        subprocess.run(
            ["taskkill", "/PID", str(identity.pid), "/T", "/F"],
            capture_output=True,
            check=False,
        )
        return
    try:
        process = psutil.Process(identity.pid)
    except psutil.NoSuchProcess:
        return
    try:
        children = process.children(recursive=True)
    except psutil.NoSuchProcess:
        return
    for child in children:
        try:
            child.kill()
        except psutil.NoSuchProcess:
            pass
    try:
        process.kill()
    except psutil.NoSuchProcess:
        pass


def _wait_gone(identities: list[ProcessIdentity], timeout: float = 15.0) -> list[int]:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        alive = [identity.pid for identity in identities if _identity_matches(identity)]
        if not alive:
            return []
        time.sleep(0.1)
    return [identity.pid for identity in identities if _identity_matches(identity)]


def _head() -> str:
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True, check=True
    )
    return result.stdout.strip()


def _discard_reused_pids(
    before: dict[int, dict[str, float | int | None]],
    after: dict[int, dict[str, float | int | None]],
) -> dict[int, dict[str, float | int | None]]:
    """Drop a t1 PID when it belongs to a process created after t0."""
    return {
        pid: sample
        for pid, sample in after.items()
        if pid not in before or sample["create_time"] == before[pid]["create_time"]
    }


def run_harness(sessions: int, window_secs: float) -> dict[str, Any]:
    """Perform one fully cleaned-up benchmark sample and return its report."""
    if sessions < 1:
        raise ValueError("--sessions must be at least 1")
    if window_secs <= 0:
        raise ValueError("--window-secs must be positive")

    clud_binary = _ensure_binary("clud", "CLUD_TEST_BINARY")
    mock_agent = _ensure_binary("mock-agent", "CLUD_TEST_MOCK_AGENT_BINARY")
    tracked_processes: list[ProcessIdentity] = []
    launchers: list[subprocess.Popen[str]] = []

    with tempfile.TemporaryDirectory(prefix="clud-idle-cpu-") as temp:
        temp_dir = Path(temp)
        state_dir = temp_dir / "state"
        mock_dir = temp_dir / "mock-agent"
        mock_dir.mkdir()
        for backend in ("claude", "codex"):
            target = mock_dir / _binary_name(backend)
            shutil.copy2(mock_agent, target)
            if sys.platform != "win32":
                target.chmod(0o755)
        env = os.environ.copy()
        env["PATH"] = str(mock_dir) + os.pathsep + env.get("PATH", "")
        env["CLUD_DAEMON_STATE_DIR"] = str(state_dir)
        env["CLUD_NO_UNLOCK"] = "1"
        env.pop("VIRTUAL_ENV", None)

        try:
            daemon_pid = _start_daemon(clud_binary, env, state_dir)
            daemon_identity = _process_identity(daemon_pid)
            if daemon_identity is None:
                raise RuntimeError(f"daemon PID {daemon_pid} exited before sampling began")
            tracked_processes.append(daemon_identity)
            roles: dict[int, str] = {daemon_pid: "daemon"}
            sleep_ms = int((window_secs + 30) * 1000)
            for index in range(sessions):
                launcher, session_id = _launch_session(clud_binary, env, index + 1, sleep_ms)
                launchers.append(launcher)
                metadata = _read_json(state_dir / "sessions" / f"{session_id}.json")
                for key, role in (("root_pid", "client-root"), ("worker_pid", "client-worker")):
                    pid = metadata.get(key)
                    if isinstance(pid, int) and pid not in roles:
                        roles[pid] = role
                        identity = _process_identity(pid)
                        if identity is not None:
                            tracked_processes.append(identity)

            before = _sample(list(roles))
            event_lines_before = _count_event_lines(state_dir)
            time.sleep(window_secs)
            after = _discard_reused_pids(before, _sample(list(roles)))
            return assemble_report(
                head=_head(),
                timestamp=datetime.now(UTC).isoformat(),
                sessions=sessions,
                window_secs=window_secs,
                roles=roles,
                before=before,
                after=after,
                event_lines_before=event_lines_before,
                event_lines_after=_count_event_lines(state_dir),
            )
        finally:
            for launcher in launchers:
                if launcher.poll() is None:
                    launcher.kill()
            for identity in reversed(tracked_processes):
                _kill_tree(identity)
            if survivors := _wait_gone(tracked_processes):
                raise RuntimeError(f"benchmark leaked processes: {survivors}")


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sessions", type=int, default=1)
    parser.add_argument("--window-secs", type=float, default=60.0)
    parser.add_argument("--json", type=Path, help="write JSON here instead of stdout")
    parser.add_argument(
        "--budget", action="store_true", help="compare against a baseline and fail on excess"
    )
    parser.add_argument("--baseline", type=Path, help="baseline JSON (defaults by session count)")
    return parser.parse_args()


def main() -> int:
    args = _parse_args()
    report = run_harness(args.sessions, args.window_secs)
    payload = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(payload, encoding="utf-8")
        print(args.json)
    else:
        print(payload, end="")

    budget_enabled = args.budget or os.environ.get("CLUD_BENCH_BUDGET") == "1"
    if not budget_enabled:
        return 0
    baseline_path = args.baseline or DEFAULT_BASELINE_DIR / f"baseline_n{args.sessions}.json"
    baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
    if violations := budget_violations(report, baseline):
        print("idle CPU budget failed:", *violations, sep="\n  ", file=sys.stderr)
        return 1
    print(f"idle CPU budget passed against {baseline_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
