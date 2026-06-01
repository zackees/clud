---
name: clud-docker-rust-app-dev
description: Build a Rust app inside Docker for **development iteration** — fast incremental cargo builds via named volumes for target/ + CARGO_HOME + RUSTUP_HOME, source bind-mounted (no COPY), soldr-wrapped cargo, and a Python orchestrator. **Not for deployment** — this produces a developer harness, not a shippable image; for production/distribution images use a multi-stage Dockerfile with `cargo chef`.
triggers:
  - When the user types "/clud-docker-rust-app-dev"
  - When the user says "turn this into a docker" / "dockerize this" / "containerize this rust app" *for development or iteration*
  - When the user asks to make a Rust build reproducible inside Docker *for local dev*
  - When the user complains that their Rust app's Docker builds are slow because every iteration re-COPYs source and loses cargo incremental state
  - When the user wants to run cargo build / test / clippy / fmt inside a container without touching the host toolchain
  - **Do NOT trigger** when the user is asking for a slim production/release image, a multi-stage distribution build, a single-binary deployable container, or anything destined for `docker push` to a registry — that is the deployment path, not the dev path.
---
<!-- managed-by: clud -->

# /clud-docker-rust-app-dev

**Development harness, not a deployment image.** Containerize a Rust application so that **incremental cargo builds inside the container are seconds, not minutes** — including on Windows + Docker Desktop, where the WSL2 9P translation layer is the silent killer of cargo fingerprints.

The output of this skill is a **dev iteration loop** — `python ci/perf_local.py cargo build` runs incrementally inside a container with the host's source bind-mounted and cargo state in named volumes. The image itself is never `docker push`ed; it's a per-developer scratch environment. For production images that *do* ship, see the "When NOT to use this skill" section below.

This playbook is distilled from the two reference implementations:

- `~/dev/soldr/docker/cook-shared-cache/` + `bench/cook_in_docker.sh` + `ci/perf_local.py` — minimal single-image setup with three named volumes, plus a thin Python wrapper that forwards arbitrary `cargo` invocations.
- `~/dev/zccache/ci/docker/` (three Dockerfiles + `runner.Dockerfile` + `perf_entrypoint.sh`) + `ci/perf_local.py` — split-image builder/runner harness with persistent `RUSTUP_HOME` volume and per-subcommand wrappers (`fmt` / `clippy` / `test` / `shell`).

Both demonstrate the same load-bearing trick. **Use the single-image shape by default; reach for split images only when you need multiple binaries, multi-stage outputs, or a separate runtime base.**

## The four hard rules

1. **Named Docker volumes for build state, never host bind mounts.** `target/`, `CARGO_HOME`, and `RUSTUP_HOME` must live in `-v <named-volume>:/path` mounts. Host bind mounts (`-v $(pwd)/target:/work/target`) on Windows + WSL2 rewrite file mtimes on every container start, defeating cargo's fingerprint check and forcing a full rebuild (~6 min on a 21-crate workspace vs ~1 s when warm).
2. **Source is bind-mounted (read-only by default).** The repo lives at `-v $(pwd):/work:ro` (or `rw` only when you need `cargo fmt` to write back). The image carries the toolchain and apt deps; source changes are *not* a layer-cache miss — they're a cargo recompile inside the live container.
3. **The image carries the toolchain pin.** `rust-toolchain.toml` is COPY'd into the image and `rustup default <version>` runs at build time so the per-run `cargo` command never re-downloads the toolchain. Persist a `RUSTUP_HOME` volume so `rustup component add rustfmt clippy` is paid once.
4. **Wrap `docker run` in a Python orchestrator at `ci/perf_local.py`.** Users never type the volume flags by hand. The wrapper handles `MSYS_NO_PATHCONV=1` for Git-Bash, the no-`-it` default (Git-Bash fools `isatty`), and exposes `--wipe` / `--status` subcommands.

## Workflow

### Step 1 — Confirm the project shape

- Is it a single binary or a workspace? Single-image shape works for both.
- Does the user need multiple output binaries on different bases (e.g. musl + glibc)? Then reach for the split builder/runner shape.
- Does the user need cargo-fmt and clippy inside the container? Add a `RUSTUP_HOME` volume and dedicated `fmt` / `clippy` subcommands.
- Where does `soldr` fit? If the host uses `soldr cargo build` to pin the toolchain, install soldr inside the image too and invoke `soldr cargo` so toolchain resolution matches host behavior. Otherwise use `cargo` directly with `rustup default <version>` baked in.

### Step 2 — Drop the Dockerfile

Create `docker/<name>/Dockerfile` (or `ci/docker/<name>.Dockerfile` for multi-image setups). Skeleton:

```dockerfile
# syntax=docker/dockerfile:1.7
#
# Why this image exists: build & test <project> inside Docker so the host's
# toolchain / cargo state is never touched. Named volumes (NOT host bind mounts)
# hold target/ and CARGO_HOME so cargo's mtime fingerprint survives container
# restarts. Without this, Windows + Docker Desktop's WSL2 9P translation layer
# rewrites mtimes per container start and cargo rebuilds the world (~6 min on
# a 21-crate workspace vs ~1 s when warm).

FROM rust:1.94.1-bookworm

# Build deps for any cc-rs / pkg-config / -sys crates in the dep graph.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        build-essential \
        pkg-config \
        libssl-dev \
        ca-certificates \
        git \
 && rm -rf /var/lib/apt/lists/*

# Pin the toolchain at image-build time so per-run cargo never re-downloads it.
RUN rustup default 1.94.1 \
 && rustup component add rustfmt clippy

# Explicit so the named-volume mount points are unambiguous.
ENV CARGO_HOME=/root/.cargo
ENV CARGO_TARGET_DIR=/work/target

# Source is mounted by the runner at /work; no COPY here so each `docker run`
# reuses the cached image but operates on the live worktree.
WORKDIR /work

# Default command — runner overrides with the actual cargo invocation.
CMD ["cargo", "--version"]
```

Key points to enforce, even if the user pushes back:

- **No `COPY . .`** of source code into the image. That single line erases the entire benefit — every source edit becomes a layer-cache miss. Source is a volume mount.
- **`ENV CARGO_HOME` and `ENV CARGO_TARGET_DIR` are mandatory and explicit.** Don't rely on inherited defaults — future base-image migrations silently change `$HOME` and break the volume mapping.
- **`rustup default <version>` baked into the image**, not deferred to runtime. The toolchain download is the slowest one-time cost.
- **No `target/` clean step in the Dockerfile.** A `cargo clean` inside the image deletes the same `/work/target` the runner mounts the volume into — wiping the warm cache on every container build. (zccache's `update-zccache-pin-honored/Dockerfile` does call `cargo clean`, but only because that image *is* a single-shot repro harness that intentionally throws away its build state.)

### Step 3 — Pick the variant

#### Variant A — Single-image setup (soldr pattern, default)

Use when one Dockerfile + one set of named volumes covers everything. This is the soldr `cook-shared-cache` pattern.

Run-time invocation:

```bash
docker run --rm --init \
    -v "$(pwd):/work" \
    -v "<proj>-perf-target:/work/target" \
    -v "<proj>-perf-cargo-home:/root/.cargo" \
    -v "<proj>-perf-rust-state:/root/.rustup" \
    -w /work \
    <image-tag> \
    cargo "$@"
```

#### Variant B — Split builder/runner setup (zccache pattern)

Use when you need:
- multiple output binaries on different bases (e.g. one musl static, one glibc dynamic),
- a slim runtime image with no toolchain,
- to ship the build artifact to a separate test scenario.

Three images:

1. `<proj>-builder` (or one per target) — `FROM rust:X-bookworm`; mounts source `/src:ro`, target volume `/target`, CARGO_HOME volume `/cargo-home`, and a writable `/out`; entrypoint `cargo build --release` then `cp` to `/out`.
2. `<proj>-runner` — `FROM rust:X-slim-bookworm` (or `debian:bookworm-slim`); mounts the `/out` from step 1 read-only at `/usr/local/bin`; runs the actual test/scenario script.
3. Optional `<proj>-extra-builder` for a second binary target (e.g. musl `FROM rust:X-alpine`).

The orchestrator runs them in sequence: builder → builder → runner. **Same volume-shape rules apply to every step**; the only difference is that built binaries flow through host directories (cheap, infrequent, small file count) while build state stays in named volumes (large, frequent, fingerprint-sensitive).

### Step 4 — `RUN --mount=type=cache` for pure-image builds

If a step really does need to build inside a `RUN` layer (e.g. the runner builds itself once at image-build time, like zccache's `Dockerfile.cc-test`), use BuildKit cache mounts so the cargo cache survives across builds even though it can't escape into a volume:

```dockerfile
# syntax=docker/dockerfile:1.7
# ...
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/work/target \
    cargo build --release -p <pkg> --bin <bin> \
 && mkdir -p /out \
 && cp target/release/<bin> /out/
```

Cache mounts live in BuildKit's cache (not in the image layers) so subsequent `docker build` runs reuse them. Important: cache mounts are *per-`docker build`*, not per-`docker run` — they don't replace the named-volume strategy for iterative work, they complement it for one-shot build-and-test images.

### Step 5 — soldr integration (optional but recommended)

If `soldr` is the project's toolchain wrapper:

- Install soldr in the image: `./install --global` (or vendor the binary into `/usr/local/bin/soldr`).
- Invoke `soldr cargo build` instead of `cargo build` in entrypoints and orchestrator wrappers so toolchain resolution matches host behavior.
- Add a `<proj>-perf-soldr-home` named volume mounted at `/root/.soldr` to persist soldr daemon state across runs. **NEVER bind-mount the host's `~/.soldr/`** — soldr is a per-user singleton and the container would corrupt the host daemon's state.
- For tests that mutate soldr state, use a *second* named volume (e.g. `cook-soldr-home`) that the orchestrator wipes per run with `docker volume rm --force <name>`. The warm volume stays untouched. (See `~/dev/soldr/bench/cook_in_docker.sh:72` for the canonical pattern.)

### Step 6 — Drop the Python orchestrator

Create `ci/perf_local.py` (or whatever the project's CI script convention is). The minimal version forwards arbitrary cargo invocations; the rich version adds per-subcommand subcommands. Skeleton:

```python
#!/usr/bin/env python3
"""Run cargo (and friends) against <project>'s warmed Docker volumes."""

from __future__ import annotations
import os, shutil, subprocess, sys
from pathlib import Path

IMAGE = "<proj>-dev"
DOCKERFILE = "docker/<name>/Dockerfile"
VOLUME_TARGET = "<proj>-perf-target"
VOLUME_CARGO_HOME = "<proj>-perf-cargo-home"
VOLUME_RUST_STATE = "<proj>-perf-rust-state"  # for rustup components

USAGE = """\
usage: python ci/perf_local.py cargo <args...>
       python ci/perf_local.py --wipe       # remove the perf volumes
       python ci/perf_local.py --status     # show volume mountpoints
"""

def main(argv: list[str]) -> int:
    repo_root = Path(__file__).resolve().parent.parent
    os.chdir(repo_root)

    if not shutil.which("docker"):
        print("error: docker not on PATH", file=sys.stderr); return 2
    if not argv or argv[0] in ("-h", "--help"):
        print(USAGE); return 0 if argv else 2
    if argv[0] == "--wipe":  return wipe()
    if argv[0] == "--status": return status()
    if argv[0] != "cargo":
        print(f"error: expected 'cargo', got {argv[0]!r}", file=sys.stderr); return 2

    if subprocess.run(["docker", "build", "-f", DOCKERFILE, "-t", IMAGE, "."]).returncode:
        return 1

    cmd = [
        "docker", "run", "--rm", "--init",
        "-v", f"{repo_root}:/work",
        "-v", f"{VOLUME_TARGET}:/work/target",
        "-v", f"{VOLUME_CARGO_HOME}:/root/.cargo",
        "-v", f"{VOLUME_RUST_STATE}:/root/.rustup",
        "-w", "/work",
        IMAGE, *argv,
    ]
    # Git-Bash on Windows fools isatty(); skip -it unless explicitly enabled,
    # else `docker run -it` errors with "the input device is not a TTY".
    if os.environ.get("CLUD_DOCKER_TTY", "").strip() in ("1", "true", "yes"):
        cmd.insert(2, "-it")
    env = os.environ.copy()
    env.setdefault("MSYS_NO_PATHCONV", "1")  # stop Git-Bash mangling /work
    return subprocess.run(cmd, env=env).returncode

def wipe() -> int:
    return subprocess.run(
        ["docker", "volume", "rm", "--force",
         VOLUME_TARGET, VOLUME_CARGO_HOME, VOLUME_RUST_STATE],
    ).returncode

def status() -> int:
    for name in (VOLUME_TARGET, VOLUME_CARGO_HOME, VOLUME_RUST_STATE):
        r = subprocess.run(
            ["docker", "volume", "inspect", "--format", "{{.Mountpoint}}", name],
            capture_output=True, text=True,
        )
        print(f"{name}: {r.stdout.strip() if r.returncode == 0 else '(absent)'}")
    return 0

if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
```

Two non-obvious lines worth keeping every time:

- **`env.setdefault("MSYS_NO_PATHCONV", "1")`** — without this, Git-Bash on Windows rewrites `/work` to a Windows path before `docker run` sees it. The container then errors with a mount path that doesn't exist.
- **No `-it` by default** — `sys.stdin.isatty()` returns True under mintty even though the underlying console isn't a real ConPTY, so a default `-it` would break for the most common Windows shell. Power users opt in via an env var.

Optionally add named subcommands following the zccache `perf_local.py:687-693` pattern: `fmt`, `clippy`, `test`, `shell`. Each is a thin wrapper around the same `docker run` shape but with pre-baked `cargo` arguments — the value is that users don't have to remember the full `--workspace -- -D warnings` incantation.

### Step 7 — Verify the fingerprint actually works

Cold/warm validation — this is the only thing that proves the setup works:

```bash
python ci/perf_local.py --wipe                            # start cold
time python ci/perf_local.py cargo build --release        # cold build (~5–8 min)
time python ci/perf_local.py cargo build --release        # no-op rebuild
```

The second run must finish in seconds. If it rebuilds the world, something is wrong — most likely a host bind mount snuck in where a named volume belongs, or `CARGO_TARGET_DIR` is unset and cargo wrote into a temp path the next run can't see.

Touch one file and re-run — only the touched crate (and its reverse deps) should compile:

```bash
touch crates/<pkg>/src/lib.rs
time python ci/perf_local.py cargo build --release        # should be 1 crate + final link
```

### Step 8 — Document the migration path

If the project previously used a host-bind-mount `target/` directory, leave a note explaining how to reclaim the disk after switching:

```
# After switching to ci/perf_local.py, the host-side target/ is orphaned:
rm -rf target/

# Reset volume state if the cache ever gets confused:
python ci/perf_local.py --wipe
```

## Decision matrix

| Situation                                                       | Variant                                  |
| --------------------------------------------------------------- | ---------------------------------------- |
| Single binary or workspace, one base image                      | A (single-image, soldr pattern)          |
| Need musl + glibc binaries, or slim runtime image               | B (split builder/runner, zccache pattern)|
| One-shot `docker build` repro of a bug (issue verification)     | Inline `RUN --mount=type=cache` (cc-test)|
| Project uses soldr on the host                                  | Install soldr in image, mount soldr-home volume |
| Project never invokes `cargo fmt` / `clippy` inside Docker      | Skip the `RUSTUP_HOME` volume            |
| Tests mutate per-user state (soldr daemon, gpg keyring, etc.)   | Add a second named volume wiped per run  |

## Failure modes to avoid

- **Host bind mount for `target/`.** Even on Linux this loses caching benefits across container restarts because `$(pwd)/target` is shared with host cargo and they fight over fingerprints. On Windows + WSL2 it's catastrophic (~6 min no-op rebuild).
- **`COPY . .` in the Dockerfile.** Defeats the volume strategy. Every source edit busts the layer cache and triggers a full rebuild. Only `COPY rust-toolchain.toml` / `Cargo.toml` / `Cargo.lock` early if you want to seed the dep graph in the image; even then, prefer a pure volume-mount setup unless cold-start latency matters more than incremental speed.
- **`cargo clean` in the Dockerfile.** Wipes the volume-mounted `/target` on every image rebuild. Don't.
- **Missing `MSYS_NO_PATHCONV=1`** in the orchestrator. Symptom on Windows: `docker run` reports a mount path with `C:/Program Files/Git/work` baked into it. Fix: set the env var in the subprocess call.
- **Default `-it` in the orchestrator.** Breaks Git-Bash users (`the input device is not a TTY`). Opt in via env var.
- **Bind-mounting the host's `~/.soldr`** when the container also runs soldr. soldr is a per-user singleton; the container will corrupt the host daemon's state. Always use a named volume.
- **Forgetting `--init`.** Without it, Ctrl-C inside `cargo test` won't propagate cleanly and zombie processes pile up. Cheap insurance.
- **Persisting `RUSTUP_HOME` but not pinning the toolchain in the image.** First run inside the warm volume installs a new toolchain, the image's pinned default disagrees, and subsequent runs use whichever wins the `which rustc` lookup. Always pin both.

## When NOT to use this skill

This skill is for **dev iteration**. It is the wrong tool when the user's intent is **deployment**. Specifically, do NOT invoke `/clud-docker-rust-app-dev` (and do NOT silently produce its output) when:

- **The user wants a deployment / distribution image.** A `docker push`-bound image — multi-stage build that produces a slim final image carrying just the release binary — is a fundamentally different artifact. Pure Docker layer caching is fine for that path because you only build per release, not per save. Use a standard multi-stage Dockerfile with `cargo chef` or `RUN --mount=type=cache,target=...` instead. The dev-iteration skill's named-volume strategy is *useless* for deployment images (the volume doesn't exist at `docker push` time) and the bind-mounted source pattern is *wrong* for deployment images (the image must be self-contained).
- **The user wants to publish a container to a registry** (`docker push`, GHCR, Docker Hub, AWS ECR, etc.). Same reason — that's a deployment pipeline, not a dev loop.
- **The user wants a reproducible CI-only build on Linux runners** (no Windows host involved). Host bind mounts are fine on Linux ext4; the named-volume cost is purely a Windows + macOS Docker Desktop fix. Reach for `cargo chef` + layer caching there.
- **The project is a tiny single-binary CLI where the full build is already under a minute.** The named-volume setup is overkill — a normal Dockerfile is enough.

The mental test: "after this skill runs, will the user's next move be `python ci/perf_local.py cargo test` (dev loop) or `docker push ghcr.io/foo/bar:1.2.3` (deployment)?" If the answer is the second one, this is not the right skill.

## End state

After this skill runs, the project should have:

- A Dockerfile at `docker/<name>/Dockerfile` (or `ci/docker/`) that bakes the toolchain pin and apt deps and does **not** COPY source.
- A Python orchestrator at `ci/perf_local.py` (or the project's equivalent) that wraps `docker run` with named-volume flags and `MSYS_NO_PATHCONV=1`.
- Three named Docker volumes: `<proj>-perf-target`, `<proj>-perf-cargo-home`, and (optionally) `<proj>-perf-rust-state`.
- A README block in the orchestrator's docstring listing volume names + the `--wipe` recovery recipe.
- A measured cold/warm comparison committed in either the README or a perf doc, proving the no-op rebuild is seconds rather than minutes.

The user should be able to run `python ci/perf_local.py cargo build --release` and get a fast incremental build, even on Windows.
