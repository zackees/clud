"""Typed view of `cargo --message-format=json` output (#1726).

`ci/xbuild.py` captures `cargo test --no-run --message-format=json` to learn
where each harness landed. With JSON output the compiler's diagnostics are
records on stdout, not text on stderr, so a failed build printed nothing
unless they are rendered back out. This module parses the stream once into
the two things xbuild needs: the harness executables and the rendered errors.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field


@dataclass(frozen=True)
class CargoMessages:
    """Harness executables (test profile, deduplicated, in order) and rendered errors."""

    harnesses: list[str] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)


def _artifact_harness(record: dict) -> str | None:
    executable = record.get("executable")
    profile = record.get("profile") or {}
    if isinstance(executable, str) and executable and profile.get("test") is True:
        return executable
    return None


def _message_error(record: dict) -> str | None:
    message = record.get("message") or {}
    level = message.get("level")
    rendered = message.get("rendered")
    if isinstance(level, str) and level.startswith("error") and isinstance(rendered, str):
        return rendered
    return None


def parse(stdout: str) -> CargoMessages:
    """Parse cargo's JSON lines; lines that are not JSON objects are skipped."""
    harnesses: list[str] = []
    errors: list[str] = []
    for line in stdout.splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(record, dict):
            continue
        reason = record.get("reason")
        if reason == "compiler-artifact":
            harness = _artifact_harness(record)
            if harness is not None and harness not in harnesses:
                harnesses.append(harness)
        elif reason == "compiler-message":
            error = _message_error(record)
            if error is not None:
                errors.append(error)
    return CargoMessages(harnesses=harnesses, errors=errors)
