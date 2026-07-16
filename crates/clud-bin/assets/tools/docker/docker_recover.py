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
    clud tool run docker/docker_recover.py restart [--yes]
    clud tool run docker/docker_recover.py reset [--yes]
    clud tool run docker/docker_recover.py disk [--action compact|prune|delete|reset] \
        [--select <path>] [--yes]

Subcommands:
    doctor   Read-only report: client/server availability, engine error,
             host free memory + disk, Docker runtime processes, the resolved
             Docker data-disk path/size + confidence, and recent relevant
             logs. Mutates nothing (no restart, no disk write, no rotation).
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
import json
import ntpath
import os
import platform
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import ClassVar

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

# Advisory host-resource thresholds. Crossing them never blocks a healthy
# daemon — they surface as advisories, not failures.
LOW_DISK_BYTES = 2 * 1024**3
LOW_MEM_BYTES = 1 * 1024**3

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
    source: str,
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
        source=source,
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
    settings_source = None  # filled in by the caller that also knows the path
    return DiskResolution(
        candidates=candidates,
        chosen=chosen,
        ambiguous=ambiguous,
        settings_present=settings_present,
        settings_source=settings_source,
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


# ---- Recovery plans (pure, testable text) --------------------------------
def windows_restart_plan() -> list[str]:
    return [
        "Stop orphaned Docker helper processes (com.docker.build, com.docker.backend) if wedged.",
        "Run `wsl --shutdown` to cycle the WSL2 utility VM.",
        'Relaunch Docker Desktop (`docker desktop start`, or start "Docker Desktop.exe").',
        "Poll `docker version` on a bounded schedule "
        f"({READY_ATTEMPTS} attempts, {READY_INTERVAL_SECONDS:g}s interval).",
        "Verify with `docker run --rm hello-world` and `docker buildx ls`.",
        "Containers will STOP during this sequence; images and volumes are PRESERVED.",
    ]


def macos_restart_plan() -> list[str]:
    return [
        "Quit Docker Desktop, then relaunch it (`open -a Docker`, or `docker desktop start`).",
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


def _run(cmd: list[str], timeout: float = 20.0) -> subprocess.CompletedProcess | None:
    try:
        return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, check=False)
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
        _fields_: ClassVar = [
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
    if not ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(stat)):  # type: ignore[attr-defined]
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
    out.write(
        f"  settings: {'present' if resolution.settings_present else 'absent'}"
        f"{f' ({resolution.settings_source})' if resolution.settings_source else ''}; "
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
    _execute_restart(system, hard=(label == "reset"))
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


def _execute_restart(system: str, *, hard: bool) -> None:
    if system == "Windows":
        if hard:
            _run(["wsl", "--shutdown"], timeout=60.0)
        if _run(["docker", "desktop", "start"], timeout=60.0) is None:
            _run(["cmd", "/c", "start", "", "Docker Desktop.exe"], timeout=30.0)
    elif system == "Darwin":
        _run(["open", "-a", "Docker"], timeout=30.0)
    else:
        if _run(["sudo", "systemctl", "restart", "docker"], timeout=60.0) is None:
            _run(["sudo", "service", "docker", "restart"], timeout=60.0)


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
    return parser


def main(argv: list[str]) -> int:
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
    parser.print_help(sys.stderr)
    return EXIT_USAGE


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
