#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# managed-by: clud
"""block-bad-cmd.py — PreToolUse hook that blocks unsafe shell commands.

Hook contract (mirrors Claude Code + Codex PreToolUse payloads):
- Input: JSON object on stdin with `tool_name`, `tool_input`, `hook_event_name`.
- Block by exiting with code 2 and writing the human-readable reason to
  stderr (plus a `permissionDecision: deny` JSON object on stdout for
  Claude Code's structured-hook protocol).
- Allow by exiting with code 0 (stdout/stderr ignored).

Invoked via `clud tool run hooks/block-bad-cmd.py` so UV_CACHE_DIR is
pinned to ~/.clud/cache/uv per the three-layer enforcement (issue #408).
clud auto-installs this file to ~/.clud/tools/hooks/block-bad-cmd.py on
every startup; the `# managed-by: clud` marker above is what gates the
installer's overwrite behavior. Hand-edit at your own risk — drop the
marker if you want the installer to leave your copy alone.

Logs every invocation to `~/.clud/tools/hooks/block-bad-cmd.log` so we
can prove the hook ran even when it allows the call.
"""

from __future__ import annotations

import datetime as _dt
import json
import os
import queue
import re
import sys
import threading
import time
import traceback
from pathlib import Path

LOG_PATH = Path.home() / ".clud" / "tools" / "hooks" / "block-bad-cmd.log"
STDIN_READ_CHUNK_BYTES = 64 * 1024
STDIN_READ_MAX_BYTES = 1024 * 1024
RUST_TOOLS = {
    "cargo",
    "rustc",
    "rustfmt",
    "clippy-driver",
    "cargo-clippy",
    "cargo-fmt",
    "rustup",
    "rustdoc",
    "rust-gdb",
    "rust-lldb",
    "rust-analyzer",
}
LEGACY_RUST_TRAMPOLINES = {"_cargo", "_rustc", "_rustfmt"}
SHELL_WRAPPERS = {"cmd", "powershell", "pwsh", "bash", "sh", "zsh"}
UV_RUN_OPTIONS_WITH_VALUE = {
    "--active",
    "--config-file",
    "--directory",
    "--env-file",
    "--exclude-newer",
    "--extra",
    "--frozen",
    "--index",
    "--index-strategy",
    "--isolated",
    "--keyring-provider",
    "--link-mode",
    "--managed-python",
    "--module",
    "--no-binary",
    "--no-binary-package",
    "--no-build",
    "--no-build-isolation-package",
    "--no-build-package",
    "--no-cache",
    "--no-config",
    "--no-default-groups",
    "--no-dev",
    "--no-editable",
    "--no-extra",
    "--no-group",
    "--no-index",
    "--no-managed-python",
    "--no-project",
    "--no-python-downloads",
    "--only-dev",
    "--only-group",
    "--project",
    "--python",
    "--python-platform",
    "--refresh-package",
    "--resolution",
    "--script",
    "--upgrade-package",
    "--with",
    "--with-editable",
    "--with-requirements",
}


def _log(msg: str) -> None:
    try:
        LOG_PATH.parent.mkdir(parents=True, exist_ok=True)
        with LOG_PATH.open("a", encoding="utf-8") as fh:
            ts = _dt.datetime.now().isoformat(timespec="seconds")
            fh.write(f"[{ts}] pid={os.getpid()} {msg}\n")
    except OSError:
        pass


def _float_env(name: str, default: float) -> float:
    raw = os.environ.get(name)
    if raw is None:
        return default
    try:
        value = float(raw)
    except ValueError:
        return default
    return max(0.01, value)


STDIN_READ_IDLE_TIMEOUT_SEC = _float_env("CLUD_HOOK_STDIN_IDLE_TIMEOUT_SEC", 0.25)
STDIN_READ_DEADLINE_SEC = _float_env("CLUD_HOOK_STDIN_DEADLINE_SEC", 2.0)


def _decode_stdin(chunks: list[bytes]) -> str:
    return b"".join(chunks).decode("utf-8", errors="replace").lstrip("\ufeff")


def _stack_summary(thread_id: int | None = None) -> str:
    frame = None if thread_id is None else sys._current_frames().get(thread_id)
    if frame is None:
        stack = traceback.format_stack(limit=8)
    else:
        stack = traceback.format_stack(frame, limit=8)
    return " | ".join(line.strip().replace("\n", " ") for line in stack)


def _log_stdin_incomplete(
    mode: str,
    reason: str,
    byte_count: int,
    thread_id: int | None = None,
) -> None:
    _log(
        "stdin_read_incomplete "
        f"mode={mode} reason={reason} bytes={byte_count} "
        f"stack={_stack_summary(thread_id)}"
    )


def _read_stdin_nonblocking() -> str | None:
    stream = getattr(sys.stdin, "buffer", sys.stdin)
    try:
        fd = stream.fileno()
        was_blocking = os.get_blocking(fd)
        os.set_blocking(fd, False)
    except (AttributeError, OSError, ValueError):
        return None

    chunks: list[bytes] = []
    byte_count = 0
    deadline = time.monotonic() + STDIN_READ_DEADLINE_SEC
    idle_until: float | None = None
    incomplete_reason: str | None = None
    try:
        while True:
            try:
                chunk = os.read(fd, STDIN_READ_CHUNK_BYTES)
            except BlockingIOError:
                now = time.monotonic()
                wait_until = deadline if idle_until is None else min(deadline, idle_until)
                if now >= wait_until:
                    incomplete_reason = (
                        "idle"
                        if idle_until is not None and idle_until <= deadline
                        else "deadline"
                    )
                    break
                time.sleep(min(0.01, max(0.001, wait_until - now)))
                continue
            except OSError as exc:
                _log(f"stdin_read_error mode=nonblocking error={exc}")
                return _decode_stdin(chunks)

            if not chunk:
                break
            chunks.append(chunk)
            byte_count += len(chunk)
            idle_until = time.monotonic() + STDIN_READ_IDLE_TIMEOUT_SEC
            if byte_count >= STDIN_READ_MAX_BYTES:
                incomplete_reason = "max_bytes"
                break
    finally:
        try:
            os.set_blocking(fd, was_blocking)
        except OSError:
            pass

    if incomplete_reason is not None:
        _log_stdin_incomplete("nonblocking", incomplete_reason, byte_count)
    return _decode_stdin(chunks)


def _read_stdin_threaded() -> str:
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

    thread = threading.Thread(target=worker, name="clud-hook-stdin-reader", daemon=True)
    thread.start()

    chunks: list[bytes] = []
    byte_count = 0
    deadline = time.monotonic() + STDIN_READ_DEADLINE_SEC
    idle_until: float | None = None
    incomplete_reason: str | None = None
    while True:
        now = time.monotonic()
        wait_until = deadline if idle_until is None else min(deadline, idle_until)
        if now >= wait_until:
            incomplete_reason = (
                "idle" if idle_until is not None and idle_until <= deadline else "deadline"
            )
            break
        try:
            item = out.get(timeout=max(0.001, wait_until - now))
        except queue.Empty:
            incomplete_reason = (
                "idle" if idle_until is not None and idle_until <= deadline else "deadline"
            )
            break
        if item is None:
            break
        if isinstance(item, BaseException):
            _log(f"stdin_read_error mode=threaded error={item}")
            break
        chunks.append(item)
        byte_count += len(item)
        idle_until = time.monotonic() + STDIN_READ_IDLE_TIMEOUT_SEC
        if byte_count >= STDIN_READ_MAX_BYTES:
            incomplete_reason = "max_bytes"
            break

    if incomplete_reason is not None:
        _log_stdin_incomplete("threaded", incomplete_reason, byte_count, thread.ident)
    return _decode_stdin(chunks)


def _read_stdin_bounded() -> str:
    if os.name == "nt":
        return _read_stdin_threaded()
    raw = _read_stdin_nonblocking()
    if raw is not None:
        return raw
    return _read_stdin_threaded()


def _extract_command(payload: dict) -> str:
    """Pull the command/argv text out of the tool input, regardless of shape."""
    tool_input = payload.get("tool_input") or payload.get("toolInput") or {}
    if isinstance(tool_input, dict):
        for key in ("command", "script"):
            cmd = tool_input.get(key)
            if isinstance(cmd, str):
                return cmd
        argv = tool_input.get("argv")
        if isinstance(argv, list):
            return " ".join(str(p) for p in argv)
    if isinstance(tool_input, str):
        return tool_input
    return ""


def _is_env_assignment(word: str) -> bool:
    return re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", word) is not None


def _split_shell_segments(command_text: str) -> list[str]:
    segments: list[str] = []
    buf: list[str] = []
    quote: str | None = None
    i = 0
    while i < len(command_text):
        ch = command_text[i]
        if quote is not None:
            buf.append(ch)
            if ch == quote:
                quote = None
            i += 1
            continue

        if ch in {"'", '"'}:
            quote = ch
            buf.append(ch)
            i += 1
            continue

        is_double_amp = ch == "&" and i + 1 < len(command_text) and command_text[i + 1] == "&"
        is_double_pipe = ch == "|" and i + 1 < len(command_text) and command_text[i + 1] == "|"
        if ch in {";", "|", "\r", "\n"} or is_double_amp:
            segment = "".join(buf).strip()
            if segment:
                segments.append(segment)
            buf = []
            i += 2 if is_double_amp or is_double_pipe else 1
            continue

        buf.append(ch)
        i += 1

    segment = "".join(buf).strip()
    if segment:
        segments.append(segment)
    return segments


def _tokenize(segment: str) -> list[str]:
    words: list[str] = []
    buf: list[str] = []
    quote: str | None = None
    for ch in segment:
        if quote is not None:
            if ch == quote:
                quote = None
            else:
                buf.append(ch)
            continue

        if ch in {"'", '"'}:
            quote = ch
            continue
        if ch.isspace():
            if buf:
                words.append("".join(buf))
                buf = []
            continue
        buf.append(ch)

    if buf:
        words.append("".join(buf))
    return words


def _program_name(word: str) -> str:
    cleaned = word.strip().strip("'\"").replace("\\", "/")
    while cleaned.startswith("./"):
        cleaned = cleaned[2:]
    base = cleaned.rsplit("/", 1)[-1].lower()
    for suffix in (".exe", ".cmd", ".bat", ".ps1"):
        if base.endswith(suffix):
            base = base[: -len(suffix)]
            break
    return base


def _command_words(segment: str) -> list[str]:
    words = _tokenize(segment)
    while words and words[0] in {"&", "call", "exec", "command"}:
        words = words[1:]
    if words and _program_name(words[0]) == "env":
        words = words[1:]
    while words and _is_env_assignment(words[0]):
        words = words[1:]
    return words


def _resolve_uv_run_tool(words: list[str]) -> str | None:
    if len(words) < 3 or _program_name(words[0]) != "uv" or words[1] != "run":
        return None

    i = 2
    while i < len(words):
        word = words[i]
        if word == "--":
            i += 1
            break
        if word == "--script" and i + 1 < len(words):
            return words[i + 1]
        if word.startswith("--script="):
            return word.split("=", 1)[1]
        if not word.startswith("-"):
            break
        if "=" not in word and word in UV_RUN_OPTIONS_WITH_VALUE:
            i += 2
        else:
            i += 1

    return words[i] if i < len(words) else None


def _nested_shell_command(words: list[str]) -> str | None:
    if not words:
        return None
    first = _program_name(words[0])
    if first not in SHELL_WRAPPERS:
        return None

    if first == "cmd":
        for i, word in enumerate(words[1:], start=1):
            if word.lower() in {"/c", "/r"} and i + 1 < len(words):
                return " ".join(words[i + 1 :])
        return None

    if first in {"powershell", "pwsh"}:
        for i, word in enumerate(words[1:], start=1):
            if word.lower() in {"-command", "-c", "/c"} and i + 1 < len(words):
                return " ".join(words[i + 1 :])
        return None

    for i, word in enumerate(words[1:], start=1):
        option = word.lower().lstrip("-")
        if "c" in option and i + 1 < len(words):
            return " ".join(words[i + 1 :])
    return None


def _python_rust_hybrid_root(cwd: Path | None) -> Path | None:
    """Walk up from `cwd` looking for the nearest ancestor directory that
    contains BOTH `pyproject.toml` and `Cargo.toml`. That coexistence is
    the signal that `uv run`'s project auto-sync may rebuild a native
    Rust extension (maturin, setuptools-rust, or a custom build script
    that calls cargo) — the case the gate is trying to prevent.

    Returns the directory path when found, else `None`. A repo with
    only one of the two files (pure Python, pure Rust) is not a hybrid
    and never triggers the block.
    """
    if cwd is None:
        return None
    try:
        anchor = cwd.resolve()
    except OSError:
        return None
    for candidate in (anchor, *anchor.parents):
        try:
            has_py = (candidate / "pyproject.toml").is_file()
            has_rs = (candidate / "Cargo.toml").is_file()
        except OSError:
            continue
        if has_py and has_rs:
            return candidate
    return None


def _forbidden_reason(command_text: str, cwd: Path | None = None) -> str | None:
    if "bad cmd" in command_text.lower():
        return f'command contains "bad cmd". Full command: {command_text!r}'

    for segment in _split_shell_segments(command_text):
        segment = segment.strip()
        if not segment:
            continue

        words = _command_words(segment)
        if not words:
            continue

        first = _program_name(words[0])
        nested = _nested_shell_command(words)
        if nested is not None:
            nested_reason = _forbidden_reason(nested, cwd=cwd)
            if nested_reason is not None:
                return nested_reason
            continue

        if first in LEGACY_RUST_TRAMPOLINES:
            return (
                f"Use `soldr {first[1:]} ...` instead of legacy `{words[0]}`. "
                "The root Rust trampolines bypass soldr's toolchain selection."
            )

        if first == "soldr":
            continue

        if first == "uv" and len(words) > 1 and words[1] == "run":
            tool = _resolve_uv_run_tool(words)
            if tool is not None:
                tool_bare = _program_name(tool)
                if tool_bare in LEGACY_RUST_TRAMPOLINES:
                    return (
                        f"Use `soldr {tool_bare[1:]} ...` instead of legacy `{tool}`. "
                        "The root Rust trampolines bypass soldr's toolchain selection."
                    )
                if tool_bare in RUST_TOOLS:
                    return (
                        f"Use `soldr {tool_bare} ...` instead of `uv run {tool} ...`. "
                        "`uv run <rust-tool>` bypasses soldr's toolchain selection."
                    )

            # Require an opt-out for the project auto-sync. Without one of
            # --no-project / --no-sync / --frozen, `uv run` reinstalls the
            # project into the venv on every invocation. In a Python+Rust
            # hybrid (a pyproject.toml + Cargo.toml in the same project
            # root) that auto-sync can trigger a full native rebuild
            # (maturin, setuptools-rust, or a build script that shells
            # out to cargo) — minutes of compile for operations that
            # don't need the native extension at all (lint, version-check,
            # doc generation, etc.). The escape hatch for the legitimate
            # full-rebuild case is `./test` — see zackees/soldr#805.
            #
            # Scoping: file-based heuristic instead of parsing pyproject
            # build-backend. Pure-Python repos (FastLED uses hatchling)
            # have no Cargo.toml at all, so they fall through.
            uv_safe_flags = {"--no-project", "--no-sync", "--frozen"}
            has_uv_safe_flag = any(
                w in uv_safe_flags or any(w.startswith(f + "=") for f in uv_safe_flags)
                for w in words[2:]
            )
            if not has_uv_safe_flag:
                hybrid_root = _python_rust_hybrid_root(cwd)
                if hybrid_root is not None:
                    # Env-var escape hatch: lets a caller intentionally
                    # eat the auto-sync cost (e.g. they DO want the
                    # rebuild, or they're running a one-off where the
                    # latency is fine). Allow the command but emit a
                    # yellow warning so the rebuild isn't silent and the
                    # calling agent knows what just happened. Logged too
                    # so the bypass is visible in the audit trail.
                    if os.environ.get("CLUD_UV_RUST_ALLOW_ALL") == "1":
                        _log(f"CLUD_UV_RUST_ALLOW_ALL=1 bypassed hybrid block at {hybrid_root}")
                        print(
                            "\x1b[33mWARNING: AUTO COMPILING RUST because of uv run\n"
                            "CLUD_UV_RUST_ALLOW_ALL=1 is set, so the auto-sync gate at "
                            f"{hybrid_root} was bypassed.\n"
                            "DIRECTIVE TO AGENT: the next `uv run` in this project root will "
                            "trigger a full native rebuild (can take minutes). If you don't need "
                            "a fresh build, pass `--no-sync` (use existing venv), `--no-project` "
                            "(pure-Python script), or `--frozen` (lock to existing lockfile) to "
                            "skip the auto-sync. If you DO need a clean rebuild, prefer `./test` "
                            "(or `bash ./test`) — the canonical full-build entrypoint.\x1b[0m",
                            file=sys.stderr,
                        )
                    else:
                        return (
                            f"this hook fired because {hybrid_root} contains "
                            "both pyproject.toml and Cargo.toml (a Python+Rust "
                            "hybrid project). `uv run` without --no-project / "
                            "--no-sync / --frozen triggers the project "
                            "auto-sync, which on a Rust-backed wheel is a full "
                            "native rebuild. Pass `--no-project` for pure-Python "
                            "scripts, `--no-sync` to use the existing venv, or "
                            "`--frozen` to lock to the existing lockfile. "
                            "Escape hatch for a legitimate full-rebuild: run "
                            "`./test` (or `bash ./test`) — the canonical "
                            "full-build entrypoint. Set "
                            "CLUD_UV_RUST_ALLOW_ALL=1 to bypass this gate "
                            "with a warning. See zackees/soldr#805."
                        )
            continue

        if first in RUST_TOOLS:
            return (
                f"Use `soldr {first} ...` instead of bare `{first}`. "
                "soldr resolves the pinned rustup-managed toolchain and avoids "
                "GNU/Chocolatey shims."
            )

    return None


def main() -> int:
    raw = _read_stdin_bounded()
    _log(f"raw_stdin_bytes={len(raw)}")
    try:
        payload = json.loads(raw) if raw.strip() else {}
    except json.JSONDecodeError as exc:
        _log(f"json_decode_error: {exc}")
        return 0  # don't block on a malformed payload

    tool_name = payload.get("tool_name") or payload.get("toolName") or "?"
    command_text = _extract_command(payload)
    cwd_raw = payload.get("cwd") or payload.get("cwdPath")
    if not isinstance(cwd_raw, str) or not cwd_raw:
        cwd_raw = os.getcwd()
    try:
        cwd = Path(cwd_raw)
    except (TypeError, ValueError):
        cwd = None
    _log(f"tool_name={tool_name!r} cwd={cwd_raw!r} command={command_text!r}")

    reason = _forbidden_reason(command_text, cwd=cwd)
    if reason is not None:
        msg = f"[block-bad-cmd hook] refusing to run {tool_name!r}: {reason}"
        _log(f"BLOCKED: {msg}")
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
        print(msg, file=sys.stderr)
        return 2

    _log("allowed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
