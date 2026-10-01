//! Keep escape sequences whole on their way into the child PTY (#1697).
//!
//! ConPTY's input parser treats the end of every write as the end of the
//! input: a CSI still open there is flushed, so a sequence split across two
//! writes reaches the child broken. Under VT input the Windows console reader
//! (`console_input.rs`) delivers a terminal's SGR mouse report one character
//! per event, and the pump wrote each event separately, so every mouse
//! movement lost its `ESC [ <` and arrived as literal `35;31;18M` text.
//!
//! The gate holds an incomplete trailing sequence until its final byte
//! arrives. The pump's idle flush releases it after
//! [`super::INPUT_PENDING_FLUSH`], so a lone Esc keypress is never stuck.

/// Longest incomplete sequence held. A longer one is malformed: release it
/// rather than buffer without bound.
const MAX_HELD: usize = 64;

#[derive(Debug, Default)]
pub(crate) struct EscapeSequenceGate {
    held: Vec<u8>,
}

impl EscapeSequenceGate {
    /// Prepend any held bytes to `bytes` and return what can be written now:
    /// everything up to an incomplete trailing escape sequence, which is held.
    pub(crate) fn push(&mut self, bytes: Vec<u8>) -> Vec<u8> {
        let mut out = std::mem::take(&mut self.held);
        out.extend(bytes);
        if let Some(start) = incomplete_tail_start(&out) {
            if out.len() - start <= MAX_HELD {
                self.held = out.split_off(start);
            }
        }
        out
    }

    pub(crate) fn has_pending(&self) -> bool {
        !self.held.is_empty()
    }

    /// Release the held bytes as they are.
    pub(crate) fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.held)
    }
}

/// Where a trailing escape sequence still waiting for bytes starts: a lone
/// ESC, `ESC O` (SS3), or `ESC [` with only parameter and intermediate bytes
/// (`0x20..=0x3F`) after it. Any other byte after ESC completes it (Alt+key).
fn incomplete_tail_start(bytes: &[u8]) -> Option<usize> {
    let esc = bytes.iter().rposition(|&b| b == 0x1b)?;
    let open = match &bytes[esc + 1..] {
        [] | [b'O'] => true,
        [b'[', rest @ ..] => rest.iter().all(|b| (0x20..=0x3f).contains(b)),
        _ => false,
    };
    open.then_some(esc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_fed_one_byte_at_a_time_leaves_whole() {
        let mut gate = EscapeSequenceGate::default();
        let report = b"\x1b[<35;31;18M";
        let mut writes = Vec::new();
        for &b in report {
            let out = gate.push(vec![b]);
            if !out.is_empty() {
                writes.push(out);
            }
        }
        assert_eq!(writes, vec![report.to_vec()]);
        assert!(!gate.has_pending());
    }

    #[test]
    fn complete_sequences_and_text_pass_straight_through() {
        let mut gate = EscapeSequenceGate::default();
        let input = b"ab\x1b[A\x1bOP\x1b\r\x1bx\x1b[13;2u\x1b[<0;1;1m".to_vec();
        assert_eq!(gate.push(input.clone()), input);
        assert!(!gate.has_pending());
    }

    #[test]
    fn open_prefixes_are_held_and_flushed() {
        for open in [&b"\x1b"[..], b"\x1b[", b"\x1bO", b"\x1b[<35;1", b"\x1b[1;"] {
            let mut gate = EscapeSequenceGate::default();
            let mut input = b"x".to_vec();
            input.extend_from_slice(open);
            assert_eq!(gate.push(input), b"x", "{open:?}");
            assert!(gate.has_pending());
            assert_eq!(gate.flush(), open);
            assert!(!gate.has_pending());
        }
    }

    #[test]
    fn an_overlong_open_sequence_is_released_not_buffered() {
        let mut gate = EscapeSequenceGate::default();
        let mut input = b"\x1b[".to_vec();
        input.extend(std::iter::repeat_n(b'1', MAX_HELD));
        assert_eq!(gate.push(input.clone()), input);
        assert!(!gate.has_pending());
    }
}
