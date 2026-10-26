//! dockhand: the workspace-aware Docker supervisor (#926).
//!
//! The design lives on #926 (v2 comment). This module grows one slice at a
//! time: identity, retention, leases and done signals are pure decisions
//! over injected facts; the registry persists generations and leases. Docker
//! I/O, the daemon service and the CLI come in later slices.

pub mod done;
pub mod identity;
pub mod lease;
pub mod registry;
pub mod retention;
