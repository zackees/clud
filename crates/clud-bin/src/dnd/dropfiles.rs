//! Parser for the Win32 OLE `DROPFILES` wire format.
//!
//! When a window accepts a `CF_HDROP` clipboard format from an OLE drop,
//! the payload is laid out as:
//!
//! ```text
//! struct DROPFILES {
//!     DWORD pFiles;   // offset of the first file path (usually 20)
//!     POINT pt;       // drop point (x, y), 8 bytes
//!     BOOL  fNC;      // 4 bytes — non-client area drop
//!     BOOL  fWide;    // 4 bytes — TRUE → UTF-16, FALSE → ANSI
//! }                   // 20 bytes total
//! // …followed by either UTF-16 LE or ANSI strings, each null-terminated,
//! //   with a final extra null after the last string (double-null term).
//! ```
//!
//! This module exposes [`parse_dropfiles_buffer`] — a pure `&[u8]` →
//! `Vec<String>` transform that decodes the wire format into a list of
//! file paths. The function never panics on malformed input; it returns
//! whatever paths it could recover (possibly empty).
//!
//! The parser is intentionally platform-agnostic — no `windows-rs`
//! types, no Windows-only `cfg`. The byte layout is the same on every
//! host, so the unit tests run everywhere even though only Windows ever
//! produces a real `DROPFILES` buffer at runtime.

/// Byte offset within `DROPFILES` of the `pFiles` field. (Field 1, no padding.)
pub const DROPFILES_PFILES_OFFSET: usize = 0;
/// Byte offset of the `fWide` field. (After `pFiles` (4) + `pt` (8) + `fNC` (4).)
pub const DROPFILES_FWIDE_OFFSET: usize = 16;
/// Minimum valid `DROPFILES` header size.
pub const DROPFILES_HEADER_SIZE: usize = 20;

/// Parse a `CF_HDROP` payload into the list of file paths it carries.
///
/// Returns an empty vec if the buffer is too short, the `pFiles` offset
/// is bogus, or the encoding flag points past the end. Partial parses
/// return whatever full paths fit before the buffer was truncated.
pub fn parse_dropfiles_buffer(buf: &[u8]) -> Vec<String> {
    if buf.len() < DROPFILES_HEADER_SIZE {
        return Vec::new();
    }

    let pfiles = read_u32_le(&buf[DROPFILES_PFILES_OFFSET..]) as usize;
    let fwide = read_u32_le(&buf[DROPFILES_FWIDE_OFFSET..]) != 0;

    if pfiles < DROPFILES_HEADER_SIZE || pfiles > buf.len() {
        return Vec::new();
    }

    let body = &buf[pfiles..];
    if fwide {
        parse_wide_paths(body)
    } else {
        parse_narrow_paths(body)
    }
}

fn read_u32_le(slice: &[u8]) -> u32 {
    debug_assert!(slice.len() >= 4);
    u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]])
}

fn parse_wide_paths(body: &[u8]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut current: Vec<u16> = Vec::new();
    let mut i = 0;
    while i + 1 < body.len() {
        let unit = u16::from_le_bytes([body[i], body[i + 1]]);
        i += 2;
        if unit == 0 {
            if current.is_empty() {
                // Two consecutive nulls: end-of-list marker.
                break;
            }
            // Lossy on lone surrogates / invalid UTF-16, matching the
            // forgiving spirit of OS shells when displaying drop targets.
            paths.push(String::from_utf16_lossy(&current));
            current.clear();
        } else {
            current.push(unit);
        }
    }
    paths
}

fn parse_narrow_paths(body: &[u8]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    for &byte in body {
        if byte == 0 {
            if current.is_empty() {
                break;
            }
            paths.push(String::from_utf8_lossy(&current).into_owned());
            current.clear();
        } else {
            current.push(byte);
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a wide-character `DROPFILES` payload from a slice of paths.
    /// Mirrors what Explorer / OLE actually emits on the wire.
    fn make_dropfiles_wide(paths: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(DROPFILES_HEADER_SIZE as u32).to_le_bytes()); // pFiles
        out.extend_from_slice(&0i32.to_le_bytes()); // pt.x
        out.extend_from_slice(&0i32.to_le_bytes()); // pt.y
        out.extend_from_slice(&0u32.to_le_bytes()); // fNC = FALSE
        out.extend_from_slice(&1u32.to_le_bytes()); // fWide = TRUE
        for path in paths {
            for unit in path.encode_utf16() {
                out.extend_from_slice(&unit.to_le_bytes());
            }
            out.extend_from_slice(&0u16.to_le_bytes()); // string terminator
        }
        out.extend_from_slice(&0u16.to_le_bytes()); // double-null end-of-list
        out
    }

    /// Build an ANSI (single-byte) `DROPFILES` payload. Used to verify
    /// that we honor `fWide = FALSE` for legacy drag sources.
    fn make_dropfiles_narrow(paths: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(DROPFILES_HEADER_SIZE as u32).to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // fWide = FALSE
        for path in paths {
            out.extend_from_slice(path.as_bytes());
            out.push(0);
        }
        out.push(0); // double-null end-of-list
        out
    }

    // ─── Wide (UTF-16) — the normal modern case ─────────────────────────────

    #[test]
    fn single_wide_ascii_path() {
        let bytes = make_dropfiles_wide(&[r"C:\test\file.txt"]);
        assert_eq!(parse_dropfiles_buffer(&bytes), vec![r"C:\test\file.txt"]);
    }

    #[test]
    fn multiple_wide_paths_round_trip() {
        let inputs = vec![
            r"C:\a.txt",
            r"C:\Users\zach\report.pdf",
            r"D:\tmp\stuff.bin",
        ];
        let bytes = make_dropfiles_wide(&inputs);
        let parsed = parse_dropfiles_buffer(&bytes);
        assert_eq!(parsed, inputs);
    }

    #[test]
    fn wide_path_with_non_ascii_characters() {
        // BMP + supplementary-plane (emoji) — both must survive UTF-16 decode.
        let path = r"C:\Пользователи\ザック\🚀.txt";
        let bytes = make_dropfiles_wide(&[path]);
        assert_eq!(parse_dropfiles_buffer(&bytes), vec![path]);
    }

    #[test]
    fn wide_path_with_spaces_is_unchanged() {
        let path = r"C:\Program Files\My App\config.json";
        let bytes = make_dropfiles_wide(&[path]);
        assert_eq!(parse_dropfiles_buffer(&bytes), vec![path]);
    }

    // ─── Narrow (ANSI) — legacy sources ─────────────────────────────────────

    #[test]
    fn single_narrow_ascii_path() {
        let bytes = make_dropfiles_narrow(&[r"C:\test\file.txt"]);
        assert_eq!(parse_dropfiles_buffer(&bytes), vec![r"C:\test\file.txt"]);
    }

    #[test]
    fn multiple_narrow_paths() {
        let inputs = vec![r"C:\a.txt", r"C:\b.txt"];
        let bytes = make_dropfiles_narrow(&inputs);
        assert_eq!(parse_dropfiles_buffer(&bytes), inputs);
    }

    // ─── Header that uses a non-default pFiles offset ───────────────────────

    #[test]
    fn pfiles_offset_with_padding_is_honored() {
        // Spec allows pFiles > sizeof(DROPFILES). Stuff 8 bytes of padding
        // between the header and the path list and verify we follow pFiles.
        let mut bytes = Vec::new();
        let pfiles = (DROPFILES_HEADER_SIZE + 8) as u32;
        bytes.extend_from_slice(&pfiles.to_le_bytes());
        bytes.extend_from_slice(&0i32.to_le_bytes());
        bytes.extend_from_slice(&0i32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes()); // fWide = TRUE
        bytes.extend_from_slice(&[0xAA; 8]); // 8 bytes of garbage padding
        for unit in r"C:\x.txt".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        assert_eq!(parse_dropfiles_buffer(&bytes), vec![r"C:\x.txt"]);
    }

    // ─── Malformed input must NOT panic ─────────────────────────────────────

    #[test]
    fn empty_buffer_yields_no_paths() {
        assert!(parse_dropfiles_buffer(&[]).is_empty());
    }

    #[test]
    fn truncated_header_yields_no_paths() {
        let bytes = vec![0u8; DROPFILES_HEADER_SIZE - 1];
        assert!(parse_dropfiles_buffer(&bytes).is_empty());
    }

    #[test]
    fn pfiles_offset_past_end_yields_no_paths() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9999u32.to_le_bytes()); // pFiles past EOF
        bytes.extend_from_slice(&[0u8; DROPFILES_HEADER_SIZE - 4]);
        assert!(parse_dropfiles_buffer(&bytes).is_empty());
    }

    #[test]
    fn missing_terminator_returns_collected_paths_so_far() {
        // First path is properly null-terminated; second is truncated mid-name.
        // We expect to recover the first and stop cleanly at the truncation
        // rather than panic or return a partial second string.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(DROPFILES_HEADER_SIZE as u32).to_le_bytes());
        bytes.extend_from_slice(&0i32.to_le_bytes());
        bytes.extend_from_slice(&0i32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes()); // fWide = TRUE
        for unit in r"C:\good.txt".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&0u16.to_le_bytes()); // terminator after first
        for unit in r"C:\bad".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        // …no trailing null, no double-null. Buffer ends mid-path.
        let parsed = parse_dropfiles_buffer(&bytes);
        assert_eq!(parsed, vec![r"C:\good.txt"]);
    }

    #[test]
    fn double_null_immediately_after_header_yields_no_paths() {
        // A well-formed but empty drop list — header followed straight by the
        // double-null terminator. Should parse cleanly to zero paths.
        let bytes = make_dropfiles_wide(&[]);
        assert!(parse_dropfiles_buffer(&bytes).is_empty());
    }
}
