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
//! from the original issue is real work again rather than a no-op.
//!
//! - `clud symbols` (bare) prints a five-line summary of the crash-
//!   reports directory.
//! - `clud symbols install` fetches the sidecar the most recent report
//!   needs, checks it against the release's `SHA256SUMS`, and caches it
//!   under `~/.clud/state/symbols/<version>/`. It used to be an alias
//!   for `verify`, which was honest while no sidecars existed and became
//!   wrong the moment `split-debuginfo = "packed"` shipped them.
//! - `clud symbols verify [--all]` checks that the running binary can
//!   resolve a report's backtrace. Exits 1 if it can't. `--all` widens
//!   the scope from the most recent report to every report.
//!
//! # Network
//!
//! `install` is the only thing here that reaches the network, and only
//! when a person types it. The crash path does not fetch — the process is
//! already dying and may be in a signal handler where allocation is
//! unsafe — the startup notice does not fetch, and `verify` does not
//! fetch. Air-gapped hosts therefore keep working, with local
//! unsymbolicated output as the fallback (#1016).
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

/// The release's checksum manifest, which covers the sidecar assets.
///
/// `auto-release.yml` publishes one `SHA256SUMS` per release spanning `dist/`
/// and `debuginfo/`, so a fetched sidecar can be checked against the bytes the
/// release actually published.
pub const CHECKSUM_MANIFEST: &str = "SHA256SUMS";

/// URL of the checksum manifest for `version`.
#[must_use]
pub fn checksum_manifest_url(version: &str) -> String {
    format!("{RELEASE_DOWNLOAD_BASE}/{version}/{CHECKSUM_MANIFEST}")
}

/// The expected sha256 of `asset`, parsed out of a `SHA256SUMS` body.
///
/// Entries are `<hex>  ./<name>`, the `sha256sum` format the release job
/// generates from inside the directory, hence the `./` prefix. Both spellings
/// are accepted so a manifest generated without it still matches.
///
/// # Why a checksum is the verification available, and what it does not prove
///
/// #1016 item 3 asks that a sidecar whose build-id does not match be refused.
/// The published `.dwp` has no build-id to compare: a DWARF package is a
/// relocatable object carrying `.debug_*.dwo` sections and `.debug_cu_index`,
/// and `readelf -n` on release 2.7.9's sidecar reports no notes at all.
///
/// So this authenticates the *asset*, not the *pairing*: it proves the bytes
/// are the ones that release published for this version and triple. It does
/// not prove they belong to the binary in hand -- a rebuild at the same
/// version would still pair with the published sidecar and mis-symbolicate.
/// DWARF's own answer to that is the DWO ID, which `.debug_cu_index` is keyed
/// by; see the discussion on #1016.
#[must_use]
pub fn expected_sha256(manifest: &str, asset: &str) -> Option<String> {
    manifest.lines().find_map(|line| {
        let (hex, name) = line.split_once("  ")?;
        let name = name.trim();
        let name = name.strip_prefix("./").unwrap_or(name);
        (name == asset && !hex.is_empty()).then(|| hex.trim().to_string())
    })
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

/// Turn a URL into bytes.
///
/// A seam, so the install path below is exercised without a network: an 85 MB
/// `.dwp` is not something a test suite should be downloading, and a test that
/// needs the real release to exist would fail for reasons that have nothing to
/// do with the code.
pub type Fetch<'a> = &'a dyn Fn(&str) -> Result<Vec<u8>, String>;

/// The real fetcher. `ureq` is already this crate's HTTP client (`codex_auth`,
/// `codex_bridge`).
fn ureq_fetch(url: &str) -> Result<Vec<u8>, String> {
    let response = ureq::get(url).call().map_err(|err| err.to_string())?;
    let mut bytes = Vec::new();
    // `into_reader`, not `into_string`: the payload is a binary sidecar tens
    // of megabytes long, and `into_string` both caps at 10 MB and would mangle
    // it as UTF-8.
    std::io::Read::read_to_end(&mut response.into_reader(), &mut bytes)
        .map_err(|err| err.to_string())?;
    Ok(bytes)
}

/// Where fetched sidecars live: `~/.clud/state/symbols/<version>/<asset>`.
///
/// Beside `~/.clud/state/crashes/`, as #1016 suggested. Keyed by version
/// because that is what the asset is keyed by -- see [`expected_sha256`] for
/// why a build-id cannot be the key here.
fn cache_root() -> std::io::Result<PathBuf> {
    let dir = crate::crash_report::crashes_dir()?
        .parent()
        .ok_or_else(|| std::io::Error::other("crashes dir has no parent"))?
        .join("symbols");
    Ok(dir)
}

/// The `(version, target)` a report was written by, or `None` if it does not
/// say. Older reports predate both fields.
fn read_report_identity(path: &Path) -> Option<(String, String)> {
    let raw = fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let version = value.get("version")?.as_str()?.to_string();
    let target = value.get("target")?.as_str()?.to_string();
    Some((version, target))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `clud symbols install` — fetch the sidecar this machine's most recent crash
/// report needs, verify it against the release manifest, and cache it.
///
/// # Network happens here and nowhere else
///
/// #1016 is explicit that a tool which phones a release host after a crash
/// will surprise people, and that some environments are air-gapped. So the
/// only thing that ever reaches the network is this subcommand, typed by a
/// person. The crash path does not fetch, the startup notice does not fetch,
/// and `verify` does not fetch. That also keeps the fetch out of a dying
/// process where allocation may be unsafe.
pub fn install(reports: &[PathBuf]) -> i32 {
    let cache = match cache_root() {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("clud symbols: cannot resolve the symbol cache: {err}");
            return 1;
        }
    };
    install_with(reports, &ureq_fetch, &cache)
}

/// Test seam for [`install`].
pub fn install_with(reports: &[PathBuf], fetch: Fetch, cache_root: &Path) -> i32 {
    let Some(report) = reports.first() else {
        println!("clud symbols: no crash reports, so no sidecar to fetch.");
        return 0;
    };
    let Some((version, target)) = read_report_identity(report) else {
        eprintln!(
            "clud symbols: {} records no version/target, so the matching \
             sidecar cannot be named. Reports written before #1016 do not \
             carry them.",
            report.display()
        );
        return 1;
    };
    let Some(asset) = sidecar_asset_name(&target) else {
        // Not a failure of this machine: no such asset is published. Saying
        // so beats a 404 the user has to interpret.
        println!(
            "clud symbols: no sidecar is published for {target}. Debug info \
             for that target is not split out of the wheel, so there is \
             nothing to fetch."
        );
        return 0;
    };

    let destination = cache_root.join(&version).join(&asset);
    if destination.is_file() {
        println!(
            "clud symbols: already have {} ({})",
            asset,
            destination.display()
        );
        return 0;
    }

    let manifest_url = checksum_manifest_url(&version);
    let manifest = match fetch(&manifest_url) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(err) => {
            eprintln!("clud symbols: cannot fetch {manifest_url}: {err}");
            return 1;
        }
    };
    // The expected digest is read *before* the payload, so there is never a
    // window where a downloaded file exists with nothing to check it against.
    let Some(expected) = expected_sha256(&manifest, &asset) else {
        eprintln!(
            "clud symbols: release {version} publishes no checksum for \
             {asset}; refusing to install bytes the release does not vouch \
             for."
        );
        return 1;
    };

    let Some(url) = sidecar_url(&version, &target) else {
        eprintln!("clud symbols: no sidecar URL for {target}");
        return 1;
    };
    println!("clud symbols: fetching {url}");
    let bytes = match fetch(&url) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("clud symbols: cannot fetch {url}: {err}");
            return 1;
        }
    };

    let actual = sha256_hex(&bytes);
    if actual != expected {
        // Nothing is written. A sidecar that is not the published one would
        // symbolicate to confident, wrong line numbers -- worse than not
        // symbolicating, which is the whole argument in #1016 item 3.
        eprintln!(
            "clud symbols: checksum mismatch for {asset}\n  expected {expected}\n  got      {actual}\nRefusing to install."
        );
        return 1;
    }

    if let Some(parent) = destination.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            eprintln!("clud symbols: cannot create {}: {err}", parent.display());
            return 1;
        }
    }
    // Write beside and rename, so a reader never sees a half-written sidecar
    // and an interrupted install leaves no file that looks complete.
    let staging = destination.with_extension("part");
    if let Err(err) = fs::write(&staging, &bytes) {
        eprintln!("clud symbols: cannot write {}: {err}", staging.display());
        return 1;
    }
    if let Err(err) = fs::rename(&staging, &destination) {
        let _ = fs::remove_file(&staging);
        eprintln!(
            "clud symbols: cannot place {}: {err}",
            destination.display()
        );
        return 1;
    }
    println!(
        "clud symbols: installed {} ({} bytes, sha256 verified)",
        destination.display(),
        bytes.len()
    );
    0
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
        // #1016: `install` now installs. It used to be an alias for `verify`,
        // which was honest while there were no sidecars to fetch and became
        // wrong the moment `split-debuginfo = "packed"` shipped them.
        Some(SymbolsSubcommand::Install) => install(&reports),
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

    /// Real bytes from release 2.7.9's `SHA256SUMS`, trimmed to the rows that
    /// matter. Using the published manifest rather than a synthetic one is
    /// what makes the parser's format assumptions testable -- the `./` prefix
    /// and the two-space separator are `sha256sum`'s, not ours.
    const REAL_MANIFEST: &str = concat!(
        "b586d13c8859db1c0f8bd665f83fdd3eecb60cc6fd6c6937f5e96e9c848c9648  ",
        "./clud-2.7.9-py3-none-manylinux_2_17_x86_64.manylinux2014_x86_64.whl\n",
        "18f5d3675e5cd1237f81751517eadce44dc0cd053c330c45b0691608acb3aef4  ",
        "./clud-aarch64-unknown-linux-gnu.dwp\n",
        "6d30b668c7eb96f5e0e0d3d9c3c07becddcabaac9f18312f623616e4838901c3  ",
        "./clud-x86_64-unknown-linux-gnu.dwp\n",
    );

    /// The checksum found must be the one the release actually published.
    ///
    /// `6d30b668...` was verified by downloading the 62 MB sidecar and running
    /// `sha256sum` over it, so this pins the parser against reality rather
    /// than against itself.
    #[test]
    fn finds_the_published_checksum_for_a_sidecar() {
        let asset = sidecar_asset_name("x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(
            expected_sha256(REAL_MANIFEST, &asset).as_deref(),
            Some("6d30b668c7eb96f5e0e0d3d9c3c07becddcabaac9f18312f623616e4838901c3")
        );
    }

    /// The two sidecars differ by one path component; picking the wrong row
    /// would verify successfully against the wrong architecture's DWARF.
    #[test]
    fn does_not_confuse_the_two_architectures() {
        let x86 = expected_sha256(REAL_MANIFEST, "clud-x86_64-unknown-linux-gnu.dwp");
        let arm = expected_sha256(REAL_MANIFEST, "clud-aarch64-unknown-linux-gnu.dwp");
        assert!(x86.is_some() && arm.is_some());
        assert_ne!(x86, arm, "each triple has its own sidecar and its own sum");
    }

    /// An asset that is not listed has no expected sum -- callers must treat
    /// that as "cannot verify", never as "verified".
    #[test]
    fn an_unlisted_asset_has_no_checksum() {
        assert_eq!(
            expected_sha256(REAL_MANIFEST, "clud-x86_64-pc-windows-msvc.dwp"),
            None
        );
        assert_eq!(expected_sha256("", "anything"), None);
        assert_eq!(expected_sha256("garbage without a separator\n", "x"), None);
    }

    /// A manifest generated without `sha256sum`'s `./` prefix still matches,
    /// so the parser does not depend on which directory the release job ran in.
    #[test]
    fn a_bare_name_matches_too() {
        let manifest = "abc123  clud-x86_64-unknown-linux-gnu.dwp\n";
        assert_eq!(
            expected_sha256(manifest, "clud-x86_64-unknown-linux-gnu.dwp").as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn the_manifest_url_sits_beside_the_sidecar() {
        assert_eq!(
            checksum_manifest_url("2.7.9"),
            "https://github.com/zackees/clud/releases/download/2.7.9/SHA256SUMS"
        );
    }

    // -----------------------------------------------------------------
    // #1016: `clud symbols install` actually fetches.
    // -----------------------------------------------------------------

    use std::cell::RefCell;

    /// A report on disk, as `install_with` will read it.
    fn write_report(dir: &Path, version: &str, target: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join("1700000000000-crash.json");
        fs::write(
            &path,
            serde_json::json!({
                "version": version,
                "target": target,
                "backtrace": "0: main\n",
            })
            .to_string(),
        )
        .unwrap();
        path
    }

    /// Records every URL asked for, so a test can assert that nothing was
    /// fetched at all — "did not touch the network" is a property worth
    /// pinning, not just "returned the right code".
    struct FakeNet {
        manifest: String,
        payload: Vec<u8>,
        seen: RefCell<Vec<String>>,
    }

    impl FakeNet {
        fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
            self.seen.borrow_mut().push(url.to_string());
            if url.ends_with(CHECKSUM_MANIFEST) {
                Ok(self.manifest.clone().into_bytes())
            } else {
                Ok(self.payload.clone())
            }
        }
    }

    const LINUX: &str = "x86_64-unknown-linux-gnu";

    fn net_for(payload: &[u8]) -> FakeNet {
        let asset = sidecar_asset_name(LINUX).unwrap();
        FakeNet {
            manifest: format!("{}  ./{asset}\n", sha256_hex(payload)),
            payload: payload.to_vec(),
            seen: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn a_verified_sidecar_is_installed_into_the_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let report = write_report(&tmp.path().join("crashes"), "2.8.0", LINUX);
        let cache = tmp.path().join("symbols");
        let net = net_for(b"dwarf package bytes");

        let code = install_with(&[report], &|url| net.fetch(url), &cache);

        assert_eq!(code, 0);
        let installed = cache.join("2.8.0").join(sidecar_asset_name(LINUX).unwrap());
        assert!(installed.is_file(), "sidecar was not cached");
        assert_eq!(fs::read(&installed).unwrap(), b"dwarf package bytes");
    }

    /// The refusal that matters. A sidecar that is not the published one
    /// symbolicates to confident, wrong line numbers — worse than not
    /// symbolicating at all, which is #1016's own argument.
    #[test]
    fn a_checksum_mismatch_installs_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let report = write_report(&tmp.path().join("crashes"), "2.8.0", LINUX);
        let cache = tmp.path().join("symbols");
        let mut net = net_for(b"the published bytes");
        net.payload = b"something else entirely".to_vec();

        let code = install_with(&[report], &|url| net.fetch(url), &cache);

        assert_eq!(code, 1);
        let asset = sidecar_asset_name(LINUX).unwrap();
        assert!(
            !cache.join("2.8.0").join(&asset).exists(),
            "a file that failed verification was left on disk"
        );
        assert!(
            !cache.join("2.8.0").join(format!("{asset}.part")).exists(),
            "the staging file was left behind"
        );
    }

    /// A release that does not list the asset vouches for nothing, so there is
    /// nothing to check the bytes against. Refuse before downloading them.
    #[test]
    fn an_asset_absent_from_the_manifest_is_refused_before_download() {
        let tmp = tempfile::tempdir().unwrap();
        let report = write_report(&tmp.path().join("crashes"), "2.8.0", LINUX);
        let cache = tmp.path().join("symbols");
        let net = FakeNet {
            manifest: "deadbeef  ./some-other-file.tar.gz\n".to_string(),
            payload: b"never requested".to_vec(),
            seen: RefCell::new(Vec::new()),
        };

        let code = install_with(&[report], &|url| net.fetch(url), &cache);

        assert_eq!(code, 1);
        let seen = net.seen.borrow();
        assert_eq!(
            seen.len(),
            1,
            "the payload must not be fetched when nothing can verify it: {seen:?}"
        );
    }

    /// Windows and macOS publish no `.dwp`. Reporting that is the correct
    /// answer, and it must not cost a request that would 404.
    #[test]
    fn a_target_with_no_published_sidecar_touches_no_network() {
        let tmp = tempfile::tempdir().unwrap();
        let report = write_report(
            &tmp.path().join("crashes"),
            "2.8.0",
            "x86_64-pc-windows-msvc",
        );
        let cache = tmp.path().join("symbols");
        let net = net_for(b"unused");

        let code = install_with(&[report], &|url| net.fetch(url), &cache);

        assert_eq!(code, 0, "not an error: no such asset is published");
        assert!(net.seen.borrow().is_empty(), "{:?}", net.seen.borrow());
    }

    /// Second run is free and offline. Re-downloading 85 MB because the user
    /// typed the command twice would be its own bug.
    #[test]
    fn an_already_installed_sidecar_is_not_fetched_again() {
        let tmp = tempfile::tempdir().unwrap();
        let report = write_report(&tmp.path().join("crashes"), "2.8.0", LINUX);
        let cache = tmp.path().join("symbols");
        let net = net_for(b"dwarf package bytes");

        assert_eq!(
            install_with(std::slice::from_ref(&report), &|url| net.fetch(url), &cache),
            0
        );
        let after_first = net.seen.borrow().len();

        assert_eq!(install_with(&[report], &|url| net.fetch(url), &cache), 0);

        assert_eq!(
            net.seen.borrow().len(),
            after_first,
            "the cached sidecar was fetched a second time"
        );
    }

    /// Reports written before #1016 carry no version/target, so the asset
    /// cannot be named. Say that rather than guessing at one.
    #[test]
    fn a_report_without_version_or_target_is_not_guessed_at() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("crashes");
        fs::create_dir_all(&dir).unwrap();
        let report = dir.join("1700000000000-crash.json");
        fs::write(&report, r#"{"backtrace":"0: main\n"}"#).unwrap();
        let cache = tmp.path().join("symbols");
        let net = net_for(b"unused");

        assert_eq!(install_with(&[report], &|url| net.fetch(url), &cache), 1);
        assert!(net.seen.borrow().is_empty());
    }

    /// No reports is a normal state, not a failure, and not a reason to fetch.
    #[test]
    fn no_reports_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let net = net_for(b"unused");

        assert_eq!(install_with(&[], &|url| net.fetch(url), tmp.path()), 0);
        assert!(net.seen.borrow().is_empty());
    }

    /// The URLs are built from the report, not from whatever this binary
    /// happens to be — a report copied from another machine still resolves to
    /// its own release.
    #[test]
    fn the_fetched_urls_name_the_reports_own_release() {
        let tmp = tempfile::tempdir().unwrap();
        let report = write_report(&tmp.path().join("crashes"), "1.2.3", LINUX);
        let cache = tmp.path().join("symbols");
        let net = net_for(b"payload");

        install_with(&[report], &|url| net.fetch(url), &cache);

        let seen = net.seen.borrow();
        assert_eq!(seen[0], checksum_manifest_url("1.2.3"));
        assert_eq!(seen[1], sidecar_url("1.2.3", LINUX).unwrap());
    }
}
