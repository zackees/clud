//! Codex-only bare-LF normalizer for the PTY output path (#1181).
//!
//! Codex's TUI writes every history line with an explicit `\r\n`, but its
//! `/goal` status cell hands the whole objective to one span
//! (`codex-rs/tui/src/goal_display.rs::goal_usage_summary`). A pasted
//! multi-line objective therefore reaches the terminal with embedded bare
//! `\n` while the terminal sits in raw mode with `OPOST` off, and every
//! line starts at the column where the previous one ended. The Windows
//! console masks this with its own LF->CRLF output processing; Linux/macOS
//! terminals do not.
//!
//! This filter gives Linux/macOS the same masking the Windows console gives
//! for free. It is **only** chained into the pump for the Codex backend:
//! a TUI may legitimately emit a bare LF inside a scroll region and rely on
//! the column staying put, and Codex never does that in its own writes.
//!
//! The filter is stream-resumable: a `\r\n` split across two chunks yields
//! exactly one `\r\n`, never `\r\r\n`.

/// Rewrites bare `\n` (not preceded by `\r`) to `\r\n` across chunk
/// boundaries. See the module docs for why this exists and why it is
/// Codex-only.
#[derive(Debug, Default)]
pub struct CodexLfNormalizer {
    /// Whether the last byte of the previous chunk was `\r`.
    prev_was_cr: bool,
}

impl CodexLfNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process one output chunk, returning the bytes to forward.
    pub fn process(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(chunk.len() + chunk.len() / 16);
        let mut prev_was_cr = self.prev_was_cr;
        for &b in chunk {
            if b == b'\n' && !prev_was_cr {
                out.push(b'\r');
            }
            out.push(b);
            prev_was_cr = b == b'\r';
        }
        self.prev_was_cr = prev_was_cr;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::CodexLfNormalizer;

    #[test]
    fn bare_lf_gets_a_carriage_return() {
        let mut n = CodexLfNormalizer::new();
        assert_eq!(n.process(b"a\nb"), b"a\r\nb");
    }

    #[test]
    fn crlf_is_left_alone() {
        let mut n = CodexLfNormalizer::new();
        assert_eq!(n.process(b"a\r\nb\r\n"), b"a\r\nb\r\n");
    }

    #[test]
    fn crlf_split_across_chunks_is_not_doubled() {
        let mut n = CodexLfNormalizer::new();
        let mut out = n.process(b"a\r");
        out.extend(n.process(b"\nb"));
        assert_eq!(out, b"a\r\nb");
    }

    #[test]
    fn lf_at_chunk_start_after_non_cr_gets_a_carriage_return() {
        let mut n = CodexLfNormalizer::new();
        let mut out = n.process(b"a");
        out.extend(n.process(b"\nb"));
        assert_eq!(out, b"a\r\nb");
    }

    #[test]
    fn bare_cr_and_escape_sequences_pass_verbatim() {
        let mut n = CodexLfNormalizer::new();
        let input = b"\x1b[1;18r\x1b[11;1H\rprogress\x1b[K";
        assert_eq!(n.process(input), input);
    }

    /// The exact shape Codex 0.154.0 emits for a pasted multi-line `/goal`
    /// objective (captured under a scripted PTY, see #1181).
    #[test]
    fn codex_goal_cell_staircase_is_repaired() {
        let mut n = CodexLfNormalizer::new();
        let input = b"\x1b[22mGoal active \x1b[38;5;8;49mObjective: fix the following\nhttps://example.com/issues/1\nGoal is achieved\x1b[39m";
        let out = n.process(input);
        assert_eq!(
            out,
            b"\x1b[22mGoal active \x1b[38;5;8;49mObjective: fix the following\r\nhttps://example.com/issues/1\r\nGoal is achieved\x1b[39m"
        );
    }

    #[test]
    fn empty_chunk_is_a_no_op() {
        let mut n = CodexLfNormalizer::new();
        assert!(n.process(b"").is_empty());
    }
}
