<!-- managed-by: clud -->

# `docker/` bundled tools

Bundled Python tools that drive Docker-based Linux build harnesses. Installed under `~/.clud/tools/docker/` by the `BundledTool` lifecycle (see `crates/clud-bin/src/tool_install.rs`); invoked via `clud tool run docker/<file>.py`.

## Tools

| File | Role |
|---|---|
| [`docker-build.py`](docker-build.py) | Trampoline. Dispatches to a per-stack tool based on the first arg. Implementation note: filename uses a hyphen to match the public CLI shape (`clud tool run docker/docker-build.py soldr <path>`); sibling per-stack files use underscores because Python module imports cannot tolerate hyphens. |
| [`docker_build_soldr.py`](docker_build_soldr.py) | Rust + soldr + zccache stack. The reference implementation. The image bakes in soldr, and persistent anonymous volumes hold `target/`, `CARGO_HOME`, `RUSTUP_HOME`, the cargo-chef recipe cache, and `/root/.soldr`; source bind-mounted read-only at `/src`. |
| [`docker_build_python.py`](docker_build_python.py) | uv-managed Python stack. **v0 scope: `init` only.** Other subcommands return EX_USAGE (64) with a clear "needs author work" notice. |
| [`docker_build_cpp.py`](docker_build_cpp.py) | CMake + ccache stack. **v0 scope: `init` only.** Same status as python. |

## Invocation shapes

```
clud tool run docker/docker-build.py <stack> <path> [subcommand]   # trampoline
clud tool run docker/docker_build_soldr.py <path> [subcommand]     # direct
clud tool run docker/docker_build_python.py <path> [subcommand]    # direct
clud tool run docker/docker_build_cpp.py <path> [subcommand]       # direct
```

`<path>` defaults to `.`. Subcommands: `init` / `up` / `run` / `shell` / `verify` / `clean` / `doctor` — identical across every per-stack tool so the trampoline is a pure dispatcher.

## Design

See [`docs/architecture/docker-build-tools.md`](../../../../docs/architecture/docker-build-tools.md) for the volume contract, the path-conversion table (cmd.exe vs MSYS Git Bash vs PowerShell vs WSL2), and the mtime troubleshooting matrix. See `skills/clud-docker-linux-build/SKILL.md` for the agent-facing entry point.

## Origin

- zackees/clud#416 — architectural design + skill discussion
- zackees/clud#421 — this implementation slice
- zackees/zccache#785 — the consumer that prompted the work; `.perf-local/docker-repro/` in that repo is the working prototype the soldr stack derives from
