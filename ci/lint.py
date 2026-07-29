"""Lint orchestrator for clud: cargo fmt + clippy + ruff + banned imports."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def run(cmd: list[str]) -> int:
    from ci.env import clean_env

    return subprocess.run(cmd, cwd=ROOT, env=clean_env()).returncode


def _cargo(subcommand: list[str]) -> list[str]:
    """Return the cargo argv for the configured CI environment."""
    from ci.env import cargo_argv, clean_env

    return cargo_argv(subcommand, env=clean_env())


def main(argv: list[str] | None = None) -> int:
    """Run the lint suite.

    `--static-only` skips clippy. CI splits the suite: the platform-independent
    checks (banned imports, cargo fmt, ruff) run once in the `static` job, while
    clippy runs once per target triple inside the build job, where that triple's
    dependency graph is already compiled. Running the whole suite six times, as
    the old per-platform workflows did, meant 6x ruff and 6x cargo fmt for
    checks whose result cannot vary by platform.

    `bash lint` (local, no flags) still runs everything, unchanged.
    """
    argv = list(sys.argv[1:] if argv is None else argv)
    static_only = "--static-only" in argv

    from ci.env import activate

    activate()

    from ci.banned_imports import main as check_banned_imports

    # Ordered cheapest-first so the common failure reds out soonest: ruff is a
    # pure-Python scan (~1s), banned_imports is a source grep, and only then do
    # we pay for a cargo subprocess. The old order put ruff last, behind fmt.
    if run([sys.executable, "-m", "ruff", "check", "src", "tests", "ci"]) != 0:
        return 1
    if check_banned_imports() != 0:
        return 1
    # setup-soldr's cargo shim can otherwise omit repository discovery for
    # `.rustfmt.toml` on some runner/architecture combinations. Pin the
    # checked formatting contract explicitly so local and CI layout agree.
    fmt_args = ["fmt", "--all", "--check", "--", "--config-path", ".rustfmt.toml"]
    if run(_cargo(fmt_args)) != 0:
        return 1

    if static_only:
        return 0
    clippy = ["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]
    return 1 if run(_cargo(clippy)) else 0


if __name__ == "__main__":
    sys.exit(main())
