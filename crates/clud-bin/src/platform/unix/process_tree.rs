//! Unix half of `crate::process_tree`.

/// No-op: POSIX has no `CREATE_NEW_PROCESS_GROUP` concept and clud's
/// foreground process group already receives the terminal's SIGINT directly.
pub fn try_break_group(pid: u32) -> bool {
    let _ = pid;
    false
}

#[cfg(test)]
mod tests {
    use crate::process_tree::kill_tree;
    use std::time::Duration;

    #[test]
    fn kill_tree_terminates_real_descendant_on_unix() {
        // Spawn `sh -c 'sleep 30'`. The shell is the parent of `sleep`,
        // so killing the tree must SIGKILL both. We check the sh process
        // is reaped within 5s.
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 30"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sh -c sleep 30");
        let pid = child.id();

        // Let sh spawn its sleep grandchild.
        std::thread::sleep(Duration::from_millis(200));

        let start = std::time::Instant::now();
        kill_tree(pid);

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        panic!("sh survived kill_tree for >5s");
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("try_wait failed: {e}"),
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "kill_tree took too long: {:?}",
            start.elapsed()
        );
    }
}
