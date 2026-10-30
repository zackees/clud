use super::*;
use std::time::Duration;

const KIB: u64 = 1024;

/// Write `len` bytes to `root/<rel>/data` and age every mtime in the session
/// dir to `age`. Returns the session dir (`root/<rel>`).
fn session(root: &Path, rel: &str, len: u64, age: Duration, now: SystemTime) -> PathBuf {
    let dir = root.join(rel);
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("data");
    fs::write(&file, vec![0u8; len as usize]).unwrap();
    let when = now - age;
    for path in [&file, &dir] {
        fs::File::open(path).unwrap().set_modified(when).unwrap();
    }
    dir
}

fn hours(n: u64) -> Duration {
    Duration::from_secs(n * 3600)
}

#[test]
fn under_cap_removes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    let s = session(tmp.path(), "claude-1/p/old", 10 * KIB, hours(100), now);
    let report = enforce_cap_at(tmp.path(), 100 * KIB, now, false);
    assert_eq!(report.evicted, 0);
    assert!(s.exists());
}

#[test]
fn cap_zero_disables() {
    let tmp = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    let s = session(tmp.path(), "claude-1/p/old", 10 * KIB, hours(100), now);
    let report = enforce_cap_at(tmp.path(), 0, now, false);
    assert_eq!(
        report,
        CapReport {
            cap: 0,
            ..Default::default()
        }
    );
    assert!(s.exists());
}

#[test]
fn evicts_oldest_first_until_within_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    let oldest = session(tmp.path(), "claude-1/p/a", 10 * KIB, hours(30), now);
    let middle = session(tmp.path(), "claude-1/p/b", 10 * KIB, hours(20), now);
    let newer = session(tmp.path(), "claude-1/p/c", 10 * KIB, hours(10), now);
    let report = enforce_cap_at(tmp.path(), 15 * KIB, now, false);
    assert_eq!(report.evicted, 2, "{report:?}");
    assert!(!oldest.exists() && !middle.exists());
    assert!(newer.exists(), "stops once within the cap");
}

#[test]
fn a_live_session_survives_even_when_far_over_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    // Huge and actively written: must never be evicted.
    let live = session(
        tmp.path(),
        "claude-1/p/live",
        500 * KIB,
        Duration::from_secs(30),
        now,
    );
    let idle = session(tmp.path(), "claude-1/p/idle", 10 * KIB, hours(50), now);
    let report = enforce_cap_at(tmp.path(), KIB, now, false);
    assert!(live.join("data").exists(), "live session data deleted");
    assert!(!idle.exists());
    assert_eq!(report.kept_live, 1);
    assert!(report.total_bytes > report.cap);
}

#[test]
fn old_dir_mtime_with_a_fresh_file_inside_is_live() {
    let tmp = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    let s = session(tmp.path(), "claude-1/p/s", 50 * KIB, hours(80), now);
    // The dir mtime is ancient; a nested file was just written.
    let nested = s.join("deep");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("fresh"), b"x").unwrap();
    fs::File::open(&s)
        .unwrap()
        .set_modified(now - hours(80))
        .unwrap();
    enforce_cap_at(tmp.path(), KIB, now, false);
    assert!(s.exists());
}

#[test]
fn a_future_mtime_is_treated_as_live() {
    let tmp = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    let s = session(tmp.path(), "claude-1/p/s", 50 * KIB, Duration::ZERO, now);
    fs::File::open(s.join("data"))
        .unwrap()
        .set_modified(now + hours(5))
        .unwrap();
    enforce_cap_at(tmp.path(), KIB, now, false);
    assert!(s.exists());
}

#[test]
fn dry_run_reports_without_deleting() {
    let tmp = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    let s = session(tmp.path(), "claude-1/p/old", 50 * KIB, hours(50), now);
    let report = enforce_cap_at(tmp.path(), KIB, now, true);
    assert_eq!(report.evicted, 1);
    assert!(s.exists());
    assert!(report.summary_line().contains("would evict"));
}

#[test]
fn flat_files_are_units_and_siblings_of_live_sessions_go() {
    let tmp = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    let live = session(
        tmp.path(),
        "claude-1/p/live",
        5 * KIB,
        Duration::from_secs(1),
        now,
    );
    let flat = tmp.path().join("scratch.bin");
    fs::write(&flat, vec![0u8; 50 * KIB as usize]).unwrap();
    fs::File::open(&flat)
        .unwrap()
        .set_modified(now - hours(40))
        .unwrap();
    enforce_cap_at(tmp.path(), 10 * KIB, now, false);
    assert!(!flat.exists());
    assert!(live.exists());
    assert!(tmp.path().join("claude-1/p").exists(), "parents stay");
}

#[test]
fn missing_root_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let report = enforce_cap_at(&tmp.path().join("nope"), KIB, SystemTime::now(), false);
    assert_eq!(report.total_bytes, 0);
}
