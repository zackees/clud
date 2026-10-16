//! String-level path helpers.
//!
//! Paths stay strings: a command evaluated on Linux may name `C:\repo`, and
//! `std::path::Path` would read that as one file name. Windows style (either
//! separator, rendered with `\`) is used whenever either side has a drive
//! letter, a UNC prefix or a backslash; POSIX style otherwise.

/// The program a word names: lowercased, with any directory and extension
/// removed. Matches the scanner's long-standing `program_name`.
pub(super) fn program_name(word: &str) -> String {
    let cleaned = word.trim().trim_matches(['\'', '"']);
    crate::path_norm::file_stem_any_separator(cleaned)
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn has_drive(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn is_windows_style(path: &str) -> bool {
    has_drive(path) || path.contains('\\')
}

pub(super) fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || path.starts_with('\\') || has_drive(path)
}

/// `rel` resolved against `base`, with `.` and `..` collapsed lexically.
pub(super) fn join(base: &str, rel: &str) -> String {
    if is_absolute(rel) {
        return normalize(rel);
    }
    let windows = is_windows_style(base) || is_windows_style(rel);
    let separator = if windows { '\\' } else { '/' };
    normalize(&format!("{base}{separator}{rel}"))
}

/// Collapse `.`/`..` and repeated separators without touching the disk.
pub(super) fn normalize(path: &str) -> String {
    let windows = is_windows_style(path);
    let separator = if windows { "\\" } else { "/" };
    let (root, rest) = split_root(path);
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else if root.is_empty() {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let body = parts.join(separator);
    match (root.is_empty(), body.is_empty()) {
        (true, true) => ".".to_string(),
        (true, false) => body,
        (false, _) => format!("{root}{body}"),
    }
}

/// The root prefix (`/`, `\`, `C:\`, `C:`, `\\server\share\`) and the rest.
fn split_root(path: &str) -> (String, &str) {
    if has_drive(path) {
        let rest = &path[2..];
        let drive = &path[..2];
        return match rest.strip_prefix(['/', '\\']) {
            Some(after) => (format!("{drive}\\"), after),
            None => (drive.to_string(), rest),
        };
    }
    if path.starts_with('/') || path.starts_with('\\') {
        let separator = if path.starts_with('\\') { "\\" } else { "/" };
        return (separator.to_string(), &path[1..]);
    }
    (String::new(), path)
}

/// `~` and `~/rest` expanded against `home`; other words unchanged.
pub(super) fn expand_tilde(word: &str, home: Option<&str>) -> Option<String> {
    let home = home?;
    if word == "~" {
        return Some(home.to_string());
    }
    let rest = word
        .strip_prefix("~/")
        .or_else(|| word.strip_prefix("~\\"))?;
    Some(join(home, rest))
}
