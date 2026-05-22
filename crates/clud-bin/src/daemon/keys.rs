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
