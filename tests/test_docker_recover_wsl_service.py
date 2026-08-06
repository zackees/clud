"""WslService STOP_PENDING recovery gate (#632).

The recovery this guards is a **UAC-elevated force-kill of a system service
process**. That is the most dangerous thing `docker_recover.py` can do, so the
decision is a pure function over an injected SCM snapshot and every state in
the issue's acceptance list is pinned here — a wedged `WslService` is not
something CI can produce on demand, and "we tested it by hand once" is not a
regression guard.

The bias throughout is fail-closed: anything that is not positively the wedged
state we diagnosed must refuse.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = (
    ROOT / "crates" / "clud-bin" / "assets" / "tools" / "docker"
    / "docker_recover.py"
)


@pytest.fixture(scope="module")
def mod():
    spec = importlib.util.spec_from_file_location("docker_recover_wsl", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules["docker_recover_wsl"] = module
    spec.loader.exec_module(module)
    return module


GOOD_BINARY = r"C:\Program Files\WSL\wslservice.exe"


# ------------------------------------------------------------- sc parsing --


def test_queryex_output_yields_state_and_pid(mod):
    text = """
SERVICE_NAME: WslService
        TYPE               : 10  WIN32_OWN_PROCESS
        STATE              : 3  STOP_PENDING
                                (NOT_STOPPABLE, NOT_PAUSABLE, IGNORES_SHUTDOWN)
        WIN32_EXIT_CODE    : 0  (0x0)
        CHECKPOINT         : 0x0
        WAIT_HINT          : 0x0
        PID                : 4242
        FLAGS              :
"""
    status = mod.parse_sc_queryex(text)
    assert status.state == "STOP_PENDING"
    assert status.pid == 4242


def test_state_is_read_from_the_numeric_code_not_the_english_word(mod):
    """`sc` output is localized on non-English Windows. Keying on the numeric
    code keeps the gate working there instead of silently refusing forever."""
    text = "        STATE              : 4  DDDDDD\n        PID                : 7\n"
    status = mod.parse_sc_queryex(text)
    assert status.state == "RUNNING"


def test_unparseable_queryex_yields_nothing_rather_than_guessing(mod):
    status = mod.parse_sc_queryex("[SC] EnumQueryServicesStatus:OpenService FAILED 1060")
    assert status.state is None
    assert status.pid is None


def test_qc_output_yields_the_binary_path(mod):
    text = """
[SC] QueryServiceConfig SUCCESS
SERVICE_NAME: WslService
        BINARY_PATH_NAME   : "C:\\Program Files\\WSL\\wslservice.exe"
        DISPLAY_NAME       : WSL Service
"""
    assert mod.parse_sc_qc_binary_path(text) == r"C:\Program Files\WSL\wslservice.exe"


# ------------------------------------------------------------- the gate ----


def test_stop_pending_with_a_valid_pid_and_image_is_authorized(mod):
    status = mod.ServiceStatus(state="STOP_PENDING", pid=4242)
    allowed, reason = mod.wsl_service_recovery_gate(status, GOOD_BINARY)
    assert allowed
    assert "4242" in reason


@pytest.mark.parametrize("state", ["RUNNING", "STOPPED", "START_PENDING", "PAUSED"])
def test_every_healthy_state_refuses(mod, state):
    """A running service is fine and a stopped one needs a plain `sc start`.
    Neither warrants killing a system service process."""
    status = mod.ServiceStatus(state=state, pid=4242)
    allowed, reason = mod.wsl_service_recovery_gate(status, GOOD_BINARY)
    assert not allowed
    assert "STOP_PENDING" in reason


def test_an_unknown_state_refuses(mod):
    allowed, _reason = mod.wsl_service_recovery_gate(
        mod.ServiceStatus(state=None, pid=4242), GOOD_BINARY)
    assert not allowed


@pytest.mark.parametrize("pid", [0, None])
def test_a_missing_service_pid_refuses(mod, pid):
    """SCM reports 0 for a service with no process. There is nothing safe to
    terminate, and PID 0 is not a target."""
    status = mod.ServiceStatus(state="STOP_PENDING", pid=pid)
    allowed, reason = mod.wsl_service_recovery_gate(status, GOOD_BINARY)
    assert not allowed
    assert "PID" in reason or "pid" in reason


def test_a_mismatched_image_refuses(mod):
    """The SCM-reported binary must be the executable we expect, so a
    mis-registered or hijacked service name cannot redirect a privileged kill."""
    status = mod.ServiceStatus(state="STOP_PENDING", pid=4242)
    allowed, reason = mod.wsl_service_recovery_gate(
        status, r"C:\Windows\System32\svchost.exe")
    assert not allowed
    assert "svchost.exe" in reason


def test_an_unknown_binary_path_refuses(mod):
    status = mod.ServiceStatus(state="STOP_PENDING", pid=4242)
    allowed, reason = mod.wsl_service_recovery_gate(status, None)
    assert not allowed
    assert "unverified" in reason


def test_the_real_wsl_install_path_is_recognized(mod):
    r"""Regression: extracting the image by splitting on whitespace looks right
    and is wrong — the shipped path is `C:\Program Files\WSL\wslservice.exe`,
    so the first token is `C:\Program` and *every* genuine install would be
    refused forever. Caught by this test, not by reading."""
    assert mod.service_image_name(GOOD_BINARY) == "wslservice.exe"
    assert mod.service_image_name(r"C:\Program Files\WSL\wslservice.exe -k x") == (
        "wslservice.exe"
    )
    assert mod.service_image_name(r'"C:\Program Files\WSL\wslservice.exe"') == (
        "wslservice.exe"
    )


def test_a_quoted_binary_path_with_arguments_still_resolves(mod):
    """`sc qc` may report the image with trailing service arguments. The gate
    must compare the executable, not the whole command line, or a legitimate
    install would be refused forever."""
    status = mod.ServiceStatus(state="STOP_PENDING", pid=4242)
    allowed, _reason = mod.wsl_service_recovery_gate(
        status, '"C:\\Program Files\\WSL\\wslservice.exe" -k netsvcs')
    assert allowed


# ------------------------------------------- the privileged command shape --


def test_the_elevated_command_is_scoped_to_kill_and_start(mod):
    """The blast radius of the privileged step, asserted as data.

    The issue is explicit that images, volumes, distros and VHDs must survive.
    This command is the only place recovery holds elevation, so it is the only
    place that could violate that — a test on its shape is worth more than a
    comment saying we won't.
    """
    argv = mod.elevated_restart_command(4242)
    joined = " ".join(argv)

    assert "running-process --terminate-tree 4242" in joined
    assert "sc.exe start WslService" in joined
    assert "-Verb RunAs" in joined, "elevation must be a visible UAC prompt"

    for destructive in (
        "--unregister", "wsl --shutdown", "Remove-Item", "diskpart",
        "docker", "rm ", "del ", "format",
    ):
        assert destructive not in joined, (
            f"elevated command must never contain {destructive!r}: {joined}"
        )


def test_the_elevated_command_rejects_a_non_integer_pid(mod):
    """The PID is interpolated into a shell string, so it must be an integer.
    `int()` is what stops a crafted value from becoming extra commands."""
    with pytest.raises((ValueError, TypeError)):
        mod.elevated_restart_command("4242; shutdown /r")


# ------------------------------------------------------ end-to-end routing --


def test_recovery_refuses_and_explains_when_the_service_is_healthy(mod, monkeypatch):
    monkeypatch.setattr(
        mod, "query_wsl_service",
        lambda: (mod.ServiceStatus(state="RUNNING", pid=10), GOOD_BINARY))
    recovered, details = mod.recover_wsl_service()
    assert not recovered
    # The issue asks for the original diagnosis *and* each guarded decision in
    # the output, so a refusal has to be legible, not silent.
    assert any("state=RUNNING" in d for d in details)
    assert any("STOP_PENDING" in d for d in details)


def test_a_pid_that_changes_between_diagnosis_and_kill_aborts(mod, monkeypatch):
    """PID reuse/race, from the acceptance list. The diagnosis and the kill are
    separated by a UAC prompt the operator may sit on, which is exactly the
    window in which a PID can be recycled onto something else."""
    seen = {"n": 0}

    def flapping():
        seen["n"] += 1
        pid = 4242 if seen["n"] == 1 else 9999
        return mod.ServiceStatus(state="STOP_PENDING", pid=pid), GOOD_BINARY

    monkeypatch.setattr(mod, "query_wsl_service", flapping)
    killed = []
    monkeypatch.setattr(
        mod, "_elevated_wsl_service_restart",
        lambda pid: (killed.append(pid), (True, "should not happen"))[1])

    recovered, details = mod.recover_wsl_service()
    assert not recovered
    assert killed == [], "must not terminate after the identity moved"
    assert any("identity changed" in d for d in details)


def test_elevation_denial_is_reported_and_leaves_wsl_untouched(mod, monkeypatch):
    monkeypatch.setattr(
        mod, "query_wsl_service",
        lambda: (mod.ServiceStatus(state="STOP_PENDING", pid=4242), GOOD_BINARY))
    monkeypatch.setattr(
        mod, "_elevated_wsl_service_restart",
        lambda pid: (False, "elevation was denied or the elevated command failed"))

    recovered, details = mod.recover_wsl_service()
    assert not recovered
    assert any("denied" in d for d in details)


def test_a_successful_restart_reports_running(mod, monkeypatch):
    monkeypatch.setattr(
        mod, "query_wsl_service",
        lambda: (mod.ServiceStatus(state="STOP_PENDING", pid=4242), GOOD_BINARY))
    monkeypatch.setattr(
        mod, "_elevated_wsl_service_restart", lambda pid: (True, "elevated recovery ran"))
    monkeypatch.setattr(mod, "_wait_for_wsl_service_running", lambda **_: True)

    recovered, details = mod.recover_wsl_service()
    assert recovered
    assert any("RUNNING" in d for d in details)


def test_a_service_that_never_reaches_running_is_not_reported_as_recovered(
    mod, monkeypatch
):
    monkeypatch.setattr(
        mod, "query_wsl_service",
        lambda: (mod.ServiceStatus(state="STOP_PENDING", pid=4242), GOOD_BINARY))
    monkeypatch.setattr(
        mod, "_elevated_wsl_service_restart", lambda pid: (True, "elevated recovery ran"))
    monkeypatch.setattr(mod, "_wait_for_wsl_service_running", lambda **_: False)

    recovered, details = mod.recover_wsl_service()
    assert not recovered
    assert any("did not reach RUNNING" in d for d in details)
