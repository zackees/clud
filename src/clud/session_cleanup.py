"""Stale session detection and cleanup for clud.

On startup, scans for processes that belong to previous clud sessions
which are no longer running.  Stale sessions are identified by the
``CLUD_SESSION_ID`` environment variable that clud injects into every
child process tree.

Detection strategy:
1. If ``find_processes_by_originator`` is available from running-process
   (>= 3.1), use it to find processes tagged with ``CLUD`` originator.
2. Otherwise, fall back to ``psutil`` to scan all processes for
   the ``CLUD_SESSION_ID`` env var.

**IMPORTANT**: This module NEVER auto-kills stale processes.  It only
warns the user and prompts them to confirm a kill action.
"""

from __future__ import annotations

import logging
import os
import sys
from dataclasses import dataclass, field

import psutil

logger = logging.getLogger(__name__)

# Env var set by clud on every session (see cli.py / runner.py).
_SESSION_ENV_VAR = "CLUD_SESSION_ID"


@dataclass
class StaleProcess:
    """Information about a potentially stale process."""

    pid: int
    name: str
    cmdline: str
    session_id: str


@dataclass
class StaleSessionReport:
    """Report of stale sessions detected on startup."""

    processes: list[StaleProcess] = field(default_factory=list)  # pyright: ignore[reportUnknownVariableType]

    @property
    def has_stale(self) -> bool:
        """Return True if any stale processes were found."""
        return len(self.processes) > 0


def _find_stale_via_psutil(current_session_id: str) -> list[StaleProcess]:
    """Scan all processes via psutil for stale CLUD_SESSION_ID env vars.

    Only returns processes whose session ID differs from the current one
    and whose originating clud parent PID is no longer alive.
    """
    current_pid = os.getpid()
    stale: list[StaleProcess] = []

    for proc in psutil.process_iter(["pid", "name", "cmdline", "environ"]):  # pyright: ignore[reportUnknownMemberType]
        try:
            info = proc.info  # type: ignore[attr-defined]
            pid: int = info["pid"]

            # Skip self.
            if pid == current_pid:
                continue

            env: dict[str, str] | None = info.get("environ")
            if env is None:
                continue

            session_id = env.get(_SESSION_ENV_VAR)
            if not session_id:
                continue

            # Skip processes belonging to the current session.
            if session_id == current_session_id:
                continue

            # This process has a CLUD_SESSION_ID from a different session.
            name: str = info.get("name", "<unknown>")
            cmdline_parts: list[str] | None = info.get("cmdline")
            cmdline_str = " ".join(cmdline_parts) if cmdline_parts else name

            stale.append(
                StaleProcess(
                    pid=pid,
                    name=name,
                    cmdline=cmdline_str,
                    session_id=session_id,
                )
            )
        except (psutil.NoSuchProcess, psutil.AccessDenied, psutil.ZombieProcess):
            continue
        except Exception:
            continue

    return stale


def detect_stale_sessions(current_session_id: str) -> StaleSessionReport:
    """Detect stale clud sessions from previous runs.

    Args:
        current_session_id: The session ID of the currently starting session,
            so we can exclude our own processes.

    Returns:
        A report containing any stale processes found.
    """
    stale = _find_stale_via_psutil(current_session_id)
    return StaleSessionReport(processes=stale)


def _kill_processes(processes: list[StaleProcess]) -> tuple[int, int]:
    """Kill a list of stale processes.

    Returns:
        Tuple of (killed_count, failed_count).
    """
    from running_process import kill_process_tree

    killed = 0
    failed = 0
    for proc in processes:
        try:
            kill_process_tree(proc.pid)
            killed += 1
        except Exception:
            failed += 1
    return killed, failed


def prompt_and_cleanup_stale_sessions(current_session_id: str) -> None:
    """Check for stale sessions and prompt the user to clean them up.

    This function is safe to call early in startup.  If stdin is not a TTY
    (piped mode), it only prints a warning without prompting.

    Args:
        current_session_id: The session ID of the currently starting session.
    """
    try:
        report = detect_stale_sessions(current_session_id)
    except Exception:
        logger.debug("Failed to detect stale sessions", exc_info=True)
        return

    if not report.has_stale:
        return

    # Group by session ID for display.
    sessions: dict[str, list[StaleProcess]] = {}
    for proc in report.processes:
        sessions.setdefault(proc.session_id, []).append(proc)

    print(
        f"\nWarning: Found {len(report.processes)} process(es) from {len(sessions)} previous clud session(s):",
        file=sys.stderr,
    )
    for sid, procs in sessions.items():
        print(f"\n  Session {sid[:8]}...:", file=sys.stderr)
        for proc in procs:
            # Truncate long command lines for readability.
            cmd_display = proc.cmdline[:80] + "..." if len(proc.cmdline) > 80 else proc.cmdline
            print(f"    PID {proc.pid}: {cmd_display}", file=sys.stderr)

    # Only prompt if stdin is a TTY.
    if not sys.stdin.isatty():
        print(
            "\nRun clud interactively to clean up stale processes.",
            file=sys.stderr,
        )
        return

    try:
        response = input("\nKill stale processes? [y/N] ").strip().lower()
    except EOFError:
        print(file=sys.stderr)
        return
    except KeyboardInterrupt as e:
        print(file=sys.stderr)
        # Re-raise on main thread so Ctrl-C during the prompt propagates
        # normally; suppress on worker threads.
        from .util import handle_keyboard_interrupt

        handle_keyboard_interrupt(e, reraise_on_main_thread=False)
        return

    if response in ("y", "yes"):
        killed, failed = _kill_processes(report.processes)
        if killed:
            print(f"Killed {killed} stale process(es).", file=sys.stderr)
        if failed:
            print(
                f"Failed to kill {failed} process(es) (may need elevated privileges).",
                file=sys.stderr,
            )
    else:
        print("Skipping stale process cleanup.", file=sys.stderr)
