# Crash reports

JSON crash reports landed in three PRs against #374:

- **PR #376** flipped both cargo profiles to `debug = "line-tables-only"`
  so every clud build embeds `file:line` debug info, and added a
  process-wide Rust panic hook (`crash_report::install`) that writes a
  JSON record to `~/.clud/state/crashes/<unix_ms>-<role>-<pid>.json`
  with bounded rotation (50 most recent).
- **PR #385** added a native crash handler (`crash_report::install_native`,
  built on the [`crash-handler`](https://docs.rs/crash-handler) crate)
  for SIGSEGV / SIGBUS / SIGILL / SIGFPE / SIGABRT on Unix and
  structured exceptions on Windows. Both panic-driven and
  native-driven crashes share one writer; the JSON gains a `kind`
  discriminator (`"panic"` vs `"native"`) plus native-only fields
  (`signal_or_exception`, `signal_number`, `exception_code`,
  `faulting_address`).
- **PR #386** added the `clud symbols` / `clud symbols install` /
  `clud symbols verify [--all]` subcommand that opportunistically
  verifies the running binary can symbolicate recent crash reports.

This doc explains the two non-obvious design choices.

## DD-N: embed line tables everywhere, no sidecars

The original #374 proposal sketched a `clud symbols install` command
that would fetch matching `.pdb` / `.dSYM` / `.dwp` sidecars from a
release artifact bucket, and a release-CI step that staged those
sidecars alongside binaries.

The agreed choice was to **embed `line-tables-only` debug info in every
profile** instead:

```toml
# Cargo.toml (workspace root)
[profile.dev]
debug = "line-tables-only"

[profile.release]
debug = "line-tables-only"
```

Tradeoffs accepted:

- **+** No sidecar staging in release CI; no per-platform path tree to
  maintain (`x86_64-unknown-linux-gnu/clud.dwp`,
  `aarch64-apple-darwin/clud.dSYM/...`, ...).
- **+** No "wrong build identity" symbol-resolution failures — the
  binary that produced the crash IS the binary that resolves it.
- **+** No network on the user's machine to symbolicate.
- **−** Slightly larger binaries. `line-tables-only` is the cheapest
  debug level (no variable / type info); growth measured in single-digit
  MB rather than the 30-40% expansion of `debug = true`.
- **−** Full source-level debugging (variable inspection, type-aware
  tooling) still requires explicitly toggling `debug = true` locally.

The `clud symbols install` subcommand is preserved as documented in the
#374 acceptance criteria, but reinterpreted as an **opportunistic
verifier** rather than a fetcher (next section).

## DD-N: opportunistic verifier, not opportunistic fetcher

The #374 open question was whether `clud symbols install` should be
explicit (user runs it after seeing an unsymbolicated crash report) or
opportunistic (auto-fetch sidecars on the next startup whenever an
unsymbolicated report is present).

With line tables embedded everywhere there is nothing to fetch, so
"opportunistic fetch" collapses into "opportunistic verify":

1. On every clud startup, `crash_report::install` scans
   `~/.clud/state/crashes/` for a report newer than the recorded
   `last_seen` watermark.
2. If a fresh report is found, the existing one-line stderr notice
   fires: `clud: previous crash report at <path>`.
3. **New in PR 3:** if that fresh report's `backtrace` field contains
   zero `at FILE:LINE` frames, a second line fires:
   `clud: backtrace appears unsymbolicated; run `clud symbols verify`
   for diagnostic details`.
4. The `last_seen` watermark advances regardless, so subsequent
   launches stay quiet about the same report.

This gives users a discoverable on-ramp to the verifier without making
network calls behind their back or running the verifier eagerly on
every launch (the JSON parse + scan only happens when a fresh report
exists, which should be vanishingly rare in normal operation).

The verifier itself supports three forms:

- `clud symbols` — five-line summary (total reports, count with /
  without `file:line` frames, most-recent path).
- `clud symbols install` — verify only the most-recent report;
  exit 1 if it's unsymbolicated.
- `clud symbols verify [--all]` — same; `--all` widens the scope from
  the most-recent report to every report under
  `~/.clud/state/crashes/`.

A report is considered "unsymbolicated" when `count_resolved_frames` in
`crates/clud-bin/src/symbols.rs:55` returns 0. The frame heuristic
matches `at FILE:LINE` lines produced by `std::backtrace::Backtrace`
(handling Windows drive-letter colons via right-anchored search).

## Test coverage

- `crash_report::tests::*` — panic-hook + rotation + sanitize + native
  signal/exception name lookup (10 unit tests).
- `tests/crash_report.rs` — end-to-end panic catch in-process (2
  integration tests).
- `symbols::tests::*` — `is_resolved_frame_line`, `count_resolved_frames`,
  `is_unsymbolicated`, `list_reports_newest_first` (8 unit tests).
- `tests/symbols.rs` — `clud symbols verify --all` / `clud symbols
  install` / bare `clud symbols` exit codes + output (4 integration
  tests spawning the real `clud` binary with `CLUD_DAEMON_STATE_DIR`
  redirected).

No deliberate-native-crash CI test: attaching a real signal/SEH handler
in a test process would intercept SIGABRT in unrelated tests. The
underlying `crash-handler` crate has upstream tests; manual reproduction
steps are documented inline in
`crates/clud-bin/src/crash_report.rs` next to the unit tests.
