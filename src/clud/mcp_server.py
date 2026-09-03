"""MCP bridge exposing the `clud` agent harness to MCP clients (e.g. Hermes Agent).

Runs as a stdio MCP server. It registers a single ``run`` tool that executes
``clud -p <prompt>`` and streams the harness output back to the client as MCP
log notifications while accumulating it for the final tool result.

Process execution uses :mod:`asyncio` subprocess primitives (never the blocking
``subprocess`` module) so clud's stdout/stderr stream back incrementally without
a blocking read — the same non-blocking-streaming rationale that clud's
``running-process`` rule encodes for its own process tree.
"""

from __future__ import annotations

import asyncio
import json
import os
import shlex
import shutil
import urllib.error
import urllib.request

from mcp.server.mcpserver import MCPServer
from mcp.server.mcpserver.context import Context

#: Upper bound on harness output returned to the client, in characters. Hermes'
#: own MCP client hard-caps a single result at 2,000,000 chars; stay well under
#: it. Log notifications are additionally capped so a chatty harness cannot
#: flood the client's log stream.
_RESULT_CHAR_CAP = 1_000_000
_LOG_LINE_CAP = 2_000

server = MCPServer(
    name="clud",
    title="clud — agent harness runner",
    description=(
        "Run the clud agent harness (Claude Code / Codex in YOLO mode) on a "
        "prompt and stream its output back."
    ),
)


def _build_argv(prompt: str, backend: str, model: str, extra_flags: str) -> list[str]:
    """Assemble the clud argv for a prompt run."""
    argv = [os.environ.get("CLUD_BIN", "clud"), "-p", prompt]
    if backend == "codex":
        argv.append("--codex")
    elif backend == "claude":
        argv.append("--claude")
    if model:
        argv.extend(("--model", model))
    if extra_flags:
        argv.extend(shlex.split(extra_flags))
    return argv


def _resolve_cwd(raw: str, default: str) -> str:
    """Expand ``~``/env vars and make a cwd absolute (relative to the bridge's cwd).

    The daemon's ``/v1/sessions`` contract only accepts an absolute, existing
    cwd, so a bare ``~`` or relative path must be normalised before it is sent.
    """
    value = (raw or "").strip()
    if not value:
        return default
    value = os.path.expandvars(os.path.expanduser(value))
    if not os.path.isabs(value):
        value = os.path.join(os.getcwd(), value)
    # Normalise separators, not just make it absolute. `~/work` and `rel/dir`
    # expand to `C:\\Users\\x/work` on Windows -- a mixed-separator path that
    # most APIs tolerate and no reader or comparison does. The docstring above
    # already promised a normalised value.
    return os.path.normpath(value)


async def _stream_and_capture(proc: asyncio.subprocess.Process, ctx: Context) -> str:
    """Pump stdout+stderr to the client and accumulate the full transcript."""
    parts: list[str] = []
    total = 0
    log_count = 0

    async def _pump(stream: asyncio.StreamReader | None, label: str) -> None:
        nonlocal total, log_count
        if stream is None:
            return
        while True:
            chunk = await stream.read(8192)
            if not chunk:
                break
            text = chunk.decode("utf-8", "replace")
            if total < _RESULT_CHAR_CAP:
                parts.append(text)
                total += len(text)
            for line in text.splitlines():
                if log_count >= _LOG_LINE_CAP:
                    return
                if line.strip():
                    log_count += 1
                    try:
                        await ctx.info(f"[{label}] {line}")
                    except Exception:  # logging must never kill a run
                        return

    await asyncio.gather(_pump(proc.stdout, "stdout"), _pump(proc.stderr, "stderr"))
    return "".join(parts)[:_RESULT_CHAR_CAP]


@server.tool()
async def run(
    prompt: str,
    ctx: Context,
    backend: str = "claude",
    cwd: str = "",
    model: str = "",
    timeout: int = 1800,
    extra_flags: str = "",
) -> str:
    """Run `clud -p <prompt>` and return the harness's streamed output.

    Args:
        prompt: The prompt passed to `clud -p`. This is a full autonomous agent
            run: clud launches Claude Code (default) or Codex in YOLO mode and
            lets it work on the prompt (e.g. "write a 4-paragraph song").
        backend: Which harness clud drives: "claude" (default) or "codex".
        cwd: Working directory for the run (defaults to clud's current dir).
        model: Optional model override passed through as `--model <model>`.
        timeout: Maximum seconds to wait for the run before terminating it.
        extra_flags: Optional space-separated extra clud flags to pass through.
    """
    clud_bin = os.environ.get("CLUD_BIN", "clud")
    if not os.path.isabs(clud_bin) and shutil.which(clud_bin) is None:
        return f"error: clud binary '{clud_bin}' not found on PATH"

    if backend not in ("claude", "codex"):
        return f"error: unknown backend '{backend}' (expected 'claude' or 'codex')"

    argv = _build_argv(prompt, backend, model, extra_flags)
    workdir = cwd or None
    if workdir is not None:
        workdir = _resolve_cwd(workdir, "")
        if not os.path.isdir(workdir):
            return f"error: cwd '{workdir}' is not a directory"

    try:
        await ctx.info(f"running: {shlex.join(argv)}")
    except Exception:
        pass

    try:
        proc = await asyncio.create_subprocess_exec(
            *argv,
            cwd=workdir,
            stdin=asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
    except FileNotFoundError:
        return f"error: could not start '{clud_bin}'"
    except OSError as exc:
        return f"error: failed to launch clud: {exc}"

    try:
        output = await asyncio.wait_for(_stream_and_capture(proc, ctx), timeout=timeout)
    except asyncio.TimeoutError:
        try:
            proc.kill()
        except ProcessLookupError:
            pass
        await proc.wait()
        return f"error: clud run exceeded {timeout}s timeout; process terminated"

    rc = await proc.wait()
    output += f"\n\n[clud exited with code {rc}]"
    return output


# ---------------------------------------------------------------------------
# JSON translation layer — clud's machine-readable surfaces.
#
# clud natively normalises its backend JSON into a typed loopback HTTP API
# (`clud daemon api-info --json` → base_url + bearer token). Claude Code runs
# headless with `--output-format stream-json` (NDJSON); Codex runs `codex exec
# --json`. The daemon captures those lines and re-emits them as bounded,
# cursor-addressable `Event` records. `run_json` drives that API end-to-end and
# hands Hermes a clean, structured result instead of a raw terminal stream.
# ---------------------------------------------------------------------------

_HTTP_TIMEOUT = 20  # seconds per HTTP round-trip


def _clud_bin() -> str:
    return os.environ.get("CLUD_BIN", "clud")


async def _discover_api() -> tuple[str, str]:
    """Return the daemon's (base_url, bearer_token) from `clud daemon api-info`."""
    proc = await asyncio.create_subprocess_exec(
        _clud_bin(), "daemon", "api-info", "--json",
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    out, err = await proc.communicate()
    if proc.returncode != 0:
        raise RuntimeError(f"clud daemon api-info failed: {err.decode().strip()}")
    try:
        doc = json.loads(out.decode())
        return doc["base_url"], doc["token"]
    except (KeyError, json.JSONDecodeError) as exc:
        raise RuntimeError(f"unexpected api-info payload: {exc}") from exc


def _http_json(base: str, token: str, method: str, path: str, body=None):
    """Blocking JSON HTTP call against the loopback daemon API."""
    req = urllib.request.Request(base + path, method=method)
    req.add_header("Authorization", "Bearer " + token)
    data = None
    if body is not None:
        req.add_header("Content-Type", "application/json")
        data = json.dumps(body).encode()
    try:
        with urllib.request.urlopen(req, data=data, timeout=_HTTP_TIMEOUT) as resp:
            raw = resp.read().decode()
            return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as exc:
        raw = exc.read().decode()
        try:
            return exc.code, json.loads(raw)
        except json.JSONDecodeError:
            return exc.code, {"error": raw}


def _extract_text(events: list) -> str:
    """Best-effort plain-text answer assembled from normalised backend JSONL."""
    parts: list[str] = []
    for ev in events:
        if ev.get("kind") != "raw_jsonl":
            continue
        data = ev.get("data")
        if not isinstance(data, dict):
            continue
        line = data.get("line")
        if not isinstance(line, str):
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        kind = obj.get("type")
        if kind == "result":
            res = obj.get("result")
            if isinstance(res, str):
                parts.append(res)
            elif isinstance(res, dict):
                parts.append(res.get("result") or res.get("text") or "")
        elif kind == "assistant":
            for item in (obj.get("message") or {}).get("content", []) or []:
                if isinstance(item, dict) and item.get("type") == "text":
                    parts.append(item.get("text", ""))
    return "\n".join(p for p in parts if p).strip()


@server.tool()
async def dry_run(
    prompt: str,
    ctx: Context,
    backend: str = "claude",
    model: str = "",
    extra_flags: str = "",
) -> str:
    """Resolve `clud -p <prompt>` to its LaunchPlan JSON without executing it.

    Args:
        prompt: The prompt that would be passed to `clud -p`.
        backend: "claude" (default) or "codex".
        model: Optional `--model` override.
        extra_flags: Optional space-separated extra clud flags.
    """
    clud_bin = _clud_bin()
    if not os.path.isabs(clud_bin) and shutil.which(clud_bin) is None:
        return f"error: clud binary '{clud_bin}' not found on PATH"
    if backend not in ("claude", "codex"):
        return f"error: unknown backend '{backend}' (expected 'claude' or 'codex')"
    argv = [clud_bin, "--dry-run", "-p", prompt]
    if backend == "codex":
        argv.append("--codex")
    if model:
        argv.extend(("--model", model))
    if extra_flags:
        argv.extend(shlex.split(extra_flags))
    proc = await asyncio.create_subprocess_exec(
        *argv,
        stdin=asyncio.subprocess.DEVNULL,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    out, err = await proc.communicate()
    raw = out.decode("utf-8", "replace")
    start, end = raw.find("{"), raw.rfind("}")
    if start != -1 and end > start:
        try:
            plan = json.loads(raw[start:end + 1])
            return json.dumps(plan, indent=2)
        except json.JSONDecodeError:
            pass
    return raw + (("\n" + err.decode("utf-8", "replace")) if err else "")


@server.tool()
async def run_json(
    prompt: str,
    ctx: Context,
    backend: str = "claude",
    cwd: str = "",
    model: str = "",
    safe: bool = False,
    timeout: int = 1800,
    resume_session_id: str = "",
) -> str:
    """Run a headless clud turn and return the translated, structured JSON.

    Drives clud's `/v1/sessions` HTTP API: creates (or resumes) a logical
    session, submits one turn, polls until it finishes, and returns the
    normalised JSON events plus a best-effort plain-text answer.

    Args:
        prompt: The task message for the turn.
        backend: "claude" (default) or "codex".
        cwd: Working directory for the run (defaults to the bridge's cwd).
        model: Optional model override persisted on the session.
        safe: If true, create the session in `--safe` (non-YOLO) mode.
        timeout: Maximum seconds to wait for the turn to finish.
        resume_session_id: Reuse an existing logical session instead of
            creating a new one (enables multi-turn conversations).
    """
    if backend not in ("claude", "codex"):
        return f"error: unknown backend '{backend}' (expected 'claude' or 'codex')"
    try:
        base, token = await _discover_api()
    except Exception as exc:  # discovery must never mask a usable run
        return f"error: could not discover clud daemon API: {exc}"

    workdir = _resolve_cwd(cwd, os.getcwd())
    if not os.path.isdir(workdir):
        return f"error: cwd '{workdir}' is not a directory"

    if resume_session_id:
        session_id = resume_session_id
    else:
        status, created = await asyncio.to_thread(
            _http_json, base, token, "POST", "/v1/sessions",
            {"backend": backend, "cwd": workdir, "model": model or None, "safe": safe},
        )
        if status != 201 or not isinstance(created, dict) or not created.get("id"):
            return f"error: create session failed ({status}): {created}"
        session_id = created["id"]

    status, turn = await asyncio.to_thread(
        _http_json, base, token, "POST", f"/v1/sessions/{session_id}/turns",
        {"message": prompt, "interrupt_running": False},
    )
    if status not in (200, 202) or not isinstance(turn, dict) or not turn.get("turn_id"):
        return f"error: submit turn failed ({status}): {turn}"
    turn_id = turn.get("turn_id")

    # Poll the durable record until the turn seals (state leaves running/
    # interrupting and current_turn_id clears) or the deadline passes.
    deadline = asyncio.get_event_loop().time() + timeout
    final_state = "unknown"
    while asyncio.get_event_loop().time() < deadline:
        await asyncio.sleep(1)
        status, record = await asyncio.to_thread(
            _http_json, base, token, "GET", f"/v1/sessions/{session_id}"
        )
        if not isinstance(record, dict):
            continue
        final_state = record.get("state", final_state)
        if final_state in ("idle", "failed", "terminated") and not record.get("current_turn_id"):
            break
    else:
        return (
            f"error: turn did not finish within {timeout}s "
            f"(last state '{final_state}'); session_id={session_id}, turn_id={turn_id}"
        )

    status, events_payload = await asyncio.to_thread(
        _http_json, base, token, "GET", f"/v1/sessions/{session_id}/events"
    )
    events = (events_payload or {}).get("events", []) if isinstance(events_payload, dict) else []
    result = {
        "session_id": session_id,
        "turn_id": turn_id,
        "state": final_state,
        "text": _extract_text(events),
        "events": events,
    }
    return json.dumps(result, indent=2)


def main() -> None:
    """Run the bridge over stdio until the client disconnects."""
    server.run("stdio")


if __name__ == "__main__":
    main()
