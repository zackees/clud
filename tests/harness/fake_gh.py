"""A stand-in for the GitHub CLI, backed by a JSON state file.

The real-harness tests (#1323) put a `gh` wrapper that runs this file first on
PATH. Everything a test world needs from GitHub lives in ``$FAKE_GH_STATE``:

    {"repo": "o/r",
     "issues": {"7": {"title": "...", "body": "...", "state": "open",
                      "sub_issues": [{"number": 8, "state": "open"}]}},
     "prs": [{"number": 1, "head": "feat/x", "state": "MERGED", "title": "..."}],
     "fail": false,
     "calls": []}

Every invocation is appended to ``calls``. ``"fail": true`` makes every call
exit 1, to exercise "gh is broken" paths.

Optional additions (#1402); every key is defaulted, so the shape above still
works unchanged:

- top level ``default_branch`` (default ``"main"``), ``next_id`` (comment id
  counter), and ``faults``: ``{"<key>": {"code": 1, "stderr": "...",
  "times": N}}``. The key is the first two argv words (``"issue close"``), or
  ``"api <METHOD>"`` / ``"api graphql"`` for ``api``. A fault is consumed per
  matching call (after the call is recorded, before dispatch); without
  ``times`` it fires forever.
- per issue: ``labels`` (list of str), ``comments`` (``[{"id", "body"}]``),
  ``closed_by`` (``{"kind": "pr"|"user"|"command", "pr": int|None}``) and
  ``parent`` (int or None).
- per PR: ``base``, ``draft``, ``body``, ``merge_method``, ``admin``,
  ``reviews_required`` and ``approved``.

Supported commands: ``issue view|create|comment|edit|close|reopen|list``,
``pr list|create|edit|ready|merge|view|checks``, ``repo view`` (name and
``defaultBranchRef``) and ``api`` (sub-issue GET,
POST, DELETE with GitHub's one-parent / 100-children / 8-levels rules; issue
comment GET/PATCH; a ``graphql`` ClosedEvent closer stub). ``api`` takes
``-X/--method``, ``-f/-F field=value`` and ``--input file``; with no method it
is POST when fields are given, else GET. Bodies over 65536 characters are
rejected. Merging a PR into ``default_branch`` closes the issues its title or
body references with a closing keyword; merges into other bases close nothing.
"""

from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path

BODY_LIMIT = 65536
SUB_ISSUE_LIMIT = 100
DEPTH_LIMIT = 8
_CLOSING = re.compile(r"\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s*:?\s+#(\d+)", re.IGNORECASE)


def _load() -> tuple[Path, dict]:
    path = Path(os.environ["FAKE_GH_STATE"])
    return path, json.loads(path.read_text(encoding="utf-8"))


def _save(path: Path, state: dict) -> None:
    path.write_text(json.dumps(state, indent=1), encoding="utf-8")


def _flag(argv: list[str], name: str) -> str | None:
    for i, arg in enumerate(argv):
        if arg == name and i + 1 < len(argv):
            return argv[i + 1]
        if arg.startswith(name + "="):
            return arg.split("=", 1)[1]
    return None


def _flags(argv: list[str], *names: str) -> list[str]:
    out = []
    for i, arg in enumerate(argv):
        for name in names:
            if arg == name and i + 1 < len(argv):
                out.append(argv[i + 1])
            elif name.startswith("--") and arg.startswith(name + "="):
                out.append(arg.split("=", 1)[1])
    return out


def _split_labels(values: list[str]) -> list[str]:
    return [p.strip() for v in values for p in v.split(",") if p.strip()]


def _number(target: str) -> str:
    return target.rstrip("/").rsplit("/", 1)[-1].lstrip("#")


def _repo(state: dict) -> str:
    return state.get("repo", "o/r")


def _issue_url(state: dict, number) -> str:
    return f"https://github.com/{_repo(state)}/issues/{number}"


def _too_long(body: str | None) -> bool:
    if body is not None and len(body) > BODY_LIMIT:
        print(
            f"gh: body is too long (maximum is {BODY_LIMIT} characters) (HTTP 422)",
            file=sys.stderr,
        )
        return True
    return False


def _issue_json(state: dict, number: str) -> dict | None:
    issue = state.get("issues", {}).get(number)
    if issue is None:
        return None
    return {
        "number": int(number),
        "title": issue.get("title", ""),
        "body": issue.get("body", ""),
        "state": issue.get("state", "open").upper(),
        "url": _issue_url(state, number),
        "labels": [{"name": n} for n in issue.get("labels", [])],
        "comments": [
            {"id": c["id"], "body": c.get("body", "")} for c in issue.get("comments", [])
        ],
    }


def _next_comment_id(state: dict) -> int:
    cid = int(state.get("next_id", 1000))
    state["next_id"] = cid + 1
    return cid


def _find_comment(state: dict, cid: int) -> dict | None:
    for issue in state.get("issues", {}).values():
        for c in issue.get("comments", []):
            if c.get("id") == cid:
                return c
    return None


def _set_issue_state(state: dict, number: str, new_state: str) -> None:
    issue = state["issues"][number]
    issue["state"] = new_state
    parent = issue.get("parent")
    if parent is not None:
        for s in state["issues"].get(str(parent), {}).get("sub_issues", []):
            if str(s["number"]) == number:
                s["state"] = new_state


def _close(state: dict, number: str, closed_by: dict) -> None:
    _set_issue_state(state, number, "closed")
    state["issues"][number]["closed_by"] = closed_by


def _fault_key(argv: list[str]) -> str:
    if argv[:1] == ["api"]:
        if len(argv) > 1 and argv[1] == "graphql":
            return "api graphql"
        return "api " + _api_method(argv)
    return " ".join(argv[:2])


def _api_method(argv: list[str]) -> str:
    method = _flag(argv, "-X") or _flag(argv, "--method")
    if method:
        return method.upper()
    if _flags(argv, "-f", "-F", "--field", "--raw-field") or _flag(argv, "--input"):
        return "POST"
    return "GET"


def _api_fields(argv: list[str]) -> dict:
    fields: dict = {}
    source = _flag(argv, "--input")
    if source:
        text = sys.stdin.read() if source == "-" else Path(source).read_text(encoding="utf-8")
        fields.update(json.loads(text))
    for raw in _flags(argv, "-f", "--raw-field"):
        key, _, value = raw.partition("=")
        fields[key] = value
    for raw in _flags(argv, "-F", "--field"):
        key, _, value = raw.partition("=")
        if value in ("true", "false"):
            fields[key] = value == "true"
        elif value.lstrip("-").isdigit():
            fields[key] = int(value)
        elif value == "null":
            fields[key] = None
        else:
            fields[key] = value
    return fields


def _depth(state: dict, number: str) -> int:
    """Levels from the root down to ``number`` (a root is level 1)."""
    depth, seen = 1, {number}
    parent = state["issues"].get(number, {}).get("parent")
    while parent is not None and str(parent) not in seen:
        seen.add(str(parent))
        depth += 1
        parent = state["issues"].get(str(parent), {}).get("parent")
    return depth


def _height(state: dict, number: str, seen: set | None = None) -> int:
    """Levels in the subtree rooted at ``number`` (a leaf is 1)."""
    seen = seen or set()
    if number in seen:
        return 0
    seen.add(number)
    subs = state["issues"].get(number, {}).get("sub_issues", [])
    return 1 + max((_height(state, str(s["number"]), seen) for s in subs), default=0)


def _unprocessable(message: str) -> int:
    print(f"gh: {message} (HTTP 422)", file=sys.stderr)
    return 1


def _detach(state: dict, child: str) -> None:
    issue = state["issues"][child]
    parent = issue.get("parent")
    if parent is not None and str(parent) in state["issues"]:
        p = state["issues"][str(parent)]
        p["sub_issues"] = [s for s in p.get("sub_issues", []) if str(s["number"]) != child]
    issue["parent"] = None


def _api(path: Path, state: dict, argv: list[str]) -> int:
    issues = state.setdefault("issues", {})
    if argv[1] == "graphql":
        fields = _api_fields(argv)
        query = str(fields.get("query", ""))
        if "ClosedEvent" in query or "closer" in query:
            number = fields.get("number") or fields.get("issue")
            if number is None:
                m = re.search(r"issue\s*\(\s*number\s*:\s*(\d+)", query)
                number = m.group(1) if m else None
            issue = issues.get(str(number)) if number is not None else None
            if issue is None:
                print(json.dumps({"data": {"repository": {"issue": None}}}))
                return 0
            nodes = []
            if issue.get("state", "open") == "closed":
                by = issue.get("closed_by") or {}
                if by.get("kind") == "pr" and by.get("pr") is not None:
                    closer_pr = next(
                        (p for p in state.get("prs", []) if p["number"] == by["pr"]), {}
                    )
                    nodes.append(
                        {
                            "closer": {
                                "__typename": "PullRequest",
                                "number": by["pr"],
                                "merged": closer_pr.get("state") == "MERGED",
                                "baseRefName": closer_pr.get(
                                    "base", state.get("default_branch", "main")
                                ),
                            }
                        }
                    )
                else:
                    nodes.append({"closer": None})
            print(
                json.dumps(
                    {"data": {"repository": {"issue": {"timelineItems": {"nodes": nodes}}}}}
                )
            )
            return 0
        print(json.dumps({"data": {}}))
        return 0

    method = _api_method(argv)
    endpoint = next(
        (a for a in argv[1:] if not a.startswith("-") and "/" in a and "=" not in a), argv[1]
    )
    parts = endpoint.split("?", 1)[0].strip("/").split("/")

    # repos/o/r/issues/comments/{id}
    if len(parts) == 6 and parts[0] == "repos" and parts[3] == "issues" and parts[4] == "comments":
        comment = _find_comment(state, int(parts[5])) if parts[5].isdigit() else None
        if comment is None:
            print("gh: Not Found (HTTP 404)", file=sys.stderr)
            return 1
        if method == "PATCH":
            body = _api_fields(argv).get("body")
            if _too_long(body):
                return 1
            if body is not None:
                comment["body"] = body
            _save(path, state)
        print(json.dumps({"id": comment["id"], "body": comment.get("body", "")}))
        return 0

    # repos/o/r/issues/N/sub_issues and .../sub_issue
    if (
        len(parts) == 6
        and parts[0] == "repos"
        and parts[3] == "issues"
        and parts[5] in ("sub_issues", "sub_issue")
    ):
        issue = issues.get(parts[4])
        if issue is None:
            print("gh: Not Found (HTTP 404)", file=sys.stderr)
            return 1
        if method == "GET":
            print(
                json.dumps(
                    [
                        {"number": s["number"], "state": s.get("state", "open")}
                        for s in issue.get("sub_issues", [])
                    ]
                )
            )
            return 0
        fields = _api_fields(argv)
        child = str(fields.get("sub_issue_id", ""))
        if child not in issues:
            print("gh: Not Found (HTTP 404)", file=sys.stderr)
            return 1
        if method == "DELETE":
            if str(issues[child].get("parent")) != parts[4]:
                print("gh: Not Found (HTTP 404)", file=sys.stderr)
                return 1
            _detach(state, child)
            _save(path, state)
            print(json.dumps({"number": int(parts[4])}))
            return 0
        if method == "POST":
            replace = fields.get("replace_parent") in (True, "true")
            if child == parts[4]:
                return _unprocessable("an issue cannot be its own sub-issue")
            current = issues[child].get("parent")
            if current is not None and not replace:
                return _unprocessable("Validation Failed: sub-issue may only have one parent")
            if len(issue.get("sub_issues", [])) >= SUB_ISSUE_LIMIT:
                return _unprocessable(
                    f"Validation Failed: parent may only have {SUB_ISSUE_LIMIT} sub-issues"
                )
            if _depth(state, parts[4]) + _height(state, child) > DEPTH_LIMIT:
                return _unprocessable(
                    f"Validation Failed: sub-issues may only be nested {DEPTH_LIMIT} levels deep"
                )
            if current is not None:
                _detach(state, child)
            issue.setdefault("sub_issues", []).append(
                {"number": int(child), "state": issues[child].get("state", "open")}
            )
            issues[child]["parent"] = int(parts[4])
            _save(path, state)
            print(json.dumps({"number": int(parts[4])}))
            return 0

    print(json.dumps({}))
    return 0


def _issue(path: Path, state: dict, argv: list[str]) -> int:
    issues = state.setdefault("issues", {})
    sub = argv[1] if len(argv) > 1 else ""

    if sub == "create":
        body = _flag(argv, "--body") or ""
        if _too_long(body):
            return 1
        taken = [int(k) for k in issues if k.isdigit()] + [
            p["number"] for p in state.get("prs", [])
        ]
        number = str(max([*taken, 0]) + 1)
        issues[number] = {
            "title": _flag(argv, "--title") or "",
            "body": body,
            "state": "open",
            "labels": _split_labels(_flags(argv, "--label", "-l")),
            "comments": [],
            "sub_issues": [],
            "parent": None,
            "closed_by": None,
        }
        _save(path, state)
        print(_issue_url(state, number))
        return 0

    if sub == "list":
        labels = _split_labels(_flags(argv, "--label", "-l"))
        wanted = (_flag(argv, "--state") or _flag(argv, "-s") or "open").lower()
        out = []
        for key in sorted(issues, key=lambda k: int(k) if k.isdigit() else 0):
            issue = issues[key]
            if wanted != "all" and issue.get("state", "open").lower() != wanted:
                continue
            if any(lbl not in issue.get("labels", []) for lbl in labels):
                continue
            out.append(
                {
                    "number": int(key),
                    "title": issue.get("title", ""),
                    "state": issue.get("state", "open").upper(),
                    "labels": [{"name": n} for n in issue.get("labels", [])],
                    "url": _issue_url(state, key),
                }
            )
        print(json.dumps(out))
        return 0

    if len(argv) < 3:
        print(f"gh: issue {sub} needs an issue number", file=sys.stderr)
        return 1
    number = _number(argv[2])
    issue = issues.get(number)
    if issue is None:
        print(f"gh: issue {number} not found", file=sys.stderr)
        return 1

    if sub == "view":
        data = _issue_json(state, number)
        if "--json" in " ".join(argv):
            print(json.dumps(data))
        else:
            print(f"title:\t{data['title']}\nstate:\t{data['state']}\n--\n{data['body']}")
        return 0

    if sub == "comment":
        body = _flag(argv, "--body") or _flag(argv, "-b") or ""
        if _too_long(body):
            return 1
        cid = _next_comment_id(state)
        issue.setdefault("comments", []).append({"id": cid, "body": body})
        _save(path, state)
        print(f"{_issue_url(state, number)}#issuecomment-{cid}")
        return 0

    if sub == "edit":
        body = _flag(argv, "--body")
        if _too_long(body):
            return 1
        if body is not None:
            issue["body"] = body
        title = _flag(argv, "--title")
        if title is not None:
            issue["title"] = title
        labels = issue.setdefault("labels", [])
        for lbl in _split_labels(_flags(argv, "--add-label")):
            if lbl not in labels:
                labels.append(lbl)
        remove = set(_split_labels(_flags(argv, "--remove-label")))
        issue["labels"] = [lbl for lbl in labels if lbl not in remove]
        _save(path, state)
        print(_issue_url(state, number))
        return 0

    if sub == "close":
        comment = _flag(argv, "--comment") or _flag(argv, "-c")
        if _too_long(comment):
            return 1
        if comment is not None:
            issue.setdefault("comments", []).append(
                {"id": _next_comment_id(state), "body": comment}
            )
        _close(state, number, {"kind": "command", "pr": None})
        _save(path, state)
        print(f"Closed issue #{number}")
        return 0

    if sub == "reopen":
        _set_issue_state(state, number, "open")
        issue["closed_by"] = None
        _save(path, state)
        print(f"Reopened issue #{number}")
        return 0

    return 0


def _find_pr(state: dict, target: str) -> dict | None:
    tail = _number(target)
    number = int(tail) if tail.isdigit() else None
    return next(
        (
            p
            for p in state.get("prs", [])
            if (number is not None and p["number"] == number)
            or (number is None and p.get("head") == target)
        ),
        None,
    )


def _pr(path: Path, state: dict, argv: list[str]) -> int:
    sub = argv[1] if len(argv) > 1 else ""
    default_branch = state.get("default_branch", "main")

    if sub == "list":
        head = _flag(argv, "--head")
        wanted = (_flag(argv, "--state") or "open").lower()
        prs = [
            p
            for p in state.get("prs", [])
            if (head is None or p.get("head") == head)
            and (wanted == "all" or p.get("state", "OPEN").lower() == wanted)
        ]
        print(
            json.dumps(
                [
                    {
                        "number": p["number"],
                        "headRefName": p.get("head"),
                        "state": p.get("state", "OPEN"),
                        "title": p.get("title", ""),
                        "url": f"https://github.com/{_repo(state)}/pull/{p['number']}",
                    }
                    for p in prs
                ]
            )
        )
        return 0

    if sub == "create":
        body = _flag(argv, "--body") or _flag(argv, "-b") or ""
        if _too_long(body):
            return 1
        prs = state.setdefault("prs", [])
        number = max([p["number"] for p in prs] + [100]) + 1
        head = _flag(argv, "--head") or os.environ.get("FAKE_GH_HEAD", "HEAD")
        prs.append(
            {
                "number": number,
                "head": head,
                "state": "OPEN",
                "title": _flag(argv, "--title") or "",
                "base": _flag(argv, "--base") or _flag(argv, "-B") or default_branch,
                "draft": "--draft" in argv or "-d" in argv,
                "body": body,
            }
        )
        _save(path, state)
        print(f"https://github.com/{_repo(state)}/pull/{number}")
        return 0

    target = argv[2] if len(argv) > 2 and not argv[2].startswith("-") else ""
    pr = _find_pr(state, target)
    if pr is None:
        print(f"gh: pull request {target} not found", file=sys.stderr)
        return 1
    number = pr["number"]

    if sub == "edit":
        body = _flag(argv, "--body")
        if _too_long(body):
            return 1
        if body is not None:
            pr["body"] = body
        title = _flag(argv, "--title")
        if title is not None:
            pr["title"] = title
        base = _flag(argv, "--base")
        if base is not None:
            pr["base"] = base
        _save(path, state)
        print(f"https://github.com/{_repo(state)}/pull/{number}")
        return 0

    if sub == "ready":
        pr["draft"] = False
        _save(path, state)
        print(f"Pull request #{number} is marked as ready for review")
        return 0

    if sub == "merge":
        admin = "--admin" in argv
        if pr.get("draft"):
            print(f"gh: pull request #{number} is still a draft", file=sys.stderr)
            return 1
        if pr.get("reviews_required") and not pr.get("approved") and not admin:
            print(
                f"gh: pull request #{number} is not mergeable: "
                "the base branch policy prohibits the merge (review required)",
                file=sys.stderr,
            )
            return 1
        method = next(
            (m for m in ("merge", "squash", "rebase") if f"--{m}" in argv or f"-{m[0]}" in argv),
            None,
        )
        pr["merge_method"] = method
        pr["admin"] = admin
        pr["state"] = "MERGED"
        if pr.get("base", default_branch) == default_branch:
            text = f"{pr.get('title', '')}\n{pr.get('body', '')}"
            for ref in _CLOSING.findall(text):
                if ref in state.get("issues", {}):
                    _close(state, ref, {"kind": "pr", "pr": number})
        _save(path, state)
        print(f"Merged pull request #{number}")
        return 0

    if sub == "checks":
        print("All checks were successful")
        return 0

    if sub == "view":
        print(
            json.dumps(
                {
                    "number": number,
                    "state": pr["state"],
                    "headRefName": pr.get("head"),
                    "baseRefName": pr.get("base", default_branch),
                    "isDraft": bool(pr.get("draft", False)),
                    "body": pr.get("body", ""),
                    "title": pr.get("title", ""),
                }
            )
        )
        return 0

    return 0


def main(argv: list[str]) -> int:
    path, state = _load()
    state.setdefault("calls", []).append(argv)
    _save(path, state)
    if state.get("fail"):
        print("fake gh: simulated failure (not signed in)", file=sys.stderr)
        return 1

    fault = state.get("faults", {}).get(_fault_key(argv))
    if fault is not None and fault.get("times", 1) != 0:
        if "times" in fault:
            fault["times"] = int(fault["times"]) - 1
        _save(path, state)
        print(fault.get("stderr", "fake gh: injected fault"), file=sys.stderr)
        return int(fault.get("code", 1))

    if argv[:1] == ["api"] and len(argv) > 1:
        return _api(path, state, argv)
    if argv[:1] == ["issue"]:
        return _issue(path, state, argv)
    if argv[:1] == ["pr"]:
        return _pr(path, state, argv)
    if argv[:2] == ["repo", "view"]:
        print(
            json.dumps(
                {
                    "nameWithOwner": _repo(state),
                    "defaultBranchRef": {"name": state.get("default_branch", "main")},
                }
            )
        )
        return 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
