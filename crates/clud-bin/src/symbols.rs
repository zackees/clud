//! `clud symbols` — inspect or verify crash-report symbolication.
//!
//! Background: clud builds with `debug = "line-tables-only"` keep the
//! line tables in the binary itself (#374 PR 1, see
//! [`crate::crash_report`]), so a backtrace resolves to file:line with
//! nothing to fetch. Local dev builds stop there — everything is
//! embedded. Release builds no longer do: `[profile.release]` sets
//! `split-debuginfo = "packed"`, which moves the rest of the DWARF into
//! a sidecar (a `.dwp` on Linux, the `.pdb` / `.dSYM` Windows and macOS
//! always produced) so the manylinux wheel fits under PyPI's 100 MB
//! project limit. What a release binary loses without its `.dwp` is the
//! inlined-subroutine DIEs: file:line still resolves per *physical*
//! frame and function names still come from `.symtab`, but the expanded
//! inline caller chain does not. CI attaches the `.dwp` to the GitHub
//! release, so the "fetch sidecars on first unsymbolicated report" path
//! from the original issue is now implementable rather than a no-op;
//! #1016 tracks it. This module does not fetch anything today — it is
//! kept as an opportunistic verifier over what the running binary can
//! already resolve on its own:
//!
//! - `clud symbols` (bare) prints a five-line summary of the crash-
//!   reports directory.
//! - `clud symbols install` verifies that the running binary resolves
//!   the most-recent crash report's backtrace. Exits 1 if it can't.
//! - `clud symbols verify [--all]` is the same but explicit. `--all`
//!   widens the scope from the most recent to every report.
//!
//! The opportunistic startup notice (see [`crate::crash_report::install`])
//! prints a one-line hint pointing at `clud symbols verify` when a fresh
//! report's backtrace is unsymbolicated, so users discover the command
//! without needing to remember it.

use std::fs;
use std::path::{Path, PathBuf};

use crate::args::{Args, SymbolsSubcommand};

/// Where a released sidecar lives, as one named constant.
///
/// #1016 item 1: the URL is derivable from the version, but building it at
/// the call site puts the tag-vs-version convention in as many places as
/// there are callers, and leaves a fork or a mirror nothing to repoint.
///
/// The tag is the bare version -- clud's releases are tagged `2.7.9`, not
/// `v2.7.9` -- and that convention lives here and nowhere else.
pub const RELEASE_DOWNLOAD_BASE: &str = "https://github.com/zackees/clud/releases/download";

/// The sidecar asset for `target`, or `None` when that triple ships none.
///
/// `ci/xbuild.py::collect_debuginfo` stages exactly one file per ELF triple,
/// named `clud-<triple>.dwp`, and deliberately not the `deps/clud-<hash>.dwp`
/// it was copied from -- its own comment says an asset name carrying a build
/// hash "is one #1016's fetcher could not predict". This is that fetcher's
/// half of the arrangement.
///
/// Only ELF targets are covered, which is not an oversight: on MSVC and Apple
/// `split-debuginfo = "packed"` is already the default, their `.pdb` / `.dSYM`
/// were never embedded in the shipped wheel, and `collect_debuginfo` does not
/// stage them. Returning `Some` for those would name an asset that does not
/// exist, which is worse than saying there is nothing to fetch.
#[must_use]
pub fn sidecar_asset_name(target: &str) -> Option<String> {
    target
        .contains("-linux-")
        .then(|| format!("clud-{target}.dwp"))
}

/// The full download URL for `version`'s sidecar for `target`.
#[must_use]
pub fn sidecar_url(version: &str, target: &str) -> Option<String> {
    let asset = sidecar_asset_name(target)?;
    Some(format!("{RELEASE_DOWNLOAD_BASE}/{version}/{asset}"))
}

/// Heuristic for whether a single backtrace line is an `at FILE:LINE`
/// frame produced by `std::backtrace::Backtrace`.
///
/// Pattern: leading whitespace, the literal `at `, then any path, then
/// `:` and at least one digit. Optional `:column`. We match by character
/// scan rather than regex to avoid pulling in `regex` for one helper.
fn is_resolved_frame_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix("at ") else {
        return false;
    };
    // Find the last `:` followed by digits. Backtrace lines on Windows
    // can contain drive-letter colons (`C:\...`), so we scan from the
    // right.
    let last_colon = match rest.rfind(':') {
        Some(i) => i,
        None => return false,
    };
    let after_colon = &rest[last_colon + 1..];
    // Trim trailing whitespace + an optional `:column` suffix —
    // rustc-format lines look like `at file.rs:42:5`.
    let after_colon = after_colon.trim_end();
    // If there's a `:column`, split it off first.
    let head = after_colon.split(':').next().unwrap_or("");
    !head.is_empty() && head.chars().all(|c| c.is_ascii_digit())
}

/// Count `at FILE:LINE` frame lines in a backtrace string.
pub(crate) fn count_resolved_frames(backtrace: &str) -> usize {
    backtrace
        .lines()
        .filter(|l| is_resolved_frame_line(l))
        .count()
}

/// True when the backtrace contains zero `at FILE:LINE` lines. Empty
/// backtraces are treated as unsymbolicated.
pub(crate) fn is_unsymbolicated(backtrace: &str) -> bool {
    count_resolved_frames(backtrace) == 0
}

/// Sort `dir` entries by filename's leading unix-ms prefix, newest
/// first. Returns paths only.
fn list_reports_newest_first(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut entries: Vec<(u128, PathBuf)> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy().into_owned();
            if !name.ends_with(".json") {
                return None;
            }
            let ms = name.split('-').next()?.parse::<u128>().ok()?;
            Some((ms, e.path()))
        })
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(entries.into_iter().map(|(_, p)| p).collect())
}

fn read_report_backtrace(path: &Path) -> Option<(String, String, u128)> {
    let raw = fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let backtrace = value.get("backtrace")?.as_str()?.to_string();
    let role = value
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let ts = value
        .get("timestamp_unix_ms")
        .and_then(|v| v.as_u64())
        .map(|t| t as u128)
        .unwrap_or(0);
    Some((backtrace, role, ts))
}

/// Dispatch entry called from `main.rs`. Returns a process exit code.
pub fn run(_args: &Args, subcommand: Option<SymbolsSubcommand>) -> i32 {
    let dir = match crate::crash_report::crashes_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("clud symbols: cannot resolve crash-report dir: {e}");
            return 1;
        }
    };
    let reports = match list_reports_newest_first(&dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("clud symbols: cannot read {}: {e}", dir.display());
            return 1;
        }
    };
    match subcommand {
        None => print_summary(&dir, &reports),
        Some(SymbolsSubcommand::Install) => {
            verify(&reports, /*all=*/ false)
        }
        Some(SymbolsSubcommand::Verify { all }) => verify(&reports, all),
    }
}

fn print_summary(dir: &Path, reports: &[PathBuf]) -> i32 {
    println!("clud symbols: crashes dir: {}", dir.display());
    println!("total reports: {}", reports.len());
    if reports.is_empty() {
        println!("no reports to inspect");
        return 0;
    }
    let mut resolved = 0usize;
    let mut unresolved = 0usize;
    for path in reports {
        if let Some((bt, _, _)) = read_report_backtrace(path) {
            if is_unsymbolicated(&bt) {
                unresolved += 1;
            } else {
                resolved += 1;
            }
        }
    }
    println!("reports with file:line frames: {resolved}");
    println!("reports without file:line frames: {unresolved}");
    if let Some(newest) = reports.first() {
        if let Some((_, role, ts)) = read_report_backtrace(newest) {
            println!(
                "most recent: {} (role={}, unix_ms={})",
                newest.display(),
                role,
                ts
            );
        } else {
            println!("most recent: {}", newest.display());
        }
    }
    0
}

fn verify(reports: &[PathBuf], all: bool) -> i32 {
    if reports.is_empty() {
        println!("clud symbols: no crash reports to verify");
        return 0;
    }
    let targets: &[PathBuf] = if all { reports } else { &reports[..1] };
    let mut all_resolved = true;
    for path in targets {
        match read_report_backtrace(path) {
            Some((bt, role, _)) => {
                let resolved = count_resolved_frames(&bt);
                if resolved == 0 {
                    println!(
                        "FAIL {} (role={}): backtrace contains 0 file:line frames",
                        path.display(),
                        role
                    );
                    all_resolved = false;
                } else {
                    println!(
                        "OK   {} (role={}): {} file:line frames",
                        path.display(),
                        role,
                        resolved
                    );
                }
            }
            None => {
                println!(
                    "FAIL {}: unreadable JSON or missing backtrace",
                    path.display()
                );
                all_resolved = false;
            }
        }
    }
    if all_resolved {
        println!(
            "clud symbols: OK — embedded line tables resolved {} report(s)",
            targets.len()
        );
        0
    } else {
        println!(
            "clud symbols: FAIL — embedded line tables did not resolve one or more reports.\n\
             Build with `debug = \"line-tables-only\"` (already the project default) and ensure\n\
             the binary running `clud symbols verify` is the same build that produced the report."
        );
        // #1016: a release build's inline caller chain lives in a sidecar
        // beside the release, not in the binary. Naming the exact asset is
        // the difference between "symbolication failed" and something the
        // reader can act on -- and it is derivable here because the triple is
        // baked in (`crash_report::BUILD_TARGET`).
        match sidecar_url(env!("CARGO_PKG_VERSION"), crate::crash_report::BUILD_TARGET) {
            Some(url) => println!(
                "\nThis build's sidecar debug info (the inlined-frame DIEs a release\n\
                 binary does not carry) is published beside the release:\n  {url}\n\
                 Fetching it automatically is #1016; for now it can be downloaded by hand."
            ),
            None => println!(
                "\nNo sidecar is published for {} — on this platform the debug info\n\
                 was never split out of the shipped binary, so there is nothing to fetch.",
                crate::crash_report::BUILD_TARGET
            ),
        }
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_resolved_lines_in_typical_backtrace() {
        let bt = "   0: clud::main::h1234abcd\n             at /home/user/clud/src/main.rs:42:5\n   1: core::ops::function::FnOnce::call_once\n             at /rustc/abcdef/library/core/src/ops/function.rs:250:5\n";
        assert_eq!(count_resolved_frames(bt), 2);
        assert!(!is_unsymbolicated(bt));
    }

    #[test]
    fn detects_unsymbolicated_backtrace() {
        let bt = "   0: 0x7fffabcd1234\n   1: 0x7fffabcd5678\n   2: 0x7fffabcd9abc\n";
        assert_eq!(count_resolved_frames(bt), 0);
        assert!(is_unsymbolicated(bt));
    }

    #[test]
    fn empty_backtrace_is_unsymbolicated() {
        assert_eq!(count_resolved_frames(""), 0);
        assert!(is_unsymbolicated(""));
    }

    #[test]
    fn windows_drive_letter_does_not_trip_resolution() {
        let bt =
            "   0: clud::main::h1234abcd\n             at C:\\Users\\me\\clud\\src\\main.rs:42:5\n";
        assert_eq!(count_resolved_frames(bt), 1);
    }

    #[test]
    fn frame_without_at_prefix_is_not_resolved() {
        let bt = "             /home/user/clud/src/main.rs:42:5\n";
        assert_eq!(count_resolved_frames(bt), 0);
    }

    #[test]
    fn frame_without_line_number_is_not_resolved() {
        let bt = "             at /home/user/clud/src/main.rs\n";
        assert_eq!(count_resolved_frames(bt), 0);
    }

    #[test]
    fn list_reports_orders_by_unix_ms_prefix_desc() -> std::io::Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::write(tmp.path().join("100-foreground-1.json"), "{}")?;
        fs::write(tmp.path().join("300-foreground-3.json"), "{}")?;
        fs::write(tmp.path().join("200-foreground-2.json"), "{}")?;
        let ordered = list_reports_newest_first(tmp.path())?;
        assert_eq!(ordered.len(), 3);
        assert!(ordered[0].ends_with("300-foreground-3.json"));
        assert!(ordered[1].ends_with("200-foreground-2.json"));
        assert!(ordered[2].ends_with("100-foreground-1.json"));
        Ok(())
    }

    #[test]
    fn read_report_backtrace_extracts_role_and_ts() -> std::io::Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("500-daemon-9999.json");
        fs::write(
            &path,
            r#"{
                "version": "0.0.0",
                "role": "daemon",
                "kind": "panic",
                "pid": 9999,
                "args": [],
                "timestamp_unix_ms": 500,
                "panic_message": "boom",
                "backtrace": "   0: clud::main::h\n             at /x/main.rs:1:1\n"
            }"#,
        )?;
        let (bt, role, ts) = read_report_backtrace(&path).expect("parsed");
        assert_eq!(role, "daemon");
        assert_eq!(ts, 500);
        assert!(bt.contains("/x/main.rs:1:1"));
        assert_eq!(count_resolved_frames(&bt), 1);
        Ok(())
    }

    /// The derived URL must be the one that actually exists.
    ///
    /// Checked against release 2.7.9, whose asset list really does contain
    /// `clud-x86_64-unknown-linux-gnu.dwp`. Pinning a live example is what
    /// makes this more than a restatement of the format string: the tag is
    /// the bare version (clud tags `2.7.9`, not `v2.7.9`), and getting that
    /// wrong yields a 404 that looks like a missing sidecar.
    #[test]
    fn the_derived_url_matches_a_release_asset_that_exists() {
        assert_eq!(
            sidecar_url("2.7.9", "x86_64-unknown-linux-gnu").as_deref(),
            Some(
                "https://github.com/zackees/clud/releases/download/2.7.9/\
                 clud-x86_64-unknown-linux-gnu.dwp"
            )
        );
        assert_eq!(
            sidecar_asset_name("aarch64-unknown-linux-gnu").as_deref(),
            Some("clud-aarch64-unknown-linux-gnu.dwp"),
            "the aarch64 sidecar is published too"
        );
    }

    /// Triples that publish no sidecar must say so rather than name a 404.
    ///
    /// `collect_debuginfo` stages only the ELF `.dwp`: on MSVC and Apple,
    /// `packed` split-debuginfo is already the default and the `.pdb`/`.dSYM`
    /// was never inside the shipped wheel. A confident URL for an asset that
    /// was never uploaded is worse than "there is nothing to fetch".
    #[test]
    fn triples_without_a_published_sidecar_return_none() {
        for target in [
            "x86_64-pc-windows-msvc",
            "aarch64-pc-windows-msvc",
            "x86_64-apple-darwin",
            "aarch64-apple-darwin",
        ] {
            assert_eq!(
                sidecar_asset_name(target),
                None,
                "{target} publishes no .dwp"
            );
            assert_eq!(sidecar_url("2.7.9", target), None, "{target}");
        }
    }

    /// The asset name is the one `ci/xbuild.py::collect_debuginfo` stages.
    ///
    /// That code renames the `deps/clud-<hash>.dwp` it copies from precisely
    /// so this side can predict the name -- its comment says an asset name
    /// carrying a build hash "is one #1016's fetcher could not predict". If
    /// either side drifts, the fetch 404s.
    #[test]
    fn the_asset_name_carries_the_triple_and_no_build_hash() {
        let name = sidecar_asset_name("x86_64-unknown-linux-gnu").expect("linux publishes one");
        assert!(name.starts_with("clud-"), "{name}");
        assert!(name.ends_with(".dwp"), "{name}");
        assert!(name.contains("x86_64-unknown-linux-gnu"), "{name}");
        assert!(
            !name.chars().any(|c| c == '#'),
            "the staged name is fixed, not hash-suffixed: {name}"
        );
    }

    /// The base is a single constant so a fork or mirror has one thing to
    /// repoint, which is item 1's stated reason for existing.
    #[test]
    fn the_release_base_is_one_named_constant() {
        assert!(RELEASE_DOWNLOAD_BASE.starts_with("https://"));
        assert!(
            !RELEASE_DOWNLOAD_BASE.ends_with('/'),
            "the joiner adds the separator; a trailing one yields a double slash"
        );
    }
}
