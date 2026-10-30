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

#[test]
fn claude_project_dir_is_live_while_a_process_works_in_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let unit = root.join("claude-1000").join("-home-u-proj").join("uuid-1");
    fs::create_dir_all(&unit).unwrap();
    // TMPDIR is the root itself; cwd is the project dir (or below it).
    for cwd in ["/home/u/proj", "/home/u/proj/src/deep"] {
        let paths = vec![root.to_path_buf(), PathBuf::from(cwd)];
        let live = LiveSignals::new(Some(paths), Some(HashSet::new()));
        assert!(live.is_live(root, &unit), "{cwd}");
    }
    let other = vec![root.to_path_buf(), PathBuf::from("/home/u/other")];
    let live = LiveSignals::new(Some(other), Some(HashSet::new()));
    assert!(!live.is_live(root, &unit));
}

fn facts(cwd: Option<&str>, env: &[&str]) -> ProcFacts {
    ProcFacts {
        cwd: cwd.map(PathBuf::from),
        environ: env.iter().map(Into::into).collect(),
        zombie: false,
        same_user: true,
    }
}

#[test]
fn denied_reads_make_the_scan_unconsulted() {
    let own = facts(Some("/a"), &["X=1"]);
    let ok = facts(Some("/b"), &[]);
    assert_eq!(
        paths_from_facts(&own, std::slice::from_ref(&ok))
            .unwrap()
            .len(),
        1
    );
    // A process of ours with neither cwd nor environ: read denied.
    assert!(paths_from_facts(&own, &[ok.clone(), facts(None, &[])]).is_none());
    // Zombies and other users' processes prove nothing either way.
    let zombie = ProcFacts {
        zombie: true,
        ..facts(None, &[])
    };
    let other_user = ProcFacts {
        same_user: false,
        ..facts(None, &[])
    };
    assert!(paths_from_facts(&own, &[ok, zombie, other_user]).is_some());
    // Our own environ unreadable: the API is not working here.
    assert!(paths_from_facts(&facts(Some("/a"), &[]), &[]).is_none());
    assert!(paths_from_facts(&facts(None, &["X=1"]), &[]).is_none());
}
