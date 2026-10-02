//! clud's integration tests: one harness (#1726).
//!
//! Every test that needs the built `clud` binaries or a real process tree
//! lives here, one module per category: `api`, `cli`, `diagnostics`, `pty`,
//! `reaper` and `signals`. Test IDs are `<category>::<module>::<test_name>`.
//!
//! One target, not six (#1056 folded 23 files into six categories): each
//! harness statically links the whole workspace, and zccache never caches
//! harness link products (zackees/zccache#1525), so every extra harness was a
//! full uncached compile and link on every CI run. `ci/harness_budget.py`
//! keeps it at one.
//!
//! The categories still run in separate processes in CI: `ci/run_bundle.py`
//! runs each category of this harness on its own (and every `pty` test in its
//! own pseudo-terminal), so process-wide state such as the reaper's host-wide
//! sweeps never meets another category's tests.

// `#[macro_use]` because `common/mod.rs` defines `require_pty_or_skip!`, which
// must be in textual scope before the categories that use it.
#[macro_use]
mod common;

// Included standalone (see its header); members reach it as `crate::exe`.
#[path = "common/exe.rs"]
mod exe;

mod api;
mod cli;
mod diagnostics;
mod pty;
mod reaper;
mod signals;
