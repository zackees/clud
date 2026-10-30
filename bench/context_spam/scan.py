"""Measure context spam from Bash tool output in local Claude Code transcripts.

Joins each Bash ``tool_use`` with its ``tool_result`` and buckets commands into
families (``gh pr checks``, ``soldr cargo test``, ...). Two reports:

- ``families``: calls and bytes per family, plus the largest example.
- ``polls``: for commands repeated within one transcript, the share of each
  result's bytes identical to the previous result of the same command. This
  is the re-measurement #2312 asks for after its skill-text change.

Local diagnostic only: ``polls`` prints truncated command text.
"""

from __future__ import annotations

import argparse
import json
import os
import re
from collections import Counter, defaultdict
from collections.abc import Iterator
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path

FAMILIES = [
    ("gh run list", r"gh run list\b"),
    ("gh run view", r"gh run view\b"),
    ("gh run watch", r"gh run watch\b"),
    ("gh pr checks", r"gh pr checks\b"),
    ("gh pr view", r"gh pr view\b"),
    ("gh pr list", r"gh pr list\b"),
    ("gh issue", r"gh issue\b"),
    ("gh api", r"gh api\b"),
    ("gh other", r"gh\s"),
    ("pr_merge_watch", r"clud tool run github/pr_merge_watch"),
    ("clud ci", r"clud ci\b"),
    ("bosn", r"bosn\b"),
    ("soldr test", r"soldr\s+(cargo\s+)?(test|nextest|ci-test)\b"),
    ("soldr clippy", r"soldr\s+(cargo\s+clippy|lint)\b"),
    ("soldr build", r"soldr\s+(cargo\s+)?(build|check)\b"),
    ("soldr fmt", r"soldr\s+(cargo\s+fmt|rustfmt)\b"),
    ("soldr other", r"soldr\b"),
    ("bash lint", r"(bash lint|\./lint)\b"),
    ("bash test", r"(bash test|\./test)\b"),
    ("git", r"git\b"),
]
FAMILY_RES = [(name, re.compile(pattern)) for name, pattern in FAMILIES]
GH_POLL_FAMILIES = r"^(gh run (view|list|watch)|gh pr (checks|view|list)|gh api|pr_merge_watch)$"

ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")
SEGMENT = re.compile(r"[;&|\n(]+|\$\(")
LEADING = re.compile(r"^\s*((cd|timeout|env)\s+\S+\s+|[A-Z_]+=\S*\s+|!\s*)*")


def family(command: str) -> str | None:
    """The family of the first shell segment that starts with a known command."""
    for segment in SEGMENT.split(command):
        segment = LEADING.sub("", segment).strip()
        for name, pattern in FAMILY_RES:
            if pattern.match(segment):
                return name
    return None


def normalize(line: str) -> str:
    """Collapse hashes, paths and numbers so repeated shapes compare equal."""
    line = ANSI.sub("", line).strip()
    line = re.sub(r"\b[0-9a-f]{7,40}\b", "H", line)
    line = re.sub(r"(/[\w.@+-]+){2,}", "/P", line)
    line = re.sub(r"\d+(\.\d+)*", "N", line)
    return re.sub(r"\s+", " ", line)[:160]


def result_text(content: object) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(c.get("text", "") for c in content if isinstance(c, dict))
    return ""


SINCE: float = 0.0  # set from --since: skip transcripts last written before it


def transcripts(paths: list[str]) -> Iterator[Path]:
    for raw in paths:
        path = Path(raw).expanduser()
        for found in sorted(path.rglob("*.jsonl")) if path.is_dir() else [path]:
            if not SINCE or found.stat().st_mtime >= SINCE:
                yield found


def content_blocks(path: Path) -> Iterator[dict]:
    try:
        lines = path.open(encoding="utf-8", errors="replace")
    except OSError:
        return
    with lines:
        for raw in lines:
            try:
                event = json.loads(raw)
            except ValueError:
                continue
            content = (event.get("message") or {}) if isinstance(event, dict) else {}
            blocks = content.get("content") if isinstance(content, dict) else None
            if isinstance(blocks, list):
                yield from (b for b in blocks if isinstance(b, dict))


def bash_calls(path: Path) -> Iterator[tuple[str, str, str]]:
    """Yield ``(family, command, output)`` for each classified Bash call."""
    pending: dict[str, tuple[str, str]] = {}
    for block in content_blocks(path):
        if block.get("type") == "tool_use" and block.get("name") == "Bash":
            command = (block.get("input") or {}).get("command", "")
            name = family(command)
            if name:
                pending[block.get("id", "")] = (name, command)
        elif block.get("type") == "tool_result" and block.get("tool_use_id") in pending:
            name, command = pending.pop(block["tool_use_id"])
            yield name, command, ANSI.sub("", result_text(block.get("content")))


@dataclass
class FamilyStats:
    calls: int = 0
    bytes: int = 0
    largest: int = 0
    example: str = ""


def report_families(paths: list[str]) -> dict[str, FamilyStats]:
    stats: dict[str, FamilyStats] = defaultdict(FamilyStats)
    for path in transcripts(paths):
        for name, command, output in bash_calls(path):
            entry = stats[name]
            entry.calls += 1
            entry.bytes += len(output)
            if len(output) > entry.largest:
                entry.largest, entry.example = len(output), command[:100]
    return stats


@dataclass
class PollGroup:
    transcript: str
    family: str
    command: str
    outputs: list[str] = field(default_factory=list)

    def redundant_bytes(self) -> tuple[int, int]:
        """Bytes of repeat results, and how many match the previous result's lines."""
        repeated = redundant = 0
        for previous, current in zip(self.outputs, self.outputs[1:], strict=False):
            seen = set(previous.splitlines())
            for line in current.splitlines():
                repeated += len(line) + 1
                redundant += len(line) + 1 if line in seen else 0
        return repeated, redundant


def poll_groups(paths: list[str], families: re.Pattern[str]) -> list[PollGroup]:
    groups: dict[tuple[str, str, str], PollGroup] = {}
    for path in transcripts(paths):
        for name, command, output in bash_calls(path):
            if not families.search(name):
                continue
            key = (str(path), name, normalize(command)[:120])
            group = groups.setdefault(key, PollGroup(str(path), name, command[:100]))
            group.outputs.append(output)
    return list(groups.values())


def print_families(stats: dict[str, FamilyStats]) -> None:
    print(f"{'family':16}{'calls':>8}{'MB':>8}{'max KB':>8}  largest example")
    for name, s in sorted(stats.items(), key=lambda kv: -kv[1].bytes):
        print(f"{name:16}{s.calls:8}{s.bytes / 1e6:8.2f}{s.largest / 1e3:8.1f}  {s.example!r}")


def print_polls(groups: list[PollGroup], minimum: int, top: int) -> None:
    totals: dict[str, Counter[str]] = defaultdict(Counter)
    rows = []
    for group in groups:
        total = totals[group.family]
        total["calls"] += len(group.outputs)
        total["bytes"] += sum(map(len, group.outputs))
        if len(group.outputs) < minimum:
            continue
        repeated, redundant = group.redundant_bytes()
        total["redundant"] += redundant
        rows.append((redundant, repeated, group))
    print(f"{'family':16}{'calls':>8}{'KB':>9}{'redundant KB':>14}")
    for name, total in sorted(totals.items(), key=lambda kv: -kv[1]["bytes"]):
        kb, redundant_kb = total["bytes"] / 1e3, total["redundant"] / 1e3
        print(f"{name:16}{total['calls']:8}{kb:9.0f}{redundant_kb:14.0f}")
    print(f"\nworst groups repeated >= {minimum}x, by redundant bytes:")
    for redundant, repeated, group in sorted(rows, key=lambda r: -r[0])[:top]:
        share = redundant / repeated if repeated else 0.0
        name = os.path.basename(group.transcript)[:13]
        count = len(group.outputs)
        print(f"{name} {group.family:14} x{count:<4} dup={share:4.0%} {group.command!r}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("report", choices=["families", "polls"])
    parser.add_argument("paths", nargs="*", default=["~/.claude/projects"])
    parser.add_argument("--families", default=GH_POLL_FAMILIES, help="polls: family regex")
    parser.add_argument("--min", type=int, default=3, help="polls: minimum repeats")
    parser.add_argument("--top", type=int, default=15)
    parser.add_argument("--since", help="only transcripts written on/after YYYY-MM-DD")
    args = parser.parse_args(argv)
    global SINCE
    SINCE = datetime.fromisoformat(args.since).timestamp() if args.since else 0.0
    if args.report == "families":
        print_families(report_families(args.paths))
    else:
        print_polls(poll_groups(args.paths, re.compile(args.families)), args.min, args.top)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
