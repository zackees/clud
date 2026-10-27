#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["running-process==4.10.1"]
# ///
# managed-by: clud
"""Read a bounded GitHub issue snapshot with explicit offline continuation.

  "$CLUD_EXE" tool run github/issue_context.py -- --repo OWNER/REPO --issue 42
  "$CLUD_EXE" tool run github/issue_context.py -- --artifact PATH --section comment:123
  "$CLUD_EXE" tool run github/issue_context.py -- --refresh PATH

Fetching never modifies GitHub. Refresh creates a new snapshot; continuation
reads only the saved snapshot. Windows private artifacts are not yet supported.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import stat
import sys
import tempfile
from datetime import datetime
from pathlib import Path
from urllib.parse import urlsplit

from running_process import PIPE, RunningProcess, TimeoutExpired

MAX_OUTPUT = 8192
MAX_SNAPSHOT = 32 * 1024 * 1024
MAX_PAGES = 1000
REPO = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\Z")
ARTIFACT = re.compile(r"issue-context-[A-Za-z0-9_-]{8}\.json\Z")


def github_get(path: str, hostname: str | None = None) -> bytes:
    try:
        # Limit both pipes before the process library captures them. The shell
        # receives all variable values as positional arguments.
        script = (
            'gh api "$1" --hostname "$2" --allow-escape-sequences '
            "2> >(head -c 4096 >&2) | head -c 33554433"
        )
        args = [
            "bash",
            "-o",
            "pipefail",
            "-c",
            script,
            "clud-issue",
            path,
            hostname or "github.com",
        ]
        env = os.environ.copy()
        env["CLUD_GH_FRESH"] = "1"
        result = RunningProcess.run(
            args,
            env=env,
            capture_output=True,
            stderr=PIPE,
            text=False,
            timeout=60,
        )
    except (OSError, TimeoutError, TimeoutExpired) as exc:
        raise RuntimeError("GitHub request failed or timed out") from exc
    if len(result.stdout or b"") > MAX_SNAPSHOT:
        raise ValueError("GitHub response exceeds snapshot limit")
    if b"clud: gh read broker: rate-limit reserve" in (result.stderr or b""):
        raise RuntimeError("GitHub broker returned stale data; retry later")
    if result.returncode != 0:
        raise RuntimeError(f"GitHub request failed (exit {result.returncode})")
    return result.stdout or b""


def safe_display_path(path: Path) -> None:
    text = str(path)
    if len(os.fsencode(text)) > 300 or not all(c.isprintable() for c in text):
        raise ValueError("artifact path is too long or contains control characters")


def quote_path(path: Path, windows: bool | None = None) -> str:
    value = str(path)
    safe_display_path(path)
    if windows is None:
        windows = os.name == "nt"
    if windows:
        if any(c in value for c in '%!"&|<>^$`'):
            raise ValueError("artifact path contains unsupported Windows shell characters")
        return f'"{value}"'
    return shlex.quote(value)


def check_ancestor(info) -> None:
    if not hasattr(os, "getuid"):
        return
    if info.st_uid not in (0, os.getuid()):
        raise ValueError("artifact directory has an untrusted ancestor owner")
    if info.st_mode & 0o022 and not info.st_mode & stat.S_ISVTX:
        raise ValueError("artifact directory has a writable non-sticky ancestor")


def artifact_root() -> Path:
    if os.name == "nt":
        raise ValueError("Windows artifact privacy verification is not available yet")
    root = Path(os.environ.get("CLUD_TOOL_OUTPUT_DIR") or Path.home() / ".clud/tmp/tool-output")
    safe_display_path(root)
    if len(os.fsencode(root)) > 200:
        raise ValueError("artifact directory path is too long")
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    base = root.resolve(strict=True)
    for directory in (base, *base.parents):
        check_ancestor(directory.stat())
    private = base / "issue-context-private"
    private.mkdir(mode=0o700, exist_ok=True)
    if private.is_symlink():
        raise ValueError("artifact directory must not be a symlink")
    resolved = private.resolve(strict=True)
    safe_display_path(resolved)
    info = resolved.stat()
    if hasattr(os, "getuid") and (info.st_uid != os.getuid() or info.st_mode & 0o077):
        raise ValueError("artifact directory must be private and owned by the current user")
    return resolved


def fetch_comments(fetch, prefix):
    comments = []
    size = 0
    for page in range(1, MAX_PAGES + 1):
        data = fetch(f"{prefix}/comments?per_page=100&page={page}")
        if not isinstance(data, list) or any(not isinstance(c, dict) for c in data):
            raise ValueError("GitHub returned a malformed comment page")
        size += len(json.dumps(data, ensure_ascii=False).encode())
        if size > MAX_SNAPSHOT:
            raise ValueError("comments exceed snapshot limit")
        comments.extend(data)
        if len(data) < 100:
            break
    else:
        raise ValueError("comment pagination exceeded the page limit")
    return comments


def valid_issue_identity(issue, repo, number, hostname):
    url = issue.get("url")
    if not isinstance(url, str) or len(url) > 1000:
        return False
    parsed = urlsplit(url)
    expected_host = "api.github.com" if hostname.lower() == "github.com" else hostname.lower()
    endpoint = f"/repos/{repo}/issues/{number}"
    return (
        parsed.scheme == "https"
        and parsed.netloc.lower() == expected_host
        and parsed.path.lower() in (endpoint.lower(), "/api/v3" + endpoint.lower())
        and not parsed.query
        and not parsed.fragment
    )


def required_issue_timestamp(issue):
    if timestamp(issue.get("updated_at")) == "unknown":
        raise ValueError("issue update timestamp is unavailable")


def fetch_snapshot(repo, number, hostname=None, get=None):
    if not REPO.fullmatch(repo) or number < 1:
        raise ValueError("expected --repo OWNER/REPO and a positive issue number")
    hostname = hostname or os.environ.get("GH_HOST") or "github.com"
    if not re.fullmatch(r"[A-Za-z0-9.-]+(?::[0-9]{1,5})?", hostname):
        raise ValueError("invalid GitHub hostname")
    get = get or github_get
    prefix = f"repos/{repo}/issues/{number}"

    def fetch(path):
        raw = get(path, hostname)
        if len(raw) > MAX_SNAPSHOT:
            raise ValueError("GitHub response exceeds snapshot limit")
        try:
            return json.loads(raw)
        except (ValueError, UnicodeDecodeError) as exc:
            raise ValueError("GitHub returned malformed JSON") from exc

    issue = fetch(prefix)
    if (
        not isinstance(issue, dict)
        or issue.get("number") != number
        or "pull_request" in issue
        or not valid_issue_identity(issue, repo, number, hostname)
    ):
        raise ValueError("GitHub did not return the requested issue")
    required_issue_timestamp(issue)
    expected = issue.get("comments")
    if not isinstance(expected, int) or expected < 0:
        raise ValueError("issue comment count is unavailable")
    comments = fetch_comments(fetch, prefix)
    end = fetch(prefix)
    if (
        not isinstance(end, dict)
        or not valid_issue_identity(end, repo, number, hostname)
        or end.get("updated_at") != issue.get("updated_at")
        or end.get("comments") != expected
    ):
        raise ValueError("issue changed during retrieval; retry to obtain a consistent snapshot")
    ids = [c.get("id") for c in comments]
    if (
        len(comments) != expected
        or any(not isinstance(i, int) for i in ids)
        or len(set(ids)) != len(ids)
    ):
        raise ValueError("comment pagination was incomplete or inconsistent; retry")
    return {
        "schema": 1,
        "repo": repo,
        "issue_number": number,
        "hostname": hostname,
        "issue": issue,
        "comments": comments,
    }


def _encoded(value):
    return json.dumps(value, ensure_ascii=True, separators=(",", ":"))


def timestamp(value):
    if not isinstance(value, str) or len(value) > 40:
        return "unknown"
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return "unknown"
    return value if parsed.tzinfo is not None else "unknown"


def attribution(comment):
    user = comment.get("user")
    author = user.get("login") if isinstance(user, dict) else None
    if not isinstance(author, str) or not re.fullmatch(r"[A-Za-z0-9-]{1,100}", author):
        author = "unknown"
    url = comment.get("html_url")
    if not isinstance(url, str) or len(url) > 300 or not all(c.isprintable() for c in url):
        url = None
    if url is not None:
        parsed = urlsplit(url)
        if parsed.scheme != "https" or not parsed.netloc or parsed.username or parsed.password:
            url = None
    return {"author": author, "url": url}


def render_comments(comments, offset, limit):
    if offset > len(comments):
        raise ValueError("comment cursor is past the snapshot")
    base = {"section": "comments", "count": len(comments), "items": []}
    for comment in comments[offset:]:
        item = {
            "id": comment["id"],
            "updated_at": timestamp(comment.get("updated_at")),
            **attribution(comment),
        }
        trial = {
            **base,
            "items": base["items"] + [item],
            "continuation": f"comments offset {offset + len(base['items']) + 1}",
        }
        if len(_encoded(trial).encode()) + 1 > limit:
            break
        base["items"].append(item)
    if offset < len(comments) and not base["items"]:
        raise ValueError("comment metadata exceeds the output limit")
    next_index = offset + len(base["items"])
    base["continuation"] = f"comments offset {next_index}" if next_index < len(comments) else None
    return _encoded(base)


def render_text(base, text, section, offset, limit):
    if offset > len(text):
        raise ValueError("text cursor is past the snapshot")
    low, high = 0, len(text) - offset
    while low < high:
        mid = (low + high + 1) // 2
        trial = {
            **base,
            "text": text[offset : offset + mid],
            "remaining_characters": len(text) - offset - mid,
            "continuation": f"{section if section != 'overview' else 'body'} offset {offset + mid}",
        }
        if len(_encoded(trial).encode()) + 1 <= limit:
            low = mid
        else:
            high = mid - 1
    end = offset + low
    if end == offset and end < len(text):
        raise ValueError("text metadata leaves no room for continuation")
    base["text"] = text[offset:end]
    base["remaining_characters"] = len(text) - end
    if end < len(text):
        base["continuation"] = f"{section if section != 'overview' else 'body'} offset {end}"
    elif section != "overview":
        base["continuation"] = None
    result = _encoded(base)
    if len(result.encode()) + 1 > limit:
        raise ValueError("metadata exceeds output limit")
    return result


def render_snapshot(snapshot, section="overview", offset=0, limit=MAX_OUTPUT):
    if not 128 <= limit <= MAX_OUTPUT or offset < 0:
        raise ValueError("insufficient output budget or negative offset")
    issue, comments = snapshot["issue"], snapshot["comments"]
    if section == "overview":
        base = {
            "number": issue.get("number"),
            "title": str(issue.get("title") or "").encode("utf-8")[:120].decode("utf-8", "ignore"),
            "state": issue.get("state"),
            "updated_at": timestamp(issue.get("updated_at")),
            "comment_count": len(comments),
            "comments_omitted": len(comments),
            "sections": "body, comments, comment:<id>",
        }
        text = str(issue.get("body") or "")
    elif section == "body":
        base = {"section": "body"}
        text = str(issue.get("body") or "")
    elif section == "comments":
        return render_comments(comments, offset, limit)
    elif section.startswith("comment:") and section[8:].isdigit():
        comment_id = int(section[8:])
        comment = next((c for c in comments if c.get("id") == comment_id), None)
        if comment is None:
            raise ValueError(
                "comment not found in this snapshot; refresh if it was added or deleted"
            )
        base = {
            "section": section,
            "id": comment_id,
            "updated_at": timestamp(comment.get("updated_at")),
            **attribution(comment),
        }
        text = str(comment.get("body") or "")
    else:
        raise ValueError("section must be overview, body, comments, or comment:<id>")
    return render_text(base, text, section, offset, limit)


def save_snapshot(snapshot):
    root = artifact_root()
    raw = _encoded(snapshot).encode()
    if len(raw) > MAX_SNAPSHOT:
        raise ValueError("snapshot exceeds artifact limit")
    fd, name = tempfile.mkstemp(prefix="issue-context-", suffix=".json", dir=root)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(raw)
    except OSError as exc:
        raise ValueError("could not save the complete snapshot") from exc
    return Path(name)


def load_snapshot(path):
    path = Path(path)
    safe_display_path(path)
    root = artifact_root()
    if path.parent.resolve(strict=True) != root or not ARTIFACT.fullmatch(path.name):
        raise ValueError("artifact is outside the private issue snapshot directory")
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(stream.fileno())
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_uid != os.getuid()
            or info.st_mode & 0o077
            or info.st_size > MAX_SNAPSHOT
        ):
            raise ValueError("snapshot is not a private owned regular file")
        snapshot = json.load(stream)
    if (
        not isinstance(snapshot, dict)
        or snapshot.get("schema") != 1
        or not isinstance(snapshot.get("issue"), dict)
        or not isinstance(snapshot.get("comments"), list)
    ):
        raise ValueError("invalid snapshot")
    return snapshot


class BoundedParser(argparse.ArgumentParser):
    def error(self, _message):
        raise ValueError("invalid command arguments; use --help for usage")


def main(argv=None):
    parser = BoundedParser(description=__doc__)
    parser.add_argument("--repo")
    parser.add_argument("--issue", type=int)
    parser.add_argument("--hostname")
    parser.add_argument("--artifact")
    parser.add_argument("--refresh")
    parser.add_argument("--section", default="overview")
    parser.add_argument("--offset", type=int, default=0)
    parser.add_argument("--limit", type=int, default=MAX_OUTPUT)
    try:
        args = parser.parse_args(argv)
        if not 512 <= args.limit <= MAX_OUTPUT:
            raise ValueError("limit must be 512..8192 bytes")
        if args.artifact and args.refresh:
            raise ValueError("choose either --artifact or --refresh")
        if (args.artifact or args.refresh) and (args.repo or args.issue or args.hostname):
            raise ValueError("artifact reads cannot be combined with GitHub selectors")
        if args.refresh:
            old = load_snapshot(args.refresh)
            snapshot = fetch_snapshot(old["repo"], old["issue_number"], old.get("hostname"))
            old_comments = {c["id"]: (c.get("updated_at"), c.get("body")) for c in old["comments"]}
            new_comments = {
                c["id"]: (c.get("updated_at"), c.get("body")) for c in snapshot["comments"]
            }
            changes = {
                "added": len(new_comments.keys() - old_comments.keys()),
                "deleted": len(old_comments.keys() - new_comments.keys()),
                "edited": sum(
                    old_comments[i] != new_comments[i]
                    for i in old_comments.keys() & new_comments.keys()
                ),
                "issue_changed": any(
                    old["issue"].get(k) != snapshot["issue"].get(k)
                    for k in ("updated_at", "title", "body", "state")
                ),
            }
            artifact = save_snapshot(snapshot)
        elif args.artifact:
            snapshot = load_snapshot(args.artifact)
            artifact = Path(args.artifact)
            changes = None
        else:
            if not args.repo or not args.issue:
                raise ValueError("provide --repo and --issue, --artifact, or --refresh")
            # Verify artifact capability before fetching private data.
            artifact_root()
            snapshot = fetch_snapshot(args.repo, args.issue, args.hostname)
            artifact = save_snapshot(snapshot)
            changes = None
        suffix = f"\nartifact: {quote_path(artifact)}"
        if changes is not None:
            suffix += "\nrefresh: " + _encoded(changes)
        budget = min(args.limit, MAX_OUTPUT) - len(suffix.encode("utf-8")) - 1
        rendered = render_snapshot(snapshot, args.section, args.offset, budget)
        print(rendered + suffix)
        return 0
    except OSError:
        print("issue_context: private artifact or GitHub I/O failed", file=sys.stderr)
        return 1
    except (ValueError, RuntimeError, KeyError, TypeError) as exc:
        message = "".join(c if c.isprintable() else "?" for c in str(exc))[:500]
        print(f"issue_context: {message}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
