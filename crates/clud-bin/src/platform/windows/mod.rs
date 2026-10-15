//! Windows implementations of the `crate::platform` areas.

pub mod block_bad_cmd_io;
pub mod block_bad_cmd_repeat;
pub mod config;
#[cfg(test)]
mod foreground_runtime;
#[cfg(test)]
mod grind_facts;
pub mod grind_scripts;
#[cfg(test)]
mod hook_health;
pub mod host;
#[cfg(test)]
pub mod large_file_guard;
pub mod log_event;
pub mod optimize;
pub mod runtime_cache;
pub mod subprocess;
