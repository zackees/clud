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

# `zigbuild` is gone: the strategy was retired when soldr took ownership of
# the Linux cross (#859); `ci/xbuild.py::cargo_argv` refuses it structurally.
# `native` remains only as xbuild's local-development default — no CI lane
# uses it (#863).
Strategy = Literal["native", "soldr"]
TargetTier = Literal["core", "full"]
Mode = Literal["minimal", "extended", "full"]

FULL_TIER_LABEL = "ci-full"
EXTENDED_TIER_LABEL = "ci-test"
LEGACY_FULL_TIER_LABEL = "ci:full"


@dataclass(frozen=True)
class Target:
    """One shippable triple and how CI produces + exercises it."""

    triple: str
    #: Runner that executes the compiled artifacts. Always native.
    exec_runs_on: str
    #: Cross-compile strategy used when building on Linux.
    strategy: Strategy
    #: `core` is the extended one-per-OS set; `full` adds the second architecture
    #: of each OS. Minimal mode selects Linux x64 alone.
    tier: TargetTier
    #: Artifact name the release pipeline publishes this triple's wheel under.
    artifact: str
    #: Runner that compiles. Every triple cross-builds on Linux via the soldr
    #: blessed path, so this is uniform -- it stays a field so a future target
    #: that genuinely cannot cross has somewhere to say so.
    build_runs_on: str = "ubuntu-24.04"


TARGETS: tuple[Target, ...] = (
    Target("x86_64-unknown-linux-gnu", "ubuntu-24.04", "soldr", "core", "wheels-linux-x86"),
    Target("x86_64-pc-windows-msvc", "windows-2025", "soldr", "core", "wheels-windows-x86"),
    Target("aarch64-apple-darwin", "macos-15", "soldr", "full", "wheels-macos-arm"),
    Target("aarch64-unknown-linux-gnu", "ubuntu-24.04-arm", "soldr", "full", "wheels-linux-arm"),
    # whisper-rs is cfg-excluded on this triple (crates/clud-bin/Cargo.toml:149),
    # so there is no C++/CMake to cross at all -- the cheapest cross in the set.
    Target("aarch64-pc-windows-msvc", "windows-11-arm", "soldr", "full", "wheels-windows-arm"),
    Target("x86_64-apple-darwin", "macos-15-intel", "soldr", "full", "wheels-macos-x86"),
)

#: The sdist is source-only, so exactly one target must produce it.
SDIST_TARGET = "x86_64-unknown-linux-gnu"

SUITES: tuple[str, ...] = ("unit", "integration")


def resolve_tier(
    event_name: str,
    dispatch_tier: str,
    pr_labels: str,
    candidate_sha: str | None = None,
    event_sha: str | None = None,
    provenance_verified: bool = False,
) -> Mode:
    """Pick CI coverage; manual full runs need proven source provenance."""
    if event_name == "workflow_dispatch":
        if dispatch_tier != "full" or not candidate_sha or len(candidate_sha) != 40:
            raise ValueError("full dispatch requires a 40-character candidate SHA")
        if not provenance_verified:
            raise ValueError("full dispatch requires verified source/workflow provenance")
        return "full"
    if event_name == "merge_group":
        return "full"
    if event_name == "push":
        return "minimal"
    if event_name != "pull_request":
        raise ValueError(f"unsupported CI event: {event_name}")
    labels = {label.strip() for label in pr_labels.split(",") if label.strip()}
    unknown = {label for label in labels if label.startswith("ci-")} - {
        FULL_TIER_LABEL,
        EXTENDED_TIER_LABEL,
    }
    if unknown:
        raise ValueError(f"unknown CI labels: {', '.join(sorted(unknown))}")
    if FULL_TIER_LABEL in labels or LEGACY_FULL_TIER_LABEL in labels:
        return "full"
    if EXTENDED_TIER_LABEL in labels:
        return "extended"
    return "minimal"


def selected(tier: Mode | TargetTier) -> list[Target]:
    if tier == "full":
        return list(TARGETS)
    if tier in ("core", "extended"):
        return [target for target in TARGETS if target.tier == "core"]
    if tier == "minimal":
        return [TARGETS[0]]
    raise ValueError(f"unsupported CI tier: {tier}")


def build_matrix(targets: list[Target]) -> dict[str, list[dict[str, str]]]:
    """Build-side matrix: one entry per triple, all on the same Linux builder.

    Darwin used to need a fallback here: `vendor/whisper-rs-sys/build.rs:27-28`
    emits `-framework Accelerate` unconditionally for apple targets, so without
    an SDK on the Linux runner the link failed and the job had to be rerouted to
    a native macOS builder. `soldr prepare --target <apple-triple>` provisions
    that SDK, so the fallback -- and the MACOS_SDK_URL repo variable it keyed
    off -- is gone.
    """
    return {
        "include": [
            {
                "target": target.triple,
                "strategy": target.strategy,
                "runs-on": target.build_runs_on,
            }
            for target in targets
        ]
    }


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


def release_matrix() -> dict[str, list[dict[str, object]]]:
    """Release-side matrix: all six triples, wheel artifacts, no test bundle.

    Deliberately derived from the same TARGETS table -- and therefore the same
    cross-compile strategy -- as CI. If release shipped natively-built wheels
    while CI only ever exercised cross-built binaries, CI would not be testing
    the artifact that ships.
    """
    base = build_matrix(list(TARGETS))["include"]
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
    parser.add_argument("--candidate-sha", default=os.environ.get("CANDIDATE_SHA", ""))
    parser.add_argument("--event-sha", default=os.environ.get("EVENT_SHA", ""))
    parser.add_argument(
        "--provenance-verified",
        default=os.environ.get("PROVENANCE_VERIFIED", "false"),
    )
    parser.add_argument(
        "--release",
        action="store_true",
        help="Emit the release matrix (all triples, wheel artifacts, no test bundle).",
    )
    args = parser.parse_args(argv)

    if args.release:
        emit({"build": json.dumps(release_matrix(), separators=(",", ":"))})
        return 0

    tier = resolve_tier(
        args.event_name,
        args.dispatch_tier,
        args.pr_labels,
        args.candidate_sha,
        args.event_sha,
        args.provenance_verified == "true",
    )
    targets = selected(tier)
    emit(
        {
            "tier": tier,
            "build": json.dumps(build_matrix(targets), separators=(",", ":")),
            "test": json.dumps(exec_matrix(targets), separators=(",", ":")),
        }
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
