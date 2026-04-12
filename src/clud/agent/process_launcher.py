"""Process launcher with Ctrl-C isolation for loop mode.

Launches Claude subprocess in a new process group so that Ctrl-C only
reaches the parent (clud). The parent then explicitly kills the child
process tree on interrupt. All launched processes are centrally tracked
by the RunningProcessManager singleton for automatic zombie cleanup.

On MSYS/mintty (Windows git-bash), SIGINT is never delivered to native
Windows Python because mintty lacks a Windows Console. In that environment
we skip process group isolation so the child stays in the same process
group and receives SIGINT directly from the MSYS PTY driver. We also
monitor os.getppid() to detect when the parent (uv) is killed by SIGINT.
"""

import contextlib
import os
import shutil
import subprocess
import sys
import time
import traceback
from collections.abc import Callable
from dataclasses import dataclass

from running_process import (
    Idle,
    IdleDetection,
    IdleStartTrigger,
    IdleTiming,
    InteractiveMode,
    PseudoTerminalProcess,
    PtyIdleDetection,
    RunningProcess,
    WaitCallbackResult,
    kill_process_tree,
)

from ..util import handle_keyboard_interrupt


@dataclass
class PtySessionResult:
    """Result from a PTY session with idle detection."""

    returncode: int
    idle_detected: bool = False
    idle_event_count: int = 0


def _is_msys_environment() -> bool:
    """Detect if running under MSYS/git-bash (mintty terminal).

    On MSYS/mintty, SIGINT is delivered via POSIX signals through the cygwin
    DLL.  Native Windows Python cannot receive these signals because it relies
    on SetConsoleCtrlHandler which requires a real Windows Console — mintty
    communicates via pipes instead.

    Note: We only check MSYSTEM (set by MSYS2/git-bash shell) and not
    MSYS_NO_PATHCONV, because clud sets MSYS_NO_PATHCONV=1 itself to prevent
    path conversion — checking it here would always return True and defeat
    CREATE_NEW_PROCESS_GROUP isolation.

    Returns:
        True if the current process is running under MSYS/git-bash.
    """
    return bool(os.environ.get("MSYSTEM"))


def _run_with_idle_timeout(
    cmd: list[str],
    idle_timeout: float,
    on_idle: Callable[[], str | None] | None,
    propagate_keyboard_interrupt: bool,
) -> PtySessionResult:
    """Run a command in a PTY with idle detection.

    Creates a ``PseudoTerminalProcess`` that relays terminal I/O and arms an
    idle detector.  When the process goes idle the optional *on_idle* callback
    is invoked; if it returns text the text is injected as a follow-up turn.

    Returns a ``PtySessionResult`` with returncode and idle event metadata.
    """
    # Forward the host terminal size so the child TUI renders correctly.
    term_size = shutil.get_terminal_size()
    pty_proc = PseudoTerminalProcess(
        cmd,
        capture=False,
        relay_terminal_input=True,
        arm_idle_timeout_on_submit=True,
        rows=term_size.lines,
        cols=term_size.columns,
    )

    idle_event_count = 0
    idle_detected = False

    def _idle_callback(_event: object, input_buffer: object) -> WaitCallbackResult:
        nonlocal idle_event_count, idle_detected
        idle_event_count += 1
        idle_detected = True
        if on_idle is not None:
            follow_up = on_idle()
            if follow_up is not None:
                input_buffer.write(follow_up + "\r")  # type: ignore[union-attr]
                return WaitCallbackResult.CONTINUE
        return WaitCallbackResult.CONTINUE_AND_DISARM

    pty_idle = PtyIdleDetection(start_trigger=IdleStartTrigger.IMMEDIATE)
    timing = IdleTiming(timeout_seconds=idle_timeout)
    detection = IdleDetection(timing=timing, pty=pty_idle)
    idle_condition = Idle(detector=detection, on_callback=_idle_callback)

    try:
        wait_result = pty_proc.wait_for(idle_condition, echo_output=True)
        pty_proc.idle_timeout_enabled = False  # type: ignore[attr-defined]
        returncode = wait_result.returncode or 0
        return PtySessionResult(
            returncode=returncode,
            idle_detected=idle_detected,
            idle_event_count=idle_event_count,
        )
    except KeyboardInterrupt as e:
        pty_proc.interrupt_and_wait()
        returncode = pty_proc.poll() or 130
        if propagate_keyboard_interrupt:
            handle_keyboard_interrupt(e, reraise_on_main_thread=True)
        return PtySessionResult(returncode=returncode)


def _run_with_child_ctrl_c(
    cmd: list[str],
    propagate_keyboard_interrupt: bool,
) -> int:
    """Run a command using running-process console isolation.

    Uses ``RunningProcess.interactive`` with ``CONSOLE_ISOLATED`` mode so the
    child receives its own Ctrl-C independently.
    """
    proc = RunningProcess.interactive(cmd, mode=InteractiveMode.CONSOLE_ISOLATED)

    try:
        exit_code = proc.poll()
        while exit_code is None:
            exit_code = proc.poll()
        return exit_code
    except KeyboardInterrupt as e:
        with contextlib.suppress(Exception):
            proc.send_interrupt()
        with contextlib.suppress(Exception):
            proc.terminate()
        with contextlib.suppress(Exception):
            proc.kill()
        while proc.poll() is None:
            pass
        handle_keyboard_interrupt(e, reraise_on_main_thread=propagate_keyboard_interrupt)
        return proc.poll() or 130


def run_claude_process(
    cmd: list[str],
    stdout_callback: Callable[[str], None] | None = None,
    use_shell: bool = False,
    propagate_keyboard_interrupt: bool = True,
    idle_timeout: float | None = None,
    on_idle: Callable[[], str | None] | None = None,
    allow_child_ctrl_c: bool = False,
    debug_keyboard_interrupt: bool = False,
) -> int | PtySessionResult:
    """Launch Claude in an isolated process group and wait for completion.

    When *stdout_callback* is provided, stdout is captured line-by-line in a
    background thread and each line is passed to the callback. The main loop
    polls the queue with a 0.1 s timeout so that ``KeyboardInterrupt`` from
    the parent's Ctrl-C is handled promptly.

    When *stdout_callback* is ``None`` (interactive / message mode), stdout and
    stderr are inherited (passed through to the terminal) but the child still
    runs in its own process group.

    On ``KeyboardInterrupt`` the child process tree is killed before the
    exception is re-raised.

    On MSYS/mintty the child is kept in the *same* process group so that the
    MSYS PTY driver can deliver SIGINT to it directly.  The parent also
    monitors ``os.getppid()`` to detect when its own parent (typically ``uv``)
    is killed by SIGINT.

    Args:
        cmd: Command list to execute.
        stdout_callback: Optional callback receiving each stdout line.
        use_shell: Whether to run via the shell.
        propagate_keyboard_interrupt: Whether to re-raise ``KeyboardInterrupt``
            on the main thread after child cleanup. Interactive top-level
            launches should pass ``False`` to exit cleanly with code 130.
        idle_timeout: If set, use PTY-based idle detection instead of plain
            subprocess. Returns ``PtySessionResult`` when used.
        on_idle: Optional callback invoked when idle is detected. If it
            returns a string, that text is injected as a follow-up turn.
        allow_child_ctrl_c: Use running-process console isolation so the
            child can receive Ctrl-C independently.
        debug_keyboard_interrupt: Print debug info when Ctrl-C is caught.

    Returns:
        The child process exit code, or ``PtySessionResult`` when
        *idle_timeout* is used.
    """
    # Dispatch to PTY idle detection path.
    if idle_timeout is not None:
        return _run_with_idle_timeout(cmd, idle_timeout, on_idle, propagate_keyboard_interrupt)

    # Dispatch to running-process console isolation path.
    if allow_child_ctrl_c and sys.platform == "win32":
        return _run_with_child_ctrl_c(cmd, propagate_keyboard_interrupt)

    # Record parent PID so we can detect if it dies (SIGINT killed uv).
    original_ppid = os.getppid()

    capture = stdout_callback is not None

    # On MSYS/mintty, keep child in the same process group so the MSYS
    # PTY driver can deliver SIGINT directly. Everywhere else, isolate.
    msys = sys.platform == "win32" and _is_msys_environment()

    if capture:
        # Captured mode: RunningProcess drains stdout via internal threads;
        # we poll drain_stdout() with a short sleep to stay responsive to
        # Ctrl-C and to monitor parent PID.
        cmd_arg: str | list[str] = subprocess.list2cmdline(cmd) if use_shell else cmd
        proc = RunningProcess(
            command=cmd_arg,
            capture=True,
            stdin=subprocess.DEVNULL,
            stderr=subprocess.STDOUT,
            shell=use_shell,
            allows_child_ctrl_c_interruption=msys,
            auto_run=True,
        )

        try:
            while True:
                # Detect parent death (e.g. uv killed by SIGINT on MSYS).
                if os.getppid() != original_ppid:
                    if proc.poll() is None:
                        with contextlib.suppress(Exception):
                            kill_process_tree(proc.pid)  # type: ignore[arg-type]
                    raise KeyboardInterrupt

                # Drain stdout lines and pass to callback.
                for line_value in proc.drain_stdout():
                    line = line_value.decode("utf-8", errors="replace") if isinstance(line_value, bytes) else line_value
                    stdout_callback(line)

                code = proc.poll()
                if code is not None:
                    # Final drain after process exit.
                    for line_value in proc.drain_stdout():
                        line = line_value.decode("utf-8", errors="replace") if isinstance(line_value, bytes) else line_value
                        stdout_callback(line)
                    return code

                time.sleep(0.01)

        except KeyboardInterrupt as e:
            if debug_keyboard_interrupt:
                print("DEBUG: Ctrl-C caught by process launcher", file=sys.stderr)
                traceback.print_stack(file=sys.stderr)

            def _cleanup_captured() -> None:
                if proc.poll() is None:
                    with contextlib.suppress(Exception):
                        kill_process_tree(proc.pid)  # type: ignore[arg-type]

            handle_keyboard_interrupt(
                e,
                cleanup=_cleanup_captured,
                reraise_on_main_thread=propagate_keyboard_interrupt,
            )
            return proc.poll() or 130  # Worker thread: suppressed

    else:
        # Interactive mode: use RunningProcess.interactive() for console
        # isolation. CONSOLE_SHARED on MSYS lets the PTY driver deliver
        # SIGINT; CONSOLE_ISOLATED everywhere else prevents Ctrl-C
        # propagation to the child.
        mode = InteractiveMode.CONSOLE_SHARED if msys else InteractiveMode.CONSOLE_ISOLATED
        iproc = RunningProcess.interactive(
            cmd,
            mode=mode,
            shell=use_shell if use_shell else None,
        )

        try:
            while iproc.poll() is None:
                # Detect parent death (e.g. uv killed by SIGINT on MSYS).
                if os.getppid() != original_ppid:
                    if iproc.poll() is None and iproc.pid is not None:
                        with contextlib.suppress(Exception):
                            kill_process_tree(iproc.pid)
                    raise KeyboardInterrupt
                time.sleep(0.1)

            return iproc.poll() or 0

        except KeyboardInterrupt as e:
            if debug_keyboard_interrupt:
                print("DEBUG: Ctrl-C caught by process launcher", file=sys.stderr)
                traceback.print_stack(file=sys.stderr)

            def _cleanup_interactive() -> None:
                with contextlib.suppress(Exception):
                    iproc.send_interrupt()
                with contextlib.suppress(Exception):
                    iproc.terminate()
                with contextlib.suppress(Exception):
                    iproc.kill()
                while iproc.poll() is None:
                    pass

            handle_keyboard_interrupt(
                e,
                cleanup=_cleanup_interactive,
                reraise_on_main_thread=propagate_keyboard_interrupt,
            )
            return iproc.poll() or 130  # Worker thread: suppressed
