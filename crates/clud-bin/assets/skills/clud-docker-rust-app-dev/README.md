# clud-docker-rust-app-dev

**Dev iteration harness, NOT a deployment image.** Containerize a Rust application for fast incremental Docker builds during local development. The skill encodes the named-volume + bind-mounted-source pattern proven out in `~/dev/soldr/docker/cook-shared-cache/` and `~/dev/zccache/ci/docker/`, then layers on the soldr toolchain wrapper, a Python orchestrator, and the Windows-specific workarounds (`MSYS_NO_PATHCONV=1`, opt-in `-it`) so the result actually works on Docker Desktop on Windows — which is where the naive "just bind-mount `target/`" approach loses minutes per iteration.

The `-dev` suffix is load-bearing: this skill produces a **per-developer scratch container** (host source bind-mounted, cargo state in named volumes, image never pushed). For images destined for `docker push` → a registry → production, see "When NOT to use this" in the SKILL.md and reach for a standard multi-stage Dockerfile (`cargo chef`, `RUN --mount=type=cache`) instead.

## When it triggers

- Explicit invocation: `/clud-docker-rust-app-dev`.
- Implicit triggers in the frontmatter cover phrases like "turn this into a docker", "dockerize this", "containerize this rust app", and any request to make a Rust build reproducible inside Docker **for local dev / iteration**.
- An explicit anti-trigger in the frontmatter blocks invocation when the user asks for a distribution image, a `docker push`-bound build, or anything destined for production. Those are deployment paths and need a different Dockerfile shape.

## Source-of-truth references

- `~/dev/soldr/docker/cook-shared-cache/Dockerfile` — minimal single-image Dockerfile with explicit `ENV CARGO_HOME` + toolchain pin.
- `~/dev/soldr/bench/cook_in_docker.sh` — bash runner showing the three named volumes (`cook-soldr-home`, `soldr-perf-target`, `soldr-perf-cargo-home`).
- `~/dev/soldr/ci/perf_local.py` — minimal Python orchestrator (cargo passthrough only).
- `~/dev/zccache/ci/docker/{soldr-builder,zccache-builder,runner}.Dockerfile` — split builder/runner setup for multi-target output.
- `~/dev/zccache/ci/docker/README.md` — explainer for the three-image rationale.
- `~/dev/zccache/ci/perf_local.py` — rich orchestrator with `fmt`/`clippy`/`test`/`shell` subcommands and `RUSTUP_HOME` volume for cached `rustup component add`.
- `~/dev/zccache/Dockerfile.cc-test` — example of `RUN --mount=type=cache` inside a one-shot build-and-test image.

## Why this matters

Cargo's incremental build relies on file mtimes matching across runs. Host bind mounts on Windows + Docker Desktop go through a WSL2 9P translation layer that rewrites mtimes per container start. That means a "no-op" rebuild rebuilds the whole workspace — measured at 4–6 minutes on a 21-crate workspace, vs ~1 s when the same `target/` lives in a named Docker volume on Linux-native ext4.

Same pain shows up on macOS Docker Desktop (different translation layer, same outcome). Linux hosts are fine, but the named-volume pattern is still a good default because it cleanly isolates container-built artifacts from any host `cargo build` running concurrently.

## How it ships

This skill is bundled into the `clud` binary via `crates/clud-bin/src/skills.rs` and auto-installed on launch into `~/.claude/skills/clud-docker-rust-app-dev/SKILL.md` and `~/.codex/skills/clud-docker-rust-app-dev/SKILL.md` when Codex is installed. The `skills.rs` installer never overwrites existing files, so once a user edits their installed copy, those edits stick. See `docs/architecture/skill-system.md` for the full installer model.
