//! The corrupt-index escape hatch: one killable `git ls-files --debug` (#556).
//!
//! [`super::index_pass`] reads `.git/index` in-process and is the fast path
//! (~1–5 ms, no worktree I/O). When the index exists but will not parse —
//! corrupt, truncated, or a format `gix-index` does not support — #556 calls
//! for **one killable `git ls-files --debug` subprocess**, not a fall-through
//! to the full tree walk, which is what the guard was built to avoid.
//!
//! # Why `--debug` and not something nicer
//!
//! `--debug` prints the index's **cached stat size** for each tracked path, so
//! it answers the guard's question without touching the object database. The
//! benchmarks in #551 rejected every ODB route outright:
//!
//! - `ls-files --format='%(objectsize)'` and `ls-tree -l` are 3–12× slower, and
//! - on a **partial clone** a per-blob size lookup can trigger a *network
//!   fetch*. A startup guard that can block on the network is worse than the
//!   problem it reports.
//!
//! So the argv is built by [`ls_files_argv`], a pure function, and a test
//! asserts no ODB-triggering token ever appears in it. That assertion is the
//! point: the danger here is a future edit reaching for `--format` because it
//! parses more cleanly.
//!
//! `--debug`'s output format is documented as unstable, which is exactly why it
//! is the *fallback* rather than the primary. The parser below is deliberately
//! forgiving: anything it cannot interpret yields no entry rather than an
//! error, and an empty result sends the caller on to the walker.

use std::path::{Path, PathBuf};
use std::time::Duration;

use running_process::{
    CommandSpec, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode,
};

/// Wall-clock cap on the fallback. The guard runs before the PTY comes up, so
/// it must never be what makes a launch feel slow: past this the subprocess is
/// killed and the caller falls through to the walker under its own deadline.
pub(super) const LS_FILES_TIMEOUT: Duration = Duration::from_millis(1_500);

/// Tokens that would route the query through the object database.
///
/// Data rather than a comment so the guarantee is testable. `%(objectsize)`
/// and friends are the tempting-but-wrong spelling of this query.
#[cfg(test)]
pub(super) const ODB_TOKENS: &[&str] = &[
    "--format",
    "ls-tree",
    "cat-file",
    "objectsize",
    "--object-only",
    "rev-parse",
];

/// Argv for the fallback. Pure so the ODB-avoidance guarantee is asserted on
/// the argv itself rather than inferred from a live `git`.
pub(super) fn ls_files_argv() -> Vec<String> {
    vec![
        "git".to_string(),
        "ls-files".to_string(),
        "--debug".to_string(),
    ]
}

/// Parse `git ls-files --debug` output into `(path, cached_size)` pairs (the index's cached stat size is a u32).
///
/// The shape is a non-indented path line followed by indented stat lines:
///
/// ```text
/// src/main.rs
///   ctime: 1700000000:0
///   mtime: 1700000000:0
///   dev: 66306 ino: 1234
///   uid: 1000  gid: 1000
///   size: 4096  flags: 0
/// ```
///
/// Forgiving by construction: a path with no `size:` line is skipped rather
/// than defaulted to 0, because 0 is meaningful here — it is the racily-clean
/// marker the index pass routes to verification, and inventing it would put
/// files on that list that were never there.
pub(super) fn parse_ls_files_debug(text: &str) -> Vec<(PathBuf, u32)> {
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented {
            current = Some(line.to_string());
            continue;
        }
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("size:") else {
            continue;
        };
        // "size: 4096  flags: 0" -> the first whitespace-delimited token.
        let Some(size) = rest.split_whitespace().next() else {
            continue;
        };
        let Ok(size) = size.parse::<u32>() else {
            continue;
        };
        if let Some(path) = current.take() {
            out.push((PathBuf::from(path), size));
        }
    }
    out
}

/// Run the fallback against `root`. `None` means it produced nothing usable —
/// git missing, non-zero exit, timeout, or unparseable output — and the caller
/// should continue to the walker.
pub(super) fn ls_files_pass(root: &Path) -> Option<Vec<(PathBuf, u32)>> {
    let process = NativeProcess::new(ProcessConfig {
        command: CommandSpec::Argv(ls_files_argv()),
        cwd: Some(root.to_path_buf()),
        env: None,
        capture: true,
        // Separate pipe, not merged: `--debug` writes the data to stdout, and
        // folding stderr in would let a git warning masquerade as a path line
        // (an unindented line is how the parser recognizes a path).
        stderr_mode: StderrMode::Pipe,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    });
    process.start().ok()?;

    let deadline = std::time::Instant::now() + LS_FILES_TIMEOUT;
    let mut text = String::new();
    loop {
        match process.read_combined(Some(Duration::from_millis(25))) {
            ReadStatus::Line(event) => {
                text.push_str(&String::from_utf8_lossy(&event.line));
                text.push('\n');
            }
            ReadStatus::Eof => break,
            ReadStatus::Timeout => {}
        }
        match process.poll() {
            Ok(Some(_)) => {
                // Drain whatever is still buffered before leaving.
                while let ReadStatus::Line(event) =
                    process.read_combined(Some(Duration::from_millis(5)))
                {
                    text.push_str(&String::from_utf8_lossy(&event.line));
                    text.push('\n');
                }
                break;
            }
            Ok(None) => {}
            Err(_) => return None,
        }
        if std::time::Instant::now() >= deadline {
            // Killable, as #556 requires: a wedged git must not hold a launch.
            let _ = process.kill();
            return None;
        }
    }

    let entries = parse_ls_files_debug(&text);
    (!entries.is_empty()).then_some(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The load-bearing guarantee of this module. A future edit reaching for
    /// `--format='%(objectsize)'` because it parses more cleanly would be 3–12×
    /// slower and, on a partial clone, could block a *startup guard* on a
    /// network fetch.
    #[test]
    fn the_fallback_never_invokes_an_odb_route() {
        let argv = ls_files_argv();
        let joined = argv.join(" ");
        for token in ODB_TOKENS {
            assert!(
                !joined.contains(token),
                "argv {joined:?} contains ODB-triggering token {token:?}"
            );
        }
        assert_eq!(argv, ["git", "ls-files", "--debug"]);
    }

    #[test]
    fn debug_output_yields_paths_and_cached_sizes() {
        let text = "\
src/main.rs
  ctime: 1700000000:0
  mtime: 1700000000:0
  dev: 66306\tino: 1234
  uid: 1000\tgid: 1000
  size: 4096\tflags: 0
assets/big.bin
  ctime: 1700000000:0
  size: 1048576\tflags: 0
";
        let parsed = parse_ls_files_debug(text);
        assert_eq!(
            parsed,
            vec![
                (PathBuf::from("src/main.rs"), 4096),
                (PathBuf::from("assets/big.bin"), 1_048_576u32),
            ]
        );
    }

    /// A path whose `size:` line is missing is skipped, not defaulted to 0.
    /// Zero is *meaningful*: it is the racily-clean marker that routes an entry
    /// to pass-2 verification, so inventing it would put files on that list
    /// that were never there.
    #[test]
    fn a_path_without_a_size_line_is_skipped_not_zeroed() {
        let text = "\
lonely/path.rs
  ctime: 1700000000:0
other/file.rs
  size: 10\tflags: 0
";
        assert_eq!(
            parse_ls_files_debug(text),
            vec![(PathBuf::from("other/file.rs"), 10)]
        );
    }

    /// `--debug`'s format is documented unstable, which is why it is only the
    /// fallback. Garbage must yield nothing rather than panic or half-parse.
    #[test]
    fn unrecognized_output_yields_no_entries_rather_than_failing() {
        assert!(parse_ls_files_debug("").is_empty());
        assert!(parse_ls_files_debug("fatal: not a git repository").is_empty());
        assert!(parse_ls_files_debug("  size: 5\tflags: 0").is_empty());
    }

    /// Paths with spaces are ordinary on Windows and must survive: the path
    /// line is taken whole, never whitespace-split.
    #[test]
    fn a_path_containing_spaces_survives() {
        let text = "My Documents/big file.png\n  size: 99\tflags: 0\n";
        assert_eq!(
            parse_ls_files_debug(text),
            vec![(PathBuf::from("My Documents/big file.png"), 99)]
        );
    }
}
