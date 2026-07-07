//! Durable per-launch diagnostics for `clud ui`.
//!
//! The live session-cap registry intentionally deletes rows on graceful exit.
//! These records are separate: one JSON file per launch under the daemon state
//! directory, retained long enough for the dashboard to explain recent exits.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::command::LaunchPlan;
use crate::loop_spec;

const DIR_NAME: &str = "launches";
const MAX_RECORDS: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchRecord {
    pub id: String,
    pub source: String,
    pub clud_pid: u32,
    pub backend: String,
    pub launch_mode: String,
    pub cwd: Option<String>,
    pub repo_root: Option<String>,
    pub command: Vec<String>,
    pub clud_argv: Vec<String>,
    pub launched_at_ms: u64,
    #[serde(default)]
    pub exited_at_ms: Option<u64>,
    #[serde(default)]
    pub exit_code: Option<i32>,
}

impl LaunchRecord {
    pub fn duration_ms(&self) -> Option<u64> {
        self.exited_at_ms
            .map(|end| end.saturating_sub(self.launched_at_ms))
    }
}

#[derive(Debug)]
pub struct LaunchLogHandle {
    state_dir: PathBuf,
    id: String,
}

impl LaunchLogHandle {
    pub fn finish(&self, exit_code: i32) {
        if let Err(err) = finish_launch(&self.state_dir, &self.id, exit_code) {
            eprintln!("[clud] warning: failed to record launch exit: {err}");
        }
    }
}

pub fn start_launch(
    state_dir: &Path,
    plan: &LaunchPlan,
    source: &str,
) -> io::Result<LaunchLogHandle> {
    let launched_at_ms = unix_millis_now();
    let id = format!("{launched_at_ms}-{}", std::process::id());
    let cwd = launch_cwd(plan);
    let repo_root = cwd.as_deref().and_then(repo_root_for_cwd);
    let record = LaunchRecord {
        id: id.clone(),
        source: source.to_string(),
        clud_pid: std::process::id(),
        backend: plan.backend.executable_name().to_string(),
        launch_mode: plan.launch_mode.as_str().to_string(),
        cwd,
        repo_root,
        command: plan.command.clone(),
        clud_argv: std::env::args().collect(),
        launched_at_ms,
        exited_at_ms: None,
        exit_code: None,
    };
    write_record(state_dir, &record)?;
    prune_old_records(state_dir);
    Ok(LaunchLogHandle {
        state_dir: state_dir.to_path_buf(),
        id,
    })
}

pub fn finish_launch(state_dir: &Path, id: &str, exit_code: i32) -> io::Result<()> {
    let path = record_path(state_dir, id);
    let bytes = fs::read(&path)?;
    let mut record: LaunchRecord = serde_json::from_slice(&bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    record.exited_at_ms = Some(unix_millis_now());
    record.exit_code = Some(exit_code);
    write_record(state_dir, &record)
}

pub fn read_recent(state_dir: &Path) -> Vec<LaunchRecord> {
    let dir = launches_dir(state_dir);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(record) = serde_json::from_slice::<LaunchRecord>(&bytes) else {
            continue;
        };
        records.push(record);
    }
    records.sort_by(|a, b| b.launched_at_ms.cmp(&a.launched_at_ms));
    records.truncate(MAX_RECORDS);
    records
}

pub fn unix_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn launch_cwd(plan: &LaunchPlan) -> Option<String> {
    plan.cwd.clone().or_else(|| {
        std::env::current_dir()
            .ok()
            .map(|path| path.display().to_string())
    })
}

pub fn repo_root_for_cwd(cwd: &str) -> Option<String> {
    let cwd = PathBuf::from(cwd);
    let root = loop_spec::git_root_from(&cwd);
    if root.join(".git").exists() {
        Some(root.display().to_string())
    } else {
        None
    }
}

fn write_record(state_dir: &Path, record: &LaunchRecord) -> io::Result<()> {
    let path = record_path(state_dir, &record.id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
    fs::write(path, bytes)
}

fn launches_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(DIR_NAME)
}

fn record_path(state_dir: &Path, id: &str) -> PathBuf {
    launches_dir(state_dir).join(format!("{id}.json"))
}

fn prune_old_records(state_dir: &Path) {
    let dir = launches_dir(state_dir);
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    if paths.len() <= MAX_RECORDS {
        return;
    }
    paths.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });
    let remove_count = paths.len().saturating_sub(MAX_RECORDS);
    for path in paths.into_iter().take(remove_count) {
        let _ = fs::remove_file(path);
    }
}
