//! Best-effort termination of an entire descendant process tree.
//!
//! Background — Ctrl+C on Windows for `clud --codex loop`:
//!
//! On Windows, `clud --codex` routes through `cmd /D /S /C "codex.cmd ..."`
//! (the BatBadBat / CVE-2024-24576 workaround in [`crate::subprocess`]).
//! That means the actual process tree at runtime is:
//!
//! ```text
//! clud.exe → cmd.exe → node.exe (real codex)
//! ```
//!
//! When the user hits Ctrl+C, `process.kill()` on a
//! `running_process::NativeProcess` only terminates the **direct**
//! child (cmd.exe). The orphaned `node.exe` keeps writing to the inherited
//! console for several seconds until clud itself exits and its Job Object
//! closes — that's the multi-second hang users were reporting.
//!
//! The fix is to walk the descendant tree before reaping the direct child.
//! This module provides [`kill_tree`] for that. It mirrors the
//! `signal_process_tree` helper already used by [`crate::daemon`]: scan
//! the process table with `sysinfo`, walk parent→children, and SIGKILL
//! (or Windows `TerminateProcess`) every descendant before the root.
//!
//! Best-effort: failures are silent and the whole operation is bounded by
//! the cost of one `sysinfo` system snapshot, which is well under our
//! sub-second Ctrl+C latency target.
//!
//! [`try_break_group`] is the cooperative companion on Windows: it sends
//! `CTRL_BREAK_EVENT` to the child's console process group so a
//! well-behaved agent can flush state before the hard `kill_tree` follows.

use std::collections::HashMap;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System};

use crate::process_identity::ProcessIdentity;
#[cfg(any(windows, test))]
use crate::process_identity::UNKNOWN_START_TIME;

#[cfg(any(windows, test))]
pub(crate) fn automatic_identity_matches(
    recorded: ProcessIdentity,
    observed: ProcessIdentity,
) -> bool {
    recorded.start_time != UNKNOWN_START_TIME
        && observed.start_time != UNKNOWN_START_TIME
        && recorded == observed
}

#[cfg(any(windows, test))]
fn automatic_target_allowed(
    recorded: ProcessIdentity,
    observed: ProcessIdentity,
    image_name: &str,
) -> bool {
    automatic_identity_matches(recorded, observed) && !is_console_host_image(image_name)
}

/// Console host images for both the inbox ConPTY (`conhost.exe`) and the
/// sidecar backend (`OpenConsole.exe`), #1367. The upstream Job assignment
/// and orphan scan are fixed in zackees/running-process#1222.
#[cfg(any(windows, test))]
fn is_console_host_image(name: &str) -> bool {
    name.eq_ignore_ascii_case("conhost.exe") || name.eq_ignore_ascii_case("openconsole.exe")
}

/// Kill the process tree rooted at `pid`, including the root itself.
///
/// Best-effort and cross-platform. Uses `sysinfo` to enumerate descendants
/// (the same approach already in [`crate::daemon::signal_process_tree`])
/// so we don't need to shell out to OS helpers like `taskkill` or `pgrep`.
///
/// We refresh with `ProcessRefreshKind::nothing()` — we only need the
/// parent-PID graph, not CPU/memory/cmdline. On Windows in particular,
/// `System::new_all()` enumerates every process's full metadata and takes
/// tens of seconds; the minimal refresh is sub-second, which is the budget
/// we have on the Ctrl+C path.
pub fn kill_tree(pid: u32) {
    kill_tree_filtered(pid, &|_| true);
}

/// Kill the tree rooted at `pid`, consulting `may_kill` for every process.
///
/// `may_kill(pid) == false` **prunes**: that process is spared *and so is its
/// entire subtree*. Sparing a daemon while killing its children would leave
/// it wedged mid-work, which is worse than either extreme — a build daemon's
/// compiler children are its in-flight work, not leaked garbage.
///
/// # Why a caller-supplied predicate instead of a policy baked in here
///
/// The two callers want opposite things. Automatic reapers (shell exit,
/// orphan sweep) must spare declared daemons — a `zccache`/`soldr`/`fbuild`
/// server started by an agent bash command is not leaked garbage, and killing
/// it throws away a warm cache shared with every other session. Deliberate
/// kills (`clud kill`, `clud slay`, Ctrl+C) mean *everything*, and pass the
/// permissive predicate via [`kill_tree`].
///
/// # Why the predicate takes only a PID
///
/// This runs on the Ctrl+C path, so the snapshot is deliberately built with
/// `ProcessRefreshKind::nothing()` (see the note on [`kill_tree`]) — no
/// cmdline, no environment. A predicate needing richer facts must precompute
/// them and close over the result; [`crate::orphan_reaper`] already has the
/// originator-tagged PID set in hand and closes over that. Keeping the
/// predicate a pure PID lookup is what preserves the sub-second budget.
///
/// Note this is deliberately *not* a name allowlist. "Is this a daemon?" is
/// answered by whether the process carries the inherited
/// `RUNNING_PROCESS_ORIGINATOR` tag: everything an agent spawns inherits it
/// transitively, and a process that spawned itself as a daemon has stripped
/// it. Matching on image names would misfire on every unrelated build.
pub fn kill_tree_filtered(pid: u32, may_kill: &dyn Fn(u32) -> bool) {
    TopologySnapshot::capture().kill_tree_filtered(pid, may_kill);
}

/// One host process-topology walk, reusable across many kills.
///
/// This is the `ProcessRefreshKind::nothing()` tier — pid, parent pid, image
/// name and creation time — which is everything a kill path needs and nothing
/// it does not. It exists because the orphan sweep used to pay for a **fresh
/// host walk per orphan**: a 20-orphan sweep walked the host process table 20
/// times to answer 20 questions it could have answered from one (#673 Phase 8).
///
/// Capture once per sweep, not once per target.
pub struct TopologySnapshot {
    system: System,
}

impl TopologySnapshot {
    pub fn capture() -> Self {
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        Self { system }
    }

    /// The identity of `pid` as this snapshot saw it, or `None` if the PID was
    /// already gone when the snapshot was taken.
    pub fn identity(&self, pid: u32) -> Option<ProcessIdentity> {
        ProcessIdentity::observe_in(&self.system, pid)
    }

    /// [`kill_tree_filtered`] against this snapshot rather than a fresh walk.
    ///
    /// Selection and termination therefore share one view of the process
    /// table: a target is chosen and killed from the same observation, which
    /// is a narrower window than choosing from one walk and killing from
    /// another.
    pub fn kill_tree_filtered(&self, pid: u32, may_kill: &dyn Fn(u32) -> bool) {
        let root = Pid::from_u32(pid);
        if self.system.process(root).is_none() {
            // Already dead, or never existed. Nothing to do.
            return;
        }
        if !may_kill(pid) {
            // Root is exempt — the whole tree is pruned.
            return;
        }

        // Kill leaves first, root last. `descendants` is BFS order
        // (root's children, then grandchildren, ...); reversing gets us
        // deepest-first.
        let mut descendants = descendant_pids_filtered(&self.system, root, may_kill);
        descendants.reverse();
        descendants.push(root);

        for descendant in descendants {
            let Some(process) = self.system.process(descendant) else {
                continue;
            };
            // #688: re-verify *every* target, not only the root. The caller
            // checks the root's `(pid, creation_time)` before entering here,
            // but descendants were previously killed straight from this
            // snapshot on a bare PID — and this snapshot can be seconds old by
            // the time the sweep reaches its last orphan. A PID that exits and
            // is recycled in that window would be killed as somebody else, and
            // because this is a *tree* kill, so would its children.
            //
            // `matches` (rather than the stricter automatic gate) is
            // deliberate: on a host whose OS declines to report creation times
            // this degrades to the PID-only comparison clud used before start
            // times existed, instead of silently disabling tree kills
            // wholesale.
            let recorded = ProcessIdentity::new(descendant.as_u32(), process.start_time());
            match ProcessIdentity::observe(descendant.as_u32()) {
                Some(observed) if recorded.matches(&observed) => {}
                _ => continue,
            }
            // `kill_with(Signal::Kill)` is SIGKILL on Unix. On Windows it
            // returns `None` (signals aren't a Windows concept), so we
            // always follow up with `process.kill()` which is
            // `TerminateProcess` on Windows and a no-op redundant SIGKILL
            // on Unix.
            let _ = process.kill_with(Signal::Kill);
            let _ = process.kill();
        }
    }
}

/// Kill an automatically selected tree only while every target still has the
/// identity and image observed by the selection snapshot.
///
/// This path is deliberately stricter than [`kill_tree_filtered`]. Automatic
/// cleanup must never act on a bare PID, and console hosts (`conhost.exe`,
/// `OpenConsole.exe`) are rejected inside
/// the same last-responsible-moment snapshot used to select each kill target.
#[cfg(windows)]
pub fn kill_tree_filtered_automatic(
    root_identity: ProcessIdentity,
    may_kill: &dyn Fn(u32) -> bool,
) {
    if root_identity.start_time == UNKNOWN_START_TIME || !may_kill(root_identity.pid) {
        return;
    }

    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let root = Pid::from_u32(root_identity.pid);
    let Some(root_process) = system.process(root) else {
        return;
    };
    let observed_root = ProcessIdentity::new(root_identity.pid, root_process.start_time());
    if !automatic_target_allowed(
        root_identity,
        observed_root,
        &root_process.name().to_string_lossy(),
    ) {
        return;
    }

    let mut descendants = descendant_identities_filtered(&system, root, may_kill);
    descendants.reverse();
    descendants.push(root_identity);

    for identity in descendants {
        kill_identity_filtered_automatic(identity, may_kill);
    }
}

#[cfg(windows)]
fn kill_identity_filtered_automatic(identity: ProcessIdentity, may_kill: &dyn Fn(u32) -> bool) {
    if identity.start_time == UNKNOWN_START_TIME || !may_kill(identity.pid) {
        return;
    }

    let pid = Pid::from_u32(identity.pid);
    let mut current = System::new();
    current.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    let Some(process) = current.process(pid) else {
        return;
    };
    let observed = ProcessIdentity::new(identity.pid, process.start_time());
    if !automatic_target_allowed(identity, observed, &process.name().to_string_lossy()) {
        return;
    }

    let _ = process.kill_with(Signal::Kill);
    let _ = process.kill();
}

#[cfg(windows)]
fn is_console_host(process: &sysinfo::Process) -> bool {
    is_console_host_image(&process.name().to_string_lossy())
}

/// Parent → children index of a snapshot, keeping only links that can be real.
///
/// Every kill-path tree walk goes through this, never through a raw
/// `process.parent()` map (#1738). Windows never rewrites a process's parent
/// PID when the parent exits, and it recycles PIDs, so a process whose parent
/// died looks like a child of whatever later process is handed that PID. On a
/// GitHub runner some ancestor of the pytest process has a dead parent; when a
/// clud daemon was handed that PID, killing the daemon's tree walked *up* into
/// the runner and killed pytest. On a desktop the same walk reaches
/// `explorer.exe`, whose parent `userinit.exe` exits at logon.
///
/// A real child is never older than its parent, so a child that started before
/// the process now holding its parent PID is dropped. A child with an unknown
/// start time is dropped too: nothing proves the link. Start times have
/// one-second resolution, so a PID recycled within the second its orphan was
/// created can still slip through; the gate removes the long-lived-ancestor
/// case that killed the runner.
pub(crate) fn children_index(system: &System) -> HashMap<Pid, Vec<Pid>> {
    let rows = system.processes().iter().map(|(pid, process)| {
        (
            pid.as_u32(),
            process.parent().map(Pid::as_u32),
            process.start_time(),
        )
    });
    index_children(rows)
        .into_iter()
        .map(|(parent, children)| {
            (
                Pid::from_u32(parent),
                children.into_iter().map(Pid::from_u32).collect(),
            )
        })
        .collect()
}

/// Pure core of [`children_index`] over `(pid, parent_pid, start_time)` rows.
pub(crate) fn index_children<I>(rows: I) -> HashMap<u32, Vec<u32>>
where
    I: IntoIterator<Item = (u32, Option<u32>, u64)>,
{
    let rows: Vec<(u32, Option<u32>, u64)> = rows.into_iter().collect();
    let start_times: HashMap<u32, u64> = rows
        .iter()
        .map(|&(pid, _, start_time)| (pid, start_time))
        .collect();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(pid, parent, start_time) in &rows {
        let Some(parent) = parent.filter(|&parent| parent != pid) else {
            continue;
        };
        if start_time == crate::process_identity::UNKNOWN_START_TIME {
            continue;
        }
        if crate::process_scan::parent_is_plausible(start_times.get(&parent).copied(), start_time) {
            children.entry(parent).or_default().push(pid);
        }
    }
    children
}

/// Depth-first walk of `children` from `root`, pruning every subtree whose
/// root `admit` rejects. Each PID is visited once, so a cycle of same-second
/// parent links cannot loop forever.
fn walk_descendants(
    children: &HashMap<Pid, Vec<Pid>>,
    root: Pid,
    admit: &mut dyn FnMut(Pid) -> bool,
) -> Vec<Pid> {
    let mut seen = std::collections::HashSet::from([root]);
    let mut stack = vec![root];
    let mut descendants = Vec::new();
    while let Some(current) = stack.pop() {
        for &child in children.get(&current).map(Vec::as_slice).unwrap_or(&[]) {
            if !seen.insert(child) || !admit(child) {
                continue;
            }
            descendants.push(child);
            stack.push(child);
        }
    }
    descendants
}

#[cfg(windows)]
fn descendant_identities_filtered(
    system: &System,
    root: Pid,
    may_kill: &dyn Fn(u32) -> bool,
) -> Vec<ProcessIdentity> {
    let children = children_index(system);
    walk_descendants(&children, root, &mut |child| {
        system
            .process(child)
            .is_some_and(|process| may_kill(child.as_u32()) && !is_console_host(process))
    })
    .into_iter()
    .filter_map(|child| {
        system
            .process(child)
            .map(|process| ProcessIdentity::new(child.as_u32(), process.start_time()))
    })
    .collect()
}

/// Walk the parent-to-child graph from `root`, pruning any subtree whose root
/// `may_kill` rejects. Pruned, not just skipped: an exempt process keeps its
/// own descendants, so the walk never descends past it.
fn descendant_pids_filtered(
    system: &System,
    root: Pid,
    may_kill: &dyn Fn(u32) -> bool,
) -> Vec<Pid> {
    walk_descendants(&children_index(system), root, &mut |child| {
        may_kill(child.as_u32())
    })
}

/// Every descendant of `root` in `system`, through [`children_index`].
pub(crate) fn descendant_pids(system: &System, root: Pid) -> Vec<Pid> {
    walk_descendants(&children_index(system), root, &mut |_| true)
}

#[cfg(test)]
mod stale_parent_tests {
    use super::index_children;

    // (pid, parent, start_time)
    const RUNNER: u32 = 100;
    const PYTEST: u32 = 101;
    const RECYCLED: u32 = 50;

    #[test]
    fn child_older_than_the_pid_holder_is_not_its_child() {
        // The runner's real parent (PID 50) died long ago. A clud daemon
        // started at t=900 was handed PID 50.
        let rows = [
            (RUNNER, Some(RECYCLED), 100),
            (PYTEST, Some(RUNNER), 200),
            (RECYCLED, Some(PYTEST), 900),
        ];
        let children = index_children(rows);
        assert_eq!(children.get(&RECYCLED), None, "{children:?}");
        assert_eq!(children.get(&RUNNER), Some(&vec![PYTEST]));
        assert_eq!(children.get(&PYTEST), Some(&vec![RECYCLED]));
    }

    #[test]
    fn child_started_in_the_same_second_as_its_parent_is_kept() {
        let children = index_children([(10, None, 500), (11, Some(10), 500)]);
        assert_eq!(children.get(&10), Some(&vec![11]));
    }

    #[test]
    fn child_with_unknown_start_time_is_not_linked() {
        let children = index_children([(10, None, 500), (11, Some(10), 0)]);
        assert_eq!(children.get(&10), None);
    }

    #[test]
    fn child_of_a_dead_parent_is_not_linked() {
        let children = index_children([(11, Some(10), 500)]);
        assert!(children.is_empty(), "{children:?}");
    }

    #[test]
    fn self_parented_process_is_not_its_own_child() {
        let children = index_children([(4, Some(4), 500)]);
        assert!(children.is_empty(), "{children:?}");
    }
}

#[cfg(test)]
mod walk_tests {
    use super::{walk_descendants, Pid};
    use std::collections::HashMap;

    #[test]
    fn same_second_parent_cycle_terminates() {
        let mut children = HashMap::new();
        children.insert(Pid::from_u32(1), vec![Pid::from_u32(2)]);
        children.insert(Pid::from_u32(2), vec![Pid::from_u32(1)]);
        let out = walk_descendants(&children, Pid::from_u32(1), &mut |_| true);
        assert_eq!(out, vec![Pid::from_u32(2)]);
    }
}

/// Whether Ctrl+C teardown should start with a cooperative Ctrl+Break.
///
/// Native backend executables can receive this best-effort signal before the
/// hard kill. Windows batch wrappers cannot: their direct child is `cmd.exe`,
/// and Ctrl+Break makes cmd's batch interpreter print `Terminate batch job
/// (Y/N)?` and wait on stdin. For those wrappers we skip straight to
/// `kill_tree`, which still terminates the cmd.exe child and its descendants.
pub fn should_cooperative_break(direct_child_is_batch_wrapper: bool) -> bool {
    !direct_child_is_batch_wrapper
}

/// Best-effort cooperative shutdown of a Windows console process group.
///
/// When clud spawns the backend with `CREATE_NEW_PROCESS_GROUP`, the
/// child becomes the root of a new console process group identified by
/// its PID. Calling `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid)`
/// delivers a break signal to every process in that group, giving a
/// well-behaved agent (one that installs a `SetConsoleCtrlHandler` for
/// `CTRL_BREAK_EVENT`) a chance to flush state before clud follows up
/// with a hard `kill_tree`.
///
/// Returns `true` if the OS accepted the call (the group existed and the
/// event was queued), `false` otherwise. Failures are silent and
/// non-fatal: the caller is expected to fall through to `kill_tree` after
/// a short grace window regardless.
///
/// Do not call this when the direct child is a `cmd.exe` batch wrapper; see
/// [`should_cooperative_break`] for the user-visible prompt it avoids.
///
/// No-op on non-Windows targets — POSIX has no `CREATE_NEW_PROCESS_GROUP`
/// concept and clud's foreground process group already receives the
/// terminal's SIGINT directly.
pub fn try_break_group(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};
        // SAFETY: `GenerateConsoleCtrlEvent` is documented as safe to
        // call with any PID; passing a non-existent group simply returns
        // FALSE without dereferencing memory. The function signature in
        // `windows-rs` is `unsafe extern "system"`, which is the standard
        // marker for Win32 entry points — no Rust invariant is violated.
        let ok = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) };
        ok.is_ok()
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod filter_tests {
    use std::collections::HashMap;

    /// Build a fake parent→children graph and run the same prune walk
    /// `descendant_pids_filtered` performs, without touching real processes.
    fn walk(edges: &[(u32, u32)], root: u32, may_kill: &dyn Fn(u32) -> bool) -> Vec<u32> {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for (parent, child) in edges {
            children.entry(*parent).or_default().push(*child);
        }
        let mut stack = vec![root];
        let mut out = Vec::new();
        while let Some(current) = stack.pop() {
            if let Some(next) = children.get(&current) {
                for child in next {
                    if !may_kill(*child) {
                        continue;
                    }
                    out.push(*child);
                    stack.push(*child);
                }
            }
        }
        out.sort_unstable();
        out
    }

    // shell(10) → bash(11) → cargo(12) → zccache-daemon(13) → compiler(14)
    const EDGES: &[(u32, u32)] = &[(10, 11), (11, 12), (12, 13), (13, 14)];

    #[test]
    fn permissive_filter_reaps_the_whole_tree() {
        assert_eq!(walk(EDGES, 10, &|_| true), vec![11, 12, 13, 14]);
    }

    /// The daemon is spared AND so is the compiler child beneath it —
    /// sparing the daemon but killing its in-flight work would leave it
    /// wedged, which is worse than either extreme.
    #[test]
    fn exempt_process_prunes_its_entire_subtree() {
        let tagged = |pid: u32| pid != 13;
        assert_eq!(walk(EDGES, 10, &tagged), vec![11, 12]);
    }

    /// An exempt process deeper in the tree must not spare its ancestors —
    /// the leaked bash/cargo above it are still garbage.
    #[test]
    fn exemption_does_not_propagate_upward() {
        let out = walk(EDGES, 10, &|pid| pid != 14);
        assert!(out.contains(&13), "daemon's parent still reaped: {out:?}");
        assert!(!out.contains(&14));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn cooperative_break_skipped_for_batch_wrapper() {
        assert!(!super::should_cooperative_break(true));
    }

    #[test]
    fn cooperative_break_sent_for_native_executable() {
        assert!(super::should_cooperative_break(false));
    }

    #[test]
    fn automatic_identity_gate_requires_exact_nonzero_start_times() {
        let recorded = ProcessIdentity::new(41, 100);

        assert!(automatic_identity_matches(
            recorded,
            ProcessIdentity::new(41, 100)
        ));
        assert!(!automatic_identity_matches(
            recorded,
            ProcessIdentity::new(41, 101)
        ));
        assert!(!automatic_identity_matches(
            recorded,
            ProcessIdentity::new(41, UNKNOWN_START_TIME)
        ));
        assert!(!automatic_identity_matches(
            ProcessIdentity::new(41, UNKNOWN_START_TIME),
            ProcessIdentity::new(41, 100)
        ));
        assert!(!automatic_identity_matches(
            recorded,
            ProcessIdentity::new(42, 100)
        ));
    }

    #[test]
    fn automatic_target_gate_never_selects_console_host() {
        let recorded = ProcessIdentity::new(41, 100);
        let observed = ProcessIdentity::new(41, 100);

        assert!(automatic_target_allowed(recorded, observed, "git.exe"));
        assert!(!automatic_target_allowed(recorded, observed, "ConHost.EXE"));
    }

    #[test]
    fn automatic_target_refuses_openconsole_sidecar_host() {
        let recorded = ProcessIdentity::new(41, 100);
        let observed = ProcessIdentity::new(41, 100);

        assert!(automatic_target_allowed(recorded, observed, "node.exe"));
        assert!(!automatic_target_allowed(
            recorded,
            observed,
            "OpenConsole.exe"
        ));
        assert!(!automatic_target_allowed(
            recorded,
            observed,
            "openconsole.exe"
        ));
    }

    #[test]
    fn kill_tree_of_dead_pid_does_not_panic() {
        // A PID that almost certainly doesn't exist: u32::MAX. The helper
        // must return promptly without panicking — the whole point of the
        // "best-effort" contract is that nothing on the Ctrl+C path can
        // throw.
        let start = std::time::Instant::now();
        kill_tree(u32::MAX);
        // One `System::new_all()` snapshot dominates the wall clock; even
        // on slow CI we expect well under 2s.
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "kill_tree on dead pid took too long: {:?}",
            start.elapsed()
        );
    }

    #[cfg(windows)]
    #[test]
    fn kill_tree_terminates_real_descendant_on_windows() {
        // Spawn `cmd /c timeout 30`. That creates a child cmd.exe which
        // itself spawns timeout.exe — mirroring the real `clud → cmd.exe
        // → node.exe` tree shape. Then call `kill_tree` on the cmd.exe
        // PID and assert it dies within 5s.
        //
        // `std::process::Command` is exempt from the banned-imports rule
        // only inside tests in this module; production code paths must
        // still go through `running-process-core`.
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "timeout", "/t", "30", "/nobreak"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn cmd /c timeout");
        let pid = child.id();

        // Give cmd.exe a moment to spawn its timeout.exe grandchild.
        std::thread::sleep(Duration::from_millis(200));

        let start = std::time::Instant::now();
        kill_tree(pid);

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        panic!("cmd.exe survived kill_tree for >5s");
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("try_wait failed: {e}"),
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "kill_tree took too long: {:?}",
            start.elapsed()
        );
    }

    /// Start time of `pid` if it is alive, from a fresh minimal snapshot.
    #[cfg(windows)]
    fn live_start_time(pid: u32) -> Option<u64> {
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        system.process(Pid::from_u32(pid)).map(|p| p.start_time())
    }

    /// #1738: Windows never rewrites a process's parent PID when the parent
    /// exits, and it recycles PIDs. A process whose parent died therefore
    /// looks like a child of whatever later process is handed that PID. A tree
    /// kill rooted at the recycled PID must not take the older process with it.
    ///
    /// The CI shape that hit this: some ancestor of the pytest runner has a
    /// dead parent; a clud daemon is later handed that PID; the daemon's tree
    /// kill then walks *up* into the runner and kills pytest.
    #[cfg(windows)]
    #[test]
    fn kill_tree_spares_older_process_whose_parent_pid_was_recycled() {
        use std::process::{Command, Stdio};

        // `start /b` makes ping.exe a child of this cmd.exe, which then exits
        // and leaves ping.exe with a parent PID that names a dead process.
        let mut launcher = Command::new("cmd")
            .args(["/c", "start", "", "/b", "ping", "-n", "120", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd /c start /b ping");
        let stale_parent = launcher.id();
        launcher.wait().expect("wait for launcher cmd.exe");
        // Close our handle: Windows keeps a PID reserved while any handle to
        // the dead process is open.
        drop(launcher);

        let mut orphan = None;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while orphan.is_none() && std::time::Instant::now() < deadline {
            let mut system = System::new();
            system.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing(),
            );
            orphan = system.processes().iter().find_map(|(pid, process)| {
                (process.parent() == Some(Pid::from_u32(stale_parent))
                    && process.name().eq_ignore_ascii_case("ping.exe"))
                .then(|| ProcessIdentity::new(pid.as_u32(), process.start_time()))
            });
            if orphan.is_none() {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let orphan = orphan.expect("ping.exe launched by `start /b` never appeared");

        // Recycle the dead parent's PID onto a fresh process. Non-matching
        // candidates are released at once; Windows hands freed PIDs back out.
        let mut holder = None;
        let mut attempts = 0u32;
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            attempts += 1;
            let mut candidate = Command::new("ping")
                .args(["-n", "120", "127.0.0.1"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn recycling candidate");
            if candidate.id() == stale_parent {
                holder = Some(candidate);
                break;
            }
            let _ = candidate.kill();
            let _ = candidate.wait();
        }
        let Some(mut holder) = holder else {
            kill_tree(orphan.pid);
            eprintln!("inconclusive: PID {stale_parent} was not recycled after {attempts} spawns");
            return;
        };
        eprintln!("PID {stale_parent} recycled after {attempts} spawns");

        kill_tree(holder.id());
        std::thread::sleep(Duration::from_millis(500));
        let survived = live_start_time(orphan.pid) == Some(orphan.start_time);

        let _ = holder.kill();
        let _ = holder.wait();
        if survived {
            kill_tree(orphan.pid);
        }
        assert!(
            survived,
            "kill_tree({stale_parent}) killed PID {} (started {}), which predates the root \
             and only names it as parent because Windows recycled the PID",
            orphan.pid, orphan.start_time
        );
    }

    #[cfg(unix)]
    #[test]
    fn kill_tree_terminates_real_descendant_on_unix() {
        // Spawn `sh -c 'sleep 30'`. The shell is the parent of `sleep`,
        // so killing the tree must SIGKILL both. We check the sh process
        // is reaped within 5s.
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 30"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sh -c sleep 30");
        let pid = child.id();

        // Let sh spawn its sleep grandchild.
        std::thread::sleep(Duration::from_millis(200));

        let start = std::time::Instant::now();
        kill_tree(pid);

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        panic!("sh survived kill_tree for >5s");
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("try_wait failed: {e}"),
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "kill_tree took too long: {:?}",
            start.elapsed()
        );
    }
}
