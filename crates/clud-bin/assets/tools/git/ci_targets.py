#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# managed-by: clud
"""ci_targets.py — the Rust target triples a project's CI actually tests (#1430).

Usage:
  ci_targets.py [--root DIR] [--host TRIPLE]

A code error can hide on the host and surface only on another OS's CI: a
`#[cfg(test)]` helper whose only caller sits behind `#[cfg(target_os =
"linux")]` compiles clean on Linux and fails `-D warnings` on Windows. The
grind integrator cross-checks every target the project's CI runs, before host
lint and test. This tool finds those targets; it runs nothing.

Targets come from what the project's CI tests, never from a fixed list:
  * `runs-on:` runner names in `.github/workflows/*.yml|yaml` (ubuntu /
    windows / macos families, with their arm and intel variants);
  * explicit `--target <triple>` and `target: <triple>` in those files;
  * `targets = [...]` in `rust-toolchain.toml` / `rust-toolchain`.

Only triples soldr owns cross toolchains for are returned as `targets`; every
other triple is listed under `skipped` with a reason, never dropped silently.
`--host` removes the host triple (host verification is a separate step).

Prints JSON on stdout:
  {"rust": bool, "targets": [{"triple", "source"}],
   "skipped": [{"triple", "reason"}]}
A repository with no Cargo.toml prints `not a Rust project` instead and exits 0.

Run through clud's `tool run` subcommand.

Exit codes:
  0  answered
  1  usage error
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

EXIT_OK = 0
EXIT_USAGE = 1

#: Triples soldr can cross-check from another host (Apple, MSVC and GNU/musl Linux).
SOLDR_OWNED = (
    re.compile(r"^(x86_64|aarch64)-pc-windows-msvc$"),
    re.compile(r"^(x86_64|aarch64)-apple-darwin$"),
    re.compile(r"^(x86_64|aarch64)-unknown-linux-(gnu|musl)$"),
)

_TRIPLE = re.compile(
    r"\b((?:x86_64|aarch64|i686|i586|armv7|arm|riscv64gc|wasm32|powerpc64le|s390x)"
    r"-[a-z0-9_]+-[a-z0-9_]+(?:-[a-z0-9_]+)?)\b"
)
# The lookbehind keeps `pc-windows-gnu` (a triple) from reading as a runner.
_RUNNER = re.compile(r"(?<![\w-])((?:ubuntu|windows|macos)-[a-z0-9.]+(?:-[a-z0-9.]+)*)\b", re.I)


def runner_triple(runner: str) -> str | None:
    """The target a GitHub-hosted runner name implies, or `None` for an unknown one."""
    name = runner.lower()
    arm = "arm" in name
    if name.startswith("ubuntu-"):
        return "aarch64-unknown-linux-gnu" if arm else "x86_64-unknown-linux-gnu"
    if name.startswith("windows-"):
        return "aarch64-pc-windows-msvc" if arm else "x86_64-pc-windows-msvc"
    if name.startswith("macos-"):
        # macos-13 and *-intel / *-x86 are Intel; macos-latest and 14+ are Apple silicon.
        intel = name.endswith(("-intel", "-x86", "-large")) or re.match(r"macos-1[0-3]\b", name)
        return "x86_64-apple-darwin" if intel else "aarch64-apple-darwin"
    return None


def _toolchain_targets(root: Path) -> list[str]:
    found: list[str] = []
    for name in ("rust-toolchain.toml", "rust-toolchain"):
        path = root / name
        if not path.is_file():
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        match = re.search(r"targets\s*=\s*\[([^\]]*)\]", text, re.S)
        if match:
            found += re.findall(r"[\"']([^\"']+)[\"']", match.group(1))
    return found


def discover(root: Path, host: str | None = None) -> dict[str, object]:  # noqa: C901
    if not (root / "Cargo.toml").is_file():
        return {"rust": False, "targets": [], "skipped": []}
    seen: dict[str, str] = {}

    def add(triple: str, source: str) -> None:
        seen.setdefault(triple, source)

    workflows = root / ".github" / "workflows"
    files = sorted([*workflows.glob("*.yml"), *workflows.glob("*.yaml")]) if workflows.is_dir() else []
    for path in files:
        text = path.read_text(encoding="utf-8", errors="replace")
        for line in text.splitlines():
            code = line.split("#", 1)[0]
            for runner in _RUNNER.findall(code):
                triple = runner_triple(runner)
                if triple:
                    add(triple, f"{path.name}: runs-on {runner}")
            if "target" in code:
                for triple in _TRIPLE.findall(code):
                    add(triple, f"{path.name}: {code.strip()[:60]}")
    for triple in _toolchain_targets(root):
        add(triple, "rust-toolchain")

    targets, skipped = [], []
    for triple, source in seen.items():
        if triple == host:
            continue
        if any(pattern.match(triple) for pattern in SOLDR_OWNED):
            targets.append({"triple": triple, "source": source})
        else:
            skipped.append({"triple": triple, "reason": "not a triple soldr owns a cross toolchain for"})
    return {"rust": True, "targets": targets, "skipped": skipped}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="ci_targets", description=__doc__.split("\n")[0])
    parser.add_argument("--root", default=".", help="repository root (default: cwd)")
    parser.add_argument("--host", help="host triple to leave out of the targets")
    try:
        ns = parser.parse_args(argv if argv is not None else sys.argv[1:])
    except SystemExit as exit_:
        return EXIT_USAGE if exit_.code not in (0, None) else EXIT_OK
    result = discover(Path(ns.root), ns.host)
    if not result["rust"]:
        print("not a Rust project")
        return EXIT_OK
    print(json.dumps(result))
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
