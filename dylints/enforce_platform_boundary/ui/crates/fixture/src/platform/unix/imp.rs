// A concrete tree: host keys and native API paths are allowed.
mod libc {
    pub type Int = i32;
}

#[cfg(target_os = "linux")]
pub fn linux_only() {}

pub fn native() -> libc::Int {
    0
}
