//! One lock for the home-directory environment, shared across test modules.
//!
//! Two test files rewrite `HOME` / `USERPROFILE`: the `CwdChanged` handler
//! tests and `gc::session_tmp`. Each grew its own private mutex, which
//! serializes that file against itself and nothing else — while the resource
//! they are guarding is this process's environment, shared by every test in
//! the binary. Two locks, one resource, so the two files can still interleave
//! and redirect each other's home mid-test.
//!
//! This is a latent race, not a diagnosis of any current failure. It was the
//! first hypothesis for the flake fixed in #1136, and it was wrong there — the
//! cause was EPIPE on a hook's payload write. The two-locks-one-resource
//! problem is real regardless, and is cheaper to remove than to re-diagnose
//! the next time a home-dependent test misbehaves.
//!
//! Anything that writes `HOME` or `USERPROFILE` holds this, not a lock of its
//! own.

use std::sync::{Mutex, MutexGuard, OnceLock};

/// Serializes every test that rewrites the home-directory environment.
///
/// Poison is recovered rather than propagated: a panicking test has already
/// failed, and turning its poison into failures across unrelated tests would
/// obscure which one actually broke.
pub(crate) fn home_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}
