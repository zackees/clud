//! Unix (Linux and macOS) implementations of the `crate::platform` areas.

pub mod alias_link;
pub mod host;
pub mod self_install;
pub mod self_install_activation;
pub mod shim_fs;
pub mod shim_main;
pub mod shim_telemetry;
pub mod subprocess;
