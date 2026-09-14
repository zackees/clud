use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;
use crate::toast::statusline::{read_live_toast, state_path, StatusStateWriter};
use crate::toast::{Severity, Toast, ToastEvent, ToastHub};

const ROWS: u16 = 24;
const COLS: u16 = 80;
const IMAGE: u32 = 77;

fn compositor(tier: ToastTier, fallback: Fallback) -> (Compositor, Arc<ToastHub>) {
    let hub = ToastHub::new();
    let c = Compositor::new(ToastPumpOptions {
        hub: Arc::clone(&hub),
        tier,
        fallback,
        rows: ROWS,
        cols: COLS,
        image_id: IMAGE,
    });
    (c, hub)
}

fn show(hub: &ToastHub, text: &str, now: Instant) {
    hub.publish(ToastEvent::Show(Toast::new(
        "cpu",
        text,
        Severity::Warn,
        now,
    )));
}

fn apcs(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    text.split("\x1b_G")
        .skip(1)
        .map(|rest| rest.split("\x1b\\").next().unwrap_or("").to_string())
        .collect()
}

fn has(bytes: &[u8], needle: &str) -> bool {
    String::from_utf8_lossy(bytes).contains(needle)
}

fn count(bytes: &[u8], action: &str) -> usize {
    apcs(bytes)
        .iter()
        .filter(|cmd| {
            cmd.split(';')
                .next()
                .unwrap()
                .split(',')
                .any(|kv| kv == action)
        })
        .count()
}

// ── kitty tier ────────────────────────────────────────────────────────────

#[test]
fn kitty_toast_is_sent_placed_with_q2_and_the_cursor_is_restored() {
    let now = Instant::now();
    let (mut c, hub) = compositor(ToastTier::Kitty, Fallback::None);
    let echo = c.on_child(b"hello\r\nworld", now);
    assert_eq!(echo, b"hello\r\nworld", "nothing to draw yet");
    show(&hub, "cpu 287 %", now);
    let out = c.on_tick(now);
    assert_eq!(count(&out, "a=t"), 1);
    assert_eq!(count(&out, "a=p"), 1);
    for cmd in apcs(&out) {
        let control = cmd.split(';').next().unwrap();
        assert!(
            control.split(',').any(|kv| kv == "q=2"),
            "missing q=2: {control}"
        );
    }
    let text = String::from_utf8_lossy(&out);
    let place_at = text.find("a=p").unwrap();
    assert!(
        text[..place_at].contains("\x1b[1;"),
        "placement must be preceded by a CUP to row 1"
    );
    assert!(
        text.ends_with("\x1b[2;6H\x1b[?25h"),
        "cursor must return to (2,6) and be re-shown: {text:?}"
    );
}

#[test]
fn child_bytes_are_forwarded_first_and_the_placement_is_re_asserted_without_resending() {
    let now = Instant::now();
    let (mut c, hub) = compositor(ToastTier::Kitty, Fallback::None);
    show(&hub, "hot", now);
    let first = c.on_tick(now);
    assert_eq!(count(&first, "a=t"), 1);
    for burst in [&b"abc"[..], &b"\r\nnext line"[..]] {
        let out = c.on_child(burst, now);
        assert!(out.starts_with(burst), "child bytes must lead the write");
        assert_eq!(count(&out, "a=t"), 0, "image is uploaded once");
        assert_eq!(
            count(&out, "a=p"),
            1,
            "placement is pinned after every burst"
        );
    }
}

#[test]
fn a_clear_reset_or_alt_switch_resends_the_image() {
    for wipe in [&b"\x1b[2J"[..], &b"\x1bc"[..], &b"\x1b[?1049h"[..]] {
        let now = Instant::now();
        let (mut c, hub) = compositor(ToastTier::Kitty, Fallback::None);
        show(&hub, "hot", now);
        c.on_tick(now);
        let out = c.on_child(wipe, now);
        assert_eq!(count(&out, "a=t"), 1, "wipe {wipe:?} must resend");
        assert_eq!(count(&out, "a=p"), 1);
    }
}

#[test]
fn a_changed_text_replaces_the_image() {
    let now = Instant::now();
    let (mut c, hub) = compositor(ToastTier::Kitty, Fallback::None);
    show(&hub, "cpu 100 %", now);
    c.on_tick(now);
    show(&hub, "cpu 200 %", now);
    let out = c.on_tick(now);
    assert!(apcs(&out).iter().any(|cmd| cmd.starts_with("a=d,d=I")));
    assert_eq!(count(&out, "a=t"), 1);
}

#[test]
fn expiry_deletes_the_placement_and_repaints_nothing() {
    let now = Instant::now();
    let (mut c, hub) = compositor(ToastTier::Kitty, Fallback::None);
    c.on_child(b"some text", now);
    hub.publish(ToastEvent::Show(
        Toast::new("done", "back to normal", Severity::Info, now)
            .expiring_after(Duration::from_secs(1)),
    ));
    c.on_tick(now);
    let out = c.on_tick(now + Duration::from_secs(2));
    assert!(apcs(&out).iter().any(|cmd| cmd.starts_with("a=d,d=i")));
    assert_eq!(
        String::from_utf8_lossy(&out).matches("\x1b[").count(),
        0,
        "the text layer is untouched: no CSI at all"
    );
    assert!(!c.wants_tick());
    let freed = c.finish();
    assert!(apcs(&freed).iter().any(|cmd| cmd.starts_with("a=d,d=I")));
}

// ── safe injection ────────────────────────────────────────────────────────

#[test]
fn injection_waits_for_the_end_of_a_split_escape_sequence() {
    let now = Instant::now();
    let (mut c, hub) = compositor(ToastTier::Kitty, Fallback::None);
    show(&hub, "hot", now);
    let out = c.on_child(b"text\x1b[3", now);
    assert_eq!(out, b"text\x1b[3", "nothing may follow a partial CSI");
    assert_eq!(c.on_tick(now + Duration::from_secs(5)), Vec::<u8>::new());
    let out = c.on_child(b"1mX", now);
    assert!(out.starts_with(b"1mX"));
    assert_eq!(count(&out, "a=p"), 1);
}

#[test]
fn a_synchronized_update_holds_the_toast_until_it_ends_or_times_out() {
    let now = Instant::now();
    let (mut c, hub) = compositor(ToastTier::Kitty, Fallback::None);
    show(&hub, "hot", now);
    assert_eq!(c.on_child(b"\x1b[?2026hframe", now), b"\x1b[?2026hframe");
    assert!(c.on_tick(now + Duration::from_millis(100)).is_empty());
    let late = c.on_tick(now + SYNC_DEFER_LIMIT + Duration::from_millis(10));
    assert_eq!(
        count(&late, "a=p"),
        1,
        "an abandoned block cannot hold a toast forever"
    );

    let (mut c, hub) = compositor(ToastTier::Kitty, Fallback::None);
    show(&hub, "hot", now);
    c.on_child(b"\x1b[?2026hframe", now);
    let out = c.on_child(b" done\x1b[?2026l", now);
    assert_eq!(count(&out, "a=p"), 1);
}

#[test]
fn a_cursor_in_the_last_column_defers_injection() {
    let now = Instant::now();
    let (mut c, hub) = compositor(ToastTier::Kitty, Fallback::None);
    show(&hub, "hot", now);
    let row = "x".repeat(usize::from(COLS));
    let out = c.on_child(row.as_bytes(), now);
    assert_eq!(
        out,
        row.as_bytes(),
        "a CUP now would cancel the pending wrap"
    );
    let out = c.on_child(b"\r\n", now);
    assert_eq!(count(&out, "a=p"), 1);
}

#[test]
fn origin_mode_is_suspended_and_the_cursor_restore_is_region_relative() {
    let now = Instant::now();
    let (mut c, hub) = compositor(ToastTier::Kitty, Fallback::None);
    c.on_child(b"\x1b[5;20r\x1b[?6h\x1b[3;4H", now);
    show(&hub, "hot", now);
    let out = c.on_tick(now);
    let text = String::from_utf8_lossy(&out);
    let off = text.find("\x1b[?6l").expect("DECOM must be switched off");
    assert!(off < text.find("a=p").unwrap());
    assert!(
        text.contains("\x1b[?6h\x1b[3;4H"),
        "DECOM back on, then a region-relative CUP: {text:?}"
    );
}

// ── text-cell tier ────────────────────────────────────────────────────────

fn terminal() -> vt100::Parser {
    vt100::Parser::new(ROWS, COLS, 0)
}

#[test]
fn a_text_toast_is_drawn_on_the_alternate_screen_and_restored_exactly() {
    let now = Instant::now();
    let (mut c, hub) = compositor(ToastTier::TextCells, Fallback::None);
    let mut term = terminal();
    let setup = b"\x1b[?1049h\x1b[1;1H\x1b[32mgreen status\x1b[0m\x1b[1;50Hright side words\x1b[5;3Hprompt> ";
    term.process(&c.on_child(setup, now));
    hub.publish(ToastEvent::Show(
        Toast::new("cpu", "cpu 300 %", Severity::Alert, now).expiring_after(Duration::from_secs(5)),
    ));
    term.process(&c.on_tick(now));
    let top: String = term.screen().rows(0, COLS).next().unwrap();
    assert!(top.contains("cpu 300 %"), "toast missing: {top:?}");
    assert_eq!(
        term.screen().cursor_position(),
        c.shadow().cursor_position()
    );

    // The child draws under the toast while it is visible.
    term.process(&c.on_child(b"\x1b[1;72HXYZ\x1b[5;11Htyped", now));
    let top: String = term.screen().rows(0, COLS).next().unwrap();
    assert!(
        top.contains("cpu 300 %"),
        "toast must survive the child's write"
    );

    term.process(&c.on_tick(now + Duration::from_secs(6)));
    assert_eq!(term.screen().contents(), c.shadow().contents());
    assert_eq!(
        term.screen().cursor_position(),
        c.shadow().cursor_position()
    );
    for col in 0..COLS {
        let (a, b) = (
            term.screen().cell(0, col).unwrap(),
            c.shadow().cell(0, col).unwrap(),
        );
        assert_eq!(
            (a.contents(), a.fgcolor(), a.bgcolor()),
            (b.contents(), b.fgcolor(), b.bgcolor()),
            "col {col}"
        );
    }
}

#[test]
fn a_scroll_under_a_text_toast_leaves_no_toast_cells_behind() {
    let now = Instant::now();
    let (mut c, hub) = compositor(ToastTier::TextCells, Fallback::None);
    let mut term = terminal();
    let mut setup = b"\x1b[?1049h".to_vec();
    for row in 1..=ROWS {
        setup.extend_from_slice(format!("\x1b[{row};1Hline {row}").as_bytes());
    }
    term.process(&c.on_child(&setup, now));
    hub.publish(ToastEvent::Show(
        Toast::new("cpu", "hot", Severity::Warn, now).expiring_after(Duration::from_secs(5)),
    ));
    term.process(&c.on_tick(now));
    term.process(&c.on_child(b"\x1b[3T", now));
    term.process(&c.on_child(b"\x1b[2S\x1b[4;1Hmore", now));
    term.process(&c.on_tick(now + Duration::from_secs(6)));
    assert_eq!(term.screen().contents(), c.shadow().contents());
}

#[test]
fn a_burst_that_starts_mid_sequence_repaints_the_whole_screen() {
    let now = Instant::now();
    let (mut c, hub) = compositor(ToastTier::TextCells, Fallback::None);
    let mut term = terminal();
    term.process(&c.on_child(b"\x1b[?1049h\x1b[1;1Hbase", now));
    hub.publish(ToastEvent::Show(
        Toast::new("cpu", "hot", Severity::Warn, now).expiring_after(Duration::from_secs(5)),
    ));
    term.process(&c.on_tick(now));
    term.process(&c.on_child(b"\x1b[3", now));
    term.process(&c.on_child(b"T\x1b[1;1Hafter", now));
    term.process(&c.on_tick(now + Duration::from_secs(6)));
    assert_eq!(term.screen().contents(), c.shadow().contents());
}

#[test]
fn on_the_main_screen_text_tier_uses_the_title_fallback() {
    let now = Instant::now();
    let (mut c, hub) = compositor(ToastTier::TextCells, Fallback::Title);
    c.on_child(b"inline prompt", now);
    hub.publish(ToastEvent::Show(
        Toast::new("cpu", "hot", Severity::Warn, now).expiring_after(Duration::from_secs(1)),
    ));
    let out = c.on_tick(now);
    assert!(has(&out, "\x1b[22;0t"), "title must be pushed");
    assert!(has(&out, "\x1b]2;clud \u{b7} hot\x07"));
    assert!(
        !has(&out, "\x1b[1;"),
        "the title fallback never moves the cursor"
    );
    let out = c.on_tick(now + Duration::from_secs(2));
    assert_eq!(out, b"\x1b[23;0t", "expiry pops the saved title");
}

#[test]
fn the_status_file_fallback_mirrors_and_clears_the_toast() {
    let now = Instant::now();
    let dir = tempfile::tempdir().unwrap();
    let path = state_path(dir.path(), 4242);
    let writer = Arc::new(StatusStateWriter::new(path.clone()));
    let (mut c, hub) = compositor(
        ToastTier::Fallback,
        Fallback::StatusFile(Arc::clone(&writer)),
    );
    show(&hub, "cpu 150 %", now);
    assert!(
        c.on_tick(now).is_empty(),
        "the status file surface writes no terminal bytes"
    );
    let live = read_live_toast(&path, crate::toast::statusline::now_ms()).unwrap();
    assert_eq!(live.text, "cpu 150 %");
    hub.publish(ToastEvent::Close { key: "cpu".into() });
    c.on_tick(now);
    assert!(read_live_toast(&path, crate::toast::statusline::now_ms()).is_none());
}

// ── dismiss, resize, off ──────────────────────────────────────────────────

#[test]
fn click_to_dismiss_is_armed_only_when_the_child_reports_sgr_mouse() {
    let now = Instant::now();
    let (mut c, hub) = compositor(ToastTier::Kitty, Fallback::None);
    let input = c.input();
    show(&hub, "hot", now);
    c.on_tick(now);
    assert!(
        input.close_rect().is_none(),
        "clud must not rely on mouse the child did not enable"
    );
    c.on_child(b"\x1b[?1000h\x1b[?1006h", now);
    let rect = input
        .close_rect()
        .expect("armed once the child reports SGR mouse");
    assert_eq!(rect.row, 0);
    assert!(rect.col + rect.width < COLS);
    hub.dismiss_visible(now);
    let out = c.on_tick(now);
    assert!(apcs(&out).iter().any(|cmd| cmd.starts_with("a=d,d=i")));
    assert!(input.close_rect().is_none());
}

#[test]
fn a_resize_re_pins_the_toast_to_the_new_top_right() {
    let now = Instant::now();
    let (mut c, hub) = compositor(ToastTier::Kitty, Fallback::None);
    show(&hub, "hot", now);
    let before = String::from_utf8_lossy(&c.on_tick(now)).into_owned();
    let after = String::from_utf8_lossy(&c.on_resize(30, 120, now)).into_owned();
    let col = |s: &str| {
        let i = s.find("a=p").unwrap();
        let cup = s[..i].rfind("\x1b[1;").unwrap();
        s[cup + 4..i]
            .split('H')
            .next()
            .unwrap()
            .parse::<u16>()
            .unwrap()
    };
    assert!(col(&after) > col(&before));
}

#[test]
fn tier_off_is_a_verbatim_pass_through() {
    let now = Instant::now();
    let (mut c, hub) = compositor(ToastTier::Off, Fallback::Title);
    show(&hub, "hot", now);
    assert_eq!(c.on_child(b"\x1b[2Jdata", now), b"\x1b[2Jdata");
    assert!(c.on_tick(now).is_empty());
    assert!(!c.wants_tick());
}
