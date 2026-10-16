"""Refresh the shell-AST recall oracle fixture from real agent sessions (#1962).

The PreToolUse scanner predicts, from a Bash command's text, which ``git`` and
``gh`` calls it will run. The git/gh shim records what actually ran
(``~/.clud/state/logs/shim/git-gh.jsonl``, #1486). Pairing each Bash tool call
in the Claude Code transcripts (``~/.claude/projects/*/*.jsonl``) with the shim
lines its shell produced gives ground truth for that prediction. The Rust test
``shell_ast::oracle_tests`` replays the fixture and fails when recall drops
below the recorded floor.

Pairing rules:

* a shim line belongs to a tool call when it was recorded between the call's
  ``tool_use`` and ``tool_result`` with ``parent=bash``, in the clud session
  that dominates that Claude session's windows;
* a recorded call whose subcommand never appears in the command text was run
  by a script the command launched, not by the command, so it is not text a
  parser could predict and is dropped.

Privacy: the fixture is committed, so every identifier that is not shell or
git/gh vocabulary (program names, subcommands, flag names, keywords) is
replaced by a stable ``tN`` token, consistently across a record's command and
its recorded argv. Records naming an email address or anything that looks like
a credential are dropped, and so are commands over 800 characters.

Run by hand: ``python -m ci.refresh_shell_ast_oracle``. Stdlib only.
"""

from __future__ import annotations

import argparse
import bisect
import collections
import datetime
import json
import random
import re
import sys
from collections.abc import Iterator
from pathlib import Path
from typing import Any

FIXTURE = (
    Path(__file__).resolve().parent.parent
    / "crates/clud-bin/src/shell_ast/oracle_fixtures/shim_oracle.json"
)
RECORDS = 200
SEED = 1962
MAX_COMMAND = 800
MAX_CALL_MS = 600_000

SECRET = re.compile(
    r"(token|password|passwd|secret|api[_-]?key|ghp_|github_pat|authorization|bearer)", re.I
)
VOCABULARY = set(
    """if then else elif fi for in do done while until case esac function select time true false
echo printf cd pushd popd export unset local read eval exec command env sudo nohup timeout xargs
test exit return set shift source break continue wait trap kill sleep cat head tail grep rg sed
awk jq tr cut sort uniq wc tee ls find mkdir cp mv rm touch chmod ln stat diff python uv bash sh
zsh git gh cargo soldr clud bosn docker make HOME PWD USER PATH OLDPWD TMPDIR CLUD_EXE home user
dev tmp null stdout stderr""".split()
)
IDENTIFIER = re.compile(r"[A-Za-z][A-Za-z0-9_]*")
FLAG = re.compile(r"""(?<![^\s'"=])--?[A-Za-z][A-Za-z0-9-]{0,30}(?=[\s='"]|$)""")
STAGE_HEAD = re.compile(r"\s*(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*([A-Za-z][A-Za-z0-9_.\-]*)(?=\s|$)")


def timestamp_ms(iso: str) -> int:
    return int(datetime.datetime.fromisoformat(iso.replace("Z", "+00:00")).timestamp() * 1000)


def read_jsonl(path: Path) -> Iterator[dict[str, Any]]:
    with path.open(encoding="utf-8", errors="replace") as handle:
        for line in handle:
            try:
                yield json.loads(line)
            except json.JSONDecodeError:
                continue


def shim_lines(log_dir: Path) -> list[dict[str, Any]]:
    lines = []
    for name in ("git-gh.jsonl.1", "git-gh.jsonl"):
        path = log_dir / name
        if path.exists():
            lines.extend(r for r in read_jsonl(path) if r.get("parent") == "bash")
    return sorted(lines, key=lambda r: r["ts_ms"])


def tool_events(entry: dict[str, Any]) -> Iterator[dict[str, Any]]:
    content = (entry.get("message") or {}).get("content")
    if isinstance(content, list) and "timestamp" in entry:
        yield from (c for c in content if isinstance(c, dict))


def transcript_calls(path: Path) -> list[tuple[int, int, str, str, str]]:
    """``(start_ms, end_ms, command, cwd, session)`` for each Bash call."""
    uses: dict[str, tuple[int, str, str, str]] = {}
    ends: dict[str, int] = {}
    for entry in read_jsonl(path):
        for event in tool_events(entry):
            at = timestamp_ms(entry["timestamp"])
            if event.get("type") == "tool_use" and event.get("name") == "Bash":
                command = (event.get("input") or {}).get("command", "")
                uses[event["id"]] = (at, command, entry.get("cwd", ""), entry.get("sessionId", ""))
            elif event.get("type") == "tool_result":
                ends[event.get("tool_use_id", "")] = at
    return [
        (start, ends[key], command, cwd, session)
        for key, (start, command, cwd, session) in uses.items()
        if key in ends and 0 <= ends[key] - start < MAX_CALL_MS
    ]


class Shim:
    """The shim lines, indexed by time."""

    def __init__(self, lines: list[dict[str, Any]]) -> None:
        self.lines = lines
        self.keys = [line["ts_ms"] for line in lines]

    def window(self, start: int, end: int) -> list[dict[str, Any]]:
        return self.lines[bisect.bisect_left(self.keys, start) : bisect.bisect_right(self.keys, end)]


def session_owners(calls: list[tuple[int, int, str, str, str]], shim: Shim) -> dict[str, str]:
    votes: dict[str, collections.Counter[str]] = collections.defaultdict(collections.Counter)
    for start, end, _, _, session in calls:
        for line in shim.window(start, end):
            votes[session][line.get("session_id") or ""] += 1
    return {session: count.most_common(1)[0][0] for session, count in votes.items() if count}


def subcommand(argv: list[str]) -> str | None:
    return next((word for word in argv if not word.startswith("-")), None)


def recorded_calls(command: str, lines: list[dict[str, Any]], owner: str) -> list[dict[str, Any]]:
    calls = []
    for line in lines:
        sub = subcommand(line["argv"])
        if line.get("session_id") != owner or sub is None or sub not in command:
            continue
        if SECRET.search(" ".join(line["argv"])):
            continue
        calls.append({"tool": line["tool"], "argv": line["argv"]})
    return calls


def pair(projects: Path, shim: Shim) -> list[dict[str, Any]]:
    if not shim.lines:
        return []
    start, end = shim.keys[0], shim.keys[-1]
    calls = []
    for path in projects.glob("*/*.jsonl"):
        if path.stat().st_mtime * 1000 >= start:
            calls.extend(c for c in transcript_calls(path) if start <= c[0] <= end)
    owners = session_owners(calls, shim)
    records = []
    for first, last, command, cwd, session in calls:
        if not re.search(r"\b(git|gh)\b", command) or SECRET.search(command):
            continue
        if len(command) > MAX_COMMAND or "@" in command:
            continue
        found = recorded_calls(command, shim.window(first, last), owners.get(session, ""))
        if found and not any("@" in " ".join(c["argv"]) for c in found):
            records.append({"command": command, "cwd": cwd, "calls": found})
    return records


class Pseudonyms:
    """Replace every non-vocabulary identifier with a stable ``tN`` token."""

    def __init__(self, records: list[dict[str, Any]]) -> None:
        self.vocabulary = set(VOCABULARY)
        self.names: dict[str, str] = {}
        for record in records:
            for stage in re.split(r"[;&|()`\n]|\$\(", record["command"]):
                match = STAGE_HEAD.match(stage)
                if match:
                    self.vocabulary.add(match.group(1))
            for call in record["calls"]:
                self.vocabulary.update([w for w in call["argv"] if not w.startswith("-")][:2])

    def _rename(self, match: re.Match[str]) -> str:
        word = match.group(0)
        if word in self.vocabulary or len(word) <= 1:
            return word
        return self.names.setdefault(word, f"t{len(self.names) + 1}")

    def text(self, text: str) -> str:
        out, at = [], 0
        for flag in FLAG.finditer(text):
            out.append(IDENTIFIER.sub(self._rename, text[at : flag.start()]))
            out.append(flag.group(0))
            at = flag.end()
        out.append(IDENTIFIER.sub(self._rename, text[at:]))
        return "".join(out)


def scrub(records: list[dict[str, Any]]) -> list[dict[str, Any]]:
    names = Pseudonyms(records)
    return [
        {
            "command": names.text(record["command"]),
            "cwd": names.text(record["cwd"]),
            "calls": [
                {"tool": call["tool"], "argv": [names.text(word) for word in call["argv"]]}
                for call in record["calls"]
            ],
        }
        for record in records
    ]


def write_fixture(records: list[dict[str, Any]], floor: float, path: Path) -> None:
    body = ",\n".join(json.dumps(record, ensure_ascii=False) for record in records)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        f'{{"floor": {floor}, "records": [\n{body}\n]}}\n', encoding="utf-8"
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--projects", type=Path, default=Path.home() / ".claude/projects")
    parser.add_argument("--shim-logs", type=Path, default=Path.home() / ".clud/state/logs/shim")
    parser.add_argument("--floor", type=float, default=0.0)
    parser.add_argument("--out", type=Path, default=FIXTURE)
    args = parser.parse_args(argv)
    records = pair(args.projects, Shim(shim_lines(args.shim_logs)))
    if not records:
        print("no paired Bash calls found; is the shim log populated?", file=sys.stderr)
        return 1
    random.seed(SEED)
    sample = sorted(random.sample(records, min(RECORDS, len(records))), key=lambda r: r["command"])
    write_fixture(scrub(sample), args.floor, args.out)
    print(f"wrote {len(sample)} records to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
