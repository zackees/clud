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

use std::collections::BTreeMap;

use crate::process_tree;

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
}

/// One descendant's view, pre-classification.
struct Descendant {
    pid: u32,
    name: String,
    command: String,
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
    let all = running_process::originator::find_processes_by_originator("CLUD");

    // Only act on descendants whose originator points at *us*. Anything
    // pointing at a different CLUD:<pid> belongs to a concurrent clud
    // invocation and is not ours to touch.
    let mine: Vec<Descendant> = all
        .into_iter()
        .filter(|p| p.parent_pid == self_pid)
        .map(|p| Descendant {
            pid: p.pid,
            name: p.name,
            command: p.command,
        })
        .collect();

    let header = format!("[clud] orphan scan on exit (originator=CLUD:{self_pid}):");
    report_and_reap(mine, &header, opts)
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
    let all = running_process::originator::find_processes_by_originator("CLUD");
    let orphans: Vec<Descendant> = all
        .into_iter()
        .filter(|p| !p.parent_alive)
        .map(|p| Descendant {
            pid: p.pid,
            name: p.name,
            command: p.command,
        })
        .collect();

    let header = "[clud] orphan sweep (dead originator):".to_string();
    report_and_reap(orphans, &header, opts)
}

/// Shared classify / report / kill body for both entry points. Returns a
/// default outcome when `descendants` is empty so callers can skip noise.
fn report_and_reap(descendants: Vec<Descendant>, header: &str, opts: &ReapOpts) -> ReapOutcome {
    let found = descendants.len();
    if found == 0 {
        return ReapOutcome::default();
    }

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
    }

    if opts.keep {
        return ReapOutcome { found, reaped: 0 };
    }

    let mut reaped = 0usize;
    for d in &descendants {
        process_tree::kill_tree(d.pid);
        reaped += 1;
    }

    if !opts.quiet {
        eprintln!("[clud] reaped {reaped} of {found} env-tagged descendant(s)");
    }

    ReapOutcome { found, reaped }
}

#[cfg(test)]
mod tests {
    use super::*;

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
