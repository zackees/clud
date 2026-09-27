//! Refuse agent-authored changes to deletion command resolution and scope.
//!
//! The child shim itself checks catastrophic expanded operands. Agent-authored
//! deletion is rewritten to the table's safe tool; there is no need to prove
//! which executable a hypothetical shell `rm` would have resolved to.

pub(super) fn check(command: &str, _path_env: &str) -> Result<(), String> {
    if super::block_bad_cmd_rm_redirect::changes_deletion_environment(command) {
        Err("deletion commands may not change PATH, CLUD_RM_ROOTS or CLUD_RM_ROLE".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rm_environment_assignments_are_refused_without_binary_identity_checks() {
        for command in [
            "PATH=/usr/bin safe-rm x",
            "export CLUD_RM_ROOTS=/tmp; safe-rm x",
            "CLUD_RM_ROLE=user safe-rm x",
        ] {
            assert!(check(command, "").is_err(), "{command}");
        }
        assert!(check("safe-rm x", "").is_ok());
    }
}
