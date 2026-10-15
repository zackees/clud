//! The Unix-only half of `pty/toast_pty.rs` (#1189): the text-cell tier test
//! needs a child that can emit escape sequences through `/bin/sh`.

use std::time::Duration;

use clud::toast::compositor::Fallback;
use clud::toast::tier::ToastTier;
use clud::toast::ToastHub;

use crate::pty::toast_pty::{expiring, replay, run_composited, COLS};

/// Text-cell tier on the alternate screen: the toast is painted as cells,
/// then repainted from the shadow on expiry, so the final screen is exactly
/// what the child drew.
#[test]
fn text_tier_restores_the_alternate_screen_exactly_after_expiry() {
    require_pty_or_skip!("text_tier_restores_the_alternate_screen_exactly_after_expiry");
    let script =
        "printf '\\033[?1049h\\033[1;1H\\033[32mheader\\033[0m\\033[1;60Hright-side-text'; \
                  sleep 1; printf '\\033[1;85HZZZ\\033[6;1Hbody'; sleep 3; printf '\\033[8;1Hdone'";
    let hub = ToastHub::new();
    hub.publish(expiring("toast-cells-e2e", Duration::from_millis(2500)));
    let bytes = run_composited(
        vec!["/bin/sh".into(), "-c".into(), script.into()],
        hub,
        ToastTier::TextCells,
        Fallback::None,
    );
    assert!(
        String::from_utf8_lossy(&bytes).contains("toast-cells-e2e"),
        "the toast was never drawn"
    );
    let screen = replay(&bytes);
    let contents = screen.screen().contents();
    assert!(
        !contents.contains("toast-cells-e2e"),
        "toast cells left behind: {contents:?}"
    );
    let top: String = screen.screen().rows(0, COLS).next().unwrap();
    assert!(top.starts_with("header"), "{top:?}");
    assert!(top.contains("right-side-text"), "{top:?}");
    assert!(
        top.contains("ZZZ"),
        "child write under the toast lost: {top:?}"
    );
    assert!(
        contents.contains("body") && contents.contains("done"),
        "{contents:?}"
    );
}
