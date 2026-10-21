//! The Windows-only case of `crate::decision`'s tests (`decision_tests.rs`):
//! drive-root shapes, which `std::path` only parses on Windows.

use std::path::PathBuf;

use crate::decision::{classify, Decision};

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

/// A drive root is a filesystem root. Checked structurally, so the several
/// spellings all answer the same.
///
/// Windows-only, and that is a real constraint rather than test tidiness:
/// `std::path` only parses a `C:\` prefix on Windows, so on Unix the same
/// string is an ordinary relative path. `tap` inspects paths on the platform
/// it runs on, so this is sound -- but a cross-platform assertion here would
/// be testing the host's path parser, not the guard.
#[test]
fn a_windows_drive_root_is_a_filesystem_root() {
    let win_root = PathBuf::from(r"C:\srv\project");
    for target in [r"C:\", "C:/"] {
        let decision = classify(&argv(&["rm", "-rf", target]), &win_root, &win_root, None);
        match decision {
            Decision::Refuse(message) => {
                assert!(
                    message.contains("filesystem root") || message.contains("outside"),
                    "{target}: {message}"
                );
            }
            Decision::Exec => panic!("{target} must not be allowed"),
        }
    }
}
