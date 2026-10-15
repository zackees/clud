//! Windows half of `spawn_detached`: no console, its own process group, but
//! still inside whatever Job Object the launcher belongs to.

use std::os::windows::process::CommandExt;
use std::process::Command;

pub fn detach(command: &mut Command) {
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}
