//! Per-run `/grind` facts, keyed by the Claude Code session id (#1337).
//!
//! The `/grind` router records its run facts (`mode`, `ci`, `scripts`, `meta`,
//! `feature`, ...) in `~/.clud/tmp/grind/<session_id>.json`, and clud's command
//! hook reads exactly that file for the `session_id` in each PreToolUse
//! payload. A workflow agent's payload carries its parent session's id
//! (`tests/harness/test_grind_facts.py` pins this), so every agent resolves
//! its own run's facts: two `/grind` runs in one repo never share a file, and
//! one run finishing cannot change another's caps.
//!
//! The file lives under clud's temp directory, not the working tree, so
//! `git status` stays clean and the 72-hour session-temp sweep removes what a
//! crashed run left behind. [`lookup_in`] also ignores a file older than that,
//! so a resumed session never inherits a dead run's facts.
//!
//! The router learns the path from `clud grind-facts path`, which reads
//! [`SESSION_ENV`] (Claude Code exports it to every shell command), and its
//! Finish step removes the file with `clud grind-facts clear`. The planner
//! records each worker task's checkout and files with `clud grind-facts task`
//! (#1340), which the hook turns into that worker's deletion roots.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

/// The session id Claude Code exports to shell commands; it equals the
/// `session_id` of the same session's hook payloads.
pub const SESSION_ENV: &str = "CLAUDE_CODE_SESSION_ID";

/// Directory under `~/.clud/tmp` that holds the facts files.
pub const SUBDIR: &str = "grind";

/// A facts file older than this belongs to a run that is gone.
pub const STALE_AFTER: Duration = crate::gc::session_tmp::STALE_THRESHOLD;

/// `~/.clud/tmp/grind`, or `None` without a home directory.
pub fn facts_dir() -> Option<PathBuf> {
    Some(crate::gc::session_tmp::session_tmp_dir()?.join(SUBDIR))
}

/// Whether `id` is safe to use as a file name: Claude Code session ids are
/// UUIDs, and anything with a separator or `..` is refused.
pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The facts file for `session_id` under `dir`, or `None` for an invalid id.
pub fn facts_path_in(dir: &Path, session_id: &str) -> Option<PathBuf> {
    valid_session_id(session_id).then(|| dir.join(format!("{session_id}.json")))
}

/// What [`lookup_in`] found for one session.
#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    /// The run's facts.
    Found(Value),
    /// The payload carried no usable session id.
    NoSession,
    /// No facts file for this session.
    Missing(PathBuf),
    /// A facts file older than [`STALE_AFTER`].
    Stale(PathBuf),
    /// A facts file that is not a JSON object.
    Invalid(PathBuf, String),
}

impl Lookup {
    /// The facts, when they were found.
    pub fn facts(&self) -> Option<&Value> {
        match self {
            Self::Found(value) => Some(value),
            _ => None,
        }
    }

    /// Why no facts apply, for the hook log. `None` when they were found.
    pub fn warning(&self) -> Option<String> {
        let tail = "applying the strictest /grind caps (sequential, no CI)";
        match self {
            Self::Found(_) => None,
            Self::NoSession => Some(format!(
                "grind run facts: no session id in the payload; {tail}"
            )),
            Self::Missing(path) => Some(format!(
                "grind run facts: none at {}; {tail}",
                path.display()
            )),
            Self::Stale(path) => Some(format!(
                "grind run facts: {} is older than {}h, ignored; {tail}",
                path.display(),
                STALE_AFTER.as_secs() / 3_600
            )),
            Self::Invalid(path, error) => Some(format!(
                "grind run facts: {} is unreadable ({error}); {tail}",
                path.display()
            )),
        }
    }
}

/// Read the facts for `session_id` from `dir`, as of `now`.
pub fn lookup_in(dir: &Path, session_id: Option<&str>, now: SystemTime) -> Lookup {
    let Some(path) = session_id.and_then(|id| facts_path_in(dir, id)) else {
        return Lookup::NoSession;
    };
    let Ok(meta) = std::fs::metadata(&path) else {
        return Lookup::Missing(path);
    };
    let age = meta
        .modified()
        .ok()
        .and_then(|mtime| now.duration_since(mtime).ok())
        .unwrap_or_default();
    if age > STALE_AFTER {
        return Lookup::Stale(path);
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => return Lookup::Invalid(path, error.to_string()),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() => Lookup::Found(value),
        Ok(_) => Lookup::Invalid(path, "not a JSON object".into()),
        Err(error) => Lookup::Invalid(path, error.to_string()),
    }
}

/// [`lookup_in`] against the real `~/.clud/tmp/grind`, now.
pub fn lookup(session_id: Option<&str>) -> Lookup {
    match facts_dir() {
        Some(dir) => lookup_in(&dir, session_id, SystemTime::now()),
        None => Lookup::NoSession,
    }
}

const USAGE: &str = "usage: clud grind-facts <path|clear|task>
  path   print this session's run-facts file (~/.clud/tmp/grind/<session>.json)
         and mark an existing one as current, so a long run never goes stale
  clear  remove this session's run-facts file
  task --checkout <dir> [--] <file>...
         record one worker task's files (the grind planner runs this)";

/// `clud grind-facts <path|clear>`. Returns the exit code.
pub fn run_cli(args: &[String]) -> i32 {
    let env = std::env::var(SESSION_ENV).ok();
    let Some(dir) = facts_dir() else {
        eprintln!("clud grind-facts: cannot resolve the home directory");
        return 1;
    };
    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    run_cli_in(args, env.as_deref(), &dir, &mut out, &mut err)
}

fn run_cli_in(
    args: &[String],
    session_id: Option<&str>,
    dir: &Path,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> i32 {
    let action = match args {
        [action] if matches!(action.as_str(), "path" | "clear") => action.as_str(),
        [action, ..] if action == "task" => "task",
        _ => {
            let _ = writeln!(err, "{USAGE}");
            return 2;
        }
    };
    let Some(path) = session_id.and_then(|id| facts_path_in(dir, id)) else {
        let _ = writeln!(
            err,
            "clud grind-facts: {SESSION_ENV} is missing or invalid; run this from a Claude Code \
             session's shell"
        );
        return 2;
    };
    match action {
        "task" => {
            let (checkout, files) = match parse_task(&args[1..]) {
                Ok(task) => task,
                Err(error) => {
                    let _ = writeln!(err, "clud grind-facts task: {error}\n{USAGE}");
                    return 2;
                }
            };
            match record_task(&path, &checkout, &files) {
                Ok(count) => {
                    let _ = writeln!(
                        out,
                        "recorded task {count}: {} file(s) in {}",
                        files.len(),
                        checkout.display()
                    );
                    0
                }
                Err(error) => {
                    let _ = writeln!(err, "clud grind-facts task: {error}");
                    1
                }
            }
        }
        "path" => {
            if let Err(error) = std::fs::create_dir_all(dir) {
                let _ = writeln!(err, "clud grind-facts: create {}: {error}", dir.display());
                return 1;
            }
            // Staleness is by age, so asking for the path keeps a live run's
            // facts current: `/grind-cron` asks on every tick, and a loop may
            // outlive [`STALE_AFTER`].
            if let Err(error) = touch_existing(&path) {
                let _ = writeln!(err, "clud grind-facts: refresh {}: {error}", path.display());
                return 1;
            }
            let _ = writeln!(out, "{}", path.display());
            0
        }
        _ => match std::fs::remove_file(&path) {
            Ok(()) => {
                let _ = writeln!(out, "removed {}", path.display());
                0
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let _ = writeln!(out, "no run facts at {}", path.display());
                0
            }
            Err(error) => {
                let _ = writeln!(err, "clud grind-facts: remove {}: {error}", path.display());
                1
            }
        },
    }
}

/// `--checkout <dir> [--] <file>...`; the checkout must be absolute.
fn parse_task(args: &[String]) -> Result<(PathBuf, Vec<String>), String> {
    let mut checkout = None;
    let mut files = Vec::new();
    let mut flags = true;
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        i += 1;
        if flags && arg == "--" {
            flags = false;
        } else if flags && arg == "--checkout" {
            checkout = args.get(i).map(PathBuf::from);
            i += 1;
        } else if flags && arg.starts_with("--checkout=") {
            checkout = Some(PathBuf::from(&arg["--checkout=".len()..]));
        } else if flags && arg.starts_with('-') {
            return Err(format!("unknown option {arg:?}"));
        } else {
            files.push(arg.clone());
        }
    }
    let checkout = checkout.ok_or("--checkout <dir> is required")?;
    if !checkout.is_absolute() {
        return Err("--checkout must be an absolute path".into());
    }
    if files.is_empty() {
        return Err("name the task's files".into());
    }
    Ok((checkout, files))
}

/// Append `{worktree, files}` to the facts' `tasks`, under a lock file so
/// concurrent planners never lose each other's tasks. Returns the new count.
fn record_task(path: &Path, checkout: &Path, files: &[String]) -> Result<usize, String> {
    let dir = path.parent().ok_or("facts path has no directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let lock = path.with_extension("lock");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // A lock left by a crashed writer is stale after the deadline.
                if std::time::Instant::now() > deadline {
                    let _ = std::fs::remove_file(&lock);
                    continue;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(format!("lock {}: {error}", lock.display())),
        }
    }
    let result = (|| {
        let mut facts: Value = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| format!("parse facts: {e}"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
            Err(error) => return Err(format!("read facts: {error}")),
        };
        let object = facts.as_object_mut().ok_or("facts are not a JSON object")?;
        let tasks = object
            .entry("tasks")
            .or_insert_with(|| Value::Array(Vec::new()));
        let tasks = tasks.as_array_mut().ok_or("facts' tasks is not an array")?;
        tasks.push(serde_json::json!({"worktree": checkout, "files": files}));
        let count = tasks.len();
        let text = serde_json::to_string_pretty(&facts).map_err(|e| e.to_string())?;
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, text).map_err(|e| format!("write facts: {e}"))?;
        std::fs::rename(&temp, path).map_err(|e| format!("replace facts: {e}"))?;
        Ok(count)
    })();
    let _ = std::fs::remove_file(&lock);
    result
}

/// Set an existing file's modification time to now; a missing file is fine.
fn touch_existing(path: &Path) -> std::io::Result<()> {
    match std::fs::OpenOptions::new().append(true).open(path) {
        Ok(file) => file.set_modified(SystemTime::now()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "11111111-2222-3333-4444-555555555555";
    const B: &str = "66666666-7777-8888-9999-000000000000";

    fn write(dir: &Path, id: &str, text: &str) -> PathBuf {
        let path = facts_path_in(dir, id).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn each_session_reads_only_its_own_facts() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), A, r#"{"mode":"parallel","ci":true}"#);
        write(dir.path(), B, r#"{"mode":"sequential"}"#);
        let now = SystemTime::now();
        let a = lookup_in(dir.path(), Some(A), now);
        let b = lookup_in(dir.path(), Some(B), now);
        assert_eq!(a.facts().unwrap()["mode"], "parallel");
        assert_eq!(b.facts().unwrap()["mode"], "sequential");
        std::fs::remove_file(facts_path_in(dir.path(), B).unwrap()).unwrap();
        assert_eq!(
            lookup_in(dir.path(), Some(A), now).facts().unwrap()["mode"],
            "parallel",
            "one run finishing must not change the other's facts"
        );
    }

    #[test]
    fn missing_stale_and_invalid_facts_warn() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        assert!(matches!(
            lookup_in(dir.path(), None, now),
            Lookup::NoSession
        ));
        assert!(matches!(
            lookup_in(dir.path(), Some("../escape"), now),
            Lookup::NoSession
        ));
        let missing = lookup_in(dir.path(), Some(A), now);
        assert!(matches!(missing, Lookup::Missing(_)));
        assert!(missing.warning().unwrap().contains("strictest"));
        write(dir.path(), A, "[1]");
        assert!(matches!(
            lookup_in(dir.path(), Some(A), now),
            Lookup::Invalid(..)
        ));
        write(dir.path(), A, r#"{"mode":"parallel"}"#);
        let later = now + STALE_AFTER + Duration::from_secs(60);
        let stale = lookup_in(dir.path(), Some(A), later);
        assert!(matches!(stale, Lookup::Stale(_)));
        assert!(stale.warning().unwrap().contains("ignored"));
        assert!(lookup_in(dir.path(), Some(A), now).warning().is_none());
    }

    #[test]
    fn planners_record_tasks_without_losing_each_others() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("grind");
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir, A, r#"{"mode":"parallel"}"#);
        let checkout = root.path().join("repo-wt-1");
        let checkout_arg = checkout.to_string_lossy().into_owned();
        std::thread::scope(|scope| {
            for i in 0..8 {
                let (dir, checkout_arg) = (&dir, &checkout_arg);
                scope.spawn(move || {
                    let file = format!("src/f{i}.rs");
                    let (code, _, err) = cli(
                        &["task", "--checkout", checkout_arg, "--", &file],
                        Some(A),
                        dir,
                    );
                    assert_eq!(code, 0, "{err}");
                });
            }
        });
        let facts = lookup_in(&dir, Some(A), SystemTime::now());
        let facts = facts.facts().unwrap();
        assert_eq!(facts["mode"], "parallel", "other facts are kept");
        assert_eq!(facts["tasks"].as_array().unwrap().len(), 8);
        assert_eq!(facts["tasks"][0]["worktree"], checkout_arg.as_str());
        for bad in [
            vec!["task", "src/a.rs"],
            vec!["task", "--checkout", "relative", "src/a.rs"],
            vec!["task", "--checkout", checkout_arg.as_str()],
            vec!["task", "--bogus", "x"],
        ] {
            assert_eq!(cli(&bad, Some(A), &dir).0, 2, "{bad:?}");
        }
    }

    #[test]
    fn session_ids_are_file_name_safe() {
        assert!(valid_session_id(A));
        for bad in ["", "a/b", "..", "a\\b", "a b", "a.json"] {
            assert!(!valid_session_id(bad), "{bad}");
        }
    }

    fn cli(args: &[&str], id: Option<&str>, dir: &Path) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_cli_in(&args, id, dir, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn cli_prints_the_path_and_clears_only_its_own_file() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("grind");
        let (code, out, _) = cli(&["path"], Some(A), &dir);
        assert_eq!(code, 0);
        assert_eq!(out.trim(), dir.join(format!("{A}.json")).to_string_lossy());
        assert!(dir.is_dir());
        write(&dir, A, "{}");
        let other = write(&dir, B, "{}");
        let (code, out, _) = cli(&["clear"], Some(A), &dir);
        assert_eq!(code, 0);
        assert!(out.starts_with("removed"));
        assert!(!facts_path_in(&dir, A).unwrap().exists());
        assert!(other.exists());
        assert_eq!(cli(&["clear"], Some(A), &dir).0, 0);
        // Asking for the path keeps a long run's facts current.
        let old = SystemTime::now() - STALE_AFTER - Duration::from_secs(3_600);
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(facts_path_in(&dir, B).unwrap())
            .unwrap();
        file.set_modified(old).unwrap();
        drop(file);
        assert!(matches!(
            lookup_in(&dir, Some(B), SystemTime::now()),
            Lookup::Stale(_)
        ));
        assert_eq!(cli(&["path"], Some(B), &dir).0, 0);
        assert!(lookup_in(&dir, Some(B), SystemTime::now())
            .facts()
            .is_some());
        let (code, _, err) = cli(&["path"], None, &dir);
        assert_eq!(code, 2);
        assert!(err.contains(SESSION_ENV));
        assert_eq!(cli(&["bogus"], Some(A), &dir).0, 2);
        assert_eq!(cli(&[], Some(A), &dir).0, 2);
    }
}
