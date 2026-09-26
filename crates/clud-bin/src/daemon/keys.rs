use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::types::KeyAction;

pub(super) fn translate_key_event(key: KeyEvent) -> KeyAction {
    // F3 is special: voice mode wants both press AND release events so
    // the hold-to-record contract works in centralized mode the same way
    // it does in the local-PTY runner. Handle it before the generic
    // release filter below.
    if matches!(key.code, KeyCode::F(3)) {
        return match key.kind {
            KeyEventKind::Release => KeyAction::F3Release,
            _ => KeyAction::F3Press,
        };
    }
    if matches!(key.kind, KeyEventKind::Release) {
        return KeyAction::Ignore;
    }
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => KeyAction::Interrupt,
        KeyCode::Char(ch) => translate_char_key(ch, key.modifiers),
        KeyCode::Enter => KeyAction::Forward(vec![b'\r']),
        KeyCode::Tab => KeyAction::Forward(vec![b'\t']),
        KeyCode::BackTab => KeyAction::Forward(b"\x1b[Z".to_vec()),
        KeyCode::Backspace => KeyAction::Forward(vec![0x7f]),
        KeyCode::Esc => KeyAction::Forward(vec![0x1b]),
        KeyCode::Left => KeyAction::Forward(b"\x1b[D".to_vec()),
        KeyCode::Right => KeyAction::Forward(b"\x1b[C".to_vec()),
        KeyCode::Up => KeyAction::Forward(b"\x1b[A".to_vec()),
        KeyCode::Down => KeyAction::Forward(b"\x1b[B".to_vec()),
        KeyCode::Home => KeyAction::Forward(b"\x1b[H".to_vec()),
        KeyCode::End => KeyAction::Forward(b"\x1b[F".to_vec()),
        KeyCode::PageUp => KeyAction::Forward(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => KeyAction::Forward(b"\x1b[6~".to_vec()),
        KeyCode::Delete => KeyAction::Forward(b"\x1b[3~".to_vec()),
        KeyCode::Insert => KeyAction::Forward(b"\x1b[2~".to_vec()),
        _ => KeyAction::Ignore,
    }
}

fn translate_char_key(ch: char, modifiers: KeyModifiers) -> KeyAction {
    let alt = modifiers.contains(KeyModifiers::ALT);
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    if is_altgr_char(ch, modifiers) {
        let mut buffer = [0u8; 4];
        return KeyAction::Forward(ch.encode_utf8(&mut buffer).as_bytes().to_vec());
    }
    if ctrl {
        if let Some(byte) = ctrl_char_to_byte(ch) {
            return if alt {
                KeyAction::Forward(vec![0x1b, byte])
            } else {
                KeyAction::Forward(vec![byte])
            };
        }
    }

    let mut bytes = Vec::new();
    if alt {
        bytes.push(0x1b);
    }
    let mut buffer = [0u8; 4];
    bytes.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
    KeyAction::Forward(bytes)
}

/// Issue #1352: Windows reports AltGr as CONTROL|ALT together with the
/// already-composed character (German AltGr+Q -> '@', AltGr+8 -> '[').
/// Such a char must be forwarded literally, not ctrl-translated. AltGr
/// never composes a plain ASCII letter, so Ctrl+Alt+<letter> keeps its
/// ESC + ctrl-byte meaning. POSIX terminals never report Ctrl+Alt with a
/// symbol char, so this is unconditional.
fn is_altgr_char(ch: char, modifiers: KeyModifiers) -> bool {
    modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::ALT) && !ch.is_ascii_alphabetic()
}

fn ctrl_char_to_byte(ch: char) -> Option<u8> {
    match ch {
        '@' | ' ' => Some(0x00),
        'a'..='z' => Some((ch as u8 - b'a') + 1),
        'A'..='Z' => Some((ch as u8 - b'A') + 1),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forwarded(ch: char, mods: KeyModifiers) -> Vec<u8> {
        let KeyAction::Forward(bytes) = translate_key_event(KeyEvent::new(KeyCode::Char(ch), mods))
        else {
            panic!("expected Forward for {ch:?} with {mods:?}");
        };
        bytes
    }

    fn altgr() -> KeyModifiers {
        KeyModifiers::CONTROL | KeyModifiers::ALT
    }

    #[test]
    fn altgr_at_sign_forwards_literal() {
        assert_eq!(forwarded('@', altgr()), b"@".to_vec());
    }

    #[test]
    fn altgr_brackets_and_backslash_forward_literal() {
        for ch in ['[', ']', '\\', '{', '}', '|', '~'] {
            assert_eq!(
                forwarded(ch, altgr()),
                ch.to_string().into_bytes(),
                "{ch:?}"
            );
        }
    }

    #[test]
    fn altgr_non_ascii_forwards_utf8() {
        assert_eq!(forwarded('€', altgr()), "€".as_bytes().to_vec());
    }

    #[test]
    fn ctrl_alt_letter_still_sends_esc_ctrl_byte() {
        assert_eq!(forwarded('a', altgr()), vec![0x1b, 0x01]);
    }

    #[test]
    fn ctrl_c_is_interrupt() {
        let action = translate_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(matches!(action, KeyAction::Interrupt));
    }

    #[test]
    fn ctrl_at_without_alt_is_nul() {
        assert_eq!(forwarded('@', KeyModifiers::CONTROL), vec![0]);
    }
}
