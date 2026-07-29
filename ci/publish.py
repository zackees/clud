#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.11"
# ///
"""Dispatch the repository's single authoritative release workflow."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import tomllib

ROOT = Path(__file__).resolve().parent.parent
RELEASE_WORKFLOW = "auto-release.yml"


def log(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


def run(command: list[str], **kwargs: Any) -> subprocess.CompletedProcess[Any]:
    log(f"  $ {' '.join(command)}")
    return subprocess.run(command, check=True, **kwargs)


def run_capture(command: list[str]) -> str:
    result = run(command, capture_output=True, text=True, errors="replace")
    return result.stdout.strip()


def run_capture_allow_failure(command: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, capture_output=True, text=True, errors="replace")


def read_project_meta() -> tuple[str, str]:
    with (ROOT / "pyproject.toml").open("rb") as handle:
        project = tomllib.load(handle)["project"]
    return project["name"], project["version"]


def detect_repo() -> str:
    url = run_capture(["git", "remote", "get-url", "origin"])
    if url.startswith("git@"):
        url = url.split(":", 1)[1]
    elif "github.com/" in url:
        url = url.split("github.com/", 1)[1]
    return url.removesuffix(".git")


def detect_publish_ref() -> str:
    current = run_capture(["git", "rev-parse", "--abbrev-ref", "HEAD"])
    upstream = run_capture_allow_failure(
        ["git", "rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]
    )
    if upstream.returncode == 0 and upstream.stdout.strip().startswith("origin/"):
        return upstream.stdout.strip().removeprefix("origin/")
    return current


def ensure_clean_and_pushed() -> None:
    dirty = run_capture(["git", "status", "--porcelain"])
    if dirty:
        raise SystemExit(f"working tree is dirty:\n{dirty}")

    local_sha = run_capture(["git", "rev-parse", "HEAD"])
    remote_sha = run_capture(["git", "rev-parse", "@{u}"])
    if local_sha != remote_sha:
        raise SystemExit(
            f"local HEAD {local_sha[:12]} differs from upstream "
            f"{remote_sha[:12]}; push first"
        )


def trigger(repo: str, version: str) -> int:
    branch = detect_publish_ref()
    existing_raw = run_capture(
        [
            "gh",
            "run",
            "list",
            "--repo",
            repo,
            "--workflow",
            RELEASE_WORKFLOW,
            "--limit",
            "5",
            "--json",
            "databaseId",
        ]
    )
    existing = {row["databaseId"] for row in json.loads(existing_raw or "[]")}

    run(
        [
            "gh",
            "workflow",
            "run",
            RELEASE_WORKFLOW,
            "--repo",
            repo,
            "--ref",
            branch,
            "-f",
            f"tag={version}",
        ]
    )

    for _ in range(30):
        time.sleep(2)
        result = run_capture(
            [
                "gh",
                "run",
                "list",
                "--repo",
                repo,
                "--workflow",
                RELEASE_WORKFLOW,
                "--limit",
                "10",
                "--json",
                "databaseId,status",
            ]
        )
        for row in json.loads(result or "[]"):
            if row["databaseId"] not in existing:
                return int(row["databaseId"])
    raise SystemExit(f"timed out waiting for {RELEASE_WORKFLOW} to start")


def wait_for_run(repo: str, run_id: int) -> None:
    started = time.time()
    while True:
        state = json.loads(
            run_capture(
                [
                    "gh",
                    "run",
                    "view",
                    str(run_id),
                    "--repo",
                    repo,
                    "--json",
                    "status,conclusion,url",
                ]
            )
        )
        if state["status"] == "completed":
            if state.get("conclusion") != "success":
                raise SystemExit(
                    f"release failed: {state.get('conclusion')} "
                    f"{state.get('url', '')}".rstrip()
                )
            log(f"  {RELEASE_WORKFLOW} completed in {int(time.time() - started)}s")
            return
        time.sleep(15)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Publish clud through GitHub Actions")
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="describe the release dispatch without starting a workflow",
    )
    args = parser.parse_args(argv)
    name, version = read_project_meta()

    if args.dry_run:
        log(
            f"Dry run: would dispatch {RELEASE_WORKFLOW} for {name} {version}; "
            "no workflow was started and nothing was published."
        )
        return 0

    try:
        run_capture(["gh", "--version"])
    except FileNotFoundError as exc:
        raise SystemExit("gh CLI is required for remote publish flow") from exc

    ensure_clean_and_pushed()
    repo = detect_repo()
    log(f"Publishing {name} {version} through {RELEASE_WORKFLOW}")
    wait_for_run(repo, trigger(repo, version))
    return 0


if __name__ == "__main__":
    sys.exit(main())
