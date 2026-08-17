---
name: clud-docker-recover
description: "Diagnose and recover a wedged Docker Desktop, missing engine pipe/socket, stopped or failed WSL distro, backing-volume exhaustion, ext4 journal abort/read-only remount, or oversized/corrupt docker_data.vhdx / Docker.raw. Use for safe restart/reset, dangling-object cleanup, disk-pressure diagnosis, VHD relocation/compaction guidance, and explicitly confirmed destructive VHDX deletion plus clean reinitialization. Start with read-only diagnosis; resolve storage from Docker Desktop's real config; preserve images/volumes unless the user explicitly confirms deletion. Do not use for healthy-daemon build workflows or macOS-x86 emulation."
triggers:
  - "When Docker Desktop is wedged: the engine pipe/socket is absent while the backend or UI stays alive, or `docker` commands hang or report the daemon is unreachable"
  - "When the WSL distro backing Docker fails to start, is stopped, or the engine won't come up after a reboot or update"
  - "When the user asks about Docker VM disk growth, backing-volume exhaustion, memory pressure, or an oversized/corrupt docker_data.vhdx / Docker.raw"
  - "When the user wants a safe restart/reset, dangling-object cleanup, VHD relocation/compaction guidance, or an explicitly confirmed destructive VHDX deletion and clean reinitialization"
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
| `engine-wedged` | Distro **Running**, server answers **5xx**, RAM+disk fine — the #891 case | `restart` → `reset` → gated **data-disk reset** (`disk --action delete`) |
| `storage-pressure` | Server down + low free disk | Free host disk; only then consider the gated `disk` flow |

`engine-wedged` is the one classification whose remedy ladder does **not** end
at `reset`. It means the engine's data disk is corrupt: in the #891 incident
the distro booted fine and `dockerd` never became healthy, `restart` and
`reset` both failed, and only deleting `docker_data.vhdx` fixed it (Docker
recreates an empty one). Confirm it against the backend log at
`%LOCALAPPDATA%\Docker\log\host\com.docker.backend.exe.log`, which shows
`still waiting for linux/wsl init control API to respond` and
`still waiting for the engine to respond to _ping ... HTTP 500`.

Do not read a 5xx engine as "docker is not installed". `doctor` now
distinguishes CLI-missing / server-unreachable / server-5xx explicitly; a
timeout on `docker version` means the **engine** is sick, not the CLI.

## Run restart/reset backgrounded under `clud tool run`

`restart` and `reset` can exceed the tool runner's **120s progress timeout**
and get killed with exit 124 — this happened twice in #891, both times at
exactly 120s, because `docker run --rm hello-world` blocks silently for up to
120s. The tool now prints a heartbeat before and during every long wait, but a
cold Docker Desktop start still routinely takes 60–120s on its own. So:

- prefer `clud tool run --background docker/docker_recover.py reset --yes`
  (or raise the progress timeout) for `restart`/`reset`;
- `doctor`, `gc` and `disk` are *usually* short. On a badly wedged host
  `doctor` can still spend several 20s probes in `gather_snapshot` before it
  prints anything, so background it too if the engine is unresponsive.

## Bounded readiness polling

After any launch attempt, poll the engine on a bounded schedule — 10
attempts, 2-second interval (the numbers from #531; the FastLED WASM
`8cf7f663` Windows Docker/WSL readiness-retry precedent). A **cold** start
gets the longer `READY_ATTEMPTS_COLD` budget (60 attempts) instead, because
20s is well short of a Docker Desktop cold start. Never spin unbounded:

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

### When WSL itself is wedged (`reset --yes` only, issue #632)

If `wsl --shutdown` times out — the signature of `WslService` stuck in
`STOP_PENDING`, where every `wsl` invocation hangs — `reset --yes` escalates to
one UAC-elevated, tightly scoped recovery. `restart --yes` stays the light path
and never does this.

Every check is **fail-closed**; anything unexpected refuses and says why:

| Check | Refuses when |
| --- | --- |
| state | not exactly `STOP_PENDING` (a running service is fine, a stopped one needs a plain `sc start`) |
| PID | absent or 0 — nothing safe to terminate |
| image | the SCM-registered binary is not `wslservice.exe` |
| re-check | the PID moved between diagnosis and termination (the UAC prompt is exactly the window in which a PID can be recycled) |

The elevated step is one `taskkill /F /PID <validated>` plus
`sc.exe start WslService`, then a bounded wait for `RUNNING` before relaunching
Docker Desktop. It never unregisters a distro, deletes a VHD, or touches Docker
images/volumes — a test asserts the elevated command contains no such token.
Elevation being declined is reported plainly and leaves WSL untouched.

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

### Full backing volume can corrupt the guest filesystem

Check free space on the **physical volume containing the resolved VHDX**, not
only the Windows system drive. A Docker VHDX on a relocated drive can exhaust
that drive while `C:` still has plenty of space. The decisive WSL signature is:

- `I/O error, dev <device>` or `Buffer I/O error`;
- `Journal has aborted` / `Detected aborted journal`;
- `Remounting filesystem read-only`.

When these appear, stop treating the incident as an ordinary
`engine-unavailable` restart. Stop Docker Desktop and run `wsl --shutdown` to
prevent more writes. Report the resolved VHDX path, its size, and free bytes on
its actual host volume. Freeing host space may permit a subsequent mount, but
an aborted ext4 journal can still require repair or replacement. Never loop on
restart while the host volume remains full.

## The VHD stays locked after `wsl --shutdown` (issue #891)

`wsl --shutdown` is **not** enough to release `docker_data.vhdx`. It stays
SURFACE-ATTACHED at the Hyper-V/HCS level — `Get-DiskImage -ImagePath <vhdx>`
still reports `Attached : True` — and both `Remove-Item` and `Optimize-VHD`
fail with *"being used by another process"*. Verified across repeated
attempts, including with `WslService` already stopped.

The tool's `disk --action delete|compact` plan therefore includes an explicit
unlock step. Follow it in order:

1. Stop the `com.docker.service` Windows service **first**, then quit Docker
   Desktop — the UI respawns with new PIDs and can re-attach the disk
   mid-operation.
2. `wsl --shutdown`.
3. `Dismount-DiskImage -ImagePath '<vhdx>'` — this released it instantly, and
   the delete then succeeded on the first attempt.
4. Verify `Get-DiskImage -ImagePath '<vhdx>'` reports `Attached : False`.
5. Re-check for respawned Docker Desktop / `com.docker.backend` /
   `com.docker.build` processes immediately before mutating the disk.

Still attached? Stop `WslService` (and `LxssManager`) and retry; as a last
resort stop `vmcompute` (Hyper-V Host Compute Service), noting its dependents
(e.g. `hns`) so you can restart them, then retry and restart the services.

Note the tool's `docker_stopped` gate (server unreachable + no docker
processes) is necessary but **not sufficient** — it passes while the VHD is
still attached. The `Attached : False` check is the real gate.

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
relocates a Docker VHD / `Docker.raw` / data-root on its own.** If diagnosis
shows an unrecoverable or disposable Docker data VHDX, **ask the user whether
they want to permanently delete it and initialize Docker from scratch**. Do
not merely mention deletion as an option and do not infer consent from a
generic request to repair or restart Docker.

Before any storage action the tool requires, in order:

1. An **unambiguous single candidate** (ambiguity always wins over action —
   even `--yes` is refused while candidates are ambiguous, exit code 4).
2. Explicit **`--yes`** confirmation (exit code 3 otherwise).
3. Docker Desktop / WSL **fully stopped**, and a **backup** of the disk.

Even with all gates satisfied, v0 prints the vetted backup + compaction plan
(`Optimize-VHD`, prune, delete, factory-reset) rather than executing it
(exit code 64). Use the [[clud-tag-release]] confirmation discipline: print
the plan, wait for an explicit decision, never proceed on silence.

### Explicit VHDX deletion and clean reinitialization

Use this path only after the user explicitly confirms deletion. State plainly
that deletion irreversibly removes every Docker image, container, volume, and
build cache stored in the VHDX. Treat a request such as "completely delete the
VHDX, then restart" as confirmation; otherwise ask and wait.

On Windows:

1. Re-run `doctor` and require exactly one high-confidence active WSL disk.
2. Run `wsl --shutdown`; verify `docker-desktop` is `Stopped` and no Docker
   Desktop/backend/build processes remain.
3. Resolve the literal path again and verify the target is the expected
   `docker_data.vhdx`. Never delete a computed, ambiguous, or default-assumed
   path.
4. Delete only that file. If normal PowerShell deletion is denied and the user
   approves elevation, launch one UAC-elevated PowerShell process whose sole
   destructive action is `Remove-Item -LiteralPath <validated-path> -Force`.
   Do not elevate Docker Desktop itself.
5. Verify the old file is absent and report space reclaimed on the backing
   volume.
6. Launch the validated Docker Desktop executable non-elevated. A fresh VHDX
   should appear small and grow dynamically. Initialization can remain
   `starting` beyond the normal 20-second poll, so inspect WSL state and `dmesg`
   for renewed disk/ext4 errors before allowing one additional bounded poll.
7. Require `docker desktop status` = `running`, a working server response from
   `docker version`, a healthy `docker buildx ls`, and a successful
   `docker run --rm hello-world` before declaring recovery complete.

If the new VHDX appears and `dmesg` has no recurrence of the prior I/O/journal
errors, a brief `starting` state is initialization rather than proof of another
failure. Preserve the original diagnosis in the final report and explicitly
record that the old Docker data was deleted at the user's request.

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
