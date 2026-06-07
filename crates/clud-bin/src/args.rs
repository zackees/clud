use clap::{Parser, Subcommand};
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
    /// Issue #259: MCP stdio bridge for Claude Code / Codex. Forwards
    /// JSON-RPC frames between stdio and the in-daemon memory MCP
    /// server's loopback TCP port. Transparently brings up the daemon if
    /// it isn't running.
    Mcp,
    /// Quarantine paths under ~/.clud/trash and let daemon GC reap them.
    Trash {
        /// Allow copy + best-effort source removal when source and trash
        /// live on different volumes.
        #[arg(long = "cross-volume")]
        cross_volume: bool,
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
    },
    /// Control the always-on clud daemon.
    Daemon {
        #[command(subcommand)]
        subcommand: DaemonSubcommand,
    },
    /// Issue #262: agent-memory CLI verbs. The daemon owns the on-disk
    /// SQLite + tantivy + embedder; the CLI proxies mutating ops through
    /// the daemon's HTTP routes so there's exactly one SQLite writer per
    /// process. Bare `clud memory` prints help.
    Memory {
        #[command(subcommand)]
        subcommand: Option<MemorySubcommand>,
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

/// Subcommands under `clud daemon`.
#[derive(Subcommand, Debug, Clone)]
pub enum DaemonSubcommand {
    /// Restart the daemon process so the next CLI call uses the current binary.
    Restart,
}

/// Subcommands under `clud memory`. See `crates/clud-bin/src/memory/cli.rs`.
#[derive(Subcommand, Debug, Clone)]
pub enum MemorySubcommand {
    /// One-time schema init + best-effort embedder warm. Prints resolved
    /// paths, embed_dim, and embedder name.
    Init,
    /// Print tier row counts, embedder status, schema user_version, db path,
    /// and the daemon's consolidation cadence.
    Status {
        #[arg(long = "json")]
        json: bool,
    },
    /// Hybrid (BM25 + KNN) search via RRF. `--tier-floor` filters out
    /// rows below the requested tier.
    Search {
        query: String,
        #[arg(short = 'k', long = "k", default_value = "10")]
        k: u32,
        #[arg(long = "session-id", value_name = "SID")]
        session_id: Option<String>,
        #[arg(long = "tier-floor", value_name = "TIER")]
        tier_floor: Option<String>,
        #[arg(long = "scope-key", value_name = "KEY")]
        scope_key: Option<String>,
        #[arg(long = "json")]
        json: bool,
    },
    /// Embed and insert a new memory row.
    Save {
        content: String,
        #[arg(long = "tier", default_value = "working")]
        tier: String,
        #[arg(long = "session-id", value_name = "SID")]
        session_id: Option<String>,
        #[arg(long = "metadata", value_name = "JSON")]
        metadata: Option<String>,
        #[arg(long = "json")]
        json: bool,
    },
    /// Delete one memory by id (cascades to memory_vec + tantivy).
    Forget {
        id: String,
        #[arg(long = "json")]
        json: bool,
    },
    /// Export rows as JSON-lines (stdout) or as a `.clud/memory/` tree
    /// of YAML-frontmatter Markdown files (`--to-disk`, #264).
    Export {
        #[arg(long = "to-disk", conflicts_with = "to_stdout")]
        to_disk: bool,
        #[arg(long = "to-stdout")]
        to_stdout: bool,
        /// Widen the tier policy so Episodic rows are also exported.
        /// Honored only with `--to-disk`. Mirrors
        /// `CLUD_MEMORY_EXPORT_EPISODIC=1`.
        #[arg(long = "include-episodic")]
        include_episodic: bool,
        /// Disable the `.cludignore` + `private:` privacy filter when
        /// writing to disk. Honored only with `--to-disk`.
        #[arg(long = "allow-private")]
        allow_private: bool,
    },
    /// Import rows from JSON-lines (`--from-stdin`) or from a
    /// `.clud/memory/` tree (`--from-disk`, #264).
    Import {
        #[arg(long = "from-disk", conflicts_with = "from_stdin")]
        from_disk: bool,
        #[arg(long = "from-stdin")]
        from_stdin: bool,
        /// Also pull in `<root>/episodic/` files. Honored only with
        /// `--from-disk`.
        #[arg(long = "include-episodic")]
        include_episodic: bool,
    },
    /// Open the dashboard in a browser at the `#memory` anchor.
    Ui {
        #[arg(long = "no-open")]
        no_open: bool,
    },
    /// Re-embed every stored row using the currently-configured embedder.
    Reembed {
        #[arg(long = "model", value_name = "NAME")]
        model: Option<String>,
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
    /// Write the branch-isolate marker so this branch keeps memories
    /// private from main.
    BranchIsolate,
    /// Remove the branch-isolate marker.
    BranchUnisolate,
}

/// Subcommands under `clud gc`. See `crates/clud-bin/src/gc.rs`.
#[derive(Subcommand, Debug, Clone)]
pub enum GcSubcommand {
    /// Print every tracked entry, newest first.
    List {
        /// Issue #135: emit a JSON array instead of the human-readable table.
        #[arg(long = "json")]
        json: bool,
        /// Restrict to a single entry kind (e.g. `worktree`, `trash`).
        #[arg(long = "kind")]
        kind: Option<String>,
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
        "--graphics",
        "--graphics-image",
        "--loop-count",
        "--done",
        "--repeat",
        "--daemon-state-dir",
        "--stale-after",
        "--daemon",
        "--state-dir",
        "--session-id",
        "--tier",
        "--tier-floor",
        "--scope-key",
        "--metadata",
        "--k",
    ];
    let short_value_flags: &[&str] = &["-p", "-m", "-r", "-k"];
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
        "--no-open",
        "--demo-gfx-sixel",
        "--to-disk",
        "--to-stdout",
        "--from-disk",
        "--from-stdin",
        "--help",
        "--version",
    ];
    let short_bool_flags: &[&str] = &["-c", "-v", "-h", "-V", "-y"];
    let subcommands: &[&str] = &[
        "loop", "up", "rebase", "fix", "wasm", "attach", "kill", "list", "logs", "gc", "ui", "mcp",
        "memory", "trash", "daemon", "__daemon", "__worker",
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
