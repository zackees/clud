"""Lint and test execution utilities.

This module provides functions for running lint-test commands and
checking for agent artifacts.
"""

import shutil
import subprocess
import sys
from pathlib import Path


def _find_and_run_lint_test() -> tuple[int, str]:
    """Find lint-test command using shutil.which and run it with output capture.

    Returns:
        Tuple of (returncode, output)

    Raises:
        FileNotFoundError: If lint-test command is not found in PATH

    Note:
        This function uses subprocess.run() with PIPE to capture output.
        While CLAUDE.md recommends RunningProcess.run_streaming() for long-running
        processes, we need to capture the full output here for ERROR.log and
        validation purposes. The output buffer (typically 64KB) should be sufficient
        for most lint-test runs, but very large outputs could potentially stall.
        Future improvement: Use streaming with a custom callback that accumulates output.
    """
    # Use shutil.which to find lint-test in PATH
    lint_test_path = shutil.which("lint-test")

    if lint_test_path is None:
        raise FileNotFoundError("lint-test command not found in PATH. Please ensure lint-test is installed and available in your PATH.")

    # Run lint-test with output capture
    # Note: We capture output here because we need to:
    # 1. Display it to the user
    # 2. Save it to ERROR.log file
    # 3. Check the return code
    result = subprocess.run(
        [lint_test_path],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        encoding="utf-8",
        errors="replace",  # Replace undecodable bytes with � instead of raising exception
        check=False,
    )

    return result.returncode, result.stdout


def _check_agent_artifacts() -> bool:
    """Check for agent task artifacts before running clud up.

    Checks for DONE.md, LOOP.md, and .loop/ directory.
    Prompts user to delete or abort if found.

    Returns:
        True if should continue, False if should abort
    """
    artifacts = ["DONE.md", "LOOP.md", ".loop"]
    found_artifacts = [name for name in artifacts if Path(name).exists()]

    if not found_artifacts:
        return True

    # Display warning
    print("\n⚠️  Agent task artifacts detected:", file=sys.stderr)
    for artifact in found_artifacts:
        print(f"  - {artifact}", file=sys.stderr)

    print("\nThese files are from a previous agent loop run.", file=sys.stderr)
    print("They may interfere with the current run.", file=sys.stderr)
    print(file=sys.stderr)
    sys.stdout.flush()

    try:
        response = input("Delete artifacts and continue? [y/n]: ").strip().lower()
    except (EOFError, KeyboardInterrupt):
        print("\nOperation cancelled.", file=sys.stderr)
        return False

    if response in ["y", "yes"]:
        # Delete artifacts
        for artifact in found_artifacts:
            artifact_path = Path(artifact)
            try:
                if artifact_path.is_dir():
                    shutil.rmtree(artifact_path)
                else:
                    artifact_path.unlink()
                print(f"✓ Deleted {artifact}", file=sys.stderr)
            except Exception as e:
                print(f"Error: Failed to delete {artifact}: {e}", file=sys.stderr)
                return False
        return True

    elif response in ["n", "no"]:
        print("Operation cancelled. Please clean up artifacts manually.", file=sys.stderr)
        return False

    else:
        print("Invalid response. Operation cancelled.", file=sys.stderr)
        return False
