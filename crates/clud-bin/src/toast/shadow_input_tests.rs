use super::*;

const ROWS: u16 = 6;
const COLS: u16 = 40;

fn translated(chunks: &[&[u8]]) -> vt100::Parser {
    let mut parser = vt100::Parser::new(ROWS, COLS, 0);
    let mut feed = ShadowFeed::default();
    for chunk in chunks {
        feed.feed(&mut parser, chunk);
    }
    parser
}

fn direct(bytes: &[u8]) -> vt100::Parser {
    let mut parser = vt100::Parser::new(ROWS, COLS, 0);
    parser.process(bytes);
    parser
}

fn assert_same(got: &vt100::Parser, want: &vt100::Parser, case: &str) {
    assert_eq!(
        got.screen().contents(),
        want.screen().contents(),
        "{case}: cells"
    );
    assert_eq!(
        got.screen().cursor_position(),
        want.screen().cursor_position(),
        "{case}: cursor"
    );
}

#[test]
fn sequences_vt100_drops_land_where_their_equivalents_do() {
    let cases: &[(&str, &[u8], &[u8])] = &[
        ("REP", b"ab\x1b[3bX", b"abbbbX"),
        ("REP box drawing", "\u{2500}\x1b[4b|".as_bytes(), "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}|".as_bytes()),
        ("HPA", b"hello\x1b[20`X", b"hello\x1b[20GX"),
        ("HPR", b"ab\x1b[5aX", b"ab\x1b[5CX"),
        ("VPR", b"ab\x1b[2eX", b"ab\x1b[2BX"),
        ("HVP", b"\x1b[3;7fX", b"\x1b[3;7HX"),
        ("CHT", b"ab\x1b[2IX", b"ab\x1b[17GX"),
        ("CBT", b"\x1b[20G\x1b[ZX", b"\x1b[17GX"),
        ("CBT on a stop", b"\x1b[17G\x1b[ZX", b"\x1b[9GX"),
    ];
    for (case, stream, equivalent) in cases {
        assert_same(&translated(&[stream]), &direct(equivalent), case);
    }
}

#[test]
fn without_translation_the_shadow_drifts() {
    // The premise of #2028: vt100 really does drop these.
    let raw = direct(b"ab\x1b[3bX");
    assert_ne!(raw.screen().contents(), direct(b"abbbbX").screen().contents());
}

#[test]
fn a_sequence_split_across_chunks_is_translated_once_whole() {
    let split = translated(&[b"ab\x1b", b"[", b"3", b"bX"]);
    assert_same(&split, &direct(b"abbbbX"), "split REP");
}

#[test]
fn modelled_and_private_sequences_pass_through_unchanged() {
    let stream: &[u8] = b"\x1b[?1049h\x1b[2;5Hhi\x1b[1mB\x1b[0m\x1b[?25l\x1b[3 qok";
    assert_same(&translated(&[stream]), &direct(stream), "passthrough");
}

#[test]
fn rep_at_the_left_margin_repeats_nothing() {
    assert_same(&translated(&[b"\x1b[5bX"]), &direct(b"X"), "REP at col 0");
}
