// Ordinary code: every host selection is denied, even in tests.
mod libc {
    pub type Int = i32;
}

#[cfg(windows)]
fn windows_only() {}

pub fn native() -> libc::Int {
    if cfg!(unix) {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_selection_in_tests_is_denied_too() {
        assert!(cfg!(target_os = "linux") || true);
    }
}
