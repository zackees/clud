"""Minimal CLI entry point for clud - routes to agent module."""

import contextlib
import os
import sys

from .agent_cli import main as agent_main


def main(args: list[str] | None = None) -> int:
    """Main entry point - delegate to agent."""
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
