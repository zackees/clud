//! Click-to-dismiss: filter SGR mouse reports aimed at the toast (#1189).
//!
//! clud never turns mouse tracking on itself — doing so steals text selection
//! and wheel scrollback from inline TUIs. The filter is only *armed* when the
//! child already enabled SGR mouse reporting (Claude Code's fullscreen mode
//! does) and a toast is visible. Then a left-button press inside the close
//! button, and its matching release, are swallowed; every other byte reaches
//! the child unchanged.

use super::text_tier::CellRect;

/// Longest SGR mouse report we will hold across a chunk boundary.
const MAX_PENDING: usize = 32;

#[derive(Debug, Default)]
pub struct MouseFilter {
    pending: Vec<u8>,
    swallowing_release: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Parse {
    /// Bytes at the front are not an SGR report; this many can pass through.
    NotReport(usize),
    /// A complete report of this length.
    Report {
        len: usize,
        button: u16,
        x: u16,
        y: u16,
        press: bool,
    },
    /// A prefix of a report; need more bytes.
    Incomplete,
}

impl MouseFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter one stdin chunk. `armed` is the close-button rectangle when the
    /// filter should act. Returns the bytes to forward and whether a dismiss
    /// click was consumed.
    pub fn process(&mut self, chunk: &[u8], armed: Option<CellRect>) -> (Vec<u8>, bool) {
        if armed.is_none() && self.pending.is_empty() && !self.swallowing_release {
            return (chunk.to_vec(), false);
        }
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(chunk);
        let mut out = Vec::with_capacity(input.len());
        let mut dismissed = false;
        let mut i = 0;
        while i < input.len() {
            if input[i] != 0x1b {
                out.push(input[i]);
                i += 1;
                continue;
            }
            match parse(&input[i..]) {
                Parse::NotReport(n) => {
                    out.extend_from_slice(&input[i..i + n]);
                    i += n;
                }
                Parse::Incomplete => {
                    if input.len() - i <= MAX_PENDING {
                        self.pending = input[i..].to_vec();
                    } else {
                        out.extend_from_slice(&input[i..]);
                    }
                    return (out, dismissed);
                }
                Parse::Report {
                    len,
                    button,
                    x,
                    y,
                    press,
                } => {
                    let left_press = press && button & 0b11 == 0 && button & (32 | 64) == 0;
                    let hit = armed.is_some_and(|rect| rect.contains_1based(x, y));
                    if left_press && hit {
                        dismissed = true;
                        self.swallowing_release = true;
                    } else if !press && self.swallowing_release {
                        self.swallowing_release = false;
                    } else {
                        out.extend_from_slice(&input[i..i + len]);
                    }
                    i += len;
                }
            }
        }
        (out, dismissed)
    }

    /// Release any held partial report (called when stdin goes idle so a lone
    /// ESC keypress is never delayed for more than one idle poll).
    pub fn flush_pending(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

/// Parse an SGR mouse report `ESC [ < b ; x ; y (M|m)` at the start of `bytes`.
fn parse(bytes: &[u8]) -> Parse {
    const PREFIX: &[u8] = b"\x1b[<";
    let prefix_len = PREFIX.len().min(bytes.len());
    if bytes[..prefix_len] != PREFIX[..prefix_len] {
        // ESC followed by something else: forward ESC alone; the rest is
        // re-examined byte by byte.
        return Parse::NotReport(1);
    }
    if bytes.len() < PREFIX.len() {
        return Parse::Incomplete;
    }
    let mut fields = [0u32; 3];
    let mut field = 0;
    let mut digits = 0;
    for (offset, &b) in bytes[PREFIX.len()..].iter().enumerate() {
        match b {
            b'0'..=b'9' => {
                fields[field] = fields[field]
                    .saturating_mul(10)
                    .saturating_add(u32::from(b - b'0'));
                digits += 1;
            }
            b';' if field < 2 && digits > 0 => {
                field += 1;
                digits = 0;
            }
            b'M' | b'm' if field == 2 && digits > 0 => {
                let clamp = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
                return Parse::Report {
                    len: PREFIX.len() + offset + 1,
                    button: clamp(fields[0]),
                    x: clamp(fields[1]),
                    y: clamp(fields[2]),
                    press: b == b'M',
                };
            }
            _ => return Parse::NotReport(PREFIX.len() + offset),
        }
    }
    Parse::Incomplete
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLOSE: CellRect = CellRect {
        row: 0,
        col: 70,
        width: 3,
        height: 1,
    };

    #[test]
    fn unarmed_filter_is_a_byte_exact_pass_through() {
        let mut f = MouseFilter::new();
        let input = b"typed\x1b[<0;71;1Mtext\x1b";
        assert_eq!(f.process(input, None), (input.to_vec(), false));
    }

    #[test]
    fn a_click_on_the_close_button_is_swallowed_with_its_release() {
        let mut f = MouseFilter::new();
        let (out, dismissed) = f.process(b"a\x1b[<0;71;1Mb\x1b[<0;71;1mc", Some(CLOSE));
        assert!(dismissed);
        assert_eq!(out, b"abc");
    }

    #[test]
    fn clicks_elsewhere_and_other_buttons_reach_the_child() {
        let mut f = MouseFilter::new();
        let input = b"\x1b[<0;10;5M\x1b[<0;10;5m\x1b[<2;71;1M\x1b[<64;71;1M\x1b[<32;71;1M";
        let (out, dismissed) = f.process(input, Some(CLOSE));
        assert!(!dismissed);
        assert_eq!(out, input.to_vec());
    }

    #[test]
    fn a_report_split_across_chunks_is_reassembled() {
        let mut f = MouseFilter::new();
        let (out, dismissed) = f.process(b"x\x1b[<0;7", Some(CLOSE));
        assert_eq!(out, b"x");
        assert!(!dismissed);
        let (out, dismissed) = f.process(b"1;1M", Some(CLOSE));
        assert!(dismissed);
        assert!(out.is_empty());
        let (out, _) = f.process(b"\x1b[<0;71;1m", None);
        assert!(
            out.is_empty(),
            "release after dismiss is swallowed even if disarmed"
        );
    }

    #[test]
    fn keyboard_escape_sequences_pass_through_unchanged() {
        let mut f = MouseFilter::new();
        let input = b"\x1b[A\x1bOP\x1b[99;5u\x1b[200~paste\x1b[201~";
        let (out, dismissed) = f.process(input, Some(CLOSE));
        assert!(!dismissed);
        assert_eq!(out, input.to_vec());
    }

    #[test]
    fn a_lone_trailing_escape_is_held_then_flushed_on_idle() {
        let mut f = MouseFilter::new();
        let (out, _) = f.process(b"q\x1b", Some(CLOSE));
        assert_eq!(out, b"q");
        assert_eq!(f.flush_pending(), b"\x1b");
        assert!(f.flush_pending().is_empty());
    }

    #[test]
    fn malformed_reports_are_forwarded() {
        let mut f = MouseFilter::new();
        let input = b"\x1b[<0;;1M\x1b[<a";
        let (out, dismissed) = f.process(input, Some(CLOSE));
        assert!(!dismissed);
        assert_eq!(out, input.to_vec());
    }
}
