use super::*;

#[test]
fn tracked_ids_skip_exited_and_terminated_records() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");
    let api = tmp.path().join("api-sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&api).unwrap();
    fs::write(
        sessions.join("a.json"),
        r#"{"id":"live-a","exit_code":null}"#,
    )
    .unwrap();
    fs::write(sessions.join("b.json"), r#"{"id":"dead-b","exit_code":0}"#).unwrap();
    fs::write(api.join("c.json"), r#"{"id":"live-c","state":"idle"}"#).unwrap();
    fs::write(
        api.join("d.json"),
        r#"{"id":"dead-d","state":"terminated"}"#,
    )
    .unwrap();
    let ids = tracked_session_ids(tmp.path()).unwrap();
    assert!(ids.contains("live-a") && ids.contains("live-c"));
    assert!(!ids.contains("dead-b") && !ids.contains("dead-d"));
}

#[test]
fn missing_state_dirs_mean_no_sessions_but_garbage_is_doubt() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(tracked_session_ids(tmp.path()).unwrap().is_empty());
    let sessions = tmp.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("x.json"), "{not json").unwrap();
    assert!(tracked_session_ids(tmp.path()).is_none());
}

#[test]
fn unconsulted_source_makes_everything_live() {
    let root = Path::new("/r");
    let unit = Path::new("/r/p/s");
    assert!(LiveSignals::new(None, Some(HashSet::new())).is_live(root, unit));
    assert!(LiveSignals::new(Some(vec![]), None).is_live(root, unit));
    assert!(!LiveSignals::nothing_live().is_live(root, unit));
}

#[test]
fn process_path_inside_unit_or_matching_id_is_live() {
    let root = Path::new("/r");
    let unit = Path::new("/r/claude-1/p/sess-9");
    let by_cwd = LiveSignals::new(Some(vec![unit.join("sub")]), Some(HashSet::new()));
    assert!(by_cwd.is_live(root, unit));
    let elsewhere = LiveSignals::new(
        Some(vec![PathBuf::from("/r/claude-1/p/other")]),
        Some(HashSet::new()),
    );
    assert!(!elsewhere.is_live(root, unit));
    let ids = HashSet::from(["sess-9".to_string()]);
    assert!(LiveSignals::new(Some(vec![]), Some(ids)).is_live(root, unit));
}
