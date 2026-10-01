//! Decision table for the creation-ledger consult (#1666): pure facts in,
//! verdict + reason out. No daemon, no filesystem.

use super::*;

const ME: u32 = 1000;

fn row(path: &str, kind: CreatedKind, id: Option<(u64, u64)>, uid: Option<u32>) -> CreatedEntry {
    CreatedEntry {
        session_id: "session-1".into(),
        path: path.into(),
        kind,
        role: "agent".into(),
        created_unix: 100,
        dev: id.map(|(d, _)| d),
        ino: id.map(|(_, i)| i),
        uid,
    }
}

/// A directory row with identity (1, 10), owned by [`ME`].
fn dir_row(path: &str) -> CreatedEntry {
    row(path, CreatedKind::Dir, Some((1, 10)), Some(ME))
}

/// A file row with identity (1, 20), owned by [`ME`].
fn file_row(path: &str) -> CreatedEntry {
    row(path, CreatedKind::File, Some((1, 20)), Some(ME))
}

fn dir_entry(id: (u64, u64), uid: u32) -> LiveEntry {
    LiveEntry {
        is_dir: true,
        is_symlink: false,
        id: Some(id),
        uid: Some(uid),
    }
}

fn file_entry(id: (u64, u64), uid: u32) -> LiveEntry {
    LiveEntry {
        is_dir: false,
        is_symlink: false,
        id: Some(id),
        uid: Some(uid),
    }
}

fn facts(
    target: &str,
    rows: Result<Vec<CreatedEntry>, String>,
    live: &[(&str, LiveEntry)],
) -> LedgerFacts {
    LedgerFacts {
        target: PathBuf::from(target),
        rows,
        live: live.iter().map(|(p, e)| (PathBuf::from(p), *e)).collect(),
        me: Some(ME),
    }
}

fn refusal(f: &LedgerFacts) -> String {
    verdict(f).expect_err("expected a refusal")
}

#[test]
fn recorded_directory_itself_is_allowed_and_names_the_ledger() {
    let f = facts(
        "/work/out",
        Ok(vec![dir_row("/work/out")]),
        &[("/work/out", dir_entry((1, 10), ME))],
    );
    let reason = verdict(&f).expect("allowed");
    assert!(reason.contains("creation ledger"), "{reason}");
    assert!(reason.contains("/work/out"), "{reason}");
}

#[test]
fn entry_under_a_recorded_directory_is_allowed() {
    let f = facts(
        "/work/out/a/b.txt",
        Ok(vec![dir_row("/work/out")]),
        &[
            ("/work/out", dir_entry((1, 10), ME)),
            ("/work/out/a", dir_entry((1, 11), ME)),
            ("/work/out/a/b.txt", file_entry((1, 12), ME)),
        ],
    );
    assert!(verdict(&f).is_ok());
}

#[test]
fn recorded_file_is_allowed() {
    let f = facts(
        "/docs/report.md",
        Ok(vec![file_row("/docs/report.md")]),
        &[("/docs/report.md", file_entry((1, 20), ME))],
    );
    assert!(verdict(&f).is_ok());
}

#[test]
fn pre_existing_sibling_of_a_recorded_file_is_refused() {
    // The daemon only returns covering rows; even if it returned the file's
    // row, a sibling is not covered by a FILE row.
    let f = facts(
        "/docs/other.md",
        Ok(vec![file_row("/docs/report.md")]),
        &[("/docs/other.md", file_entry((1, 21), ME))],
    );
    assert!(refusal(&f).contains("not created by this session"));
}

#[test]
fn parent_of_a_recorded_file_is_refused() {
    let f = facts(
        "/docs",
        Ok(vec![file_row("/docs/report.md")]),
        &[("/docs", dir_entry((1, 2), ME))],
    );
    assert!(refusal(&f).contains("not created by this session"));
}

#[test]
fn no_row_refuses_not_created_by_this_session() {
    let f = facts("/elsewhere/x", Ok(Vec::new()), &[]);
    assert!(refusal(&f).contains("not created by this session"));
}

#[test]
fn unavailable_ledger_refuses_and_says_so() {
    let f = facts("/elsewhere/x", Err("daemon not running".into()), &[]);
    let reason = refusal(&f);
    assert!(reason.contains("creation ledger unavailable"), "{reason}");
    assert!(reason.contains("daemon not running"), "{reason}");
}

#[test]
fn recorded_path_swapped_for_a_symlink_is_refused() {
    let f = facts(
        "/work/out",
        Ok(vec![dir_row("/work/out")]),
        &[(
            "/work/out",
            LiveEntry {
                is_dir: false,
                is_symlink: true,
                id: Some((1, 99)),
                uid: Some(ME),
            },
        )],
    );
    assert!(refusal(&f).contains("symlink"));
}

#[test]
fn recorded_path_with_a_different_inode_is_refused() {
    let f = facts(
        "/work/out",
        Ok(vec![dir_row("/work/out")]),
        &[("/work/out", dir_entry((1, 77), ME))],
    );
    assert!(refusal(&f).contains("replaced"));
}

#[test]
fn recorded_path_on_a_different_device_is_refused() {
    let f = facts(
        "/work/out",
        Ok(vec![dir_row("/work/out")]),
        &[("/work/out", dir_entry((2, 10), ME))],
    );
    assert!(refusal(&f).contains("replaced"));
}

#[test]
fn recorded_file_now_a_directory_is_refused() {
    let f = facts(
        "/docs/report.md",
        Ok(vec![file_row("/docs/report.md")]),
        &[("/docs/report.md", dir_entry((1, 20), ME))],
    );
    assert!(refusal(&f).contains("is now a"));
}

#[test]
fn recorded_path_gone_is_refused() {
    let f = facts("/work/out/x", Ok(vec![dir_row("/work/out")]), &[]);
    assert!(refusal(&f).contains("no longer exists"));
}

#[test]
fn row_without_identity_refuses_on_doubt() {
    let f = facts(
        "/work/out",
        Ok(vec![row("/work/out", CreatedKind::Dir, None, None)]),
        &[("/work/out", dir_entry((1, 10), ME))],
    );
    assert!(refusal(&f).contains("cannot verify"));
}

#[test]
fn platform_without_identity_refuses_on_doubt() {
    let mut f = facts(
        "/work/out",
        Ok(vec![dir_row("/work/out")]),
        &[(
            "/work/out",
            LiveEntry {
                is_dir: true,
                is_symlink: false,
                id: None,
                uid: None,
            },
        )],
    );
    f.me = None;
    assert!(refusal(&f).contains("cannot verify"));
}

#[test]
fn row_recorded_for_another_uid_is_refused() {
    let mut foreign_row = dir_row("/work/out");
    foreign_row.uid = Some(ME + 1);
    let f = facts(
        "/work/out",
        Ok(vec![foreign_row]),
        &[("/work/out", dir_entry((1, 10), ME))],
    );
    assert!(refusal(&f).contains("not owned by you"));
}

#[test]
fn entry_owned_by_another_user_under_a_recorded_dir_is_refused() {
    let f = facts(
        "/work/out/theirs/f",
        Ok(vec![dir_row("/work/out")]),
        &[
            ("/work/out", dir_entry((1, 10), ME)),
            ("/work/out/theirs", dir_entry((1, 11), ME + 1)),
            ("/work/out/theirs/f", file_entry((1, 12), ME)),
        ],
    );
    let reason = refusal(&f);
    assert!(reason.contains("theirs"), "{reason}");
    assert!(reason.contains("not owned by you"), "{reason}");
}

#[test]
fn a_failing_row_does_not_hide_a_passing_one() {
    let f = facts(
        "/work/out/inner/f",
        Ok(vec![
            row("/work/out", CreatedKind::Dir, Some((1, 10)), Some(ME)),
            row("/work/out/inner", CreatedKind::Dir, Some((1, 55)), Some(ME)),
        ]),
        &[
            ("/work/out", dir_entry((1, 10), ME)),
            ("/work/out/inner", dir_entry((1, 11), ME)),
            ("/work/out/inner/f", file_entry((1, 12), ME)),
        ],
    );
    assert!(verdict(&f).is_ok());
}

#[test]
fn covers_is_component_wise() {
    let dir = row("/work/out", CreatedKind::Dir, Some((1, 10)), Some(ME));
    assert!(covers(&dir, Path::new("/work/out")));
    assert!(covers(&dir, Path::new("/work/out/x")));
    assert!(!covers(&dir, Path::new("/work/outside")));
}
