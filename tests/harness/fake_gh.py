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
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path


def _load() -> tuple[Path, dict]:
    path = Path(os.environ["FAKE_GH_STATE"])
    return path, json.loads(path.read_text(encoding="utf-8"))


def _flag(argv: list[str], name: str) -> str | None:
    for i, arg in enumerate(argv):
        if arg == name and i + 1 < len(argv):
            return argv[i + 1]
        if arg.startswith(name + "="):
            return arg.split("=", 1)[1]
    return None


def _issue_json(state: dict, number: str) -> dict | None:
    issue = state.get("issues", {}).get(number)
    if issue is None:
        return None
    return {
        "number": int(number),
        "title": issue.get("title", ""),
        "body": issue.get("body", ""),
        "state": issue.get("state", "open").upper(),
        "url": f"https://github.com/{state.get('repo', 'o/r')}/issues/{number}",
    }


def main(argv: list[str]) -> int:
    path, state = _load()
    state.setdefault("calls", []).append(argv)
    path.write_text(json.dumps(state, indent=1), encoding="utf-8")
    if state.get("fail"):
        print("fake gh: simulated failure (not signed in)", file=sys.stderr)
        return 1

    if argv[:1] == ["api"] and len(argv) > 1:
        endpoint = argv[1].split("?", 1)[0]
        parts = endpoint.strip("/").split("/")
        if (
            len(parts) == 6
            and parts[0] == "repos"
            and parts[3] == "issues"
            and parts[5] == "sub_issues"
        ):
            issue = state.get("issues", {}).get(parts[4])
            if issue is None:
                print("gh: Not Found (HTTP 404)", file=sys.stderr)
                return 1
            print(
                json.dumps(
                    [
                        {"number": s["number"], "state": s.get("state", "open")}
                        for s in issue.get("sub_issues", [])
                    ]
                )
            )
            return 0
        print(json.dumps({}))
        return 0

    if argv[:2] == ["issue", "view"] and len(argv) > 2:
        number = argv[2].rstrip("/").rsplit("/", 1)[-1]
        issue = _issue_json(state, number)
        if issue is None:
            print(f"gh: issue {number} not found", file=sys.stderr)
            return 1
        if "--json" in " ".join(argv):
            print(json.dumps(issue))
        else:
            print(f"title:\t{issue['title']}\nstate:\t{issue['state']}\n--\n{issue['body']}")
        return 0

    if argv[:2] == ["pr", "list"]:
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
                        "url": f"https://github.com/{state.get('repo', 'o/r')}/pull/{p['number']}",
                    }
                    for p in prs
                ]
            )
        )
        return 0

    if argv[:2] == ["pr", "create"]:
        prs = state.setdefault("prs", [])
        number = max([p["number"] for p in prs] + [100]) + 1
        head = _flag(argv, "--head") or os.environ.get("FAKE_GH_HEAD", "HEAD")
        prs.append(
            {"number": number, "head": head, "state": "OPEN", "title": _flag(argv, "--title") or ""}
        )
        path.write_text(json.dumps(state, indent=1), encoding="utf-8")
        print(f"https://github.com/{state.get('repo', 'o/r')}/pull/{number}")
        return 0

    if argv[:2] in (["pr", "merge"], ["pr", "view"], ["pr", "checks"]):
        target = argv[2] if len(argv) > 2 else ""
        number = (
            int(target.rstrip("/").rsplit("/", 1)[-1])
            if target.rstrip("/").rsplit("/", 1)[-1].isdigit()
            else None
        )
        pr = next(
            (
                p
                for p in state.get("prs", [])
                if (number is not None and p["number"] == number)
                or (number is None and p.get("head") == target)
            ),
            None,
        )
        if pr is None:
            print(f"gh: pull request {target} not found", file=sys.stderr)
            return 1
        if argv[1] == "merge":
            pr["state"] = "MERGED"
            path.write_text(json.dumps(state, indent=1), encoding="utf-8")
            print(f"Merged pull request #{number}")
        elif argv[1] == "checks":
            print("All checks were successful")
        else:
            print(
                json.dumps({"number": number, "state": pr["state"], "headRefName": pr.get("head")})
            )
        return 0

    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
