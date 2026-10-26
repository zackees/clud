//! dockhand: the workspace-aware Docker supervisor (#926).
//!
//! The design lives on #926 (v2 comment). This module grows one slice at a
//! time; today it holds only the pure identity layer every later slice keys
//! on: canonical workspace roots, workspace ids, label sets and resource
//! names. Nothing here talks to Docker or the filesystem.

pub mod identity;
pub mod retention;
