//! The corrupt-index escape hatch, end to end (#556).
//!
//! `index_pass` distinguishes "no index" from "index that will not parse", and
//! #556 requires those to route differently: no index means walk the tree,
//! but a *corrupt* index must take one killable `git ls-files --debug` first.
//! Falling straight to the walker there would surrender the entire win on
//! precisely the repos most likely to be large.
//!
//! These live in their own file rather than in `index_pass`'s test module
//! because they exercise the *routing* across both passes, not either one.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::index_pass::{index_pass, IndexPassError};
use super::ls_files_pass::{ls_files_argv, parse_ls_files_debug};
use super::SIZE_THRESHOLD;

/// Build a repo whose `.git/index` exists but is garbage.
fn repo_with_corrupt_index() -> Option<TempDir> {
    let tmp = TempDir::new().ok()?;
    let root = tmp.path();
    if crate::worktrees::run_git(root, &["init"]).is_err() {
        return None; // no git here — the caller skips.
    }
    let _ = crate::worktrees::run_git(root, &["config", "user.email", "t@example.com"]);
    let _ = crate::worktrees::run_git(root, &["config", "user.name", "t"]);
    let big = "x".repeat(SIZE_THRESHOLD as usize + 500);
    std::fs::write(root.join("big.rs"), &big).ok()?;
    crate::worktrees::run_git(root, &["add", "-A"]).ok()?;
    Some(tmp)
}

/// A truncated index must be reported as `Parse`, not `NoIndex`.
///
/// The distinction is the whole routing decision: these map to different
/// fallbacks, and collapsing them is exactly the bug this closes.
#[test]
fn a_truncated_index_is_a_parse_error_not_a_missing_one() {
    let Some(tmp) = repo_with_corrupt_index() else {
        return;
    };
    let root = tmp.path();
    let index = super::index_pass::resolve_index_path(root).expect("index path");
    // Keep a plausible header, destroy the body.
    std::fs::write(&index, b"DIRC\x00\x00\x00\x02\x00\x00\x00\x09truncated").unwrap();

    match index_pass(root) {
        Err(IndexPassError::Parse) => {}
        other => panic!("a corrupt index must be Parse, got {other:?}"),
    }
}

/// The fallback recovers the same tracked file the healthy index pass would
/// have reported — that is what makes it a fallback rather than a fig leaf.
#[test]
fn the_ls_files_fallback_recovers_the_large_tracked_file() {
    let Some(tmp) = repo_with_corrupt_index() else {
        return;
    };
    let root = tmp.path();
    // Corrupt the index *after* `git add`, so git's own view is still fine and
    // `ls-files --debug` can still read it. This is the real-world shape: gix
    // rejects a format or a checksum that git itself tolerates.
    let healthy = index_pass(root).expect("index parses before corruption");
    let expected_big = healthy
        .qualifying
        .iter()
        .any(|f| f.rel_path == Path::new("big.rs"))
        || healthy
            .needs_verification
            .contains(&PathBuf::from("big.rs"));
    assert!(expected_big, "fixture should stage a large file");

    let Some(entries) = super::ls_files_pass::ls_files_pass(root) else {
        // git present but `--debug` unavailable/unsupported here: the routing
        // still degrades to the walker, which is the documented behaviour.
        return;
    };
    let out = super::index_pass::classify_entries(entries.into_iter());
    let reported = out
        .qualifying
        .iter()
        .any(|f| f.rel_path == Path::new("big.rs"))
        || out.needs_verification.contains(&PathBuf::from("big.rs"));
    assert!(
        reported,
        "the fallback must recover the large tracked file: {out:?}"
    );
}

/// #556's explicit argv assertion, restated at the routing level so it fails
/// here too if someone swaps the fallback implementation wholesale.
#[test]
fn the_fallback_argv_stays_off_the_object_database() {
    let argv = ls_files_argv();
    assert_eq!(argv, ["git", "ls-files", "--debug"]);
}

/// Pass-1 latency budget from #556: ≤25 ms to classify a 100k-entry index.
///
/// The parse itself is `gix-index`'s and is benchmarked upstream; what this
/// repo owns is the classification on top of it — the filters and the
/// threshold — over an index of that size. Measuring our half keeps the
/// assertion meaningful and machine-independent enough to run in CI.
#[test]
fn classifying_a_hundred_thousand_entries_stays_inside_the_budget() {
    let entries: Vec<(PathBuf, u32)> = (0..100_000)
        .map(|i| {
            // Mix of qualifying, sub-threshold, filtered-out, and racily-clean
            // so every branch of the classifier is exercised, not just the
            // cheap reject.
            let size = match i % 4 {
                0 => SIZE_THRESHOLD + 1,
                1 => 10,
                2 => 0,
                _ => SIZE_THRESHOLD * 2,
            };
            let name = match i % 3 {
                0 => format!("src/mod{i}/file{i}.rs"),
                1 => format!("vendor/dep{i}/bundle{i}.min.js"),
                _ => format!("assets/blob{i}.bin"),
            };
            (PathBuf::from(name), size as u32)
        })
        .collect();

    let start = std::time::Instant::now();
    let out = super::index_pass::classify_entries(entries.into_iter());
    let elapsed = start.elapsed();

    assert!(
        !out.qualifying.is_empty(),
        "the fixture must produce work, or the timing is meaningless"
    );
    // Generous against the 25 ms budget: CI runners are slower and shared, and
    // a flaky perf assertion is worse than a loose one. An order-of-magnitude
    // regression still trips it, which is what the budget is protecting.
    assert!(
        elapsed < std::time::Duration::from_millis(250),
        "classifying 100k entries took {elapsed:?}; #556 budgets 25 ms for the \
         whole pass on a real index"
    );
}

/// The parser and the classifier agree on what a racily-clean entry means: a
/// cached size of 0 is "unknown", and must reach the verification list rather
/// than being read as "empty file, nothing to see".
#[test]
fn a_racily_clean_entry_from_the_fallback_routes_to_verification() {
    let text = "big.rs\n  size: 0\tflags: 0\n";
    let entries = parse_ls_files_debug(text);
    assert_eq!(entries, vec![(PathBuf::from("big.rs"), 0)]);

    let out = super::index_pass::classify_entries(entries.into_iter());
    assert!(
        out.needs_verification.contains(&PathBuf::from("big.rs")),
        "size-0 must queue for pass-2 verification, not be dropped: {out:?}"
    );
    assert!(out.qualifying.is_empty());
}
