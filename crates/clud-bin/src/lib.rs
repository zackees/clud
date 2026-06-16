//! Library surface for the `clud` binary crate. Exposed so integration tests
//! under `tests/` can link against internals (notably `session::run_raw_pty_pump`
//! and `session::F3Observer`). The production binary in `src/main.rs` imports
//! modules from this library rather than declaring its own `mod ...` copies,
//! so there is exactly one instance of each module in the build.

pub mod args;
pub mod backend;
pub mod backend_bootstrap;
pub mod capture;
pub mod clud_settings;
pub mod codex_hook_normalize;
pub mod command;
pub mod console_input;
pub mod console_setup;
pub mod console_title;
pub mod ctrl_c_track;
pub mod daemon;
pub mod dnd;
pub mod gc;
pub mod graphics;
pub mod hook_health;
pub mod large_file_guard;
pub mod launch_setup;
pub mod loop_artifacts;
pub mod loop_check;
pub mod loop_spec;
pub mod optimize;
pub mod orphan_reaper;
pub mod paste_image;
pub mod process_tree;
pub mod runner;
pub mod runtime_cache;
pub mod session;
pub mod session_registry;
pub mod skill_install;
pub mod skills;
pub mod startup;
pub mod stream_json;
pub mod subprocess;
pub mod trampoline;
pub mod trash;
pub mod ui;
pub mod verbose_log;
pub mod voice;
pub mod wasm;
pub mod win_creation_flags;
pub mod worktrees;
