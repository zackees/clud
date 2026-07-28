"""Single source of truth for the CI target matrix.

Emits the `build` and `test` matrices consumed by `.github/workflows/ci.yml`.
Keeping this in Python rather than YAML means the triple -> {build host, cross
strategy, exec runner} mapping exists exactly once, and is unit-testable.

Design: docs/architecture/ci.md
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from dataclasses import dataclass
from typing import Literal

Strategy = Literal["native", "zigbuild", "xwin"]
Tier = Literal["core", "full"]

FULL_TIER_LABEL = "ci:full"


@dataclass(frozen=True)
class Target:
    """One shippable triple and how CI produces + exercises it."""

    triple: str
    #: Runner that executes the compiled artifacts. Always native.
    exec_runs_on: str
    #: Cross-compile strategy used when building on Linux.
    strategy: Strategy
    #: `core` targets run on every PR push; `full` adds the second architecture
    #: of each OS, which is an ABI/codegen check rather than a behaviour check.
    tier: Tier
    #: Artifact name the release pipeline publishes this triple's wheel under.
    artifact: str
    #: Runner that compiles. Overridden to `exec_runs_on` for darwin when no
    #: macOS SDK is available to cross-compile against.
    build_runs_on: str = "ubuntu-24.04"


TARGETS: tuple[Target, ...] = (
    Target("x86_64-unknown-linux-gnu", "ubuntu-24.04", "native", "core", "wheels-linux-x86"),
    Target("x86_64-pc-windows-msvc", "windows-2025", "xwin", "core", "wheels-windows-x86"),
    Target("aarch64-apple-darwin", "macos-15", "zigbuild", "core", "wheels-macos-arm"),
    Target("aarch64-unknown-linux-gnu", "ubuntu-24.04-arm", "zigbuild", "full", "wheels-linux-arm"),
    # whisper-rs is cfg-excluded on this triple (crates/clud-bin/Cargo.toml:149),
    # so there is no C++/CMake to cross at all -- the cheapest cross in the set.
    Target("aarch64-pc-windows-msvc", "windows-11-arm", "xwin", "full", "wheels-windows-arm"),
    Target("x86_64-apple-darwin", "macos-15-intel", "zigbuild", "full", "wheels-macos-x86"),
)

#: The sdist is source-only, so exactly one target must produce it.
SDIST_TARGET = "x86_64-unknown-linux-gnu"

SUITES: tuple[str, ...] = ("unit", "integration")


def resolve_tier(event_name: str, dispatch_tier: str, pr_labels: str) -> Tier:
    """Pick the target tier from the triggering event.

    `main` pushes and merge-queue runs always take the full tier: that is what
    keeps every triple's build cache warm for the PR jobs that restore from it.
    """
    if event_name == "workflow_dispatch":
        return "full" if dispatch_tier != "core" else "core"
    if event_name in ("push", "merge_group"):
        return "full"
    labels = {label.strip() for label in pr_labels.split(",") if label.strip()}
    return "full" if FULL_TIER_LABEL in labels else "core"


def selected(tier: Tier) -> list[Target]:
    if tier == "full":
        return list(TARGETS)
    return [target for target in TARGETS if target.tier == "core"]


def build_matrix(targets: list[Target], *, macos_sdk: bool) -> dict[str, list[dict[str, str]]]:
    """Build-side matrix: one entry per triple.

    Without a macOS SDK on the Linux runner we cannot link `-framework
    Accelerate`, which `vendor/whisper-rs-sys/build.rs:27-28` emits
    unconditionally for apple targets with no feature to disable it. Fall back
    to a native macOS builder rather than failing -- the build-once/run-anywhere
    split still removes the other three macOS jobs.
    """
    include: list[dict[str, str]] = []
    for target in targets:
        cross_darwin = "apple" in target.triple and not macos_sdk
        include.append(
            {
                "target": target.triple,
                "strategy": "native" if cross_darwin else target.strategy,
                "runs-on": target.exec_runs_on if cross_darwin else target.build_runs_on,
            }
        )
    return {"include": include}


def exec_matrix(targets: list[Target]) -> dict[str, list[dict[str, str]]]:
    """Exec-side matrix: triple x suite, always on a native runner.

    Splitting unit and integration into separate jobs costs one extra bundle
    download (seconds) and halves the critical path on the slowest platform.
    """
    return {
        "include": [
            {"target": target.triple, "runs-on": target.exec_runs_on, "suite": suite}
            for target in targets
            for suite in SUITES
        ]
    }


def release_matrix(*, macos_sdk: bool) -> dict[str, list[dict[str, object]]]:
    """Release-side matrix: all six triples, wheel artifacts, no test bundle.

    Deliberately derived from the same TARGETS table -- and therefore the same
    cross-compile strategy -- as CI. If release shipped natively-built wheels
    while CI only ever exercised cross-built binaries, CI would not be testing
    the artifact that ships.
    """
    base = build_matrix(list(TARGETS), macos_sdk=macos_sdk)["include"]
    by_triple = {target.triple: target for target in TARGETS}
    return {
        "include": [
            {
                **entry,
                "artifact": by_triple[str(entry["target"])].artifact,
                "include-sdist": entry["target"] == SDIST_TARGET,
            }
            for entry in base
        ]
    }


def emit(outputs: dict[str, str]) -> None:
    path = os.environ.get("GITHUB_OUTPUT")
    lines = [f"{key}={value}" for key, value in outputs.items()]
    if path:
        with open(path, "a", encoding="utf-8") as handle:
            handle.write("\n".join(lines) + "\n")
    for line in lines:
        print(line)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event-name", default=os.environ.get("EVENT_NAME", ""))
    parser.add_argument("--dispatch-tier", default=os.environ.get("DISPATCH_TIER", ""))
    parser.add_argument("--pr-labels", default=os.environ.get("PR_LABELS", ""))
    parser.add_argument(
        "--release",
        action="store_true",
        help="Emit the release matrix (all triples, wheel artifacts, no test bundle).",
    )
    args = parser.parse_args(argv)

    macos_sdk = bool(os.environ.get("MACOS_SDK_URL", "").strip())

    if args.release:
        emit({"build": json.dumps(release_matrix(macos_sdk=macos_sdk), separators=(",", ":"))})
        return 0

    tier = resolve_tier(args.event_name, args.dispatch_tier, args.pr_labels)
    targets = selected(tier)
    emit(
        {
            "tier": tier,
            "build": json.dumps(build_matrix(targets, macos_sdk=macos_sdk), separators=(",", ":")),
            "test": json.dumps(exec_matrix(targets), separators=(",", ":")),
        }
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
