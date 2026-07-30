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
    monkeypatch.setattr(dr, "run_hello_world", lambda: (True, "hello-world ran"))
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
    cli = dr.subprocess.CompletedProcess(
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
    cli = dr.subprocess.CompletedProcess(
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

    assert cli_kwargs["env"]["RUNNING_PROCESS_IS_DAEMON"] == "1"
    assert any("direct Docker Desktop launch unavailable" in detail for detail in details)
    assert any("runtime process observed" in detail.lower() for detail in details)


def test_windows_restart_falls_back_to_cli_after_direct_launch_failure(dr, monkeypatch):
    desktop = r"C:\Program Files\Docker\Docker\Docker Desktop.exe"
    cli = dr.subprocess.CompletedProcess(
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

    def fake_popen(argv, **kwargs):
        captured["argv"] = argv
        captured.update(kwargs)
        return object()

    monkeypatch.setattr(dr.subprocess, "Popen", fake_popen)

    ok, detail = dr._launch_windows_docker_desktop(desktop)

    assert ok is True
    assert captured["argv"] == [desktop]
    assert captured["env"]["RUNNING_PROCESS_IS_DAEMON"] == "1"
    assert captured["stdin"] is dr.subprocess.DEVNULL
    assert captured["stdout"] is dr.subprocess.DEVNULL
    assert captured["stderr"] is dr.subprocess.DEVNULL
    assert captured["close_fds"] is True
    assert captured["creationflags"] & dr.WINDOWS_DETACHED_PROCESS
    assert captured["creationflags"] & dr.WINDOWS_CREATE_NEW_PROCESS_GROUP
    assert "direct Docker Desktop launch" in detail


def test_windows_guard_is_detached_declared_daemon_and_self_invokes(dr, monkeypatch):
    captured = {}

    def fake_popen(argv, **kwargs):
        captured["argv"] = argv
        captured.update(kwargs)
        return object()

    monkeypatch.setattr(dr.subprocess, "Popen", fake_popen)

    ok, detail = dr._launch_windows_docker_desktop_guard()

    assert ok is True
    assert captured["argv"][-1] == dr.WINDOWS_DAEMON_GUARD_ARG
    assert captured["env"]["RUNNING_PROCESS_IS_DAEMON"] == "1"
    assert captured["stdin"] is dr.subprocess.DEVNULL
    assert captured["stdout"] is dr.subprocess.DEVNULL
    assert captured["stderr"] is dr.subprocess.DEVNULL
    assert captured["close_fds"] is True
    assert captured["creationflags"] & dr.WINDOWS_DETACHED_PROCESS
    assert captured["creationflags"] & dr.WINDOWS_CREATE_NEW_PROCESS_GROUP
    assert "daemon guard" in detail


def test_windows_guard_owns_cli_tree_until_docker_exits(dr, monkeypatch):
    cli = dr.subprocess.CompletedProcess(
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
    cli = dr.subprocess.CompletedProcess(
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


def test_windows_desktop_cli_timeout_never_drains_inherited_pipes(dr, monkeypatch):
    events = []

    class HungCli:
        waits = 0

        def wait(self, *, timeout):
            events.append(("wait", timeout))
            self.waits += 1
            if self.waits == 1:
                raise dr.subprocess.TimeoutExpired(
                    ["docker", "desktop", "start"], timeout
                )
            return 1

        def kill(self):
            events.append(("kill",))

        def communicate(self):
            raise AssertionError(
                "communicate can hang on pipe handles inherited by daemon descendants"
            )

    def popen(command, **kwargs):
        events.append(("popen", command, kwargs))
        return HungCli()

    monkeypatch.setattr(dr.subprocess, "Popen", popen)

    assert dr._run_windows_desktop_cli(timeout=7.0) is None
    assert events[0][1] == ["docker", "desktop", "start"]
    assert events[0][2]["env"]["RUNNING_PROCESS_IS_DAEMON"] == "1"
    assert events[0][2]["stdin"] is dr.subprocess.DEVNULL
    assert events[0][2]["stdout"] is dr.subprocess.DEVNULL
    assert events[0][2]["stderr"] is dr.subprocess.DEVNULL
    assert events[1:] == [
        ("wait", 7.0),
        ("kill",),
        ("wait", dr.WINDOWS_GUARD_CLI_TERMINATE_SECONDS),
    ]


def test_windows_desktop_cli_wait_error_returns_unavailable(dr, monkeypatch):
    class BrokenCli:
        def wait(self, *, timeout):
            raise OSError(f"wait failed after {timeout}")

    monkeypatch.setattr(dr.subprocess, "Popen", lambda *_args, **_kwargs: BrokenCli())

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
