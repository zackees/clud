//! Stream-resumable escape-sequence tracker for safe toast injection (#1189).
//!
//! The compositor may only write its own bytes into the terminal stream at a
//! point where the child is *between* sequences. Injecting inside a CSI, OSC,
//! DCS or APC string corrupts both, and injecting inside a synchronized-update
//! block (DECSET 2026, used by Claude Code and Codex) tears the child's frame.
//!
//! `vte`'s parser state is private, so this is a small dedicated state machine
//! in the style of `OscTitleStripper`. It also sniffs the few modes the
//! compositor must honour and that `vt100` does not expose: origin mode
//! (DECOM), the scroll region (DECSTBM), and the events that wipe kitty
//! graphics (erase display, reset, alternate-screen switches).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Ground,
    Esc,
    EscIntermediate,
    Csi,
    Osc,
    OscEsc,
    /// DCS / APC / SOS / PM payload, terminated by ST.
    Str,
    StrEsc,
}

/// Events seen since the last [`EscapeTracker::take_burst`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BurstFlags {
    /// `ED 2` / `ED 3`: kitty drops images; the toast must be re-sent.
    pub cleared: bool,
    /// `RIS` (ESC c).
    pub reset: bool,
    /// DECSET/DECRST 47, 1047 or 1049.
    pub alt_switched: bool,
}

impl BurstFlags {
    pub fn any(self) -> bool {
        self.cleared || self.reset || self.alt_switched
    }
}

const MAX_PARAM_BYTES: usize = 64;

#[derive(Debug, Clone, Default)]
pub struct EscapeTracker {
    state: State,
    params: Vec<u8>,
    utf8_remaining: u8,
    sync_active: bool,
    origin_mode: bool,
    scroll_region: Option<(u16, u16)>,
    burst: BurstFlags,
}

impl EscapeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.step(byte);
        }
    }

    /// Between sequences and not mid-UTF-8 character.
    pub fn at_ground(&self) -> bool {
        self.state == State::Ground && self.utf8_remaining == 0
    }

    /// Safe to inject right now: at ground and outside a synchronized update.
    pub fn is_safe(&self) -> bool {
        self.at_ground() && !self.sync_active
    }

    pub fn sync_active(&self) -> bool {
        self.sync_active
    }

    pub fn origin_mode(&self) -> bool {
        self.origin_mode
    }

    /// 1-based `(top, bottom)` as last set by DECSTBM; `None` = full screen.
    pub fn scroll_region(&self) -> Option<(u16, u16)> {
        self.scroll_region
    }

    pub fn take_burst(&mut self) -> BurstFlags {
        std::mem::take(&mut self.burst)
    }

    fn step(&mut self, byte: u8) {
        // CAN and SUB abort any sequence in every state.
        if byte == 0x18 || byte == 0x1a {
            self.state = State::Ground;
            self.params.clear();
            return;
        }
        match self.state {
            State::Ground => self.ground(byte),
            State::Esc => self.esc(byte),
            State::EscIntermediate => {
                if !(0x20..=0x2f).contains(&byte) {
                    self.state = State::Ground;
                }
            }
            State::Csi => self.csi(byte),
            State::Osc => match byte {
                0x07 => self.state = State::Ground,
                0x1b => self.state = State::OscEsc,
                _ => {}
            },
            State::OscEsc => {
                if byte == b'\\' {
                    self.state = State::Ground;
                } else {
                    // An unterminated OSC followed by a new sequence.
                    self.state = State::Esc;
                    self.esc(byte);
                }
            }
            State::Str => {
                if byte == 0x1b {
                    self.state = State::StrEsc;
                }
            }
            State::StrEsc => {
                self.state = if byte == b'\\' {
                    State::Ground
                } else {
                    State::Str
                };
            }
        }
    }

    fn ground(&mut self, byte: u8) {
        if byte == 0x1b {
            self.utf8_remaining = 0;
            self.state = State::Esc;
            return;
        }
        match byte {
            0x80..=0xbf => self.utf8_remaining = self.utf8_remaining.saturating_sub(1),
            0xc0..=0xdf => self.utf8_remaining = 1,
            0xe0..=0xef => self.utf8_remaining = 2,
            0xf0..=0xf7 => self.utf8_remaining = 3,
            _ => self.utf8_remaining = 0,
        }
    }

    fn esc(&mut self, byte: u8) {
        self.state = match byte {
            b'[' => {
                self.params.clear();
                State::Csi
            }
            b']' => State::Osc,
            b'P' | b'_' | b'^' | b'X' => State::Str,
            b'c' => {
                self.burst.reset = true;
                self.origin_mode = false;
                self.scroll_region = None;
                self.sync_active = false;
                State::Ground
            }
            0x1b => State::Esc,
            0x20..=0x2f => State::EscIntermediate,
            _ => State::Ground,
        };
    }

    fn csi(&mut self, byte: u8) {
        match byte {
            // Parameter bytes (0x30..=0x3f) and intermediates (0x20..=0x2f).
            0x20..=0x3f => {
                if self.params.len() < MAX_PARAM_BYTES {
                    self.params.push(byte);
                }
            }
            0x40..=0x7e => {
                self.dispatch_csi(byte);
                self.params.clear();
                self.state = State::Ground;
            }
            0x1b => {
                self.params.clear();
                self.state = State::Esc;
            }
            // C0 controls execute without leaving the sequence.
            _ => {}
        }
    }

    fn dispatch_csi(&mut self, final_byte: u8) {
        let private = self.params.first() == Some(&b'?');
        let body = if private {
            &self.params[1..]
        } else {
            &self.params[..]
        };
        if body.iter().any(|b| (0x20..=0x2f).contains(b)) {
            // Intermediates mean a different command family (e.g. DECSCUSR).
            return;
        }
        let numbers: Vec<Option<u16>> = body
            .split(|b| *b == b';')
            .map(|part| std::str::from_utf8(part).ok()?.parse::<u16>().ok())
            .collect();
        match (private, final_byte) {
            (true, b'h') | (true, b'l') => {
                let set = final_byte == b'h';
                for mode in numbers.into_iter().flatten() {
                    match mode {
                        2026 => self.sync_active = set,
                        6 => self.origin_mode = set,
                        47 | 1047 | 1049 => self.burst.alt_switched = true,
                        _ => {}
                    }
                }
            }
            (false, b'r') => {
                let top = numbers.first().copied().flatten();
                let bottom = numbers.get(1).copied().flatten();
                self.scroll_region = match (top, bottom) {
                    (Some(t), Some(b)) if t >= 1 && b > t => Some((t, b)),
                    _ => None,
                };
            }
            (false, b'J') => {
                if matches!(numbers.first().copied().flatten(), Some(2) | Some(3)) {
                    self.burst.cleared = true;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_safe() {
        let mut t = EscapeTracker::new();
        t.feed(b"hello world\r\n");
        assert!(t.is_safe());
    }

    #[test]
    fn a_csi_split_across_chunks_is_unsafe_until_its_final_byte() {
        let mut t = EscapeTracker::new();
        t.feed(b"abc\x1b[3");
        assert!(!t.is_safe());
        t.feed(b"8;5;20");
        assert!(!t.is_safe());
        t.feed(b"8mX");
        assert!(t.is_safe());
    }

    #[test]
    fn osc_terminated_by_bel_or_st_returns_to_ground() {
        let mut t = EscapeTracker::new();
        t.feed(b"\x1b]0;title");
        assert!(!t.is_safe());
        t.feed(b"\x07");
        assert!(t.is_safe());
        t.feed(b"\x1b]8;;http://x\x1b");
        assert!(!t.is_safe());
        t.feed(b"\\");
        assert!(t.is_safe());
    }

    #[test]
    fn apc_and_dcs_payloads_are_unsafe_until_st_even_with_inner_escapes() {
        let mut t = EscapeTracker::new();
        t.feed(b"\x1b_Ga=t;AAAA");
        assert!(!t.is_safe());
        t.feed(b"\x1bX");
        assert!(
            !t.is_safe(),
            "ESC not followed by \\ stays inside the string"
        );
        t.feed(b"\x1b\\");
        assert!(t.is_safe());
        t.feed(b"\x1bPq#0;2;0;0;0");
        assert!(!t.is_safe());
        t.feed(b"\x1b\\");
        assert!(t.is_safe());
    }

    #[test]
    fn synchronized_update_blocks_are_unsafe_but_at_ground() {
        let mut t = EscapeTracker::new();
        t.feed(b"\x1b[?2026h frame");
        assert!(t.at_ground());
        assert!(t.sync_active());
        assert!(!t.is_safe());
        t.feed(b"\x1b[?2026l");
        assert!(t.is_safe());
    }

    #[test]
    fn a_utf8_character_split_across_chunks_is_unsafe() {
        let mut t = EscapeTracker::new();
        let dot = "·".as_bytes();
        t.feed(&dot[..1]);
        assert!(!t.is_safe());
        t.feed(&dot[1..]);
        assert!(t.is_safe());
        let wide = "漢".as_bytes();
        t.feed(&wide[..2]);
        assert!(!t.is_safe());
        t.feed(&wide[2..]);
        assert!(t.is_safe());
    }

    #[test]
    fn origin_mode_and_scroll_region_are_tracked() {
        let mut t = EscapeTracker::new();
        t.feed(b"\x1b[5;20r\x1b[?6h");
        assert!(t.origin_mode());
        assert_eq!(t.scroll_region(), Some((5, 20)));
        t.feed(b"\x1b[?6l\x1b[r");
        assert!(!t.origin_mode());
        assert_eq!(t.scroll_region(), None);
    }

    #[test]
    fn combined_private_modes_are_all_applied() {
        let mut t = EscapeTracker::new();
        t.feed(b"\x1b[?6;2026h");
        assert!(t.origin_mode());
        assert!(t.sync_active());
    }

    #[test]
    fn clears_resets_and_alt_switches_are_reported_once() {
        let mut t = EscapeTracker::new();
        t.feed(b"\x1b[2J");
        assert!(t.take_burst().cleared);
        assert!(!t.take_burst().any());
        t.feed(b"\x1b[?1049h");
        assert!(t.take_burst().alt_switched);
        t.feed(b"\x1bc");
        let flags = t.take_burst();
        assert!(flags.reset);
        t.feed(b"\x1b[J\x1b[1J");
        assert!(!t.take_burst().cleared, "ED 0 and ED 1 keep images");
    }

    #[test]
    fn decscusr_with_intermediate_does_not_look_like_a_mode_set() {
        let mut t = EscapeTracker::new();
        t.feed(b"\x1b[6 q");
        assert!(!t.origin_mode());
        assert!(t.is_safe());
    }

    #[test]
    fn cancel_aborts_a_sequence() {
        let mut t = EscapeTracker::new();
        t.feed(b"\x1b[12");
        t.feed(&[0x18]);
        assert!(t.is_safe());
    }
}
