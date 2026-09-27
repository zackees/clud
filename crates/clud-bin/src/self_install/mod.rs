//! Native self-installer components shared by the CLI and install transaction.

pub mod activation;
#[cfg(windows)]
pub(super) mod activation_windows;
pub mod catalog;
pub mod entry;
pub mod picker;
pub mod transaction;
