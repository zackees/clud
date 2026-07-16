---
name: clud-docker-recover
description: "Diagnose and recover a wedged Docker Desktop (engine pipe/socket absent while the UI stays alive, WSL/Docker startup failures) and answer Docker VM disk-growth / memory-pressure questions. Read-only `doctor` first; every restart/reset is confirmation-gated and preserves images/volumes; storage disks are resolved from Docker Desktop's real config (never assumed) and never compacted or deleted automatically."
triggers:
  - "When Docker Desktop hangs or `docker` commands fail with a missing engine socket/pipe (e.g. `open //./pipe/dockerDesktopLinuxEngine: The system cannot find the file specified`)"
  - "When WSL or Docker Desktop fails to start, or `wsl -l -v` shows the docker-desktop distro Stopped/Installing"
  - "When the user asks why the Docker VM disk (docker_data.vhdx / Docker.raw) is huge, or about Docker memory/disk pressure"
  - "When the user wants to safely reclaim Docker disk space or relocate/compact the Docker data disk"
  - "When the user wants to clean up / garbage-collect / trim unused Docker images, stopped containers, or dangling volumes, or wants Docker dev disk usage to stop growing unbounded (including on a schedule)"
  - "Do NOT trigger for fast incremental Linux build loops (use /clud-docker-linux-build); for macOS-x86 emulation (use /clud-docker-mac-x86); or when Docker is already healthy and the user just wants to run a container"
---
<!-- managed-by: clud -->

# /clud-docker-recover

Recover a wedged Docker Desktop the way zackees/clud#531 was recovered:
**diagnose non-destructively, classify the failure before acting, restart on
a bounded schedule, verify against the real daemon, and never touch a Docker
storage disk without explicit confirmation.** The incident that motivated
this skill had the engine pipe absent while the backend/UI stayed alive —
root cause a killed `com.docker.build` child, NOT memory or disk pressure —
so *classification comes before action*.

Everything routes through the bundled tool:

```
clud tool run docker/docker_recover.py doctor            # read-only; mutates nothing
clud tool run docker/docker_recover.py gc [--age-hours N] [--dry-run]  # reclaim dangling objects (safe)
clud tool run docker/docker_recover.py restart [--yes]   # clean runtime restart
clud tool run docker/docker_recover.py reset [--yes]     # wsl --shutdown + relaunch
clud tool run docker/docker_recover.py disk [--action compact|prune|delete|reset] \
    [--select <path>] [--yes]                            # storage report; gated actions
```

## Always start read-only

`doctor` never mutates state — no restart, no disk write, not even a log
rotation. Run it first, every time:

```
clud tool run docker/docker_recover.py doctor
```

It reports client/server availability, the engine error, host free memory +
disk, Docker runtime processes, the resolved Docker data-disk path/size +
confidence, `wsl --status` / `wsl -l -v` (Windows), and the failure
classification. Read the classification before choosing a remedy.

| Classification | Meaning | Remedy |
|---|---|---|
| `healthy` | Server reachable | Nothing to do (low disk/memory surface as ADVISORY, never blocking) |
| `engine-unavailable` | Server down, host has RAM+disk to spare — the #531 case | `restart` (or `reset` if the WSL distro is Stopped) |
| `resource-pressure` | Server down + low free memory | Free memory / raise Docker's memory limit, then `restart` |
| `storage-pressure` | Server down + low free disk | Free host disk; only then consider the gated `disk` flow |

## Bounded readiness polling

After any launch attempt, poll the engine on a bounded schedule — 10
attempts, 2-second interval (the numbers from #531; the FastLED WASM
`8cf7f663` Windows Docker/WSL readiness-retry precedent). Never spin
unbounded:

```
fn ensure_docker_running(attempts = 10, interval = 2s) -> Result<()> {
    if docker_server_ok() { return Ok(()); }
    launch_runtime_for_platform();        // non-blocking
    for i in 0..attempts {
        if docker_server_ok() { return Ok(()); }
        if i < attempts - 1 { sleep(interval); }
    }
    Err("engine not ready after bounded wait; diagnosis preserved")
}
```

## Platform dispatch

| Platform | Runtime | Restart sequence | Storage disk |
|---|---|---|---|
| Windows | Docker Desktop + WSL2 | stop orphaned helpers → `wsl --shutdown` → `docker desktop start` (or launch Docker Desktop.exe) → bounded poll | Resolved from config — NOT assumed (see below) |
| macOS | Docker Desktop | quit → `open -a Docker` / `docker desktop start` → bounded poll | `Docker.raw` (query settings for a relocated disk first) |
| Linux | Docker Engine | `sudo systemctl restart docker` (or `sudo service docker restart`) → bounded poll | data-root, normally `/var/lib/docker` (confirm with `docker info -f '{{.DockerRootDir}}'`) |

Restart and reset **stop running containers** but preserve images and volumes.
The tool states this plainly before acting and refuses without `--yes`.

## Windows storage resolver — do NOT assume the default path

`%LOCALAPPDATA%\Docker\wsl\data\docker_data.vhdx` is only the *fallback
default*. In the #531 incident, Docker Desktop's `settings-store.json` set
`CustomWslDistroDir = E:\docker\wsl` and the live 29.5 GiB disk was
`E:\docker\wsl\disk\docker_data.vhdx` — not on C: at all. `DataFolder`
(configured separately as `C:\ProgramData\DockerDesktop\vm-data`) is a
Hyper-V/legacy VM location and MUST NOT be treated as the WSL engine disk.

The resolver therefore:
1. Reads `%APPDATA%\Docker\settings-store.json` (legacy `settings.json` as
   fallback).
2. Honours `CustomWslDistroDir` first, resolving junctions/symlinks, probing
   `disk\docker_data.vhdx`, `data\docker_data.vhdx`, and constrained
   `*.vhdx` below that root.
3. Inspects `DataFolder` **separately** as Hyper-V/legacy — never conflated
   with WSL storage.
4. Falls back to a short explicit set (`%LOCALAPPDATA%\Docker\wsl`,
   `%LOCALAPPDATA%\DockerDesktop`, configured `DataFolder`, WSL distro base)
   only when settings are missing/stale — never a recursive profile scan.
5. Scores every candidate (configured-parent match, exact
   `docker_data.vhdx` filename, resolved path, recent write) and reports
   each with size + confidence.
6. If more than one candidate stays plausible, **refuses** backup /
   compaction / deletion / reset / relocation until the user selects one
   with `--select <path>`.

## Garbage collection — the lightest rung (default-safe)

`gc` (alias `trim`) reclaims **dangling** Docker objects so dev disk usage
stops growing unbounded. It is deliberately *distinct* from the VHD/raw-disk
tier: pruning an image/container/anon-volume is cheap and reversible
(rebuilding is fast), whereas compacting the backing disk is not. So `gc`
runs **default-safe — no confirmation prompt** — but still never touches:

- running containers, or images backing a running container;
- **named volumes** (mirrors `docker volume prune` without `-a`: only
  anonymous/unreferenced volumes are candidates — named volumes almost
  always hold intentional persistent data);
- anything below the age threshold (default 24h for images/containers).

It reports counts + freed bytes every run. Use it as the FIRST rung when
storage pressure appears — before restart/reset/disk-remediation. `doctor`
prints the escalation ladder (`gc -> restart -> disk`, lightest first) when
disk is low.

```
clud tool run docker/docker_recover.py gc --dry-run     # preview candidates
clud tool run docker/docker_recover.py gc               # reclaim (safe, no --yes needed)
clud tool run docker/docker_recover.py gc --age-hours 72
```

**More aggressive on the system/boot volume.** When the resolved Docker data
disk sits on C: (or the macOS/Linux system volume) — typically smaller and
shared with the OS — the age threshold is halved so GC reclaims sooner. On a
dedicated data drive it stays at the default. The Windows resolver (above)
decides which physical drive the data root is on.

**Periodic use.** `gc` is an idempotent one-shot — wire it into a schedule so
it runs even when nobody hits a low-disk wall: `clud schedule`, cron, or
Windows Task Scheduler calling `clud tool run docker/docker_recover.py gc`.
The tool does NOT embed its own daemon/scheduler; trigger it externally.

## Storage remediation is opt-in and never automatic

The one hard rule: **this skill never compacts, prunes, deletes, resets, or
relocates a Docker VHD / `Docker.raw` / data-root on its own.** Before any
storage action the tool requires, in order:

1. An **unambiguous single candidate** (ambiguity always wins over action —
   even `--yes` is refused while candidates are ambiguous, exit code 4).
2. Explicit **`--yes`** confirmation (exit code 3 otherwise).
3. Docker Desktop / WSL **fully stopped**, and a **backup** of the disk.

Even with all gates satisfied, v0 prints the vetted backup + compaction plan
(`Optimize-VHD`, prune, delete, factory-reset) rather than executing it
(exit code 64). Use the [[clud-tag-release]] confirmation discipline: print
the plan, wait for an explicit decision, never proceed on silence.

## Verify recovery, preserve the diagnosis

A restart/reset is only "done" once verification passes: the tool checks the
server API (`docker version`) AND runs a minimal container
(`docker run --rm hello-world`). The final report keeps the ORIGINAL failure
diagnosis alongside the verification result, so a failed recovery still
tells you what was wrong.

## v0 scope

- **doctor** — full read-only report on Windows / macOS / Linux, including
  the Windows config-driven storage resolver and the escalation-ladder
  recommendation.
- **gc / trim** — full: dangling-object reclaim (unused images, stopped
  containers, anonymous unreferenced volumes) with age-threshold + system-
  volume-aware aggression; default-safe; reports counts + freed bytes;
  `--dry-run` preview.
- **restart / reset** — full: bounded readiness wait + verify; `--yes`-gated;
  images/volumes preserved. `reset` adds `wsl --shutdown` on Windows.
- **disk** — full report + full refusal gate on Windows. The destructive
  action itself is NOT auto-executed in v0 (prints the vetted plan, exit
  64). macOS/Linux storage is report-only.

## Code change discipline

When extending this tool, follow the clud RED -> GREEN loop: add or extend a
focused failing test first, then implement to green. The Python resolver /
doctor logic is unit-tested in `tests/test_docker_recover.py` (the three
mandatory Windows fixtures from the #531 follow-up live there). The bundle
invariants are locked by Rust guardrails in `crates/clud-bin/src/tools.rs`
(`bundled_includes_docker_recover`, `docker_recover_documents_exit_codes`,
`docker_recover_declares_subcommands`,
`docker_recover_never_auto_mutates_storage`,
`docker_recover_gc_reclaims_only_dangling_objects_safely`) and
`crates/clud-bin/src/skills.rs` (`bundled_includes_all_known_skills`). Extend
those rather than working around them.

## Related skills

- `/clud-docker-linux-build` — fast incremental Linux build containers (a
  healthy-daemon workflow, not recovery).
- `/clud-docker-mac-x86` — macOS-x86 emulation (its own launch-and-wait probe
  bails with a checklist rather than recovering).

## Origin

- zackees/clud#531 — the wedged-Docker-Desktop incident + acceptance criteria.
- zackees/clud#531 (comment 4990040248) — the config-driven Windows storage
  resolver requirement (`CustomWslDistroDir` / `DataFolder`, never assume the
  C: default).
