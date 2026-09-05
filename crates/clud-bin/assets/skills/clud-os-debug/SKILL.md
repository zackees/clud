---
name: clud-os-debug
description: Diagnose a live crash, hang, or CPU burn on Windows, macOS, or Linux with the OS's own debugger and sampling profiler.
triggers:
  - When a process is hung, spinning, or crashing and the cause is not in a log
  - When the user asks why something is using CPU, or asks for a stack dump or profile
  - When a crash report exists but the question is what the process is doing *now*
---
<!-- managed-by: clud -->

# clud-os-debug

`clud symbols` and the crash reporter answer "what did it do before it died". This
skill answers "what is it doing right now" — a live stack, an attached debugger, or a
sampled CPU profile, using the tools the OS already ships.

## Code Change Rule

A diagnosis is not a fix. When this investigation turns into a bug fix, switch to
RED -> GREEN: turn what you observed into a focused failing test first — the stack
you captured names the function, the profile names the hot path — run it to see it
fail, make the scoped change, and rerun it to pass. A profile that leads straight to
an edit leaves nothing behind to stop the same regression returning.

## Pick the right instrument first

These three get confused constantly, and reaching for the wrong one wastes the
reproduction you may not get again:

| You want to know | Use | Cost |
|---|---|---|
| Where is it stuck *right now* | **Live stack dump** — one sample of every thread | Milliseconds; process pauses briefly |
| Why does this state happen | **Debugger attach** — breakpoints, memory, stepping | Process stops until you detach |
| What is eating the CPU | **Sampled profile** — periodic stacks over a window | Seconds to minutes; process keeps running |

A hang wants a stack dump. A crash wants the debugger (or the existing crash report).
CPU burn wants a profile — a single stack of a busy process tells you almost nothing,
because you sampled one arbitrary instant.

## Before you attach to anything

1. **Get the PID and confirm it is the right one.** `pgrep -af <name>` /
   `Get-Process <name>`. Attaching to the wrong PID of a multi-process app (browsers,
   Electron, anything with a helper tree) is the most common wasted attempt.
2. **A stopped process is a stopped service.** Attaching a debugger *suspends the
   target*. On anything shared or user-facing, prefer a stack dump or a sampled
   profile, which do not.
3. **Elevation is the user's call, not yours.** Where a step needs sudo, UAC, or a
   `ptrace_scope` change, say which step needs it and why, run the OS's normal consent
   mechanism, and stop if the user declines. Never weaken a system policy to make a
   diagnosis convenient — `ptrace_scope`, SIP, and UAC exist to stop exactly what you
   are about to do, and turning them off outlives your investigation.

## Linux

**Live stack of a running process** — no install, no elevation on your own processes:

```bash
gdb -p <pid> -batch -ex "thread apply all bt"     # every thread
eu-stack -p <pid>                                  # elfutils, lighter
cat /proc/<pid>/stack                              # kernel-side only
```

**Sampled CPU profile:**

```bash
perf record -F 99 -g -p <pid> -- sleep 20
perf report --stdio           # or: perf script | stackcollapse-perf.pl | flamegraph.pl
```

**What blocks it, and the exact prerequisite:**

- `ptrace: Operation not permitted` attaching to *your own* process →
  `/proc/sys/kernel/yama/ptrace_scope` is `1`. The scoped fix is to run the debugger
  under `sudo`, **not** to set the sysctl to `0` machine-wide.
- `perf` returning no samples → `/proc/sys/kernel/perf_event_paranoid`. Report the
  value and what it permits; changing it is a system-policy decision.
- Missing symbols → install the distro's `-dbg`/`-debuginfo` package for that binary.

**Syscall-level**: `strace -f -p <pid>` for a hang that is blocked in the kernel;
`ltrace` for library calls. Both slow the target materially — bound the window.

## macOS

```bash
sample <pid> 10 -f /tmp/sample.txt   # sampled profile, no elevation for your own procs
spindump <pid> 10                    # system-wide, needs sudo, best for beachballs
lldb -p <pid>                        # attach
xcrun xctrace record --template 'Time Profiler' --attach <pid>
```

- `sample` is the first thing to run for a spinning process. It is non-invasive and
  usually enough.
- Attaching to a process you do not own, or one that is SIP-protected, will fail;
  that is the boundary working. Report it rather than looking for a way around.
- Hardened-runtime apps reject debuggers unless built with
  `com.apple.security.get-task-allow`. For a third-party app, this is a dead end —
  say so.

## Windows

```powershell
Get-Process <name> | Select Id,ProcessName,CPU
```

**Live stack / crash dump** — inbox, no install:

```powershell
# Full user-mode dump; open in WinDbg or Visual Studio
Get-Process <name> | ForEach-Object {
  & "$env:SystemRoot\System32\rundll32.exe" `
    "$env:SystemRoot\System32\comsvcs.dll",MiniDump $_.Id C:\temp\dump.dmp full
}
```

`procdump -ma <pid> C:\temp\dump.dmp` (Sysinternals) is friendlier if present.

**Sampled CPU profile** — WPR is inbox on Windows 10/11:

```powershell
wpr -start CPU -filemode
# ... reproduce for a bounded window ...
wpr -stop C:\temp\trace.etl
```

- `wpr` requires an elevated shell. Ask once, through UAC, and explain that the trace
  is machine-wide for the capture window.
- **If WPA (the analysis GUI) is missing**, that is the Windows Performance Toolkit,
  an ADK component. Installing it is a privileged, machine-wide change: propose it,
  let the user run the installer, and do not launch an installer unattended. The
  `.etl` is still captured and analysable elsewhere without it.
- Keep the capture bounded. A `CPU` profile writes hundreds of MB per minute.

## Reporting

Whatever you capture, report: the **artifact path**, the **exact command**, the
**PIDs** it covers, and the **window** it spans. A profile with no window and a stack
with no PID are both unfalsifiable later.

State plainly which of the three instruments you used. "I profiled it" when you took
one stack dump is the error this skill exists to prevent.

## What this skill does not do

- **It installs nothing.** Every command above is either inbox or a package the user
  already has. Where a tool is missing, name the tool and let the user install it.
- **It does not elevate on its own**, change `ptrace_scope`, disable SIP, or work
  around a hardened runtime. Each of those is the OS refusing on purpose.
- **It is not a substitute for the crash report.** For a process that already died,
  `clud symbols` and `~/.clud/state/crashes/` are the right starting point.
- **Automated, checksum-verified tool acquisition** (#563's second half) is not here;
  bundled skills ship as documentation only.
