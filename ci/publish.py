#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# ///
from __future__ import annotations

import argparse
import fnmatch
import json
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

import tomllib

ROOT = Path(__file__).resolve().parent.parent
DIST_DIR = ROOT / "dist"
WORKFLOWS = {
    "linux-x86-build.yml": "wheels-linux-x86",
    "linux-arm-build.yml": "wheels-linux-arm",
    "windows-x86-build.yml": "wheels-windows-x86",
    "windows-arm-build.yml": "wheels-windows-arm",
    "macos-x86-build.yml": "wheels-macos-x86",
    "macos-arm-build.yml": "wheels-macos-arm",
}
EXPECTED_ARTIFACT_GLOBS = (
    "{name}-{version}.tar.gz",
    "{name}-{version}-*linux*_x86_64.whl",
    "{name}-{version}-*linux*_aarch64.whl",
    "{name}-{version}-*-win_amd64.whl",
    "{name}-{version}-*-win_arm64.whl",
    "{name}-{version}-*-macosx*_x86_64.whl",
    "{name}-{version}-*-macosx*_arm64.whl",
)


def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def run(cmd: list[str], **kwargs: Any) -> subprocess.CompletedProcess[Any]:
    log(f"  $ {' '.join(cmd)}")
    return subprocess.run(cmd, check=True, **kwargs)


def _captured_text_kwargs() -> dict[str, Any]:
    return {
        "capture_output": True,
        "text": True,
        "errors": "replace",
    }


def run_capture(cmd: list[str]) -> str:
    result: subprocess.CompletedProcess[str] = run(cmd, **_captured_text_kwargs())
    return result.stdout.strip()


def run_capture_allow_failure(cmd: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(cmd, **_captured_text_kwargs())


def read_project_meta() -> tuple[str, str]:
    with open(ROOT / "pyproject.toml", "rb") as f:
        data = tomllib.load(f)
    project = data["project"]
    return project["name"], project["version"]


def detect_repo() -> str:
    url = run_capture(["git", "remote", "get-url", "origin"])
    if url.startswith("git@"):
        url = url.split(":", 1)[1]
    elif "github.com/" in url:
        url = url.split("github.com/", 1)[1]
    return url.removesuffix(".git")


def check_pypi_version(name: str, version: str) -> None:
    existing = existing_pypi_files(name, version)
    if existing is None:
        return
    if existing:
        log(
            f"{name} {version} already exists on PyPI with {len(existing)} file(s); "
            "will upload only missing artifacts"
        )


def existing_pypi_files(name: str, version: str) -> set[str] | None:
    url = f"https://pypi.org/pypi/{name}/json"
    try:
        with urllib.request.urlopen(url, timeout=10) as resp:
            data = json.loads(resp.read())
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            return None
        raise
    release = data.get("releases", {}).get(version)
    if release is None:
        return None
    return {file["filename"] for file in release}


def ensure_clean_and_pushed() -> None:
    dirty = run_capture(["git", "status", "--porcelain"])
    if dirty:
        raise SystemExit(f"working tree is dirty:\n{dirty}")

    local_sha = run_capture(["git", "rev-parse", "HEAD"])
    remote_sha = run_capture(["git", "rev-parse", "@{u}"])
    if local_sha != remote_sha:
        raise SystemExit(
            f"local HEAD {local_sha[:12]} differs from upstream {remote_sha[:12]}; push first"
        )


def trigger(repo: str, workflow_file: str) -> int:
    branch = run_capture(["git", "rev-parse", "--abbrev-ref", "HEAD"])
    existing_raw = run_capture(
        [
            "gh",
            "run",
            "list",
            "--repo",
            repo,
            "--workflow",
            workflow_file,
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
            workflow_file,
            "--repo",
            repo,
            "--ref",
            branch,
            "-f",
            "build-mode=release",
        ]
    )

    run_id: int | None = None
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
                workflow_file,
                "--limit",
                "10",
                "--json",
                "databaseId,status",
            ]
        )
        for row in json.loads(result or "[]"):
            if row["databaseId"] not in existing:
                run_id = row["databaseId"]
                break
        if run_id is not None:
            return run_id
    raise SystemExit(f"timed out waiting for {workflow_file} to start")


def wait_for_run(repo: str, workflow_file: str, run_id: int) -> int:
    started = time.time()
    while True:
        result = run_capture(
            [
                "gh",
                "run",
                "view",
                str(run_id),
                "--repo",
                repo,
                "--json",
                "status,conclusion",
            ]
        )
        state = json.loads(result)
        if state["status"] == "completed":
            if state.get("conclusion") != "success":
                display_failure_logs(repo, run_id, workflow_file)
                raise SystemExit(
                    f"remote build failed: {state.get('conclusion')} "
                    f"https://github.com/{repo}/actions/runs/{run_id}"
                )
            log(f"  {workflow_file} completed in {int(time.time() - started)}s")
            return run_id
        time.sleep(15)


def display_failure_logs(repo: str, run_id: int, workflow_file: str) -> None:
    logs_dir = DIST_DIR / "logs"
    logs_dir.mkdir(parents=True, exist_ok=True)

    artifact_download = run_capture_allow_failure(
        [
            "gh",
            "run",
            "download",
            str(run_id),
            "--repo",
            repo,
            "--pattern",
            "failure-logs-*",
            "--dir",
            str(logs_dir),
        ]
    )
    if artifact_download.returncode == 0:
        log(f"  Downloaded failure log artifacts to {logs_dir}")
        for path in sorted(logs_dir.rglob("*.log")):
            log(f"\n  --- {workflow_file} :: {path.name} ---")
            content = path.read_text(encoding="utf-8", errors="replace").splitlines()
            for line in content[-60:]:
                log(f"  | {line}")
        return

    result = run_capture_allow_failure(
        ["gh", "run", "view", str(run_id), "--repo", repo, "--log-failed"]
    )
    if result.stdout:
        log(f"\n  --- {workflow_file} failed log ---")
        for line in result.stdout.splitlines()[-120:]:
            log(f"  | {line}")


def download_artifacts(repo: str, runs: dict[str, int]) -> list[Path]:
    if DIST_DIR.exists():
        shutil.rmtree(DIST_DIR)
    DIST_DIR.mkdir(parents=True)
    temp = DIST_DIR / "_tmp"
    temp.mkdir()

    for workflow_file, run_id in runs.items():
        artifact_name = WORKFLOWS[workflow_file]
        workflow_temp = temp / workflow_file.removesuffix(".yml")
        workflow_temp.mkdir()
        run(
            [
                "gh",
                "run",
                "download",
                str(run_id),
                "--repo",
                repo,
                "--pattern",
                artifact_name,
                "--dir",
                str(workflow_temp),
            ]
        )

        artifact_dir = workflow_temp / artifact_name
        if not artifact_dir.is_dir():
            raise SystemExit(
                f"{workflow_file} did not produce expected artifact directory {artifact_name}"
            )

    built: list[Path] = []
    for workflow_file, artifact_name in WORKFLOWS.items():
        artifact_dir = temp / workflow_file.removesuffix(".yml") / artifact_name
        for path in artifact_dir.rglob("*"):
            if not path.is_file():
                continue
            if path.suffix not in {".whl", ".gz"}:
                continue
            target = DIST_DIR / path.name
            shutil.copy2(path, target)
            built.append(target)

    shutil.rmtree(temp)

    wheels = [path for path in built if path.suffix == ".whl"]
    sdists = [path for path in built if path.name.endswith(".tar.gz")]
    if len(wheels) < len(WORKFLOWS):
        raise SystemExit(f"expected at least {len(WORKFLOWS)} wheels, found {len(wheels)}")
    if len(sdists) != 1:
        raise SystemExit(f"expected exactly one sdist, found {len(sdists)}")
    return sorted(built)


def expected_artifact_globs(name: str, version: str) -> list[str]:
    return [pattern.format(name=name, version=version) for pattern in EXPECTED_ARTIFACT_GLOBS]


def select_expected_artifacts(
    artifacts: list[Path], *, name: str, version: str
) -> tuple[list[Path], list[str]]:
    matched: list[Path] = []
    missing: list[str] = []
    by_name = {path.name: path for path in artifacts if path.exists()}
    for pattern in expected_artifact_globs(name, version):
        names = sorted(filename for filename in by_name if fnmatch.fnmatch(filename, pattern))
        if not names:
            missing.append(pattern)
            continue
        matched.append(by_name[names[0]])
    return matched, missing


def filter_missing_artifacts(artifacts: list[Path], existing_files: set[str]) -> list[Path]:
    missing = [path for path in artifacts if path.name not in existing_files]
    if missing:
        log("Artifacts missing from PyPI:")
        for path in missing:
            log(f"  {path.name}")
    else:
        log("All artifacts for this version are already present on PyPI")
    return missing


def main() -> int:
    parser = argparse.ArgumentParser(description="Publish clud from remote GitHub builds")
    parser.add_argument("--dry-run", action="store_true", help="build remotely but do not upload")
    args = parser.parse_args()

    try:
        run_capture(["gh", "--version"])
    except FileNotFoundError as exc:
        raise SystemExit("gh CLI is required for remote publish flow") from exc

    name, version = read_project_meta()
    ensure_clean_and_pushed()
    existing_files: set[str] = set()
    if not args.dry_run:
        check_pypi_version(name, version)
        existing_files = existing_pypi_files(name, version) or set()

    repo = detect_repo()
    log(f"Publishing {name} {version} via remote GitHub builds")
    triggered = {workflow_file: trigger(repo, workflow_file) for workflow_file in WORKFLOWS}
    runs = {
        workflow_file: wait_for_run(repo, workflow_file, run_id)
        for workflow_file, run_id in triggered.items()
    }
    artifacts = download_artifacts(repo, runs)
    expected_artifacts, missing_expected = select_expected_artifacts(
        artifacts, name=name, version=version
    )
    if missing_expected:
        log("Expected artifacts not found locally:")
        for pattern in missing_expected:
            log(f"  {pattern}")

    if args.dry_run:
        log("Dry run artifacts:")
        for artifact in expected_artifacts:
            log(f"  {artifact.name}")
        return 0

    to_upload = filter_missing_artifacts(expected_artifacts, existing_files)
    if to_upload:
        run(["uv", "publish", *[str(path) for path in to_upload]])

    return 0


if __name__ == "__main__":
    sys.exit(main())
