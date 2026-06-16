use std::collections::BTreeSet;
use std::path::Path;

pub(in crate::hook_health) fn same_path(left: &Path, right: &Path) -> bool {
    if cfg!(target_os = "windows") {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

pub(in crate::hook_health) fn join_matchers(matchers: &BTreeSet<String>) -> String {
    matchers.iter().cloned().collect::<Vec<_>>().join(", ")
}

pub(in crate::hook_health) fn display_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}
