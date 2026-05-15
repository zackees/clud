//! Startup-time nudge: warn when the project contains source files large
//! enough to cause AI agents to get stuck. See zackees/clud#132.
//!
//! On launch we walk the project's git root, honoring `.gitignore`,
//! `.ignore`, hidden-file rules, and global git excludes via the `ignore`
//! crate's parallel walker. Files whose size is ≥ [`SIZE_THRESHOLD`] and
//! whose extension is on a small whitelist of source-code languages are
//! reported to stderr — top [`REPORT_LIMIT`] by size, with a `(N more)`
//! tail when more qualify.
//!
//! The walk hard-stops at [`DEADLINE`] (1 s wall) regardless of progress
//! so that startup is never blocked by a pathological repo: if the
//! deadline trips, stderr reports a partial-scan note instead of stalling.

use ignore::{DirEntry, WalkBuilder, WalkState};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 40 kB ≈ 1000 LOC at clud's measured ~37 bytes/line (see issue #132).
pub const SIZE_THRESHOLD: u64 = 40 * 1024;

/// Hard wall-clock deadline for the entire walk. Per user requirement
/// the check stops at 1 s even if files remain unvisited.
pub const DEADLINE: Duration = Duration::from_secs(1);

/// Top-N largest files reported.
pub const REPORT_LIMIT: usize = 4;

/// Source-file extensions worth checking (no leading dot).
const SOURCE_EXTS: &[&str] = &[
    "rs", "py", "js", "jsx", "ts", "tsx", "mjs", "cjs", "go", "c", "cc", "cpp", "cxx", "h", "hpp",
    "hxx", "java", "kt", "scala", "rb", "swift", "cs", "php", "lua", "m", "mm", "r", "jl", "ex",
    "exs", "erl", "clj", "cljs", "ml", "fs", "vb", "dart", "sh", "ps1",
];

/// Conventional directory names that hold third-party / vendored source we
/// never want to flag. Most are also gitignored in practice, but some repos
/// commit them (cargo vendor, go vendor, git-subrepo trees), so pruning by
/// name is a belt-and-suspenders complement to gitignore + nested-`.git`.
const VENDOR_DIRS: &[&str] = &[
    "vendor",
    "third_party",
    "third-party",
    "external",
    "deps",
    "subprojects",
    "node_modules",
];

/// A single file that crossed the size threshold.
#[derive(Debug, Clone)]
pub struct LargeFile {
    pub rel_path: PathBuf,
    pub size: u64,
}

/// Run the startup check from `project_root`. Silent on non-git roots and
/// when no files qualify. Errors are swallowed — a failed scan must never
/// block a launch.
pub fn run(project_root: &Path) {
    // Silent if not in a git repo (the warning is project-scoped).
    if !project_root.join(".git").exists() {
        return;
    }

    let report = collect(project_root, DEADLINE);
    emit(&report.files, report.total_qualifying, report.timed_out);
}

struct Report {
    files: Vec<LargeFile>,
    total_qualifying: usize,
    timed_out: bool,
}

fn collect(root: &Path, deadline: Duration) -> Report {
    let start = Instant::now();
    let stop = AtomicBool::new(false);
    let hits: Mutex<Vec<LargeFile>> = Mutex::new(Vec::new());

    let walker = WalkBuilder::new(root)
        .standard_filters(true) // .gitignore + .ignore + hidden
        .same_file_system(true)
        .filter_entry({
            let root = root.to_path_buf();
            move |e| !is_pruned_nested_git(&root, e)
        })
        .build_parallel();

    walker.run(|| {
        let stop = &stop;
        let hits = &hits;
        let root = root.to_path_buf();
        Box::new(move |result| {
            if start.elapsed() >= deadline {
                stop.store(true, Ordering::Relaxed);
                return WalkState::Quit;
            }
            if stop.load(Ordering::Relaxed) {
                return WalkState::Quit;
            }
            let Ok(entry) = result else {
                return WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }
            let path = entry.path();
            if !is_whitelisted_source(path) {
                return WalkState::Continue;
            }
            let Ok(md) = entry.metadata() else {
                return WalkState::Continue;
            };
            if md.len() < SIZE_THRESHOLD {
                return WalkState::Continue;
            }
            let rel = path.strip_prefix(&root).unwrap_or(path).to_path_buf();
            hits.lock().unwrap().push(LargeFile {
                rel_path: rel,
                size: md.len(),
            });
            WalkState::Continue
        })
    });

    let timed_out = start.elapsed() >= deadline;
    let mut files = hits.into_inner().unwrap();
    files.sort_by(|a, b| b.size.cmp(&a.size));
    let total = files.len();
    files.truncate(REPORT_LIMIT);
    Report {
        files,
        total_qualifying: total,
        timed_out,
    }
}

fn is_whitelisted_source(path: &Path) -> bool {
    // Exclude *.min.* bundles (multi-component check, not a single extension).
    let name = match path.file_name().and_then(|s| s.to_str()) {
        Some(s) => s,
        None => return false,
    };
    if name.contains(".min.") {
        return false;
    }
    let ext = match path.extension().and_then(|s| s.to_str()) {
        Some(e) => e.to_ascii_lowercase(),
        None => return false,
    };
    SOURCE_EXTS.contains(&ext.as_str())
}

fn is_pruned_nested_git(root: &Path, e: &DirEntry) -> bool {
    if !e.file_type().is_some_and(|t| t.is_dir()) {
        return false;
    }
    if e.path() == root {
        return false;
    }
    // Pruned if the directory has its own `.git` (vendored sub-repo).
    if e.path().join(".git").exists() {
        return true;
    }
    // Pruned if the directory's basename is a conventional vendor / deps
    // directory. Repos that commit `cargo vendor` / `go vendor` / similar
    // trees would otherwise dominate the report with third-party LOC.
    if let Some(name) = e.path().file_name().and_then(|s| s.to_str()) {
        if VENDOR_DIRS.contains(&name) {
            return true;
        }
    }
    false
}

fn emit(files: &[LargeFile], total: usize, timed_out: bool) {
    if files.is_empty() {
        if timed_out {
            eprintln!("[clud] note: large-file scan exceeded 1s budget — skipping");
        }
        return;
    }
    eprintln!(
        "[clud] warning: large source files (\u{2265}40 kB) detected — AI may get stuck on these; recommend refactoring:"
    );
    for f in files {
        eprintln!("  {} ({})", f.rel_path.display(), human_kb(f.size));
    }
    let extra = total.saturating_sub(files.len());
    if extra > 0 {
        eprintln!("  ({extra} more)");
    }
    if timed_out {
        eprintln!("[clud] note: scan stopped at 1s — report may be partial");
    }
}

fn human_kb(bytes: u64) -> String {
    let kb = (bytes as f64) / 1024.0;
    if kb >= 1024.0 {
        format!("{:.1} MB", kb / 1024.0)
    } else {
        format!("{} kB", kb.round() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Helper: make a fixture dir with a `.git` marker so `run()` won't bail.
    fn fixture() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        dir
    }

    fn write_file(dir: &Path, rel: &str, size: usize) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        // Write `size` bytes of ASCII content. Use 'a' so the file is a
        // valid source-text payload for any tool that might inspect it.
        let payload = vec![b'a'; size];
        fs::write(path, payload).unwrap();
    }

    #[test]
    fn threshold_constant_is_documented() {
        // Sanity-check that the public constant matches the issue spec —
        // future drift will flag here before the doc comment ages out.
        assert_eq!(SIZE_THRESHOLD, 40 * 1024);
    }

    #[test]
    fn whitelist_excludes_json() {
        let dir = fixture();
        write_file(dir.path(), "big.json", 200 * 1024);
        let report = collect(dir.path(), DEADLINE);
        assert!(report.files.is_empty());
        assert_eq!(report.total_qualifying, 0);
    }

    #[test]
    fn whitelist_excludes_min_files() {
        let dir = fixture();
        write_file(dir.path(), "bundle.min.js", 200 * 1024);
        let report = collect(dir.path(), DEADLINE);
        assert!(report.files.is_empty());
        assert_eq!(report.total_qualifying, 0);
    }

    #[test]
    fn whitelist_includes_known_extensions() {
        let dir = fixture();
        for ext in &["rs", "py", "ts", "go", "cpp"] {
            write_file(dir.path(), &format!("file.{ext}"), 50 * 1024);
        }
        let report = collect(dir.path(), DEADLINE);
        assert_eq!(report.total_qualifying, 5);
        // truncated to REPORT_LIMIT
        assert_eq!(report.files.len(), REPORT_LIMIT);
    }

    #[test]
    fn under_threshold_silent() {
        let dir = fixture();
        write_file(dir.path(), "small.rs", 10 * 1024);
        let report = collect(dir.path(), DEADLINE);
        assert!(report.files.is_empty());
        assert_eq!(report.total_qualifying, 0);
    }

    #[test]
    fn gitignore_pruning() {
        let dir = fixture();
        fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        write_file(dir.path(), "target/foo.rs", 100 * 1024);
        let report = collect(dir.path(), DEADLINE);
        assert!(report.files.is_empty(), "got: {:?}", report.files);
        assert_eq!(report.total_qualifying, 0);
    }

    #[test]
    fn nested_git_pruned() {
        let dir = fixture();
        fs::create_dir_all(dir.path().join("vendored-dep/.git")).unwrap();
        write_file(dir.path(), "vendored-dep/huge.rs", 100 * 1024);
        let report = collect(dir.path(), DEADLINE);
        assert!(report.files.is_empty(), "got: {:?}", report.files);
        assert_eq!(report.total_qualifying, 0);
    }

    #[test]
    fn vendor_dirs_pruned_by_name() {
        // No nested `.git` here — pruning is purely by directory name.
        // Mirrors the real-world case in zackees/clud where `vendor/...`
        // contains committed C++ third-party source without its own .git.
        let dir = fixture();
        for d in &[
            "vendor",
            "third_party",
            "third-party",
            "external",
            "deps",
            "subprojects",
            "node_modules",
        ] {
            write_file(dir.path(), &format!("{d}/big.cpp"), 100 * 1024);
        }
        let report = collect(dir.path(), DEADLINE);
        assert!(report.files.is_empty(), "got: {:?}", report.files);
        assert_eq!(report.total_qualifying, 0);
    }

    #[test]
    fn root_git_does_not_prune_self() {
        let dir = fixture();
        write_file(dir.path(), "src.rs", 50 * 1024);
        let report = collect(dir.path(), DEADLINE);
        assert_eq!(report.total_qualifying, 1);
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].rel_path, PathBuf::from("src.rs"));
    }

    #[test]
    fn report_caps_at_four() {
        let dir = fixture();
        for i in 0..10 {
            write_file(dir.path(), &format!("f{i}.rs"), 100 * 1024);
        }
        let report = collect(dir.path(), DEADLINE);
        assert_eq!(report.files.len(), 4);
        assert_eq!(report.total_qualifying, 10);
    }

    #[test]
    fn report_sorted_descending() {
        let dir = fixture();
        write_file(dir.path(), "a.rs", 120 * 1024);
        write_file(dir.path(), "b.rs", 60 * 1024);
        write_file(dir.path(), "c.rs", 80 * 1024);
        let report = collect(dir.path(), DEADLINE);
        assert_eq!(report.files.len(), 3);
        let sizes: Vec<u64> = report.files.iter().map(|f| f.size).collect();
        assert_eq!(sizes, vec![120 * 1024, 80 * 1024, 60 * 1024]);
    }

    #[test]
    fn report_sizes_human_readable() {
        assert_eq!(human_kb(112 * 1024), "112 kB");
        assert_eq!(human_kb(2 * 1024 * 1024), "2.0 MB");
    }

    #[test]
    fn deadline_short_circuits() {
        // Zero deadline guarantees an immediate cutoff without any
        // sleep-based flake risk in CI.
        let dir = fixture();
        write_file(dir.path(), "src.rs", 100 * 1024);
        let report = collect(dir.path(), Duration::from_millis(0));
        assert!(report.timed_out);
    }
}
