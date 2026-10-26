//! Issue #2101 item 1: warn, then clean.
//!
//! When `~/.clud/worktrees` is over `worktrees.warn_bytes`, each of the
//! largest worktrees (`gc::worktree_top`) that the verdict cannot prove safe
//! gets a removal date [`GRACE_SECS`] out, recorded in
//! `~/.clud/worktrees-schedule.json`. The launch banner names the date. A
//! tick after the date moves the worktree to `clud trash` (recoverable for
//! 72 h) instead of deleting it. A candidate leaves the schedule as soon as
//! it is kept (`clud gc keep`), in use, reclaimable by the normal verdict, or
//! no longer among the largest, and the whole schedule clears once the root
//! is back under the limit.
//!
//! [`plan`] is the pure decision; the daemon's probe thread gathers the facts
//! and performs the moves.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name of the schedule, a sibling of `worktrees`.
pub const SCHEDULE_FILE_NAME: &str = "worktrees-schedule.json";

/// Warning period before a scheduled worktree moves to trash.
pub const GRACE_SECS: i64 = 7 * 24 * 60 * 60;

/// A worktree has to be idle this long before it is even warned about.
pub const MIN_IDLE_SECS: i64 = 24 * 60 * 60;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// Path → unix time after which it moves to trash.
    #[serde(default)]
    pub remove_after: BTreeMap<String, i64>,
}

/// One of the largest worktrees, with the facts that can exempt it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: String,
    pub bytes: u64,
    pub idle_secs: i64,
    /// Kept, in use, unknown to the probe, or already reclaimable.
    pub exempt: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Newly scheduled; log the date.
    Warn {
        path: String,
        bytes: u64,
        remove_after: i64,
    },
    /// The date has passed: move it to trash.
    Trash { path: String },
}

/// Update `schedule` for this tick and return what to do.
pub fn plan(
    schedule: &mut Schedule,
    over: bool,
    candidates: &[Candidate],
    now: i64,
) -> Vec<Action> {
    if !over {
        schedule.remove_after.clear();
        return Vec::new();
    }
    let eligible: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| !c.exempt && c.idle_secs >= MIN_IDLE_SECS)
        .collect();
    schedule
        .remove_after
        .retain(|path, _| eligible.iter().any(|c| &c.path == path));
    let mut actions = Vec::new();
    for c in eligible {
        match schedule.remove_after.get(&c.path) {
            Some(&after) if now >= after => {
                schedule.remove_after.remove(&c.path);
                actions.push(Action::Trash {
                    path: c.path.clone(),
                });
            }
            Some(_) => {}
            None => {
                let remove_after = now + GRACE_SECS;
                schedule.remove_after.insert(c.path.clone(), remove_after);
                actions.push(Action::Warn {
                    path: c.path.clone(),
                    bytes: c.bytes,
                    remove_after,
                });
            }
        }
    }
    actions
}

/// `<wt_root>/../worktrees-schedule.json`.
pub fn schedule_path_for(wt_root: &Path) -> PathBuf {
    wt_root.parent().unwrap_or(wt_root).join(SCHEDULE_FILE_NAME)
}

/// Missing or corrupt reads as empty: an empty schedule only delays.
pub fn load(wt_root: &Path) -> Schedule {
    fs::read_to_string(schedule_path_for(wt_root))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Atomic write; an empty schedule removes the file.
pub fn save(wt_root: &Path, schedule: &Schedule) -> std::io::Result<()> {
    let path = schedule_path_for(wt_root);
    if schedule.remove_after.is_empty() {
        return match fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let text = serde_json::to_string(schedule).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &path)
}

/// Banner suffix: `; moves to trash unless kept: a in 3d, b in 6d`.
pub fn banner_suffix(schedule: &Schedule, now: i64) -> String {
    if schedule.remove_after.is_empty() {
        return String::new();
    }
    let items: Vec<String> = schedule
        .remove_after
        .iter()
        .map(|(path, after)| {
            let name = Path::new(path)
                .file_name()
                .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
            let days = (after - now).max(0).div_ceil(24 * 60 * 60);
            format!("{name} in {days}d")
        })
        .collect();
    format!(
        "; moves to trash unless `clud gc keep`: {}",
        items.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 24 * 60 * 60;

    fn cand(path: &str, exempt: bool) -> Candidate {
        Candidate {
            path: path.to_string(),
            bytes: 10,
            idle_secs: 2 * DAY,
            exempt,
        }
    }

    #[test]
    fn warns_first_then_trashes_after_the_date() {
        let mut s = Schedule::default();
        let c = [cand("/wt/a", false)];
        let first = plan(&mut s, true, &c, 0);
        assert_eq!(
            first,
            [Action::Warn {
                path: "/wt/a".into(),
                bytes: 10,
                remove_after: GRACE_SECS
            }]
        );
        assert!(
            plan(&mut s, true, &c, GRACE_SECS - 1).is_empty(),
            "no repeat warning"
        );
        assert_eq!(
            plan(&mut s, true, &c, GRACE_SECS),
            [Action::Trash {
                path: "/wt/a".into()
            }]
        );
        assert!(s.remove_after.is_empty());
    }

    #[test]
    fn kept_recent_and_under_budget_are_never_trashed() {
        let mut s = Schedule::default();
        plan(&mut s, true, &[cand("/wt/a", false)], 0);
        // Kept (or in use, or reclaimable) after the warning: dropped.
        assert!(plan(&mut s, true, &[cand("/wt/a", true)], GRACE_SECS).is_empty());
        assert!(s.remove_after.is_empty());
        // Recently used: never scheduled.
        let mut fresh = cand("/wt/b", false);
        fresh.idle_secs = DAY - 1;
        assert!(plan(&mut s, true, &[fresh], 0).is_empty());
        // Back under budget: the schedule clears.
        plan(&mut s, true, &[cand("/wt/c", false)], 0);
        assert!(plan(&mut s, false, &[cand("/wt/c", false)], GRACE_SECS).is_empty());
        assert!(s.remove_after.is_empty());
    }

    #[test]
    fn schedule_round_trips_and_names_days_left() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("worktrees");
        let mut s = Schedule::default();
        s.remove_after
            .insert(root.join("repo-wt-x").to_string_lossy().into(), 3 * DAY);
        save(&root, &s).unwrap();
        let loaded = load(&root);
        assert_eq!(loaded, s);
        assert_eq!(
            banner_suffix(&loaded, DAY / 2),
            "; moves to trash unless `clud gc keep`: repo-wt-x in 3d"
        );
        save(&root, &Schedule::default()).unwrap();
        assert!(!schedule_path_for(&root).exists());
    }
}
