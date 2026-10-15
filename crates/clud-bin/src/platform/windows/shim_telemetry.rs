//! Windows half of `crate::shim_telemetry`'s parent-process lookup.

/// Always `None`: the parent's name is not one cheap file read here.
pub fn parent_name() -> Option<String> {
    None
}
