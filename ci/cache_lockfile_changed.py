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

The base is the push event's ``before`` SHA, so a multi-commit push covers
all incoming commits. Missing event data or an unavailable shallow-checkout
base conservatively requires pruning rather than trusting the last commit.
"""

from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path

from ci.process import run

ROOT = Path(__file__).resolve().parent.parent
WATCHED = ("Cargo.lock", "uv.lock", "rust-toolchain.toml")


def push_base() -> str | None:
    event_path = os.environ.get("GITHUB_EVENT_PATH")
    if not event_path:
        return None
    try:
        event = json.loads(Path(event_path).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    if not isinstance(event, dict):
        return None
    before = event.get("before")
    if not isinstance(before, str) or not re.fullmatch(r"[0-9a-f]{40}", before):
        return None
    return before if before != "0" * 40 else None


def lockfiles_changed() -> bool:
    base = push_base()
    if base is None:
        print("push base unavailable; conservatively requiring pre-pruning", file=sys.stderr)
        return True
    proc = run(
        ["git", "diff", "--name-only", base, "HEAD", "--", *WATCHED],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        print(
            f"lockfile comparison failed (rc={proc.returncode}): {proc.stderr.strip()}; "
            "conservatively requiring pre-pruning",
            file=sys.stderr,
        )
        return True
    return bool(proc.stdout.strip())


def main() -> int:
    changed = lockfiles_changed()
    print(f"lockfile-changed: {changed} (watched: {', '.join(WATCHED)})")
    gh_out = os.environ.get("GITHUB_OUTPUT")
    if gh_out:
        with open(gh_out, "a", encoding="utf-8") as handle:
            handle.write(f"changed={'true' if changed else 'false'}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
