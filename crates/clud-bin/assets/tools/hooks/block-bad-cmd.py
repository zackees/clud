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
import re
import sys
from pathlib import Path

LOG_PATH = Path.home() / ".clud" / "tools" / "hooks" / "block-bad-cmd.log"
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


def _find_pyproject(start: Path) -> Path | None:
    """Walk up from `start` looking for the nearest `pyproject.toml`."""
    try:
        anchor = start.resolve()
    except OSError:
        return None
    for candidate in (anchor, *anchor.parents):
        pp = candidate / "pyproject.toml"
        try:
            if pp.is_file():
                return pp
        except OSError:
            continue
    return None


def _maturin_build_backend(cwd: Path | None) -> tuple[Path, str] | None:
    """Return `(pyproject_path, backend_value)` when the nearest
    `pyproject.toml` above `cwd` declares a maturin build backend. The
    `uv run` auto-sync block is scoped to this case — other repos (hatchling,
    setuptools, no pyproject at all) don't pay the maturin rebuild cost and
    must not be blocked. Returns `None` when no cwd is available, no
    pyproject is found, the file can't be parsed, or the backend isn't
    maturin.
    """
    if cwd is None:
        return None
    try:
        import tomllib
    except ImportError:
        return None
    pp = _find_pyproject(cwd)
    if pp is None:
        return None
    try:
        with pp.open("rb") as fh:
            data = tomllib.load(fh)
    except (OSError, tomllib.TOMLDecodeError):
        return None
    build_system = data.get("build-system")
    if not isinstance(build_system, dict):
        return None
    backend = build_system.get("build-backend")
    if isinstance(backend, str) and "maturin" in backend.lower():
        return pp, backend
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
            # project into the venv on every invocation. On maturin-backed
            # repos that is a full Rust+PyO3 rebuild (~10s+ per call) for
            # operations that don't need the native extension at all
            # (lint, version-check, doc generation, etc.). The escape hatch
            # for the legitimate full-rebuild case is `./test` — see
            # zackees/soldr#805.
            #
            # Scoping: only enforce this in projects whose `pyproject.toml`
            # actually declares a maturin build backend. Other repos pay no
            # rebuild cost on `uv run` and the block was a false positive
            # there (e.g. FastLED uses hatchling — `uv run` is fast and
            # blocking it was just noise).
            uv_safe_flags = {"--no-project", "--no-sync", "--frozen"}
            has_uv_safe_flag = any(
                w in uv_safe_flags or any(w.startswith(f + "=") for f in uv_safe_flags)
                for w in words[2:]
            )
            if not has_uv_safe_flag:
                maturin = _maturin_build_backend(cwd)
                if maturin is not None:
                    pp_path, backend = maturin
                    return (
                        f"this hook fired because {pp_path} declares "
                        f"build-backend = {backend!r}. `uv run` without "
                        "--no-project / --no-sync / --frozen triggers the "
                        "project auto-sync, which on a maturin-backed repo "
                        "is a full Rust+PyO3 rebuild. Pass `--no-project` "
                        "for pure-Python scripts, `--no-sync` to use the "
                        "existing venv, or `--frozen` to lock to the "
                        "existing lockfile. Escape hatch for a legitimate "
                        "full-rebuild: run `./test` (or `bash ./test`) — "
                        "the canonical full-build entrypoint. See "
                        "zackees/soldr#805."
                    )
            continue

        if first in RUST_TOOLS:
            return (
                f"Use `soldr {first} ...` instead of bare `{first}`. "
                "soldr resolves the pinned rustup-managed toolchain and avoids GNU/Chocolatey shims."
            )

    return None


def main() -> int:
    raw = sys.stdin.read()
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
