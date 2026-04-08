"""Process launcher with Ctrl-C isolation for loop mode.

Launches Claude subprocess in a new process group so that Ctrl-C only
reaches the parent (clud). The parent then explicitly kills the child
process tree on interrupt. An atexit handler guarantees cleanup of any
lingering child processes.

On MSYS/mintty (Windows git-bash), SIGINT is never delivered to native
Windows Python because mintty lacks a Windows Console. In that environment
we skip CREATE_NEW_PROCESS_GROUP so the child stays in the same process
group and receives SIGINT directly from the MSYS PTY driver. We also
monitor os.getppid() to detect when the parent (uv) is killed by SIGINT.
"""

import atexit
import contextlib
import os
import queue
import subprocess
import sys
import threading
from collections.abc import Callable

from running_process import kill_process_tree

from ..util import handle_keyboard_interrupt

# Module-level tracking of the active subprocess for atexit cleanup.
_active_process: subprocess.Popen[bytes] | None = None
_active_process_lock = threading.Lock()


def _cleanup_active_process() -> None:
    """Kill any active child process on interpreter exit."""
    with _active_process_lock:
        proc = _active_process
    if proc is not None and proc.poll() is None:
        with contextlib.suppress(Exception):
            kill_process_tree(proc.pid)


atexit.register(_cleanup_active_process)


def _reader_thread(proc: subprocess.Popen[bytes], q: queue.Queue[str | None]) -> None:
    """Read stdout lines from the subprocess and push them to the queue.

    Runs as a daemon thread. Sends None as a sentinel when EOF is reached.
    """
    assert proc.stdout is not None
    try:
        for raw_line in proc.stdout:
            try:
                line = raw_line.decode("utf-8", errors="replace")
            except Exception:
                line = repr(raw_line)
            q.put(line)
    except Exception:
        pass
    finally:
        q.put(None)  # sentinel


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


def run_claude_process(
    cmd: list[str],
    stdout_callback: Callable[[str], None] | None = None,
    use_shell: bool = False,
    propagate_keyboard_interrupt: bool = True,
) -> int:
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

    Returns:
        The child process exit code.
    """
    global _active_process

    # Record parent PID so we can detect if it dies (SIGINT killed uv).
    original_ppid = os.getppid()

    # Platform-specific process-group configuration.
    popen_kwargs: dict[str, object] = {}
    if sys.platform == "win32":
        if not _is_msys_environment():
            # Native Windows console: isolate child so Ctrl-C doesn't propagate.
            CREATE_NEW_PROCESS_GROUP = 0x00000200
            popen_kwargs["creationflags"] = CREATE_NEW_PROCESS_GROUP
        # On MSYS/mintty: do NOT set CREATE_NEW_PROCESS_GROUP.
        # SIGINT is delivered via POSIX signals, not GenerateConsoleCtrlEvent.
        # Keeping the child in the same group lets MSYS deliver SIGINT to it
        # directly, and _is_interrupt_exit_code() detects the resulting exit code.
    else:
        popen_kwargs["start_new_session"] = True

    if use_shell:
        shell_cmd = subprocess.list2cmdline(cmd)
        popen_kwargs["shell"] = True
        cmd_arg: str | list[str] = shell_cmd
    else:
        cmd_arg = cmd

    capture = stdout_callback is not None

    proc = subprocess.Popen(
        cmd_arg,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.STDOUT if capture else None,
        stdin=subprocess.DEVNULL if capture else None,
        **popen_kwargs,  # type: ignore[arg-type]
    )

    # Register as active process for atexit cleanup.
    with _active_process_lock:
        _active_process = proc

    try:
        if capture:
            assert proc.stdout is not None
            q: queue.Queue[str | None] = queue.Queue()
            t = threading.Thread(target=_reader_thread, args=(proc, q), daemon=True)
            t.start()

            # Poll the queue with a short timeout to stay responsive to Ctrl-C.
            eof = False
            while not eof or proc.poll() is None:
                # Detect parent death (e.g. uv killed by SIGINT on MSYS).
                if os.getppid() != original_ppid:
                    if proc.poll() is None:
                        with contextlib.suppress(Exception):
                            kill_process_tree(proc.pid)
                        with contextlib.suppress(subprocess.TimeoutExpired):
                            proc.wait(timeout=2)
                    raise KeyboardInterrupt

                try:
                    line = q.get(timeout=0.1)
                except queue.Empty:
                    # No data yet — check if process is still alive.
                    if proc.poll() is not None:
                        # Drain remaining items.
                        while True:
                            try:
                                line = q.get_nowait()
                            except queue.Empty:
                                break
                            if line is None:
                                break
                            stdout_callback(line)
                        break
                    continue

                if line is None:
                    eof = True
                    continue
                stdout_callback(line)

            # Wait for process to finish (should be fast since EOF was reached).
            proc.wait()
        else:
            # Interactive mode: poll in 0.1 s increments.
            while proc.poll() is None:
                # Detect parent death (e.g. uv killed by SIGINT on MSYS).
                if os.getppid() != original_ppid:
                    if proc.poll() is None:
                        with contextlib.suppress(Exception):
                            kill_process_tree(proc.pid)
                        with contextlib.suppress(subprocess.TimeoutExpired):
                            proc.wait(timeout=2)
                    raise KeyboardInterrupt

                try:
                    proc.wait(timeout=0.1)
                except subprocess.TimeoutExpired:
                    continue

        return proc.returncode

    except KeyboardInterrupt as e:

        def _cleanup() -> None:
            if proc.poll() is None:
                with contextlib.suppress(Exception):
                    kill_process_tree(proc.pid)
                with contextlib.suppress(subprocess.TimeoutExpired):
                    proc.wait(timeout=2)

        handle_keyboard_interrupt(
            e,
            cleanup=_cleanup,
            reraise_on_main_thread=propagate_keyboard_interrupt,
        )
        return proc.returncode or 130  # Worker thread: suppressed

    finally:
        with _active_process_lock:
            if _active_process is proc:
                _active_process = None
