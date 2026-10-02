"""The `clud` package builds exactly two test harnesses (#1726).

`cargo test --workspace --no-run` builds one harness per test-enabled target,
and each harness in the `clud` package statically links the whole workspace.
zccache never caches harness link products (zackees/zccache#1525), so every
harness is a full, uncached compile and link on every CI run, hosted or
local. The package therefore builds two: the lib's unit tests and the single
`integration` harness (`crates/clud-bin/tests/integration/main.rs`).

A new integration test is a module under `tests/integration/<category>/`,
never a new `tests/*.rs` file or `tests/<dir>/main.rs` target. A new bin
keeps its logic and tests in the lib and declares `test = false`.

`ci/banned_empty_harnesses.py` (#1714) is the companion rule: no harness
without tests. Run via `bash lint` (see `ci/lint.py`).
"""

from __future__ import annotations

import sys
from pathlib import Path

import tomllib

from ci.banned_empty_harnesses import ROOT, Target, package_targets, workspace_manifests

PACKAGE = "clud"
#: (kind, name) of every harness the `clud` package may build.
ALLOWED = frozenset({("lib", "clud"), ("test", "integration")})

REASON = (
    "the `clud` package builds only its lib unit tests and the one `integration` "
    "harness (#1726: each harness links the whole workspace, uncached, on every "
    "CI run). Put an integration test in a module under "
    "crates/clud-bin/tests/integration/<category>/; give a bin `test = false` "
    "and move its tests into the lib."
)


def _package_name(manifest: Path) -> str:
    return tomllib.loads(manifest.read_text(encoding="utf-8"))["package"]["name"]


def extra_harnesses(root: Path = ROOT) -> list[Target]:
    """Harnesses of the `clud` package beyond the allowed two."""
    return [
        target
        for manifest in workspace_manifests(root)
        if _package_name(manifest) == PACKAGE
        for target in package_targets(manifest)
        if (target.kind, target.name) not in ALLOWED
    ]


def main() -> int:
    failures = extra_harnesses()
    for target in failures:
        where = target.manifest.relative_to(ROOT).as_posix()
        print(f"{where}: {target.kind} `{target.name}`: BANNED -- {REASON}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
