//! Input-source decisions and byte normalization for the raw PTY pump.

pub(super) fn stdin_source_is_real_stdin<R: 'static>() -> bool {
    std::any::TypeId::of::<R>() == std::any::TypeId::of::<std::io::Stdin>()
}

pub(super) fn should_normalize_interactive_console_stdin(interactive_real_stdin: bool) -> bool {
    cfg!(windows) && interactive_real_stdin
}

pub(super) fn should_spawn_byte_stream_stdin_reader(
    interactive_real_stdin: bool,
    has_extra_rx: bool,
) -> bool {
    !(cfg!(windows) && interactive_real_stdin && has_extra_rx)
}

pub(super) fn normalize_interactive_console_stdin_chunk(chunk: &mut [u8]) {
    if cfg!(windows) {
        for byte in chunk {
            if *byte == 0x08 {
                *byte = 0x7f;
            }
        }
    }
}

/// True when a stdin chunk carries a keyboard interrupt request.
///
/// Two encodings count:
///
/// * Legacy `0x03` — what a terminal sends for Ctrl+C in raw mode when no
///   keyboard-protocol enhancements are in play. This is the normal path.
/// * Kitty keyboard-protocol CSI u — `\x1b[99;5u`, plus the explicit
///   `;5:1u` (press) and `;5:2u` (autorepeat) spellings. A terminal only
///   emits these once something has pushed `DISAMBIGUATE_ESCAPE_CODES`,
///   which clud no longer does (see `KEYBOARD_ENHANCEMENT_FLAGS`).
///
/// The CSI u branch is a backstop, not the fix for issue #1101 — the fix is
/// not pushing the flag. It is here because the failure mode was so bad:
/// with Ctrl+C re-encoded, `contains(&0x03)` never matched, raw mode had
/// already cleared `ISIG` so no SIGINT reached the `ctrlc` handler either,
/// and a 200-iteration `clud grind` could only be stopped by closing the
/// terminal window. A child TUI that pushes the flag for its own use, or a
/// future change to the flags above, must not be able to take Ctrl+C away
/// from the user a second time.
///
/// Release events (`:3`) deliberately do not count: the press or repeat
/// that preceded them already did.
///
/// Stateless by design, and therefore blind to a sequence split across two
/// reads. That is an acceptable trade for an interrupt — a terminal writes
/// one keystroke as one `write`, and a key held long enough to matter
/// repeats, so a torn first chunk is followed by an intact one within
/// milliseconds.
pub(super) fn stdin_chunk_requests_interrupt(chunk: &[u8]) -> bool {
    chunk.contains(&0x03) || contains_csi_u_ctrl_c(chunk)
}

/// Scan for a kitty CSI u encoding of Ctrl+C anywhere in the chunk.
///
/// Walks `\x1b[` … final-byte sequences, checking the parameter bytes of
/// every one that terminates in `u`. A truncated trailing sequence (no
/// final byte yet) is not a match; the next chunk carries the rest.
fn contains_csi_u_ctrl_c(chunk: &[u8]) -> bool {
    let mut rest = chunk;
    while let Some(start) = rest.windows(2).position(|pair| pair == b"\x1b[") {
        let params = &rest[start + 2..];
        let Some(end) = params
            .iter()
            .position(|byte| super::is_csi_terminator(*byte))
        else {
            return false;
        };
        if params[end] == b'u' && csi_u_params_are_ctrl_c(&params[..end]) {
            return true;
        }
        rest = &params[end + 1..];
    }
    false
}

/// Decide whether a CSI u parameter payload (e.g. `99;5:2`) is Ctrl+C.
///
/// The kitty encoding is `CSI <key>[:<shifted>:<base>] ; <mods>[:<event>] u`,
/// where `<mods>` is a modifier bitfield **plus one**: shift 1, alt 2,
/// ctrl 4, super 8, and so on.
fn csi_u_params_are_ctrl_c(params: &[u8]) -> bool {
    /// Ctrl's bit in the (already decremented) kitty modifier bitfield.
    const CTRL_BIT: u32 = 4;
    /// Kitty event type 3 is a key release.
    const EVENT_RELEASE: u32 = 3;

    let Ok(payload) = std::str::from_utf8(params) else {
        return false;
    };
    let mut fields = payload.split(';');

    // First field is the key's unicode codepoint, optionally followed by
    // `:shifted:base` alternates. Ctrl+C reports `c` (99); with shift also
    // held a terminal reports `C` (67) and puts `c` in the alternates, so
    // accept either rather than making Ctrl+Shift+C fail to interrupt.
    let key = fields
        .next()
        .and_then(|field| field.split(':').next())
        .and_then(|code| code.parse::<u32>().ok());
    if !matches!(key, Some(99) | Some(67)) {
        return false;
    }

    // Second field is `modifiers[:event-type]`. Absent means no modifiers,
    // i.e. a bare `c` — not an interrupt.
    let Some(modifier_field) = fields.next() else {
        return false;
    };
    let mut modifier_parts = modifier_field.split(':');
    let Some(modifiers) = modifier_parts
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .and_then(|value| value.checked_sub(1))
    else {
        return false;
    };
    if modifiers & CTRL_BIT == 0 {
        return false;
    }

    let event_type = modifier_parts
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(1);
    event_type != EVENT_RELEASE
}
