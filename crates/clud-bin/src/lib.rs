//! Library surface for the `clud` binary crate. Exposed so integration tests
//! under `tests/` can link against internals (notably `session::run_raw_pty_pump`
//! and `session::F3Observer`). The production binary in `src/main.rs` imports
//! modules from this library rather than declaring its own `mod ...` copies,
//! so there is exactly one instance of each module in the build.

pub mod args;
pub mod backend;
pub mod capture;
pub mod command;
pub mod daemon;
pub mod dnd;
pub mod loop_spec;
pub mod session;
pub mod session_registry;
pub mod subprocess;
pub mod trampoline;
pub mod voice;
pub mod wasm;
pub mod win_creation_flags;
