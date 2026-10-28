#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["running-process==4.10.1"]
# ///
# managed-by: clud
"""Read a bounded GitHub pull-request snapshot with explicit offline continuation.

  "$CLUD_EXE" tool run github/pr_context.py -- --repo OWNER/REPO --pr 42
  "$CLUD_EXE" tool run github/pr_context.py -- --artifact PATH --section comments
  "$CLUD_EXE" tool run github/pr_context.py -- --refresh PATH

A review of one busy pull request otherwise runs `gh pr view --json
reviews,comments`, which prints every review body and every inline thread
comment at once. This tool fetches the PR, its reviews, its conversation and
inline review comments, and its check runs once, saves the complete snapshot
outside the checkout, and prints a byte-bounded view with explicit omission
counts and continuation instructions.

Fetching never modifies GitHub. Refresh creates a new snapshot; continuation
reads only the saved snapshot. Windows and macOS private artifacts are not yet
supported.
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
MAX_FAILING_CHECKS = 20
REPO = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\Z")
ARTIFACT = re.compile(r"pr-context-[A-Za-z0-9_-]{8}\.json\Z")
SECTIONS = "body, reviews, review:<id>, comments, comment:<id>, review-comments, checks"


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
            "clud-pr",
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
    if sys.platform == "darwin":
        raise ValueError("macOS artifact ACL verification is not available yet")
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
    private = base / "pr-context-private"
    private.mkdir(mode=0o700, exist_ok=True)
    if private.is_symlink():
        raise ValueError("artifact directory must not be a symlink")
    resolved = private.resolve(strict=True)
    safe_display_path(resolved)
    info = resolved.stat()
    if hasattr(os, "getuid") and (info.st_uid != os.getuid() or info.st_mode & 0o077):
        raise ValueError("artifact directory must be private and owned by the current user")
    return resolved


def fetch_pages(fetch, path, cap=MAX_SNAPSHOT):
    items = []
    size = 0
    separator = "&" if "?" in path else "?"
    for page in range(1, MAX_PAGES + 1):
        data = fetch(f"{path}{separator}per_page=100&page={page}")
        if not isinstance(data, list) or any(not isinstance(item, dict) for item in data):
            raise ValueError("GitHub returned a malformed paginated page")
        size += len(json.dumps(data, ensure_ascii=False).encode())
        if size > cap:
            raise ValueError("a paginated list exceeds the snapshot limit")
        items.extend(data)
        if len(data) < 100:
            return items
    raise ValueError("pagination exceeded the page limit")


def fetch_check_runs(fetch, path, cap=8 * 1024 * 1024):
    """Check runs page inside `{total_count, check_runs: [...]}`, not a bare list."""
    runs = []
    size = 0
    for page in range(1, MAX_PAGES + 1):
        data = fetch(f"{path}?per_page=100&page={page}")
        page_runs = data.get("check_runs") if isinstance(data, dict) else None
        if not isinstance(page_runs, list) or any(not isinstance(run, dict) for run in page_runs):
            raise ValueError("GitHub returned a malformed check-run page")
        total = data.get("total_count")
        if not isinstance(total, int) or total < 0:
            raise ValueError("check-run count is unavailable")
        size += len(json.dumps(page_runs, ensure_ascii=False).encode())
        if size > cap:
            raise ValueError("check runs exceed the snapshot limit")
        if not page_runs:
            break
        runs.extend(page_runs)
        if len(runs) >= total:
            break
    else:
        raise ValueError("check-run pagination exceeded the page limit")
    if len(runs) != total:
        raise ValueError("check-run pagination was incomplete; retry")
    return runs


def valid_pull_identity(pull, repo, number, hostname):
    url = pull.get("url")
    if not isinstance(url, str) or len(url) > 1000:
        return False
    parsed = urlsplit(url)
    expected_host = "api.github.com" if hostname.lower() == "github.com" else hostname.lower()
    endpoint = f"/repos/{repo}/pulls/{number}"
    return (
        parsed.scheme == "https"
        and parsed.netloc.lower() == expected_host
        and parsed.path.lower() in (endpoint.lower(), "/api/v3" + endpoint.lower())
        and not parsed.query
        and not parsed.fragment
    )


def validate_pull(pull, repo, number, hostname):
    """Accept only the requested pull request, with the fields the snapshot needs."""
    if (
        not isinstance(pull, dict)
        or pull.get("number") != number
        or not valid_pull_identity(pull, repo, number, hostname)
    ):
        raise ValueError("GitHub did not return the requested pull request")
    if timestamp(pull.get("updated_at")) == "unknown":
        raise ValueError("pull request update timestamp is unavailable")
    sha = (pull.get("head") or {}).get("sha")
    if not isinstance(sha, str) or not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ValueError("pull request head commit is unavailable")
    expected_comments = pull.get("comments")
    if not isinstance(expected_comments, int) or expected_comments < 0:
        raise ValueError("pull request comment count is unavailable")
    return sha, expected_comments


def ensure_unchanged(pull, end, repo, number, hostname, sha, expected_comments):
    if (
        not isinstance(end, dict)
        or not valid_pull_identity(end, repo, number, hostname)
        or end.get("updated_at") != pull.get("updated_at")
        or end.get("comments") != expected_comments
        or (end.get("head") or {}).get("sha") != sha
    ):
        raise ValueError("pull request changed during retrieval; retry for a consistent snapshot")


def ensure_unique(name, items):
    ids = [item.get("id") for item in items]
    if any(not isinstance(item_id, int) for item_id in ids) or len(set(ids)) != len(ids):
        raise ValueError(f"{name} pagination was incomplete or inconsistent; retry")


def fetch_snapshot(repo, number, hostname=None, get=None):
    if not REPO.fullmatch(repo) or number < 1:
        raise ValueError("expected --repo OWNER/REPO and a positive pull request number")
    hostname = hostname or os.environ.get("GH_HOST") or "github.com"
    if not re.fullmatch(r"[A-Za-z0-9.-]+(?::[0-9]{1,5})?", hostname):
        raise ValueError("invalid GitHub hostname")
    get = get or github_get
    prefix = f"repos/{repo}/pulls/{number}"

    def fetch(path):
        raw = get(path, hostname)
        if len(raw) > MAX_SNAPSHOT:
            raise ValueError("GitHub response exceeds snapshot limit")
        try:
            return json.loads(raw)
        except (ValueError, UnicodeDecodeError) as exc:
            raise ValueError("GitHub returned malformed JSON") from exc

    pull = fetch(prefix)
    sha, expected_comments = validate_pull(pull, repo, number, hostname)
    reviews = fetch_pages(fetch, f"{prefix}/reviews")
    review_comments = fetch_pages(fetch, f"{prefix}/comments")
    comments = fetch_pages(fetch, f"repos/{repo}/issues/{number}/comments")
    checks = fetch_check_runs(fetch, f"repos/{repo}/commits/{sha}/check-runs")
    ensure_unchanged(pull, fetch(prefix), repo, number, hostname, sha, expected_comments)
    for name, items in (
        ("reviews", reviews),
        ("review comments", review_comments),
        ("comments", comments),
        ("check runs", checks),
    ):
        ensure_unique(name, items)
    if len(comments) != expected_comments:
        raise ValueError("comment pagination was incomplete; retry")
    return {
        "schema": 1,
        "repo": repo,
        "pull_number": number,
        "hostname": hostname,
        "pull": pull,
        "reviews": reviews,
        "review_comments": review_comments,
        "comments": comments,
        "checks": checks,
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


def author_of(record):
    user = record.get("user")
    login = user.get("login") if isinstance(user, dict) else None
    if not isinstance(login, str) or not re.fullmatch(r"[A-Za-z0-9-]{1,100}", login):
        return "unknown"
    return login


def attribution(record):
    url = record.get("html_url")
    if not isinstance(url, str) or len(url) > 300 or not all(c.isprintable() for c in url):
        url = None
    if url is not None:
        parsed = urlsplit(url)
        if parsed.scheme != "https" or not parsed.netloc or parsed.username or parsed.password:
            url = None
    return {"author": author_of(record), "url": url}


def body_characters(record):
    body = record.get("body")
    return len(body) if isinstance(body, str) else 0


def check_state(check):
    conclusion = check.get("conclusion")
    if not isinstance(conclusion, str) or not conclusion:
        status = check.get("status")
        return status if isinstance(status, str) and status else "unknown"
    return conclusion


def check_summary(checks):
    counts = {}
    failing = []
    for check in checks:
        state = check_state(check)
        counts[state] = counts.get(state, 0) + 1
        if state not in ("success", "neutral", "skipped"):
            name = check.get("name")
            if isinstance(name, str) and 0 < len(name) <= 120 and len(failing) < MAX_FAILING_CHECKS:
                failing.append({"name": name, "state": state})
    return {"total": len(checks), "by_state": counts, "not_successful": failing}


def review_counts(reviews):
    counts = {}
    for review in reviews:
        state = review.get("state")
        key = state if isinstance(state, str) and state else "UNKNOWN"
        counts[key] = counts.get(key, 0) + 1
    return counts


def render_items(key, records, offset, limit, fields):
    if offset > len(records):
        raise ValueError("item cursor is past the snapshot")
    base = {"section": key, "count": len(records), "items": []}
    for record in records[offset:]:
        item = fields(record)
        trial = {
            **base,
            "items": base["items"] + [item],
            "continuation": f"{key} offset {offset + len(base['items']) + 1}",
        }
        if len(_encoded(trial).encode()) + 1 > limit:
            break
        base["items"].append(item)
    if offset < len(records) and not base["items"]:
        raise ValueError("item metadata exceeds the output limit")
    next_index = offset + len(base["items"])
    base["continuation"] = f"{key} offset {next_index}" if next_index < len(records) else None
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
            "continuation": f"{'body' if section == 'overview' else section} offset {offset + mid}",
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
        base["continuation"] = f"{'body' if section == 'overview' else section} offset {end}"
    elif section != "overview":
        base["continuation"] = None
    result = _encoded(base)
    if len(result.encode()) + 1 > limit:
        raise ValueError("metadata exceeds output limit")
    return result


def review_item(review):
    return {
        "id": review.get("id"),
        "author": author_of(review),
        "state": review.get("state") if isinstance(review.get("state"), str) else "unknown",
        "submitted_at": timestamp(review.get("submitted_at")),
        "body_characters": body_characters(review),
    }


def thread_item(comment):
    path = comment.get("path")
    return {
        "id": comment.get("id"),
        "author": author_of(comment),
        "path": path if isinstance(path, str) and len(path) <= 200 else None,
        "line": comment.get("line") if isinstance(comment.get("line"), int) else None,
        "updated_at": timestamp(comment.get("updated_at")),
        "body_characters": body_characters(comment),
    }


def comment_item(comment):
    return {
        "id": comment.get("id"),
        "author": author_of(comment),
        "updated_at": timestamp(comment.get("updated_at")),
        "body_characters": body_characters(comment),
    }


def check_item(check):
    return {
        "name": str(check.get("name") or "")[:120],
        "state": check_state(check),
        "url": check.get("html_url") if isinstance(check.get("html_url"), str) else None,
    }


def find_by_id(records, wanted, label):
    for record in records:
        if record.get("id") == wanted:
            return record
    raise ValueError(f"{label} not found in this snapshot; refresh if it was added or deleted")


def render_snapshot(snapshot, section="overview", offset=0, limit=MAX_OUTPUT):
    if not 128 <= limit <= MAX_OUTPUT or offset < 0:
        raise ValueError("insufficient output budget or negative offset")
    pull = snapshot["pull"]
    reviews = snapshot["reviews"]
    if section == "overview":
        base = {
            "number": pull.get("number"),
            "title": str(pull.get("title") or "").encode("utf-8")[:120].decode("utf-8", "ignore"),
            "state": pull.get("state"),
            "draft": pull.get("draft"),
            "merged": pull.get("merged"),
            "author": author_of(pull),
            "base": (pull.get("base") or {}).get("ref"),
            "head": (pull.get("head") or {}).get("ref"),
            "mergeable_state": pull.get("mergeable_state"),
            "changed_files": pull.get("changed_files"),
            "additions": pull.get("additions"),
            "deletions": pull.get("deletions"),
            "review_count": len(reviews),
            "reviews_by_state": review_counts(reviews),
            "comment_count": len(snapshot["comments"]),
            "review_comment_count": len(snapshot["review_comments"]),
            "comments_omitted": len(snapshot["comments"]),
            "checks": check_summary(snapshot["checks"]),
            "updated_at": timestamp(pull.get("updated_at")),
            "sections": SECTIONS,
        }
        text = str(pull.get("body") or "")
    elif section == "body":
        base = {"section": "body"}
        text = str(pull.get("body") or "")
    elif section == "reviews":
        return render_items("reviews", reviews, offset, limit, review_item)
    elif section == "comments":
        return render_items("comments", snapshot["comments"], offset, limit, comment_item)
    elif section == "review-comments":
        return render_items(
            "review-comments", snapshot["review_comments"], offset, limit, thread_item
        )
    elif section == "checks":
        return render_items("checks", snapshot["checks"], offset, limit, check_item)
    elif section.startswith("review:") and section[7:].isdigit():
        review = find_by_id(reviews, int(section[7:]), "review")
        base = {
            "section": section,
            "id": review.get("id"),
            "state": review.get("state"),
            "submitted_at": timestamp(review.get("submitted_at")),
            **attribution(review),
        }
        text = str(review.get("body") or "")
    elif section.startswith("comment:") and section[8:].isdigit():
        comment = find_by_id(snapshot["comments"], int(section[8:]), "comment")
        base = {
            "section": section,
            "id": comment.get("id"),
            "updated_at": timestamp(comment.get("updated_at")),
            **attribution(comment),
        }
        text = str(comment.get("body") or "")
    else:
        raise ValueError(f"section must be {SECTIONS}")
    return render_text(base, text, section, offset, limit)


def save_snapshot(snapshot):
    root = artifact_root()
    raw = _encoded(snapshot).encode()
    if len(raw) > MAX_SNAPSHOT:
        raise ValueError("snapshot exceeds artifact limit")
    fd, name = tempfile.mkstemp(prefix="pr-context-", suffix=".json", dir=root)
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
        raise ValueError("artifact is outside the private pull request snapshot directory")
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
        or not isinstance(snapshot.get("pull"), dict)
        or not isinstance(snapshot.get("reviews"), list)
        or not isinstance(snapshot.get("comments"), list)
        or not isinstance(snapshot.get("review_comments"), list)
        or not isinstance(snapshot.get("checks"), list)
    ):
        raise ValueError("invalid snapshot")
    return snapshot


def refresh_changes(old, new):
    def index(records):
        return {record["id"]: (record.get("updated_at"), record.get("body")) for record in records}

    old_comments, new_comments = index(old["comments"]), index(new["comments"])
    old_threads, new_threads = index(old["review_comments"]), index(new["review_comments"])
    return {
        "comments_added": len(new_comments.keys() - old_comments.keys()),
        "comments_deleted": len(old_comments.keys() - new_comments.keys()),
        "comments_edited": sum(
            old_comments[i] != new_comments[i] for i in old_comments.keys() & new_comments.keys()
        ),
        "review_comments_added": len(new_threads.keys() - old_threads.keys()),
        "review_comments_deleted": len(old_threads.keys() - new_threads.keys()),
        "reviews": len(new["reviews"]) - len(old["reviews"]),
        "pull_changed": any(
            old["pull"].get(key) != new["pull"].get(key)
            for key in ("updated_at", "title", "body", "state", "mergeable_state", "head")
        ),
    }


class BoundedParser(argparse.ArgumentParser):
    def error(self, _message):
        raise ValueError("invalid command arguments; use --help for usage")


def run(argv, parser_args):
    if not 512 <= parser_args.limit <= MAX_OUTPUT:
        raise ValueError("limit must be 512..8192 bytes")
    if parser_args.artifact and parser_args.refresh:
        raise ValueError("choose either --artifact or --refresh")
    selectors = parser_args.repo or parser_args.pr or parser_args.hostname
    if (parser_args.artifact or parser_args.refresh) and selectors:
        raise ValueError("artifact reads cannot be combined with GitHub selectors")
    if parser_args.refresh:
        old = load_snapshot(parser_args.refresh)
        snapshot = fetch_snapshot(old["repo"], old["pull_number"], old.get("hostname"))
        changes = refresh_changes(old, snapshot)
        artifact = save_snapshot(snapshot)
    elif parser_args.artifact:
        snapshot = load_snapshot(parser_args.artifact)
        artifact = Path(parser_args.artifact)
        changes = None
    else:
        if not parser_args.repo or not parser_args.pr:
            raise ValueError("provide --repo and --pr, --artifact, or --refresh")
        # Verify artifact capability before fetching private data.
        artifact_root()
        snapshot = fetch_snapshot(parser_args.repo, parser_args.pr, parser_args.hostname)
        artifact = save_snapshot(snapshot)
        changes = None
    suffix = f"\nartifact: {quote_path(artifact)}"
    if changes is not None:
        suffix += "\nrefresh: " + _encoded(changes)
    budget = min(parser_args.limit, MAX_OUTPUT) - len(suffix.encode("utf-8")) - 1
    rendered = render_snapshot(snapshot, parser_args.section, parser_args.offset, budget)
    print(rendered + suffix)
    return 0


def main(argv=None):
    parser = BoundedParser(description=__doc__)
    parser.add_argument("--repo")
    parser.add_argument("--pr", type=int)
    parser.add_argument("--hostname")
    parser.add_argument("--artifact")
    parser.add_argument("--refresh")
    parser.add_argument("--section", default="overview")
    parser.add_argument("--offset", type=int, default=0)
    parser.add_argument("--limit", type=int, default=MAX_OUTPUT)
    try:
        return run(argv, parser.parse_args(argv))
    except OSError:
        print("pr_context: private artifact or GitHub I/O failed", file=sys.stderr)
        return 1
    except (ValueError, RuntimeError, KeyError, TypeError) as exc:
        message = "".join(c if c.isprintable() else "?" for c in str(exc))[:500]
        print(f"pr_context: {message}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
