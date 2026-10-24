use super::*;

fn stop(active: bool, message: &str) -> Value {
    serde_json::json!({
        "session_id": "s-1",
        "hook_event_name": "Stop",
        "stop_hook_active": active,
        "last_assistant_message": message,
    })
}

#[test]
fn identical_hook_driven_continuations_tell_the_user_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let blocked = "Blocked: the user must run rm -rf on that path.";
    assert_eq!(observe_in(dir.path(), &stop(true, blocked)), None);
    let told = observe_in(dir.path(), &stop(true, blocked)).unwrap();
    assert!(
        told.contains("systemMessage") && told.contains("/goal clear"),
        "{told}"
    );
    for _ in 0..5 {
        assert_eq!(observe_in(dir.path(), &stop(true, blocked)), None);
    }
}

#[test]
fn a_different_answer_keeps_re_prompting_normally() {
    let dir = tempfile::tempdir().unwrap();
    for n in 0..6 {
        let text = format!("progress step {n}");
        assert_eq!(observe_in(dir.path(), &stop(true, &text)), None);
    }
}

#[test]
fn a_user_driven_stop_resets_the_count() {
    let dir = tempfile::tempdir().unwrap();
    let same = "same";
    assert_eq!(observe_in(dir.path(), &stop(true, same)), None);
    assert_eq!(observe_in(dir.path(), &stop(false, same)), None);
    assert_eq!(observe_in(dir.path(), &stop(true, same)), None);
    assert!(observe_in(dir.path(), &stop(true, same)).is_some());
}

#[test]
fn unsafe_session_ids_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let mut payload = stop(true, "x");
    payload["session_id"] = "../escape".into();
    assert_eq!(observe_in(dir.path(), &payload), None);
    assert_eq!(observe_in(dir.path(), &payload), None);
    assert!(!dir.path().join("stop-repeat").exists());
}

#[test]
fn step_is_pure() {
    let (one, tell) = step(State::default(), Some("a"));
    assert!(!tell);
    let (two, tell) = step(one, Some("a"));
    assert!(tell);
    assert_eq!(two.count, 2);
    assert_eq!(step(two, None), (State::default(), false));
}
