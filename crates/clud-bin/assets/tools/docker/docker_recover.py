#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# managed-by: clud
"""docker_recover.py — cross-platform Docker Desktop recovery + diagnostics.

Diagnoses a wedged Docker Desktop (engine pipe/socket absent while the
backend/UI stay alive — the failure mode from zackees/clud#531, whose root
cause was a killed `com.docker.build` child, NOT memory or disk pressure)
and drives a bounded, non-destructive recovery. It classifies the failure
(engine-unavailable / resource-pressure / storage-pressure) before acting,
polls readiness on a bounded schedule (10 attempts, 2s interval — the
FastLED WASM `8cf7f663` precedent), and verifies recovery against the
server API plus a minimal container run.

The single hard rule: this tool NEVER compacts, prunes, deletes, resets, or
otherwise mutates Docker storage on its own. `doctor` is strictly read-only.
Every restart/reset states plainly that containers stop but images and
volumes are preserved. Any VHD / `Docker.raw` / `data-root` remediation is
refused unless (a) the caller passes `--yes`, (b) exactly one storage
candidate is unambiguous, and (c) Docker/WSL is fully stopped — and even
then v0 only prints the vetted backup + compaction plan rather than running
it.

Windows storage resolver (per zackees/clud#531 follow-up comment):
`%LOCALAPPDATA%\\Docker\\wsl\\data\\docker_data.vhdx` is only the fallback
default — it is NOT authoritative. The resolver reads Docker Desktop's
`settings-store.json`, honours `CustomWslDistroDir` for the live WSL engine
disk, inspects `DataFolder` separately as a Hyper-V/legacy location (never
conflated with WSL storage), scores every candidate, and refuses any
destructive action while more than one candidate stays plausible.

Usage:

    clud tool run docker/docker_recover.py doctor
    clud tool run docker/docker_recover.py gc [--age-hours N] [--dry-run]
    clud tool run docker/docker_recover.py restart [--yes]
    clud tool run docker/docker_recover.py reset [--yes]
    clud tool run docker/docker_recover.py disk [--action compact|prune|delete|reset] \
        [--select <path>] [--yes]

Subcommands:
    doctor   Read-only report: client/server availability, engine error,
             host free memory + disk, Docker runtime processes, the resolved
             Docker data-disk path/size + confidence, and recent relevant
             logs. Mutates nothing (no restart, no disk write, no rotation).
    gc       Reclaim dangling Docker objects (unused images, stopped
             containers, anonymous unreferenced volumes) older than an age
             threshold (`trim` is an alias). Default-safe: NO confirmation
             gate — pruned objects are cheap to rebuild. Never touches
             running containers, images backing a running container, or named
             volumes. The lightest rung of the escalation ladder; more
             aggressive on the system/boot volume. Idempotent one-shot,
             suitable for periodic cron / Task Scheduler / `clud schedule`.
    restart  Restart the normal Docker runtime via a documented clean
             sequence. Containers stop; images/volumes are preserved.
             Refused without --yes. Bounded readiness wait, then verifies.
    reset    Platform runtime reset (`wsl --shutdown` + relaunch on Windows).
             Same --yes gate and preservation guarantees as restart.
    disk     Report Docker storage candidates (read-only by default). A
             mutating --action is refused unless the candidate is
             unambiguous AND --yes is given AND Docker is stopped; even then
             v0 prints the plan instead of executing it.

Exit codes:
    0   success — daemon healthy (doctor) or recovery verified (restart/reset)
    1   unhealthy — doctor found a blocking problem, or recovery failed with
        the original diagnosis preserved in the report
    2   usage error
    3   destructive action refused pending confirmation / precondition
        (needs --yes, or Docker/WSL still running)
    4   destructive action refused — storage candidate is ambiguous or
        unresolved; the user must select one before any disk action
    64  requested but deliberately not auto-executed in v0 (destructive disk
        mutation prints the vetted plan instead of running it)
"""

from __future__ import annotations

import argparse
import ctypes
import csv
import json
import ntpath
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import typing
from collections.abc import Mapping
from dataclasses import dataclass, field
from pathlib import Path

# --------------------------------------------------------------------------
# Exit codes — the public contract callers (SKILL.md, other tooling) rely on.
# --------------------------------------------------------------------------
EXIT_OK = 0
EXIT_UNHEALTHY = 1
EXIT_USAGE = 2
EXIT_REFUSED_CONFIRM = 3
EXIT_REFUSED_AMBIGUOUS = 4
EXIT_NOT_AUTO_EXECUTED = 64

# Bounded readiness polling — 10 attempts, 2s interval (issue #531; the
# FastLED WASM `8cf7f663` Windows Docker/WSL readiness-retry precedent).
READY_ATTEMPTS = 10
READY_INTERVAL_SECONDS = 2.0

# Windows process creation flags are only exported by subprocess on Windows.
# Keep their documented values available so the launcher contract remains
# unit-testable on Linux and macOS CI.
WINDOWS_DETACHED_PROCESS = getattr(subprocess, "DETACHED_PROCESS", 0x00000008)
WINDOWS_CREATE_NEW_PROCESS_GROUP = getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0x00000200)
DAEMON_ENV_VAR = "RUNNING_PROCESS_IS_DAEMON"
WINDOWS_DAEMON_GUARD_ARG = "__windows-daemon-guard"
WINDOWS_DAEMON_GUARD_MUTEX = r"Local\clud-docker-recover-daemon-guard"
WINDOWS_GUARD_PARENT_WAIT_SECONDS = 60.0
WINDOWS_GUARD_STARTUP_SECONDS = 300.0
WINDOWS_GUARD_CLI_SECONDS = 40.0
WINDOWS_GUARD_CLI_TERMINATE_SECONDS = 1.0
WINDOWS_GUARD_LOG_MAX_BYTES = 1024 * 1024
WINDOWS_GUARD_EMPTY_SAMPLES = 3
WINDOWS_GUARD_MONITOR_INTERVAL = 5.0
AUTHORITATIVE_WINDOWS_DOCKER_PROCESSES = {
    "docker desktop": "Docker Desktop",
    "com.docker.backend": "com.docker.backend",
    "com.docker.docker": "com.docker.docker",
}

# Advisory host-resource thresholds. Crossing them never blocks a healthy
# daemon — they surface as advisories, not failures.
LOW_DISK_BYTES = 2 * 1024**3
LOW_MEM_BYTES = 1 * 1024**3

# Garbage-collection default age threshold. GC reclaims *dangling* Docker
# objects (unused images, stopped containers, anonymous unreferenced
# volumes) — cheap to rebuild, so GC runs default-safe WITHOUT a
# confirmation gate, unlike VHD/raw-disk remediation. It is the lightest
# rung of the escalation ladder and is halved on the system/boot volume.
GC_DEFAULT_AGE_HOURS = 24.0

# The canonical WSL engine-disk filename Docker Desktop writes.
DOCKER_DATA_FILENAME = "docker_data.vhdx"

# Failure categories — classified before any action is taken.
CAT_HEALTHY = "healthy"
CAT_ENGINE_UNAVAILABLE = "engine-unavailable"
CAT_RESOURCE_PRESSURE = "resource-pressure"
CAT_STORAGE_PRESSURE = "storage-pressure"

# Storage kinds. WSL and Hyper-V/legacy locations are never conflated.
KIND_WSL = "wsl"
KIND_HYPERV_LEGACY = "hyperv-legacy"


# ==========================================================================
# Pure decision layer — no IO, unit-tested directly (see
# tests/test_docker_recover.py). The IO layer below feeds these dataclasses.
# ==========================================================================
@dataclass
class HealthSnapshot:
    """A read-only snapshot of the Docker host, gathered by the IO layer."""

    client_present: bool
    server_ok: bool
    engine_error: str | None = None
    free_mem_bytes: int | None = None
    free_disk_bytes: int | None = None
    runtime_processes: list[str] = field(default_factory=list)
    build_child_present: bool | None = None
    wsl_docker_distro_state: str | None = None


@dataclass
class HealthReport:
    healthy: bool
    category: str
    failures: list[str] = field(default_factory=list)
    advisories: list[str] = field(default_factory=list)


def classify_failure(snap: HealthSnapshot) -> str:
    """Classify the host state BEFORE acting.

    A reachable server is healthy regardless of low resources (those are
    advisory). When the server is unreachable, storage pressure and memory
    pressure are ruled out first; the residual — engine down while the host
    has resources to spare — is the incident case from #531 (a killed
    `com.docker.build` child, pipe absent while the UI stayed alive).
    """
    if snap.server_ok:
        return CAT_HEALTHY
    if snap.free_disk_bytes is not None and snap.free_disk_bytes < LOW_DISK_BYTES:
        return CAT_STORAGE_PRESSURE
    if snap.free_mem_bytes is not None and snap.free_mem_bytes < LOW_MEM_BYTES:
        return CAT_RESOURCE_PRESSURE
    return CAT_ENGINE_UNAVAILABLE


def assess_health(snap: HealthSnapshot) -> HealthReport:
    """Turn a snapshot into blocking failures + non-blocking advisories.

    Low free disk / memory are advisories: they never flip a reachable
    daemon to unhealthy (the #531 low-space-advisory acceptance criterion).
    """
    failures: list[str] = []
    advisories: list[str] = []

    if not snap.client_present:
        failures.append("docker CLI not found on PATH — Docker is not installed")
    if not snap.server_ok:
        err = snap.engine_error or "no server response"
        failures.append(f"docker engine unreachable: {err}")

    if snap.free_disk_bytes is not None and snap.free_disk_bytes < LOW_DISK_BYTES:
        advisories.append(
            f"low free disk: {_human_bytes(snap.free_disk_bytes)} "
            f"(< {_human_bytes(LOW_DISK_BYTES)} advisory threshold)"
        )
    if snap.free_mem_bytes is not None and snap.free_mem_bytes < LOW_MEM_BYTES:
        advisories.append(
            f"low free memory: {_human_bytes(snap.free_mem_bytes)} "
            f"(< {_human_bytes(LOW_MEM_BYTES)} advisory threshold)"
        )

    healthy = snap.client_present and snap.server_ok
    return HealthReport(
        healthy=healthy,
        category=classify_failure(snap),
        failures=failures,
        advisories=advisories,
    )


# ---- Windows storage resolver -------------------------------------------
@dataclass
class DiskCandidate:
    """One plausible Docker storage disk, with provenance + a score."""

    path: str
    resolved_path: str
    size_bytes: int | None
    kind: str
    source: str
    score: int
    signals: list[str] = field(default_factory=list)

    @property
    def confidence(self) -> str:
        if self.score >= 75:
            return "high"
        if self.score >= 40:
            return "medium"
        return "low"


@dataclass
class DiskResolution:
    candidates: list[DiskCandidate]
    chosen: DiskCandidate | None
    ambiguous: bool
    settings_present: bool
    settings_source: str | None
    used_fallback: bool
    notes: list[str] = field(default_factory=list)


class SystemDiskProbe:
    """Real filesystem probe surface the Windows resolver depends on.

    Split behind this narrow interface so tests inject canned data without
    touching a real Windows registry or filesystem (methods: read_text,
    exists, size_bytes, resolve_final, recent_write, glob_vhdx).
    """

    def read_text(self, path: str) -> str | None:
        try:
            return Path(path).read_text(encoding="utf-8")
        except OSError:
            return None

    def exists(self, path: str) -> bool:
        return Path(path).is_file()

    def size_bytes(self, path: str) -> int | None:
        try:
            return Path(path).stat().st_size
        except OSError:
            return None

    def resolve_final(self, path: str) -> str:
        """Resolve junctions/symlinks to the final on-disk path."""
        try:
            return str(Path(path).resolve())
        except OSError:
            return path

    def recent_write(self, path: str, within_hours: float = 24.0) -> bool:
        try:
            mtime = Path(path).stat().st_mtime
        except OSError:
            return False
        return (time.time() - mtime) <= within_hours * 3600.0

    def glob_vhdx(self, root: str) -> list[str]:
        """Constrained `*.vhdx` scan one level below `root` and its
        conventional `disk/` + `data/` subdirs — never a recursive
        user-profile walk."""
        out: list[str] = []
        for sub in ("", "disk", "data"):
            base = Path(root, sub) if sub else Path(root)
            try:
                out.extend(str(p) for p in base.glob("*.vhdx"))
            except OSError:
                continue
        return out


def _clean_str(value: object) -> str | None:
    if isinstance(value, str) and value.strip():
        return value.strip()
    return None


def read_docker_settings(
    appdata: str | None, probe: SystemDiskProbe
) -> tuple[dict | None, str | None]:
    """Read Docker Desktop settings, current name first then legacy.

    Returns (settings_dict, source_path) or (None, None) when nothing
    parseable is found.
    """
    if not appdata:
        return None, None
    for name in ("settings-store.json", "settings.json"):
        path = ntpath.join(appdata, "Docker", name)
        raw = probe.read_text(path)
        if raw is None:
            continue
        try:
            data = json.loads(raw)
        except (ValueError, TypeError):
            continue
        if isinstance(data, dict):
            return data, path
    return None, None


def _score_candidate(
    *,
    path: str,
    configured_parent: str | None,
    resolved_path: str,
    recent: bool,
) -> tuple[int, list[str]]:
    score = 0
    signals: list[str] = []
    if configured_parent is not None and resolved_path.lower().startswith(
        configured_parent.lower()
    ):
        score += 50
        signals.append("configured-parent-match")
    if ntpath.basename(path).lower() == DOCKER_DATA_FILENAME:
        score += 25
        signals.append("exact-docker-data-filename")
    # An existing (probed) candidate always has a resolved path on disk.
    score += 15
    signals.append("resolved-path-exists")
    if recent:
        score += 10
        signals.append("recent-docker-write")
    return score, signals


def _consider(
    out: list[DiskCandidate],
    probe: SystemDiskProbe,
    path: str,
    *,
    kind: str,
    source: str,
    configured_parent: str | None,
) -> None:
    if not probe.exists(path):
        return
    resolved = probe.resolve_final(path)
    if any(c.resolved_path.lower() == resolved.lower() for c in out):
        return
    score, signals = _score_candidate(
        path=path,
        configured_parent=configured_parent,
        resolved_path=resolved,
        recent=probe.recent_write(path),
    )
    out.append(
        DiskCandidate(
            path=path,
            resolved_path=resolved,
            size_bytes=probe.size_bytes(path),
            kind=kind,
            source=source,
            score=score,
            signals=signals,
        )
    )


def resolve_windows_docker_disks(
    settings: dict | None,
    probe: SystemDiskProbe,
    *,
    localappdata: str | None = None,
    wsl_distro_base: str | None = None,
) -> DiskResolution:
    """Resolve the live Docker storage disk(s) on Windows.

    Order (per #531 follow-up): configured `CustomWslDistroDir` first, then
    `DataFolder` as a SEPARATE Hyper-V/legacy location, and only if no
    configured WSL disk materialises do we fall back to a short explicit set
    of defaults (never a recursive profile scan). Ambiguity among WSL
    candidates always wins over action: `chosen` is set only when exactly
    one WSL candidate strictly out-scores the rest.
    """
    candidates: list[DiskCandidate] = []
    notes: list[str] = []
    settings_present = settings is not None
    custom_wsl = _clean_str(settings.get("CustomWslDistroDir")) if settings else None
    data_folder = _clean_str(settings.get("DataFolder")) if settings else None

    # 1. CustomWslDistroDir — the authoritative live WSL engine disk.
    if custom_wsl:
        root = probe.resolve_final(custom_wsl)
        notes.append(f"CustomWslDistroDir configured: {custom_wsl} -> {root}")
        for rel in (
            ntpath.join("disk", DOCKER_DATA_FILENAME),
            ntpath.join("data", DOCKER_DATA_FILENAME),
        ):
            _consider(
                candidates,
                probe,
                ntpath.join(root, rel),
                kind=KIND_WSL,
                source="CustomWslDistroDir",
                configured_parent=root,
            )
        for extra in probe.glob_vhdx(root):
            _consider(
                candidates,
                probe,
                extra,
                kind=KIND_WSL,
                source="CustomWslDistroDir(scan)",
                configured_parent=root,
            )

    # 2. DataFolder — Hyper-V / legacy VM layout ONLY. Never a WSL disk.
    if data_folder:
        root = probe.resolve_final(data_folder)
        notes.append(f"DataFolder configured (legacy/Hyper-V only): {data_folder} -> {root}")
        for rel in (
            "DockerDesktop.vhdx",
            ntpath.join("DockerDesktop", "DockerDesktop.vhdx"),
        ):
            _consider(
                candidates,
                probe,
                ntpath.join(root, rel),
                kind=KIND_HYPERV_LEGACY,
                source="DataFolder",
                configured_parent=root,
            )

    # 3. Fallback — only when settings are missing/stale OR no configured
    #    WSL disk materialised. Never mutate a default-path candidate merely
    #    because the configured lookup came up empty.
    have_configured_wsl = any(c.kind == KIND_WSL for c in candidates)
    used_fallback = (not settings_present) or (not have_configured_wsl)
    if used_fallback:
        notes.append("using explicit fallback default set (no configured WSL disk found)")
        fallback_roots: list[tuple[str, str]] = []
        if localappdata:
            fallback_roots.append((ntpath.join(localappdata, "Docker", "wsl"), KIND_WSL))
            fallback_roots.append((ntpath.join(localappdata, "DockerDesktop"), KIND_WSL))
        if wsl_distro_base:
            fallback_roots.append((wsl_distro_base, KIND_WSL))
        if data_folder:
            fallback_roots.append((data_folder, KIND_HYPERV_LEGACY))
        for root, kind in fallback_roots:
            rroot = probe.resolve_final(root)
            for rel in (
                ntpath.join("disk", DOCKER_DATA_FILENAME),
                ntpath.join("data", DOCKER_DATA_FILENAME),
                DOCKER_DATA_FILENAME,
                "ext4.vhdx",
            ):
                _consider(
                    candidates,
                    probe,
                    ntpath.join(rroot, rel),
                    kind=kind,
                    source="fallback",
                    configured_parent=None,
                )
            for extra in probe.glob_vhdx(rroot):
                _consider(
                    candidates,
                    probe,
                    extra,
                    kind=kind,
                    source="fallback(scan)",
                    configured_parent=None,
                )

    candidates.sort(key=lambda c: c.score, reverse=True)
    chosen, ambiguous = _pick_wsl_disk(candidates)
    return DiskResolution(
        candidates=candidates,
        chosen=chosen,
        ambiguous=ambiguous,
        settings_present=settings_present,
        settings_source=None,  # filled in by the caller that knows the path
        used_fallback=used_fallback,
        notes=notes,
    )


def _pick_wsl_disk(candidates: list[DiskCandidate]) -> tuple[DiskCandidate | None, bool]:
    """Choose the single unambiguous WSL engine disk, or refuse.

    Hyper-V/legacy candidates are never eligible as the WSL disk. Ambiguity
    (two WSL candidates tied at the top score) leaves `chosen` unset.
    """
    wsl = [c for c in candidates if c.kind == KIND_WSL]
    if not wsl:
        return None, False
    top = wsl[0].score
    tied = [c for c in wsl if c.score == top]
    if len(tied) == 1:
        return tied[0], False
    return None, True


def apply_selection(resolution: DiskResolution, select: str | None) -> DiskResolution:
    """Honour an explicit user disk selection, clearing ambiguity."""
    if not select:
        return resolution
    want = ntpath.normcase(ntpath.normpath(select))
    for cand in resolution.candidates:
        if ntpath.normcase(ntpath.normpath(cand.path)) == want or (
            ntpath.normcase(ntpath.normpath(cand.resolved_path)) == want
        ):
            resolution.chosen = cand
            resolution.ambiguous = False
            resolution.notes.append(f"user selected candidate: {cand.path}")
            return resolution
    resolution.notes.append(f"--select {select} matched no candidate; refusing")
    resolution.chosen = None
    return resolution


def disk_action_gate(
    resolution: DiskResolution,
    *,
    confirmed: bool,
    docker_stopped: bool,
) -> tuple[int, str]:
    """Gate a destructive storage action. Returns (exit_code, message).

    EXIT_OK means every gate passed and the caller may proceed. This
    function itself NEVER mutates anything — it only decides.
    """
    if resolution.chosen is None:
        return (
            EXIT_REFUSED_AMBIGUOUS,
            "refusing storage action: the active Docker disk is ambiguous or "
            "unresolved. Re-run `disk` to see candidates, then pass "
            "`--select <path>` to choose one.",
        )
    if not confirmed:
        return (
            EXIT_REFUSED_CONFIRM,
            "refusing storage action without --yes. Containers are unaffected, "
            "but backup/compaction/deletion are irreversible — pass --yes to "
            "confirm you have a backup and understand the impact.",
        )
    if not docker_stopped:
        return (
            EXIT_REFUSED_CONFIRM,
            "refusing storage action: Docker Desktop / WSL must be fully "
            "stopped first (run `reset` or `wsl --shutdown`). Back up "
            f"{resolution.chosen.path} before any compaction.",
        )
    return EXIT_OK, "gates passed"


# ---- Garbage collection (dangling Docker objects) ------------------------
@dataclass
class GcImage:
    id: str
    tags: list[str]
    created_epoch: float
    size_bytes: int
    in_use: bool

    @property
    def dangling(self) -> bool:
        return not self.tags or all(t.endswith(":<none>") or t == "<none>" for t in self.tags)


@dataclass
class GcContainer:
    id: str
    running: bool
    created_epoch: float
    size_bytes: int = 0


@dataclass
class GcVolume:
    name: str
    anonymous: bool
    in_use: bool
    size_bytes: int = 0


@dataclass
class GcInventory:
    images: list[GcImage] = field(default_factory=list)
    containers: list[GcContainer] = field(default_factory=list)
    volumes: list[GcVolume] = field(default_factory=list)


@dataclass
class GcPlan:
    images: list[GcImage]
    containers: list[GcContainer]
    volumes: list[GcVolume]
    age_hours: float
    on_system_volume: bool

    @property
    def reclaimable_bytes(self) -> int:
        return (
            sum(i.size_bytes for i in self.images)
            + sum(c.size_bytes for c in self.containers)
            + sum(v.size_bytes for v in self.volumes)
        )

    @property
    def is_empty(self) -> bool:
        return not (self.images or self.containers or self.volumes)


def _age_hours(now: float, epoch: float) -> float:
    return (now - epoch) / 3600.0


def gc_age_threshold_hours(
    on_system_volume: bool, base_hours: float = GC_DEFAULT_AGE_HOURS
) -> float:
    """GC is more aggressive on the system/boot volume (smaller, shared with
    the OS): reclaim younger objects there than on a dedicated data drive."""
    return base_hours / 2.0 if on_system_volume else base_hours


def is_system_volume(path: str | None, *, system_drive: str | None = None) -> bool:
    """True when `path` lives on the system/boot volume.

    Windows: compare the drive letter to %SystemDrive% (default C:). POSIX: a
    root-filesystem data-root (`/var/lib/docker`) is treated as the system
    volume. An unknown path is assumed to be the system volume — the
    conservative, more-aggressive choice for GC."""
    if not path:
        return True
    drive = ntpath.splitdrive(path)[0]
    if drive:
        sysdrive = (system_drive or "C:").rstrip("\\/").upper()
        return drive.rstrip("\\/").upper() == sysdrive
    return True  # POSIX root-fs data-root


def plan_gc(
    inventory: GcInventory,
    *,
    now: float,
    on_system_volume: bool,
    base_age_hours: float = GC_DEFAULT_AGE_HOURS,
) -> GcPlan:
    """Select reclaimable dangling objects. Pure — no IO.

    Never selects: running containers, images backing a running container
    (`in_use`), named volumes, in-use volumes, or anything below the
    (possibly tightened) age threshold. Volumes carry no age gate — Docker
    prunes anonymous unreferenced volumes regardless of age, and so do we.
    """
    age = gc_age_threshold_hours(on_system_volume, base_age_hours)
    images = [
        img
        for img in inventory.images
        if not img.in_use and _age_hours(now, img.created_epoch) >= age
    ]
    containers = [
        c
        for c in inventory.containers
        if not c.running and _age_hours(now, c.created_epoch) >= age
    ]
    volumes = [v for v in inventory.volumes if v.anonymous and not v.in_use]
    return GcPlan(images, containers, volumes, age, on_system_volume)


def recommended_remedy(report: HealthReport, *, disk_low: bool) -> list[str]:
    """Escalation ladder, lightest rung first: GC (reclaim dangling objects)
    precedes restart/reset, which precede gated VHD/disk remediation."""
    steps: list[str] = []
    if disk_low:
        steps.append("gc")
    if not report.healthy:
        steps.append("restart")
    if disk_low:
        steps.append("disk")
    return steps


# ---- Recovery plans (pure, testable text) --------------------------------
def windows_restart_plan() -> list[str]:
    return [
        "Stop orphaned Docker helper processes (com.docker.build, "
        "com.docker.backend) if wedged.",
        "Run `wsl --shutdown` to cycle the WSL2 utility VM.",
        "Relaunch Docker Desktop (`docker desktop start`, or start "
        '"Docker Desktop.exe").',
        "Poll `docker version` on a bounded schedule "
        f"({READY_ATTEMPTS} attempts, {READY_INTERVAL_SECONDS:g}s interval).",
        "Verify with `docker run --rm hello-world` and `docker buildx ls`.",
        "Containers will STOP during this sequence; images and volumes are "
        "PRESERVED.",
    ]


def macos_restart_plan() -> list[str]:
    return [
        "Quit Docker Desktop, then relaunch it "
        "(`open -a Docker`, or `docker desktop start`).",
        "Poll `docker version` on a bounded schedule "
        f"({READY_ATTEMPTS} attempts, {READY_INTERVAL_SECONDS:g}s interval).",
        "Verify with `docker run --rm hello-world`.",
        "Containers will STOP; images and volumes (Docker.raw) are PRESERVED.",
    ]


def linux_restart_plan() -> list[str]:
    return [
        "Restart the engine service (`sudo systemctl restart docker`, or "
        "`sudo service docker restart`).",
        "Poll `docker version` on a bounded schedule "
        f"({READY_ATTEMPTS} attempts, {READY_INTERVAL_SECONDS:g}s interval).",
        "Verify with `docker run --rm hello-world`.",
        "Containers will STOP; images and volumes (data-root, normally "
        "/var/lib/docker) are PRESERVED.",
    ]


def restart_plan_for(system: str) -> list[str]:
    if system == "Windows":
        return windows_restart_plan()
    if system == "Darwin":
        return macos_restart_plan()
    return linux_restart_plan()


# ==========================================================================
# IO layer — thin, monkeypatched wholesale in tests. Each function is a
# best-effort probe that degrades to a safe default rather than raising.
# ==========================================================================
def docker_cli_present() -> bool:
    return shutil.which("docker") is not None


def _run(
    cmd: list[str],
    timeout: float = 20.0,
    *,
    env: Mapping[str, str] | None = None,
) -> subprocess.CompletedProcess | None:
    try:
        return subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
            env=env,
        )
    except (OSError, subprocess.SubprocessError):
        return None


def docker_server_version() -> str | None:
    """Server version via the engine API, or None when unreachable."""
    r = _run(["docker", "version", "--format", "{{.Server.Version}}"])
    if r is None or r.returncode != 0:
        return None
    out = r.stdout.strip()
    return out or None


def docker_engine_error() -> str | None:
    r = _run(["docker", "version", "--format", "{{.Server.Version}}"])
    if r is None:
        return "docker CLI could not be executed"
    if r.returncode != 0:
        return (r.stderr or r.stdout).strip() or "unknown engine error"
    return None


def run_hello_world() -> tuple[bool, str]:
    r = _run(["docker", "run", "--rm", "hello-world"], timeout=120.0)
    if r is None:
        return False, "docker run could not be executed"
    if r.returncode != 0:
        return False, (r.stderr or r.stdout).strip()
    return True, "hello-world container ran successfully"


def host_free_disk(path: str | None = None) -> int | None:
    try:
        return shutil.disk_usage(path or os.getcwd()).free
    except OSError:
        return None


def host_free_memory() -> int | None:
    system = platform.system()
    try:
        if system == "Linux":
            return _linux_free_mem()
        if system == "Windows":
            return _windows_free_mem()
        if system == "Darwin":
            return _macos_free_mem()
    except (OSError, ValueError, subprocess.SubprocessError):
        return None
    return None


def _linux_free_mem() -> int | None:
    for line in Path("/proc/meminfo").read_text(encoding="utf-8").splitlines():
        if line.startswith("MemAvailable:"):
            return int(line.split()[1]) * 1024
    return None


def _windows_free_mem() -> int | None:
    import ctypes

    class _MemStatus(ctypes.Structure):
        _fields_ = [
            ("dwLength", ctypes.c_ulong),
            ("dwMemoryLoad", ctypes.c_ulong),
            ("ullTotalPhys", ctypes.c_ulonglong),
            ("ullAvailPhys", ctypes.c_ulonglong),
            ("ullTotalPageFile", ctypes.c_ulonglong),
            ("ullAvailPageFile", ctypes.c_ulonglong),
            ("ullTotalVirtual", ctypes.c_ulonglong),
            ("ullAvailVirtual", ctypes.c_ulonglong),
            ("ullAvailExtendedVirtual", ctypes.c_ulonglong),
        ]

    stat = _MemStatus()
    stat.dwLength = ctypes.sizeof(_MemStatus)
    kernel32 = ctypes.windll.kernel32  # type: ignore[attr-defined]
    if not kernel32.GlobalMemoryStatusEx(ctypes.byref(stat)):
        return None
    return int(stat.ullAvailPhys)


def _macos_free_mem() -> int | None:
    r = _run(["vm_stat"])
    if r is None or r.returncode != 0:
        return None
    page_size = 4096
    free_pages = 0
    for line in r.stdout.splitlines():
        if "page size of" in line:
            page_size = int("".join(ch for ch in line if ch.isdigit()) or page_size)
        elif line.startswith(("Pages free:", "Pages inactive:", "Pages speculative:")):
            free_pages += int(line.rstrip(".").split()[-1])
    return free_pages * page_size if free_pages else None


def list_docker_processes() -> list[str]:
    names = (
        "Docker Desktop",
        "com.docker.backend",
        "com.docker.build",
        "dockerd",
        "com.docker.docker",
    )
    found: list[str] = []
    system = platform.system()
    if system == "Windows":
        r = _run(["tasklist"])
    else:
        r = _run(["ps", "-A", "-o", "comm"])
    if r is None or r.returncode != 0:
        return found
    haystack = r.stdout.lower()
    for name in names:
        if name.lower() in haystack:
            found.append(name)
    return found


def _parse_windows_docker_process_identities(text: str) -> set[tuple[str, int]]:
    identities: set[tuple[str, int]] = set()
    for row in csv.reader(text.splitlines()):
        if len(row) < 2:
            continue
        image = ntpath.splitext(row[0])[0].lower()
        name = AUTHORITATIVE_WINDOWS_DOCKER_PROCESSES.get(image)
        if name is None:
            continue
        try:
            pid = int(row[1].replace(",", ""))
        except ValueError:
            continue
        identities.add((name, pid))
    return identities


def _windows_docker_process_identities(
    *, timeout: float = 20.0
) -> set[tuple[str, int]]:
    result = _run(["tasklist", "/FO", "CSV", "/NH"], timeout=timeout)
    if result is None or result.returncode != 0:
        return set()
    return _parse_windows_docker_process_identities(result.stdout)


def wsl_status() -> str | None:
    r = _run(["wsl", "--status"])
    return r.stdout.strip() if r and r.returncode == 0 else None


def wsl_list_verbose() -> str | None:
    r = _run(["wsl", "--list", "--verbose"])
    return r.stdout if r and r.returncode == 0 else None


def wsl_docker_distro_state(listing: str | None) -> str | None:
    """Parse `wsl -l -v` output for the docker-desktop distro's state."""
    if not listing:
        return None
    # wsl -l -v emits UTF-16; when captured as text it can carry NULs.
    for raw in listing.replace("\x00", "").splitlines():
        low = raw.lower()
        if "docker-desktop" in low:
            for token in ("running", "stopped", "installing"):
                if token in low:
                    return token.capitalize()
    return None


def gather_snapshot() -> HealthSnapshot:
    client = docker_cli_present()
    server = docker_server_version() if client else None
    processes = list_docker_processes()
    listing = wsl_list_verbose() if platform.system() == "Windows" else None
    return HealthSnapshot(
        client_present=client,
        server_ok=server is not None,
        engine_error=None if server else (docker_engine_error() if client else "no docker CLI"),
        free_mem_bytes=host_free_memory(),
        free_disk_bytes=host_free_disk(),
        runtime_processes=processes,
        build_child_present=("com.docker.build" in processes) if processes else None,
        wsl_docker_distro_state=wsl_docker_distro_state(listing),
    )


def wait_for_docker(
    check=docker_server_version,
    *,
    attempts: int = READY_ATTEMPTS,
    interval: float = READY_INTERVAL_SECONDS,
    sleep=time.sleep,
    out=None,
) -> bool:
    """Bounded readiness poll. Returns True once `check()` is truthy."""
    write = (out or sys.stderr).write
    for i in range(attempts):
        if check():
            return True
        write(f"  readiness attempt {i + 1}/{attempts}: engine not ready\n")
        if i < attempts - 1:
            sleep(interval)
    return False


def verify_recovery() -> tuple[bool, list[str]]:
    """Confirm recovery via the server API AND a minimal container run."""
    details: list[str] = []
    version = docker_server_version()
    if version is None:
        return False, ["docker server API still unreachable"]
    details.append(f"docker server API reachable: v{version}")
    ok, msg = run_hello_world()
    details.append(msg)
    return ok, details


# ---- Garbage-collection IO -----------------------------------------------
@dataclass
class GcResult:
    images_removed: int = 0
    containers_removed: int = 0
    volumes_removed: int = 0
    freed_bytes: int = 0


def _parse_docker_size(text: str) -> int:
    """Parse a Docker human size ('1.22GB', '512MB', '0B') into bytes."""
    units = {
        "B": 1,
        "KB": 10**3,
        "MB": 10**6,
        "GB": 10**9,
        "TB": 10**12,
        "KIB": 1024,
        "MIB": 1024**2,
        "GIB": 1024**3,
        "TIB": 1024**4,
    }
    s = text.strip().upper()
    num = ""
    for ch in s:
        if ch.isdigit() or ch == ".":
            num += ch
        else:
            break
    unit = s[len(num) :].strip() or "B"
    try:
        value = float(num) if num else 0.0
    except ValueError:
        return 0
    return int(value * units.get(unit, 1))


def _parse_docker_time(text: str) -> float:
    """Parse a Docker `CreatedAt` timestamp to epoch seconds.

    Unparseable input returns `now` (treated as fresh, so it is kept — the
    conservative choice for a default-safe GC)."""
    from datetime import datetime

    tokens = text.strip().split()
    candidate = " ".join(tokens[:3]) if len(tokens) >= 3 else text.strip()
    for fmt in ("%Y-%m-%d %H:%M:%S %z", "%Y-%m-%dT%H:%M:%SZ", "%Y-%m-%d %H:%M:%S"):
        try:
            return datetime.strptime(candidate, fmt).timestamp()
        except ValueError:
            continue
    return time.time()


def _list_running_image_refs() -> set[str]:
    refs: set[str] = set()
    r = _run(["docker", "ps", "--format", "{{.Image}}\t{{.ImageID}}"])
    if r is None or r.returncode != 0:
        return refs
    for line in r.stdout.splitlines():
        for part in line.split("\t"):
            if part.strip():
                refs.add(part.strip())
    return refs


def _list_images() -> list[GcImage]:
    running = _list_running_image_refs()
    r = _run(
        [
            "docker",
            "image",
            "ls",
            "--all",
            "--no-trunc",
            "--format",
            "{{.ID}}\t{{.Repository}}:{{.Tag}}\t{{.CreatedAt}}\t{{.Size}}",
        ]
    )
    out: list[GcImage] = []
    if r is None or r.returncode != 0:
        return out
    for line in r.stdout.splitlines():
        parts = line.split("\t")
        if len(parts) < 4:
            continue
        iid, ref, created, size = parts[0], parts[1], parts[2], parts[3]
        out.append(
            GcImage(
                id=iid,
                tags=[] if ref in ("<none>:<none>", ":") else [ref],
                created_epoch=_parse_docker_time(created),
                size_bytes=_parse_docker_size(size),
                in_use=(iid in running or ref in running),
            )
        )
    return out


def _list_containers() -> list[GcContainer]:
    r = _run(
        [
            "docker",
            "ps",
            "--all",
            "--no-trunc",
            "--format",
            "{{.ID}}\t{{.State}}\t{{.CreatedAt}}",
        ]
    )
    out: list[GcContainer] = []
    if r is None or r.returncode != 0:
        return out
    for line in r.stdout.splitlines():
        parts = line.split("\t")
        if len(parts) < 3:
            continue
        cid, state, created = parts[0], parts[1], parts[2]
        out.append(
            GcContainer(
                id=cid,
                running=(state.strip().lower() == "running"),
                created_epoch=_parse_docker_time(created),
            )
        )
    return out


def _looks_anonymous(name: str) -> bool:
    return len(name) == 64 and all(c in "0123456789abcdef" for c in name.lower())


def _list_volumes() -> list[GcVolume]:
    unreferenced: set[str] = set()
    rd = _run(["docker", "volume", "ls", "-f", "dangling=true", "-q"])
    if rd and rd.returncode == 0:
        unreferenced = {x.strip() for x in rd.stdout.splitlines() if x.strip()}
    r = _run(["docker", "volume", "ls", "--format", "{{.Name}}\t{{.Labels}}"])
    out: list[GcVolume] = []
    if r is None or r.returncode != 0:
        return out
    for line in r.stdout.splitlines():
        parts = line.split("\t")
        name = parts[0].strip()
        labels = parts[1] if len(parts) > 1 else ""
        if not name:
            continue
        anon = "com.docker.volume.anonymous" in labels or _looks_anonymous(name)
        out.append(GcVolume(name=name, anonymous=anon, in_use=name not in unreferenced))
    return out


def gather_gc_inventory() -> GcInventory:
    return GcInventory(
        images=_list_images(),
        containers=_list_containers(),
        volumes=_list_volumes(),
    )


def _data_disk_on_system_volume() -> bool:
    if platform.system() == "Windows":
        resolution = _windows_resolution()
        path = resolution.chosen.path if resolution.chosen else None
        return is_system_volume(path, system_drive=os.environ.get("SystemDrive", "C:"))
    return True  # macOS/Linux data-root typically lives on the system volume


def execute_gc(plan: GcPlan) -> GcResult:
    """Remove the planned dangling objects. Never touches named volumes,
    running containers, or images backing a running container — the plan
    already excludes those."""
    result = GcResult()
    for img in plan.images:
        r = _run(["docker", "image", "rm", img.id])
        if r is not None and r.returncode == 0:
            result.images_removed += 1
            result.freed_bytes += img.size_bytes
    for c in plan.containers:
        r = _run(["docker", "rm", c.id])
        if r is not None and r.returncode == 0:
            result.containers_removed += 1
            result.freed_bytes += c.size_bytes
    for v in plan.volumes:
        r = _run(["docker", "volume", "rm", v.name])
        if r is not None and r.returncode == 0:
            result.volumes_removed += 1
            result.freed_bytes += v.size_bytes
    return result


# ==========================================================================
# Presentation helpers.
# ==========================================================================
def _human_bytes(n: int | None) -> str:
    if n is None:
        return "unknown"
    step = 1024.0
    value = float(n)
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if value < step:
            return f"{value:.1f} {unit}"
        value /= step
    return f"{value:.1f} PiB"


def _print_report_header(out, title: str) -> None:
    out.write(f"\n=== {title} ===\n")


def _print_resolution(out, resolution: DiskResolution) -> None:
    out.write("Docker storage resolution:\n")
    src = f" ({resolution.settings_source})" if resolution.settings_source else ""
    out.write(
        f"  settings: {'present' if resolution.settings_present else 'absent'}{src}; "
        f"fallback-used={resolution.used_fallback}\n"
    )
    for note in resolution.notes:
        out.write(f"  note: {note}\n")
    if not resolution.candidates:
        out.write("  no storage candidates found\n")
    for cand in resolution.candidates:
        marker = "*" if cand is resolution.chosen else "-"
        out.write(
            f"  {marker} [{cand.kind}] {cand.path} "
            f"size={_human_bytes(cand.size_bytes)} "
            f"confidence={cand.confidence} score={cand.score} "
            f"({', '.join(cand.signals)}) via {cand.source}\n"
        )
    if resolution.chosen is not None:
        out.write(f"  active WSL disk: {resolution.chosen.path}\n")
    elif resolution.ambiguous:
        out.write("  active WSL disk: AMBIGUOUS — refusing any storage action\n")


def _windows_resolution() -> DiskResolution:
    probe = SystemDiskProbe()
    settings, source = read_docker_settings(os.environ.get("APPDATA"), probe)
    resolution = resolve_windows_docker_disks(
        settings,
        probe,
        localappdata=os.environ.get("LOCALAPPDATA"),
    )
    resolution.settings_source = source
    return resolution


# ==========================================================================
# Subcommands.
# ==========================================================================
def cmd_doctor(_args: argparse.Namespace) -> int:
    """Strictly read-only. Mutates nothing — no restart, no disk write."""
    out = sys.stdout
    snap = gather_snapshot()
    report = assess_health(snap)
    system = platform.system()

    _print_report_header(out, "docker doctor (read-only)")
    out.write(f"platform: {system}\n")
    out.write(f"docker CLI present: {snap.client_present}\n")
    out.write(f"docker server reachable: {snap.server_ok}\n")
    if snap.engine_error:
        out.write(f"engine error: {snap.engine_error}\n")
    out.write(f"host free memory: {_human_bytes(snap.free_mem_bytes)}\n")
    out.write(f"host free disk: {_human_bytes(snap.free_disk_bytes)}\n")
    out.write(f"docker runtime processes: {', '.join(snap.runtime_processes) or 'none detected'}\n")
    out.write(f"classification: {report.category}\n")

    if system == "Windows":
        status = wsl_status()
        out.write(f"wsl --status: {'ok' if status else 'unavailable'}\n")
        out.write(f"docker-desktop distro state: {snap.wsl_docker_distro_state or 'unknown'}\n")
        _print_resolution(out, _windows_resolution())
    elif system == "Darwin":
        out.write(
            "macOS storage: Docker.raw default at "
            "~/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw "
            "(query Docker Desktop settings for a relocated disk before acting)\n"
        )
    else:
        out.write(
            "linux storage: data-root normally /var/lib/docker "
            "(confirm with `docker info -f '{{.DockerRootDir}}'` before acting)\n"
        )

    for advisory in report.advisories:
        out.write(f"ADVISORY: {advisory}\n")

    disk_low = snap.free_disk_bytes is not None and snap.free_disk_bytes < LOW_DISK_BYTES
    remedy = recommended_remedy(report, disk_low=disk_low)
    if remedy:
        out.write(f"recommended remedy (lightest first): {' -> '.join(remedy)}\n")
        if "gc" in remedy:
            out.write(
                "  gc is the lightest rung — reclaim dangling images/containers/"
                "anonymous volumes first (safe, reversible) before restart/reset "
                "or VHD remediation.\n"
            )

    if report.healthy:
        out.write("\ndoctor: healthy\n")
        return EXIT_OK
    sys.stderr.write("\nDOCTOR FOUND PROBLEMS:\n")
    for failure in report.failures:
        sys.stderr.write(f"  - {failure}\n")
    sys.stderr.write(f"  category: {report.category}\n")
    return EXIT_UNHEALTHY


def _run_recovery(args: argparse.Namespace, *, label: str) -> int:
    out = sys.stdout
    system = platform.system()
    snap = gather_snapshot()
    report = assess_health(snap)
    diagnosis = f"category={report.category}; " + (
        "engine reachable" if snap.server_ok else (snap.engine_error or "engine unreachable")
    )

    _print_report_header(out, f"docker {label}")
    out.write(f"initial diagnosis: {diagnosis}\n")

    if report.healthy and not getattr(args, "force", False):
        out.write("daemon already healthy — nothing to do (pass --force to restart anyway)\n")
        return EXIT_OK

    plan = restart_plan_for(system)
    out.write(f"planned {label} sequence:\n")
    for step in plan:
        out.write(f"  - {step}\n")

    if not args.yes:
        out.write(
            f"\nrefusing to {label} without --yes. Re-run with --yes to proceed. "
            "Containers will stop; images and volumes are preserved.\n"
        )
        return EXIT_REFUSED_CONFIRM

    out.write(f"\nexecuting {label} (containers will stop; images/volumes preserved)...\n")
    for detail in _execute_restart(system, hard=(label == "reset")):
        out.write(f"  launch: {detail}\n")
    ready = wait_for_docker()
    if not ready:
        sys.stderr.write(f"{label} FAILED: engine not ready after bounded wait\n")
        sys.stderr.write(f"  preserved diagnosis: {diagnosis}\n")
        return EXIT_UNHEALTHY

    ok, details = verify_recovery()
    for detail in details:
        out.write(f"  verify: {detail}\n")
    if ok:
        out.write(f"{label}: recovery verified\n")
        return EXIT_OK
    sys.stderr.write(f"{label} FAILED: verification did not pass\n")
    sys.stderr.write(f"  preserved diagnosis: {diagnosis}\n")
    return EXIT_UNHEALTHY


def _command_result_detail(label: str, result: subprocess.CompletedProcess | None) -> str:
    if result is None:
        return f"{label}: command could not be executed"
    output = (result.stderr or result.stdout).strip().replace("\r", " ").replace("\n", " ")
    suffix = f": {output}" if output else ""
    return f"{label}: exit {result.returncode}{suffix}"


def _windows_docker_desktop_executable(
    *, env: Mapping[str, str] | None = None, exists=os.path.isfile
) -> str | None:
    """Resolve Docker Desktop's executable without depending on PATH."""
    values = os.environ if env is None else env
    roots: list[str] = []
    for key in ("ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"):
        value = values.get(key)
        if value and ntpath.normcase(value) not in {ntpath.normcase(root) for root in roots}:
            roots.append(value)

    candidates = [
        ntpath.join(root, "Docker", "Docker", "Docker Desktop.exe") for root in roots
    ]
    localappdata = values.get("LOCALAPPDATA")
    if localappdata:
        candidates.append(ntpath.join(localappdata, "Docker", "Docker Desktop.exe"))

    return next((candidate for candidate in candidates if exists(candidate)), None)


def _declared_daemon_environment() -> dict[str, str]:
    child_env = os.environ.copy()
    child_env[DAEMON_ENV_VAR] = "1"
    return child_env


def _launch_windows_docker_desktop(executable: str) -> tuple[bool, str]:
    """Launch Desktop independently and positively declare it as a daemon."""
    flags = WINDOWS_DETACHED_PROCESS | WINDOWS_CREATE_NEW_PROCESS_GROUP
    try:
        subprocess.Popen(
            [executable],
            cwd=ntpath.dirname(executable),
            env=_declared_daemon_environment(),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            close_fds=True,
            creationflags=flags,
        )
    except OSError as exc:
        return False, f"direct Docker Desktop launch failed for {executable}: {exc}"
    return True, f"direct Docker Desktop launch started {executable} (daemon marker set)"


def _windows_docker_desktop_log_path(log_dir: Path | None = None) -> Path:
    """Return the durable, bounded log location for the guarded CLI launch."""
    directory = log_dir or Path(tempfile.gettempdir(), "clud", "docker-recover")
    return directory / "docker-desktop-start.log"


def _launch_windows_docker_desktop_guard() -> tuple[bool, str]:
    """Start a declared-daemon parent that owns Docker's CLI launch subtree."""
    flags = WINDOWS_DETACHED_PROCESS | WINDOWS_CREATE_NEW_PROCESS_GROUP
    script = os.path.abspath(__file__)
    try:
        subprocess.Popen(
            [sys.executable, script, WINDOWS_DAEMON_GUARD_ARG],
            cwd=os.path.dirname(script),
            env=_declared_daemon_environment(),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            close_fds=True,
            creationflags=flags,
        )
    except OSError as exc:
        return False, f"Docker Desktop daemon guard launch failed: {exc}"
    return (
        True,
        "Docker Desktop daemon guard started (daemon marker set; CLI diagnostics: "
        f"{_windows_docker_desktop_log_path()})",
    )


def _write_bounded_diagnostic_log(stream, path: Path) -> None:
    """Drain one stream incrementally while retaining only its newest 1 MiB.

    The reader is deliberately a daemon thread: Docker descendants can retain
    the inherited pipe after the `running-process` supervisor exits, so joining
    it indefinitely would recreate the inherited-handle wedge this guard avoids.
    """
    retained = 0
    try:
        with path.open("wb") as log:
            while chunk := stream.read(8192):
                if len(chunk) > WINDOWS_GUARD_LOG_MAX_BYTES:
                    chunk = chunk[-WINDOWS_GUARD_LOG_MAX_BYTES :]
                if retained + len(chunk) > WINDOWS_GUARD_LOG_MAX_BYTES:
                    log.seek(0)
                    log.truncate()
                    retained = 0
                log.write(chunk)
                log.flush()
                retained += len(chunk)
    except OSError:
        # Diagnostics must not alter the recovery path. The supervisor still
        # owns the launch tree and the caller retains its hard wall-clock bound.
        return


def _run_windows_desktop_cli(
    *, timeout: float, log_dir: Path | None = None
) -> subprocess.CompletedProcess | None:
    """Run Desktop's CLI through running-process with streamed diagnostics.

    `running-process --wall-clock-timeout` owns the required CLI-phase
    deadline and tree cleanup on every platform. Its merged output is drained
    incrementally to a bounded durable log.
    """
    command = ["docker", "desktop", "start"]
    log_path = _windows_docker_desktop_log_path(log_dir)
    log_directory = log_path.parent
    try:
        log_directory.mkdir(parents=True, exist_ok=True)
        # Fail before launch rather than silently discarding the only useful
        # startup diagnostics when the detached guard cannot create its log.
        log_path.touch(exist_ok=True)
    except OSError:
        return None

    supervised_command = [
        "running-process",
        "--no-auto-stack-dumping",
        "--wall-clock-timeout",
        str(timeout),
        "--",
        *command,
    ]
    try:
        process = subprocess.Popen(
            supervised_command,
            env=_declared_daemon_environment(),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            close_fds=True,
            creationflags=WINDOWS_CREATE_NEW_PROCESS_GROUP,
        )
    except OSError:
        return None

    stream = getattr(process, "stdout", None)
    reader = None
    if stream is not None:
        reader = threading.Thread(
            target=_write_bounded_diagnostic_log,
            args=(stream, log_path),
            name="clud-docker-desktop-output",
            daemon=True,
        )
        reader.start()
    try:
        returncode = process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        # The blessed supervisor normally exits after its own wall-clock tree
        # cleanup. This is only a defensive wrapper cleanup, never a platform
        # command or a direct Docker-tree fallback.
        try:
            process.kill()
            process.wait(timeout=WINDOWS_GUARD_CLI_TERMINATE_SECONDS)
        except (OSError, subprocess.TimeoutExpired):
            pass
        return None
    except OSError:
        return None
    finally:
        if reader is not None:
            # Give ordinary short-lived CLI output a chance to reach the log,
            # without ever waiting on a descendant-held pipe handle.
            reader.join(timeout=0.1)
    return subprocess.CompletedProcess(command, returncode, stdout="", stderr="")


def _acquire_windows_guard_mutex() -> int | object | None:
    """Acquire the per-session singleton mutex held for the guard's lifetime."""
    if platform.system() != "Windows":
        return object()
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CreateMutexW.argtypes = [ctypes.c_void_p, ctypes.c_bool, ctypes.c_wchar_p]
    kernel32.CreateMutexW.restype = ctypes.c_void_p
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
    kernel32.CloseHandle.restype = ctypes.c_bool
    handle = kernel32.CreateMutexW(None, False, WINDOWS_DAEMON_GUARD_MUTEX)
    if not handle:
        return None
    if ctypes.get_last_error() == 183:  # ERROR_ALREADY_EXISTS
        kernel32.CloseHandle(handle)
        return None
    return int(handle)


def _release_windows_guard_mutex(handle: int | object) -> None:
    if platform.system() != "Windows" or not isinstance(handle, int):
        return
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
    kernel32.CloseHandle.restype = ctypes.c_bool
    kernel32.CloseHandle(handle)


def _windows_server_ready(*, timeout: float = 1.0) -> bool:
    result = _run(
        ["docker", "version", "--format", "{{.Server.Version}}"],
        timeout=timeout,
    )
    return bool(result is not None and result.returncode == 0 and result.stdout.strip())


def _wait_for_windows_server_deadline(
    *,
    seconds: float = WINDOWS_GUARD_PARENT_WAIT_SECONDS,
    interval: float = 2.0,
    check=_windows_server_ready,
    now=time.monotonic,
    sleep=time.sleep,
) -> bool:
    """Wait for the engine with a real wall-clock deadline."""
    deadline = now() + seconds
    while True:
        remaining = deadline - now()
        if remaining <= 0:
            return False
        if check(timeout=min(1.0, remaining)):
            return True
        remaining = deadline - now()
        if remaining <= 0:
            return False
        sleep(min(interval, remaining))


def _windows_desktop_launch_observation(
    baseline: set[tuple[str, int]],
    *,
    source: str = "CLI",
) -> str | None:
    if docker_server_version() is not None:
        return f"Docker server API observed after {source} launch"
    new_processes = sorted(_windows_docker_process_identities() - baseline)
    if new_processes:
        rendered = ", ".join(f"{name} (PID {pid})" for name, pid in new_processes)
        return f"new Docker runtime process observed after {source} launch: " + rendered
    return None


def _windows_daemon_guard_main() -> int:
    """Own Docker's launch tree until Desktop exits, then retire quietly."""
    mutex = _acquire_windows_guard_mutex()
    if mutex is None:
        return EXIT_OK
    try:
        deadline = time.monotonic() + WINDOWS_GUARD_STARTUP_SECONDS
        baseline = _windows_docker_process_identities(timeout=1.0)
        _run_windows_desktop_cli(timeout=WINDOWS_GUARD_CLI_SECONDS)
        ready = _windows_server_ready(timeout=1.0)
        new_process = bool(
            _windows_docker_process_identities(timeout=1.0) - baseline
        )
        if not ready and not new_process:
            executable = _windows_docker_desktop_executable()
            if executable is not None:
                _launch_windows_docker_desktop(executable)

        while not ready and time.monotonic() < deadline:
            remaining = deadline - time.monotonic()
            ready = _windows_server_ready(timeout=min(1.0, max(remaining, 0.01)))
            if not ready and remaining > 0:
                time.sleep(min(2.0, remaining))
        if not ready:
            return EXIT_UNHEALTHY

        # Require sustained absence before retiring so a transient API/process
        # handoff cannot remove the positive daemon boundary.
        empty_samples = 0
        while empty_samples < WINDOWS_GUARD_EMPTY_SAMPLES:
            api_ready = _windows_server_ready(timeout=1.0)
            processes = _windows_docker_process_identities(timeout=1.0)
            empty_samples = 0 if api_ready or processes else empty_samples + 1
            if empty_samples < WINDOWS_GUARD_EMPTY_SAMPLES:
                time.sleep(WINDOWS_GUARD_MONITOR_INTERVAL)
        return EXIT_OK
    finally:
        _release_windows_guard_mutex(mutex)


def _wait_for_windows_desktop_launch(
    check,
    *,
    attempts: int = 3,
    interval: float = 1.0,
    sleep=time.sleep,
) -> str | None:
    """Briefly observe an asynchronous CLI launch before using the fallback."""
    for index in range(attempts):
        observation = check()
        if observation is not None:
            return observation
        if index < attempts - 1:
            sleep(interval)
    return None


# ---- WslService STOP_PENDING recovery (issue #632) -----------------------
#
# `reset --yes` cannot recover Docker when WSL itself is wedged: `WslService`
# sits in STOP_PENDING indefinitely (WAIT_HINT=0, CHECKPOINT=0), and both
# `wsl --shutdown` and `wsl -l -v` hang until their timeouts. Recovery needs one
# UAC-elevated, tightly scoped operation — force-terminate the exact
# SCM-reported wslservice.exe PID, then start the service.
#
# That is a privileged kill, so every check here is **fail-closed**: anything
# unexpected refuses rather than guesses. The decision is a pure function over
# an injected snapshot so all six states in the acceptance list are unit-tested
# without a wedged WSL, which is not something CI can produce on demand.

#: The only service state this recovery is allowed to act on.
WSL_SERVICE = "WslService"
WSL_SERVICE_IMAGE = "wslservice.exe"


class ServiceStatus(typing.NamedTuple):
    """One SCM observation of a service."""

    state: str | None
    pid: int | None


def parse_sc_queryex(text: str) -> ServiceStatus:
    """Extract STATE and PID from `sc queryex <service>` output.

    Fail-closed: anything unrecognized yields `None` fields, which the gate
    then refuses on. `sc`'s output is localized on non-English Windows, so the
    *numeric* state code is preferred over the English name when present —
    `STATE : 3 STOP_PENDING` parses from the 3, not from the word.
    """
    state: str | None = None
    pid: int | None = None
    for raw in text.splitlines():
        line = raw.strip()
        if not line:
            continue
        key, sep, value = line.partition(":")
        if not sep:
            continue
        key = key.strip().upper()
        value = value.strip()
        if key == "STATE":
            parts = value.split()
            if parts and parts[0].isdigit():
                state = _SC_STATE_CODES.get(int(parts[0]))
                if state is None and len(parts) > 1:
                    state = parts[1].upper()
            elif parts:
                state = parts[0].upper()
        elif key == "PID":
            try:
                pid = int(value.split()[0])
            except (ValueError, IndexError):
                pid = None
    return ServiceStatus(state=state, pid=pid)


#: SCM numeric state codes. Numeric so a localized Windows still parses.
_SC_STATE_CODES = {
    1: "STOPPED",
    2: "START_PENDING",
    3: "STOP_PENDING",
    4: "RUNNING",
    5: "CONTINUE_PENDING",
    6: "PAUSE_PENDING",
    7: "PAUSED",
}


def parse_sc_qc_binary_path(text: str) -> str | None:
    """Extract BINARY_PATH_NAME from `sc qc <service>` output."""
    for raw in text.splitlines():
        line = raw.strip()
        key, sep, value = line.partition(":")
        if not sep:
            continue
        if key.strip().upper().replace(" ", "_") == "BINARY_PATH_NAME":
            value = value.strip().strip('"')
            return value or None
    return None


def service_image_name(binary_path: str) -> str:
    r"""Lowercased executable name from an SCM `BINARY_PATH_NAME`.

    Splitting on whitespace to drop trailing service arguments is the obvious
    approach and it is wrong: the real path is
    `C:\Program Files\WSL\wslservice.exe`, so the first token is
    `C:\Program` and every genuine install would be refused forever.

    Handle the two shapes SCM actually emits:

    - quoted (`"C:\Program Files\WSL\wslservice.exe" -k netsvcs`) — take
      the quoted span;
    - unquoted, possibly with arguments — cut at the first `.exe`, which is the
      only reliable boundary when the path itself may contain spaces.
    """
    text = binary_path.strip()
    if text.startswith('"'):
        end = text.find('"', 1)
        if end > 0:
            return ntpath.basename(text[1:end]).lower()
        text = text[1:]
    lowered = text.lower()
    cut = lowered.find(".exe")
    if cut != -1:
        text = text[: cut + 4]
    return ntpath.basename(text).lower()


def wsl_service_recovery_gate(
    status: ServiceStatus,
    binary_path: str | None,
) -> tuple[bool, str]:
    """May we force-terminate this service PID? Returns `(allowed, reason)`.

    Fail-closed by construction — this authorizes a privileged kill, so every
    branch that is not positively "the wedged state we diagnosed" refuses. The
    reason string is returned rather than logged so the caller can print each
    guarded decision, which the issue asks for.

    Checks, in order:

    1. **State is exactly STOP_PENDING.** A running service is healthy and a
       stopped one needs a plain `sc start`; neither warrants a kill.
    2. **PID is present and nonzero.** SCM reports 0 for a service with no
       process, and killing PID 0 is meaningless at best.
    3. **The image is `wslservice.exe`.** The SCM-reported binary must be the
       executable we expect, so a mis-registered or hijacked service name
       cannot direct the kill at something else.
    """
    state = (status.state or "").upper()
    if state != "STOP_PENDING":
        return (
            False,
            f"{WSL_SERVICE} state is {status.state or 'unknown'}, not STOP_PENDING "
            "— no forced recovery (fail-closed)",
        )
    if not status.pid:
        return (
            False,
            f"{WSL_SERVICE} reports no service PID — nothing safe to terminate",
        )
    if not binary_path:
        return (
            False,
            f"{WSL_SERVICE} binary path is unknown — refusing to terminate an "
            "unverified image",
        )
    image = service_image_name(binary_path)
    if image != WSL_SERVICE_IMAGE:
        return (
            False,
            f"{WSL_SERVICE} resolves to {image!r}, not {WSL_SERVICE_IMAGE!r} — "
            "refusing to terminate an unexpected image",
        )
    return (
        True,
        f"{WSL_SERVICE} is STOP_PENDING with pid={status.pid} and image "
        f"{WSL_SERVICE_IMAGE}; forced recovery is authorized",
    )


def query_wsl_service() -> tuple[ServiceStatus, str | None]:
    """Ask SCM for the service's state/PID and its registered binary."""
    queryex = _run(["sc", "queryex", WSL_SERVICE], timeout=15.0)
    status = parse_sc_queryex(queryex.stdout) if queryex else ServiceStatus(None, None)
    qc = _run(["sc", "qc", WSL_SERVICE], timeout=15.0)
    binary = parse_sc_qc_binary_path(qc.stdout) if qc else None
    return status, binary


def recover_wsl_service() -> tuple[bool, list[str]]:
    """Diagnose and, if every gate passes, force-recover a wedged `WslService`.

    Returns `(recovered, details)`; `details` carries the original diagnosis and
    each guarded decision so the command output explains itself.

    Storage safety is unconditional: this terminates a process and starts a
    service. It never unregisters a distro, deletes a VHD, or touches Docker
    images/volumes.
    """
    details: list[str] = []
    status, binary = query_wsl_service()
    details.append(
        f"SCM: {WSL_SERVICE} state={status.state or 'unknown'} "
        f"pid={status.pid or 0} binary={binary or 'unknown'}"
    )

    allowed, reason = wsl_service_recovery_gate(status, binary)
    details.append(reason)
    if not allowed:
        return False, details

    # Re-check immediately before terminating: the diagnosis above and the kill
    # below are separated by an elevation prompt the user may sit on for a
    # while, and a PID can be recycled in that window.
    recheck, recheck_binary = query_wsl_service()
    allowed_now, recheck_reason = wsl_service_recovery_gate(recheck, recheck_binary)
    if not allowed_now or recheck.pid != status.pid:
        details.append(
            "service identity changed between diagnosis and termination "
            f"(was pid={status.pid}, now pid={recheck.pid or 0}: {recheck_reason}) "
            "— aborting rather than killing a recycled PID"
        )
        return False, details

    ok, elevate_detail = _elevated_wsl_service_restart(recheck.pid)
    details.append(elevate_detail)
    if not ok:
        return False, details

    running = _wait_for_wsl_service_running()
    details.append(
        f"{WSL_SERVICE} reached RUNNING" if running
        else f"{WSL_SERVICE} did not reach RUNNING within the wait budget"
    )
    return running, details


class RunningProcessUnavailable(RuntimeError):
    """The running-process CLI could not be resolved for the elevated step."""


def _resolve_running_process_binary(resolver=None) -> str | None:
    """Absolute path to the running-process CLI, or None when unresolvable.

    Resolution happens here, in the *invoking* user's PATH, because the
    elevated step runs as Administrator — whose PATH does not generally carry
    the user venv or `~/.local/bin` that running-process installs into. A bare
    command name would simply not be found there, and `&&` would then skip the
    `sc start` that is the whole point of the operation.
    """
    # Bound at call time, not as a default: a def-time default would capture
    # the original `shutil.which` and ignore any later substitution.
    found = (resolver or shutil.which)("running-process")
    return str(Path(found).resolve()) if found else None


def elevated_restart_command(
    pid: int, *, running_process: str | None = None
) -> list[str]:
    """PowerShell argv for the scoped elevated operation.

    Shape-only so the blast radius is asserted in tests: exactly one blessed
    cross-platform tree termination against the validated PID and one
    `sc start`, with a visible UAC prompt (`-Verb RunAs`). Nothing here may
    grow a `wsl --unregister`, a `Remove-Item`, or a diskpart call — that is
    what the test guards.

    Raises `RunningProcessUnavailable` rather than emitting a command that
    would fail after the operator has already approved elevation.
    """
    validated = int(pid)
    binary = running_process or _resolve_running_process_binary()
    if binary is None:
        raise RunningProcessUnavailable(
            "running-process CLI not found on PATH — cannot terminate the "
            f"process holding {WSL_SERVICE}"
        )
    if "'" in binary:
        # The path is embedded in a single-quoted PowerShell argument; a quote
        # would break out of it. Refuse rather than build a malformed command.
        raise RunningProcessUnavailable(
            f"running-process path contains a quote and cannot be elevated safely: {binary}"
        )
    inner = f'"{binary}" --terminate-tree {validated} && sc.exe start {WSL_SERVICE}'
    return [
        "powershell.exe",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        f"Start-Process -FilePath cmd.exe -ArgumentList '/c {inner}' "
        "-Verb RunAs -Wait -WindowStyle Hidden",
    ]


def _elevated_wsl_service_restart(pid: int) -> tuple[bool, str]:
    """Run the scoped terminate-and-start under a visible UAC prompt."""
    try:
        command = elevated_restart_command(pid)
    except RunningProcessUnavailable as exc:
        # Fail before prompting. A UAC dialog the operator approves only for
        # cmd.exe to fail on an unresolvable binary — leaving the service
        # stopped — is strictly worse than saying so up front.
        return False, f"{exc} — WSL was left untouched"
    result = _run(command, timeout=120.0)
    if result is None:
        return False, "elevated recovery could not be launched (or timed out)"
    if result.returncode != 0:
        # A declined UAC prompt surfaces here; say so plainly rather than
        # reporting a generic failure the operator cannot act on.
        return False, (
            "elevation was denied or the elevated command failed "
            f"(exit {result.returncode}) — WSL was left untouched"
        )
    return True, f"elevated recovery ran: terminated pid={pid}, started {WSL_SERVICE}"


def _wait_for_wsl_service_running(
    *, attempts: int = 20, interval: float = 1.0
) -> bool:
    for index in range(attempts):
        status, _binary = query_wsl_service()
        if (status.state or "").upper() == "RUNNING":
            return True
        if index < attempts - 1:
            time.sleep(interval)
    return False


def _execute_restart(system: str, *, hard: bool) -> list[str]:
    details: list[str] = []
    if system == "Windows":
        if hard:
            wsl = _run(["wsl", "--shutdown"], timeout=60.0)
            details.append(_command_result_detail("wsl --shutdown", wsl))
            # #632: `wsl --shutdown` returning nothing means it timed out or
            # could not run -- the signature of WslService wedged in
            # STOP_PENDING, where every `wsl` invocation hangs. Only the
            # explicitly-confirmed `reset --yes` path (hard) reaches here;
            # `restart --yes` stays the light path and never force-recovers.
            if wsl is None or wsl.returncode != 0:
                recovered, wsl_details = recover_wsl_service()
                details.extend(wsl_details)
                if recovered:
                    details.append(
                        "WSL service recovered; continuing to the Docker "
                        "relaunch and readiness checks"
                    )

        guard_ok, guard_detail = _launch_windows_docker_desktop_guard()
        details.append(guard_detail)
        if guard_ok:
            if _wait_for_windows_server_deadline():
                details.append("Docker server API observed after guarded CLI launch")
                return details
            details.append(
                "daemon guard started but the Docker server API was not observed "
                "within 60 seconds; the guarded startup attempt remains authoritative"
            )
            return details

        # Prefer launching the real executable so Docker Desktop itself
        # inherits the positive daemon marker. `docker desktop start` may
        # hand the launch off through an existing CLI/service path that
        # drops the caller's environment; the engine can briefly become
        # healthy and then be reaped as soon as this tool exits.
        executable = _windows_docker_desktop_executable()
        if executable is not None:
            baseline = _windows_docker_process_identities()
            ok, detail = _launch_windows_docker_desktop(executable)
            details.append(detail)
            if ok:
                observation = _wait_for_windows_desktop_launch(
                    check=lambda: _windows_desktop_launch_observation(
                        baseline, source="direct"
                    )
                )
                if observation is not None:
                    details.append(observation)
                    return details
                details.append(
                    "direct launch started but no Docker server API or new runtime "
                    "process was observed; trying CLI fallback"
                )
        else:
            details.append("direct Docker Desktop launch unavailable: executable not found")

        # Retain the CLI as a compatibility fallback for non-standard
        # installations or a direct launch failure.
        baseline = _windows_docker_process_identities()
        cli = _run(
            ["docker", "desktop", "start"],
            timeout=60.0,
            env=_declared_daemon_environment(),
        )
        details.append(_command_result_detail("docker desktop start", cli))
        if cli is not None and cli.returncode == 0:
            observation = _wait_for_windows_desktop_launch(
                check=lambda: _windows_desktop_launch_observation(baseline, source="CLI")
            )
            if observation is not None:
                details.append(observation)
                return details
            details.append(
                "CLI reported success but no Docker server API or runtime process was observed"
            )
    elif system == "Darwin":
        result = _run(["open", "-a", "Docker"], timeout=30.0)
        details.append(_command_result_detail("open -a Docker", result))
    else:
        result = _run(["sudo", "systemctl", "restart", "docker"], timeout=60.0)
        details.append(_command_result_detail("systemctl restart docker", result))
        if result is None:
            fallback = _run(["sudo", "service", "docker", "restart"], timeout=60.0)
            details.append(_command_result_detail("service docker restart", fallback))
    return details


def cmd_restart(args: argparse.Namespace) -> int:
    return _run_recovery(args, label="restart")


def cmd_reset(args: argparse.Namespace) -> int:
    return _run_recovery(args, label="reset")


def cmd_disk(args: argparse.Namespace) -> int:
    out = sys.stdout
    system = platform.system()
    _print_report_header(out, "docker storage")

    if system != "Windows":
        out.write(
            f"storage resolution for {system} is report-only in v0. "
            "macOS: Docker.raw; Linux: data-root (normally /var/lib/docker). "
            "Query Docker config first; never auto-mutate.\n"
        )
        if args.action:
            out.write("refusing: destructive storage actions are Windows-only in v0.\n")
            return EXIT_NOT_AUTO_EXECUTED
        return EXIT_OK

    resolution = _windows_resolution()
    resolution = apply_selection(resolution, args.select)
    _print_resolution(out, resolution)

    if not args.action:
        return EXIT_OK

    docker_stopped = docker_server_version() is None and not list_docker_processes()
    code, message = disk_action_gate(resolution, confirmed=args.yes, docker_stopped=docker_stopped)
    if code != EXIT_OK:
        sys.stderr.write(f"\n{message}\n")
        return code

    # All gates passed. v0 deliberately does NOT auto-execute destructive
    # storage work — it prints the vetted plan instead (issue #531: never
    # compact, delete, prune, reset, or mutate Docker storage automatically).
    chosen = resolution.chosen
    assert chosen is not None  # guaranteed by the gate
    out.write(f"\ngates passed for `{args.action}` on {chosen.path}. v0 will NOT run it.\n")
    out.write("Vetted manual plan:\n")
    out.write(f"  1. Back up {chosen.path} to a separate volume first.\n")
    out.write("  2. Confirm Docker Desktop and WSL are fully stopped (`wsl --shutdown`).\n")
    if args.action == "compact":
        out.write(f"  3. Compact: `Optimize-VHD -Path '{chosen.path}' -Mode Full` (admin).\n")
    elif args.action == "prune":
        out.write("  3. Prune from a HEALTHY daemon: `docker system prune` (opt-in).\n")
    elif args.action == "delete":
        out.write(f"  3. Delete only after backup: remove {chosen.path}; Docker recreates it.\n")
    elif args.action == "reset":
        out.write("  3. Factory reset via Docker Desktop > Troubleshoot (wipes images/volumes).\n")
    out.write("  4. Relaunch Docker Desktop and run `doctor` to verify.\n")
    return EXIT_NOT_AUTO_EXECUTED


def cmd_gc(args: argparse.Namespace) -> int:
    """Reclaim dangling Docker objects. Default-safe: no confirmation gate,
    because pruned images/containers/anon volumes are cheap to rebuild. Never
    touches running containers, images backing a running container, named
    volumes, or objects below the age threshold. Idempotent one-shot — safe
    to wire into cron / Task Scheduler / `clud schedule`."""
    out = sys.stdout
    _print_report_header(out, "docker gc")

    if docker_server_version() is None:
        out.write(
            "docker engine unreachable — cannot GC. Restart the engine first "
            "(`restart`), then re-run gc.\n"
        )
        return EXIT_UNHEALTHY

    on_system = _data_disk_on_system_volume()
    free_before = host_free_disk()
    inventory = gather_gc_inventory()
    plan = plan_gc(
        inventory,
        now=time.time(),
        on_system_volume=on_system,
        base_age_hours=args.age_hours,
    )
    out.write(
        f"data disk on system volume: {on_system}; age threshold: {plan.age_hours:g}h\n"
    )
    out.write(
        f"reclaimable: {len(plan.images)} images, {len(plan.containers)} stopped "
        f"containers, {len(plan.volumes)} anonymous volumes "
        f"(~{_human_bytes(plan.reclaimable_bytes)})\n"
    )
    if args.dry_run:
        out.write("dry-run: nothing reclaimed. Named volumes are never touched.\n")
        return EXIT_OK

    result = execute_gc(plan)
    out.write(
        f"reclaimed: {result.images_removed} images, {result.containers_removed} "
        f"containers, {result.volumes_removed} anonymous volumes; "
        f"freed ~{_human_bytes(result.freed_bytes)}\n"
    )
    out.write(
        f"free disk before -> after: {_human_bytes(free_before)} -> "
        f"{_human_bytes(host_free_disk())}\n"
    )
    out.write("named volumes and running-container images were preserved.\n")
    return EXIT_OK


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="docker_recover",
        description="Cross-platform Docker Desktop recovery + diagnostics (read-only doctor; "
        "confirmation-gated, non-destructive recovery).",
    )
    sub = parser.add_subparsers(dest="cmd")

    sub.add_parser("doctor", help="read-only health + storage report; mutates nothing")

    p_restart = sub.add_parser("restart", help="clean restart of the normal Docker runtime")
    p_restart.add_argument("--yes", action="store_true", help="confirm the restart")
    p_restart.add_argument("--force", action="store_true", help="restart even if healthy")

    p_reset = sub.add_parser("reset", help="platform runtime reset (wsl --shutdown + relaunch)")
    p_reset.add_argument("--yes", action="store_true", help="confirm the reset")
    p_reset.add_argument("--force", action="store_true", help="reset even if healthy")

    p_disk = sub.add_parser("disk", help="report Docker storage; destructive actions are gated")
    p_disk.add_argument(
        "--action",
        choices=("compact", "prune", "delete", "reset"),
        help="requested destructive action (refused unless unambiguous + --yes + stopped)",
    )
    p_disk.add_argument("--select", help="disambiguate by selecting a candidate disk path")
    p_disk.add_argument("--yes", action="store_true", help="confirm the destructive action")

    p_gc = sub.add_parser(
        "gc",
        aliases=["trim"],
        help="reclaim dangling Docker objects (safe, no confirmation)",
    )
    p_gc.add_argument(
        "--age-hours",
        type=float,
        default=GC_DEFAULT_AGE_HOURS,
        help="reclaim images/containers older than this "
        "(default 24; halved on the system volume)",
    )
    p_gc.add_argument("--dry-run", action="store_true", help="report candidates without reclaiming")
    return parser


def main(argv: list[str]) -> int:
    if argv == [WINDOWS_DAEMON_GUARD_ARG]:
        return _windows_daemon_guard_main()

    parser = build_parser()
    args = parser.parse_args(argv)
    if args.cmd == "doctor":
        return cmd_doctor(args)
    if args.cmd == "restart":
        return cmd_restart(args)
    if args.cmd == "reset":
        return cmd_reset(args)
    if args.cmd == "disk":
        return cmd_disk(args)
    if args.cmd in ("gc", "trim"):
        return cmd_gc(args)
    parser.print_help(sys.stderr)
    return EXIT_USAGE


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
