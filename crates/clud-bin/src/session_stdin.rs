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

pub(super) fn stdin_chunk_requests_interrupt(chunk: &[u8]) -> bool {
    chunk.contains(&0x03)
}
