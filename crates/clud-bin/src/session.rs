use std::io::{self, IsTerminal};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use running_process_core::pty::NativePtyProcess;

pub trait InteractiveHooks {
    fn intercept_f3(&self) -> bool {
        false
    }

    fn on_f3_press(&mut self, _process: &NativePtyProcess) -> io::Result<()> {
        Ok(())
    }

    fn on_f3_release(&mut self, _process: &NativePtyProcess) -> io::Result<()> {
        Ok(())
    }

    fn on_tick(&mut self, _process: &NativePtyProcess) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum KeyAction {
    Forward(Vec<u8>),
    Interrupt,
    F3Press,
    F3Release,
    Ignore,
}

#[derive(Debug)]
struct RawTerminalGuard {
    enhancement_flags_pushed: bool,
}

impl RawTerminalGuard {
    fn enter() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;

        let mut stdout = io::stdout();
        let flags = KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            | KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
        let enhancement_flags_pushed =
            execute!(stdout, PushKeyboardEnhancementFlags(flags)).is_ok();

        Ok(Self {
            enhancement_flags_pushed,
        })
    }
}

impl Drop for RawTerminalGuard {
    fn drop(&mut self) {
        let _ = if self.enhancement_flags_pushed {
            execute!(io::stdout(), PopKeyboardEnhancementFlags)
        } else {
            Ok(())
        };
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

pub fn terminals_are_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

pub fn run_interactive_pty_session<H: InteractiveHooks>(
    process: &NativePtyProcess,
    interrupted: &AtomicBool,
    hooks: &mut H,
) -> i32 {
    let _raw_guard = match RawTerminalGuard::enter() {
        Ok(guard) => guard,
        Err(err) => {
            eprintln!(
                "[clud] warning: failed to enable raw terminal mode: {}",
                err
            );
            return run_pty_output_loop(process, interrupted);
        }
    };

    loop {
        match process.read_chunk_impl(Some(0.01)) {
            Ok(Some(chunk)) => {
                let _ = process.respond_to_queries_impl(&chunk);
            }
            Ok(None) => {}
            Err(_) => return reap_pty_exit(process),
        }

        while matches!(event::poll(Duration::from_millis(0)), Ok(true)) {
            let event = match event::read() {
                Ok(event) => event,
                Err(err) => {
                    eprintln!("[clud] warning: failed to read terminal input: {}", err);
                    break;
                }
            };

            if let Err(err) = handle_terminal_event(process, event, hooks) {
                eprintln!("[clud] warning: failed to handle terminal input: {}", err);
            }

            if interrupted.load(Ordering::SeqCst) {
                return interrupt_pty_process(process);
            }
        }

        if let Err(err) = hooks.on_tick(process) {
            eprintln!("[clud] warning: interactive hook tick failed: {}", err);
        }

        if let Ok(Some(code)) =
            running_process_core::pty::poll_pty_process(&process.handles, &process.returncode)
        {
            return code;
        }

        if interrupted.load(Ordering::SeqCst) {
            return interrupt_pty_process(process);
        }
    }
}

pub fn run_pty_output_loop(process: &NativePtyProcess, interrupted: &AtomicBool) -> i32 {
    loop {
        match process.read_chunk_impl(Some(0.1)) {
            Ok(Some(chunk)) => {
                let _ = process.respond_to_queries_impl(&chunk);
            }
            Ok(None) => {}
            Err(_) => return reap_pty_exit(process),
        }

        if let Ok(Some(code)) =
            running_process_core::pty::poll_pty_process(&process.handles, &process.returncode)
        {
            return code;
        }

        if interrupted.load(Ordering::SeqCst) {
            return interrupt_pty_process(process);
        }
    }
}

fn handle_terminal_event<H: InteractiveHooks>(
    process: &NativePtyProcess,
    event: Event,
    hooks: &mut H,
) -> io::Result<()> {
    match event {
        Event::Key(key) => match translate_key_event(key, hooks.intercept_f3()) {
            KeyAction::Forward(bytes) => {
                let submit = bytes == b"\r";
                process
                    .write_impl(&bytes, submit)
                    .map_err(|err| io::Error::other(err.to_string()))?;
            }
            KeyAction::Interrupt => {
                process
                    .send_interrupt_impl()
                    .map_err(|err| io::Error::other(err.to_string()))?;
            }
            KeyAction::F3Press => hooks.on_f3_press(process)?,
            KeyAction::F3Release => hooks.on_f3_release(process)?,
            KeyAction::Ignore => {}
        },
        Event::Paste(text) => {
            process
                .write_impl(text.as_bytes(), false)
                .map_err(|err| io::Error::other(err.to_string()))?;
        }
        Event::Resize(cols, rows) => {
            process
                .resize_impl(rows, cols)
                .map_err(|err| io::Error::other(err.to_string()))?;
        }
        Event::FocusGained | Event::FocusLost | Event::Mouse(_) => {}
    }
    Ok(())
}

fn reap_pty_exit(process: &NativePtyProcess) -> i32 {
    process.wait_impl(Some(1.0)).unwrap_or(1)
}

fn interrupt_pty_process(process: &NativePtyProcess) -> i32 {
    match process.send_interrupt_impl() {
        Ok(()) => match process.wait_impl(Some(2.0)) {
            Ok(code) => {
                eprintln!("[clud] interrupted via Ctrl+C (pty)");
                code
            }
            Err(_) => {
                let _ = process.close_impl();
                eprintln!("[clud] interrupted via Ctrl+C (pty)");
                130
            }
        },
        Err(_) => {
            let _ = process.close_impl();
            eprintln!("[clud] interrupted via Ctrl+C (pty)");
            130
        }
    }
}

fn translate_key_event(key: KeyEvent, intercept_f3: bool) -> KeyAction {
    let is_release = matches!(key.kind, KeyEventKind::Release);
    match key.code {
        KeyCode::F(3) if intercept_f3 && is_release => KeyAction::F3Release,
        KeyCode::F(3) if intercept_f3 => KeyAction::F3Press,
        _ if is_release => KeyAction::Ignore,
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
        KeyCode::F(n) => translate_function_key(n),
        KeyCode::Null | KeyCode::CapsLock | KeyCode::ScrollLock | KeyCode::NumLock => {
            KeyAction::Ignore
        }
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
    let mut buf = [0u8; 4];
    bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
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

fn translate_function_key(n: u8) -> KeyAction {
    let seq = match n {
        1 => Some("\x1bOP"),
        2 => Some("\x1bOQ"),
        3 => Some("\x1bOR"),
        4 => Some("\x1bOS"),
        5 => Some("\x1b[15~"),
        6 => Some("\x1b[17~"),
        7 => Some("\x1b[18~"),
        8 => Some("\x1b[19~"),
        9 => Some("\x1b[20~"),
        10 => Some("\x1b[21~"),
        11 => Some("\x1b[23~"),
        12 => Some("\x1b[24~"),
        _ => None,
    };
    match seq {
        Some(seq) => KeyAction::Forward(seq.as_bytes().to_vec()),
        None => KeyAction::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn translates_plain_chars() {
        assert_eq!(
            translate_key_event(key(KeyCode::Char('a')), false),
            KeyAction::Forward(vec![b'a'])
        );
    }

    #[test]
    fn translates_ctrl_c_to_interrupt() {
        assert_eq!(
            translate_key_event(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                false
            ),
            KeyAction::Interrupt
        );
    }

    #[test]
    fn reserves_f3_press() {
        assert_eq!(
            translate_key_event(key(KeyCode::F(3)), true),
            KeyAction::F3Press
        );
    }

    #[test]
    fn reserves_f3_release() {
        assert_eq!(
            translate_key_event(
                KeyEvent::new_with_kind(KeyCode::F(3), KeyModifiers::NONE, KeyEventKind::Release),
                true
            ),
            KeyAction::F3Release
        );
    }

    #[test]
    fn ignores_releases_for_non_voice_keys() {
        assert_eq!(
            translate_key_event(
                KeyEvent::new_with_kind(
                    KeyCode::Char('a'),
                    KeyModifiers::NONE,
                    KeyEventKind::Release
                ),
                false
            ),
            KeyAction::Ignore
        );
    }

    #[test]
    fn translates_arrow_keys() {
        assert_eq!(
            translate_key_event(key(KeyCode::Left), false),
            KeyAction::Forward(b"\x1b[D".to_vec())
        );
    }

    #[test]
    fn translates_alt_chars() {
        assert_eq!(
            translate_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT), false),
            KeyAction::Forward(vec![0x1b, b'x'])
        );
    }

    #[test]
    fn forwards_f3_when_not_intercepted() {
        assert_eq!(
            translate_key_event(key(KeyCode::F(3)), false),
            KeyAction::Forward(b"\x1bOR".to_vec())
        );
    }
}
