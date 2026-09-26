//! UTF-16 surrogate-pair assembly for Windows console key records (#1351).
//!
//! A Win32 `KEY_EVENT_RECORD` carries one UTF-16 code unit. A character
//! outside the Basic Multilingual Plane (most emoji, some CJK ideographs)
//! therefore arrives as two key-down records: a high surrogate, then a low
//! surrogate. `running-process`'s per-record translator rejects each half as
//! an invalid `char` and drops both, so this module pairs them before a record
//! reaches that translator.
//!
//! The logic is platform-neutral so it is unit-tested on every OS; only the
//! Windows reader in `console_input.rs` calls it. The rules:
//!
//! - Pairing state lives in [`SurrogatePairer`], which the reader keeps for its
//!   whole life, so a pair split across two `ReadConsoleInputW` batches still
//!   joins.
//! - Key-up records never change the state. Windows sends a key-up after each
//!   key-down, including between the two halves of a pair; key-ups carry no
//!   input, and the generic translator drops them.
//! - The completed character repeats `wRepeatCount` times (from the low
//!   surrogate's record; zero counts as one), like any other character.
//! - An unpaired half becomes U+FFFD, once per repeat. A high surrogate is
//!   unpaired when the next key-down is not a low surrogate; a low surrogate
//!   is unpaired when no high surrogate is pending. Emitting U+FFFD, as
//!   `String::from_utf16_lossy` does, shows the loss instead of hiding it.
//! - The character is literal text: no Alt ESC prefix and no Ctrl mapping,
//!   since surrogate records come from the emoji picker, an IME, or pasted
//!   text, never from a modified key chord.
//!
//! Pasted text (including a bracketed paste, whose markers are ordinary key
//! records) travels the same record path, so it needs no separate handling.

/// First high (leading) surrogate code unit.
const HIGH_SURROGATE_START: u16 = 0xD800;
/// Last high (leading) surrogate code unit.
const HIGH_SURROGATE_END: u16 = 0xDBFF;
/// First low (trailing) surrogate code unit.
const LOW_SURROGATE_START: u16 = 0xDC00;
/// Last low (trailing) surrogate code unit.
const LOW_SURROGATE_END: u16 = 0xDFFF;

/// The parts of a console key record that surrogate pairing reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyUnit {
    /// `bKeyDown != 0`.
    pub key_down: bool,
    /// `uChar.UnicodeChar`: one UTF-16 code unit, or 0 for a non-text key.
    pub unit: u16,
    /// `wRepeatCount`.
    pub repeat_count: u16,
}

/// What to do with one key record.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Step {
    /// UTF-8 bytes to emit before the record is translated (if it is).
    pub text: Vec<u8>,
    /// Whether the record should still go through the generic translator.
    pub translate: bool,
}

/// Pairs surrogate halves across a stream of console key records.
#[derive(Debug, Default)]
pub struct SurrogatePairer {
    /// A high surrogate waiting for its low half, with its repeat count.
    pending_high: Option<(u16, u16)>,
}

impl SurrogatePairer {
    /// A pairer with no pending half.
    pub fn new() -> Self {
        Self::default()
    }

    /// Classify one key record and update the pairing state.
    pub fn feed(&mut self, key: KeyUnit) -> Step {
        if !key.key_down {
            return Step {
                text: Vec::new(),
                translate: true,
            };
        }
        let repeat = usize::from(key.repeat_count.max(1));
        match key.unit {
            HIGH_SURROGATE_START..=HIGH_SURROGATE_END => {
                let text = self.abandon_pending();
                self.pending_high = Some((key.unit, key.repeat_count));
                Step {
                    text,
                    translate: false,
                }
            }
            LOW_SURROGATE_START..=LOW_SURROGATE_END => {
                let ch = match self.pending_high.take() {
                    Some((high, _)) => char::decode_utf16([high, key.unit])
                        .next()
                        .and_then(Result::ok)
                        .unwrap_or(char::REPLACEMENT_CHARACTER),
                    None => char::REPLACEMENT_CHARACTER,
                };
                Step {
                    text: ch.to_string().repeat(repeat).into_bytes(),
                    translate: false,
                }
            }
            _ => Step {
                text: self.abandon_pending(),
                translate: true,
            },
        }
    }

    /// U+FFFD bytes for a pending high surrogate that will never be paired.
    fn abandon_pending(&mut self) -> Vec<u8> {
        self.pending_high
            .take()
            .map(|(_, repeat)| {
                char::REPLACEMENT_CHARACTER
                    .to_string()
                    .repeat(usize::from(repeat.max(1)))
                    .into_bytes()
            })
            .unwrap_or_default()
    }

    /// Run one `ReadConsoleInputW` batch through the pairer.
    ///
    /// `unit_of` reads a record's [`KeyUnit`]. `translate` is the generic
    /// per-record translator, called only for records that are not surrogate
    /// halves. `text_event` wraps assembled UTF-8 text in the output type.
    /// Outputs keep input order: a U+FFFD for an abandoned high surrogate comes
    /// before the translation of the record that abandoned it.
    pub fn translate_batch<R, T>(
        &mut self,
        records: &[R],
        unit_of: impl Fn(&R) -> KeyUnit,
        mut translate: impl FnMut(&R) -> Option<T>,
        mut text_event: impl FnMut(&R, Vec<u8>) -> T,
    ) -> Vec<T> {
        let mut out = Vec::new();
        for record in records {
            let step = self.feed(unit_of(record));
            if !step.text.is_empty() {
                out.push(text_event(record, step.text));
            }
            if step.translate {
                out.extend(translate(record));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPLACEMENT: &str = "\u{FFFD}";

    fn down(unit: u16) -> KeyUnit {
        KeyUnit {
            key_down: true,
            unit,
            repeat_count: 1,
        }
    }

    fn up(unit: u16) -> KeyUnit {
        KeyUnit {
            key_down: false,
            unit,
            repeat_count: 1,
        }
    }

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    /// Stand-in for running-process's translator: key-ups and 0 units drop,
    /// a BMP unit becomes its UTF-8, and a surrogate half drops (the #1351 bug).
    fn fake_translate(key: &KeyUnit) -> Option<Vec<u8>> {
        if !key.key_down || key.unit == 0 {
            return None;
        }
        let ch = char::from_u32(u32::from(key.unit))?;
        Some(
            ch.to_string()
                .repeat(usize::from(key.repeat_count.max(1)))
                .into_bytes(),
        )
    }

    /// Run batches through one pairer and concatenate every emitted chunk.
    fn run(pairer: &mut SurrogatePairer, batches: &[&[KeyUnit]]) -> Vec<u8> {
        batches
            .iter()
            .flat_map(|batch| pairer.translate_batch(batch, |k| *k, fake_translate, |_, t| t))
            .flatten()
            .collect()
    }

    /// Every unit of `text` as a key-down followed by its key-up, the way
    /// Windows Terminal and `SendInput(KEYEVENTF_UNICODE)` deliver text.
    fn typed(text: &str) -> Vec<KeyUnit> {
        units(text)
            .into_iter()
            .flat_map(|u| [down(u), up(u)])
            .collect()
    }

    #[test]
    fn emoji_pair_in_one_batch_becomes_utf8() {
        let [high, low] = units("😀")[..] else {
            panic!("U+1F600 is a surrogate pair")
        };
        let mut pairer = SurrogatePairer::new();
        assert_eq!(
            run(&mut pairer, &[&[down(high), down(low)]]),
            "😀".as_bytes()
        );
    }

    #[test]
    fn emoji_pair_split_across_read_batches_still_joins() {
        let [high, low] = units("😀")[..] else {
            panic!("U+1F600 is a surrogate pair")
        };
        let mut pairer = SurrogatePairer::new();
        assert_eq!(run(&mut pairer, &[&[down(high)]]), b"");
        assert_eq!(run(&mut pairer, &[&[down(low)]]), "😀".as_bytes());
    }

    #[test]
    fn key_up_records_between_halves_are_ignored() {
        let mut pairer = SurrogatePairer::new();
        let records = typed("😀");
        assert_eq!(run(&mut pairer, &[&records]), "😀".as_bytes());
    }

    #[test]
    fn key_up_records_alone_emit_nothing() {
        let [high, low] = units("😀")[..] else {
            panic!("U+1F600 is a surrogate pair")
        };
        let mut pairer = SurrogatePairer::new();
        assert_eq!(run(&mut pairer, &[&[up(high), up(low), up(0x61)]]), b"");
    }

    #[test]
    fn pair_honors_repeat_count() {
        let [high, low] = units("😀")[..] else {
            panic!("U+1F600 is a surrogate pair")
        };
        let mut pairer = SurrogatePairer::new();
        let mut low_key = down(low);
        low_key.repeat_count = 3;
        assert_eq!(
            run(&mut pairer, &[&[down(high), low_key]]),
            "😀😀😀".as_bytes()
        );
    }

    #[test]
    fn pair_repeat_count_zero_counts_as_one() {
        let [high, low] = units("😀")[..] else {
            panic!("U+1F600 is a surrogate pair")
        };
        let mut pairer = SurrogatePairer::new();
        let mut low_key = down(low);
        low_key.repeat_count = 0;
        assert_eq!(run(&mut pairer, &[&[down(high), low_key]]), "😀".as_bytes());
    }

    #[test]
    fn lone_low_surrogate_becomes_replacement_char() {
        let low = units("😀")[1];
        let mut pairer = SurrogatePairer::new();
        let out = run(&mut pairer, &[&[down(low), down(0x61)]]);
        assert_eq!(out, format!("{REPLACEMENT}a").as_bytes());
    }

    #[test]
    fn high_surrogate_abandoned_by_bmp_key_becomes_replacement_before_it() {
        let high = units("😀")[0];
        let mut pairer = SurrogatePairer::new();
        let out = run(&mut pairer, &[&[down(high)], &[down(0x61)]]);
        assert_eq!(out, format!("{REPLACEMENT}a").as_bytes());
    }

    #[test]
    fn high_surrogate_abandoned_by_another_high_keeps_the_new_one_pending() {
        let [high, low] = units("😀")[..] else {
            panic!("U+1F600 is a surrogate pair")
        };
        let mut pairer = SurrogatePairer::new();
        let out = run(&mut pairer, &[&[down(high), down(high), down(low)]]);
        assert_eq!(out, format!("{REPLACEMENT}😀").as_bytes());
    }

    #[test]
    fn high_surrogate_abandoned_by_non_text_key_still_translates_that_key() {
        let high = units("😀")[0];
        let mut pairer = SurrogatePairer::new();
        let arrow = KeyUnit {
            key_down: true,
            unit: 0,
            repeat_count: 1,
        };
        let steps = pairer.translate_batch(
            &[down(high), arrow],
            |k| *k,
            |k| (k.unit == 0).then(|| b"\x1b[D".to_vec()),
            |_, t| t,
        );
        assert_eq!(
            steps,
            vec![REPLACEMENT.as_bytes().to_vec(), b"\x1b[D".to_vec()]
        );
    }

    #[test]
    fn bmp_characters_pass_through_the_translator_unchanged() {
        let mut pairer = SurrogatePairer::new();
        let text = "aé€中\u{FFFD}";
        assert_eq!(run(&mut pairer, &[&typed(text)]), text.as_bytes());
        for unit in units(text) {
            assert_eq!(
                pairer.feed(down(unit)),
                Step {
                    text: Vec::new(),
                    translate: true
                }
            );
        }
    }

    #[test]
    fn pasted_text_with_emoji_and_bracketed_paste_markers_round_trips() {
        let pasted = "\u{1b}[200~héllo 😀 wörld 𠀋!\u{1b}[201~";
        let records = typed(pasted);
        // Split mid-pair: the batch boundary falls between the halves of 😀.
        let split = records
            .iter()
            .position(|k| {
                k.key_down && (HIGH_SURROGATE_START..=HIGH_SURROGATE_END).contains(&k.unit)
            })
            .expect("pasted text contains a high surrogate")
            + 1;
        let mut pairer = SurrogatePairer::new();
        let out = run(&mut pairer, &[&records[..split], &records[split..]]);
        assert_eq!(out, pasted.as_bytes());
    }

    #[test]
    fn surrogate_ranges_cover_every_supplementary_plane_code_unit() {
        for ch in ['\u{10000}', '😀', '𠀋', '\u{10FFFF}'] {
            let mut buf = [0u16; 2];
            let [high, low] = *ch.encode_utf16(&mut buf) else {
                panic!("{ch:?} is a surrogate pair")
            };
            assert!((HIGH_SURROGATE_START..=HIGH_SURROGATE_END).contains(&high));
            assert!((LOW_SURROGATE_START..=LOW_SURROGATE_END).contains(&low));
        }
    }
}
