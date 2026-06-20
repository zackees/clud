#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# managed-by: clud
"""docker_build_soldr.py — Rust + soldr + zccache docker-build stack.

The reference implementation of the volume contract from
zackees/clud#416. Source bind-mounted read-only at `/src`; build state
in anonymous Docker volumes named after `<project>-soldr-<role>` so the
mount lives inside Docker's native VM filesystem and avoids the 5-10x
host-bind FS-translation tax on Docker-for-Windows / Docker-for-Mac.

Origin: derived from `.perf-local/docker-repro/build_in_docker.sh` in
the zackees/zccache repo, which proved a 20m22s cold-build against a
Windows host bind dropped to ~3 min once `target/` moved into an anon
volume. That single config change is the whole point of this tool.

Usage:

    clud tool run docker/docker_build_soldr.py <path> [subcommand]

Subcommands:
    init    Write Dockerfile + entry.sh + stack.toml under <path>/.clud/docker-build/soldr/
    up      Create volumes + image; start an idle container; print container id.
    run -- <cmd...>
            Execute <cmd...> inside the container with /src:ro + all volumes mounted.
    shell   Interactive bash in the container.
    verify  Cold + warm-no-op + single-file-edit benchmark (NOT YET IMPLEMENTED — exits 64).
    clean   Remove volumes for this stack+path; force cold rebuild next time.
    doctor  Diagnose docker daemon up, clock skew, MSYS path mangling.

Exit codes:
    0   success
    2   usage error
    64  EX_USAGE — subcommand not implemented in v0 (verify) or missing argument
    *   propagated from docker / cargo on failure
"""

from __future__ import annotations

import argparse
import hashlib
import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path

STACK = "soldr"

DOCKERFILE = r"""# managed-by: clud (docker_build_soldr.py)
# zccache/soldr reference stack. Build cache lives in anon volumes;
# this image is a thin host for rustup + a baseline rust toolchain.
FROM rust:1.94-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        bash ca-certificates curl git pkg-config build-essential \
        libssl-dev clang lld \
 && rm -rf /var/lib/apt/lists/*

# Toolchain caches live in named volumes so cold rebuilds amortize
# across `clud tool run docker-build` invocations.
ENV CARGO_HOME=/cargo-home \
    CARGO_TARGET_DIR=/target \
    RUSTUP_HOME=/rustup-home \
    CARGO_CHEF_LOCAL_DIR=/cargo-chef \
    CARGO_TERM_COLOR=always

RUN mkdir -p /target /cargo-home /rustup-home /cargo-chef /src
WORKDIR /src
CMD ["bash", "-l"]
"""

ENTRY_SH = r"""#!/usr/bin/env bash
# managed-by: clud (docker_build_soldr.py)
# Idle entry script — the tool execs `docker run` directly for one-shot
# commands; this script is here so `clud tool run docker/docker_build_soldr.py up`
# has a long-running PID inside the container to attach against.
set -euo pipefail
exec tail -f /dev/null
"""

STACK_TOML = r"""# managed-by: clud (docker_build_soldr.py)
[stack]
name = "soldr"
image_tag_base = "clud-docker-build-soldr"

[volumes]
target = "/target"
cargo_home = "/cargo-home"
rustup_home = "/rustup-home"
cargo_chef = "/cargo-chef"

[env]
CARGO_HOME = "/cargo-home"
CARGO_TARGET_DIR = "/target"
RUSTUP_HOME = "/rustup-home"
CARGO_CHEF_LOCAL_DIR = "/cargo-chef"
"""

USAGE = """\
usage: clud tool run docker/docker_build_soldr.py <path> <subcommand> [args]

Subcommands: init | up | run -- <cmd...> | shell | verify | clean | doctor
"""


def _project_key(path: Path) -> str:
    """Stable short hash of the absolute project path so two checkouts
    on the same host don't share build volumes by accident."""
    return hashlib.blake2b(str(path.resolve()).encode("utf-8"),
                           digest_size=6).hexdigest()


def _volume_name(path: Path, role: str) -> str:
    return f"clud-docker-build-soldr-{_project_key(path)}-{role}"


def _container_name(path: Path) -> str:
    return f"clud-docker-build-soldr-{_project_key(path)}"


def _image_tag(path: Path) -> str:
    return f"clud-docker-build-soldr:{_project_key(path)}"


def _docker(*args: str, check: bool = True,
            capture: bool = False) -> subprocess.CompletedProcess:
    """Wrap docker so we can swap shells later if needed. On Windows we
    rely on Docker Desktop's CLI being on PATH; the harness assumes
    native path quoting (see SKILL.md `path-conversion` table)."""
    cmd = ["docker", *args]
    if capture:
        return subprocess.run(cmd, check=check, capture_output=True,
                              text=True)
    return subprocess.run(cmd, check=check)


def cmd_init(path: Path) -> int:
    out = path / ".clud" / "docker-build" / STACK
    out.mkdir(parents=True, exist_ok=True)
    (out / "Dockerfile").write_text(DOCKERFILE, encoding="utf-8")
    entry = out / "entry.sh"
    entry.write_text(ENTRY_SH, encoding="utf-8")
    try:
        os.chmod(entry, 0o755)
    except (OSError, NotImplementedError):
        # Windows fs doesn't carry POSIX modes — harmless, the entry
        # script runs inside the container which has its own fs.
        pass
    (out / "stack.toml").write_text(STACK_TOML, encoding="utf-8")
    sys.stdout.write(f"wrote {out}/{{Dockerfile,entry.sh,stack.toml}}\n")
    return 0


def cmd_up(path: Path) -> int:
    stack_dir = path / ".clud" / "docker-build" / STACK
    dockerfile = stack_dir / "Dockerfile"
    if not dockerfile.is_file():
        sys.stderr.write(f"missing {dockerfile} — run `init` first\n")
        return 2

    image = _image_tag(path)
    sys.stdout.write(f"building image {image} (cached layers reused)...\n")
    _docker("build", "-t", image, "-f", str(dockerfile), str(stack_dir))

    name = _container_name(path)
    existing = _docker("ps", "-aq", "-f", f"name=^{name}$",
                       capture=True, check=False).stdout.strip()
    if existing:
        sys.stdout.write(f"container {name} already exists ({existing[:12]}) "
                         f"— start if needed\n")
        _docker("start", name, check=False)
        return 0

    vol_args = []
    for role, mount in (("target", "/target"), ("cargo-home", "/cargo-home"),
                        ("rustup-home", "/rustup-home"),
                        ("cargo-chef", "/cargo-chef")):
        vol_args += ["-v", f"{_volume_name(path, role)}:{mount}"]

    sys.stdout.write(f"starting container {name}...\n")
    _docker("run", "-d", "--name", name,
            "-v", f"{path.resolve()}:/src:ro",
            *vol_args,
            image, "tail", "-f", "/dev/null")
    return 0


def cmd_run(path: Path, cmdline: list[str]) -> int:
    if not cmdline:
        sys.stderr.write("run: missing command (use `run -- <cmd...>`)\n")
        return 2
    name = _container_name(path)
    # Idempotent up — bring it up if it isn't already running.
    cmd_up(path)
    rc = _docker("exec", "-w", "/src", name, *cmdline, check=False).returncode
    return rc


def cmd_shell(path: Path) -> int:
    name = _container_name(path)
    cmd_up(path)
    rc = _docker("exec", "-it", "-w", "/src", name, "bash", "-l",
                 check=False).returncode
    return rc


def cmd_verify(path: Path) -> int:
    sys.stderr.write(
        "verify: NOT YET IMPLEMENTED in v0 — see zackees/clud#421\n"
        "Cold + warm-no-op + single-file-edit benchmarking with a\n"
        "30s wall-clock budget for warm-no-op needs its own isolation\n"
        "(empty cache via `clean`, then triplicate timed builds).\n"
    )
    return 64


def cmd_clean(path: Path) -> int:
    name = _container_name(path)
    _docker("rm", "-f", name, check=False)
    for role in ("target", "cargo-home", "rustup-home", "cargo-chef"):
        _docker("volume", "rm", _volume_name(path, role), check=False)
    sys.stdout.write(f"removed container + {STACK} volumes for {path}\n")
    return 0


def cmd_doctor(_path: Path | None = None) -> int:
    failures: list[str] = []

    docker_ok = shutil.which("docker") is not None
    if not docker_ok:
        failures.append("docker not on PATH")
    else:
        ping = subprocess.run(["docker", "version", "--format", "{{.Server.Version}}"],
                              capture_output=True, text=True, check=False)
        if ping.returncode != 0:
            failures.append(f"docker daemon not reachable: {ping.stderr.strip()}")
        else:
            sys.stdout.write(f"docker server: {ping.stdout.strip()}\n")

    if platform.system() == "Windows":
        # MSYS Git Bash mangles -v paths; we can't fix the user's shell
        # but we can warn loudly.
        if "MSYSTEM" in os.environ:
            sys.stdout.write(
                "WARN: detected MSYS shell ($MSYSTEM=" + os.environ["MSYSTEM"]
                + "). `docker -v` flag values may be path-mangled — prefer "
                "PowerShell for docker invocations from this tool.\n"
            )

    # Clock skew check — start a sub-second container and compare.
    if docker_ok:
        try:
            r = subprocess.run(
                ["docker", "run", "--rm", "alpine:3", "date", "+%s"],
                capture_output=True, text=True, check=True, timeout=20)
            container_epoch = int(r.stdout.strip())
            host_epoch = int(subprocess.run(
                ["python", "-c", "import time; print(int(time.time()))"],
                capture_output=True, text=True, check=True).stdout.strip())
            skew = abs(container_epoch - host_epoch)
            sys.stdout.write(f"clock skew (container vs host): {skew}s\n")
            if skew > 1:
                failures.append(
                    f"clock skew {skew}s exceeds 1s budget; warm incremental"
                    " builds will treat fresh outputs as stale and rebuild"
                    " from scratch")
        except (subprocess.SubprocessError, ValueError) as e:
            failures.append(f"clock skew probe failed: {e}")

    if failures:
        sys.stderr.write("\nDOCTOR FAILED:\n")
        for f in failures:
            sys.stderr.write(f"  - {f}\n")
        return 1
    sys.stdout.write("doctor: ok\n")
    return 0


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(prog="docker_build_soldr", add_help=False,
                                description=USAGE)
    p.add_argument("path", nargs="?", default=".")
    p.add_argument("sub", nargs="?", default="verify")
    p.add_argument("rest", nargs=argparse.REMAINDER)
    ns = p.parse_args(argv)

    # `doctor` does not consume <path>.
    if ns.path == "doctor":
        return cmd_doctor(None)

    path = Path(ns.path).resolve()
    sub = ns.sub

    if sub == "init":
        return cmd_init(path)
    if sub == "up":
        return cmd_up(path)
    if sub == "run":
        # argparse REMAINDER preserves the `--` if present; strip it.
        rest = ns.rest
        if rest and rest[0] == "--":
            rest = rest[1:]
        return cmd_run(path, rest)
    if sub == "shell":
        return cmd_shell(path)
    if sub == "verify":
        return cmd_verify(path)
    if sub == "clean":
        return cmd_clean(path)
    if sub == "doctor":
        return cmd_doctor(path)

    sys.stderr.write(f"unknown subcommand: {sub}\n{USAGE}")
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
