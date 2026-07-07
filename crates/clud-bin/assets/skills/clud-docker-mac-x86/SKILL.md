---
name: clud-docker-mac-x86
description: Run Intel macOS builds and tests locally inside dockur/macos when the host qualifies, instead of waiting for the GitHub Actions macOS x86 runner.
triggers:
  - "When the user wants to build or test code targeting Intel macOS without waiting for GitHub Actions runners"
  - "When the user invokes /clud-docker-mac-x86 with a build or test command"
  - "When the user asks to iterate locally against a mac x86 environment"
---
<!-- managed-by: clud -->

# /clud-docker-mac-x86

Build and test the current project inside [`dockurr/macos`](https://hub.docker.com/r/dockurr/macos) on a qualifying Linux or Windows host, so the developer does not have to wait for the GitHub Actions `macos-15-intel` runner for every Mac x86 iteration. On disqualifying hosts (macOS, ARM, Docker Desktop, Windows 10), the skill bails fast with one diagnostic line — no half-started downloads, no speculative boot attempts.

User-invoked only. The cost of a mis-fire (image pull, multi-hour macOS install, snapshot creation) is too high for an auto-trigger.

## Code Change Rule

If running a build or test inside `dockurr/macos` surfaces a real mac-x86 failure that needs a code change, follow RED -> GREEN: prove the failing case is reproducible inside the guest, write or extend the focused test, implement the scoped fix, then re-run the same command inside the guest until it passes.

## Precondition probe (fail fast, ordered)

The first two checks are the **hard, non-negotiable applicability gates** and must run before anything else (no probe containers, no image pulls, no `docker info` round-trip). They answer "should this skill even be considered on this host?"

| # | Check | Bail message on failure |
|---|---|---|
| 1 | **Host OS is Linux or Windows.** Reject macOS explicitly (Darwin kernel detection: `uname -s` returns `Darwin`, or Rust `std::env::consts::OS == "macos"`). | `Windows or Linux Only — /clud-docker-mac-x86 is not applicable on macOS hosts (dockur/macos compat chart marks macOS as unsupported, and Apple Silicon has no arm64 image).` |
| 2 | **Docker is installed.** Resolve the `docker` binary on `PATH` (`which docker` / `where.exe docker`). This is a separate condition from "the daemon is reachable" — if the CLI itself is missing, the user hasn't installed Docker at all. | `Docker is not installed. Install Docker Engine (Linux) or Docker Desktop (Windows) and re-run. See https://docs.docker.com/engine/install/.` |
| 3 | Host arch is `x86_64`. | `dockurr/macos has no arm64 image; this skill cannot help on ARM hosts.` |
| 4 | Host OS version: Linux any modern kernel, or Windows 11 22H2+ (Windows 10 is rejected here). | `Windows 10 WSL hard-codes "no nested virt" — see https://github.com/microsoft/WSL/issues/40735. Upgrade to Windows 11, or run on Linux.` |
| 5 | **Docker daemon is reachable** (`docker info` exits 0). If first attempt fails, run the platform-specific launch command (see "Docker launch + recovery" below), then poll `docker info` every 2 s for up to **60 s** total. | `Docker did not become reachable within 60 s after attempting to launch it. Last error: <captured stderr>. See "Docker launch + recovery" troubleshooting.` |
| 6 | Docker variant is engine, not Desktop (detected via `docker info --format '{{.OperatingSystem}}'` — Docker Desktop reports literally `Docker Desktop`). | `Docker Desktop LinuxKit VM does not expose /dev/kvm. Install Docker engine inside a WSL2 distro (Win11) or use a Linux host.` |
| 7 | `/dev/kvm` reachable from a container (`docker run --rm --device=/dev/kvm alpine ls /dev/kvm`). | `/dev/kvm not exposed to containers. On Win11+WSL2, set nestedVirtualization=true in .wslconfig and restart WSL.` |
| 8 | Read CPU vendor + flags from a probe container. | If AMD: warn about https://github.com/dockur/macos/issues/268 and force `CPU_CORES=1`. If Intel: allow user-configured cores. |
| 9 | At least 8 GB RAM allocated to Docker engine. | `dockur/macos needs 4 GB minimum; recommend 8 GB to the Docker engine.` |
| 10 | At least 80 GB free at the storage volume target. | `macOS install + Xcode CLT + Rust target dir easily exceeds 60 GB. Free up space.` |

**Ordering rationale:** checks 1 and 2 are the cheapest and most decisive — they answer "is this skill applicable at all?" without spawning a process, hitting the network, or pulling an image. Checks 3 and 4 reject the broad disqualifying configurations (ARM hosts, Windows 10). Only after those pass do we touch the Docker daemon (5, with launch-and-wait), inspect engine variant (6), and probe `/dev/kvm` with a throwaway alpine container (7).

## Docker launch + recovery (check 5 implementation detail)

If `docker info` fails on the first attempt, do not bail immediately — attempt to launch Docker and poll for readiness. Pseudocode:

```
fn ensure_docker_running(timeout_secs = 60) -> Result<()> {
    if docker_info_ok() { return Ok(()); }

    let last_err = docker_info_stderr();
    launch_docker_for_platform()?;          // platform-specific, non-blocking

    let deadline = now() + timeout_secs;
    while now() < deadline {
        if docker_info_ok() { return Ok(()); }
        sleep(2.seconds);
    }
    Err(format!("Docker did not become reachable in {timeout_secs}s. Last error: {last_err}"))
}
```

### Platform-specific launch commands

| Platform | Detection | Launch command | Notes |
|---|---|---|---|
| Linux + systemd | `systemctl --version` succeeds | `sudo systemctl start docker` | Most common modern path. Requires sudo. |
| Linux + SysV/OpenRC | `/etc/init.d/docker` exists, no systemd | `sudo service docker start` | Older distros, Alpine, some Devuan/Gentoo setups. |
| Linux, no init manager (containers, minimal WSL distros) | Neither above | `sudo dockerd > /tmp/dockerd.log 2>&1 &` | Last resort. Surface log path in the bail message. |
| Windows + Docker Desktop | `Get-Process "Docker Desktop"` returns nothing | `Start-Process "$env:ProgramFiles\Docker\Docker\Docker Desktop.exe"` | Non-blocking; Docker Desktop boots its WSL2/Hyper-V backend on its own schedule (typically 10–30 s). |
| Windows + Docker Desktop (newer CLI, 2024+) | `docker desktop --help` succeeds | `docker desktop start` | Preferred when available — no GUI flash, returns when the engine socket is ready. Detect by probing the subcommand first. |
| Windows + Docker engine inside WSL2 distro | The `OperatingSystem` field from `docker info` (or the absence of Docker Desktop process) indicates WSL native engine | `wsl -d <distro> -- sudo service docker start` from the Windows side, or `sudo service docker start` inside the WSL shell | The Win11-recommended path for `/dev/kvm` access — this skill will most often be invoked here. |

The skill chooses the launch command by reading host OS + the most-recently-cached Docker variant from probe-cache (or, on first run, probes for the existence of each detection signal in order).

### Workarounds when Docker fails to launch

These are the failure modes observed across the dockur and Docker-on-WSL2 issue trackers. Print these as a bulleted checklist in the bail message rather than auto-remediating (each carries side effects). Filter by the detected platform.

1. **Docker Desktop hung VM (Windows).** Symptom: GUI shows "Docker Desktop starting…" indefinitely; `docker info` returns "error during connect: open //./pipe/dockerDesktopLinuxEngine: The system cannot find the file specified." Fix: `wsl --shutdown` from PowerShell, then relaunch Docker Desktop. Nuclear option: Docker Desktop → Troubleshoot → Reset to factory defaults (wipes images/volumes).
2. **WSL2 utility VM is stuck (Windows).** Symptom: `wsl -l -v` shows the docker-desktop distro in `Stopped` or `Installing` state; `wsl --shutdown` followed by relaunch fixes most cases.
3. **Hyper-V service stopped (Windows Pro/Enterprise).** Symptom: Docker Desktop errors with "Hardware assisted virtualization and data execution protection must be enabled in the BIOS." Fix: `Get-Service vmms; Start-Service vmms` (run as admin). If service is missing entirely, enable Hyper-V feature: `Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -All` and reboot.
4. **User not in `docker` group (Linux).** Symptom: `docker info` returns "permission denied while trying to connect to the Docker daemon socket." Fix: `sudo usermod -aG docker $USER` then log out + back in. Detect this exact error string and surface the fix directly.
5. **Docker socket path mismatch (Linux + non-default install).** Symptom: `docker info` errors with "Cannot connect to the Docker daemon at unix:///var/run/docker.sock." Fix: `export DOCKER_HOST=unix:///path/to/actual.sock` or symlink. Respect `$DOCKER_HOST` if set.
6. **Antivirus/EDR blocking Docker Desktop's vmms or VPNKit (Windows).** Symptom: Docker Desktop logs show "vpnkit.exe terminated unexpectedly." Fix: add Docker install dir to AV exclusions. No autofix — surface as a checklist item.
7. **Out of disk on Docker VM image (Windows).** Symptom: Docker Desktop refuses to start; logs reference `ext4.vhdx`. Fix: free space on the host disk hosting `%LOCALAPPDATA%\Docker\wsl\`, or use Docker Desktop's "Clean / Purge data".
8. **VM corruption after a Windows feature update.** Symptom: Docker Desktop won't start after a Windows monthly update. Fix: Settings → Resources → WSL Integration → toggle off/on, then `wsl --shutdown` + relaunch.

If `ensure_docker_running` returns Err, print the 60 s timeout message, the captured last stderr, and the platform-filtered subset of the above checklist.

## Operating modes

Two modes, selected by the skill based on a sentinel volume:

| Mode | Trigger | Behavior |
|---|---|---|
| `bootstrap` | First run, or sentinel `~/.clud/docker-mac-x86/READY-<macos-ver>-<toolchain-hash>` absent, or `--rebuild` | Pull `dockurr/macos`, start container, expose web viewer on `:8006`, prompt user to drive macOS install + Xcode CLT + Homebrew + `rustup` (via [soldr](https://github.com/zackees/soldr)) in the browser. On `/clud-docker-mac-x86 setup-done`, snapshot the disk and write the sentinel. |
| `iterate` | Sentinel present | Restore snapshot, sync source into guest, run user command (`cargo test`, `cargo build`, etc.), stream stdout back with real exit code, restore-on-exit for the next run. |

## Host-compat table

| Host | Status | Notes |
|---|---|---|
| Linux x86_64, Docker engine, `/dev/kvm`, Intel CPU | Primary | Multi-core, fastest path |
| Linux x86_64, Docker engine, `/dev/kvm`, AMD CPU | Degraded | https://github.com/dockur/macos/issues/268 PCID mismatch — single-core only |
| Win11 22H2+, Docker engine in a WSL2 distro with `nestedVirtualization=true` | Secondary | Hardware-accelerated; setup is non-trivial; measured speed unknown |
| Win10/11 host running a Hyper-V Linux guest with `ExposeVirtualizationExtensions $true`, Docker engine in the guest | Tertiary | Hardware-accelerated nested SVM (not TCG); heavyweight one-time setup; measured speed unknown |
| Windows + Docker Desktop (any version) | Rejected | LinuxKit VM doesn't expose `/dev/kvm` |
| Windows 10 + lifted WSL2 | Rejected | https://github.com/microsoft/WSL/issues/40735 hardcodes the gate |
| macOS host (Intel or Apple Silicon) | Rejected | dockur compat chart marks macOS unsupported as host; Apple Silicon also has no arm64 image |
| Any Linux ARM host | Rejected | No arm64 image |
| GH Actions `ubuntu-24.04` runner | Investigate | Open question — may be a useful secondary CI mode |

## Exit Guidance

If the precondition probe bails at any step, print one diagnostic line + the relevant platform-filtered workaround checklist, then stop. Do not attempt to remediate environment problems automatically. If the probe passes and the user-supplied command runs to completion inside the guest, surface the guest's exit code as the skill's exit code so callers can chain on success/failure.

## Open Design Questions

These runtime-mode details are deliberately not pinned in this skill yet; the implementing PR for `bootstrap`/`iterate` should resolve them in design review before code lands. The applicability gate above does not depend on the answers.

1. **Source sync** — QEMU 9p vs virtfs vs Samba vs `rsync` over a side-loaded sshd; pick after measuring on a qualifying host.
2. **Snapshot mechanism** — `qemu-img snapshot` (container stopped) vs Docker volume snapshot via LVM/ZFS vs `docker commit`; pick on sub-minute restore target.
3. **Toolchain bake-in scope** — bake `xcode-select --install` + Homebrew + `soldr` + rustup into the post-bootstrap snapshot by default; offer `--minimal`.
4. **Sentinel cache key** — `(macOS version, rustup toolchain hash, baked-tools list)`; mismatch forces re-bootstrap.
5. **Output protocol** — likely launchd-autostart sshd on a known port; ssh into the guest from the host with the user's command.
6. **CI applicability** — investigate `ubuntu-24.04` GH Actions runner with KVM as a secondary smoke-test mode.
7. **Hyper-V workaround surface area** — bail with a doc link by default; do not script the multi-step Hyper-V Linux-guest setup from inside the skill.
