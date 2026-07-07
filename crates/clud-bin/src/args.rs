use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

use crate::graphics::GraphicsMode;

/// Fast CLI for running Claude Code and Codex in YOLO mode.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "clud",
    version,
    about = "Fast CLI for running Claude Code and Codex in YOLO mode",
    after_help = "Unknown flags are forwarded directly to the backend agent."
)]
pub struct Args {
    #[arg(short = 'p', long = "prompt")]
    pub prompt: Option<String>,

    #[arg(short = 'm', long = "message")]
    pub message: Option<String>,

    #[arg(short = 'c', long = "continue")]
    pub continue_session: bool,

    #[arg(short = 'r', long = "resume")]
    pub resume: Option<Option<String>>,

    #[arg(long = "claude", conflicts_with = "codex")]
    pub claude: bool,

    #[arg(long = "codex", conflicts_with = "claude")]
    pub codex: bool,

    #[arg(long = "subprocess", conflicts_with = "pty")]
    pub subprocess: bool,

    #[arg(long = "pty", conflicts_with = "subprocess")]
    pub pty: bool,

    /// Control terminal graphics headers for PTY sessions.
    #[arg(long = "graphics", value_enum, default_value_t = GraphicsMode::Auto)]
    pub graphics: GraphicsMode,

    /// Render this image as the PTY graphics header when Sixel is enabled.
    #[arg(long = "graphics-image", value_name = "PATH")]
    pub graphics_image: Option<PathBuf>,

    /// Render the bundled README hero image as a standalone Sixel demo and exit.
    #[arg(long = "demo-gfx-sixel")]
    pub demo_gfx_sixel: bool,

    #[arg(long = "model")]
    pub model: Option<String>,

    #[arg(long = "safe")]
    pub safe: bool,

    #[arg(long = "dry-run")]
    pub dry_run: bool,

    #[arg(long = "detach", conflicts_with = "dry_run")]
    pub detach: bool,

    #[arg(long = "detachable", conflicts_with = "dry_run")]
    pub detachable: bool,

    #[arg(long = "name")]
    pub session_name: Option<String>,

    /// Write daemon-managed session output bytes to a transcript file.
    /// Implies centralized daemon execution.
    #[arg(long = "transcript", value_name = "PATH")]
    pub transcript: Option<PathBuf>,

    /// Override the in-memory attach-replay backlog cap. Accepts bytes
    /// (`262144`), or SI/binary suffixes (`256k`, `256KiB`, `1mb`). The
    /// compiled default is 256 KiB. Also honored as `CLUD_BACKLOG_BYTES`.
    #[arg(long = "backlog-size")]
    pub backlog_size: Option<String>,

    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,

    /// Disable the Windows console drag-and-drop target registration.
    /// Issue #79: by default `clud` registers an `IDropTarget` on the
    /// console window so dragged files are forwarded to the backend.
    /// Pass `--no-dnd` to opt out (no-op on POSIX, where drops already
    /// arrive as bracketed-paste stdin bytes).
    #[arg(long = "no-dnd", alias = "no-drag-drop")]
    pub no_dnd: bool,

    /// Issue #83: enumerate this repo's git worktrees and remove the
    /// stale ones. Combine with `--dry-run` to preview, `--yes` to skip
    /// confirmation, `--force` to also remove dirty/unpushed worktrees.
    #[arg(long = "clean-worktrees")]
    pub clean_worktrees: bool,

    /// Inspect Claude/Codex PreToolUse hook parity and apply explicit,
    /// repo-scoped repairs where clud can do so safely.
    #[arg(long = "fix-hooks")]
    pub fix_hooks: bool,

    /// Globally disable automatic deterministic hook-health repairs on launch.
    #[arg(long = "no-fix-hooks", conflicts_with = "fix_hooks")]
    pub no_fix_hooks: bool,

    /// Issue #83: minimum age before a clean worktree is treated as stale.
    /// Accepts `30s`, `5m`, `2h`, `1d`. Defaults to `1d`.
    #[arg(long = "stale-after", default_value = "1d")]
    pub stale_after: String,

    /// Issue #83: skip interactive confirmation prompts (combined with
    /// `--clean-worktrees`).
    #[arg(long = "yes", short = 'y')]
    pub yes: bool,

    /// Issue #83: allow `--clean-worktrees` to remove dirty / unpushed
    /// worktrees. Locked worktrees are still preserved.
    #[arg(long = "force")]
    pub force: bool,

    #[arg(long = "experimental-daemon-centralized", hide = true)]
    pub experimental_daemon_centralized: bool,

    #[arg(long = "daemon-state-dir", hide = true)]
    pub daemon_state_dir: Option<PathBuf>,

    /// Issue #135: reserved for forward compatibility. The merged
    /// always-on clud daemon (`__daemon`) hosts both session ops and the
    /// GC registry, so the prior `--daemon=gc` / `--daemon=session`
    /// distinction is no longer required. Kept as an accepted flag so
    /// older clud invocations don't error.
    #[arg(long = "daemon", value_name = "MODE", hide = true)]
    pub daemon_mode: Option<String>,

    /// Issue #135: opt out of the GC daemon auto-spawn for this invocation.
    /// `clud gc *` operations fail fast with this flag because they
    /// require the daemon. Other code paths skip the spawn silently.
    #[arg(long = "no-daemon")]
    pub no_daemon: bool,

    /// Issue #340: keep env-tagged orphaned descendants alive on exit
    /// (skip the auto-reap; still prints the report unless --quiet-orphans).
    #[arg(long = "keep-orphans")]
    pub keep_orphans: bool,

    /// Issue #340: suppress the orphan-reaper surprise report on exit.
    /// Reaping still happens unless --keep-orphans is also set.
    #[arg(long = "quiet-orphans")]
    pub quiet_orphans: bool,

    /// Issue #340: dump the relevant env vars for each detected orphan
    /// alongside the report, to help author allowlist rules.
    #[arg(long = "explain-orphans")]
    pub explain_orphans: bool,

    /// Issue #466: suppress the foreground CPU-burn banner for this
    /// invocation. Banner is otherwise on by default; it polls the subtree
    /// CPU every 2 s and emits `[clud] cpu N % …` to stderr when subtree
    /// CPU crosses `max(50 %, 0.20 × num_cpus × 100 %)` for 3 sustained
    /// ticks. Permanent opt-out via `[foreground.cpu_banner] enabled =
    /// false` in `~/.clud/settings.json`.
    #[arg(long = "no-cpu-banner")]
    pub no_cpu_banner: bool,

    #[command(subcommand)]
    pub command: Option<Command>,

    #[arg(last = true, id = "BACKEND_ARGS")]
    pub passthrough: Vec<String>,

    /// Runtime Codex `-c` config overrides loaded from ~/.clud/settings.json.
    #[arg(skip)]
    pub codex_config_overrides: Vec<String>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    Loop {
        /// Prompt text, path to a local file, or a GH issue/PR URL.
        task: Option<String>,
        #[arg(long = "loop-count", default_value = "50")]
        loop_count: u32,
        /// Force re-fetch of a cached GH issue/PR body.
        #[arg(long = "refresh")]
        refresh: bool,
        /// Do not inject the DONE/BLOCKED marker contract into the prompt.
        #[arg(long = "no-done", alias = "no-done-marker", conflicts_with = "done")]
        no_done: bool,
        /// Re-enable the DONE/BLOCKED contract using a custom DONE marker path.
        #[arg(long = "done", conflicts_with = "no_done")]
        done: Option<String>,
        /// Re-run the loop after it completes, sleeping for the given duration
        /// between runs (for example `30s`, `5m`, `1h`).
        #[arg(long = "repeat")]
        repeat: Option<String>,
    },
    Up {
        #[arg(short = 'm', long = "message")]
        message: Option<String>,
        #[arg(long = "publish")]
        publish: bool,
    },
    Rebase,
    Fix {
        url: Option<String>,
    },
    Wasm {
        module: String,
        #[arg(long = "invoke", default_value = "run")]
        invoke: String,
    },
    Attach {
        session_id: Option<String>,
        #[arg(long = "last", short = 'l')]
        last: bool,
    },
    Kill {
        session_id: Option<String>,
        #[arg(long = "all")]
        all: bool,
    },
    /// Kill all active background sessions.
    Slay,
    List,
    /// Print current clud daemon CPU metrics.
    Top {
        /// Emit machine-readable JSON.
        #[arg(long = "json")]
        json: bool,
    },
    /// pm2-style log viewer: dump or tail a session's captured output.
    ///
    /// With no session id, lists all sessions that have log files and prints
    /// the last line of each. With an id, prints the log (last `--lines` or
    /// all) and optionally keeps following new output via `--follow`.
    /// `--last` resolves to the most-recently-created session (live or
    /// exited), mirroring `clud attach --last`. Read-only: never takes
    /// exclusive ownership of the session and never evicts attached clients.
    Logs {
        session_id: Option<String>,
        /// Keep watching the file and print new output as it arrives. Exits
        /// once the session has terminated (after printing a status line).
        #[arg(long = "follow", short = 'f')]
        follow: bool,
        /// Print only the last N lines from the file. Default: all.
        #[arg(long = "lines", short = 'n')]
        lines: Option<usize>,
        /// Operate on the most recently created session (live or exited).
        #[arg(long = "last", short = 'l', conflicts_with = "session_id")]
        last: bool,
    },
    /// Issue #110: tracked-entry garbage collection (redb-backed
    /// registry at `~/.clud/data.redb`).
    ///
    /// Subcommands: `list`, `prune`, `purge`, `all`, `reconcile`. Running
    /// `clud gc` with no subcommand prints this help summary.
    Gc {
        #[command(subcommand)]
        subcommand: Option<GcSubcommand>,
    },
    /// Issue #183: open the local web dashboard served by the always-on
    /// clud daemon. Shows live sessions, garbage tracking, and the repos
    /// clud has been launched in. Loopback only.
    Ui {
        /// Print `/state.json` to stdout and exit without launching a browser.
        #[arg(long = "json")]
        json: bool,
        /// Print the dashboard URL and ensure the daemon is up, but do
        /// not launch a browser. Handy when running on a headless host.
        #[arg(long = "no-open")]
        no_open: bool,
    },
    /// Quarantine paths under ~/.clud/trash and let daemon GC reap them.
    Trash {
        /// Allow copy + best-effort source removal when source and trash
        /// live on different volumes.
        #[arg(long = "cross-volume")]
        cross_volume: bool,
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
    },
    /// Run and inspect bundled clud tools without starting the daemon.
    Tool {
        #[command(subcommand)]
        subcommand: ToolSubcommand,
    },
    /// Issue #469 (beta prototype): POST one telemetry event to the
    /// always-on clud daemon's HTTP server. Captures parent PID, time,
    /// the `cmd` string passed in, the current working directory, and
    /// every env var beginning with `CLUD_`. The daemon URL is read
    /// from `$CLUD_DAEMON_HTTP_SERVER`. By default missing env / unreachable
    /// daemon are silent (exit 0) so a hook caller is never broken; with
    /// `--fail-on-no-server` either failure causes a non-zero exit so
    /// tests can prove a real round-trip.
    Log {
        /// Free-form command string describing what the caller was doing.
        /// Stored verbatim in the telemetry record.
        #[arg(long = "cmd", short = 'c')]
        cmd: String,
        /// Exit non-zero if `CLUD_DAEMON_HTTP_SERVER` is unset OR the
        /// POST fails. Without this flag, failures are swallowed.
        #[arg(long = "fail-on-no-server")]
        fail_on_no_server: bool,
    },
    /// Install and persist fast local tooling defaults.
    Optimize {
        /// Toolchain family to optimize. Defaults to Rust.
        #[arg(value_enum, default_value_t = OptimizeTarget::Rust)]
        target: OptimizeTarget,
        /// Persist the recommendation in ~/.clud/settings.json.
        #[arg(long = "global", conflicts_with = "repo")]
        global: bool,
        /// Write a repo-local .clud/settings.json directive.
        #[arg(long = "repo", conflicts_with = "global")]
        repo: bool,
        /// Install soldr if it is missing from PATH.
        #[arg(
            long = "install-soldr",
            default_value_t = true,
            action = ArgAction::Set,
            num_args = 0..=1,
            default_missing_value = "true",
            value_parser = clap::value_parser!(bool),
        )]
        install_soldr: bool,
        /// Enable soldr shims for future clud-managed Rust setup.
        #[arg(
            long = "use-soldr-shims",
            default_value_t = true,
            action = ArgAction::Set,
            num_args = 0..=1,
            default_missing_value = "true",
            value_parser = clap::value_parser!(bool),
        )]
        use_soldr_shims: bool,
        /// soldr release version to install and persist.
        #[arg(long = "soldr-version", default_value = "0.7.11")]
        soldr_version: String,
    },
    /// Control the always-on clud daemon.
    Daemon {
        #[command(subcommand)]
        subcommand: DaemonSubcommand,
    },
    /// Inspect or verify crash-report symbolication (#374 PR 3/3).
    ///
    /// clud builds with `debug = "line-tables-only"` embed every line
    /// table in the binary itself, so there are no sidecar files to
    /// install. This subcommand is an opportunistic verifier that
    /// confirms the running binary can symbolicate recent crash reports
    /// in `~/.clud/state/crashes/`. `clud symbols` (bare) prints a
    /// summary; `clud symbols install` and `clud symbols verify` exit 1
    /// when any inspected report is unsymbolicated.
    Symbols {
        #[command(subcommand)]
        subcommand: Option<SymbolsSubcommand>,
    },
    #[command(name = "__daemon", hide = true)]
    InternalDaemon {
        #[arg(long = "state-dir")]
        state_dir: PathBuf,
    },
    #[command(name = "__worker", hide = true)]
    InternalWorker {
        #[arg(long = "state-dir")]
        state_dir: PathBuf,
        #[arg(long = "session-id")]
        session_id: String,
        #[arg(long = "daemon-pid")]
        daemon_pid: u32,
        #[arg(long = "spec-file")]
        spec_file: PathBuf,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptimizeTarget {
    #[value(alias = "soldr")]
    Rust,
}

/// Subcommands under `clud symbols`. See `crates/clud-bin/src/symbols.rs`.
#[derive(Subcommand, Debug, Clone)]
pub enum SymbolsSubcommand {
    /// Verify that the running binary's embedded line tables resolve
    /// recent crash report backtraces. With the embed-everywhere
    /// strategy, this is a no-op when symbols are already present and
    /// a diagnostic when they're not. Exits 1 if any inspected report
    /// is unsymbolicated.
    Install,
    /// Same as `install` but with an explicit `--all` toggle.
    Verify {
        /// Verify every report under `~/.clud/state/crashes/` rather
        /// than just the most-recent one.
        #[arg(long = "all")]
        all: bool,
    },
}

/// Subcommands under `clud daemon`. See `crates/clud-bin/src/daemon/`.
#[derive(Subcommand, Debug, Clone)]
pub enum DaemonSubcommand {
    /// Restart the daemon process so the next CLI call uses the current binary.
    Restart,
    /// Print the current running-process adoption preview.
    #[command(name = "running-process", alias = "servicedef")]
    RunningProcess {
        /// Emit machine-readable JSON.
        #[arg(long = "json")]
        json: bool,
    },
}

/// Subcommands under `clud tool`. See `crates/clud-bin/src/tool_run.rs`.
#[derive(Subcommand, Debug, Clone)]
pub enum ToolSubcommand {
    /// Invoke a bundled tool by its `~/.clud/tools/`-relative path,
    /// forwarding any trailing args to the tool. Example:
    /// `clud tool run github/pr_merge_watch.py 404 --interval 30`.
    Run {
        /// Path under `~/.clud/tools/` (e.g. `github/pr_merge_watch.py`).
        rel_path: String,
        /// Arguments forwarded verbatim to the tool. Use `--` to pass flags
        /// the clud parser would otherwise consume.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// List tool invocations in the current clud session — slice 3 of #427.
    List {
        /// Emit a JSON array instead of the human-readable table.
        #[arg(long = "json")]
        json: bool,
        /// Show the long-form `<session-pid>-<tool-id>` ID in the table.
        #[arg(long = "long")]
        long: bool,
    },
    /// Query the full JSONL log of one invocation with optional filters
    /// — slice 4 of #427.
    Log {
        /// Reference to the invocation. Same forms as `tool info`.
        reference: Option<String>,
        /// Look up by the tool's own OS PID instead of session-local id.
        #[arg(long = "pid")]
        pid: Option<u32>,
        /// Which stream to read: `stdout`, `stderr`, or `combined` (default).
        #[arg(long = "stream", default_value = "combined")]
        stream: String,
        /// Only entries newer than `now - <duration>` (e.g. `5m`, `1h`).
        #[arg(long = "since")]
        since: Option<String>,
        /// Only entries older than `now - <duration>`.
        #[arg(long = "until")]
        until: Option<String>,
        /// Absolute time range as two integer epoch-ms values.
        #[arg(long = "between", number_of_values = 2)]
        between: Option<Vec<String>>,
        /// Substring match on the decoded line text.
        #[arg(long = "grep")]
        grep: Option<String>,
        /// Show only the first N matching entries.
        #[arg(long = "head")]
        head: Option<usize>,
        /// Show only the last N matching entries.
        #[arg(long = "tail")]
        tail: Option<usize>,
        /// Emit the raw JSONL stream instead of decoded text.
        #[arg(long = "json")]
        json: bool,
    },
    /// History of tool invocations matching optional filters — slice 4
    /// of #427.
    Ledger {
        /// Restrict to one tool name.
        #[arg(long = "tool")]
        tool: Option<String>,
        /// Session scope: `current` (default), `previous`, or `all`.
        #[arg(long = "session", default_value = "current")]
        session: String,
        /// Emit a JSON array instead of the human-readable table.
        #[arg(long = "json")]
        json: bool,
    },
    /// Show current state + last N lines of stdout/stderr for one
    /// invocation — slice 3 of #427.
    Info {
        /// Reference to the invocation. Accepts:
        /// * a bare session-local integer (`3`)
        /// * a long-form `<session-pid>-<tool-id>` (`47180-3`)
        /// * `@<tool-name>` or `@<tool-name>:N` for N-th-most-recent
        ///
        /// Omit to default to the most recently started invocation.
        reference: Option<String>,
        /// Look up by the tool's own OS PID instead of the session-local
        /// integer. Bare integers always mean tool-id, not PID — use this
        /// flag to disambiguate.
        #[arg(long = "pid")]
        pid: Option<u32>,
        /// Number of trailing stdout/stderr lines to show per stream.
        #[arg(long = "lines", default_value_t = 20)]
        lines: usize,
        /// Emit a JSON object instead of the human-readable view.
        #[arg(long = "json")]
        json: bool,
    },
}

/// Subcommands under `clud gc`. See `crates/clud-bin/src/gc/`.
#[derive(Subcommand, Debug, Clone)]
pub enum GcSubcommand {
    /// Print tracked entries, newest first.
    List {
        /// Issue #135: emit a JSON array instead of the human-readable table.
        #[arg(long = "json")]
        json: bool,
        /// Restrict to a single managed kind (e.g. `worktree`, `trash`).
        #[arg(long = "kind")]
        kind: Option<String>,
    },
    /// Drop stale/unreferenced entries for one managed kind.
    Prune {
        /// Preview the removal plan without touching anything.
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Managed kind to prune (e.g. `worktree`, `uv-cache`, `trash`).
        #[arg(long = "kind")]
        kind: Option<String>,
    },
    /// Remove all entries for one managed kind. Destructive; requires `--yes`.
    Purge {
        /// Preview the removal plan without touching anything.
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Skip the interactive confirmation prompt.
        #[arg(long = "yes", short = 'y')]
        yes: bool,
        /// Managed kind to purge (e.g. `worktree`, `uv-cache`, `trash`).
        #[arg(long = "kind")]
        kind: Option<String>,
    },
    /// Operate across every managed kind. Defaults to safe prune.
    All {
        /// Purge every managed kind instead of pruning stale entries.
        #[arg(long = "purge")]
        purge: bool,
        /// Preview the removal plan without touching anything.
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Required with `--purge`.
        #[arg(long = "yes", short = 'y')]
        yes: bool,
    },
    /// Walk `.claude/worktrees/` in the current repo and insert any
    /// previously-untracked worktree directories.
    Reconcile,
}

impl Args {
    pub fn parse_with_passthrough() -> Self {
        let raw: Vec<String> = std::env::args().collect();
        Self::parse_from_raw(raw)
    }

    pub fn parse_from_raw(raw: Vec<String>) -> Self {
        let (known, unknown) = split_known_unknown(&raw);
        let mut args = Args::parse_from(known);
        args.passthrough.extend(unknown);
        args
    }
}

fn split_known_unknown(raw: &[String]) -> (Vec<String>, Vec<String>) {
    let mut known = vec![raw[0].clone()];
    let mut unknown = Vec::new();
    let mut i = 1;

    let value_flags: &[&str] = &[
        "--prompt",
        "--message",
        "--resume",
        "--model",
        "--name",
        "--transcript",
        "--backlog-size",
        "--graphics",
        "--graphics-image",
        "--loop-count",
        "--done",
        "--repeat",
        "--daemon-state-dir",
        "--stale-after",
        "--daemon",
        "--state-dir",
        // Issue #469: `clud log --cmd "..."` arg.
        "--cmd",
    ];
    let short_value_flags: &[&str] = &["-p", "-m", "-r"];
    let bool_flags: &[&str] = &[
        "--continue",
        "--claude",
        "--codex",
        "--subprocess",
        "--pty",
        "--safe",
        "--dry-run",
        "--detach",
        "--detachable",
        "--verbose",
        "--experimental-daemon-centralized",
        "--all",
        "--last",
        "--refresh",
        "--no-done",
        "--no-done-marker",
        "--no-dnd",
        "--no-drag-drop",
        "--clean-worktrees",
        "--fix-hooks",
        "--no-fix-hooks",
        "--yes",
        "--force",
        "--no-daemon",
        "--keep-orphans",
        "--quiet-orphans",
        "--explain-orphans",
        "--json",
        "--no-open",
        "--demo-gfx-sixel",
        "--help",
        "--version",
        // Issue #469: `clud log --fail-on-no-server` bool flag.
        "--fail-on-no-server",
    ];
    let short_bool_flags: &[&str] = &["-c", "-v", "-h", "-V", "-y"];
    let subcommands: &[&str] = &[
        "loop", "up", "rebase", "fix", "wasm", "attach", "kill", "slay", "list", "top", "logs",
        "log", "gc", "ui", "trash", "tool", "optimize", "daemon", "symbols", "__daemon",
        "__worker",
    ];

    let mut in_subcommand = false;

    while i < raw.len() {
        let arg = &raw[i];

        if arg == "--" {
            unknown.extend_from_slice(&raw[i + 1..]);
            break;
        }

        if in_subcommand {
            known.push(arg.clone());
            i += 1;
            continue;
        }

        if subcommands.contains(&arg.as_str()) {
            known.push(arg.clone());
            in_subcommand = true;
            i += 1;
            continue;
        }

        if bool_flags.contains(&arg.as_str()) || short_bool_flags.contains(&arg.as_str()) {
            known.push(arg.clone());
            i += 1;
            continue;
        }

        if arg.starts_with("--") {
            if let Some((prefix, _)) = arg.split_once('=') {
                if value_flags.contains(&prefix) {
                    known.push(arg.clone());
                    i += 1;
                    continue;
                }
            }
        }

        if value_flags.contains(&arg.as_str()) || short_value_flags.contains(&arg.as_str()) {
            known.push(arg.clone());
            i += 1;
            if i < raw.len() {
                known.push(raw[i].clone());
            }
            i += 1;
            continue;
        }

        unknown.push(arg.clone());
        i += 1;
    }

    (known, unknown)
}

#[cfg(test)]
#[path = "args_tests.rs"]
mod tests;
