//! Drag-and-drop path normalization for terminal-embedded clud sessions.
//!
//! When a user drags a file into the terminal window running `clud`, the
//! terminal injects a path-shaped byte sequence into stdin. Each terminal
//! uses a different convention:
//!
//! | Terminal           | Example (single dropped file)                          |
//! |--------------------|--------------------------------------------------------|
//! | cmd.exe            | `C:\Users\me\file.txt` (bare, or quoted if spaces)     |
//! | mintty / git-bash  | `/c/Users/me/file.txt` (MSYS POSIX translation)        |
//! | mintty (alt build) | `C:\\Users\\me\\file.txt` (double-escaped)             |
//! | PowerShell         | `& 'C:\path\file.txt'` (call operator prefix)          |
//! | macOS Terminal     | `/Users/me/my\ file.txt` (backslash-escaped spaces)    |
//! | iTerm2             | same as macOS Terminal                                 |
//! | GNOME Terminal     | `file:///home/me/my%20file.txt` (RFC 3986 URI)         |
//!
//! This module canonicalizes those forms so downstream consumers see a
//! plain, shell-unescaped absolute path. It does NOT itself read stdin or
//! inspect bytes in the PTY pump — it is a pure string transform intended
//! to be called from input-handling paths that have already collected a
//! complete line (paste buffer, slash-command argument, etc.).
//!
//! The heuristics here are conservative: strings that do not look like a
//! dragged path are returned unchanged, so it is safe to call this on
//! arbitrary user input.
//!
//! ## Submodules
//!
//! - [`dropfiles`] — pure-byte parser for the Win32 OLE `DROPFILES`
//!   structure. Used by the (Windows-only) `IDropTarget` adapter that
//!   intercepts drops before conhost can refuse them. See issue #66.

pub mod dropfiles;

/// Normalize a single dragged-path payload emitted by a terminal into a
/// plain filesystem path.
///
/// The function is cross-platform: on Windows it prefers Windows-style
/// output (`C:\...`); on POSIX it prefers POSIX-style (`/home/...`). When
/// the input does not look like a dragged path at all, the original string
/// is returned trimmed of trailing whitespace only.
pub fn normalize_dropped_path(input: &str) -> String {
    let mut s = input.trim().to_string();

    s = strip_powershell_call_prefix(&s);
    s = strip_matching_surrounding_quotes(&s);

    if let Some(decoded) = decode_file_uri(&s) {
        s = decoded;
    }

    s = unescape_shell_spaces(&s);

    #[cfg(windows)]
    {
        s = msys_to_windows_path(&s);
        s = collapse_double_backslashes(&s);
    }

    s
}

/// Detect whether `input` plausibly originated from a terminal drag-and-drop
/// event. A strict check: leading quote/URI-scheme, a Windows drive letter,
/// an MSYS `/c/` path, or a POSIX absolute path with a file extension.
///
/// Callers use this to decide whether to *invoke* the normalizer at all —
/// e.g. a slash command that accepts either a literal prompt or a dragged
/// file path.
pub fn looks_like_dropped_path(input: &str) -> bool {
    let s = input.trim();
    if s.is_empty() {
        return false;
    }

    if s.starts_with("file://") {
        return true;
    }

    let inner = strip_matching_surrounding_quotes(s);
    let inner = strip_powershell_call_prefix(&inner);
    let inner = strip_matching_surrounding_quotes(&inner);

    if looks_like_windows_drive_path(&inner) {
        return true;
    }
    if looks_like_msys_drive_path(&inner) {
        return true;
    }
    if is_posix_absolute_with_extension(&inner) {
        return true;
    }

    false
}

// ─── Helpers ──────────────────────────────────────────────────────────────

fn strip_matching_surrounding_quotes(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

fn strip_powershell_call_prefix(s: &str) -> String {
    // `& 'C:\path\file.txt'` — the call operator. Only meaningful when
    // followed by a quoted string.
    if let Some(rest) = s.strip_prefix("& ") {
        return rest.to_string();
    }
    if let Some(rest) = s.strip_prefix("&") {
        let rest = rest.trim_start();
        if rest.starts_with('\'') || rest.starts_with('"') {
            return rest.to_string();
        }
    }
    s.to_string()
}

fn decode_file_uri(s: &str) -> Option<String> {
    let rest = s.strip_prefix("file://")?;
    // Common forms:
    //   file:///home/me/x.txt     → /home/me/x.txt
    //   file:///C:/Users/me/x.txt → /C:/Users/me/x.txt (POSIX) or C:\... (Win)
    //   file://localhost/...      → strip the authority
    let without_host = if let Some(slash) = rest.find('/') {
        &rest[slash..]
    } else {
        rest
    };
    Some(percent_decode(without_host))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_digit(bytes[i + 1]);
            let lo = hex_digit(bytes[i + 2]);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(&e.into_bytes()).into_owned())
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Unescape `\<space>` → `<space>` as produced by macOS Terminal / iTerm2
/// drag-and-drop on POSIX. On Windows this is a no-op — backslash is a path
/// separator, not an escape.
fn unescape_shell_spaces(s: &str) -> String {
    #[cfg(windows)]
    {
        s.to_string()
    }
    #[cfg(not(windows))]
    {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b' ' {
                out.push(b' ');
                i += 2;
                continue;
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8(out).unwrap_or_default()
    }
}

/// `/c/Users/me/x.txt` → `C:\Users\me\x.txt`. Windows-only; on POSIX a
/// path starting with `/c/` is a legitimate ordinary directory.
#[cfg(windows)]
fn msys_to_windows_path(s: &str) -> String {
    // Accept `/<letter>/...` or `/<letter>:/...`. Require exactly one char
    // between the first two slashes so we don't mistake `/cache/foo` for a
    // drive path.
    // Accept file-URI residue like `/C:/Users/...` too.
    let bytes = s.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b'/' && bytes[1].is_ascii_alphabetic() {
        let drive = (bytes[1] as char).to_ascii_uppercase();
        let tail = &s[3..].replace('/', "\\");
        return format!("{}:\\{}", drive, tail);
    }
    if bytes.len() >= 4
        && bytes[0] == b'/'
        && bytes[2] == b':'
        && bytes[3] == b'/'
        && bytes[1].is_ascii_alphabetic()
    {
        let drive = (bytes[1] as char).to_ascii_uppercase();
        let tail = &s[4..].replace('/', "\\");
        return format!("{}:\\{}", drive, tail);
    }
    s.to_string()
}

/// Collapse `\\` inside paths to `\`. mintty sometimes emits shell-escaped
/// paths with doubled backslashes. Applied only after we already recognize
/// the string as a path (post quote-strip, post URI-decode).
#[cfg(windows)]
fn collapse_double_backslashes(s: &str) -> String {
    if !s.contains("\\\\") {
        return s.to_string();
    }
    // Preserve a leading UNC `\\server\share` form by skipping the first two
    // backslashes if the string starts with them.
    let (prefix, rest) = if let Some(stripped) = s.strip_prefix("\\\\") {
        ("\\\\", stripped)
    } else {
        ("", s)
    };
    let collapsed = rest.replace("\\\\", "\\");
    format!("{}{}", prefix, collapsed)
}

fn looks_like_windows_drive_path(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

fn looks_like_msys_drive_path(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b'/'
}

fn is_posix_absolute_with_extension(s: &str) -> bool {
    if !s.starts_with('/') {
        return false;
    }
    // Require a trailing filename with an extension — keeps "/path" as a
    // plausible prompt and "/usr/bin/env" as not-a-drop, but accepts
    // "/Users/me/file.txt".
    if let Some(last) = s.rsplit('/').next() {
        return last.contains('.') && !last.starts_with('.');
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── cmd.exe ────────────────────────────────────────────────────────────

    #[test]
    fn cmd_exe_bare_path_is_passthrough() {
        let dropped = r"C:\Users\niteris\file.txt";
        assert_eq!(
            normalize_dropped_path(dropped),
            r"C:\Users\niteris\file.txt"
        );
    }

    #[test]
    fn cmd_exe_quoted_path_strips_surrounding_quotes() {
        // cmd.exe wraps the path in double quotes when it contains a space.
        let dropped = "\"C:\\Users\\my name\\file.txt\"";
        assert_eq!(
            normalize_dropped_path(dropped),
            r"C:\Users\my name\file.txt"
        );
    }

    #[test]
    fn cmd_exe_trailing_space_is_trimmed() {
        // cmd.exe typically appends a single space after the dropped name so
        // the next command-line token starts fresh. We drop it.
        let dropped = "\"C:\\tmp\\file.txt\" ";
        assert_eq!(normalize_dropped_path(dropped), r"C:\tmp\file.txt");
    }

    // ─── mintty / git-bash ─────────────────────────────────────────────────

    #[test]
    fn mintty_msys_posix_path_converts_to_windows_on_windows() {
        let dropped = "/c/Users/niteris/file.txt";
        #[cfg(windows)]
        {
            assert_eq!(
                normalize_dropped_path(dropped),
                r"C:\Users\niteris\file.txt"
            );
        }
        #[cfg(not(windows))]
        {
            // On real POSIX this is just an ordinary absolute path, not MSYS
            // — leave it alone.
            assert_eq!(normalize_dropped_path(dropped), "/c/Users/niteris/file.txt");
        }
    }

    #[test]
    fn mintty_double_escaped_backslashes_collapse() {
        // Some mintty builds paste paths with doubled backslashes (shell-safe
        // form). On Windows we collapse them to a single separator.
        let dropped = "C:\\\\Users\\\\niteris\\\\file.txt";
        #[cfg(windows)]
        {
            assert_eq!(
                normalize_dropped_path(dropped),
                r"C:\Users\niteris\file.txt"
            );
        }
        #[cfg(not(windows))]
        {
            // On POSIX this is just an opaque string — don't rewrite.
            assert_eq!(
                normalize_dropped_path(dropped),
                "C:\\\\Users\\\\niteris\\\\file.txt"
            );
        }
    }

    // ─── PowerShell ────────────────────────────────────────────────────────

    #[test]
    fn powershell_call_operator_prefix_is_stripped() {
        let dropped = "& 'C:\\path\\file.txt'";
        assert_eq!(normalize_dropped_path(dropped), r"C:\path\file.txt");
    }

    // ─── macOS Terminal / iTerm2 ───────────────────────────────────────────

    #[test]
    fn macos_backslash_escaped_space_is_unescaped() {
        let dropped = "/Users/me/my\\ file.txt";
        #[cfg(not(windows))]
        {
            assert_eq!(normalize_dropped_path(dropped), "/Users/me/my file.txt");
        }
        #[cfg(windows)]
        {
            // On Windows the byte sequence `\ ` is not a shell escape — it
            // is a literal backslash followed by a space, which can appear
            // inside a Windows path. Leave it alone.
            assert_eq!(normalize_dropped_path(dropped), "/Users/me/my\\ file.txt");
        }
    }

    // ─── GNOME Terminal (file:// URI) ──────────────────────────────────────

    #[test]
    fn gnome_file_uri_is_decoded() {
        let dropped = "file:///home/me/my%20file.txt";
        assert_eq!(normalize_dropped_path(dropped), "/home/me/my file.txt");
    }

    #[test]
    fn windows_file_uri_is_decoded() {
        let dropped = "file:///C:/Users/me/my%20file.txt";
        #[cfg(windows)]
        {
            assert_eq!(normalize_dropped_path(dropped), r"C:\Users\me\my file.txt");
        }
        #[cfg(not(windows))]
        {
            assert_eq!(normalize_dropped_path(dropped), "/C:/Users/me/my file.txt");
        }
    }

    // ─── looks_like_dropped_path ───────────────────────────────────────────

    #[test]
    fn plain_prompt_text_is_not_a_path() {
        assert!(!looks_like_dropped_path("fix the authentication bug"));
        assert!(!looks_like_dropped_path(""));
        assert!(!looks_like_dropped_path("why does this fail?"));
    }

    #[test]
    fn file_uri_scheme_is_detected() {
        assert!(looks_like_dropped_path("file:///tmp/x.txt"));
    }

    #[test]
    fn quoted_drive_path_is_detected() {
        assert!(looks_like_dropped_path("\"C:\\tmp\\x.txt\""));
    }

    #[test]
    fn msys_drive_path_is_detected() {
        assert!(looks_like_dropped_path("/c/Users/me/x.txt"));
    }

    #[test]
    fn posix_absolute_with_extension_is_detected() {
        assert!(looks_like_dropped_path("/Users/me/x.txt"));
        assert!(looks_like_dropped_path("/home/me/script.sh"));
    }
}
