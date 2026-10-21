// The fixture crate's files live under `ui/crates/fixture/src/`, so the lint
// sees them at repo-relative `crates/fixture/src/...` paths. This root file
// itself is outside `crates/` and is not checked.
#![allow(dead_code, unused_imports)]

#[path = "crates/fixture/src/platform/mod.rs"]
mod platform;

#[path = "crates/fixture/src/outside.rs"]
mod outside;

#[path = "crates/fixture/src/platform/facade.rs"]
mod facade;

#[path = "crates/fixture/src/daemon/platform/windows/nested.rs"]
mod nested;

fn main() {}
