//! Drag-and-drop stubs for [`crate::dnd`]. POSIX terminals deliver a drop
//! as typed path text, so there is no OLE `IDropTarget` to register and no
//! console input buffer to write to.

use std::time::Duration;

use crate::dnd::console_drop_target::{
    ConsoleDropTargetGuard, DropInjector, RefreshConfig, RegisterError,
};

/// No-op: there is no OLE message queue to pump.
pub fn pump_ole_messages() {}

/// One `responsive_sleep` slice: a plain sleep.
pub fn wait_for_messages(step: Duration) {
    std::thread::sleep(step);
}

/// Always [`RegisterError::UnsupportedPlatform`]: the `IDropTarget`
/// subsystem is Windows-only.
pub fn register_console_drop_target(
    _injector: DropInjector,
    _config: RefreshConfig,
) -> Result<ConsoleDropTargetGuard, RegisterError> {
    Err(RegisterError::UnsupportedPlatform)
}

/// An injector that ignores every drop: there is no console input buffer.
pub fn subprocess_console_injector() -> DropInjector {
    Box::new(|_paths: &[String]| {})
}

/// Always [`std::io::ErrorKind::Unsupported`]: there is no console input
/// buffer.
pub fn write_to_console_input(_records_bytes: &[u8]) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the console input buffer exists only on Windows",
    ))
}
