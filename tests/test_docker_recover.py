"""Focused unit tests for the bundled Docker recovery tool (issue #531).

Covers the mandatory Windows storage-resolver fixtures from the follow-up
comment (CustomWslDistroDir-only, CustomWslDistroDir+DataFolder both set,
ambiguous/missing config), the read-only doctor health assessment, the
bounded readiness wait, and the confirmation gate that refuses to mutate a
VHD without an unambiguous candidate + explicit confirmation.

The Windows resolver uses `ntpath` internally so these tests are
deterministic on Linux / macOS CI without touching a real Windows registry
or filesystem: paths are canned in a FakeProbe.
"""

from __future__ import annotations

import argparse
import importlib.util
import io
import ntpath
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "docker" / "docker_recover.py"

# 29.5 GiB — the real docker_data.vhdx size from the #531 incident.
INCIDENT_DISK_SIZE = int(29.5 * 1024**3)


@pytest.fixture
def dr():
    name = "clud_test_docker_recover"
    spec = importlib.util.spec_from_file_location(name, SCRIPT)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    try:
        yield module
    finally:
        sys.modules.pop(name, None)


class FakeProbe:
    """In-memory stand-in for SystemDiskProbe, keyed on Windows paths."""

    def __init__(self, files, *, settings_text=None, resolve=None, recent=None):
        # files: {windows_path: size_bytes} of EXISTING files only.
        self._files = {ntpath.normcase(k): (k, v) for k, v in files.items()}
        self._settings_text = settings_text or {}
        self._resolve = resolve or {}
        self._recent = {ntpath.normcase(x) for x in (recent or ())}

    def read_text(self, path):
        return self._settings_text.get(path)

    def exists(self, path):
        return ntpath.normcase(path) in self._files

    def size_bytes(self, path):
        entry = self._files.get(ntpath.normcase(path))
        return entry[1] if entry else None

    def resolve_final(self, path):
        return self._resolve.get(path, path)

    def recent_write(self, path, within_hours=24.0):
        return ntpath.normcase(path) in self._recent

    def glob_vhdx(self, root):
        roots = {
            ntpath.normcase(root),
            ntpath.normcase(ntpath.join(root, "disk")),
            ntpath.normcase(ntpath.join(root, "data")),
        }
        out = []
        for _norm, (orig, _size) in self._files.items():
            if not orig.lower().endswith(".vhdx"):
                continue
            if ntpath.normcase(ntpath.dirname(orig)) in roots:
                out.append(orig)
        return out


# --------------------------------------------------------------------------
# Windows resolver — the three mandatory fixtures.
# --------------------------------------------------------------------------
def test_custom_wsl_distro_dir_resolves_configured_disk_not_c_default(dr):
    r"""Fixture 1: CustomWslDistroDir=E:\docker\wsl resolves the E: disk and
    never reports the C: fallback default as active."""
    configured = r"E:\docker\wsl"
    active = r"E:\docker\wsl\disk\docker_data.vhdx"
    c_default = r"C:\Users\me\AppData\Local\Docker\wsl\data\docker_data.vhdx"
    probe = FakeProbe(
        {
            active: INCIDENT_DISK_SIZE,
            # A real C: default file that must be ignored entirely.
            c_default: 5 * 1024**3,
        }
    )
    resolution = dr.resolve_windows_docker_disks(
        {"CustomWslDistroDir": configured},
        probe,
        localappdata=r"C:\Users\me\AppData\Local",
    )
    assert resolution.chosen is not None
    assert resolution.chosen.path == active
    assert resolution.chosen.kind == dr.KIND_WSL
    assert resolution.used_fallback is False
    assert not resolution.ambiguous
    # The C: default must never appear as a candidate.
    assert all("AppData" not in cand.path for cand in resolution.candidates)


def test_custom_wsl_and_datafolder_are_distinguished(dr):
    """Fixture 2: both CustomWslDistroDir and DataFolder set — the resolver
    keeps the WSL engine disk separate from the Hyper-V/legacy location."""
    wsl_disk = r"E:\docker\wsl\disk\docker_data.vhdx"
    legacy = r"C:\ProgramData\DockerDesktop\vm-data\DockerDesktop.vhdx"
    probe = FakeProbe(
        {
            wsl_disk: INCIDENT_DISK_SIZE,
            legacy: 12 * 1024**3,
        }
    )
    resolution = dr.resolve_windows_docker_disks(
        {
            "CustomWslDistroDir": r"E:\docker\wsl",
            "DataFolder": r"C:\ProgramData\DockerDesktop\vm-data",
        },
        probe,
        localappdata=r"C:\Users\me\AppData\Local",
    )
    kinds = {cand.kind for cand in resolution.candidates}
    assert kinds == {dr.KIND_WSL, dr.KIND_HYPERV_LEGACY}
    # The active WSL disk is the CustomWslDistroDir one, not DataFolder.
    assert resolution.chosen is not None
    assert resolution.chosen.kind == dr.KIND_WSL
    assert resolution.chosen.path == wsl_disk
    legacy_cands = [c for c in resolution.candidates if c.kind == dr.KIND_HYPERV_LEGACY]
    assert legacy_cands
    assert legacy_cands[0].path == legacy
    assert resolution.chosen not in legacy_cands


def test_ambiguous_missing_config_refuses_disk_action(dr):
    """Fixture 3: missing config with two plausible fallback disks is
    ambiguous — any disk action is refused even WITH confirmation."""
    disk_a = r"C:\Users\me\AppData\Local\Docker\wsl\data\docker_data.vhdx"
    disk_b = r"C:\Users\me\AppData\Local\DockerDesktop\disk\docker_data.vhdx"
    probe = FakeProbe({disk_a: 10 * 1024**3, disk_b: 9 * 1024**3})
    resolution = dr.resolve_windows_docker_disks(
        None,  # settings missing
        probe,
        localappdata=r"C:\Users\me\AppData\Local",
    )
    assert resolution.settings_present is False
    assert resolution.used_fallback is True
    assert resolution.ambiguous is True
    assert resolution.chosen is None
    # Ambiguity beats confirmation: even --yes + stopped is refused.
    code, message = dr.disk_action_gate(resolution, confirmed=True, docker_stopped=True)
    assert code == dr.EXIT_REFUSED_AMBIGUOUS
    assert "ambiguous" in message.lower() or "select" in message.lower()


def test_junction_resolution_scores_configured_parent(dr):
    """CustomWslDistroDir given as a junction resolves to its final path and
    still counts as a configured-parent match."""
    configured = r"E:\docker\wsl"
    resolved_root = r"F:\real\docker\wsl"
    active = r"F:\real\docker\wsl\disk\docker_data.vhdx"
    probe = FakeProbe({active: INCIDENT_DISK_SIZE}, resolve={configured: resolved_root})
    resolution = dr.resolve_windows_docker_disks(
        {"CustomWslDistroDir": configured}, probe, localappdata=r"C:\x"
    )
    assert resolution.chosen is not None
    assert resolution.chosen.path == active
    assert "configured-parent-match" in resolution.chosen.signals
    assert resolution.chosen.confidence == "high"


def test_user_selection_clears_ambiguity(dr):
    disk_a = r"C:\Users\me\AppData\Local\Docker\wsl\data\docker_data.vhdx"
    disk_b = r"C:\Users\me\AppData\Local\DockerDesktop\disk\docker_data.vhdx"
    probe = FakeProbe({disk_a: 10 * 1024**3, disk_b: 9 * 1024**3})
    resolution = dr.resolve_windows_docker_disks(
        None, probe, localappdata=r"C:\Users\me\AppData\Local"
    )
    assert resolution.ambiguous is True

    resolution = dr.apply_selection(resolution, disk_b)
    assert resolution.ambiguous is False
    assert resolution.chosen is not None
    assert resolution.chosen.path == disk_b
    code, _ = dr.disk_action_gate(resolution, confirmed=True, docker_stopped=True)
    assert code == dr.EXIT_OK


# --------------------------------------------------------------------------
# Settings reader.
# --------------------------------------------------------------------------
def test_read_docker_settings_prefers_settings_store_then_legacy(dr):
    appdata = r"C:\Users\me\AppData\Roaming"
    store = ntpath.join(appdata, "Docker", "settings-store.json")
    legacy = ntpath.join(appdata, "Docker", "settings.json")

    probe = FakeProbe({}, settings_text={store: '{"CustomWslDistroDir": "E:\\\\d"}'})
    data, source = dr.read_docker_settings(appdata, probe)
    assert data == {"CustomWslDistroDir": "E:\\d"}
    assert source == store

    probe = FakeProbe({}, settings_text={legacy: '{"DataFolder": "C:\\\\vm"}'})
    data, source = dr.read_docker_settings(appdata, probe)
    assert data == {"DataFolder": "C:\\vm"}
    assert source == legacy

    data, source = dr.read_docker_settings(appdata, FakeProbe({}))
    assert data is None
    assert source is None


# --------------------------------------------------------------------------
# Health assessment + classification.
# --------------------------------------------------------------------------
def test_healthy_daemon_is_a_noop(dr):
    snap = dr.HealthSnapshot(
        client_present=True,
        server_ok=True,
        free_disk_bytes=100 * 1024**3,
        free_mem_bytes=8 * 1024**3,
    )
    report = dr.assess_health(snap)
    assert report.healthy is True
    assert report.category == dr.CAT_HEALTHY
    assert report.failures == []
    assert report.advisories == []


def test_low_disk_is_advisory_not_failure(dr):
    snap = dr.HealthSnapshot(
        client_present=True,
        server_ok=True,
        free_disk_bytes=1 * 1024**3,  # below the 2 GiB advisory threshold
        free_mem_bytes=8 * 1024**3,
    )
    report = dr.assess_health(snap)
    assert report.healthy is True  # a reachable daemon stays healthy
    assert report.failures == []
    assert any("low free disk" in a for a in report.advisories)
    assert report.category == dr.CAT_HEALTHY


def test_missing_pipe_engine_down_classifies_as_engine_unavailable(dr):
    snap = dr.HealthSnapshot(
        client_present=True,
        server_ok=False,
        engine_error="open //./pipe/dockerDesktopLinuxEngine: "
        "The system cannot find the file specified.",
        free_disk_bytes=100 * 1024**3,
        free_mem_bytes=8 * 1024**3,
        wsl_docker_distro_state="Stopped",
    )
    assert dr.classify_failure(snap) == dr.CAT_ENGINE_UNAVAILABLE
    report = dr.assess_health(snap)
    assert report.healthy is False
    assert any("unreachable" in f for f in report.failures)


def test_classify_distinguishes_storage_and_resource_pressure(dr):
    storage = dr.HealthSnapshot(
        client_present=True,
        server_ok=False,
        free_disk_bytes=1 * 1024**3,
        free_mem_bytes=8 * 1024**3,
    )
    assert dr.classify_failure(storage) == dr.CAT_STORAGE_PRESSURE

    resource = dr.HealthSnapshot(
        client_present=True,
        server_ok=False,
        free_disk_bytes=100 * 1024**3,
        free_mem_bytes=256 * 1024**2,
    )
    assert dr.classify_failure(resource) == dr.CAT_RESOURCE_PRESSURE

    # A reachable daemon is healthy even with low disk (advisory only).
    healthy_low = dr.HealthSnapshot(
        client_present=True,
        server_ok=True,
        free_disk_bytes=1 * 1024**3,
        free_mem_bytes=8 * 1024**3,
    )
    assert dr.classify_failure(healthy_low) == dr.CAT_HEALTHY


# --------------------------------------------------------------------------
# Confirmation gate.
# --------------------------------------------------------------------------
def test_disk_action_gate_refuses_without_confirmation(dr):
    active = r"E:\docker\wsl\disk\docker_data.vhdx"
    probe = FakeProbe({active: INCIDENT_DISK_SIZE})
    resolution = dr.resolve_windows_docker_disks(
        {"CustomWslDistroDir": r"E:\docker\wsl"}, probe, localappdata=r"C:\x"
    )
    assert resolution.chosen is not None  # unambiguous

    code, _ = dr.disk_action_gate(resolution, confirmed=False, docker_stopped=True)
    assert code == dr.EXIT_REFUSED_CONFIRM

    code, msg = dr.disk_action_gate(resolution, confirmed=True, docker_stopped=False)
    assert code == dr.EXIT_REFUSED_CONFIRM
    assert "stopped" in msg.lower()

    code, _ = dr.disk_action_gate(resolution, confirmed=True, docker_stopped=True)
    assert code == dr.EXIT_OK


# --------------------------------------------------------------------------
# Bounded readiness wait + recovery verification.
# --------------------------------------------------------------------------
def test_wait_for_docker_is_bounded_and_polls(dr):
    calls = {"n": 0}

    def check():
        calls["n"] += 1
        return calls["n"] >= 3  # ready on the third poll

    sleeps: list[float] = []
    ready = dr.wait_for_docker(check=check, sleep=sleeps.append, out=io.StringIO())
    assert ready is True
    assert calls["n"] == 3
    assert sleeps == [dr.READY_INTERVAL_SECONDS, dr.READY_INTERVAL_SECONDS]


def test_wait_for_docker_gives_up_after_attempts(dr):
    sleeps: list[float] = []
    ready = dr.wait_for_docker(
        check=lambda: False, attempts=4, interval=2.0, sleep=sleeps.append, out=io.StringIO()
    )
    assert ready is False
    assert len(sleeps) == 3  # no sleep after the final failed attempt


def test_verify_recovery_checks_api_then_container(dr, monkeypatch):
    monkeypatch.setattr(dr, "docker_server_version", lambda: "27.0.1")
    # `run_hello_world` takes an `out=` sink since #891 (it announces the
    # wait before blocking, so the tool runner's heartbeat does not starve).
    monkeypatch.setattr(dr, "run_hello_world", lambda **_: (True, "hello-world ran"))
    ok, details = dr.verify_recovery()
    assert ok is True
    assert any("27.0.1" in d for d in details)
    assert any("hello-world" in d for d in details)

    monkeypatch.setattr(dr, "docker_server_version", lambda: None)
    ok, details = dr.verify_recovery()
    assert ok is False
    assert any("unreachable" in d for d in details)


def test_windows_restart_prefers_guarded_cli_launch(dr, monkeypatch):
    guard_calls = []
    monkeypatch.setattr(
        dr,
        "_launch_windows_docker_desktop_guard",
        lambda: (guard_calls.append(True) is None, "daemon guard started"),
    )
    monkeypatch.setattr(
        dr,
        "_wait_for_windows_server_deadline",
        lambda: True,
    )

    details = dr._execute_restart("Windows", hard=False)

    assert guard_calls == [True]
    assert any("guarded CLI" in detail for detail in details)


def test_windows_restart_falls_back_to_cli_when_direct_launch_is_unobserved(
    dr, monkeypatch
):
    desktop = r"C:\Program Files\Docker\Docker\Docker Desktop.exe"
    cli = dr.CompletedProcess(
        ["docker", "desktop", "start"], 0, stdout="Docker Desktop started\n", stderr=""
    )
    cli_calls: list[list[str]] = []

    def run_cli(argv, **_kwargs):
        cli_calls.append(argv)
        return cli

    monkeypatch.setattr(dr, "_run", run_cli)
    monkeypatch.setattr(dr, "_windows_docker_desktop_executable", lambda: desktop)
    monkeypatch.setattr(dr, "_windows_docker_process_identities", lambda: set())
    monkeypatch.setattr(
        dr,
        "_launch_windows_docker_desktop",
        lambda _path: (True, f"direct launch started {desktop}"),
    )
    observations = iter(
        [None, "new Docker runtime process observed after CLI launch: Docker Desktop"]
    )
    monkeypatch.setattr(
        dr,
        "_launch_windows_docker_desktop_guard",
        lambda: (False, "daemon guard launch failed"),
    )
    monkeypatch.setattr(
        dr, "_wait_for_windows_desktop_launch", lambda **_kwargs: next(observations)
    )

    details = dr._execute_restart("Windows", hard=False)

    assert ["docker", "desktop", "start"] in cli_calls
    assert any("trying CLI fallback" in detail for detail in details)
    assert any("after CLI launch" in detail for detail in details)


def test_windows_restart_falls_back_to_cli_when_executable_missing(dr, monkeypatch):
    cli = dr.CompletedProcess(
        ["docker", "desktop", "start"], 0, stdout="Docker Desktop started\n", stderr=""
    )
    cli_kwargs = {}

    def run_cli(*_args, **kwargs):
        cli_kwargs.update(kwargs)
        return cli

    monkeypatch.setattr(dr, "_run", run_cli)
    monkeypatch.setattr(
        dr,
        "_launch_windows_docker_desktop_guard",
        lambda: (False, "daemon guard launch failed"),
    )
    monkeypatch.setattr(dr, "_windows_docker_desktop_executable", lambda: None)
    monkeypatch.setattr(
        dr,
        "_wait_for_windows_desktop_launch",
        lambda **_kwargs: "new Docker runtime process observed after CLI launch: Docker Desktop",
    )

    details = dr._execute_restart("Windows", hard=False)

    assert "env" not in cli_kwargs
    assert any("direct Docker Desktop launch unavailable" in detail for detail in details)
    assert any("runtime process observed" in detail.lower() for detail in details)


def test_windows_restart_falls_back_to_cli_after_direct_launch_failure(dr, monkeypatch):
    desktop = r"C:\Program Files\Docker\Docker\Docker Desktop.exe"
    cli = dr.CompletedProcess(
        ["docker", "desktop", "start"], 1, stdout="", stderr="backend unavailable"
    )

    monkeypatch.setattr(dr, "_windows_docker_desktop_executable", lambda: desktop)
    monkeypatch.setattr(
        dr,
        "_launch_windows_docker_desktop_guard",
        lambda: (False, "daemon guard launch failed"),
    )
    monkeypatch.setattr(
        dr,
        "_launch_windows_docker_desktop",
        lambda _path: (False, f"direct Docker Desktop launch failed for {desktop}: denied"),
    )
    monkeypatch.setattr(dr, "_run", lambda *_args, **_kwargs: cli)

    details = dr._execute_restart("Windows", hard=False)

    assert any("direct Docker Desktop launch failed" in detail for detail in details)
    assert any("docker desktop start: exit 1" in detail.lower() for detail in details)


def test_direct_windows_launch_is_detached_and_declared_daemon(dr, monkeypatch):
    desktop = r"C:\Program Files\Docker\Docker\Docker Desktop.exe"
    captured = {}

    def fake_launch(command, **kwargs):
        captured["command"] = command
        captured.update(kwargs)
        return object()

    monkeypatch.setattr(dr, "launch_detached", fake_launch)

    ok, detail = dr._launch_windows_docker_desktop(desktop)

    assert ok is True
    assert desktop in captured["command"]
    assert captured["originator"] == "clud-docker-recover"
    assert "env" not in captured
    assert "direct Docker Desktop launch" in detail


def test_windows_guard_is_detached_declared_daemon_and_self_invokes(dr, monkeypatch):
    captured = {}

    def fake_launch(command, **kwargs):
        captured["command"] = command
        captured.update(kwargs)
        return object()

    monkeypatch.setattr(dr, "launch_detached", fake_launch)

    ok, detail = dr._launch_windows_docker_desktop_guard()

    assert ok is True
    assert captured["command"].endswith(dr.WINDOWS_DAEMON_GUARD_ARG)
    assert captured["originator"] == "clud-docker-recover-guard"
    assert "env" not in captured
    assert "daemon guard" in detail
    assert "docker-desktop-start.log" in detail


def test_windows_guard_owns_cli_tree_until_docker_exits(dr, monkeypatch):
    cli = dr.CompletedProcess(
        ["docker", "desktop", "start"], 0, stdout="started", stderr=""
    )
    calls = {}
    server_states = iter([True, True, False, True, False, False, False])
    released = []

    def run_cli(*, timeout):
        calls["timeout"] = timeout
        return cli

    monkeypatch.setattr(dr, "_run_windows_desktop_cli", run_cli)
    monkeypatch.setattr(dr, "_acquire_windows_guard_mutex", lambda: object())
    monkeypatch.setattr(
        dr, "_release_windows_guard_mutex", lambda handle: released.append(handle)
    )
    monkeypatch.setattr(
        dr, "_windows_docker_process_identities", lambda **_kwargs: set()
    )
    monkeypatch.setattr(
        dr, "_windows_server_ready", lambda **_kwargs: next(server_states)
    )
    monkeypatch.setattr(dr.time, "monotonic", lambda: 0.0)
    sleeps = []
    monkeypatch.setattr(dr.time, "sleep", sleeps.append)

    assert dr._windows_daemon_guard_main() == dr.EXIT_OK
    assert calls["timeout"] == dr.WINDOWS_GUARD_CLI_SECONDS
    assert sleeps == [5.0] * 5
    assert len(released) == 1


def test_windows_guard_singleton_retry_exits_without_starting_cli(dr, monkeypatch):
    monkeypatch.setattr(dr, "_acquire_windows_guard_mutex", lambda: None)

    def unexpected_run(*_args, **_kwargs):
        raise AssertionError("a second guard must reuse the live singleton")

    monkeypatch.setattr(dr, "_run_windows_desktop_cli", unexpected_run)

    assert dr._windows_daemon_guard_main() == dr.EXIT_OK


def test_windows_guard_false_success_uses_direct_fallback(dr, monkeypatch):
    cli = dr.CompletedProcess(
        ["docker", "desktop", "start"],
        0,
        stdout="Docker Desktop is already running\n",
        stderr="",
    )
    desktop = r"C:\Program Files\Docker\Docker\Docker Desktop.exe"
    launched = []
    server_states = iter([False, True, False, False, False])

    monkeypatch.setattr(dr, "_acquire_windows_guard_mutex", lambda: object())
    monkeypatch.setattr(dr, "_release_windows_guard_mutex", lambda _handle: None)
    monkeypatch.setattr(dr, "_run_windows_desktop_cli", lambda **_kwargs: cli)
    monkeypatch.setattr(
        dr, "_windows_docker_process_identities", lambda **_kwargs: set()
    )
    monkeypatch.setattr(
        dr, "_windows_server_ready", lambda **_kwargs: next(server_states)
    )
    monkeypatch.setattr(dr, "_windows_docker_desktop_executable", lambda: desktop)
    monkeypatch.setattr(
        dr,
        "_launch_windows_docker_desktop",
        lambda path: (launched.append(path) is None, f"started {path}"),
    )
    monkeypatch.setattr(dr.time, "monotonic", lambda: 0.0)
    monkeypatch.setattr(dr.time, "sleep", lambda _seconds: None)

    assert dr._windows_daemon_guard_main() == dr.EXIT_OK
    assert launched == [desktop]


def test_windows_guard_cli_budget_leaves_direct_observation_time(dr, monkeypatch):
    clock = {"now": 0.0}
    desktop = r"C:\Program Files\Docker\Docker\Docker Desktop.exe"
    launched = []
    server_states = iter([False, True, False, False, False])

    def run_cli(*, timeout):
        assert timeout == dr.WINDOWS_GUARD_CLI_SECONDS
        clock["now"] += timeout
        return None

    def sleep(seconds):
        clock["now"] += seconds

    monkeypatch.setattr(dr, "_acquire_windows_guard_mutex", lambda: object())
    monkeypatch.setattr(dr, "_release_windows_guard_mutex", lambda _handle: None)
    monkeypatch.setattr(dr, "_run_windows_desktop_cli", run_cli)
    monkeypatch.setattr(
        dr, "_windows_docker_process_identities", lambda **_kwargs: set()
    )
    monkeypatch.setattr(
        dr, "_windows_server_ready", lambda **_kwargs: next(server_states)
    )
    monkeypatch.setattr(dr, "_windows_docker_desktop_executable", lambda: desktop)
    monkeypatch.setattr(
        dr,
        "_launch_windows_docker_desktop",
        lambda path: (launched.append(path) is None, f"started {path}"),
    )
    monkeypatch.setattr(dr.time, "monotonic", lambda: clock["now"])
    monkeypatch.setattr(dr.time, "sleep", sleep)

    assert dr._windows_daemon_guard_main() == dr.EXIT_OK
    assert launched == [desktop]
    assert clock["now"] < dr.WINDOWS_GUARD_PARENT_WAIT_SECONDS


def test_windows_desktop_cli_uses_running_process_and_streams_to_bounded_log(
    dr, monkeypatch, tmp_path
):
    captured = {}

    class StartedCli:
        def wait(self, *, echo, timeout):
            captured["wait_timeout"] = timeout
            echo(b"starting Docker Desktop")
            return 0

    def running_process(command, **kwargs):
        captured["command"] = command
        captured.update(kwargs)
        return StartedCli()

    monkeypatch.setattr(dr, "RunningProcess", running_process)

    result = dr._run_windows_desktop_cli(timeout=7.0, log_dir=tmp_path)

    assert result is not None
    assert captured["command"] == ["docker", "desktop", "start"]
    assert captured == {
        "command": ["docker", "desktop", "start"],
        "capture": True,
        "text": False,
        "wait_timeout": 7.0,
    }
    assert captured["wait_timeout"] == 7.0
    assert (tmp_path / "docker-desktop-start.log").read_bytes() == b"starting Docker Desktop\n"


def test_windows_desktop_cli_diagnostic_log_keeps_only_bounded_tail(dr, tmp_path, monkeypatch):
    monkeypatch.setattr(dr, "WINDOWS_GUARD_LOG_MAX_BYTES", 8)
    log_path = tmp_path / "docker-desktop-start.log"

    dr._write_bounded_diagnostic_log(io.BytesIO(b"abcdefghij"), log_path)

    assert log_path.read_bytes() == b"cdefghij"


def test_windows_desktop_cli_timeout_is_owned_by_running_process(dr, monkeypatch):
    events = []

    class HungCli:
        def wait(self, *, echo, timeout):
            del echo
            events.append(("wait", timeout))
            raise TimeoutError("running-process enforced the deadline")

    def running_process(command, **kwargs):
        events.append(("running-process", command, kwargs))
        return HungCli()

    monkeypatch.setattr(dr, "RunningProcess", running_process)
    assert dr._run_windows_desktop_cli(timeout=7.0) is None
    assert events == [
        (
            "running-process",
            ["docker", "desktop", "start"],
            {"capture": True, "text": False},
        ),
        ("wait", 7.0),
    ]


def test_windows_desktop_cli_wait_error_returns_unavailable(dr, monkeypatch):
    class BrokenCli:
        def wait(self, *, echo, timeout):
            del echo
            raise OSError(f"wait failed after {timeout}")

    monkeypatch.setattr(dr, "RunningProcess", lambda *_args, **_kwargs: BrokenCli())

    assert dr._run_windows_desktop_cli(timeout=7.0) is None


def test_windows_guarded_server_wait_honors_wall_clock_deadline(dr):
    clock = {"now": 0.0}
    probe_timeouts = []

    def now():
        return clock["now"]

    def check(*, timeout):
        probe_timeouts.append(timeout)
        clock["now"] += timeout
        return False

    def sleep(seconds):
        clock["now"] += seconds

    assert (
        dr._wait_for_windows_server_deadline(
            seconds=3.0,
            interval=1.0,
            check=check,
            now=now,
            sleep=sleep,
        )
        is False
    )
    assert clock["now"] == 3.0
    assert max(probe_timeouts) <= 1.0


def test_windows_launch_observation_ignores_stale_authoritative_process(dr, monkeypatch):
    baseline = {("com.docker.backend", 123)}
    monkeypatch.setattr(dr, "docker_server_version", lambda: None)
    monkeypatch.setattr(dr, "_windows_docker_process_identities", lambda: baseline)
    assert dr._windows_desktop_launch_observation(baseline) is None

    monkeypatch.setattr(
        dr,
        "_windows_docker_process_identities",
        lambda: baseline | {("Docker Desktop", 456)},
    )
    observation = dr._windows_desktop_launch_observation(baseline)
    assert observation is not None
    assert "Docker Desktop (PID 456)" in observation


def test_windows_process_identity_parser_filters_helpers(dr):
    tasklist = "\n".join(
        [
            '"Docker Desktop.exe","101","Console","1","123,456 K"',
            '"com.docker.backend.exe","202","Console","1","50,000 K"',
            '"com.docker.build.exe","303","Console","1","40,000 K"',
            '"dockerd.exe","404","Console","1","30,000 K"',
        ]
    )

    assert dr._parse_windows_docker_process_identities(tasklist) == {
        ("Docker Desktop", 101),
        ("com.docker.backend", 202),
    }


def test_windows_launch_observation_wait_is_bounded(dr):
    calls = {"count": 0}
    sleeps: list[float] = []

    def check():
        calls["count"] += 1
        return "observed" if calls["count"] == 3 else None

    observation = dr._wait_for_windows_desktop_launch(
        check=check, attempts=4, interval=0.5, sleep=sleeps.append
    )

    assert observation == "observed"
    assert calls["count"] == 3
    assert sleeps == [0.5, 0.5]


def test_windows_desktop_executable_resolves_program_files_without_path(dr):
    expected = r"D:\Apps\Docker\Docker\Docker Desktop.exe"
    checked: list[str] = []

    def exists(path):
        checked.append(path)
        return path == expected

    actual = dr._windows_docker_desktop_executable(
        env={
            "ProgramFiles": r"D:\Apps",
            "LOCALAPPDATA": r"C:\Users\me\AppData\Local",
            "PATH": r"C:\not-docker",
        },
        exists=exists,
    )

    assert actual == expected
    assert checked[0] == expected
    assert all("not-docker" not in path for path in checked)


# --------------------------------------------------------------------------
# Recovery plans + doctor read-only guarantee.
# --------------------------------------------------------------------------
def test_windows_restart_plan_preserves_data_and_cycles_wsl(dr):
    plan = " ".join(dr.windows_restart_plan())
    assert "wsl --shutdown" in plan
    assert "PRESERVED" in plan
    assert "STOP" in plan


def test_doctor_never_restarts(dr, monkeypatch):
    """cmd_doctor is read-only: it must never invoke the restart path."""
    monkeypatch.setattr(dr.platform, "system", lambda: "Linux")
    monkeypatch.setattr(
        dr,
        "gather_snapshot",
        lambda: dr.HealthSnapshot(
            client_present=True,
            server_ok=True,
            free_disk_bytes=100 * 1024**3,
            free_mem_bytes=8 * 1024**3,
        ),
    )

    def boom(*_args, **_kwargs):
        raise AssertionError("doctor must not execute a restart")

    monkeypatch.setattr(dr, "_execute_restart", boom)
    assert dr.cmd_doctor(_namespace()) == dr.EXIT_OK


def test_doctor_reports_unhealthy_engine(dr, monkeypatch):
    monkeypatch.setattr(dr.platform, "system", lambda: "Linux")
    monkeypatch.setattr(
        dr,
        "gather_snapshot",
        lambda: dr.HealthSnapshot(
            client_present=True,
            server_ok=False,
            engine_error="engine down",
            free_disk_bytes=100 * 1024**3,
            free_mem_bytes=8 * 1024**3,
        ),
    )
    assert dr.cmd_doctor(_namespace()) == dr.EXIT_UNHEALTHY


# --------------------------------------------------------------------------
# Garbage collection (dangling Docker objects).
# --------------------------------------------------------------------------
NOW = 1_000_000.0


def _img(dr, name, *, hours_old, in_use=False, size=1_000):
    return dr.GcImage(
        id=f"sha256:{name}",
        tags=[] if name.startswith("dangling") else [f"repo:{name}"],
        created_epoch=NOW - hours_old * 3600,
        size_bytes=size,
        in_use=in_use,
    )


def test_gc_age_threshold_filters_by_age(dr):
    inv = dr.GcInventory(
        images=[
            _img(dr, "old", hours_old=48, size=2_000),
            _img(dr, "recent", hours_old=1, size=5_000),
        ]
    )
    plan = dr.plan_gc(inv, now=NOW, on_system_volume=False)  # threshold 24h
    picked = {i.id for i in plan.images}
    assert picked == {"sha256:old"}
    assert plan.reclaimable_bytes == 2_000


def test_gc_never_touches_named_or_in_use_volumes(dr):
    inv = dr.GcInventory(
        volumes=[
            dr.GcVolume(name="my-named-data", anonymous=False, in_use=False),
            dr.GcVolume(name="a" * 64, anonymous=True, in_use=False),  # anon orphan
            dr.GcVolume(name="b" * 64, anonymous=True, in_use=True),  # anon but attached
        ]
    )
    plan = dr.plan_gc(inv, now=NOW, on_system_volume=False)
    picked = {v.name for v in plan.volumes}
    assert picked == {"a" * 64}  # only the anonymous, unreferenced volume


def test_gc_never_touches_running_or_in_use_images(dr):
    inv = dr.GcInventory(
        images=[
            _img(dr, "old-in-use", hours_old=999, in_use=True),  # backs a running container
            _img(dr, "old-free", hours_old=999, in_use=False),
        ],
        containers=[
            dr.GcContainer(id="run", running=True, created_epoch=NOW - 999 * 3600),
            dr.GcContainer(id="dead", running=False, created_epoch=NOW - 999 * 3600),
        ],
    )
    plan = dr.plan_gc(inv, now=NOW, on_system_volume=False)
    assert {i.id for i in plan.images} == {"sha256:old-free"}
    assert {c.id for c in plan.containers} == {"dead"}


def test_low_disk_recommends_gc_before_restart(dr):
    report = dr.HealthReport(healthy=False, category=dr.CAT_STORAGE_PRESSURE)
    steps = dr.recommended_remedy(report, disk_low=True)
    assert "gc" in steps
    assert "restart" in steps
    assert steps.index("gc") < steps.index("restart")  # lightest rung first
    assert steps.index("gc") < steps.index("disk")


def test_system_volume_lowers_gc_threshold(dr):
    assert dr.gc_age_threshold_hours(True, 24.0) < dr.gc_age_threshold_hours(False, 24.0)
    c_disk = r"C:\Users\me\AppData\Local\Docker\wsl\data\x.vhdx"
    e_disk = r"E:\docker\wsl\disk\docker_data.vhdx"
    assert dr.is_system_volume(c_disk, system_drive="C:")
    assert not dr.is_system_volume(e_disk, system_drive="C:")


def test_parse_docker_size(dr):
    assert dr._parse_docker_size("1.5GB") == 1_500_000_000
    assert dr._parse_docker_size("512MB") == 512_000_000
    assert dr._parse_docker_size("0B") == 0
    assert dr._parse_docker_size("2GiB") == 2 * 1024**3


def _namespace():
    import argparse

    return argparse.Namespace()


# ==========================================================================
# Issue #891 - corrupted engine DATA disk on the WSL2 backend.
#
# The incident these cover: Docker Desktop 28.5.1, docker-desktop distro
# STATE=Running, engine answering HTTP 500 for 12+ minutes, 86 GiB RAM and
# 55 GiB disk free. Both `restart` and `reset` failed; only deleting
# docker_data.vhdx fixed it, and the delete only worked after
# `Dismount-DiskImage` released the surface-attached VHD.
# ==========================================================================
def test_engine_probe_states_are_not_collapsed(dr):
    """A slow/5xx engine must not be reported as a missing CLI.

    `_run` returns None for a timeout AND for a missing binary, so
    the engine probe blamed the CLI when the engine was the sick part.
    """
    assert (
        dr.classify_engine_message(
            "Error response from daemon: 500 Internal Server Error"
        )
        == dr.ENGINE_SERVER_ERROR
    )
    assert (
        dr.classify_engine_message(
            "still waiting for linux/wsl init control API to respond"
        )
        == dr.ENGINE_SERVER_ERROR
    )
    # The daemon genuinely being absent stays 'unreachable'.
    assert (
        dr.classify_engine_message(
            "error during connect: cannot connect to the Docker daemon at npipe"
        )
        == dr.ENGINE_UNREACHABLE
    )


def test_engine_probe_timeout_reports_server_error_not_cli_missing(dr, monkeypatch):
    """The exact #891 gap-1 misdiagnosis, at the probe boundary."""
    monkeypatch.setattr(dr, "_run_timed", lambda *a, **k: (None, True))
    state, message = dr.probe_engine()
    # A timeout is its OWN state: a cold Docker Desktop looks identical for a
    # minute or two, and treating it as the wedged signature would recommend
    # deleting the data disk during a normal start.
    assert state == dr.ENGINE_TIMEOUT
    assert "did not respond" in message
    assert "CLI could not be executed" not in message

    # And a genuinely missing binary still says so.
    monkeypatch.setattr(dr, "_run_timed", lambda *a, **k: (None, False))
    state, message = dr.probe_engine()
    assert state == dr.ENGINE_CLI_MISSING
    assert "CLI could not be executed" in message


def test_engine_wedged_classification_and_ladder(dr):
    """Distro Running + 5xx + resources fine is its own category.

    Previously this landed in `engine-unavailable`, whose remedy is
    `restart` - which does not clear a corrupted data disk.
    """
    wedged = dr.HealthSnapshot(
        client_present=True,
        server_ok=False,
        engine_error="500 Internal Server Error",
        engine_state=dr.ENGINE_SERVER_ERROR,
        free_mem_bytes=86 * 1024**3,
        free_disk_bytes=55 * 1024**3,
        wsl_docker_distro_state="Running",
    )
    assert dr.classify_failure(wedged) == dr.CAT_ENGINE_WEDGED

    report = dr.assess_health(wedged)
    ladder = dr.recommended_remedy(report, disk_low=False)
    assert ladder[0] == "restart"
    assert "reset" in ladder
    # The ladder has to reach the gated data-disk reset; restart/reset alone
    # did not fix the incident.
    assert ladder[-1] == "disk"


def test_doctor_reports_engine_unhealthy_not_unreachable(dr):
    """Criterion 1's wording: the server is reachable, the engine is sick."""
    wedged = dr.HealthSnapshot(
        client_present=True,
        server_ok=False,
        engine_error="500 Internal Server Error",
        engine_state=dr.ENGINE_SERVER_ERROR,
        free_mem_bytes=86 * 1024**3,
        free_disk_bytes=55 * 1024**3,
        wsl_docker_distro_state="Running",
    )
    text = " ".join(dr.assess_health(wedged).failures)
    assert "server reachable but engine unhealthy" in text
    assert "docker engine unreachable" not in text
    assert "CLI could not be executed" not in text

    # A genuinely absent daemon keeps the original wording.
    absent = dr.HealthSnapshot(
        client_present=True,
        server_ok=False,
        engine_error="cannot connect to the Docker daemon",
        engine_state=dr.ENGINE_UNREACHABLE,
    )
    assert "docker engine unreachable" in " ".join(dr.assess_health(absent).failures)


def test_engine_wedged_is_conservative(dr):
    """It only ever narrows a diagnosis that would be engine-unavailable."""
    base = dict(
        client_present=True,
        server_ok=False,
        free_mem_bytes=86 * 1024**3,
        free_disk_bytes=55 * 1024**3,
    )
    # Distro not running -> the #531 shape, not this one.
    stopped = dr.HealthSnapshot(
        **base, engine_state=dr.ENGINE_SERVER_ERROR, wsl_docker_distro_state="Stopped"
    )
    assert dr.classify_failure(stopped) == dr.CAT_ENGINE_UNAVAILABLE
    # Server unreachable (pipe absent) -> also not this one.
    unreachable = dr.HealthSnapshot(
        **base, engine_state=dr.ENGINE_UNREACHABLE, wsl_docker_distro_state="Running"
    )
    assert dr.classify_failure(unreachable) == dr.CAT_ENGINE_UNAVAILABLE
    # Unknown engine state (older snapshot) -> unchanged behaviour.
    unknown = dr.HealthSnapshot(**base, wsl_docker_distro_state="Running")
    assert dr.classify_failure(unknown) == dr.CAT_ENGINE_UNAVAILABLE
    # Resource pressure still wins - it is checked first.
    starved = dr.HealthSnapshot(
        client_present=True,
        server_ok=False,
        engine_state=dr.ENGINE_SERVER_ERROR,
        free_mem_bytes=1,
        free_disk_bytes=1,
        wsl_docker_distro_state="Running",
    )
    assert dr.classify_failure(starved) == dr.CAT_STORAGE_PRESSURE


def test_disk_plan_dismounts_the_surface_attached_vhd(dr):
    """Gap 3, the highest-value one.

    `wsl --shutdown` does not release the VHDX - `Get-DiskImage` still
    reports Attached=True and the delete fails with "being used by another
    process". Only `Dismount-DiskImage` released it.
    """
    path = "E:\\Docker\\wsl\\disk\\docker_data.vhdx"
    for action in ("delete", "compact"):
        plan = dr.windows_disk_plan(action, path)
        text = "\n".join(plan)
        assert "Dismount-DiskImage" in text, action
        assert "Get-DiskImage" in text, action
        assert "Attached : False" in text, action
        # The escalation ladder if it is still locked.
        assert "WslService" in text, action
        assert "vmcompute" in text, action

        dismount = next(i for i, s in enumerate(plan) if "Dismount-DiskImage" in s)
        verify = next(i for i, s in enumerate(plan) if "Get-DiskImage" in s)
        mutate = next(
            i
            for i, s in enumerate(plan)
            if ("remove " in s.lower() or "Optimize-VHD" in s)
        )
        assert dismount < verify < mutate, action


def test_disk_plan_stops_service_first_and_rechecks_processes(dr):
    """Gap 5: the UI respawns and can re-attach the disk mid-operation."""
    plan = dr.windows_disk_plan("delete", "E:\\d\\docker_data.vhdx")
    service = next(i for i, s in enumerate(plan) if "com.docker.service" in s)
    shutdown = next(i for i, s in enumerate(plan) if "wsl --shutdown" in s)
    recheck = next(i for i, s in enumerate(plan) if "respawned" in s)
    mutate = next(i for i, s in enumerate(plan) if "remove " in s.lower())
    assert service < shutdown, "service must stop before wsl --shutdown"
    assert recheck < mutate, "processes re-checked immediately before mutation"


def test_non_mutating_disk_actions_skip_the_unlock(dr):
    """`prune` and `reset` never touch the VHD file, so no dismount."""
    for action in ("prune", "reset"):
        text = "\n".join(dr.windows_disk_plan(action, "E:\\d\\docker_data.vhdx"))
        assert "Dismount-DiskImage" not in text, action


def test_readiness_poll_emits_progress_and_supports_cold_start(dr):
    """Gap 4: `reset --yes` was killed twice at exactly 120s (exit 124).

    Every attempt must print, so the clud tool runner's progress heartbeat
    never starves, and the cold-start budget must exceed a Docker Desktop
    cold start (60-120s) rather than giving up at 20s.
    """
    assert dr.READY_ATTEMPTS_COLD * dr.READY_INTERVAL_SECONDS >= 120.0

    out = io.StringIO()
    calls = {"n": 0}

    def check():
        calls["n"] += 1
        return calls["n"] >= 3

    assert (
        dr.wait_for_docker(check, attempts=5, interval=0, sleep=lambda _: None, out=out)
        is True
    )
    text = out.getvalue()
    assert "waiting for the engine" in text
    assert "readiness attempt 1/5" in text
    assert "engine ready after 3 attempt(s)" in text


def test_hello_world_announces_before_blocking(dr, monkeypatch):
    """The 120s silent call is what tripped the runner's progress timeout."""
    monkeypatch.setattr(dr, "_run", lambda *a, **k: None)
    out = io.StringIO()
    ok, _ = dr.run_hello_world(out=out)
    assert ok is False
    text = out.getvalue()
    assert "hello-world" in text
    # It must say how long it may block, before it blocks.
    assert f"{dr.HELLO_WORLD_TIMEOUT_SECONDS:g}s" in text


# --------------------------------------------------------------------------
# Wiring tests (issue #891 review).
#
# The first cut of this work implemented every fix as a well-tested pure
# function and then never called any of them from the command path. The pure
# tests stayed green while the shipped tool behaved exactly as before. These
# assert the COMMAND path, so an unwired fix fails.
# --------------------------------------------------------------------------
def test_cmd_disk_actually_prints_the_unlock_plan(dr, monkeypatch, capsys):
    """`cmd_disk` must render `windows_disk_plan`, not a hardcoded plan."""
    path = "E:\\Docker\\wsl\\disk\\docker_data.vhdx"
    chosen = dr.DiskCandidate(
        path=path,
        resolved_path=path,
        size_bytes=INCIDENT_DISK_SIZE,
        kind=dr.KIND_WSL,
        source="CustomWslDistroDir",
        score=100,
        signals=["configured-parent-match"],
    )
    resolution = dr.DiskResolution(
        candidates=[chosen],
        chosen=chosen,
        settings_present=True,
        settings_source="settings-store.json",
        used_fallback=False,
        ambiguous=False,
    )
    monkeypatch.setattr(dr.platform, "system", lambda: "Windows")
    monkeypatch.setattr(dr, "_windows_resolution", lambda: resolution)
    # Gates: Docker is stopped and the user confirmed.
    monkeypatch.setattr(dr, "docker_server_version", lambda: None)
    monkeypatch.setattr(dr, "list_docker_processes", lambda: [])

    args = argparse.Namespace(action="delete", select=None, yes=True, json=False)
    code = dr.cmd_disk(args)

    assert code == dr.EXIT_NOT_AUTO_EXECUTED, "must still refuse to auto-execute"
    printed = capsys.readouterr().out
    assert "Dismount-DiskImage" in printed
    assert "Attached : False" in printed
    assert "com.docker.service" in printed
    assert "respawned" in printed


def test_recovery_uses_the_cold_start_budget(dr, monkeypatch):
    """`_run_recovery` must pass READY_ATTEMPTS_COLD, not the 20s default."""
    seen = {}

    def fake_wait(check=None, **kwargs):
        seen.update(kwargs)
        return True

    monkeypatch.setattr(dr, "wait_for_docker", fake_wait)
    monkeypatch.setattr(dr, "_execute_restart", lambda *a, **k: ["launched"])
    monkeypatch.setattr(dr, "verify_recovery", lambda **_: (True, ["ok"]))
    monkeypatch.setattr(
        dr,
        "gather_snapshot",
        lambda: dr.HealthSnapshot(client_present=True, server_ok=False),
    )
    monkeypatch.setattr(dr.platform, "system", lambda: "Windows")

    args = argparse.Namespace(yes=True, force=False, json=False)
    dr._run_recovery(args, label="reset")

    assert seen.get("attempts") == dr.READY_ATTEMPTS_COLD, (
        "a cold Docker Desktop start needs the long budget; the 20s default "
        "reports 'not ready' while Desktop is still starting normally"
    )


def test_engine_timeout_alone_never_recommends_deleting_the_disk(dr):
    """The safety property behind splitting ENGINE_TIMEOUT out.

    A cold Docker Desktop produces a probe timeout with the distro already
    Running. If that classified as `engine-wedged`, `doctor` would recommend
    a ladder ending in `disk --action delete` — total image/volume loss —
    during an ordinary start.
    """
    starting = dr.HealthSnapshot(
        client_present=True,
        server_ok=False,
        engine_state=dr.ENGINE_TIMEOUT,
        engine_error="docker server did not respond within the probe timeout",
        free_mem_bytes=86 * 1024**3,
        free_disk_bytes=55 * 1024**3,
        wsl_docker_distro_state="Running",
    )
    assert dr.classify_failure(starting) != dr.CAT_ENGINE_WEDGED
    ladder = dr.recommended_remedy(dr.assess_health(starting), disk_low=False)
    assert "disk" not in ladder
    # And it must not claim the server was reachable.
    text = " ".join(dr.assess_health(starting).failures)
    assert "reachable" not in text


def test_engine_wedged_does_not_steal_healthy_or_resource_pressure(dr):
    """Completes the conservatism matrix the first cut left partial."""
    # Reachable server wins outright, whatever the engine_state says.
    healthy = dr.HealthSnapshot(
        client_present=True, server_ok=True, engine_state=dr.ENGINE_SERVER_ERROR
    )
    assert dr.classify_failure(healthy) == dr.CAT_HEALTHY
    # Low memory (disk fine) is resource-pressure, checked before the wedge.
    mem = dr.HealthSnapshot(
        client_present=True,
        server_ok=False,
        engine_state=dr.ENGINE_SERVER_ERROR,
        free_mem_bytes=1,
        free_disk_bytes=55 * 1024**3,
        wsl_docker_distro_state="Running",
    )
    assert dr.classify_failure(mem) == dr.CAT_RESOURCE_PRESSURE
    # No WSL distro state at all (macOS/Linux) never wedges.
    posix = dr.HealthSnapshot(
        client_present=True,
        server_ok=False,
        engine_state=dr.ENGINE_SERVER_ERROR,
        free_mem_bytes=86 * 1024**3,
        free_disk_bytes=55 * 1024**3,
    )
    assert dr.classify_failure(posix) == dr.CAT_ENGINE_UNAVAILABLE
