use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use super::paths::daemon_events_path;

const MAX_EVENT_LOG_BYTES: u64 = 1024 * 1024;
const EVENT_LOG_BACKUP_SUFFIX: &str = "1";
static APPEND_LOCK: Mutex<()> = Mutex::new(());

pub(super) fn log_event(
    state_dir: &Path,
    op: &str,
    fields: impl IntoIterator<Item = (&'static str, Value)>,
) {
    let mut event = base_event(op);
    for (key, value) in fields {
        event.insert(key.to_string(), value);
    }
    let _ = append_event_line(&daemon_events_path(state_dir), &Value::Object(event));
}

pub(super) fn request_id() -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::AcqRel)
}

fn base_event(op: &str) -> Map<String, Value> {
    let mut event = Map::new();
    event.insert("ts_ms".to_string(), json!(unix_millis_now()));
    event.insert("event_id".to_string(), json!(request_id()));
    event.insert("daemon_pid".to_string(), json!(std::process::id()));
    event.insert("thread".to_string(), json!(thread_name()));
    event.insert("op".to_string(), json!(op));
    event
}

fn append_event_line(path: &Path, event: &Value) -> io::Result<()> {
    let _guard = APPEND_LOCK
        .lock()
        .map_err(|_| io::Error::other("daemon event log append lock poisoned"))?;
    rotate_if_needed(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing daemon event log parent"))?;
    fs::create_dir_all(parent)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, event)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    file.write_all(b"\n")?;
    file.flush()
}

fn rotate_if_needed(path: &Path) -> io::Result<()> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(());
    };
    if metadata.len() < MAX_EVENT_LOG_BYTES {
        return Ok(());
    }
    let backup = rotated_path(path);
    if backup.exists() {
        let _ = fs::remove_file(&backup);
    }
    fs::rename(path, backup)
}

fn rotated_path(path: &Path) -> PathBuf {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => path.with_extension(format!("{ext}.{EVENT_LOG_BACKUP_SUFFIX}")),
        None => path.with_extension(EVENT_LOG_BACKUP_SUFFIX),
    }
}

fn unix_millis_now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn thread_name() -> String {
    std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_lines(path: &Path) -> Vec<Value> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn appends_one_json_object_per_line() {
        let tmp = tempfile::tempdir().unwrap();
        log_event(tmp.path(), "daemon_start", [("port", json!(1234))]);
        log_event(
            tmp.path(),
            "adopt_kill_accepted",
            [("pids", json!([1, 2])), ("reason", json!("ctrl_c"))],
        );

        let lines = read_lines(&daemon_events_path(tmp.path()));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["op"], "daemon_start");
        assert_eq!(lines[0]["port"], 1234);
        assert_eq!(lines[1]["op"], "adopt_kill_accepted");
        assert_eq!(lines[1]["pids"], json!([1, 2]));
        assert!(lines[1]["ts_ms"].is_u64());
        assert!(lines[1]["event_id"].is_u64());
    }

    #[test]
    fn rotates_existing_log_before_append_when_over_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let path = daemon_events_path(tmp.path());
        fs::write(&path, vec![b'x'; (MAX_EVENT_LOG_BYTES + 1) as usize]).unwrap();

        log_event(tmp.path(), "after_rotate", []);

        let backup = rotated_path(&path);
        assert!(backup.exists(), "expected rotated backup at {backup:?}");
        let lines = read_lines(&path);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["op"], "after_rotate");
    }

    #[test]
    fn concurrent_append_keeps_one_valid_json_object_per_line() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().to_path_buf();
        let threads = 12;
        let per_thread = 75;

        std::thread::scope(|scope| {
            for worker in 0..threads {
                let state_dir = state_dir.clone();
                scope.spawn(move || {
                    for seq in 0..per_thread {
                        log_event(
                            &state_dir,
                            "stress",
                            [
                                ("worker", json!(worker)),
                                ("seq", json!(seq)),
                                ("payload", json!("x".repeat(1024))),
                            ],
                        );
                    }
                });
            }
        });

        let text = fs::read_to_string(daemon_events_path(tmp.path())).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), threads * per_thread);
        for (idx, line) in lines.iter().enumerate() {
            let value: Value = serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("invalid jsonl line {idx}: {err}: {line:?}"));
            assert_eq!(value["op"], "stress");
            assert!(value["worker"].is_u64());
            assert!(value["seq"].is_u64());
        }
    }
}
