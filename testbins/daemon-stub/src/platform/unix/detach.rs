//! Unix half of `spawn_detached`: `setsid()` in the child, making it a
//! session leader.

use std::os::unix::process::CommandExt;
use std::process::Command;

pub fn detach(command: &mut Command) {
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}
