---
name: "clud-docker-linux-build"
description: "Spin up a fast Linux build container for a Rust + soldr + zccache, Python (uv), or C++ (CMake + ccache) project using the bundled `docker-build` tool family. Uses anonymous Docker volumes for build state and a read-only bind for source — the one rule that turns Docker-Desktop's 20-minute cold-build into a sub-30-second warm cycle."
triggers:
  - "When the user types /clud-docker-linux-build"
  - "When the user is on Windows or macOS and needs to reproduce a Linux CI build locally without burning a GitHub Actions cycle"
  - "When the user complains about slow Docker rebuild loops on Docker-for-Windows or Docker-for-Mac"
  - "When the user asks to set up a per-project Linux container that survives cold→warm cycles for a Rust, Python, or C++ project"
  - "Do NOT trigger when the user wants a production multi-stage image (use cargo-chef + multi-stage instead); when the user is on a native Linux host (host bind mounts are already fast there); when the user needs macOS-x86 emulation (use /clud-docker-mac-x86)"
---
<!-- managed-by: clud -->

# /clud-docker-linux-build

**Use the bundled tool — do not hand-write a Dockerfile.** The `clud tool run docker/docker-build.py` family ships verified Dockerfiles + entry scripts for Rust + soldr, uv-Python, and CMake + ccache. They're written to disk via `init`; the volume contract, path conversion, and mtime gotchas are already baked in.

## Concrete entry point

```
clud tool run docker/docker-build.py soldr  <repo-root> init    # write Dockerfile to <repo>/.clud/docker-build/soldr/
clud tool run docker/docker-build.py soldr  <repo-root> up      # build image + start container
clud tool run docker/docker-build.py soldr  <repo-root> run -- soldr cargo check
clud tool run docker/docker-build.py soldr  <repo-root> shell
clud tool run docker/docker-build.py soldr  <repo-root> clean   # wipe volumes; force cold next time

clud tool run docker/docker-build.py python <repo-root> init    # python stack (v0: init only)
clud tool run docker/docker-build.py cpp    <repo-root> init    # cpp stack (v0: init only)

clud tool run docker/docker-build.py doctor                     # cross-stack health check
```

The trampoline dispatches in-process to the right per-stack tool — invoking directly (`clud tool run docker/docker_build_soldr.py <repo> init`) is exactly equivalent, just less ergonomic.

## The one rule that makes this work

| What | Mount type |
|---|---|
| Source code | **read-only bind mount** (`-v $repo:/src:ro`) |
| `target/`, `CARGO_HOME`, `RUSTUP_HOME`, `cargo-chef` | **anonymous (named) Docker volumes** |
| Build output (binaries you want out) | anon volume → `docker cp` at the end (NEVER host bind) |

Host bind mounts on Docker-for-Windows / Docker-for-Mac pay a 5-10× FS-translation tax through the FUSE / 9p / virtiofs layer between the Linux VM and the host filesystem. Named volumes live inside Docker's own native ext4 — no translation. Observed datapoint from the zackees/zccache prototype: cold-build 20m22s with host bind → ~3 min with anon volume. Same machine, same image, single config change.

## Path conversion — the Windows trap

Each shell mangles `docker -v` arguments differently:

| Shell | `-v $repo:/src` behavior |
|---|---|
| `cmd.exe` | Pass `C:\path\to\repo:/src` literally. Works. |
| MSYS Git Bash | Mangles `/src` → `C:/Program Files/Git/src`. **Docker rejects.** Set `MSYS_NO_PATHCONV=1` to disable, OR run from PowerShell instead. |
| PowerShell | Native path handling. Works. **Recommended.** |
| WSL2 bash | POSIX paths native, but `C:\` isn't reachable — use `/mnt/c/...`. |

The bundled tools detect MSYS shells in `doctor` and warn loudly. If `doctor` says "MSYS shell detected", switch to PowerShell before continuing.

## mtime — the silent correctness footgun

Incremental builders (cargo, ccache, make, ninja) compare **source mtime vs build-output mtime** to decide what to rebuild. Two traps lurk:

1. **Container/host clock skew > 1s** makes fresh build outputs look older than source — every "no-op" rebuild becomes a full rebuild. `doctor` measures this; on Docker Desktop, set `clock=host` in Settings → General if skew is high.
2. **`git checkout` rewrites every file's mtime** to "now". Cargo treats the whole tree as freshly modified. Use `git restore-mtime` (from the `git-mtime` apt/brew package) after switching branches if warm cycles matter.

## When NOT to use this skill

- **Native Linux developer:** host bind mounts are fast on Linux; just `cargo build`. The tool detects this and short-circuits.
- **Production / shippable images:** use multi-stage Dockerfile with cargo-chef instead. See `/clud-docker-rust-app-dev` for the development-vs-production discussion.
- **macOS x86 emulation:** use `/clud-docker-mac-x86`.
- **Single-shot one-off builds:** no warm cycle to optimize; just run `docker run --rm rust:1.94 cargo build` and move on.

## v0 scope (this PR — zackees/clud#421)

- **soldr stack:** init + up + run + shell + clean + doctor — full implementation
- **python stack:** init only (Dockerfile + entry.sh + stack.toml writeout). Other subcommands return exit 64 with a clear notice.
- **cpp stack:** same as python.
- **verify:** stub on every stack (exit 64). The cold + warm-no-op + single-edit benchmark is the next slice.

## Code change discipline

When extending the bundled tool family (adding a stack, hardening a subcommand, fixing a path-conversion edge case), follow the standard clud RED -> GREEN -> REFACTOR loop: write a failing `tools::tests` guardrail asserting the new invariant first; ship the minimal embedded-asset / dispatch change to make it pass; then refactor without changing the test surface. The existing guardrails in `crates/clud-bin/src/tools.rs` (`bundled_includes_docker_build_family`, `docker_build_trampoline_documents_dispatch_shape`, `docker_build_stack_v0_scopes_match_issue_421`) are the precedent — extend or add to them rather than working around them.

## Related skills

- `/clud-docker-rust-app-dev` — the *pattern* this tool implements as a bundled artifact. Read it for the architectural reasoning if you're customizing the soldr Dockerfile.
- `/clud-docker-mac-x86` — for the orthogonal macOS-x86 emulation case (different concern entirely).

## Origin

- zackees/clud#416 — architectural design + open-questions discussion
- zackees/clud#421 — implementation slice (this PR)
- zackees/zccache#785 — the consumer that prompted the work; `.perf-local/docker-repro/` in that repo is the working prototype the soldr stack derives from
