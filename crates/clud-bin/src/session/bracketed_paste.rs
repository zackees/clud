use super::{looks_like_dropped_path, normalize_dropped_path};

/// Bracketed-paste byte sequence emitted by xterm-class terminals when
/// the user pastes (or drags-and-drops) text.
pub(crate) const PASTE_START: &[u8] = b"\x1b[200~";
pub(crate) const PASTE_END: &[u8] = b"\x1b[201~";

/// Stream-resumable bracketed-paste detector that, on each completed
/// paste, runs the buffered inner content through
/// [`looks_like_dropped_path`] / [`normalize_dropped_path`] to canonicalize
/// terminal-specific drop encodings (issue #63 / #79).
///
/// Behavior:
/// - Bytes outside any paste pass through unchanged.
/// - Bytes inside a bracketed paste are buffered. On `\x1b[201~`:
///     - If `looks_like_dropped_path(inner)` returns true, emit
///       `\x1b[200~` + `normalize_dropped_path(inner)` + `\x1b[201~`.
///     - Otherwise, emit the original `\x1b[200~ inner \x1b[201~` verbatim.
/// - The detector survives across chunks: a paste split across reads is
///   reassembled correctly.
///
/// The PASS-IT-VERBATIM rule on non-path content is essential — a
/// multi-line code paste must not be mutated, even if its first line
/// happens to start with `/`.
pub struct BracketedPasteNormalizer {
    /// How many bytes of `PASTE_START` we've matched while outside a
    /// paste. 0..PASTE_START.len().
    start_match: usize,
    /// `Some(buf)` while we are inside a paste body. The buffer holds
    /// the *inner* paste content (no `\x1b[200~` prefix and no terminal
    /// `\x1b[201~`).
    inside: Option<Vec<u8>>,
    /// How many bytes of `PASTE_END` we've matched while inside a paste.
    end_match: usize,
}

impl BracketedPasteNormalizer {
    pub fn new() -> Self {
        Self {
            start_match: 0,
            inside: None,
            end_match: 0,
        }
    }

    /// Process a chunk, returning the byte stream that should be
    /// forwarded downstream (PTY master, in production).
    pub fn process(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(chunk.len());
        for &b in chunk {
            if let Some(buf) = self.inside.as_mut() {
                // We are inside a paste body. Look for PASTE_END.
                if b == PASTE_END[self.end_match] {
                    self.end_match += 1;
                    if self.end_match == PASTE_END.len() {
                        // Complete: emit normalized form and reset.
                        let inner = std::mem::take(buf);
                        self.inside = None;
                        self.end_match = 0;
                        emit_paste_block(&mut out, &inner);
                    }
                    continue;
                }

                // PASTE_END prefix broke. Flush any partial-end bytes
                // back into the inner buffer, then this byte too.
                if self.end_match > 0 {
                    buf.extend_from_slice(&PASTE_END[..self.end_match]);
                    // The byte that broke the prefix may itself start a
                    // new PASTE_END match.
                    self.end_match = if b == PASTE_END[0] { 1 } else { 0 };
                    if self.end_match == 0 {
                        buf.push(b);
                    }
                } else {
                    buf.push(b);
                }
            } else {
                // We are outside a paste. Look for PASTE_START.
                if b == PASTE_START[self.start_match] {
                    self.start_match += 1;
                    if self.start_match == PASTE_START.len() {
                        // Complete: enter paste body.
                        self.start_match = 0;
                        self.end_match = 0;
                        self.inside = Some(Vec::new());
                    }
                    continue;
                }

                // PASTE_START prefix broke. Flush partial bytes verbatim.
                if self.start_match > 0 {
                    out.extend_from_slice(&PASTE_START[..self.start_match]);
                    self.start_match = if b == PASTE_START[0] { 1 } else { 0 };
                    if self.start_match == 0 {
                        out.push(b);
                    }
                } else {
                    out.push(b);
                }
            }
        }
        out
    }
}

impl Default for BracketedPasteNormalizer {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper for `BracketedPasteNormalizer::process` — given a captured
/// inner paste body, emit the wrapped (and possibly path-normalized)
/// bracketed-paste block to `out`.
fn emit_paste_block(out: &mut Vec<u8>, inner: &[u8]) {
    out.extend_from_slice(PASTE_START);
    // Decide path-rewrite on the WHOLE buffer, not per-line: a
    // multi-line code paste with a path-shaped first line must remain
    // verbatim. `looks_like_dropped_path` is conservative — it requires
    // the entire trimmed string to look like a single path token.
    let s = match std::str::from_utf8(inner) {
        Ok(s) => s,
        Err(_) => {
            out.extend_from_slice(inner);
            out.extend_from_slice(PASTE_END);
            return;
        }
    };
    if looks_like_dropped_path(s) {
        let normalized = normalize_dropped_path(s);
        out.extend_from_slice(normalized.as_bytes());
    } else {
        out.extend_from_slice(inner);
    }
    out.extend_from_slice(PASTE_END);
}
