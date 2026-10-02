---
name: terminal-pty-macos
description: Use before changing or debugging clud's terminal/PTY path on macOS (POSIX PTY, raw mode, libc termios/openpty type differences from Linux, escape sequences, terminal restore). Covers what differs from Linux, how to catch macOS-only compile errors early, and how to validate with the ci-full label since neither the host nor act can run macOS.
---

# Terminal / PTY on macOS

Shared facts (data flow, the three invariants, test tiers, act rules, CI
labels) are in
[docs/architecture/terminal-pty.md](../../../docs/architecture/terminal-pty.md).
The pump, escape gate, stream scanners, signal handling and terminal
restore are the **same POSIX code as Linux**. Read
[terminal-pty-linux](../terminal-pty-linux/SKILL.md) for those. This page
lists only what differs on macOS.

## What is different on macOS

- **No local execution.** There is no local macOS machine, and `act` cannot
  run macOS jobs. Any runtime behaviour can only be observed on GitHub's
  hosted macOS runners.
- **libc signatures and types differ from Linux.** Both of these were
  verified while #1707 carried a `libc::openpty` call; the current tree has
  none.
  - `libc::openpty` takes `*mut termios` and `*mut winsize` on macOS, but
    `*const` on Linux. Passing `std::ptr::null_mut()` compiles on both.
  - `libc::tcflag_t` is `u64` on macOS and `u32` on Linux. An `as` cast that
    is a no-op on one platform trips `clippy::unnecessary_cast` on the
    other. Prefer `From` / `try_from` or slice copies.
- **Terminal emulators.** Nothing in this repo pins down how Terminal.app or
  iTerm2 handle the restore sequence, kitty keyboard frames, or mouse
  modes. Treat any claim about them as **unverified** unless a test or an
  issue documents it.

## Files to read first

These are the same as on Linux. Start with:

- `crates/clud-bin/src/session.rs`: `RawTerminalGuard` (`:316`, `Drop` at
  `:602`), `KeyboardEnhancementTracker` (`:344`) and `forward_user_input`
  (`:1252`).
- `crates/clud-bin/src/session/child_modes.rs` and
  `crates/clud-bin/src/session/escape_gate.rs`.
- `crates/clud-bin/tests/pty/pty_pump.rs:378`, the #1704 restore test. On
  `cfg(unix)` (macOS included) it asserts the full mode set.
- Any `#[cfg(target_os = "macos")]` or `#[cfg(unix)]` code that touches
  `libc` termios or PTY APIs.

## Invariants you must not break

All of the invariants in terminal-pty.md apply: sequence atomicity,
resumable scanners, and restore from `Drop`. In addition:

- **Code that touches `libc` must compile on both macOS and Linux.** Write
  it against the looser signature (`*mut`) and convert integer types
  without `as` casts.

## How to validate a change

1. Run the shared Linux checks locally:
   `bosn run --task act-ci-static` and `bosn run --task act-ci-linux`. See
   terminal-pty.md § Validation for the container gotchas.
2. Push. On routine PRs the **Dylint** job also compiles check-only for
   `aarch64-apple-darwin`. It caught a macOS-only compile error while #1709
   was in progress, so read its result before anything else.
3. Add the `ci-full` label for runtime coverage. It adds the hosted macOS
   arm64 and x64 build and test lanes, plus the other targets.
4. Watch the run with
   `clud tool run github/pr_merge_watch.py -- <PR> [--no-cancel]`. Read a
   failing job with
   `gh api --allow-escape-sequences repos/zackees/clud/actions/jobs/<job_id>/logs`.

Report honestly: a green `act-ci-linux` run does not prove macOS behaviour,
and Dylint proves only that the code compiles for macOS.

## Known behaviours and gotchas

- `ci-full` is the only label that runs macOS. `ci-windows` and `ci-test`
  do not ([ci.md § Current CI selection](../../../docs/architecture/ci.md#current-ci-selection)).
- The `pty` test harness runs inside a pseudo-terminal in CI with
  `CLUD_REQUIRE_PTY=1` on every platform, so a test that would skip locally
  fails on the macOS lane if no PTY is available
  ([testing-tiers.md](../../../docs/architecture/testing-tiers.md)).
- Mouse garbage, a stuck alternate screen, or a missing cursor after Ctrl+C
  is the same bug class as on Linux (#1701, #1704). Debug it with the Linux
  skill's checklist.
