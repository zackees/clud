use super::*;

/// Spawn a short-lived sleep-like command in a PTY so we have a live
/// master handle to test resize_pty against. Returns `None` if the
/// host environment can't allocate a PTY (nested Windows shells where
/// ConPTY spawn silently no-ops; see issue #31 notes).
fn spawn_idle_pty(rows: u16, cols: u16) -> Option<NativePtyProcess> {
    let argv: Vec<String> = if cfg!(windows) {
        // `ping -n 3 127.0.0.1` keeps the child alive ~2s without needing
        // a console for stdout, which is enough for a resize roundtrip.
        vec![
            "cmd.exe".into(),
            "/c".into(),
            "ping -n 3 127.0.0.1 > NUL".into(),
        ]
    } else {
        vec!["/bin/sh".into(), "-c".into(), "sleep 2".into()]
    };
    let process = NativePtyProcess::new(argv, None, None, rows, cols, None).ok()?;
    process.set_echo(false);
    process.start_impl().ok()?;
    Some(process)
}

/// resize_pty should change the master's reported size on every platform,
/// including Windows (where the library's own `resize_impl` is a no-op).
/// Issue #31, theory T2.
#[test]
fn resize_pty_updates_master_size_on_all_platforms() {
    let Some(process) = spawn_idle_pty(20, 80) else {
        eprintln!("resize_pty_updates_master_size_on_all_platforms: SKIP (PTY spawn unavailable)");
        return;
    };

    // Sanity: the master reports the initial size we requested.
    {
        let guard = process.handles.lock().expect("handles");
        let handles = guard.as_ref().expect("handles present");
        let before = handles.master.get_size().expect("get_size");
        assert_eq!(
            (before.rows, before.cols),
            (20, 80),
            "initial master size wrong: {:?}",
            before
        );
    }

    // Resize via the helper and verify the master advances.
    resize_pty(&process, 40, 120).expect("resize_pty");

    {
        let guard = process.handles.lock().expect("handles");
        let handles = guard.as_ref().expect("handles present");
        let after = handles.master.get_size().expect("get_size");
        assert_eq!(
            (after.rows, after.cols),
            (40, 120),
            "resize_pty did not propagate to master: {:?}",
            after
        );
    }

    let _ = process.close_impl();
}

// F3Observer — byte-level observer for voice-mode F3 press detection.
// Observer, not interceptor: bytes are still forwarded verbatim to the
// child. These tests drive Steps 1–4 of the raw-pump refactor.

#[test]
fn observer_passes_arbitrary_bytes_through_without_detecting_f3() {
    // Random bytes, DSR, paste chunks, newlines — none of these should
    // register as F3 presses. The observer doesn't modify bytes; tests
    // here only assert the count of events it reports.
    let mut obs = F3Observer::new();
    assert_eq!(obs.observe(b"\x1b[6n").presses, 0, "DSR query is not F3");
    assert_eq!(obs.observe(b"hello\n").presses, 0);
    assert_eq!(obs.observe(b"\x03").presses, 0, "raw Ctrl+C byte is not F3");
    let smoke: Vec<u8> = (0..=255u8).collect();
    // The smoke vector happens to contain \x1b,O,R bytes somewhere, but
    // they are not adjacent in that order, so no press should fire.
    assert_eq!(obs.observe(&smoke).presses, 0);
}

#[test]
fn observer_detects_single_and_multiple_f3_presses() {
    let mut obs = F3Observer::new();
    assert_eq!(obs.observe(b"\x1bOR").presses, 1);
    let mut obs = F3Observer::new();
    assert_eq!(obs.observe(b"hello\x1bORworld").presses, 1);
    let mut obs = F3Observer::new();
    assert_eq!(obs.observe(b"\x1bOR\x1bOR\x1bOR").presses, 3);
}

#[test]
fn observer_detects_f3_across_fragmented_reads() {
    // 2-way split: \x1b | OR
    let mut obs = F3Observer::new();
    let mut total = 0;
    total += obs.observe(b"\x1b").presses;
    total += obs.observe(b"OR").presses;
    assert_eq!(total, 1, "2-way split should still detect one press");

    // 3-way split: \x1b | O | R
    let mut obs = F3Observer::new();
    let mut total = 0;
    for chunk in [&b"\x1b"[..], &b"O"[..], &b"R"[..]] {
        total += obs.observe(chunk).presses;
    }
    assert_eq!(total, 1, "3-way split should still detect one press");

    // Broken prefix then a clean press later: only the clean one counts.
    let mut obs = F3Observer::new();
    let mut total = 0;
    total += obs.observe(b"\x1b").presses;
    total += obs.observe(b"XYZ").presses; // breaks the prefix, X is not O
    total += obs.observe(b"\x1bOR").presses;
    assert_eq!(total, 1);
}

#[test]
fn observer_ignores_non_f3_escapes() {
    let mut obs = F3Observer::new();
    assert_eq!(obs.observe(b"\x1b[6n").presses, 0, "DSR");
    assert_eq!(obs.observe(b"\x1bOA").presses, 0, "SS3 up arrow");
    assert_eq!(obs.observe(b"\x1bOP").presses, 0, "F1 (SS3 P)");
    assert_eq!(
        obs.observe(b"\x1bOX\x1bOR tail").presses,
        1,
        "valid F3 after a bogus SS3 prefix should still count"
    );
}

// ─── Kitty keyboard-protocol release / repeat events ──────────────────
// Issue #13 hold-to-record uses release events when terminals support
// the kitty protocol. Three F3 encodings can carry release info:
//   * CSI tilde with event-type:    `\x1b[13;1:3~`
//   * CSI u (numeric):               `\x1b[13;1:3u`
//   * CSI u (functional encoding):   `\x1b[57346;1:3u`
// Repeats (event-type 2) are intentionally silent — they signal the
// key is still held and would otherwise spam the voice state machine.

#[test]
fn observer_detects_csi_tilde_f3_press() {
    let mut obs = F3Observer::new();
    let events = obs.observe(b"\x1b[13~");
    assert_eq!(events.presses, 1);
    assert_eq!(events.releases, 0);
}

#[test]
fn observer_detects_kitty_csi_u_press_and_release() {
    // Press then release via CSI u with the keycode-13 form.
    let mut obs = F3Observer::new();
    let events = obs.observe(b"\x1b[13;1:1u\x1b[13;1:3u");
    assert_eq!(events.presses, 1);
    assert_eq!(events.releases, 1);
}

#[test]
fn observer_detects_kitty_functional_encoding_release() {
    // F3 functional-encoding keycode is 57346 in the kitty protocol.
    let mut obs = F3Observer::new();
    let events = obs.observe(b"\x1b[57346;1:3u");
    assert_eq!(events.releases, 1);
    assert_eq!(events.presses, 0);
}

#[test]
fn observer_ignores_kitty_repeat_event() {
    // event-type 2 = autorepeat. Must NOT be counted as a fresh press —
    // doing so would tear down the recording the user is still holding.
    let mut obs = F3Observer::new();
    let events = obs.observe(b"\x1b[13;1:2~");
    assert_eq!(events.presses, 0);
    assert_eq!(events.releases, 0);
}

#[test]
fn observer_handles_release_split_across_reads() {
    // Same fragmentation tolerance as the legacy SS3 path: a release
    // sequence chopped one byte at a time must still register exactly
    // once.
    let mut obs = F3Observer::new();
    let mut presses = 0;
    let mut releases = 0;
    for &b in b"\x1b[13;1:3~" {
        let ev = obs.observe(&[b]);
        presses += ev.presses;
        releases += ev.releases;
    }
    assert_eq!(presses, 0);
    assert_eq!(releases, 1);
}

#[test]
fn observer_ignores_non_f3_csi_sequences() {
    // Other CSI sequences must not be mis-attributed to F3.
    let mut obs = F3Observer::new();
    assert_eq!(obs.observe(b"\x1b[1~").presses, 0, "Home (CSI 1~)");
    assert_eq!(obs.observe(b"\x1b[15~").presses, 0, "F5 (CSI 15~)");
    assert_eq!(obs.observe(b"\x1b[57347u").presses, 0, "F4 functional");
    assert_eq!(obs.observe(b"\x1b[6n").presses, 0, "DSR (still no F3)");
}

#[test]
fn console_stdin_normalization_is_windows_only() {
    let mut chunk = vec![b'a', 0x08, 0x7f, b'z'];
    normalize_interactive_console_stdin_chunk(&mut chunk);
    if cfg!(windows) {
        assert_eq!(chunk, vec![b'a', 0x7f, 0x7f, b'z']);
    } else {
        assert_eq!(chunk, vec![b'a', 0x08, 0x7f, b'z']);
    }
}

#[test]
fn ctrl_c_byte_requests_interrupt() {
    assert!(!stdin_chunk_requests_interrupt(b"abc"));
    assert!(stdin_chunk_requests_interrupt(b"a\x03c"));
}

#[test]
fn only_real_stdin_gets_interactive_console_policy() {
    assert!(stdin_source_is_real_stdin::<std::io::Stdin>());
    assert!(!stdin_source_is_real_stdin::<std::io::Cursor<Vec<u8>>>());
}

// ─── should_spawn_byte_stream_stdin_reader (issue #188) ─────────────

/// Issue #188 GREEN: Windows + interactive console + extra_rx wired
/// suppresses the byte-stream stdin reader so the `console_input`
/// `ReadConsoleInputW` worker is the sole consumer of the STDIN
/// console queue. Without this gate, the byte-stream reader's
/// `ReadFile` call races with `ReadConsoleInputW` on the same queue
/// and Shift+Enter events surface as `\r` instead of `\n`.
#[cfg(windows)]
#[test]
fn windows_interactive_with_extra_rx_suppresses_byte_stream_reader() {
    assert!(!should_spawn_byte_stream_stdin_reader(true, true));
}

/// Issue #188: without an `extra_rx`, nothing else is consuming the
/// console queue, so the byte-stream reader must run — otherwise no
/// keystrokes reach the child at all.
#[test]
fn no_extra_rx_keeps_byte_stream_reader() {
    assert!(should_spawn_byte_stream_stdin_reader(true, false));
    assert!(should_spawn_byte_stream_stdin_reader(false, false));
}

/// Issue #188: piped stdin (`echo "..." | clud`) is not a console
/// queue at all — `ReadConsoleInputW` can't function on a pipe handle
/// — so the byte-stream reader must run even when an `extra_rx` is
/// supplied. The `interactive_real_stdin` gate keys on
/// `terminals_are_interactive()`.
#[test]
fn piped_stdin_keeps_byte_stream_reader_even_with_extra_rx() {
    assert!(should_spawn_byte_stream_stdin_reader(false, true));
}

/// Issue #188: POSIX has no conhost / `ReadFile` modifier-stripping
/// race, so the suppression must not apply there — the gate is
/// `cfg!(windows)`-scoped.
#[cfg(not(windows))]
#[test]
fn posix_keeps_byte_stream_reader_even_with_extra_rx() {
    assert!(should_spawn_byte_stream_stdin_reader(true, true));
}

// ─── BracketedPasteNormalizer (issue #63 / #79) ────────────────────

#[test]
fn paste_normalizer_passthrough_bytes_outside_paste() {
    let mut p = BracketedPasteNormalizer::new();
    // Plain typing — no PASTE_START seen — passes through verbatim.
    let out = p.process(b"hello world\n");
    assert_eq!(out, b"hello world\n");
}

/// session_paste_normalizes_path_on_drop — when a bracketed paste
/// arrives whose body looks like a dragged path, the body must be
/// rewritten through `normalize_dropped_path` before forwarding.
#[test]
fn session_paste_normalizes_path_on_drop() {
    let mut p = BracketedPasteNormalizer::new();
    // GNOME-Terminal-style file URI drop. Should canonicalize to
    // the platform-appropriate path form.
    let chunk = b"\x1b[200~file:///home/me/my%20file.txt\x1b[201~";
    let out = p.process(chunk);
    // Both POSIX and Windows must wrap output in bracketed-paste
    // markers and percent-decode the URI.
    assert!(out.starts_with(PASTE_START), "must keep PASTE_START");
    assert!(out.ends_with(PASTE_END), "must keep PASTE_END");
    let inner_start = PASTE_START.len();
    let inner_end = out.len() - PASTE_END.len();
    let inner = std::str::from_utf8(&out[inner_start..inner_end]).expect("utf8");
    assert!(
        inner.contains("my file.txt"),
        "inner must be percent-decoded; got {inner:?}"
    );
    // The original URI scheme is gone (normalized form is a path,
    // not a URI).
    assert!(!inner.contains("file://"), "URI must be stripped");
}

/// session_paste_passthrough_for_non_path_text — a paste of plain
/// text (e.g. a code snippet) must be forwarded VERBATIM with the
/// PASTE_START / PASTE_END markers preserved.
#[test]
fn session_paste_passthrough_for_non_path_text() {
    let mut p = BracketedPasteNormalizer::new();
    let chunk = b"\x1b[200~hello world\x1b[201~";
    let out = p.process(chunk);
    // No path → exact passthrough.
    assert_eq!(out, b"\x1b[200~hello world\x1b[201~");
}

#[test]
fn paste_normalizer_multiline_paste_with_path_first_line_is_passthrough() {
    // A multi-line paste whose first line happens to look like a
    // path must not have the path-rewrite applied. The whole-buffer
    // `looks_like_dropped_path` check handles this — multi-line
    // strings don't match the heuristic.
    let mut p = BracketedPasteNormalizer::new();
    let chunk = b"\x1b[200~/Users/me/x.txt\nlet x = 1;\x1b[201~";
    let out = p.process(chunk);
    assert_eq!(out, chunk);
}

#[test]
fn paste_normalizer_handles_split_chunks() {
    // PASTE_START split across two chunks, body in a third, end in a
    // fourth. The detector must reassemble correctly.
    let mut p = BracketedPasteNormalizer::new();
    let mut all = Vec::new();
    all.extend_from_slice(&p.process(b"abc\x1b[2"));
    all.extend_from_slice(&p.process(b"00~"));
    all.extend_from_slice(&p.process(b"hello"));
    all.extend_from_slice(&p.process(b"\x1b[201~tail"));
    assert_eq!(all, b"abc\x1b[200~hello\x1b[201~tail");
}

#[test]
fn paste_normalizer_broken_start_prefix_is_flushed() {
    // \x1b[2 then a non-matching byte — the partial prefix should
    // be forwarded verbatim, not swallowed.
    let mut p = BracketedPasteNormalizer::new();
    let out = p.process(b"\x1b[2X");
    assert_eq!(out, b"\x1b[2X");
}

#[test]
fn paste_normalizer_two_pastes_back_to_back() {
    // Two pastes, neither path-shaped, should pass through cleanly.
    let mut p = BracketedPasteNormalizer::new();
    let out = p.process(b"\x1b[200~foo\x1b[201~bar\x1b[200~baz\x1b[201~");
    assert_eq!(out, b"\x1b[200~foo\x1b[201~bar\x1b[200~baz\x1b[201~");
}
