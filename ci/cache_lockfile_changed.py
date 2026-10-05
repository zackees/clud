"""Did a lockfile or toolchain file change on this push?

zackees/ci.yml CACHE-004: ``[flow.main]`` declares ``pre-prune = true``, which
is what waives the lockfile-change peak from the worst-case cache arithmetic.
That waiver is only honest if something actually pre-prunes, so ci.yml's
``cache-maint`` job runs ``ci-lint cache preprune --lockfile-changed`` before
the two default-branch cache writers.

Pre-pruning spends live deletes, so it must only run on a push that really
changed a lockfile. ci.toml's ``lockfile = true`` families are ``compile``
(``Cargo.lock``) and ``deps`` (``uv.lock``); ``rust-toolchain.toml`` is watched
too because a toolchain bump changes what every cached target contains even
when no lockfile line moved.

The parent is deliberately ``HEAD^`` on whatever ref this job checked out,
which is why ``cache-maint`` checks out with ``fetch-depth: 2``. This lives in
``ci/`` rather than as an inline ``run:`` block because GEN-005 caps the shell
budget of a workflow step at a single command.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WATCHED = ("Cargo.lock", "uv.lock", "rust-toolchain.toml")


def main() -> int:
    proc = subprocess.run(
        ["git", "diff", "--name-only", "HEAD^", "HEAD", "--", *WATCHED],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        # No parent commit (a shallow or first commit) or another git error.
        # Never red the run over a probe: treat conservatively as "changed" so
        # the forecast runs rather than silently skipping the prune.
        print(
            f"ci/cache_lockfile_changed.py: git diff failed (rc={proc.returncode}): "
            f"{proc.stderr.strip()} -- treating as changed (conservative)",
            file=sys.stderr,
        )
        changed = True
    else:
        changed = bool(proc.stdout.strip())

    print(f"lockfile-changed: {changed} (watched: {', '.join(WATCHED)})")
    gh_out = os.environ.get("GITHUB_OUTPUT")
    if gh_out:
        with open(gh_out, "a", encoding="utf-8") as handle:
            handle.write(f"changed={'true' if changed else 'false'}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
