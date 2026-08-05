//! Resumable OSC title filtering for terminal output.

pub struct OscTitleStripper {
    state: OscState,
    digits: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OscState {
    Normal,
    AfterEsc,
    InOscNumber,
    SwallowOscBody,
    SwallowAfterEsc,
    PassthroughOscBody,
    PassthroughAfterEsc,
}

impl OscTitleStripper {
    pub fn new() -> Self {
        Self {
            state: OscState::Normal,
            digits: Vec::new(),
        }
    }
    pub fn process(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(chunk.len());
        for &byte in chunk {
            self.process_byte(byte, &mut out);
        }
        out
    }
    fn process_byte(&mut self, byte: u8, out: &mut Vec<u8>) {
        match self.state {
            OscState::Normal => {
                if byte == 0x1b {
                    self.state = OscState::AfterEsc
                } else {
                    out.push(byte)
                }
            }
            OscState::AfterEsc => match byte {
                b']' => {
                    self.state = OscState::InOscNumber;
                    self.digits.clear()
                }
                0x1b => out.push(0x1b),
                _ => {
                    out.extend_from_slice(&[0x1b, byte]);
                    self.state = OscState::Normal
                }
            },
            OscState::InOscNumber => {
                if byte.is_ascii_digit() {
                    self.digits.push(byte)
                } else if byte == b';' {
                    self.state = if self.digits == b"0" || self.digits == b"2" {
                        OscState::SwallowOscBody
                    } else {
                        out.extend_from_slice(&[0x1b, b']']);
                        out.extend_from_slice(&self.digits);
                        out.push(b';');
                        OscState::PassthroughOscBody
                    };
                    self.digits.clear()
                } else if byte == 0x07 {
                    self.digits.clear();
                    self.state = OscState::Normal
                } else {
                    out.extend_from_slice(&[0x1b, b']']);
                    out.extend_from_slice(&self.digits);
                    if byte != 0x1b {
                        out.push(byte)
                    };
                    self.digits.clear();
                    self.state = if byte == 0x1b {
                        OscState::PassthroughAfterEsc
                    } else {
                        OscState::PassthroughOscBody
                    }
                }
            }
            OscState::SwallowOscBody => match byte {
                0x07 => self.state = OscState::Normal,
                0x1b => self.state = OscState::SwallowAfterEsc,
                _ => {}
            },
            OscState::SwallowAfterEsc => match byte {
                b'\\' | 0x07 => self.state = OscState::Normal,
                0x1b => {}
                _ => self.state = OscState::SwallowOscBody,
            },
            OscState::PassthroughOscBody => {
                out.push(byte);
                match byte {
                    0x07 => self.state = OscState::Normal,
                    0x1b => self.state = OscState::PassthroughAfterEsc,
                    _ => {}
                }
            }
            OscState::PassthroughAfterEsc => {
                out.push(byte);
                if byte == b'\\' || byte == 0x07 {
                    self.state = OscState::Normal
                } else if byte != 0x1b {
                    self.state = OscState::PassthroughOscBody
                }
            }
        }
    }
}
impl Default for OscTitleStripper {
    fn default() -> Self {
        Self::new()
    }
}
