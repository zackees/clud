//! Issue #340: detect and reap env-tagged orphaned descendants on clud exit.
//!
//! The `running-process` crate stamps every spawned descendant with
//! `RUNNING_PROCESS_ORIGINATOR=CLUD:<pid>` so we can find them later via
//! [`running_process::originator::find_processes_by_originator`]. This module
//! consumes that tag at the *end* of a foreground `clud` invocation: it walks
//! the descendant set whose `parent_pid` is *this* process, classifies each by
//! cmdline shape (so the report reads "vite preview port=5173" instead of a
//! 180-char path), prints a one-shot "surprise orphan" listing, then
//! terminates them via [`crate::process_tree::kill_tree`].
//!
//! Default-on. Suppress per-knob:
//!
//! - `--keep-orphans`: list, but do not kill
//! - `--quiet-orphans`: kill, but do not list
//! - `--explain-orphans`: include each descendant's relevant env vars
//!
//! Detached / handover paths (`--detach`, `--detachable`) skip the reaper
//! entirely — those descendants are intentionally outliving the foreground
//! `clud` per (6) in the issue. The caller is responsible for that gate.

use std::collections::{BTreeMap, HashSet};

use crate::process_identity::ProcessIdentity;
use crate::process_scan;
use crate::process_tree;
use crate::reaper_facts::{self, SpareList, SpareReason};

/// Caller-controlled knobs for the reaper. Filled in from the parsed CLI args
/// at the exit-hook site in `main.rs`.
#[derive(Debug, Clone, Default)]
pub struct ReapOpts {
    /// `--keep-orphans`: list them, do not kill.
    pub keep: bool,
    /// `--quiet-orphans`: kill silently.
    pub quiet: bool,
    /// `--explain-orphans`: dump each descendant's classification-relevant
    /// env vars beneath its row.
    pub explain: bool,
}

/// Result returned by [`scan_and_report`]. Useful for tests and for the
/// summary log line in `main.rs`.
#[derive(Debug, Clone, Default)]
pub struct ReapOutcome {
    /// Number of descendants found whose `parent_pid` is `self_pid`.
    pub found: usize,
    /// Number we actually terminated.
    pub reaped: usize,
    /// Candidate PIDs selected for this scan before `--keep-orphans` is applied.
    pub candidate_pids: Vec<u32>,
    /// PIDs passed to `kill_tree`.
    pub reaped_pids: Vec<u32>,
    /// Candidates an OS-authoritative signal protected, and **why** (#688).
    ///
    /// The reason is part of the outcome rather than a log-only detail because
    /// "the daemon survived" is not the property worth asserting — a reaper
    /// that spared it for the wrong reason (or by accident, having failed to
    /// see it at all) passes that test and still regresses. Sorted by PID so
    /// the report and any assertion over it are deterministic.
    pub spared: Vec<(u32, SpareReason)>,
}

/// One descendant's view, pre-classification.
struct Descendant {
    pid: u32,
    /// Creation time observed in the *same* snapshot that selected this PID.
    /// The kill path re-reads it and refuses to act if it moved (#673 Phase 6).
    start_time: u64,
    name: String,
    command: String,
}

impl Descendant {
    fn identity(&self) -> ProcessIdentity {
        ProcessIdentity::new(self.pid, self.start_time)
    }
}

impl From<process_scan::TaggedProcess> for Descendant {
    fn from(tagged: process_scan::TaggedProcess) -> Self {
        Self {
            pid: tagged.pid,
            start_time: tagged.start_time,
            name: tagged.name,
            command: tagged.command,
        }
    }
}

/// Coarse cmdline-shape label. The classifier table is the heart of the
/// "what is it likely?" answer; expand cases as we observe new leak shapes
/// in the wild.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Shape {
    VitePreview {
        port: Option<String>,
    },
    ViteDev {
        port: Option<String>,
    },
    OpenAiCodex,
    ClaudeCode,
    PyrightLangserver,
    Pm2,
    NpmRunPreview,
    NpmRunDev,
    NodejsWheelShim,
    /// Long-lived `powershell.exe` / `pwsh.exe` helper. Issue #360: Codex
    /// ships its command-safety layer as a `powershell.exe -NoProfile ...
    /// -EncodedCommand <multi-kilobyte-base64>` child, and without this
    /// case it falls into `Shape::Generic`, which surfaces the base64
    /// payload as the row label and sprays kilobytes into the terminal.
    PowerShell {
        mode: PowerShellMode,
    },
    /// Catch-all: `name + first-arg-basename`.
    Generic {
        label: String,
    },
}

/// Which `-…` flag PowerShell was started with. The label format
/// distinguishes "Codex's AST parser" (`-EncodedCommand`) from "someone
/// running `powershell.exe -File foo.ps1`" so a triager can tell at a
/// glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PowerShellMode {
    EncodedCommand,
    Command,
    File,
    /// Plain `powershell.exe`, `-NoExit` interactive, or anything we
    /// didn't classify.
    Other,
}

impl Shape {
    fn label(&self) -> String {
        match self {
            Shape::VitePreview { port } => match port {
                Some(p) => format!("vite preview port={p}"),
                None => "vite preview".to_string(),
            },
            Shape::ViteDev { port } => match port {
                Some(p) => format!("vite dev port={p}"),
                None => "vite dev".to_string(),
            },
            Shape::OpenAiCodex => "openai codex".to_string(),
            Shape::ClaudeCode => "claude-code".to_string(),
            Shape::PyrightLangserver => "pyright-langserver".to_string(),
            Shape::Pm2 => "pm2".to_string(),
            Shape::NpmRunPreview => "npm run preview".to_string(),
            Shape::NpmRunDev => "npm run dev".to_string(),
            Shape::NodejsWheelShim => "node (nodejs_wheel shim)".to_string(),
            Shape::PowerShell { mode } => match mode {
                PowerShellMode::EncodedCommand => "powershell (EncodedCommand)".to_string(),
                PowerShellMode::Command => "powershell (-Command)".to_string(),
                PowerShellMode::File => "powershell (-File)".to_string(),
                PowerShellMode::Other => "powershell".to_string(),
            },
            Shape::Generic { label } => label.clone(),
        }
    }
}

fn classify(name: &str, command: &str) -> Shape {
    let cmd_lc = command.to_ascii_lowercase();
    let name_lc = name.to_ascii_lowercase();

    let port = extract_port(command);

    if cmd_lc.contains("vite.js") && cmd_lc.contains("preview") {
        return Shape::VitePreview { port };
    }
    if cmd_lc.contains("vite.js") || cmd_lc.contains("\\vite\\bin") || cmd_lc.contains("/vite/bin")
    {
        return Shape::ViteDev { port };
    }
    if cmd_lc.contains("codex.js") {
        return Shape::OpenAiCodex;
    }
    if cmd_lc.contains("claude-code") || cmd_lc.contains("@anthropic-ai/claude-code") {
        return Shape::ClaudeCode;
    }
    if cmd_lc.contains("pyright-langserver") || cmd_lc.contains("pyright/dist/langserver") {
        return Shape::PyrightLangserver;
    }
    if cmd_lc.contains("pm2") {
        return Shape::Pm2;
    }
    if name_lc == "npm.exe" || name_lc == "npm" {
        if cmd_lc.contains(" preview") || cmd_lc.contains("\tpreview") {
            return Shape::NpmRunPreview;
        }
        if cmd_lc.contains(" dev") || cmd_lc.contains("\tdev") {
            return Shape::NpmRunDev;
        }
    }
    if cmd_lc.contains("nodejs_wheel") {
        return Shape::NodejsWheelShim;
    }
    if name_lc == "powershell.exe"
        || name_lc == "powershell"
        || name_lc == "pwsh.exe"
        || name_lc == "pwsh"
    {
        return Shape::PowerShell {
            mode: detect_powershell_mode(command),
        };
    }

    // Fallback: name + first non-flag arg basename, so users still get a
    // readable row instead of a raw path dump. Issue #360: bound the
    // basename length so a long argv token (e.g. a base64 blob that
    // didn't trigger any specific classifier above) can't spew kilobytes
    // through the print site below.
    let first_arg = command
        .split_whitespace()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .unwrap_or("");
    let basename = first_arg.rsplit(['/', '\\']).next().unwrap_or("");
    let basename = truncate_with_ellipsis(basename, GENERIC_BASENAME_MAX);
    let label = if basename.is_empty() {
        name.to_string()
    } else {
        format!("{name}  {basename}")
    };
    Shape::Generic { label }
}

/// Walk the PowerShell command line for the first recognized mode flag.
/// Matched case-insensitively because PowerShell accepts mixed-case (and
/// abbreviated) switches. The first hit wins; we don't second-guess the
/// shell's own ordering.
fn detect_powershell_mode(command: &str) -> PowerShellMode {
    for tok in command.split_whitespace() {
        let tok = tok.trim_start_matches(['-', '/']);
        let tok_lc = tok.to_ascii_lowercase();
        // `-EncodedCommand` and the common `-enc` abbreviation. We
        // recognize anything that's a unique prefix of "encodedcommand"
        // because PowerShell itself does too.
        if tok_lc == "encodedcommand" || tok_lc == "enc" || tok_lc == "encoded" {
            return PowerShellMode::EncodedCommand;
        }
        // `-Command` and the single-letter `-c`. Reject `-Co…` (Configuration etc.)
        // by requiring an exact match — PowerShell needs `-Command` spelled out
        // far enough to disambiguate, and Codex's helper would never use a
        // prefix shorter than `-c`.
        if tok_lc == "command" || tok_lc == "c" {
            return PowerShellMode::Command;
        }
        // `-File` / `-f`.
        if tok_lc == "file" || tok_lc == "f" {
            return PowerShellMode::File;
        }
    }
    PowerShellMode::Other
}

/// Max length for the basename portion of a `Shape::Generic` label.
/// Anything longer is almost certainly a base64 or path that shouldn't
/// be in the reaper's one-line summary anyway. Issue #360.
const GENERIC_BASENAME_MAX: usize = 40;

/// Hard cap on the label printed by `report_and_reap`'s row format.
/// Belt-and-suspenders against any future label longer than this — the
/// classifier-side cap above bounds Generic labels, but a future
/// `Shape` variant or a poorly-bounded label() implementation would
/// otherwise still be able to spew kilobytes through this single line.
const PRINTED_LABEL_MAX: usize = 60;

/// Truncate `s` to at most `max` bytes, appending `…` when truncated.
/// Walks back to the nearest char boundary so we never split a UTF-8
/// sequence in the middle.
fn truncate_with_ellipsis(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + 3);
    out.push_str(&s[..end]);
    out.push('…');
    out
}

/// Extract a `--port N` (or `--port=N`) value from a command line. Returns
/// `None` if not present or not numeric.
fn extract_port(command: &str) -> Option<String> {
    let mut tokens = command.split_whitespace();
    while let Some(tok) = tokens.next() {
        if let Some(eq_value) = tok.strip_prefix("--port=") {
            if !eq_value.is_empty() && eq_value.chars().all(|c| c.is_ascii_digit()) {
                return Some(eq_value.to_string());
            }
        }
        if tok == "--port" {
            if let Some(next) = tokens.next() {
                if !next.is_empty() && next.chars().all(|c| c.is_ascii_digit()) {
                    return Some(next.to_string());
                }
            }
        }
    }
    None
}

/// Scan, classify, report, and (unless `opts.keep`) reap orphans whose
/// originator-PID is `self_pid`.
///
/// This is the public entry point called from `main.rs` right before the
/// foreground process exits. Returns counts for the caller's summary log.
pub fn scan_and_report(self_pid: u32, opts: &ReapOpts) -> ReapOutcome {
    // One host environment pass answers both questions this needs — which
    // processes are tagged as ours, and which declared themselves daemons.
    // Asking them separately cost two full-host PEB walks (#673 Phase 7b).
    let scan = process_scan::scan_env("CLUD");

    // Only act on descendants whose originator points at *us*. Anything
    // pointing at a different CLUD:<pid> belongs to a concurrent clud
    // invocation and is not ours to touch.
    let mine: Vec<Descendant> = scan
        .tagged
        .into_iter()
        .filter(|p| p.parent_pid == self_pid)
        .map(Descendant::from)
        .collect();

    let header = format!("[clud] orphan scan on exit (originator=CLUD:{self_pid}):");
    report_and_reap(mine, &header, opts, &scan.declared_daemons)
}

/// Scan for *abandoned* CLUD-tagged descendants whose originator process is
/// no longer alive (dead PID, or PID reused by a later-started process —
/// `running_process` already guards both with a start-time check).
///
/// This is the broader sweep called by `clud slay`, by the daemon's
/// periodic heartbeat, and on `DaemonRequest::ReapOrphans`. Unlike
/// [`scan_and_report`], it does NOT restrict to descendants of the current
/// process — anything CLUD-tagged with a dead originator is fair game.
pub fn reap_orphans(opts: &ReapOpts) -> ReapOutcome {
    reap_orphans_filtered(opts, &mut |_| true)
}

/// How long a freshly observed orphan is left alone by the *periodic* sweep
/// (issue #465 rule 4, re-homed here in #614 when the unwired `orphan_sweep`
/// decision module was retired).
///
/// A clud that just exited may have its on-exit `Drop` guard mid-reap.
/// [`scan_and_report`] owns that cleanup; a periodic sweep racing it would
/// kill processes out from under the guard and confuse both sets of logs.
/// Waiting costs one tick.
///
/// Deliberately *not* applied to the explicit paths (`clud slay`, the on-exit
/// `ReapOrphans` request): there the caller has asked for a reap now, and the
/// race this guards against is precisely the caller itself.
pub const ORPHAN_GRACE_MS: u64 = 10_000;

/// [`reap_orphans`] with a caller-supplied admission test.
///
/// `admit` is invoked once per dead-originator candidate, in scan order, and
/// returning `false` spares that PID for this pass. It exists so the periodic
/// sweeper can apply [`ORPHAN_GRACE_MS`] without paying for a second
/// full-process environment scan (the scan reads `environ` for every process
/// on the host and is the expensive half of this function).
///
/// `admit` is called for *every* candidate even after one is rejected, so a
/// caller may use it to record which PIDs it observed this pass.
pub fn reap_orphans_filtered(opts: &ReapOpts, admit: &mut dyn FnMut(u32) -> bool) -> ReapOutcome {
    reap_orphans_filtered_sparing(opts, &std::collections::BTreeSet::new(), admit).0
}

/// [`reap_orphans_filtered`] that additionally **spares** any candidate whose
/// originator PID is in `spared_origins` (issue #465 handover registry: the
/// daemon passes the persisted set of prior-daemon PIDs that launched detached
/// sessions, so a successor daemon doesn't reap a live detached session across
/// a restart). This is **purely subtractive** — it can only remove candidates
/// from the reap set, never add — so a stale/over-broad set can at worst leave
/// an orphan un-reaped, never cause a wrong kill.
///
/// Also returns the set of originator PIDs observed on live CLUD-tagged
/// processes this pass, so the caller can prune stale registry entries without
/// paying for a second full-host environment scan.
pub fn reap_orphans_filtered_sparing(
    opts: &ReapOpts,
    spared_origins: &std::collections::BTreeSet<u32>,
    admit: &mut dyn FnMut(u32) -> bool,
) -> (ReapOutcome, std::collections::BTreeSet<u32>) {
    reap_orphans_from_scan(process_scan::scan_env("CLUD"), opts, spared_origins, admit)
}

/// [`reap_orphans_filtered_sparing`] over a scan the caller already has.
///
/// The daemon's periodic sweep shares one host environment pass with the proc
/// sampler via `process_scan::EnvScanCache` (#548), so it arrives here holding
/// a scan that may be up to one cache-tolerance window old. The client-exit
/// path ([`scan_and_report`]) has no sampler and keeps taking its own.
///
/// # Why a stale scan is safe here
///
/// Candidates selected from a stale scan cannot become wrong *kills*.
/// [`crate::process_tree::kill_tree_filtered_automatic`] takes its own fresh
/// topology snapshot at kill time and re-derives every target's
/// `(pid, start_time)` from it, requiring exact equality and rejecting
/// `UNKNOWN_START_TIME` — for the root and, since #688, for every descendant.
/// A recycled PID therefore fails the gate rather than dying in place of the
/// process that vacated the number.
///
/// What staleness *can* do is omit an orphan that appeared after the scan, or
/// carry one that has since exited. Both resolve on the next sweep, so the
/// failure mode is bounded latency, not a wrong action.
pub fn reap_orphans_from_scan(
    scan: process_scan::EnvScan,
    opts: &ReapOpts,
    spared_origins: &std::collections::BTreeSet<u32>,
    admit: &mut dyn FnMut(u32) -> bool,
) -> (ReapOutcome, std::collections::BTreeSet<u32>) {
    let observed_origins: std::collections::BTreeSet<u32> =
        scan.tagged.iter().map(|p| p.parent_pid).collect();
    let orphans: Vec<Descendant> = scan
        .tagged
        .into_iter()
        .filter(|p| !p.parent_alive)
        // Handover-registered originators are intentionally-detached sessions;
        // spare their whole cohort.
        .filter(|p| !spared_origins.contains(&p.parent_pid))
        // `filter` short-circuits nothing here: `admit` is invoked for each
        // candidate regardless of earlier results, which is what lets the
        // sweeper use it as an observation hook as well as a gate.
        .filter(|p| admit(p.pid))
        .map(Descendant::from)
        .collect();

    let header = "[clud] orphan sweep (dead originator):".to_string();
    (
        report_and_reap(orphans, &header, opts, &scan.declared_daemons),
        observed_origins,
    )
}

/// PIDs of every process that has **declared itself a daemon** by setting
/// `RUNNING_PROCESS_IS_DAEMON` (running-process #685/#686, released in
/// 4.6.3).
///
/// This is the reaper's spare-list, and it replaces the previous
/// tag-presence test (#522).
///
/// The old predicate killed a PID only when it carried a `CLUD:` originator
/// tag, which conflated two very different populations, because sparing
/// *prunes the whole subtree*:
///
/// - a genuine daemon, which strips the tag deliberately (#683) — correct
///   to spare;
/// - any descendant that merely lost the tag, because some link in the
///   spawn chain rebuilt its environment (a login shell, `env -i`, a
///   service wrapper, a setuid hop). Those are ordinary abandoned
///   processes, and sparing them also shielded everything beneath them.
///
/// Absence of a tag cannot distinguish the two: "did not declare itself"
/// and "had its environment clobbered" look identical. A positive marker
/// can, so the predicate now spares only what actually claims daemon-hood
/// and lets the tree walk reach env-stripped descendants.
///
/// Both callers now obtain this set from [`process_scan::scan_env`] in the same
/// pass that finds the tagged processes (#673 Phase 7b), so this exists only as
/// the standalone form of the query.
#[cfg(test)]
fn declared_daemon_pids() -> HashSet<u32> {
    process_scan::scan_env("CLUD").declared_daemons
}

/// May the kill proceed against the process now holding the recorded PID?
///
/// `observed` is what the process table reports for that PID *right now*;
/// `None` means the PID is gone. Pure so the recycled-PID case is testable
/// without persuading the OS to actually recycle a PID (#673 Phase 6).
///
/// A recorded identity with no creation time degrades to the PID-only
/// comparison clud used before start times existed. That is deliberate: it is
/// the same fallback [`ProcessIdentity::matches`] documents, and failing closed
/// instead would silently disable orphan reaping wholesale on any host whose
/// OS declines to report creation times, rather than on the one PID in question.
fn kill_target_is_current(recorded: ProcessIdentity, observed: Option<ProcessIdentity>) -> bool {
    match observed {
        Some(observed) => recorded.matches(&observed),
        None => false,
    }
}

/// Build the OS-authoritative spare-list for one sweep's candidate set.
///
/// #688: this reaper used to spare by `RUNNING_PROCESS_IS_DAEMON` alone. That
/// marker is **cooperative**, and the daemons whose loss actually hurts —
/// sccache, `FBuildWorker`, dockerd, language servers — never call
/// `running-process` at all, so they never set it. They do keep the inherited
/// `CLUD:<pid>` originator tag, which put them squarely in the candidate set
/// with nothing protecting them. The Windows tool-shell reaper got OS signals
/// in #673 Phase 1a; this one, with the wider blast radius, did not.
///
/// The candidate set is the already-selected tagged orphans, so the cost is
/// proportional to the sweep, not to the host.
fn spare_list_for(descendants: &[Descendant], daemons: &HashSet<u32>) -> SpareList {
    let pids: Vec<u32> = descendants.iter().map(|d| d.pid).collect();
    let facts = reaper_facts::collect_host_facts(
        &pids,
        daemons,
        reaper_facts::configured_spare_images_from_env(),
    );
    reaper_facts::build_spare_list(&facts, descendants.iter().map(|d| (d.pid, d.name.clone())))
}

/// May the tree walk terminate `pid`?
///
/// Consults *both* sets, for two different jobs. `spares` is the OS verdict
/// over this sweep's candidates. `daemons` covers the whole host, so it also
/// protects a declared daemon that is merely a *descendant* of a candidate and
/// was never a candidate itself — nothing collected facts about it.
///
/// A `false` **prunes** rather than skips (see
/// [`crate::process_tree::kill_tree_filtered`]): sparing a daemon while killing
/// its children would leave it wedged mid-work.
fn may_kill(spares: &SpareList, daemons: &HashSet<u32>, pid: u32) -> bool {
    !spares.contains_key(&pid) && !daemons.contains(&pid)
}

/// Shared classify / report / kill body for both entry points. Returns a
/// default outcome when `descendants` is empty so callers can skip noise.
///
/// `daemons` is the set of PIDs that declared themselves daemons. It is used
/// twice, for two different jobs: as one input to the candidate spare-list
/// (see [`spare_list_for`]), and — over the *whole host*, not just candidates —
/// to prune the tree walk, so a daemon survives even when it is a
/// PPID-descendant of something being reaped. Everything else in the tree is
/// reachable, including descendants whose environment was rebuilt and no
/// longer carries the originator tag (#522).
fn report_and_reap(
    descendants: Vec<Descendant>,
    header: &str,
    opts: &ReapOpts,
    daemons: &HashSet<u32>,
) -> ReapOutcome {
    let found = descendants.len();
    if found == 0 {
        return ReapOutcome::default();
    }
    let candidate_pids: Vec<u32> = descendants.iter().map(|d| d.pid).collect();
    let spares = spare_list_for(&descendants, daemons);
    let mut spared: Vec<(u32, SpareReason)> = spares.iter().map(|(pid, r)| (*pid, *r)).collect();
    spared.sort_unstable_by_key(|(pid, _)| *pid);

    // Group by shape label so the report collapses N identical leaks into
    // a single row with a list of PIDs/ports.
    let mut by_label: BTreeMap<String, Vec<&Descendant>> = BTreeMap::new();
    let classified: Vec<(Shape, &Descendant)> = descendants
        .iter()
        .map(|d| (classify(&d.name, &d.command), d))
        .collect();
    for (shape, d) in &classified {
        by_label.entry(shape.label()).or_default().push(*d);
    }

    if !opts.quiet {
        let action_word = if opts.keep {
            "(--keep-orphans set, leaving alive)"
        } else {
            "(reaping)"
        };
        eprintln!("{header} {found} env-tagged descendant(s) {action_word}");
        for (label, ds) in &by_label {
            let pids: Vec<String> = ds.iter().map(|d| d.pid.to_string()).collect();
            // Issue #360: `{label:<30}` is a min-width pad, not a max-width
            // truncate. A misclassified powershell `-EncodedCommand <base64>`
            // (or any future row whose label slipped past the classifier
            // bound) would otherwise spew kilobytes through this single
            // line. Cap defensively so the reaper's output stays one line
            // per row no matter what shape the cmdline took.
            let safe_label = truncate_with_ellipsis(label, PRINTED_LABEL_MAX);
            eprintln!(
                "         {count}x  {safe_label:<30}  pids=[{joined}]",
                count = ds.len(),
                safe_label = safe_label,
                joined = pids.join(", "),
            );
            if opts.explain {
                for d in ds {
                    let cmd_display = if d.command.len() > 140 {
                        format!("{}...", &d.command[..137])
                    } else {
                        d.command.clone()
                    };
                    eprintln!("            pid={:<7} {}", d.pid, cmd_display);
                }
            }
        }
        for (pid, reason) in &spared {
            eprintln!(
                "         sparing pid={pid} ({reason}) — OS signal says this is a daemon",
                reason = reason.as_str(),
            );
        }
    }

    if opts.keep {
        return ReapOutcome {
            found,
            reaped: 0,
            candidate_pids,
            reaped_pids: Vec::new(),
            spared,
        };
    }

    // #673 Phase 8: one host topology walk for the whole sweep. This used to be
    // a fresh full-host walk *per orphan* — a 20-orphan sweep walked the host
    // process table 20 times to answer 20 questions it could answer from one —
    // plus one targeted refresh per orphan for the identity check.
    let topology = process_tree::TopologySnapshot::capture();
    let mut reaped = 0usize;
    let mut reaped_pids = Vec::with_capacity(descendants.len());
    let mut skipped_recycled = 0usize;
    let admit = |pid: u32| may_kill(&spares, daemons, pid);
    for d in &descendants {
        if spares.contains_key(&d.pid) {
            continue;
        }
        // #673 Phase 6: the scan that selected this PID and the kill that acts
        // on it are separated by the rest of the scan, the classification, and
        // the report. A PID can die and be recycled in that window, and killing
        // a *tree* means the replacement's children go with it. Re-read the
        // creation time and refuse if it moved. The check reads from the same
        // snapshot the kill acts on, so selection and termination cannot
        // disagree about which process holds the number.
        if !kill_target_is_current(d.identity(), topology.identity(d.pid)) {
            skipped_recycled += 1;
            continue;
        }
        topology.kill_tree_filtered(d.pid, &admit);
        reaped += 1;
        reaped_pids.push(d.pid);
    }

    if !opts.quiet {
        eprintln!("[clud] reaped {reaped} of {found} env-tagged descendant(s)");
        if !spared.is_empty() {
            eprintln!(
                "[clud] spared {} descendant(s) protected by an OS daemon signal",
                spared.len()
            );
        }
        if skipped_recycled > 0 {
            eprintln!(
                "[clud] skipped {skipped_recycled} descendant(s) whose PID was \
                 recycled or exited between scan and reap"
            );
        }
    }

    ReapOutcome {
        found,
        reaped,
        candidate_pids,
        reaped_pids,
        spared,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_identity::UNKNOWN_START_TIME;

    /// The production predicate, with an empty OS spare-list — the #522 half
    /// of the rule on its own.
    fn marker_only(daemons: &HashSet<u32>, pid: u32) -> bool {
        may_kill(&SpareList::new(), daemons, pid)
    }

    /// A declared daemon is spared. This is what keeps zccache/soldr/fbuild
    /// alive across a clud exit, and it is why the marker exists.
    #[test]
    fn declared_daemon_is_spared() {
        let daemons: HashSet<u32> = [4242].into_iter().collect();
        assert!(!marker_only(&daemons, 4242));
    }

    /// The regression #522 actually reports: a descendant whose environment
    /// was rebuilt somewhere in the spawn chain carries no originator tag,
    /// yet is an ordinary abandoned process. The old predicate spared it --
    /// and, because sparing prunes, everything beneath it too.
    #[test]
    fn untagged_non_daemon_is_reapable() {
        let daemons: HashSet<u32> = [4242].into_iter().collect();
        assert!(
            marker_only(&daemons, 9001),
            "an env-stripped descendant that never declared itself a daemon \
             must stay reachable (regression of #522)"
        );
    }

    /// Absence of evidence is not evidence of daemon-hood. With no marker
    /// anywhere, nothing is spared -- the previous behavior spared
    /// everything untagged, which is the inversion this fixes.
    #[test]
    fn empty_daemon_set_spares_nothing() {
        let daemons: HashSet<u32> = HashSet::new();
        assert!(marker_only(&daemons, 1));
        assert!(marker_only(&daemons, u32::MAX));
    }

    // ---- #688: the OS-signal spare-list reaches *this* reaper too ----

    /// Build the spare-list the way `report_and_reap` does, but from injected
    /// facts. Tier 1 (#674): a reap decision must be a pure function of
    /// `ProcessFacts`, assertable on every platform without spawning anything.
    fn spares_from(facts: &reaper_facts::FactsSnapshot, candidates: &[(u32, &str)]) -> SpareList {
        reaper_facts::build_spare_list(
            facts,
            candidates
                .iter()
                .map(|(pid, image)| (*pid, (*image).to_string())),
        )
    }

    /// The exact shape #688 reports: sccache-class daemons never call
    /// `running-process`, so they keep the inherited `CLUD:<pid>` tag and never
    /// set the marker. Before this fix nothing in *this* reaper protected them,
    /// and `clud slay` / the on-exit scan / the daemon sweep all killed them.
    ///
    /// Asserted on the **reason**, not merely on survival: a reaper that spared
    /// it by accident — or that never saw it at all — passes a survival-only
    /// test and still regresses.
    #[test]
    fn a_listening_undeclared_orphan_is_spared_with_a_reason() {
        let mut facts = reaper_facts::FactsSnapshot::default();
        facts.listening.insert(5150);
        let spares = spares_from(&facts, &[(5150, "sccache.exe")]);

        assert_eq!(spares.get(&5150), Some(&SpareReason::ListeningEndpoint));
        assert!(!may_kill(&spares, &HashSet::new(), 5150));
    }

    /// The counterweight, and the reason the signal table is ranked rather than
    /// permissive: a genuinely leaked orphan with no daemon signal at all is
    /// still reaped. Over-sparing would silently turn the reaper off.
    #[test]
    fn a_leaked_orphan_with_no_daemon_signal_is_still_reaped() {
        let facts = reaper_facts::FactsSnapshot::default();
        let spares = spares_from(&facts, &[(9001, "node.exe")]);

        assert!(spares.is_empty(), "nothing here deserves protection");
        assert!(may_kill(&spares, &HashSet::new(), 9001));
    }

    /// Sparing prunes. A spared daemon's children are its in-flight work, not
    /// leaked garbage, so the walk must stop at it rather than step over it.
    #[test]
    fn a_spared_daemon_prunes_its_subtree() {
        let mut facts = reaper_facts::FactsSnapshot::default();
        facts.session_leaders.insert(13);
        let spares = spares_from(&facts, &[(13, "sccache"), (14, "cc1plus")]);

        assert_eq!(spares.get(&13), Some(&SpareReason::SessionLeader));
        // The compiler child is not itself protected...
        assert!(may_kill(&spares, &HashSet::new(), 14));
        // ...but the walk never reaches it, because it prunes at 13. That is
        // `process_tree`'s contract, covered by its own `filter_tests`.
    }

    /// A declared daemon that is only a *descendant* of a candidate was never
    /// in the candidate set, so no facts were collected about it. The host-wide
    /// marker set is what keeps it alive.
    #[test]
    fn a_host_declared_daemon_is_spared_even_when_it_is_not_a_candidate() {
        let spares = SpareList::new();
        let daemons: HashSet<u32> = [777].into_iter().collect();
        assert!(!may_kill(&spares, &daemons, 777));
    }

    /// An unavailable signal must not spare. Absence of evidence is not
    /// evidence of daemon-hood — the exact inversion #522 fixed, re-asserted
    /// here because the fix added a second, platform-varying source of `None`.
    #[test]
    fn a_signal_this_platform_cannot_evaluate_never_spares() {
        let mut facts = reaper_facts::FactsSnapshot::default();
        facts.listening.insert(31);
        facts
            .unavailable
            .insert(reaper_facts::Signal::ListeningEndpoint);
        let spares = spares_from(&facts, &[(31, "sccache")]);

        assert!(spares.is_empty());
        assert!(may_kill(&spares, &HashSet::new(), 31));
    }

    /// The producer this reaper actually calls must never claim job membership
    /// it cannot observe: there is no Job Object on the `clud slay` / on-exit /
    /// daemon-sweep path, and answering "inside the job" would read as a
    /// positive finding of containment.
    #[test]
    fn the_cross_platform_producer_reports_no_job_object() {
        let facts =
            reaper_facts::collect_host_facts(&[std::process::id()], &HashSet::new(), Vec::new());
        assert!(facts
            .unavailable
            .contains(&reaper_facts::Signal::JobMembership));
    }

    /// The query must not blow up when nothing has declared itself; a scan
    /// on a machine with no daemons is the common case.
    #[test]
    fn declared_daemon_pids_is_callable() {
        let _ = declared_daemon_pids();
    }

    // ---- #673 Phase 6: the kill path re-verifies identity ----

    /// The bug this closes: between the scan that selected a target and the
    /// kill that acts, the PID died and was handed to something else. Killing
    /// it would take out the replacement *and its whole subtree*.
    #[test]
    fn a_recycled_pid_is_never_killed() {
        let recorded = ProcessIdentity::new(4321, 1_700_000_000);
        let replacement = ProcessIdentity::new(4321, 1_700_000_042);
        assert!(!kill_target_is_current(recorded, Some(replacement)));
    }

    /// The ordinary case: same PID, same creation time, kill proceeds.
    #[test]
    fn an_unchanged_target_is_still_killed() {
        let recorded = ProcessIdentity::new(4321, 1_700_000_000);
        assert!(kill_target_is_current(recorded, Some(recorded)));
    }

    /// A target that simply exited before the kill is skipped rather than
    /// having its number acted on.
    #[test]
    fn a_target_that_vanished_is_skipped() {
        assert!(!kill_target_is_current(
            ProcessIdentity::new(4321, 1_700_000_000),
            None
        ));
    }

    /// Without a recorded creation time there is nothing to compare, and
    /// refusing would disable reaping wholesale rather than for one PID. This
    /// is the documented `ProcessIdentity::matches` fallback, asserted here so
    /// the choice is visible rather than incidental.
    #[test]
    fn an_unrecorded_creation_time_falls_back_to_pid_only() {
        let recorded = ProcessIdentity::new(4321, UNKNOWN_START_TIME);
        assert!(kill_target_is_current(
            recorded,
            Some(ProcessIdentity::new(4321, 1_700_000_000))
        ));
        assert!(!kill_target_is_current(
            recorded,
            Some(ProcessIdentity::new(9999, 1_700_000_000))
        ));
    }

    /// Descendants carry the creation time observed by the selecting scan, so
    /// the kill path has something to re-verify against.
    #[test]
    fn descendant_identity_pairs_pid_with_observed_creation_time() {
        let tagged = process_scan::TaggedProcess {
            pid: 4321,
            start_time: 1_700_000_000,
            name: "node.exe".into(),
            command: "node vite.js".into(),
            originator: "CLUD:1".into(),
            parent_pid: 1,
            parent_alive: false,
        };
        let descendant = Descendant::from(tagged);
        assert_eq!(descendant.identity().pid, 4321);
        assert_eq!(descendant.identity().start_time, 1_700_000_000);
    }

    #[test]
    fn classify_vite_preview_with_port() {
        let cmd =
            "node C:\\Users\\x\\node_modules\\.bin\\..\\vite\\bin\\vite.js preview --port 8081";
        assert_eq!(
            classify("node.exe", cmd),
            Shape::VitePreview {
                port: Some("8081".to_string())
            }
        );
    }

    #[test]
    fn classify_vite_preview_without_port() {
        let cmd = "node vite.js preview";
        assert_eq!(classify("node.exe", cmd), Shape::VitePreview { port: None });
    }

    #[test]
    fn classify_vite_dev() {
        let cmd = "node /usr/local/lib/node_modules/.bin/../vite/bin/vite.js --port=5173";
        assert_eq!(
            classify("node", cmd),
            Shape::ViteDev {
                port: Some("5173".to_string())
            }
        );
    }

    #[test]
    fn classify_codex() {
        let cmd = "node /home/user/.npm/lib/codex.js --bypass";
        assert_eq!(classify("node", cmd), Shape::OpenAiCodex);
    }

    #[test]
    fn classify_claude_code_cli() {
        let cmd =
            "C:\\npm\\node_modules\\@anthropic-ai\\claude-code\\bin\\claude.exe --dangerously-skip-permissions";
        assert_eq!(classify("claude.exe", cmd), Shape::ClaudeCode);
    }

    #[test]
    fn classify_pyright_langserver_python_shim() {
        let cmd = "python.exe Scripts\\pyright-langserver.exe --stdio";
        assert_eq!(classify("python.exe", cmd), Shape::PyrightLangserver);
    }

    #[test]
    fn classify_pyright_langserver_node_side() {
        let cmd = "node pyright/dist/langserver.index.js -- --stdio";
        assert_eq!(classify("node", cmd), Shape::PyrightLangserver);
    }

    #[test]
    fn classify_pm2() {
        let cmd = "node pm2-beta/lib/Daemon.js";
        assert_eq!(classify("node", cmd), Shape::Pm2);
    }

    #[test]
    fn classify_npm_run_preview() {
        let cmd = "C:\\Scripts\\npm.exe run preview -- --port 9090";
        assert_eq!(classify("npm.exe", cmd), Shape::NpmRunPreview);
    }

    #[test]
    fn classify_npm_run_dev() {
        let cmd = "C:\\Scripts\\npm.exe run dev";
        assert_eq!(classify("npm.exe", cmd), Shape::NpmRunDev);
    }

    #[test]
    fn classify_nodejs_wheel_shim() {
        let cmd = "C:\\tools\\python13\\Lib\\site-packages\\nodejs_wheel\\node.exe some-script.js";
        assert_eq!(classify("node.exe", cmd), Shape::NodejsWheelShim);
    }

    #[test]
    fn classify_generic_falls_back_to_name_and_basename() {
        let cmd = "/usr/bin/some-thing /home/u/work/script.sh --arg1";
        match classify("some-thing", cmd) {
            Shape::Generic { label } => assert!(
                label.contains("some-thing") && label.contains("script.sh"),
                "got {label:?}"
            ),
            other => panic!("expected Generic, got {other:?}"),
        }
    }

    /// Issue #360: Codex's command-safety AST parser runs as
    /// `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy
    /// Bypass -EncodedCommand <multi-KB-base64>`. The classifier must
    /// recognize it as PowerShell-EncodedCommand so the printed label
    /// stays short instead of falling into `Shape::Generic` and
    /// surfacing the base64 payload as the row label.
    #[test]
    fn classify_powershell_encoded_command_codex_shape() {
        let cmd = "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass \
                   -EncodedCommand JABFAHIAcgBvAHIAQQBjAHQAaQBvAG4AUAByAGUAZgBlAHIAZQBuAGMAZQA=";
        assert_eq!(
            classify("powershell.exe", cmd),
            Shape::PowerShell {
                mode: PowerShellMode::EncodedCommand
            }
        );
    }

    #[test]
    fn classify_powershell_command_flag() {
        let cmd = "powershell.exe -NoProfile -Command Get-Process";
        assert_eq!(
            classify("powershell.exe", cmd),
            Shape::PowerShell {
                mode: PowerShellMode::Command
            }
        );
    }

    #[test]
    fn classify_powershell_file_flag() {
        let cmd = "powershell.exe -NoProfile -File C:\\scripts\\foo.ps1";
        assert_eq!(
            classify("powershell.exe", cmd),
            Shape::PowerShell {
                mode: PowerShellMode::File
            }
        );
    }

    #[test]
    fn classify_powershell_bare_other() {
        // Plain `powershell.exe` with no recognized mode flag still
        // gets classified as PowerShell — not Generic — so the label
        // stays short.
        let cmd = "powershell.exe -NoExit";
        assert_eq!(
            classify("powershell.exe", cmd),
            Shape::PowerShell {
                mode: PowerShellMode::Other
            }
        );
    }

    #[test]
    fn classify_pwsh_core_recognized() {
        // PowerShell 7+ ships as `pwsh.exe`; same classifier path.
        let cmd = "pwsh.exe -NoProfile -EncodedCommand JABF";
        assert_eq!(
            classify("pwsh.exe", cmd),
            Shape::PowerShell {
                mode: PowerShellMode::EncodedCommand
            }
        );
    }

    #[test]
    fn classify_powershell_case_insensitive_flag() {
        // PowerShell accepts mixed case on switches; the classifier
        // must too. Codex emits `-EncodedCommand`, ad-hoc scripts often
        // use `-encodedcommand` or `-EnC`.
        let cmd = "powershell.exe -noprofile -encodedcommand AAAA";
        assert_eq!(
            classify("powershell.exe", cmd),
            Shape::PowerShell {
                mode: PowerShellMode::EncodedCommand
            }
        );
    }

    #[test]
    fn powershell_label_is_short_and_distinguishes_modes() {
        // Each mode produces a distinct, single-line label. The
        // critical guarantee for #360 is that no mode can produce a
        // multi-kilobyte label — the format is hard-coded.
        assert_eq!(
            Shape::PowerShell {
                mode: PowerShellMode::EncodedCommand
            }
            .label(),
            "powershell (EncodedCommand)"
        );
        assert_eq!(
            Shape::PowerShell {
                mode: PowerShellMode::Command
            }
            .label(),
            "powershell (-Command)"
        );
        assert_eq!(
            Shape::PowerShell {
                mode: PowerShellMode::File
            }
            .label(),
            "powershell (-File)"
        );
        assert_eq!(
            Shape::PowerShell {
                mode: PowerShellMode::Other
            }
            .label(),
            "powershell"
        );
        // Sanity: every mode's label stays well under the print-site
        // cap, so PowerShell rows never get ellipsized.
        for mode in [
            PowerShellMode::EncodedCommand,
            PowerShellMode::Command,
            PowerShellMode::File,
            PowerShellMode::Other,
        ] {
            let label = Shape::PowerShell { mode }.label();
            assert!(
                label.len() < PRINTED_LABEL_MAX,
                "label {label:?} ({} bytes) must stay under PRINTED_LABEL_MAX = {PRINTED_LABEL_MAX}",
                label.len()
            );
        }
    }

    /// Issue #360 defense-in-depth: even if a future classifier path
    /// produces a Generic label longer than the basename cap (e.g.
    /// from a renamed argv format), the truncation helper guarantees
    /// the printed row stays one line.
    #[test]
    fn truncate_with_ellipsis_caps_long_input_and_adds_ellipsis() {
        let blob = "a".repeat(500);
        let out = truncate_with_ellipsis(&blob, 40);
        // Truncated bytes + the 3-byte ellipsis (UTF-8) = 43 bytes max.
        assert!(out.len() <= 40 + "…".len());
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncate_with_ellipsis_short_input_unchanged() {
        // No truncation, no ellipsis appended.
        let out = truncate_with_ellipsis("hello", 40);
        assert_eq!(out, "hello");
    }

    #[test]
    fn truncate_with_ellipsis_at_exact_boundary_unchanged() {
        // Boundary case: input length == max. No truncation expected.
        let s = "a".repeat(40);
        let out = truncate_with_ellipsis(&s, 40);
        assert_eq!(out, s);
    }

    #[test]
    fn truncate_with_ellipsis_respects_utf8_boundaries() {
        // Truncating mid-multibyte-char would panic on the slice
        // operation. Walk-back-to-boundary must save us — and the
        // resulting string must still be valid UTF-8.
        let s = "中文测试字符串";
        let out = truncate_with_ellipsis(s, 5);
        // Round-trip through UTF-8 must succeed; the only way this
        // fails is if we sliced through a multibyte sequence.
        assert!(out.is_char_boundary(out.len()));
        assert!(out.ends_with('…'));
    }

    /// Issue #360 root case: a Generic basename longer than
    /// `GENERIC_BASENAME_MAX` (e.g. a base64 token with no path
    /// separators) must be truncated by the classifier so the printed
    /// label stays short even before the print-site cap kicks in.
    #[test]
    fn classify_generic_truncates_long_basename() {
        let blob = "A".repeat(500);
        let cmd = format!("/usr/bin/some-tool {blob}");
        match classify("some-tool", &cmd) {
            Shape::Generic { label } => {
                assert!(
                    label.len() < 100,
                    "label should be truncated, got {} bytes: {label:?}",
                    label.len()
                );
                assert!(
                    label.ends_with('…'),
                    "expected trailing ellipsis, got {label:?}"
                );
            }
            other => panic!("expected Generic, got {other:?}"),
        }
    }

    #[test]
    fn detect_powershell_mode_recognizes_short_aliases() {
        // PowerShell accepts `-enc` and `-c` abbreviations; the
        // detector should too.
        assert_eq!(
            detect_powershell_mode("powershell.exe -enc AAAA"),
            PowerShellMode::EncodedCommand
        );
        assert_eq!(
            detect_powershell_mode("powershell.exe -c Get-Process"),
            PowerShellMode::Command
        );
        assert_eq!(
            detect_powershell_mode("powershell.exe -f foo.ps1"),
            PowerShellMode::File
        );
    }

    #[test]
    fn detect_powershell_mode_other_when_no_recognized_flag() {
        // Plain `powershell.exe` and unrecognized flags both fall
        // through to Other rather than being misclassified as a
        // specific mode.
        assert_eq!(
            detect_powershell_mode("powershell.exe"),
            PowerShellMode::Other
        );
        assert_eq!(
            detect_powershell_mode("powershell.exe -NoProfile -NoExit"),
            PowerShellMode::Other
        );
    }

    #[test]
    fn extract_port_space_form() {
        assert_eq!(
            extract_port("vite preview --port 8081 --host"),
            Some("8081".to_string())
        );
    }

    #[test]
    fn extract_port_equals_form() {
        assert_eq!(extract_port("vite --port=5173"), Some("5173".to_string()));
    }

    #[test]
    fn extract_port_none() {
        assert_eq!(extract_port("vite preview"), None);
    }

    #[test]
    fn extract_port_non_numeric_ignored() {
        assert_eq!(extract_port("vite --port=abc"), None);
    }

    #[test]
    fn shape_label_includes_port_when_present() {
        assert_eq!(
            Shape::VitePreview {
                port: Some("8082".into())
            }
            .label(),
            "vite preview port=8082"
        );
        assert_eq!(Shape::VitePreview { port: None }.label(), "vite preview");
    }

    #[test]
    fn scan_finds_nothing_when_no_descendants_match_self_pid() {
        // Pick a PID that is overwhelmingly unlikely to be a real CLUD originator
        // on the test host. Even if a real clud session were running, its
        // originator is its own PID, which is not u32::MAX. The scan returns
        // a default outcome (0/0) without touching anything.
        let outcome = scan_and_report(
            u32::MAX,
            &ReapOpts {
                keep: true,
                quiet: true,
                explain: false,
            },
        );
        assert_eq!(outcome.found, 0);
        assert_eq!(outcome.reaped, 0);
    }

    #[test]
    fn reap_orphans_in_keep_mode_does_not_kill() {
        // `keep: true` means: list candidates but never invoke kill_tree. The
        // test host may or may not have CLUD-tagged descendants with a dead
        // originator, so we only assert that `reaped == 0` (never kill) and
        // that `found >= reaped`. This guards against regressions where the
        // shared report_and_reap path stops honoring `keep`.
        let outcome = reap_orphans(&ReapOpts {
            keep: true,
            quiet: true,
            explain: false,
        });
        assert_eq!(outcome.reaped, 0);
        assert!(outcome.found >= outcome.reaped);
    }
}
