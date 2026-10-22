//! Feed the compositor's `vt100` shadow the sequences it would drop (#2028).
//!
//! `vt100` 0.16 has no `csi_dispatch` arm for `REP` (CSI b), `HPA` (CSI \`),
//! `HVP` (CSI f), `HPR` (CSI a), `VPR` (CSI e), `CHT` (CSI I) or `CBT`
//! (CSI Z). A stream that uses one leaves the shadow's cursor or cells
//! somewhere the real terminal's are not, and the next text-cell repaint
//! writes that drift over the child's screen. Each of these is rewritten
//! here into the sequence `vt100` does model, computed against the shadow's
//! own cursor at the point the sequence occurs. The bytes the terminal
//! receives are never touched; only the shadow's input is.

/// Longest CSI held across a chunk boundary. A longer run is not a sequence
/// any emitter writes, and holding it would starve the shadow.
const MAX_HELD_CSI: usize = 64;

/// The translator's only state: an incomplete CSI at the end of a chunk.
#[derive(Debug, Default)]
pub struct ShadowFeed {
    held: Vec<u8>,
}

enum Scan {
    /// The bytes at this position are not a CSI.
    NotCsi,
    /// A CSI that may still complete in the next chunk.
    Incomplete,
    /// A complete CSI of `len` bytes; `rewrite` is set when `vt100` drops it.
    Complete {
        len: usize,
        rewrite: Option<Rewrite>,
    },
}

#[derive(Clone, Copy)]
enum Rewrite {
    Repeat(u16),
    Column(u16),
    Forward(u16),
    Down(u16),
    TabForward(u16),
    TabBack(u16),
    Position,
}

impl ShadowFeed {
    /// Process `bytes` into `parser`, rewriting the sequences it drops.
    pub fn feed(&mut self, parser: &mut vt100::Parser, bytes: &[u8]) {
        let mut data = std::mem::take(&mut self.held);
        data.extend_from_slice(bytes);
        let mut start = 0;
        let mut i = 0;
        while i < data.len() {
            if data[i] != 0x1b {
                i += 1;
                continue;
            }
            match scan_csi(&data[i..]) {
                Scan::NotCsi => i += 1,
                Scan::Incomplete => {
                    parser.process(&data[start..i]);
                    self.held = data[i..].to_vec();
                    return;
                }
                Scan::Complete { len, rewrite } => {
                    if let Some(rewrite) = rewrite {
                        parser.process(&data[start..i]);
                        let replacement = replacement(parser.screen(), rewrite, &data[i..i + len]);
                        parser.process(&replacement);
                        start = i + len;
                    }
                    i += len;
                }
            }
        }
        parser.process(&data[start..]);
    }
}

fn scan_csi(data: &[u8]) -> Scan {
    match data.get(1) {
        None => return Scan::Incomplete,
        Some(b'[') => {}
        Some(_) => return Scan::NotCsi,
    }
    let mut private = false;
    let mut intermediate = false;
    for (offset, &byte) in data.iter().enumerate().skip(2) {
        if offset >= MAX_HELD_CSI {
            return Scan::NotCsi;
        }
        match byte {
            b'<'..=b'?' if offset == 2 => private = true,
            0x30..=0x3f => {}
            0x20..=0x2f => intermediate = true,
            0x40..=0x7e => {
                let len = offset + 1;
                let rewrite = if private || intermediate {
                    None
                } else {
                    rewrite_for(byte, &data[2..offset])
                };
                return Scan::Complete { len, rewrite };
            }
            _ => return Scan::NotCsi,
        }
    }
    Scan::Incomplete
}

/// The first numeric parameter, with 0 or absent read as 1.
fn count(params: &[u8]) -> u16 {
    let first = params.split(|b| *b == b';').next().unwrap_or_default();
    std::str::from_utf8(first)
        .ok()
        .and_then(|text| text.parse::<u16>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(1)
}

fn rewrite_for(final_byte: u8, params: &[u8]) -> Option<Rewrite> {
    let n = count(params);
    Some(match final_byte {
        b'b' => Rewrite::Repeat(n),
        b'`' => Rewrite::Column(n),
        b'a' => Rewrite::Forward(n),
        b'e' => Rewrite::Down(n),
        b'I' => Rewrite::TabForward(n),
        b'Z' => Rewrite::TabBack(n),
        b'f' => Rewrite::Position,
        _ => return None,
    })
}

const TAB: u16 = 8;

fn replacement(screen: &vt100::Screen, rewrite: Rewrite, sequence: &[u8]) -> Vec<u8> {
    let (row, col) = screen.cursor_position();
    let (rows, cols) = screen.size();
    match rewrite {
        Rewrite::Repeat(n) => repeated(screen, row, col, n, rows, cols),
        Rewrite::Column(n) => format!("\x1b[{n}G").into_bytes(),
        Rewrite::Forward(n) => format!("\x1b[{n}C").into_bytes(),
        Rewrite::Down(n) => format!("\x1b[{n}B").into_bytes(),
        Rewrite::TabForward(n) => {
            let target = (col / TAB + n)
                .saturating_mul(TAB)
                .min(cols.saturating_sub(1));
            format!("\x1b[{}G", target + 1).into_bytes()
        }
        Rewrite::TabBack(n) => {
            let stops_back = if col % TAB == 0 { n } else { n - 1 };
            let target = (col / TAB).saturating_sub(stops_back) * TAB;
            format!("\x1b[{}G", target + 1).into_bytes()
        }
        Rewrite::Position => {
            let mut out = sequence.to_vec();
            if let Some(last) = out.last_mut() {
                *last = b'H';
            }
            out
        }
    }
}

/// `REP`: the graphic character before the cursor, `n` more times.
fn repeated(screen: &vt100::Screen, row: u16, col: u16, n: u16, rows: u16, cols: u16) -> Vec<u8> {
    let previous = col
        .checked_sub(1)
        .and_then(|c| screen.cell(row, c))
        .and_then(|cell| {
            if cell.is_wide_continuation() {
                col.checked_sub(2).and_then(|c| screen.cell(row, c))
            } else {
                Some(cell)
            }
        })
        .map(vt100::Cell::contents)
        .filter(|text| !text.is_empty());
    let Some(text) = previous else {
        return Vec::new();
    };
    let limit = usize::from(rows).saturating_mul(usize::from(cols));
    text.repeat(usize::from(n).min(limit)).into_bytes()
}

#[cfg(test)]
#[path = "shadow_input_tests.rs"]
mod tests;
