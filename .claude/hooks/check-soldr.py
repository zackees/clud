#!/usr/bin/env python3
"""PreToolUse hook: route Rust toolchain calls through soldr.

Blocks bare ``cargo``/``rustc``/``rustfmt`` Bash invocations and tells the
agent to prefix them with ``soldr`` (so they resolve through the rustup-
managed toolchain via ``rustup which`` — see CLAUDE.md). If soldr itself is
not on PATH, the denial points the user at ``./install``.
"""

from __future__ import annotations

import json
import os
import queue
import re
import shutil
import sys
import threading
import time

GUARDED = ("cargo", "rustc", "rustfmt")
INSTALL_HINT = (
    "Install it with: ./install   "
    "(or ./install --global for a system-wide install)."
)
STDIN_READ_CHUNK_BYTES = 64 * 1024
STDIN_READ_MAX_BYTES = 1024 * 1024
STDIN_READ_IDLE_TIMEOUT_SEC = 0.25
STDIN_READ_DEADLINE_SEC = 2.0


def first_command(cmd: str) -> str | None:
    """Return the first executable token in a shell command line.

    Skips leading env-var assignments (``FOO=bar BAR=baz cargo ...``) so we
    still see ``cargo`` as the head.
    """
    cmd = cmd.lstrip()
    while True:
        m = re.match(r"\s*[A-Za-z_][A-Za-z0-9_]*=\S*\s+", cmd)
        if not m:
            break
        cmd = cmd[m.end() :]
    m = re.match(r"\s*([^\s|;&<>()`]+)", cmd)
    return m.group(1) if m else None


def normalize(token: str) -> str:
    base = token.rsplit("/", 1)[-1].rsplit("\\", 1)[-1]
    return base.split(".", 1)[0]


def deny(reason: str) -> None:
    json.dump(
        {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        },
        sys.stdout,
    )


def read_stdin_bounded() -> str:
    out: queue.Queue[bytes | BaseException | None] = queue.Queue()

    def worker() -> None:
        try:
            fd = sys.stdin.fileno()
            while True:
                chunk = os.read(fd, STDIN_READ_CHUNK_BYTES)
                if not chunk:
                    out.put(None)
                    return
                out.put(chunk)
        except BaseException as exc:  # pragma: no cover - defensive fallback path
            out.put(exc)

    thread = threading.Thread(target=worker, name="clud-soldr-hook-stdin", daemon=True)
    thread.start()

    chunks: list[bytes] = []
    byte_count = 0
    deadline = time.monotonic() + STDIN_READ_DEADLINE_SEC
    idle_until: float | None = None
    while True:
        now = time.monotonic()
        wait_until = deadline if idle_until is None else min(deadline, idle_until)
        if now >= wait_until:
            break
        try:
            item = out.get(timeout=max(0.001, wait_until - now))
        except queue.Empty:
            break
        if item is None:
            break
        if isinstance(item, BaseException):
            break
        chunks.append(item)
        byte_count += len(item)
        idle_until = time.monotonic() + STDIN_READ_IDLE_TIMEOUT_SEC
        if byte_count >= STDIN_READ_MAX_BYTES:
            break

    return b"".join(chunks).decode("utf-8", errors="replace").lstrip("\ufeff")


def main() -> int:
    try:
        raw = read_stdin_bounded()
        if not raw.strip():
            return 0
        payload = json.loads(raw)
    except Exception:
        return 0

    tool_input = payload.get("tool_input") or {}
    command = tool_input.get("command")
    if not isinstance(command, str):
        return 0

    head = first_command(command)
    if head is None:
        return 0
    base = normalize(head)

    soldr_present = shutil.which("soldr") is not None

    if base == "soldr":
        if not soldr_present:
            deny(
                "soldr is not installed but the command requires it.\n"
                + INSTALL_HINT
            )
        return 0

    if base not in GUARDED:
        return 0

    if not soldr_present:
        deny(
            f"`{base}` is blocked: this repo requires Rust toolchain calls to "
            "go through soldr (https://github.com/zackees/soldr), but soldr "
            "is not on PATH.\n" + INSTALL_HINT
        )
        return 0

    deny(
        f"`{base}` is blocked: prefix Rust toolchain calls with `soldr` so "
        "they resolve through the rustup-managed toolchain. "
        f"Re-run as: soldr {base} ... (preserves remaining args)."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
