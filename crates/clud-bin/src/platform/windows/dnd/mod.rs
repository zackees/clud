//! Windows drag-and-drop for [`crate::dnd`]: the OLE `IDropTarget`
//! registration (`console_drop_target`) and the `WriteConsoleInputW`
//! subprocess injector (`injectors`). The basenames match the neutral
//! `dnd/` modules they implement.

mod console_drop_target;
mod injectors;

pub use console_drop_target::{pump_ole_messages, register_console_drop_target, wait_for_messages};
pub use injectors::{subprocess_console_injector, write_to_console_input};
