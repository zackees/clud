#!/usr/bin/env python3
"""Codex PreToolUse hook: route Rust toolchain calls through soldr.

Codex sibling of `.claude/hooks/check-soldr.py`. Same intent and command
classifier; the deny contract differs:

* Codex blocks via exit code 2 with the human-readable reason on stderr.
* Allow by exiting with code 0 (stdout/stderr ignored).

Payload shape on stdin mirrors Claude's PreToolUse JSON
(`tool_name`, `tool_input`, `hook_event_name`) per codex 0.130's hook
contract — we read the bash string from `tool_input.command` (or argv).

If you change classifier logic here, mirror it in
`.claude/hooks/check-soldr.py` to keep Claude/Codex parity.
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


def extract_command(payload: dict) -> str | None:
    tool_input = payload.get("tool_input") or payload.get("toolInput") or {}
    if isinstance(tool_input, dict):
        command = tool_input.get("command")
        if isinstance(command, str):
            return command
        argv = tool_input.get("argv")
        if isinstance(argv, list):
            return " ".join(str(p) for p in argv)
    if isinstance(tool_input, str):
        return tool_input
    return None


def deny(reason: str) -> int:
    print(reason, file=sys.stderr)
    return 2


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

    command = extract_command(payload)
    if not isinstance(command, str):
        return 0

    head = first_command(command)
    if head is None:
        return 0
    base = normalize(head)

    soldr_present = shutil.which("soldr") is not None

    if base == "soldr":
        if not soldr_present:
            return deny(
                "soldr is not installed but the command requires it.\n"
                + INSTALL_HINT
            )
        return 0

    if base not in GUARDED:
        return 0

    if not soldr_present:
        return deny(
            f"`{base}` is blocked: this repo requires Rust toolchain calls to "
            "go through soldr (https://github.com/zackees/soldr), but soldr "
            "is not on PATH.\n" + INSTALL_HINT
        )

    return deny(
        f"`{base}` is blocked: prefix Rust toolchain calls with `soldr` so "
        "they resolve through the rustup-managed toolchain. "
        f"Re-run as: soldr {base} ... (preserves remaining args)."
    )


if __name__ == "__main__":
    sys.exit(main())
