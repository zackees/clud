#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# managed-by: clud
"""transcript_report.py — privacy-preserving report on a Claude session transcript.

Usage:
  transcript_report.py <transcript.jsonl> [--json] [--burst-threshold N]
                       [--jump-threshold TOKENS]

Reads one Claude Code session transcript (JSONL) and reports what drove its
context usage (issue #1276):

* responses: rows collapsed by assistant ``message.id``. Claude Code repeats
  a response's ``usage`` on every row it splits that response into, so the
  output tokens are counted once per message id, not once per row.
* repeated tool-call bursts: identical tool calls (same tool name and the
  same input) inside one response, with the total copy count and the
  longest consecutive streak.
* compaction boundaries (``compact_boundary`` system rows), context jumps
  between consecutive responses, and terminal context errors.
* tool-result byte totals and the largest single result.
* the effective max-context setting, which a transcript does not record; the
  field is reported as unavailable instead of being guessed.

Privacy: the report never contains prompts, commands, tool inputs, tool
output or the session id. Tool calls are identified by tool name plus a
fingerprint keyed with a random per-invocation salt, so a fingerprint can
only be compared within one report. The tool writes no files.

Run through clud's `tool run` subcommand:
  "$CLUD_EXE" tool run diagnostics/transcript_report.py ~/.claude/projects/<p>/<id>.jsonl

Exit codes:
  0  report printed
  1  usage error, or the transcript could not be read
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import json
import secrets
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

DEFAULT_BURST_THRESHOLD = 20
DEFAULT_JUMP_THRESHOLD = 50_000

# Lower-cased markers of a terminal context failure. Only the marker name is
# reported, never the surrounding text.
CONTEXT_ERROR_MARKERS = (
    ("autocompact is thrashing", "autocompact_thrashing"),
    ("context limit reached", "context_limit_reached"),
    ("prompt is too long", "prompt_too_long"),
    ("context window exceeded", "context_window_exceeded"),
)


@dataclass
class Response:
    message_id_fp: str
    first_ts: str | None
    last_ts: str | None
    output_tokens: int = 0
    input_tokens: int = 0
    cache_read_tokens: int = 0
    cache_creation_tokens: int = 0
    stop_reason: str | None = None
    calls: list[tuple[str, str]] = field(default_factory=list)

    @property
    def context_tokens(self) -> int:
        return self.input_tokens + self.cache_read_tokens + self.cache_creation_tokens


def _fingerprint(salt: bytes, value: str) -> str:
    return hmac.new(salt, value.encode("utf-8"), hashlib.sha256).hexdigest()[:12]


def _int(v: Any) -> int:
    return v if isinstance(v, int) and not isinstance(v, bool) else 0


def _text_of(obj: Any) -> str:
    """Flatten the text in a row's message content (for marker matching only)."""
    if isinstance(obj, str):
        return obj
    if isinstance(obj, list):
        return " ".join(_text_of(x) for x in obj)
    if isinstance(obj, dict):
        parts = []
        for key in ("text", "content", "message"):
            if key in obj:
                parts.append(_text_of(obj[key]))
        return " ".join(parts)
    return ""


def _result_bytes(block: dict[str, Any]) -> int:
    content = block.get("content")
    if isinstance(content, str):
        return len(content.encode("utf-8"))
    if isinstance(content, list):
        return sum(len(_text_of(c).encode("utf-8")) for c in content)
    return 0


def analyze(
    rows: list[dict[str, Any]],
    *,
    burst_threshold: int = DEFAULT_BURST_THRESHOLD,
    jump_threshold: int = DEFAULT_JUMP_THRESHOLD,
    salt: bytes | None = None,
) -> dict[str, Any]:
    salt = salt if salt is not None else secrets.token_bytes(16)
    responses: dict[str, Response] = {}
    order: list[str] = []
    compactions: list[dict[str, Any]] = []
    errors: list[dict[str, Any]] = []
    result_count = 0
    result_total = 0
    result_max = 0
    malformed = 0

    for row in rows:
        if not isinstance(row, dict):
            malformed += 1
            continue
        ts = row.get("timestamp") if isinstance(row.get("timestamp"), str) else None
        rtype = row.get("type")
        msg = row.get("message") if isinstance(row.get("message"), dict) else {}

        if rtype == "system" and row.get("subtype") == "compact_boundary":
            meta = row.get("compactMetadata")
            meta = meta if isinstance(meta, dict) else {}
            trigger = meta.get("trigger")
            compactions.append(
                {
                    "timestamp": ts,
                    "trigger": trigger if isinstance(trigger, str) else None,
                    "pre_tokens": _int(meta.get("preTokens")) or None,
                }
            )
            continue

        haystack = (_text_of(row.get("content")) + " " + _text_of(msg.get("content"))).lower()
        for needle, kind in CONTEXT_ERROR_MARKERS:
            if needle in haystack and rtype != "user":
                errors.append({"timestamp": ts, "kind": kind})

        if rtype == "assistant":
            mid = msg.get("id")
            if not isinstance(mid, str) or not mid:
                malformed += 1
                continue
            resp = responses.get(mid)
            if resp is None:
                resp = Response(_fingerprint(salt, "msg:" + mid), ts, ts)
                responses[mid] = resp
                order.append(mid)
            resp.last_ts = ts or resp.last_ts
            usage = msg.get("usage") if isinstance(msg.get("usage"), dict) else {}
            # Usage repeats on every row of one response: keep the maximum,
            # never the sum.
            resp.output_tokens = max(resp.output_tokens, _int(usage.get("output_tokens")))
            resp.input_tokens = max(resp.input_tokens, _int(usage.get("input_tokens")))
            resp.cache_read_tokens = max(
                resp.cache_read_tokens, _int(usage.get("cache_read_input_tokens"))
            )
            resp.cache_creation_tokens = max(
                resp.cache_creation_tokens, _int(usage.get("cache_creation_input_tokens"))
            )
            if isinstance(msg.get("stop_reason"), str):
                resp.stop_reason = msg["stop_reason"]
            content = msg.get("content")
            for block in content if isinstance(content, list) else []:
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    name = block.get("name") if isinstance(block.get("name"), str) else "?"
                    canon = json.dumps(block.get("input"), sort_keys=True, default=str)
                    resp.calls.append((name, _fingerprint(salt, name + "\0" + canon)))
        elif rtype == "user":
            content = msg.get("content")
            for block in content if isinstance(content, list) else []:
                if isinstance(block, dict) and block.get("type") == "tool_result":
                    n = _result_bytes(block)
                    result_count += 1
                    result_total += n
                    result_max = max(result_max, n)

    resp_out = []
    bursts = []
    jumps = []
    prev_ctx: int | None = None
    for mid in order:
        r = responses[mid]
        counts: dict[tuple[str, str], int] = {}
        longest: dict[tuple[str, str], int] = {}
        run_key, run_len = None, 0
        for call in r.calls:
            counts[call] = counts.get(call, 0) + 1
            run_len = run_len + 1 if call == run_key else 1
            run_key = call
            longest[call] = max(longest.get(call, 0), run_len)
        for (name, fp), n in sorted(counts.items(), key=lambda kv: -kv[1]):
            if n >= burst_threshold:
                bursts.append(
                    {
                        "response": r.message_id_fp,
                        "tool": name,
                        "call_fingerprint": fp,
                        "copies": n,
                        "longest_streak": longest[(name, fp)],
                        "output_tokens": r.output_tokens,
                        "first_timestamp": r.first_ts,
                        "last_timestamp": r.last_ts,
                    }
                )
        ctx = r.context_tokens
        if prev_ctx is not None and ctx - prev_ctx >= jump_threshold:
            jumps.append(
                {"response": r.message_id_fp, "timestamp": r.first_ts, "from": prev_ctx, "to": ctx}
            )
        if ctx:
            prev_ctx = ctx
        resp_out.append(
            {
                "response": r.message_id_fp,
                "first_timestamp": r.first_ts,
                "output_tokens": r.output_tokens,
                "context_tokens": ctx,
                "stop_reason": r.stop_reason,
                "tool_calls": len(r.calls),
            }
        )

    return {
        "rows": len(rows),
        "malformed_rows": malformed,
        "responses": len(order),
        "output_tokens_total": sum(r.output_tokens for r in responses.values()),
        "max_output_tokens_single_response": max(
            (r.output_tokens for r in responses.values()), default=0
        ),
        "tool_calls_total": sum(len(r.calls) for r in responses.values()),
        "repeated_call_bursts": bursts,
        "compactions": compactions,
        "context_jumps": jumps,
        "context_errors": errors,
        "tool_results": {
            "count": result_count,
            "total_bytes": result_total,
            "max_bytes": result_max,
        },
        "max_context_tokens": {
            "value": None,
            "source": "unavailable: a transcript does not record the child environment",
        },
        "per_response": resp_out,
    }


def render_text(report: dict[str, Any]) -> str:
    lines = [
        f"rows: {report['rows']} (malformed: {report['malformed_rows']})",
        f"responses (by message id): {report['responses']}",
        f"output tokens (once per response): {report['output_tokens_total']}"
        f" (max single response: {report['max_output_tokens_single_response']})",
        f"tool calls: {report['tool_calls_total']}",
        f"tool results: {report['tool_results']['count']} totaling"
        f" {report['tool_results']['total_bytes']} bytes"
        f" (largest {report['tool_results']['max_bytes']})",
        f"max context: {report['max_context_tokens']['source']}",
        f"repeated tool-call bursts: {len(report['repeated_call_bursts'])}",
    ]
    for b in report["repeated_call_bursts"]:
        lines.append(
            f"  {b['copies']} x {b['tool']} [{b['call_fingerprint']}] in response"
            f" {b['response']} (streak {b['longest_streak']}, {b['output_tokens']} output"
            f" tokens, {b['first_timestamp']} .. {b['last_timestamp']})"
        )
    lines.append(f"compaction boundaries: {len(report['compactions'])}")
    for c in report["compactions"]:
        lines.append(f"  {c['timestamp']} trigger={c['trigger']} pre_tokens={c['pre_tokens']}")
    lines.append(f"context jumps: {len(report['context_jumps'])}")
    for j in report["context_jumps"]:
        lines.append(f"  {j['timestamp']} {j['from']} -> {j['to']}")
    lines.append(f"context errors: {len(report['context_errors'])}")
    for e in report["context_errors"]:
        lines.append(f"  {e['timestamp']} {e['kind']}")
    return "\n".join(lines)


def load_rows(path: Path) -> list[Any]:
    rows: list[Any] = []
    with path.open("r", encoding="utf-8", errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError:
                rows.append(None)
    return rows


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("transcript", type=Path)
    parser.add_argument("--json", action="store_true", help="print the report as JSON")
    parser.add_argument("--burst-threshold", type=int, default=DEFAULT_BURST_THRESHOLD)
    parser.add_argument("--jump-threshold", type=int, default=DEFAULT_JUMP_THRESHOLD)
    try:
        args = parser.parse_args(argv)
    except SystemExit as exc:
        return 0 if exc.code == 0 else 1
    try:
        rows = load_rows(args.transcript)
    except OSError as exc:
        print(f"cannot read transcript: {exc.strerror}", file=sys.stderr)
        return 1
    report = analyze(
        rows, burst_threshold=max(2, args.burst_threshold), jump_threshold=args.jump_threshold
    )
    print(json.dumps(report, indent=2) if args.json else render_text(report))
    return 0


if __name__ == "__main__":
    sys.exit(main())
