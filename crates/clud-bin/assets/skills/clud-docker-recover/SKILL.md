---
name: "clud-docker-recover"
description: "Diagnose and recover a wedged local Docker engine or Docker Desktop on Windows, macOS, or Linux. Use when Docker commands cannot reach the daemon, builds stall after a cancelled Docker process, Docker Desktop will not start, or the user asks where Docker's VHD/raw storage actually lives. Resolves configured storage paths before defaults and never performs automatic VHD compaction, deletion, reset, prune, or other storage mutation."
triggers:
  - "When the user asks to diagnose, recover, restart, or un-wedge Docker or Docker Desktop"
  - "When Docker reports a client but cannot connect to the daemon or Docker Desktop is stuck starting"
  - "When the user asks to locate Docker Desktop's VHD, Docker.raw, data-root, or nonstandard Docker storage location"
  - "When a Docker build was cancelled or an agent may have killed Docker's build worker"
---
<!-- managed-by: clud -->

# /clud-docker-recover

Start with a read-only report:

```
clud tool run docker/docker_recover.py doctor
```

Trust the configured path before a platform default. On Windows, read `%APPDATA%\\Docker\\settings-store.json` (then the legacy settings file), follow `CustomWslDistroDir`, check its known `disk/docker_data.vhdx` and `data/docker_data.vhdx` layouts, resolve junctions/symlinks, and perform only a bounded scan under configured Docker roots. Treat `DataFolder` as a separate legacy/Hyper-V setting, not proof that the WSL VHD lives there. Never recursively scan the whole profile or assume `C:` is active.

Interpret the report before acting:

- **Engine available:** Do not restart it merely because a build is slow; inspect the build logs first.
- **Client available but server unavailable:** Capture `doctor` output. On Windows, check the WSL state and whether Docker Desktop is still running.
- **Cancelled/stalled build:** Do not kill `com.docker.build` or Docker backend children. Forced termination can leave Desktop unable to create the Linux engine pipe.
- **Storage found:** Report its path, size, and free space. A VHD on a non-system drive is normal. Low free space is evidence to discuss backup/compaction, not permission to do it.

Use the explicit recovery path only after the user agrees that active containers may stop:

```
clud tool run docker/docker_recover.py restart --yes
```

This asks Docker Desktop to restart when possible. Its Windows fallback closes the Desktop window, runs `wsl --shutdown`, relaunches Docker Desktop, waits for `docker version`, then runs `hello-world`. Use `--no-smoke` only when pulling/running the smoke image is not appropriate. The Linux path requires passwordless `sudo`; if unavailable, print the exact manual `sudo systemctl restart docker` command rather than hanging for credentials.

## Safety boundary

Do not compact, delete, reset, unregister, prune, or otherwise mutate Docker storage automatically. Before any VHD/Docker.raw operation, require an explicit user decision, identify one unambiguous active path from `doctor`, confirm a backup location, stop Docker cleanly, and state the platform-specific recovery risk. If candidates are ambiguous, stop at diagnosis.

## Code change discipline

When modifying this tool or skill, use RED -> GREEN: first add a focused registry/contract test in `tools.rs` or `skills.rs`, then change the embedded asset and registration, then run the targeted test plus the tool's `doctor --json` smoke test. Preserve the path-chasing and non-mutation guarantees.
