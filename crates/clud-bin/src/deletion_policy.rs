//! Shared normalized deletion-base policy for source analysis and the rm shim.

pub fn unsafe_delete_base_reason(value: &str) -> Option<&'static str> {
    if value.is_empty() {
        return Some("its value is empty");
    }
    if value.contains(['\0', '\n', '\r']) {
        return Some("its value contains control characters");
    }
    if value.contains('\\') {
        return Some("its POSIX value contains a literal backslash");
    }
    let normalized = value.to_string();
    let trimmed = normalized.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." || trimmed.starts_with("../") {
        return Some("its normalized value is a filesystem root or relative escape");
    }
    if normalized.starts_with("//") {
        let Some(components) = normalized_component_count(trimmed.trim_start_matches('/')) else {
            return Some("its normalized value escapes its UNC root");
        };
        if components <= 2 {
            return Some("its normalized value is a UNC root or share root");
        }
    } else if normalized.len() >= 3
        && normalized.starts_with('/')
        && normalized.as_bytes()[1].is_ascii_alphabetic()
        && normalized.as_bytes()[2] == b'/'
    {
        let Some(components) = normalized_component_count(
            normalized
                .get(3..)
                .unwrap_or_default()
                .trim_end_matches('/'),
        ) else {
            return Some("its normalized value escapes the MSYS drive root");
        };
        if components <= 1 {
            return Some("its normalized value is an MSYS drive root or top-level directory");
        }
    } else if normalized.starts_with('/') {
        let Some(components) = normalized_component_count(trimmed.trim_start_matches('/')) else {
            return Some("its normalized value escapes the filesystem root");
        };
        if components <= 1 {
            return Some("its normalized value is a filesystem root or top-level directory");
        }
    } else if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        if !normalized.as_bytes()[0].is_ascii_alphabetic() || !normalized[2..].starts_with('/') {
            return Some("its Windows drive path is not absolute");
        }
        let Some(components) = normalized_component_count(
            normalized
                .get(3..)
                .unwrap_or_default()
                .trim_end_matches('/'),
        ) else {
            return Some("its normalized value escapes the drive root");
        };
        if components <= 1 {
            return Some("its normalized value is a drive root or top-level directory");
        }
    } else if normalized_component_count(trimmed).is_none() {
        return Some("its normalized relative value escapes its working directory");
    }
    None
}

pub(crate) fn normalized_component_count(path: &str) -> Option<usize> {
    let mut depth = 0usize;
    for component in path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
    {
        if component == ".." {
            depth = depth.checked_sub(1)?;
        } else if component.contains(['*', '?', '[']) {
            // A glob can select descendants, but it cannot prove that the
            // deletion base itself is deeper than a protected root.
            continue;
        } else {
            depth += 1;
        }
    }
    Some(depth)
}
