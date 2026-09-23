//! Exact, incremental Claude-transcript usage for routes without a bridge.
//! No transcript text or raw identifiers are serialized into the ledger.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::statusline::{StatusState, StatusUsage, STALE_AFTER};

const FINGERPRINT_BYTES: u64 = 4096;
const MAX_BYTES_PER_CALLBACK: u64 = 8 * 1024 * 1024;
static TMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Snapshot {
    launch_nonce: String,
    updated_ms: u64,
    usage: StatusUsage,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Cursor {
    offset: u64,
    prefix_hash: String,
    tail_hash: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Ledger {
    launch_nonce: String,
    files: BTreeMap<String, Cursor>,
    // SHA-256(message.id) -> provider tuple and opaque source-file key.
    seen: BTreeMap<String, SeenRecord>,
    uncertain: bool,
    usage: Option<StatusUsage>,
    last_timestamp: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Counts {
    input: u64,
    created: u64,
    cached: u64,
    output: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SeenRecord {
    source: String,
    counts: Counts,
}

pub(super) fn snapshot_path(state_path: &Path) -> PathBuf {
    state_path.with_extension("usage.json")
}

fn cursor_path(state_path: &Path) -> PathBuf {
    state_path.with_extension("usage.state.json")
}

fn lock_path(state_path: &Path) -> PathBuf {
    state_path.with_extension("usage.lock")
}

pub(super) fn cleanup(state_path: &Path) {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path(state_path));
    if let Ok(lock) = &lock {
        let _ = lock.lock_exclusive();
    }
    let _ = fs::remove_file(snapshot_path(state_path));
    let _ = fs::remove_file(cursor_path(state_path));
    drop(lock);
    let _ = fs::remove_file(lock_path(state_path));
}

pub(super) fn read_live(state_path: &Path, launch_nonce: &str, now_ms: u64) -> Option<StatusUsage> {
    let bytes = fs::read(snapshot_path(state_path)).ok()?;
    let snapshot: Snapshot = serde_json::from_slice(&bytes).ok()?;
    let stale_ms = u64::try_from(STALE_AFTER.as_millis()).unwrap_or(u64::MAX);
    (snapshot.launch_nonce == launch_nonce
        && now_ms.saturating_sub(snapshot.updated_ms) <= stale_ms)
        .then_some(snapshot.usage)
}

pub(super) fn update(
    state_path: &Path,
    launch_nonce: &str,
    transcript: &Path,
    provider: &str,
    fallback_model: &str,
    now_ms: u64,
) -> Option<StatusUsage> {
    let old_snapshot = read_live(state_path, launch_nonce, now_ms);
    let parent = state_path.parent()?;
    fs::create_dir_all(parent).ok()?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path(state_path))
        .ok()?;
    if lock.try_lock_exclusive().is_err() {
        return old_snapshot;
    }
    let mut ledger: Ledger = fs::read(cursor_path(state_path))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|state: &Ledger| state.launch_nonce == launch_nonce)
        .unwrap_or_else(|| Ledger {
            launch_nonce: launch_nonce.to_string(),
            ..Ledger::default()
        });
    let paths = transcript_paths(transcript);
    if paths.is_empty() {
        return old_snapshot;
    }
    let mut remaining = MAX_BYTES_PER_CALLBACK;
    let mut changed = false;
    for path in &paths {
        if remaining == 0 {
            break;
        }
        match scan_file(path, &mut ledger, &mut remaining, provider, fallback_model) {
            Ok(scanned) => changed |= scanned,
            Err(_) => return old_snapshot,
        }
    }
    if ledger.uncertain {
        if !same_launch_alive(state_path, launch_nonce) {
            return None;
        }
        if changed {
            let bytes = serde_json::to_vec(&ledger).ok()?;
            atomic_write(&cursor_path(state_path), &bytes).ok()?;
        }
        let _ = fs::remove_file(snapshot_path(state_path));
        return None;
    }
    if remaining == 0 {
        // Persist progress, but never present a prefix of a large transcript
        // as if it were the full session total.
        if !same_launch_alive(state_path, launch_nonce) {
            return old_snapshot;
        }
        if changed {
            let bytes = serde_json::to_vec(&ledger).ok()?;
            atomic_write(&cursor_path(state_path), &bytes).ok()?;
        }
        return old_snapshot;
    }
    let Some(usage) = ledger.usage.clone() else {
        return old_snapshot;
    };
    let snapshot = Snapshot {
        launch_nonce: launch_nonce.to_string(),
        updated_ms: now_ms,
        usage: usage.clone(),
    };
    let snapshot_bytes = serde_json::to_vec(&snapshot).ok()?;
    if !same_launch_alive(state_path, launch_nonce) {
        return old_snapshot;
    }
    if changed {
        let state_bytes = serde_json::to_vec(&ledger).ok()?;
        atomic_write(&cursor_path(state_path), &state_bytes).ok()?;
    }
    atomic_write(&snapshot_path(state_path), &snapshot_bytes).ok()?;
    Some(usage)
}

fn same_launch_alive(state_path: &Path, launch_nonce: &str) -> bool {
    fs::read(state_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<StatusState>(&bytes).ok())
        .is_some_and(|state| {
            state.launch_nonce == launch_nonce
                && state
                    .owner_identity
                    .is_some_and(|identity| identity.is_live())
        })
}

fn transcript_paths(main: &Path) -> Vec<PathBuf> {
    if main
        .extension()
        .is_none_or(|extension| extension != "jsonl")
        || !main.is_file()
    {
        return Vec::new();
    }
    let mut paths = vec![main.to_path_buf()];
    let subagents = main.with_extension("").join("subagents");
    if let Ok(entries) = fs::read_dir(subagents) {
        paths.extend(
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "jsonl")
                        && path.is_file()
                }),
        );
    }
    paths.sort();
    paths
}

fn scan_file(
    path: &Path,
    ledger: &mut Ledger,
    remaining: &mut u64,
    provider: &str,
    fallback_model: &str,
) -> io::Result<bool> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let key = digest(path.to_string_lossy().as_bytes());
    let mut cursor = ledger.files.remove(&key).unwrap_or_default();
    let original_offset = cursor.offset;
    let reset = len < cursor.offset || !fingerprints_match(&mut file, &cursor)?;
    if reset {
        cursor = Cursor::default();
    }
    file.seek(SeekFrom::Start(cursor.offset))?;
    let mut reader = BufReader::new(file);
    loop {
        if *remaining == 0 {
            break;
        }
        let mut line = Vec::new();
        let bytes = reader.read_until(b'\n', &mut line)?;
        if bytes == 0 || !line.ends_with(b"\n") {
            break;
        }
        cursor.offset = cursor.offset.saturating_add(bytes as u64);
        *remaining = remaining.saturating_sub(bytes as u64);
        // A complete malformed line may hide usage. Checkpoint it but never
        // present the remaining prefix as an exact session total.
        if let Ok(record) = serde_json::from_slice::<Value>(&line) {
            record_usage(&record, ledger, &key, !reset, provider, fallback_model);
        } else {
            ledger.uncertain = true;
        }
    }
    let mut file = reader.into_inner();
    cursor.prefix_hash = range_digest(&mut file, 0, cursor.offset.min(FINGERPRINT_BYTES))?;
    let tail_start = cursor.offset.saturating_sub(FINGERPRINT_BYTES);
    cursor.tail_hash = range_digest(&mut file, tail_start, cursor.offset - tail_start)?;
    let changed = reset || cursor.offset != original_offset;
    ledger.files.insert(key, cursor);
    Ok(changed)
}

fn fingerprints_match(file: &mut File, cursor: &Cursor) -> io::Result<bool> {
    if cursor.offset == 0 {
        return Ok(true);
    }
    let prefix = range_digest(file, 0, cursor.offset.min(FINGERPRINT_BYTES))?;
    let tail_start = cursor.offset.saturating_sub(FINGERPRINT_BYTES);
    let tail = range_digest(file, tail_start, cursor.offset - tail_start)?;
    Ok(prefix == cursor.prefix_hash && tail == cursor.tail_hash)
}

fn range_digest(file: &mut File, start: u64, len: u64) -> io::Result<String> {
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = vec![0; usize::try_from(len).unwrap_or(0)];
    file.read_exact(&mut bytes)?;
    Ok(digest(&bytes))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn record_usage(
    record: &Value,
    ledger: &mut Ledger,
    source: &str,
    allow_revision: bool,
    provider: &str,
    fallback_model: &str,
) {
    let Some(message) = record.get("message") else {
        return;
    };
    let Some(id) = message.get("id").and_then(Value::as_str) else {
        return;
    };
    let Some(usage) = message.get("usage") else {
        return;
    };
    let number = |name| usage.get(name).and_then(Value::as_u64);
    let (Some(input), Some(created), Some(cached), Some(output)) = (
        number("input_tokens"),
        number("cache_creation_input_tokens"),
        number("cache_read_input_tokens"),
        number("output_tokens"),
    ) else {
        return;
    };
    let identity = digest(id.as_bytes());
    let counts = Counts {
        input,
        created,
        cached,
        output,
    };
    let previous = ledger.seen.get(&identity);
    if previous.is_some_and(|previous| previous.counts == counts) {
        return;
    }
    if previous.is_some_and(|previous| previous.source != source || !allow_revision) {
        ledger.uncertain = true;
        return;
    }
    let old = previous.map_or(Counts::default(), |previous| previous.counts);
    let is_new = previous.is_none();
    ledger.seen.insert(
        identity,
        SeenRecord {
            source: source.to_string(),
            counts,
        },
    );
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .unwrap_or({
            if fallback_model.is_empty() {
                "unknown model"
            } else {
                fallback_model
            }
        });
    let model: String = crate::secret_redaction::redact_text(model)
        .chars()
        .filter(|character| !character.is_control())
        .take(96)
        .collect();
    let totals = ledger.usage.get_or_insert_with(|| StatusUsage {
        provider: provider.to_string(),
        model: model.clone(),
        request_count: 0,
        cached_input_tokens: 0,
        uncached_input_tokens: 0,
        output_tokens: 0,
        cache_health: "unavailable".into(),
    });
    if is_new {
        totals.request_count = totals.request_count.saturating_add(1);
    }
    totals.cached_input_tokens = totals
        .cached_input_tokens
        .saturating_sub(old.cached)
        .saturating_add(counts.cached);
    totals.uncached_input_tokens = totals
        .uncached_input_tokens
        .saturating_sub(old.input.saturating_add(old.created))
        .saturating_add(counts.input.saturating_add(counts.created));
    totals.output_tokens = totals
        .output_tokens
        .saturating_sub(old.output)
        .saturating_add(counts.output);
    let timestamp = record.get("timestamp").and_then(Value::as_str);
    if timestamp.is_none_or(|timestamp| {
        ledger
            .last_timestamp
            .as_deref()
            .is_none_or(|last| timestamp >= last)
    }) {
        totals.model = model;
        ledger.last_timestamp = timestamp.map(str::to_string);
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let sequence = TMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp-{}-{sequence}", std::process::id()));
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?
    };
    #[cfg(not(unix))]
    let file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    use std::io::Write;
    let mut file = file;
    file.write_all(bytes)?;
    fs::rename(&tmp, path)
}
