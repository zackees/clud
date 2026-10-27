#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "running-process==4.10.1",
# ]
# ///

# managed-by: clud
"""issue_context.py — bounded, read-only GitHub issue context for loop agents (#2229).

An agent investigating an issue otherwise runs `gh issue view --json
body,comments` and hand-slices each comment. One such session made 142 Bash
calls containing `gh issue view`, and a single call can print an issue body
plus every comment -- hundreds of KB that then ride along in the transcript
until compaction. This tool is the bounded, discoverable read path.

It fetches an issue and its comments once, saves the complete snapshot under
`~/.clud/logs/issue-context/<owner>__<repo>/` (never in the checkout), and
prints a byte-bounded view: metadata, a body excerpt, one excerpt per comment,
and explicit omission counts plus continuation instructions. Nothing is hidden
silently: the snapshot holds every byte, and `--comment <id>` prints one
comment in full. It is strictly read-only -- it performs no write or comment
operation (only `gh api` GETs).

Usage:
  "$CLUD_EXE" tool run github/issue_context.py <issue> [--repo OWNER/REPO]
      [--max-bytes N] [--comment ID] [--refresh] [--json]

<issue> is an issue number or a https://github.com/OWNER/REPO/issues/N URL
(the URL supplies the repo). Without --repo or a URL, the repo is the one
`git remote get-url origin` names.

Exit codes:
  0  answered (the view, or the requested comment)
  1  usage error
  2  a gh call failed (nonzero exit, timeout, unparseable JSON); nothing
     useful is printed on stdout, so a failure never reads as a clean read
  3  the issue or the requested comment does not exist (404 / not in the
     snapshot); the diagnostic names which, so it never reads as "empty"
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import time
from dataclasses import dataclass
from pathlib import Path

from running_process import PIPE, RunningProcess, TimeoutExpired

EXIT_OK = 0
EXIT_USAGE = 1
EXIT_GH_FAILED = 2
EXIT_NOT_FOUND = 3

GH_TIMEOUT = 60.0
# Bounded stdout (#1676, docs/architecture/bounded-output.md): a result over
# STDOUT_CAP_BYTES prints its head, a truncation notice, and an artifact path.
STDOUT_CAP_BYTES = 32 * 1024
# Default summary size. The CUSTOM view is bounded by --max-bytes; a --json
# caller may raise it, but stdout stays under STDOUT_CAP_BYTES via emit_bounded.
DEFAULT_MAX_BYTES = 24_000
# A fresh snapshot is reused without a GitHub read for this long.
DEFAULT_TTL_SEC = 300
PER_COMMENT_EXCERPT_BYTES = 400
BODY_EXCERPT_BYTES = 4_000
SCHEMA_VERSION = 1

_ISSUE_URL = re.compile(r"^https?://github\.com/([\w.-]+)/([\w.-]+)/issues/(\d+)/?(?:[?#].*)?$")
_GITHUB_REMOTE = re.compile(
    r"^(?:[a-z][a-z0-9+.-]*://)?(?:[^@/\s]+@)?github\.com(?::\d+)?[:/]"
    r"([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+?)(?:\.git)?/?$",
    re.IGNORECASE,
)


class GhError(Exception):
    """A gh call failed; the message is user-facing."""


class UsageError(Exception):
    """The command line was invalid."""


class NotFoundError(Exception):
    """The requested issue or comment does not exist."""


@dataclass
class GhResult:
    exit_code: int
    stdout: str
    stderr: str

    @property
    def ok(self) -> bool:
        return self.exit_code == 0


def gh(*args: str, timeout: float | None = GH_TIMEOUT) -> GhResult:
    """Run gh with the supplied args; return the captured outcome."""
    try:
        # stderr=PIPE: running-process merges stderr into stdout otherwise (#1175).
        res = RunningProcess.run(
            ["gh", *args], capture_output=True, stderr=PIPE, text=True, timeout=timeout
        )
    except (TimeoutError, TimeoutExpired):
        return GhResult(124, "", f"gh {' '.join(args)} timed out after {timeout}s")
    except OSError as exc:
        return GhResult(127, "", f"could not start gh: {exc}")
    return GhResult(res.returncode, res.stdout or "", res.stderr or "")


def _tool_output_dir() -> Path | None:
    override = os.environ.get("CLUD_TOOL_OUTPUT_DIR")
    if override:
        return Path(override)
    home = os.environ.get("USERPROFILE") or os.environ.get("HOME")
    return Path(home) / ".clud" / "tmp" / "tool-output" if home else None


def _state_root() -> Path:
    home = os.environ.get("USERPROFILE") or os.environ.get("HOME")
    return Path(home) / ".clud" if home else Path(".clud")


def emit_bounded(text: str, name: str, cap: int = STDOUT_CAP_BYTES) -> None:
    data = text.encode("utf-8")
    if len(data) <= cap:
        print(text)
        return
    try:
        out_dir = _tool_output_dir()
        if out_dir is None:
            raise OSError("no home directory")
        out_dir.mkdir(parents=True, exist_ok=True)
        stamp = time.strftime("%Y%m%dT%H%M%S")
        artifact = out_dir / f"{name}-{stamp}-{os.getpid()}.txt"
        artifact.write_bytes(data)
        where = f"full output: {artifact}"
    except OSError as exc:
        where = f"full output could not be saved ({exc})"
    print(data[:cap].decode("utf-8", "ignore"))
    print(f"[TRUNCATED: showed {cap} of {len(data)} bytes; {where}]")


def _to_str(value: object) -> str:
    return "" if value is None else str(value)


def _json_docs(text: str, what: str) -> list[object]:
    """Parse one or more concatenated JSON documents (`gh api --paginate`)."""
    decoder = json.JSONDecoder()
    docs: list[object] = []
    idx = 0
    stripped = text.strip()
    try:
        while idx < len(stripped):
            doc, end = decoder.raw_decode(stripped, idx)
            docs.append(doc)
            idx = end
            while idx < len(stripped) and stripped[idx].isspace():
                idx += 1
    except json.JSONDecodeError as exc:
        raise GhError(f"could not parse {what} JSON from gh: {exc}") from exc
    if not docs:
        raise GhError(f"gh returned no {what} JSON")
    return docs


def parse_issue(text: str) -> dict:
    docs = _json_docs(text, "issue")
    issue = docs[0]
    if not isinstance(issue, dict) or "number" not in issue:
        raise GhError("unexpected issue JSON shape from gh")
    labels = issue.get("labels")
    return {
        "number": int(issue["number"]),
        "title": _to_str(issue.get("title")),
        "state": _to_str(issue.get("state")),
        "state_reason": _to_str(issue.get("state_reason")),
        "author": _to_str((issue.get("user") or {}).get("login")),
        "labels": (
            [lab.get("name") for lab in labels if isinstance(lab, dict)]
            if isinstance(labels, list)
            else []
        ),
        "created_at": _to_str(issue.get("created_at")),
        "updated_at": _to_str(issue.get("updated_at")),
        "comment_count": int(issue.get("comments") or 0),
        "html_url": _to_str(issue.get("html_url")),
        "body": issue.get("body") or "",
    }


def parse_comments(text: str) -> list[dict]:
    out: list[dict] = []
    for doc in _json_docs(text, "comments"):
        if not isinstance(doc, list):
            raise GhError("unexpected comments JSON shape from gh")
        for item in doc:
            if not isinstance(item, dict) or not isinstance(item.get("id"), int):
                raise GhError("a comment is missing its id")
            out.append(
                {
                    "id": int(item["id"]),
                    "author": _to_str((item.get("user") or {}).get("login")),
                    "created_at": _to_str(item.get("created_at")),
                    "updated_at": _to_str(item.get("updated_at")),
                    "body": item.get("body") or "",
                }
            )
    return out


def resolve_repo(issue_arg: str, repo: str | None) -> tuple[str, int]:
    m = _ISSUE_URL.match(issue_arg.strip())
    if m:
        return f"{m.group(1)}/{m.group(2)}", int(m.group(3))
    if not issue_arg.strip().isdigit():
        raise UsageError(f"<issue> must be a number or an issue URL, got {issue_arg!r}")
    number = int(issue_arg.strip())
    if repo:
        if "/" not in repo:
            raise UsageError(f"--repo must be OWNER/REPO, got {repo!r}")
        return repo, number
    origin = gh("repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner")
    if origin.ok and origin.stdout.strip():
        return origin.stdout.strip(), number
    remote = gh("config", "--get", "remote.origin.url")
    if remote.ok:
        mm = _GITHUB_REMOTE.match(remote.stdout.strip())
        if mm:
            return f"{mm.group(1)}/{mm.group(2)}", number
    raise UsageError("could not resolve the repo; pass --repo OWNER/REPO")


def fetch_snapshot(repo: str, number: int) -> dict:
    """Fetch the issue and its comments once, over read-only `gh api` GETs."""
    issue_res = gh("api", f"repos/{repo}/issues/{number}")
    if not issue_res.ok:
        low = f"{issue_res.stderr} {issue_res.stdout}".lower()
        if "404" in low or "not found" in low:
            raise NotFoundError(f"issue {repo}#{number} does not exist")
        raise GhError(
            f"gh api repos/{repo}/issues/{number} failed (exit {issue_res.exit_code}): "
            f"{issue_res.stderr.strip()}"
        )
    issue = parse_issue(issue_res.stdout)
    comments: list[dict] = []
    if issue["comment_count"] > 0:
        com_res = gh("api", "--paginate", f"repos/{repo}/issues/{number}/comments")
        if not com_res.ok:
            raise GhError(
                f"gh api comments for {repo}#{number} failed (exit {com_res.exit_code}): "
                f"{com_res.stderr.strip()}"
            )
        comments = parse_comments(com_res.stdout)
    return {
        "v": SCHEMA_VERSION,
        "fetched_at": _utc_now(),
        "repo": repo,
        "number": number,
        "issue": issue,
        "comments": comments,
    }


def _utc_now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def snapshot_path(repo: str, number: int) -> Path:
    slug = re.sub(r"[^A-Za-z0-9._-]", "_", repo.replace("/", "__"))
    return _state_root() / "logs" / "issue-context" / slug / f"issue-{number}.json"


def load_snapshot(path: Path) -> dict | None:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    return data if isinstance(data, dict) else None


def save_snapshot(path: Path, snapshot: dict) -> str:
    """Persist the full snapshot; return a note on failure (never fatal)."""
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(snapshot, ensure_ascii=False), encoding="utf-8")
        return ""
    except OSError as exc:
        return f" (snapshot could not be saved: {exc})"


def _fresh(snapshot: dict, ttl: int) -> bool:
    try:
        when = time.strptime(snapshot.get("fetched_at", ""), "%Y-%m-%dT%H:%M:%SZ")
    except ValueError:
        return False
    return (time.time() - time.mktime(when) - time.timezone) < ttl


def excerpt(text: str, limit: int) -> tuple[str, bool]:
    """Return (excerpt, truncated) bounded by utf-8 bytes on a char boundary."""
    data = text.encode("utf-8")
    if len(data) <= limit:
        return text, False
    cut = data[:limit]
    while cut:
        try:
            return cut.decode("utf-8"), True
        except UnicodeDecodeError:
            cut = cut[:-1]
    return "", True


def _body_digest(body: str) -> str:
    return hashlib.sha256(body.encode("utf-8")).hexdigest()[:16]


def diff_snapshots(prev: dict | None, cur: dict) -> dict:
    """New and edited comments between two snapshots.

    An "edited" comment is one whose body digest OR `updated_at` changed:
    GitHub bumps `updated_at` on an edit, so a maintainer correction is
    surfaced even when the excerpt we kept is unchanged.
    """
    if not prev:
        return {"new": [], "edited": []}
    before = {
        c["id"]: (_body_digest(c.get("body", "")), c.get("updated_at"))
        for c in prev.get("comments", [])
    }
    new: list[int] = []
    edited: list[int] = []
    for c in cur.get("comments", []):
        prior = before.get(c["id"])
        if prior is None:
            new.append(c["id"])
        elif prior[0] != _body_digest(c.get("body", "")) or prior[1] != c.get("updated_at"):
            edited.append(c["id"])
    return {"new": new, "edited": edited}


def render_text(snapshot: dict, delta: dict, max_bytes: int, note: str) -> str:
    issue = snapshot["issue"]
    comments = snapshot.get("comments", [])
    lines: list[str] = []
    labels = ", ".join(_to_str(lab) for lab in issue.get("labels", [])) or "-"
    lines.append(f"issue {snapshot['repo']}#{issue['number']}  [{issue['state']}]")
    lines.append(f"title:    {issue['title']}")
    lines.append(
        f"author:   {issue['author']}   labels: {labels}   "
        f"created: {issue['created_at']}   updated: {issue['updated_at']}"
    )
    lines.append(
        f"comments: {issue['comment_count']}   "
        f"fetched: {snapshot['fetched_at']}{note}"
    )
    if delta.get("new") or delta.get("edited"):
        lines.append(
            f"refresh:  +{len(delta['new'])} new, {len(delta['edited'])} edited "
            f"(new ids: {delta['new'] or '-'}; edited ids: {delta['edited'] or '-'})"
        )
    header = "\n".join(lines)

    budget = max(1_000, max_bytes - len(header.encode("utf-8")) - 600)

    body, body_trunc = excerpt(issue.get("body", ""), min(BODY_EXCERPT_BYTES, budget))
    body_bytes = len(body.encode("utf-8"))
    budget -= body_bytes
    body_note = (
        f" (showed {body_bytes} of {len(issue.get('body','').encode('utf-8'))} bytes; "
        "full text in the snapshot)" if body_trunc else ""
    )
    body_block = f"\n--- body{body_note} ---\n{body}" if body else "\n--- body: (empty) ---"

    com_lines: list[str] = []
    shown = 0
    for c in comments:
        exc, trunc = excerpt(c.get("body", ""), PER_COMMENT_EXCERPT_BYTES)
        suffix = "\u2026 (truncated)" if trunc else ""
        entry = (
            f"\n#{c['id']} {c['author']} {c['created_at']}"
            f"{' [edited]' if trunc else ''}\n{exc}{suffix}"
        )
        if len(entry.encode("utf-8")) > budget:
            break
        budget -= len(entry.encode("utf-8"))
        com_lines.append(entry)
        shown += 1
    omitted = len(comments) - shown
    footer = [
        "",
        f"--- comments: showed {shown} of {len(comments)} ---"
        + (f" {omitted} omitted (snapshot has all)" if omitted else ""),
        "continuation: --comment <id> prints one in full; --refresh re-fetches; "
        f"full snapshot at {snapshot_path(snapshot['repo'], issue['number'])}",
    ]
    return header + body_block + "".join(com_lines) + "\n".join(footer)


def render_comment(snapshot: dict, comment_id: int, max_bytes: int) -> str:
    for c in snapshot.get("comments", []):
        if c["id"] == comment_id:
            head = f"#{c['id']} {c['author']} {c['created_at']} (updated {c['updated_at']})"
            body, trunc = excerpt(c.get("body", ""), max(1_000, max_bytes - len(head) - 200))
            tail = "\n[comment truncated at --max-bytes]" if trunc else ""
            return f"{head}\n{body}{tail}"
    raise NotFoundError(
        f"comment {comment_id} not found in {snapshot['repo']}#{snapshot['number']} "
        "(deleted, or added after the snapshot -- try --refresh)"
    )


def render_json(snapshot: dict, delta: dict, max_bytes: int, note: str) -> str:
    issue = snapshot["issue"]
    body, body_trunc = excerpt(issue.get("body", ""), BODY_EXCERPT_BYTES)
    out_comments = []
    used = len(body.encode("utf-8"))
    for c in snapshot.get("comments", []):
        if used >= max_bytes:
            break
        exc, trunc = excerpt(c.get("body", ""), PER_COMMENT_EXCERPT_BYTES)
        used += len(exc.encode("utf-8"))
        out_comments.append(
            {"id": c["id"], "author": c["author"], "created_at": c["created_at"],
             "updated_at": c["updated_at"], "body": exc, "truncated": trunc}
        )
    payload = {
        "v": SCHEMA_VERSION,
        "repo": snapshot["repo"],
        "number": issue["number"],
        "state": issue["state"],
        "title": issue["title"],
        "author": issue["author"],
        "labels": issue.get("labels", []),
        "created_at": issue["created_at"],
        "updated_at": issue["updated_at"],
        "comment_count": issue["comment_count"],
        "fetched_at": snapshot["fetched_at"],
        "body": body,
        "body_truncated": body_trunc,
        "comments": out_comments,
        "comments_omitted": max(0, len(snapshot.get("comments", [])) - len(out_comments)),
        "changed": delta,
        "snapshot_path": str(snapshot_path(snapshot["repo"], issue["number"])),
        "note": note.strip(),
    }
    return json.dumps(payload, ensure_ascii=False, indent=2)


def _load_or_fetch(repo: str, number: int, refresh: bool, ttl: int) -> tuple[dict, dict, str]:
    path = snapshot_path(repo, number)
    prev = load_snapshot(path) if refresh else None
    cached = None if refresh else load_snapshot(path)
    if cached is not None and _fresh(cached, ttl):
        return cached, {"new": [], "edited": []}, " (cached)"
    snapshot = fetch_snapshot(repo, number)
    delta = diff_snapshots(prev, snapshot) if refresh else {"new": [], "edited": []}
    note = save_snapshot(path, snapshot)
    return snapshot, delta, note


def _ttl() -> int:
    raw = os.environ.get("CLUD_ISSUE_CONTEXT_TTL", "").strip()
    try:
        return int(raw) if raw else DEFAULT_TTL_SEC
    except ValueError:
        return DEFAULT_TTL_SEC


def parse_args(argv: list[str]) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        prog="issue_context",
        description="Bounded, read-only GitHub issue context (#2229).",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    p.add_argument("issue", help="issue number or https://github.com/OWNER/REPO/issues/N URL")
    p.add_argument("--repo", help="owner/name (default: origin of `git remote get-url origin`)")
    p.add_argument("--max-bytes", type=int, default=DEFAULT_MAX_BYTES,
                   help=f"bound on the summary view (default {DEFAULT_MAX_BYTES})")
    p.add_argument("--comment", type=int, default=None,
                   help="print this comment id in full instead of the summary")
    p.add_argument("--refresh", action="store_true",
                   help="re-fetch, bypass the TTL cache, and report new/edited comments")
    p.add_argument("--json", action="store_true", help="emit the summary as JSON")
    p.add_argument("--no-cache", dest="cache", action="store_false",
                   help="always re-fetch (same as --refresh, without the delta)")
    return p.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    try:
        ns = parse_args(argv if argv is not None else sys.argv[1:])
    except SystemExit as exc:
        return EXIT_USAGE if exc.code else EXIT_OK
    try:
        repo, number = resolve_repo(ns.issue, ns.repo)
        refresh = ns.refresh or not ns.cache
        snapshot, delta, note = _load_or_fetch(repo, number, refresh, _ttl())
        if ns.comment is not None:
            emit_bounded(render_comment(snapshot, ns.comment, ns.max_bytes), "issue_context")
        elif ns.json:
            emit_bounded(render_json(snapshot, delta, ns.max_bytes, note), "issue_context")
        else:
            emit_bounded(render_text(snapshot, delta, ns.max_bytes, note), "issue_context")
        return EXIT_OK
    except NotFoundError as exc:
        print(f"NOT-FOUND  {exc}", file=sys.stderr)
        return EXIT_NOT_FOUND
    except UsageError as exc:
        print(f"issue_context: error: {exc}", file=sys.stderr)
        return EXIT_USAGE
    except GhError as exc:
        print(f"UNREACHABLE  {exc}", file=sys.stderr)
        return EXIT_GH_FAILED


if __name__ == "__main__":
    raise SystemExit(main())
