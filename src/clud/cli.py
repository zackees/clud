"""Minimal CLI entry point for clud - routes to agent module."""

import contextlib
import os
import sys
from types import TracebackType

from .agent_cli import main as agent_main


def _silent_keyboard_interrupt_hook(
    exc_type: type[BaseException] | None,
    exc_value: BaseException | None,
    exc_traceback: TracebackType | None,
) -> None:
    """Custom exception hook that silences KeyboardInterrupt stack traces."""
    if exc_type is KeyboardInterrupt:
        # Print a clean message instead of the full stack trace
        print("\nCtrl-c pressed, exiting...", file=sys.stderr)
        sys.exit(130)  # Standard exit code for SIGINT
    elif exc_type is not None and exc_value is not None:
        # For all other exceptions, use the default behavior
        sys.__excepthook__(exc_type, exc_value, exc_traceback)


def main(args: list[str] | None = None) -> int:
    """Main entry point - delegate to agent."""
    # Install custom exception hook to silence KeyboardInterrupt stack traces
    sys.excepthook = _silent_keyboard_interrupt_hook

    # On Windows, re-exec as 'python -m clud' to unlock the .exe file
    # This allows the package to be upgraded while clud is running
    if sys.platform == "win32" and not os.environ.get("CLUD_REEXEC_DONE"):
        # Check if we're running as the installed .exe entry point
        argv0_lower = sys.argv[0].lower()
        if argv0_lower.endswith(("clud.exe", "clud-script.py", "clud-script.pyw")):
            # Set flag to prevent infinite re-exec loop
            os.environ["CLUD_REEXEC_DONE"] = "1"

            # Re-execute as python -m clud with all original arguments
            python = sys.executable
            new_args = [python, "-m", "clud"] + sys.argv[1:]

            # execv replaces the current process, unlocking clud.exe
            # This never returns on success
            # If execv fails, suppress the error and continue with normal execution
            with contextlib.suppress(OSError):
                os.execv(python, new_args)

    return agent_main(args)


if __name__ == "__main__":
    sys.exit(main())
