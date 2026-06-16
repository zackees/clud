use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use super::reconcile::{best_effort_branch, path_matches_repo_root, ScanKind};
use super::registry::now_unix;
use super::InsertInput;
use crate::worktrees;

// ---------- background scanner thread ----------

/// Polling scanner that watches a `.claude/worktrees/` directory and
/// inserts new agent-* subdirs into the registry as they appear.
/// Cancels cooperatively via `Arc<AtomicBool>`.
///
/// Issue #135 Phase 1: the scanner now sends `gc.insert` IPC ops to the
/// daemon instead of opening redb directly. If the daemon is unreachable
/// the scanner logs once at debug level and stops trying for the rest of
/// the session. Phase 2 moves this entire scanner into the daemon
/// process; for now the scanner thread still lives in the clud-bin
/// process.
pub struct WorktreeScanner {
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl WorktreeScanner {
    /// Spawn a scanner watching the *current* repo's `.claude/worktrees/`.
    /// Returns `None` if the repo root can't be located — the caller logs
    /// and continues.
    pub fn maybe_spawn() -> Option<Self> {
        let main_root = match worktrees::locate_main_repo_root() {
            Ok(p) => p,
            Err(_) => {
                // Not inside a git repo (e.g. running `clud` from /tmp).
                // No worktrees to scan — skip spawning.
                return None;
            }
        };
        let watch_dir = main_root.join(".claude").join("worktrees");
        Some(Self::spawn_with_kind(
            watch_dir,
            Some(main_root),
            ScanKind::Worktree,
        ))
    }

    /// Spawn a scanner watching the current repo's `.extern-repos/`.
    /// Returns `None` if the repo root can't be located.
    pub fn maybe_spawn_extern_repos() -> Option<Self> {
        let main_root = match worktrees::locate_main_repo_root() {
            Ok(p) => p,
            Err(_) => return None,
        };
        let watch_dir = main_root.join(".extern-repos");
        Some(Self::spawn_with_kind(
            watch_dir,
            Some(main_root),
            ScanKind::ExternRepo,
        ))
    }

    /// Spawn a scanner watching top-level sibling temp clones next to the
    /// current repo. Returns `None` if the repo root can't be located or
    /// has no parent directory.
    pub fn maybe_spawn_sibling_clones() -> Option<Self> {
        let main_root = match worktrees::locate_main_repo_root() {
            Ok(p) => p,
            Err(_) => return None,
        };
        let parent = main_root.parent()?.to_path_buf();
        Some(Self::spawn_with_kind(
            parent,
            Some(main_root),
            ScanKind::SiblingClone,
        ))
    }

    /// Explicit spawn. Tests pass a custom watch dir. Inserts go through
    /// the GC daemon IPC; if the daemon is unreachable the scanner gives
    /// up silently.
    pub fn spawn(watch_dir: PathBuf, repo_root: Option<PathBuf>) -> Self {
        Self::spawn_with_kind(watch_dir, repo_root, ScanKind::Worktree)
    }

    fn spawn_with_kind(
        watch_dir: PathBuf,
        repo_root: Option<PathBuf>,
        scan_kind: ScanKind,
    ) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_t = cancel.clone();
        let thread_name = format!("clud-gc-scanner-{}", scan_kind.kind());
        let handle = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run_scanner_loop(watch_dir, repo_root, scan_kind, cancel_t))
            .expect("spawn scanner thread");
        Self {
            cancel,
            handle: Some(handle),
        }
    }

    /// Signal cancellation **without** waiting for the worker thread.
    ///
    /// Issue #285 rec 3: callers that hold several scanners (the main
    /// launch path holds three) can call `signal_cancel` on all of them
    /// first, then drop each guard, so the joins overlap rather than
    /// serializing 3 × poll-interval of latency into the Ctrl-C exit
    /// path. Safe to call from `&self` because the only state mutated
    /// is the cancel `AtomicBool`.
    pub fn signal_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Signal cancellation and wait for the worker thread to exit.
    pub fn cancel(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for WorktreeScanner {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Walk `watch_dir` once, sending `gc.insert` IPC ops for each matching
/// immediate subdir. Returns `Err` on the first IPC failure so the caller
/// can stop retrying.
fn scan_once_via_ipc(
    watch_dir: &Path,
    repo_root: Option<&Path>,
    scan_kind: ScanKind,
) -> Result<(), String> {
    let entries = match std::fs::read_dir(watch_dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("read_dir({:?}): {e}", watch_dir)),
    };
    for entry in entries.flatten() {
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if !ft.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        if !scan_kind.accepts_dir_name(&name_str, repo_root) {
            continue;
        }
        let path = entry.path();
        if matches!(scan_kind, ScanKind::SiblingClone)
            && repo_root
                .map(|root| path_matches_repo_root(&path, root))
                .unwrap_or(false)
        {
            continue;
        }
        let path_str = path.to_string_lossy().to_string();
        let branch = best_effort_branch(&path);
        let input = InsertInput {
            kind: scan_kind.kind().to_string(),
            path: path_str,
            repo_root: repo_root.map(|p| p.to_string_lossy().to_string()),
            branch,
            agent_id: scan_kind.agent_id(&name_str),
            now_unix: now_unix(),
        };
        let state_dir = crate::daemon::default_state_dir().map_err(|e| e.to_string())?;
        crate::daemon::gc_client_insert(&state_dir, &input).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn run_scanner_loop(
    watch_dir: PathBuf,
    repo_root: Option<PathBuf>,
    scan_kind: ScanKind,
    cancel: Arc<AtomicBool>,
) {
    let repo_root_ref = repo_root.as_deref();
    let mut ipc_failed = false;
    while !cancel.load(Ordering::SeqCst) {
        if !ipc_failed {
            if let Err(e) = scan_once_via_ipc(&watch_dir, repo_root_ref, scan_kind) {
                // Best-effort: log once, then stop trying.
                if std::env::var_os("CLUD_GC_SCANNER_VERBOSE").is_some() {
                    eprintln!("[clud] debug: gc scanner: daemon ipc failed: {e}");
                }
                ipc_failed = true;
            }
        }
        // Interruptible sleep: 80 × 25ms = ~2s outer cycle, but
        // cancellable within ~25ms. Issue #285 rec 3 dropped this from
        // 100ms granularity so the Ctrl-C teardown's scanner joins
        // don't add up to several hundred ms of dead time before the
        // shell returns.
        for _ in 0..80 {
            if cancel.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}
