use clap::{Parser, Subcommand};
use std::path::PathBuf;

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

    #[command(subcommand)]
    pub command: Option<Command>,

    #[arg(last = true, id = "BACKEND_ARGS")]
    pub passthrough: Vec<String>,
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
    List,
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
    /// Subcommands: `list`, `purge <duration>`, `reconcile`. Running
    /// `clud gc` with no subcommand prints this help summary.
    Gc {
        #[command(subcommand)]
        subcommand: Option<GcSubcommand>,
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

/// Subcommands under `clud gc`. See `crates/clud-bin/src/gc.rs`.
#[derive(Subcommand, Debug, Clone)]
pub enum GcSubcommand {
    /// Print every tracked entry, newest first.
    List {
        /// Issue #135: emit a JSON array instead of the human-readable table.
        #[arg(long = "json")]
        json: bool,
    },
    /// Remove tracked entries older than `<duration>`. When `<duration>`
    /// is omitted, purge ALL tracked entries that are not live-locked.
    Purge {
        /// Duration (e.g. `30s`, `5m`, `2h`, `1d`). When omitted, purge
        /// every non-live-locked entry regardless of age.
        duration: Option<String>,
        /// Preview the removal plan without touching anything.
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Skip the interactive confirmation prompt.
        #[arg(long = "yes", short = 'y')]
        yes: bool,
        /// Restrict to a single entry kind (e.g. `worktree`).
        #[arg(long = "kind")]
        kind: Option<String>,
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
        "--loop-count",
        "--done",
        "--repeat",
        "--daemon-state-dir",
        "--stale-after",
        "--daemon",
        "--state-dir",
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
        "--yes",
        "--force",
        "--no-daemon",
        "--json",
        "--help",
        "--version",
    ];
    let short_bool_flags: &[&str] = &["-c", "-v", "-h", "-V", "-y"];
    let subcommands: &[&str] = &[
        "loop", "up", "rebase", "fix", "wasm", "attach", "kill", "list", "logs", "gc", "__daemon",
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
