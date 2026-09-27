use super::*;

fn t0() -> Instant {
    Instant::now()
}

#[test]
fn the_highest_severity_toast_is_visible_then_the_most_recent() {
    let now = t0();
    let mut board = ToastBoard::default();
    board.apply(ToastEvent::Show(Toast::new(
        "a",
        "info",
        Severity::Info,
        now,
    )));
    board.apply(ToastEvent::Show(Toast::new(
        "b",
        "alert",
        Severity::Alert,
        now - Duration::from_secs(5),
    )));
    assert_eq!(board.visible(now).unwrap().key, "b");
    board.apply(ToastEvent::Show(Toast::new(
        "c",
        "newer alert",
        Severity::Alert,
        now,
    )));
    assert_eq!(board.visible(now).unwrap().key, "c");
}

#[test]
fn republishing_the_same_key_replaces_in_place() {
    let now = t0();
    let mut board = ToastBoard::default();
    assert!(board.apply(ToastEvent::Show(Toast::new(
        "cpu",
        "100 %",
        Severity::Warn,
        now
    ))));
    assert!(board.apply(ToastEvent::Show(Toast::new(
        "cpu",
        "150 %",
        Severity::Warn,
        now
    ))));
    assert_eq!(board.visible(now).unwrap().text, "150 %");
    assert!(
        !board.apply(ToastEvent::Show(Toast::new(
            "cpu",
            "150 %",
            Severity::Warn,
            now + Duration::from_secs(1)
        ))),
        "an unchanged republish is not a change"
    );
}

#[test]
fn expiring_toasts_disappear_and_close_removes_immediately() {
    let now = t0();
    let mut board = ToastBoard::default();
    board.apply(ToastEvent::Show(
        Toast::new("done", "back to normal", Severity::Info, now)
            .expiring_after(Duration::from_secs(10)),
    ));
    board.apply(ToastEvent::Show(Toast::new(
        "live",
        "running",
        Severity::Info,
        now,
    )));
    assert_eq!(board.next_deadline(), Some(now + Duration::from_secs(10)));
    let later = now + Duration::from_secs(11);
    assert!(board.expire(later));
    assert_eq!(board.visible(later).unwrap().key, "live");
    assert!(board.apply(ToastEvent::Close { key: "live".into() }));
    assert!(board.visible(later).is_none());
    assert!(board.is_empty());
    assert!(!board.apply(ToastEvent::Close { key: "live".into() }));
}

#[test]
fn an_expired_toast_is_never_visible_even_before_expire_runs() {
    let now = t0();
    let mut board = ToastBoard::default();
    board.apply(ToastEvent::Show(
        Toast::new("x", "brief", Severity::Info, now).expiring_after(Duration::from_millis(5)),
    ));
    assert!(board.visible(now + Duration::from_millis(6)).is_none());
}

#[test]
fn the_hub_version_moves_on_changes_expiry_and_dismissal_only() {
    let now = t0();
    let hub = ToastHub::new();
    let v0 = hub.snapshot(now).version;
    hub.publish(ToastEvent::Show(
        Toast::new("cpu", "hot", Severity::Warn, now).expiring_after(Duration::from_secs(1)),
    ));
    let v1 = hub.snapshot(now).version;
    assert_ne!(v0, v1);
    assert_eq!(hub.snapshot(now).version, v1, "a quiet snapshot is stable");
    let expired = hub.snapshot(now + Duration::from_secs(2));
    assert_ne!(expired.version, v1);
    assert!(expired.visible.is_none());
    assert!(expired.is_empty);

    hub.publish(ToastEvent::Show(Toast::new(
        "cpu",
        "hot",
        Severity::Warn,
        now,
    )));
    let before = hub.snapshot(now).version;
    hub.dismiss_visible(now);
    let after = hub.snapshot(now);
    assert_ne!(after.version, before);
    assert!(after.visible.is_none());
}

#[test]
fn the_recorder_sink_keeps_events_in_order_and_discard_drops_them() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = ToastSink::Recorder(Arc::clone(&events));
    let now = t0();
    sink.publish(ToastEvent::Show(Toast::new("a", "1", Severity::Info, now)));
    sink.publish(ToastEvent::Close { key: "a".into() });
    let recorded = events.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert!(matches!(recorded[1], ToastEvent::Close { .. }));
    ToastSink::Discard.publish(ToastEvent::Close { key: "a".into() });
    assert!(ToastSink::Discard.is_discard());
}

#[test]
fn a_hub_sink_feeds_the_hub() {
    let hub = ToastHub::new();
    let sink = ToastSink::Hub(Arc::clone(&hub));
    let now = t0();
    sink.publish(ToastEvent::Show(Toast::new("a", "1", Severity::Info, now)));
    assert_eq!(hub.snapshot(now).visible.unwrap().text, "1");
}

#[test]
fn keyed_snapshot_keeps_cpu_and_highest_priority_other_independent() {
    let now = t0();
    let hub = ToastHub::new();
    hub.publish(ToastEvent::Show(Toast::new(
        "cpu",
        "hot",
        Severity::Warn,
        now,
    )));
    hub.publish(ToastEvent::Show(Toast::new(
        "alert",
        "urgent",
        Severity::Alert,
        now,
    )));
    hub.publish(ToastEvent::Show(Toast::new(
        "info",
        "later",
        Severity::Info,
        now,
    )));

    let snapshot = hub.snapshot_with_key(now, "cpu");
    assert_eq!(snapshot.keyed.as_ref().unwrap().text, "hot");
    assert_eq!(snapshot.other_visible.as_ref().unwrap().key, "alert");
    assert_eq!(snapshot.visible.as_ref().unwrap().key, "alert");

    hub.dismiss_visible_except(now, "cpu");
    let after = hub.snapshot_with_key(now, "cpu");
    assert_eq!(after.keyed.as_ref().unwrap().key, "cpu");
    assert_eq!(after.other_visible.as_ref().unwrap().key, "info");
}
