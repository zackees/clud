#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# managed-by: clud
"""docker_build_soldr.py — Rust + soldr + zccache docker-build stack.

The reference implementation of the volume contract from
zackees/clud#416. Source bind-mounted read-only at `/src`; build state
and soldr's daemon home live in anonymous Docker volumes named after
`<project>-soldr-<role>` so the mount lives inside Docker's native VM
filesystem and avoids the 5-10x host-bind FS-translation tax on
Docker-for-Windows / Docker-for-Mac.

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
    gc      Reclaim stale clud-managed groups (dry-run; `gc --force` deletes).
            Removes groups >=48h old or whose source worktree is gone; never
            the currently-selected group. Discovered by label, not by name.
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
ARG RUST_VERSION=1.94.1
FROM rust:${RUST_VERSION}-trixie

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        bash ca-certificates curl git pkg-config build-essential \
        libssl-dev clang lld zstd \
 && rm -rf /var/lib/apt/lists/*

# Toolchain caches live in named volumes so cold rebuilds amortize
# across `clud tool run docker-build` invocations.
ENV HOME=/root \
    CARGO_HOME=/cargo-home \
    CARGO_TARGET_DIR=/target \
    RUSTUP_HOME=/rustup-home \
    CARGO_CHEF_LOCAL_DIR=/cargo-chef \
    SOLDR_TRUST_MODE=permissive \
    CARGO_TERM_COLOR=always \
    PATH=/cargo-home/bin:/usr/local/cargo/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin

# Seed the toolchain into the same paths that later become named
# volumes. Docker copies this image content into a fresh named volume
# on first mount, so the first `docker exec` does not re-download Rust.
ARG RUST_VERSION=1.94.1
RUN mkdir -p /target /cargo-home /rustup-home /cargo-chef /root/.soldr /src \
 && rustup default "${RUST_VERSION}" \
 && rustup component add rustfmt clippy

# Bake soldr into the helper image instead of installing it during the
# first command. Keep its persistent daemon/cache state in /root/.soldr,
# which cmd_up mounts as a named volume.
ARG SOLDR_VERSION=0.8.28
RUN mkdir -p /opt/soldr-bin \
 && curl -fsSL \
        "https://github.com/zackees/soldr/releases/download/v${SOLDR_VERSION}/soldr-v${SOLDR_VERSION}-x86_64-unknown-linux-gnu.tar.zst" \
    | zstd -d --stdout \
    | tar -xf - -C /opt/soldr-bin \
 && for bin in soldr soldr-clang-shim cargo-chef crgx; do \
        [ -f "/opt/soldr-bin/$bin" ] && cp "/opt/soldr-bin/$bin" "/usr/local/bin/$bin" && chmod +x "/usr/local/bin/$bin"; \
    done \
 && soldr --version

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
soldr_home = "/root/.soldr"

[env]
HOME = "/root"
CARGO_HOME = "/cargo-home"
CARGO_TARGET_DIR = "/target"
RUSTUP_HOME = "/rustup-home"
CARGO_CHEF_LOCAL_DIR = "/cargo-chef"
SOLDR_TRUST_MODE = "permissive"
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


# Issue #518: every managed resource carries labels so `gc` can discover and
# scope cleanup by label rather than by fragile name parsing — an externally
# named container reusing a clud volume set must still be discoverable.
LABEL_NS = "com.clud.docker-build"


def _label_args(path: Path, role: str) -> list[str]:
    """`--label` flags identifying this resource as clud-managed, plus its
    stack, project key, canonical source root, and role."""
    labels = [
        f"{LABEL_NS}.managed=true",
        f"{LABEL_NS}.stack={STACK}",
        f"{LABEL_NS}.project-key={_project_key(path)}",
        f"{LABEL_NS}.project-root={path.resolve()}",
        f"{LABEL_NS}.role={role}",
    ]
    args: list[str] = []
    for label in labels:
        args += ["--label", label]
    return args


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

    name = _container_name(path)
    image = _image_tag(path)

    # Issue #518: reuse an existing container before rebuilding. Building first
    # (as v0 did) replaces the image tag on every `up`, orphaning the older
    # image config that the running container still pins — the exact duplicate
    # buildup the issue reported. If the container already exists, just start it;
    # `clean` forces a rebuild after a Dockerfile change.
    existing = _docker("ps", "-aq", "-f", f"name=^{name}$",
                       capture=True, check=False).stdout.strip()
    if existing:
        sys.stdout.write(f"container {name} already exists ({existing[:12]}) "
                         f"— starting (run `clean` first to rebuild)\n")
        _docker("start", name, check=False)
        return 0

    sys.stdout.write(f"building image {image} (cached layers reused)...\n")
    # #518: build through clud's own builder so its BuildKit cache lands in a
    # namespace `gc` can prune without touching anyone else's build cache.
    # `--load` puts the result in the local image store, which is what the
    # subsequent `docker run` needs; plain `docker build` is the fallback for
    # a Docker without buildx.
    if _ensure_builder():
        _docker("buildx", "build", "--builder", BUILDER_NAME, "--load",
                *_label_args(path, "image"),
                "-t", image, "-f", str(dockerfile), str(stack_dir))
    else:
        _docker("build", *_label_args(path, "image"),
                "-t", image, "-f", str(dockerfile), str(stack_dir))

    vol_args = []
    for role, mount in (("target", "/target"), ("cargo-home", "/cargo-home"),
                        ("rustup-home", "/rustup-home"),
                        ("cargo-chef", "/cargo-chef"),
                        ("soldr-home", "/root/.soldr")):
        vol = _volume_name(path, role)
        # Pre-create the volume with labels; `docker run -v name:...` would
        # otherwise auto-create it unlabeled, invisible to label-scoped gc.
        _docker("volume", "create", *_label_args(path, "volume"),
                "--label", f"{LABEL_NS}.cache-role={role}", vol,
                check=False, capture=True)
        vol_args += ["-v", f"{vol}:{mount}"]

    sys.stdout.write(f"starting container {name}...\n")
    _docker("run", "-d", "--init", "--name", name,
            *_label_args(path, "container"),
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
    for role in ("target", "cargo-home", "rustup-home", "cargo-chef",
                 "soldr-home"):
        _docker("volume", "rm", _volume_name(path, role), check=False)
    sys.stdout.write(f"removed container + {STACK} volumes for {path}\n")
    return 0


#: Builder (and therefore BuildKit cache namespace) clud owns (#518).
#:
#: BuildKit's cache is *shared per builder*. Pruning the `default` builder
#: would delete unrelated users' build cache on the same machine — the issue is
#: explicit that this must not happen. Building through a clud-named builder
#: gives us a namespace we can prune without touching anyone else's.
BUILDER_NAME = "clud-docker-build-soldr"


def buildx_prune_args(threshold_hours: float, *, force: bool) -> list[str]:
    """Argv for pruning *only* clud's BuildKit cache namespace.

    Pure so the "never prunes the default builder" and "never prunes younger
    than the threshold" guarantees are asserted on the argv itself, rather than
    inferred from a live daemon CI cannot run.
    """
    args = [
        "buildx", "prune",
        "--builder", BUILDER_NAME,
        "--filter", f"until={threshold_hours:g}h",
    ]
    if force:
        args.append("--force")
    return args


def _buildx_available() -> bool:
    return _docker("buildx", "version", capture=True,
                   check=False).returncode == 0


def _builder_exists() -> bool:
    return _docker("buildx", "inspect", BUILDER_NAME,
                   capture=True, check=False).returncode == 0


def _ensure_builder() -> bool:
    """Create clud's builder if absent.

    Returns False when buildx is unavailable so callers fall back to plain
    `docker build` rather than failing: a Docker without buildx must still be
    able to build, it just gets no prunable cache namespace.
    """
    if not _buildx_available():
        return False
    if _builder_exists():
        return True
    return _docker("buildx", "create", "--name", BUILDER_NAME,
                   capture=True, check=False).returncode == 0


def _prune_buildkit_cache(threshold_hours: float, *, force: bool) -> None:
    """Prune BuildKit records at least ``threshold_hours`` old, in clud's
    namespace only.

    Best-effort: a missing builder means nothing of ours has been built on this
    machine yet, which is not an error.
    """
    if not _buildx_available():
        sys.stdout.write("buildkit: buildx unavailable — skipping cache prune\n")
        return
    if not _builder_exists():
        sys.stdout.write(
            f"buildkit: no {BUILDER_NAME} builder — nothing of ours to prune\n")
        return
    args = buildx_prune_args(threshold_hours, force=force)
    if not force:
        sys.stdout.write(
            f"buildkit: would run `docker {' '.join(args)}` "
            f"(clud namespace only; other builders untouched)\n")
        return
    _docker(*args, capture=True, check=False)
    sys.stdout.write(
        f"buildkit: pruned records older than {threshold_hours:g}h "
        f"in builder {BUILDER_NAME}\n")


#: How many managed generations under the shared clud tag prefix count as
#: "crowded". Three sets is what the reporting box actually had (~43 GB), and
#: it is the point where repeated `up` is clearly churning generations rather
#: than one project being rebuilt.
DENSITY_THRESHOLD = 3

#: Shortened grace applied to unreferenced generations once the prefix is
#: crowded. Still long enough to survive a normal working session, so an
#: afternoon of rebuilds does not evict the cache someone is about to reuse.
CROWDED_GRACE_HOURS = 12.0


def gc_plan(groups: list[dict], *, threshold_hours: float = 48.0,
            density_threshold: int = DENSITY_THRESHOLD,
            crowded_grace_hours: float = CROWDED_GRACE_HOURS) -> tuple[list, list]:
    """Pure garbage-collection decision over managed resource groups (#518).

    Each group is a dict with keys: ``project_key`` (str), ``age_hours``
    (float), ``root_exists`` (bool — does the canonical source worktree still
    exist), ``is_selected`` (bool — is this the group the *current* invocation
    would reuse), and optionally ``referenced`` (bool — is any container still
    using it; defaults to False). Returns ``(to_remove, kept)`` as lists of
    ``(project_key, reason)`` tuples.

    Policy (per the issue's superseding clarification — these builds are
    ephemeral, so bounded disk usage wins over preserving old state):

    - The currently-selected group is always kept, even past the threshold and
      even under a crowded tag prefix. The density heuristic *accelerates*
      cleanup of stale generations; it must never evict the active one.
    - A group whose source worktree is gone is eligible immediately, any age.
    - **Density heuristic:** once at least ``density_threshold`` managed groups
      share the clud tag prefix, that is evidence of active development
      churning generations. Unreferenced groups then become eligible at
      ``crowded_grace_hours`` instead of waiting the full threshold. A group
      still referenced by a container keeps the full 48 h, because killing a
      container someone is using to save disk is the wrong trade at 12 h and
      the right one only once the resource is genuinely stale.
    - ``threshold_hours`` remains the hard upper bound: past it a group goes,
      referenced or not (gc may force-stop the containers pinning it).

    Pure and dependency-free so the safety boundaries are exhaustively
    unit-tested without a live Docker daemon.
    """
    crowded = len(groups) >= density_threshold
    to_remove: list = []
    kept: list = []
    for group in groups:
        key = group["project_key"]
        age = group.get("age_hours", 0.0)
        if group.get("is_selected"):
            kept.append((key, "currently-selected"))
        elif not group.get("root_exists", True):
            to_remove.append((key, "worktree-gone"))
        elif age >= threshold_hours:
            to_remove.append((key, "stale-past-threshold"))
        elif (crowded
              and not group.get("referenced", False)
              and age >= crowded_grace_hours):
            to_remove.append((key, "crowded-prefix-accelerated"))
        else:
            kept.append((key, "within-grace"))
    return to_remove, kept


def cmd_gc(path: Path, *, force: bool, threshold_hours: float = 48.0) -> int:
    """Discover clud-managed docker resource groups by label and print (or,
    with ``force``, execute) the removal plan. Dry-run by default."""
    selected_key = _project_key(path)
    groups = _discover_managed_groups(selected_key)
    to_remove, kept = gc_plan(groups, threshold_hours=threshold_hours)

    sys.stdout.write(f"managed groups: {len(groups)} "
                     f"(threshold {threshold_hours:g}h)\n")
    for key, reason in kept:
        sys.stdout.write(f"  KEEP   {key}  ({reason})\n")
    for key, reason in to_remove:
        sys.stdout.write(f"  REMOVE {key}  ({reason})\n")

    # BuildKit cache is a separate lifecycle from the named-volume caches
    # (#518): volumes hold the warm `target/`, BuildKit holds layer history.
    # Both are swept, but reported separately so the distinction the issue
    # asks to document stays visible at the command line too.
    _prune_buildkit_cache(threshold_hours, force=force)

    if not to_remove:
        sys.stdout.write("nothing to reclaim.\n")
        return 0
    if not force:
        sys.stdout.write("\ndry-run — pass `--force` to delete the REMOVE groups.\n")
        return 0
    for key, _reason in to_remove:
        _remove_managed_group(key)
    sys.stdout.write(f"removed {len(to_remove)} stale group(s).\n")
    return 0


def _discover_managed_groups(selected_key: str) -> list[dict]:
    """Enumerate clud-managed containers/volumes via the managed label and fold
    them into per-project groups. Best-effort: docker errors yield no groups
    rather than raising, so `gc` never blocks on a flaky daemon."""
    import time

    keys: set[str] = set()
    created: dict[str, float] = {}
    roots: dict[str, str] = {}
    now = time.time()
    label = f"{LABEL_NS}.managed=true"
    # Containers first (their CreatedAt is the freshest signal of activity),
    # then volumes for groups that have no live container.
    #
    # Volumes are listed WITHOUT the label filter on purpose. Volumes created
    # before labelling existed carry no labels at all, so a label-only query
    # reports "0 managed groups" while the abandoned cache sets that motivated
    # this feature sit on disk forever (observed: 20 legacy volumes / 4 groups,
    # none labelled). `_parse_managed_line` requires the exact
    # `clud-docker-build-soldr-` name prefix, which is itself an unambiguous
    # clud marker, so this cannot pick up a third-party volume (e.g. the
    # unrelated `soldr-perf-target` volume is not matched).
    for kind, args in (
        ("container", ["ps", "-a", "--filter", f"label={label}",
                       "--format", "{{.Labels}}\t{{.CreatedAt}}"]),
        ("volume", ["volume", "ls", "--format", "{{.Name}}"]),
    ):
        out = _docker(*args, capture=True, check=False).stdout
        for line in out.splitlines():
            line = line.strip()
            if not line:
                continue
            key, root, age = _parse_managed_line(kind, line)
            if key is None:
                continue
            keys.add(key)
            if root is not None:
                roots.setdefault(key, root)
            if age is not None:
                created[key] = min(created.get(key, age), age)

    # A group discovered only through its volumes has no container timestamp.
    # Without an age it would read as brand new and be kept forever — the exact
    # failure mode for legacy, containerless cache sets. Fall back to the
    # volumes' own CreatedAt (one batched inspect, not one call per volume).
    missing_age = [key for key in keys if key not in created]
    if missing_age:
        created.update(_volume_created_epochs(missing_age))

    groups: list[dict] = []
    for key in sorted(keys):
        root = roots.get(key)
        age_hours = ((now - created[key]) / 3600.0) if key in created else 0.0
        groups.append({
            "project_key": key,
            "age_hours": age_hours,
            "root_exists": (root is not None and Path(root).exists())
            if root is not None else True,
            "is_selected": key == selected_key,
        })
    return groups


def _volume_created_epochs(keys: list[str]) -> dict[str, float]:
    """Oldest volume CreatedAt per project key, as a unix epoch.

    One batched `docker volume inspect` (it accepts many names) rather than a
    call per volume. Volumes whose timestamp cannot be parsed are skipped,
    which leaves their group ageless and therefore kept — the safe direction.
    """
    wanted = set(keys)
    names = [
        name.strip()
        for name in _docker("volume", "ls", "--format", "{{.Name}}",
                            capture=True, check=False).stdout.splitlines()
        if name.strip() and _parse_managed_line("volume", name.strip())[0] in wanted
    ]
    if not names:
        return {}
    out = _docker("volume", "inspect", *names,
                  "--format", "{{.Name}}\t{{.CreatedAt}}",
                  capture=True, check=False).stdout
    oldest: dict[str, float] = {}
    for line in out.splitlines():
        name, _, created_at = line.strip().partition("\t")
        key = _parse_managed_line("volume", name.strip())[0]
        epoch = _parse_docker_created_at(created_at.strip())
        if key is None or epoch is None:
            continue
        oldest[key] = min(oldest.get(key, epoch), epoch)
    return oldest


def _parse_managed_line(kind: str, line: str):
    """Extract (project_key, project_root|None, created_epoch|None) from one
    `docker` list line. Volume lines carry only the name (key in the suffix)."""
    if kind == "volume":
        # clud-docker-build-soldr-<key>-<cache-role>
        parts = line.split("-")
        if len(parts) >= 6 and parts[:4] == ["clud", "docker", "build", "soldr"]:
            return parts[4], None, None
        return None, None, None
    # container: "<labels>\t<created-at>"; labels are k=v,k=v.
    labels_str, _, created_str = line.partition("\t")
    labels = dict(
        kv.split("=", 1) for kv in labels_str.split(",") if "=" in kv
    )
    key = labels.get(f"{LABEL_NS}.project-key")
    root = labels.get(f"{LABEL_NS}.project-root")
    created = _parse_docker_created_at(created_str.strip())
    return key, root, created


def _parse_docker_created_at(text: str):
    """Best-effort parse of docker's `CreatedAt` into a unix epoch; None if the
    format is unrecognized (gc then treats the group as age 0 = kept)."""
    if not text:
        return None
    import datetime
    # Docker emits e.g. "2026-07-28 11:04:46 -0700 PDT"; drop the trailing tz
    # name which %z cannot parse.
    trimmed = text.rsplit(" ", 1)[0] if text.count(" ") >= 3 else text
    # `docker ps` emits "2026-07-28 11:04:46 -0700 PDT"; `docker volume
    # inspect` emits RFC3339 ("...T11:04:46-07:00" or "...Z").
    for fmt in ("%Y-%m-%d %H:%M:%S %z", "%Y-%m-%dT%H:%M:%SZ", "%Y-%m-%dT%H:%M:%S%z"):
        try:
            return datetime.datetime.strptime(trimmed, fmt).timestamp()
        except ValueError:
            continue
    return None


def _remove_managed_group(key: str) -> None:
    """Force-remove one managed group: its containers, then its volumes."""
    containers = _docker(
        "ps", "-aq", "--filter", f"label={LABEL_NS}.project-key={key}",
        capture=True, check=False).stdout.split()
    for cid in containers:
        _docker("rm", "-f", cid, check=False, capture=True)
    # Union of labelled volumes and same-key volumes matched by name prefix:
    # a legacy group predates labelling entirely, so a label-only removal would
    # discover the group in `gc` and then silently delete nothing.
    volumes = set(
        _docker("volume", "ls", "-q", "--filter",
                f"label={LABEL_NS}.project-key={key}",
                capture=True, check=False).stdout.split()
    )
    volumes.update(
        name.strip()
        for name in _docker("volume", "ls", "--format", "{{.Name}}",
                            capture=True, check=False).stdout.splitlines()
        if name.strip() and _parse_managed_line("volume", name.strip())[0] == key
    )
    for vol in sorted(volumes):
        _docker("volume", "rm", "-f", vol, check=False, capture=True)


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
    if sub == "gc":
        return cmd_gc(path, force="--force" in ns.rest)
    if sub == "doctor":
        return cmd_doctor(path)

    sys.stderr.write(f"unknown subcommand: {sub}\n{USAGE}")
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
