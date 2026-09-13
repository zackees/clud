"""Dry-run argv/identity checks and disposable-only execution gate checks.

Command-source corpus strings are JSON data, never shell input. Only the named
files created by gate_cases below may be removed, inside the disposable image.
"""

from __future__ import annotations

import json
import os
import shutil
from pathlib import Path

from running_process import PIPE, RunningProcess

SHIM = Path("/usr/local/bin/clud-shim")
RM = Path("/opt/rm-shims/rm")
HOOK = Path("/usr/local/bin/clud-block-bad-cmd")


def run(argv: list[str], env: dict[str, str], payload: str | None = None):
    return RunningProcess.run(
        argv, env=env, input=payload, capture_output=True, stderr=PIPE, text=True, timeout=30
    )


def environment() -> dict[str, str]:
    return {"PATH": "/opt/rm-shims:/usr/local/bin:/usr/bin:/bin", "HOME": "/home/checker"}


def dry_cases() -> None:
    cases = json.loads(Path("/opt/rm-protection/stub_cases.json").read_text())
    for case in cases:
        env = environment() | {"CLUD_RM_DRY_RUN": "1", "TEST_CI": ""}
        result = run([str(RM), *case["argv"]], env)
        expected = case["decision"]
        assert result.returncode == (0 if expected == "allow" else 2), result
        verdict = json.loads(result.stdout)
        assert verdict["decision"] == expected, verdict
        if expected == "allow":
            assert verdict["dry_run"] is True, verdict
    print(f"stub dry-run decisions: {len(cases)}/{len(cases)}", flush=True)


def identity_cases() -> None:
    root = Path("/work/identity")
    root.mkdir()
    for state in ("trusted", "replaced", "missing", "system"):
        directory = root / state
        directory.mkdir()
        if state == "trusted":
            shutil.copy2(SHIM, directory / "rm")
        elif state == "replaced":
            (directory / "rm").write_text("#!/bin/sh\nexit 0\n")
            (directory / "rm").chmod(0o755)
        elif state == "system":
            shutil.copy2("/bin/rm", directory / "rm")
        env = environment() | {"PATH": str(directory)}
        payload = json.dumps(
            {"tool_name": "Bash", "cwd": "/work", "tool_input": {"command": "rm -rf ./build"}}
        )
        result = run([str(HOOK)], env, payload)
        assert result.returncode == (0 if state == "trusted" else 2), (state, result)
        if state != "trusted":
            assert json.loads(result.stdout)["hookSpecificOutput"]["permissionDecision"] == "deny"
    print("hook identity decisions: 4/4", flush=True)


def gate_cases() -> None:
    assert Path("/.dockerenv").is_file(), "execution checks require actual Docker"
    # PRoot changes the observed root without privileges or an enabling override
    # in the shim. No host directory is mounted writable, and /proc and the
    # Docker marker are deliberately absent from this separate root.
    isolated = Path("/work/isolated")
    (isolated / "work").mkdir(parents=True)
    (isolated / "bin").mkdir()
    shutil.copy2(SHIM, isolated / "bin/rm")
    for docker in (False, True):
        for ci in (False, True):
            for dry in (False, True):
                # No-Docker dry-run needs mountinfo for validation; that is not
                # the execution gate, so use the full truth-table unit coverage.
                if dry and not docker:
                    continue
                filename = f"gate-{docker}-{ci}-{dry}"
                target = Path("/work") / filename if docker else isolated / "work" / filename
                target.write_text("disposable gate test only\n")
                env = environment()
                if ci:
                    env["TEST_CI"] = ""  # set, even an empty value qualifies
                if dry:
                    env["CLUD_RM_DRY_RUN"] = "1"
                argv = [str(RM), "--", str(target)]
                if not docker:
                    argv = ["/usr/bin/proot", "-r", str(isolated), "-w", "/work"]
                    for dependency in ("/usr", "/lib", "/lib64", "/nix/store"):
                        if Path(dependency).exists():
                            argv.extend(["-b", dependency])
                    argv.extend(["/bin/rm", "--", f"/work/{filename}"])
                result = run(argv, env)
                remove = ci and docker and not dry
                assert target.exists() != remove, (docker, ci, dry, result)
                assert result.returncode == (0 if remove or dry else 2), (docker, ci, dry, result)
                if not remove and not dry:
                    assert "requires both" in json.loads(result.stdout)["reason"], result
                print(
                    f"gate CI={ci} Docker={docker} dry_run={dry}: "
                    f"rc={result.returncode}, exists={target.exists()}",
                    flush=True,
                )


def main() -> None:
    os.chdir("/work")
    dry_cases()
    identity_cases()
    gate_cases()


if __name__ == "__main__":
    main()
