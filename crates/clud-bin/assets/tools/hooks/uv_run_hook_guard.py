#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# managed-by: clud
"""uv_run_hook_guard.py — warn on bare `uv run` in agent hooks of polyglot repos.

When clud is invoked inside a repo that has BOTH a Cargo workspace AND a
Python project with a `[build-system].build-backend` declaration, this
script scans `.claude/settings.json` (incl. `settings.local.json`) and
`.codex/hooks.json` for `PreToolUse` / `PostToolUse` hook commands that
use bare `uv run …` without one of the safeguard flags
(`--no-project`, `--no-sync`, `--frozen`).

The failure mode this catches:
  - maturin-backed pyproject: every hook fire is a full Rust+PyO3 rebuild
    (minutes per call); cold-cache CI runners exceed the typical 5s hook
    timeout and the guard silently never runs.
  - setuptools-backed pyproject co-existing with Cargo: every hook fire
    is a venv re-sync (seconds per call) plus the same silent-timeout
    risk.

For each offender the script prints a yellow stderr warning naming the
config file, the hook event (PreToolUse / PostToolUse), the matcher,
and the offending command. After printing it sleeps 3 seconds so the
warning is visible before the next hook fires. Always exits 0 — this
is informational, never blocking.

The scanner also follows hook commands one level deep: if the hook
invokes a local repo script (`./ci.sh`, `bash ./test`, `./build`,
etc.) the referenced script is read and grepped for the same anti-
pattern, so a hook that wraps the offender through a shell file gets
flagged too.

Invoked via `clud tool run hooks/uv_run_hook_guard.py` so UV_CACHE_DIR
is pinned to ~/.clud/cache/uv (issue #408 three-layer enforcement) and
managed install lifecycle preserves user edits.
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
from dataclasses import dataclass
from pathlib import Path

# ANSI escape sequences. Yellow for the warning, reset to clear.
# `NO_COLOR` env var (de-facto cross-CLI standard) disables.
def _ansi(seq: str) -> str:
    if os.environ.get("NO_COLOR"):
        return ""
    return seq


YELLOW = _ansi("\x1b[33m")
BOLD = _ansi("\x1b[1m")
RESET = _ansi("\x1b[0m")

# Long-form flags that opt OUT of the project auto-sync. Presence of
# any one of these on a `uv run` invocation makes it safe.
SAFE_UV_RUN_FLAGS = {"--no-project", "--no-sync", "--frozen"}

# Hook event keys we care about. SessionStart, Stop, etc. don't pay
# the per-tool-call cost so the rebuild penalty is much smaller; not
# in scope for v0.
SCANNED_EVENTS = ("PreToolUse", "PostToolUse")

# File extensions we'll dereference one level when a hook command is
# a path to a local repo script. .sh / .bash for POSIX, .cmd / .bat /
# .ps1 for Windows, .py for direct python scripts (which could call
# uv run internally via subprocess).
FOLLOWABLE_SCRIPT_EXTS = {".sh", ".bash", ".cmd", ".bat", ".ps1", ".py"}


@dataclass(frozen=True)
class Offender:
    config_path: Path
    event: str
    matcher: str
    command: str
    indirect_via: Path | None = None  # set when surfaced via a wrapper script

    def render(self) -> str:
        loc = (
            f"{self.config_path} → {self.event} ({self.matcher})"
            if not self.indirect_via
            else f"{self.config_path} → {self.event} ({self.matcher}) → {self.indirect_via}"
        )
        return (
            f"{YELLOW}{BOLD}[clud] WARNING: bare `uv run` in agent hook{RESET}\n"
            f"  {loc}\n"
            f"  command: {self.command.strip()}\n"
            f"  fix:     add `--no-project`, `--no-sync`, or `--frozen` to the uv run\n"
        )


def _repo_qualifies(repo_root: Path) -> bool:
    """Gate: scan only when the repo is a Python+Rust polyglot with a build backend."""
    cargo = repo_root / "Cargo.toml"
    pyproject = repo_root / "pyproject.toml"
    if not (cargo.is_file() and pyproject.is_file()):
        return False
    try:
        body = pyproject.read_text(encoding="utf-8")
    except OSError:
        return False
    # Permissive grep — we don't want to depend on tomllib's TOML
    # subset enforcement here, just signal whether the user has a
    # build-system that uv would resolve when it sees the file.
    return re.search(r"(?m)^\s*build-backend\s*=\s*[\"']", body) is not None


def _iter_hooks_from_claude(config_path: Path) -> list[tuple[str, str, str]]:
    """Extract (event, matcher, command) tuples from a Claude settings.json."""
    try:
        data = json.loads(config_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return []
    hooks = data.get("hooks") if isinstance(data, dict) else None
    if not isinstance(hooks, dict):
        return []
    return _iter_hooks_from_hooks_obj(hooks)


def _iter_hooks_from_codex(config_path: Path) -> list[tuple[str, str, str]]:
    """Extract (event, matcher, command) tuples from a Codex hooks.json."""
    try:
        data = json.loads(config_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return []
    hooks = data.get("hooks") if isinstance(data, dict) else None
    if not isinstance(hooks, dict):
        return []
    return _iter_hooks_from_hooks_obj(hooks)


def _iter_hooks_from_hooks_obj(hooks: dict) -> list[tuple[str, str, str]]:
    """Common (event, matcher, command) extractor for both config shapes.

    The two CLIs share the same schema:
        {"hooks": {"<event>": [{"matcher": "<m>", "hooks": [{"command": "<c>"}]}]}}
    """
    out: list[tuple[str, str, str]] = []
    for event in SCANNED_EVENTS:
        for matcher_block in hooks.get(event) or []:
            if not isinstance(matcher_block, dict):
                continue
            matcher = str(matcher_block.get("matcher", ""))
            for entry in matcher_block.get("hooks") or []:
                if not isinstance(entry, dict):
                    continue
                command = entry.get("command")
                if isinstance(command, str) and command.strip():
                    out.append((event, matcher, command))
    return out


def _has_bare_uv_run(command: str) -> bool:
    """True iff `command` invokes `uv run` and lacks every safeguard flag."""
    # Word-boundary match so `aiouv run` etc. don't false-positive.
    if not re.search(r"\buv\s+run\b", command):
        return False
    # Tokenize on whitespace; checks the literal flag presence. The
    # CI-typical case is whitespace-separated args, no equals form. We
    # also handle `--foo=bar` for safety since `--no-project` is a
    # bare boolean flag (no value) so equals form isn't legal, but be
    # forgiving on the others.
    tokens = command.split()
    for flag in SAFE_UV_RUN_FLAGS:
        for tok in tokens:
            if tok == flag or tok.startswith(flag + "="):
                return False
    return True


def _resolve_referenced_script(command: str, repo_root: Path) -> Path | None:
    """If the hook command starts with a local repo script, return its path."""
    tokens = command.split()
    if not tokens:
        return None
    # Strip common shell wrappers: `bash ./foo.sh args`, `python ./bar.py`, etc.
    shell_wrappers = {"bash", "sh", "zsh", "python", "python3", "pwsh", "powershell", "cmd"}
    i = 0
    while i < len(tokens) - 1 and tokens[i] in shell_wrappers:
        i += 1
    candidate = tokens[i]
    # Strip leading `./` and resolve under repo root.
    if candidate.startswith("./"):
        candidate = candidate[2:]
    # Reject absolute paths and `..` traversal — we only follow scripts
    # that live inside the repo we're scanning.
    if Path(candidate).is_absolute():
        return None
    if ".." in Path(candidate).parts:
        return None
    target = (repo_root / candidate).resolve()
    try:
        # Sanity: resolved path must be under repo_root.
        target.relative_to(repo_root.resolve())
    except ValueError:
        return None
    if not target.is_file():
        return None
    if target.suffix.lower() not in FOLLOWABLE_SCRIPT_EXTS:
        return None
    return target


def _scan_referenced_script(
    script_path: Path,
) -> list[str]:
    """Return command-like lines inside `script_path` that contain bare uv run."""
    try:
        body = script_path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return []
    hits: list[str] = []
    for line in body.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if _has_bare_uv_run(stripped):
            hits.append(stripped)
    return hits


def scan(repo_root: Path) -> list[Offender]:
    """Run the full scan, returning every offender found."""
    if not _repo_qualifies(repo_root):
        return []

    configs: list[tuple[Path, str]] = [
        (repo_root / ".claude" / "settings.json", "claude"),
        (repo_root / ".claude" / "settings.local.json", "claude"),
        (repo_root / ".codex" / "hooks.json", "codex"),
    ]

    offenders: list[Offender] = []
    for config_path, kind in configs:
        if not config_path.is_file():
            continue
        if kind == "claude":
            entries = _iter_hooks_from_claude(config_path)
        else:
            entries = _iter_hooks_from_codex(config_path)
        for event, matcher, command in entries:
            if _has_bare_uv_run(command):
                offenders.append(
                    Offender(
                        config_path=config_path,
                        event=event,
                        matcher=matcher,
                        command=command,
                    )
                )
                continue
            # The hook doesn't directly call uv run, but it might
            # wrap a local script that does. Dereference one level.
            target = _resolve_referenced_script(command, repo_root)
            if target is None:
                continue
            for hit in _scan_referenced_script(target):
                offenders.append(
                    Offender(
                        config_path=config_path,
                        event=event,
                        matcher=matcher,
                        command=hit,
                        indirect_via=target,
                    )
                )
    return offenders


def main(argv: list[str]) -> int:
    # Single optional arg: the repo root. Defaults to CWD so the
    # clud-side caller doesn't have to compute it.
    repo_root = Path(argv[1]).resolve() if len(argv) > 1 else Path.cwd().resolve()
    offenders = scan(repo_root)
    if not offenders:
        return 0
    sys.stderr.write(
        f"{YELLOW}{BOLD}[clud] uv_run_hook_guard: detected {len(offenders)} "
        f"bare `uv run` invocation(s) in agent hooks of a Python+Rust "
        f"polyglot repo.{RESET}\n"
        "Bare `uv run` walks the tree to pyproject.toml, finds the "
        "build-backend, and triggers a project re-sync (full maturin "
        "rebuild on maturin-backed projects) on every hook fire — "
        "minutes per call, often silently failing past the hook "
        "timeout. Fix each one below.\n\n"
    )
    for off in offenders:
        sys.stderr.write(off.render())
    sys.stderr.write("\n")
    sys.stderr.flush()
    # 3-second pause so the user actually sees the warning before
    # the next tool call obliterates the scrollback.
    time.sleep(3)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
