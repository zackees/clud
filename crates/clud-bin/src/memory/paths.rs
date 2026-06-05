use std::path::PathBuf;

use crate::daemon::default_state_dir;

pub fn memory_dir() -> std::io::Result<PathBuf> {
    Ok(default_state_dir()?.join("memory"))
}

pub fn memory_db_path() -> std::io::Result<PathBuf> {
    Ok(memory_dir()?.join("memory.db"))
}

pub fn tantivy_dir() -> std::io::Result<PathBuf> {
    Ok(memory_dir()?.join("tantivy"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The path helpers compose off `default_state_dir`, which itself reads
    // `CLUD_DAEMON_STATE_DIR`. Set that to a temp dir so the test does not
    // depend on or pollute the real per-user state dir.
    #[test]
    fn paths_compose_off_state_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: tests in this crate run single-threaded for env writes; the
        // env var is a documented part of the daemon paths API.
        unsafe {
            std::env::set_var("CLUD_DAEMON_STATE_DIR", tmp.path());
        }
        let mem = memory_dir().unwrap();
        let db = memory_db_path().unwrap();
        let tv = tantivy_dir().unwrap();
        assert_eq!(mem, tmp.path().join("memory"));
        assert_eq!(db, mem.join("memory.db"));
        assert_eq!(tv, mem.join("tantivy"));
        unsafe {
            std::env::remove_var("CLUD_DAEMON_STATE_DIR");
        }
    }
}
