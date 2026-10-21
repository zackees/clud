//! Windows half of `crate::command::grind_scripts`.

/// Windows has no execute bit.
pub fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}
