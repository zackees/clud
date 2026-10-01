use super::*;
use std::cell::RefCell;
use std::path::Path;

/// Records every row it is asked to write; optionally fails.
#[derive(Default)]
struct FakeWriter {
    rows: RefCell<Vec<CreatedEntry>>,
    error: Option<String>,
}

impl LedgerWriter for FakeWriter {
    fn record(&self, entry: &CreatedEntry) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        self.rows.borrow_mut().push(entry.clone());
        Ok(())
    }
}

fn base() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let base = crate::path_norm::canonicalize_plain(tmp.path()).unwrap();
    (tmp, base)
}

fn request(cwd: &Path, session: Option<&str>) -> Request {
    Request {
        cwd: cwd.to_path_buf(),
        session_id: session.map(String::from),
        role: "agent".into(),
        now_unix: 42,
    }
}

#[cfg(unix)]
#[test]
fn creates_the_directory_and_records_its_identity() {
    use std::os::unix::fs::MetadataExt;
    let (_tmp, base) = base();
    let writer = FakeWriter::default();
    let created = create("scratch", &request(&base, Some("s1")), &writer).unwrap();
    assert_eq!(created, base.join("scratch"));
    let meta = std::fs::symlink_metadata(&created).unwrap();
    assert!(meta.is_dir());
    let rows = writer.rows.borrow();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.session_id, "s1");
    assert_eq!(row.path, created.display().to_string());
    assert_eq!(row.kind, crate::gc::CreatedKind::Dir);
    assert_eq!(row.role, "agent");
    assert_eq!(row.created_unix, 42);
    assert_eq!((row.dev, row.ino), (Some(meta.dev()), Some(meta.ino())));
    assert_eq!(row.uid, Some(meta.uid()));
}

/// Acceptance: an existing path fails and records nothing, whatever it is.
#[cfg(unix)]
#[test]
fn an_existing_path_fails_and_records_nothing() {
    let (_tmp, base) = base();
    let dir = base.join("dir");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("keep.txt"), "keep").unwrap();
    let file = base.join("file");
    std::fs::write(&file, "keep").unwrap();
    let link = base.join("link");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    let dangling = base.join("dangling");
    std::os::unix::fs::symlink(base.join("nowhere"), &dangling).unwrap();
    let writer = FakeWriter::default();
    for (raw, path) in [
        ("dir", &dir),
        ("file", &file),
        ("link", &link),
        ("dangling", &dangling),
    ] {
        let failure = create(raw, &request(&base, Some("s1")), &writer).unwrap_err();
        assert_eq!(failure.code, 1, "{raw}");
        assert!(failure.message.contains("already exists"), "{failure:?}");
        assert!(std::fs::symlink_metadata(path).is_ok(), "{raw}");
    }
    // A trailing `.` names the existing directory itself.
    let failure = create("dir/.", &request(&base, Some("s1")), &writer).unwrap_err();
    assert!(failure.message.contains("already exists"), "{failure:?}");
    assert!(writer.rows.borrow().is_empty());
    assert!(
        !base.join("nowhere").exists(),
        "a dangling link is not followed"
    );
    assert!(dir.join("keep.txt").exists());
}

#[cfg(unix)]
#[test]
fn parents_are_never_created() {
    let (_tmp, base) = base();
    let writer = FakeWriter::default();
    let failure = create("a/b", &request(&base, Some("s1")), &writer).unwrap_err();
    assert!(failure.message.contains("never its parents"), "{failure:?}");
    assert!(!base.join("a").exists());
    assert!(writer.rows.borrow().is_empty());
    let failure = create("..", &request(&base, Some("s1")), &writer).unwrap_err();
    assert_eq!(failure.code, 2);
}

#[cfg(unix)]
#[test]
fn no_session_id_creates_nothing() {
    let (_tmp, base) = base();
    let writer = FakeWriter::default();
    for session in [None, Some("")] {
        let failure = create("scratch", &request(&base, session), &writer).unwrap_err();
        assert!(failure.message.contains("session id"), "{failure:?}");
    }
    assert!(!base.join("scratch").exists());
    assert!(writer.rows.borrow().is_empty());
}

/// Fail closed: a failed insert removes the directory it just made.
#[cfg(unix)]
#[test]
fn a_failed_insert_removes_the_new_directory_and_exits_non_zero() {
    let (_tmp, base) = base();
    let writer = FakeWriter {
        error: Some("clud daemon: connection refused".into()),
        ..FakeWriter::default()
    };
    let failure = create("scratch", &request(&base, Some("s1")), &writer).unwrap_err();
    assert_eq!(failure.code, 1);
    assert!(failure.message.contains("insert failed"), "{failure:?}");
    assert!(failure.message.contains("removed"), "{failure:?}");
    assert!(!base.join("scratch").exists());
}

#[cfg(not(unix))]
#[test]
fn windows_refuses_before_creating_anything() {
    let (_tmp, base) = base();
    let writer = FakeWriter::default();
    let failure = create("scratch", &request(&base, Some("s1")), &writer).unwrap_err();
    assert_eq!(failure.code, 2);
    assert!(failure.message.contains("Windows"), "{failure:?}");
    assert!(!base.join("scratch").exists());
    assert!(writer.rows.borrow().is_empty());
}

/// Acceptance: no code path but `safe-mktemp` writes a creation-ledger row.
#[test]
fn only_safe_mktemp_calls_the_ledger_insert() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut callers = Vec::new();
    let mut stack = vec![src.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                if text.contains(concat!("gc_client_insert", "_created(")) {
                    callers.push(path.strip_prefix(&src).unwrap().to_path_buf());
                }
            }
        }
    }
    callers.sort();
    assert_eq!(
        callers,
        vec![
            PathBuf::from("daemon").join("client.rs"),
            PathBuf::from("safe_mktemp.rs"),
        ]
    );
}

// ---------- the session env is shared by sub-agent processes ----------

/// A ledger writer and reader over a registry file, standing in for the
/// daemon (the daemon's GC op is this same `Registry` call).
#[cfg(unix)]
struct RegistryWriter<'a>(&'a crate::gc::Registry);

#[cfg(unix)]
impl LedgerWriter for RegistryWriter<'_> {
    fn record(&self, entry: &CreatedEntry) -> Result<(), String> {
        self.0
            .insert_created(entry)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct RegistryLedger {
    db: PathBuf,
    session: Option<String>,
}

#[cfg(unix)]
impl crate::rm_tool::ledger::CreationLedger for RegistryLedger {
    fn lookup(&self, path: &Path) -> Result<Vec<CreatedEntry>, String> {
        let session = self.session.as_deref().ok_or("no clud session id")?;
        let registry = crate::gc::Registry::open_at(&self.db).map_err(|e| e.to_string())?;
        registry
            .query_created(session, &path.to_string_lossy())
            .map_err(|e| e.to_string())
    }
}

#[cfg(unix)]
const CHILD_ENV: &str = "CLUD_SAFE_MKTEMP_TEST_CHILD";

#[cfg(unix)]
fn child_filter() -> String {
    let (_, module) = module_path!().split_once("::").unwrap();
    format!("{module}::safe_rm_child_from_the_session_env")
}

/// Acceptance: a directory `safe-mktemp` made outside the roots is removed by
/// `safe-rm -r` in a second process sharing the session env (a sub-agent),
/// and refused from a process with another session id.
#[cfg(unix)]
#[test]
fn a_recorded_directory_is_removable_from_a_process_sharing_the_session_env() {
    let (_tmp, base) = base();
    let db = base.join("state").join("data.redb");
    let root = base.join("root");
    std::fs::create_dir(&root).unwrap();
    let created = {
        let registry = crate::gc::Registry::open_at(&db).unwrap();
        let created = create(
            base.join("scratch").to_str().unwrap(),
            &request(&base, Some("parent-session")),
            &RegistryWriter(&registry),
        )
        .unwrap();
        std::fs::create_dir(created.join("a")).unwrap();
        std::fs::write(created.join("a").join("b.txt"), "x").unwrap();
        created
    };
    for (session, expect) in [("other-session", "refused"), ("parent-session", "removed")] {
        let argv = vec![
            std::env::current_exe().unwrap().display().to_string(),
            "--ignored".into(),
            "--exact".into(),
            child_filter(),
            "--nocapture".into(),
        ];
        let mut env: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| k != crate::grind_facts::SESSION_ENV)
            .collect();
        env.retain(|(k, _)| k != "CLUD_SESSION_ID");
        env.push(("CLUD_SESSION_ID".into(), session.into()));
        env.push((CHILD_ENV.into(), "1".into()));
        env.push(("CHILD_DB".into(), db.display().to_string()));
        env.push(("CHILD_ROOT".into(), root.display().to_string()));
        env.push(("CHILD_TARGET".into(), created.display().to_string()));
        env.push(("CHILD_EXPECT".into(), expect.into()));
        let child = crate::subprocess::ManagedSubprocess::start(argv, None, env, false, None)
            .expect("spawn the child test");
        let code = child
            .wait(Some(std::time::Duration::from_secs(120)))
            .unwrap();
        assert_eq!(code, 0, "child as {session} expected {expect}");
        assert_eq!(created.exists(), expect == "refused", "{session}");
    }
}

/// The second process: `safe-rm -r` with the session id read from its env.
#[cfg(unix)]
#[test]
#[ignore = "run by a_recorded_directory_is_removable_from_a_process_sharing_the_session_env"]
fn safe_rm_child_from_the_session_env() {
    use crate::rm_tool::{parse_args, run_with, Context, Kind, Roots};
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let var = |key: &str| PathBuf::from(std::env::var(key).unwrap());
    let (db, root, target) = (var("CHILD_DB"), var("CHILD_ROOT"), var("CHILD_TARGET"));
    let session_id = crate::rm_tool::session_id_from_env();
    let ledger = RegistryLedger {
        db: db.clone(),
        session: session_id.clone(),
    };
    let mut roots = Roots::fixed(vec![root.clone()], true).with_ledger(std::sync::Arc::new(ledger));
    let ctx = Context {
        cwd: root,
        home: None,
        trash_root: db.parent().unwrap().join("trash"),
        audit_dir: None,
        session_id,
        role: "agent".into(),
        register: false,
        now: std::time::SystemTime::now(),
    };
    let options = parse_args(&["-r".to_string(), target.display().to_string()])
        .unwrap()
        .unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_with(Kind::Safe, &options, &ctx, &mut roots, &mut out, &mut err);
    let err = String::from_utf8_lossy(&err);
    match std::env::var("CHILD_EXPECT").unwrap().as_str() {
        "removed" => {
            assert_eq!(code, 0, "{err}");
            assert!(!target.exists());
        }
        _ => {
            assert_eq!(code, 1, "{err}");
            assert!(err.contains("not created by this session"), "{err}");
            assert!(target.exists());
        }
    }
}
