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

    /// Override the in-memory attach-replay backlog cap. Accepts bytes
    /// (`262144`), or SI/binary suffixes (`256k`, `256KiB`, `1mb`). The
    /// compiled default is 256 KiB. Also honored as `CLUD_BACKLOG_BYTES`.
    #[arg(long = "backlog-size")]
    pub backlog_size: Option<String>,

    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,

    #[arg(long = "experimental-daemon-centralized", hide = true)]
    pub experimental_daemon_centralized: bool,

    #[arg(long = "daemon-state-dir", hide = true)]
    pub daemon_state_dir: Option<PathBuf>,

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
    Logs {
        session_id: Option<String>,
        /// Keep watching the file and print new output as it arrives.
        #[arg(long = "follow", short = 'f')]
        follow: bool,
        /// Print only the last N lines from the file. Default: all.
        #[arg(long = "lines", short = 'n')]
        lines: Option<usize>,
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
        "--backlog-size",
        "--loop-count",
        "--done",
        "--repeat",
        "--daemon-state-dir",
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
        "--help",
        "--version",
    ];
    let short_bool_flags: &[&str] = &["-c", "-v", "-h", "-V"];
    let subcommands: &[&str] = &[
        "loop", "up", "rebase", "fix", "wasm", "attach", "kill", "list", "logs", "__daemon",
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
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Args {
        let raw: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        Args::parse_from_raw(raw)
    }

    #[test]
    fn test_prompt_flag() {
        let args = parse(&["clud", "-p", "hello world"]);
        assert_eq!(args.prompt.as_deref(), Some("hello world"));
        assert!(!args.safe);
    }

    #[test]
    fn test_message_flag() {
        let args = parse(&["clud", "-m", "fix the bug"]);
        assert_eq!(args.message.as_deref(), Some("fix the bug"));
    }

    #[test]
    fn test_continue_flag() {
        let args = parse(&["clud", "-c"]);
        assert!(args.continue_session);
    }

    #[test]
    fn test_claude_backend() {
        let args = parse(&["clud", "--claude"]);
        assert!(args.claude);
        assert!(!args.codex);
    }

    #[test]
    fn test_codex_backend() {
        let args = parse(&["clud", "--codex"]);
        assert!(args.codex);
        assert!(!args.claude);
    }

    #[test]
    fn test_model_flag() {
        let args = parse(&["clud", "--model", "opus"]);
        assert_eq!(args.model.as_deref(), Some("opus"));
    }

    #[test]
    fn test_subprocess_flag() {
        let args = parse(&["clud", "--subprocess"]);
        assert!(args.subprocess);
        assert!(!args.pty);
    }

    #[test]
    fn test_pty_flag() {
        let args = parse(&["clud", "--pty"]);
        assert!(args.pty);
        assert!(!args.subprocess);
    }

    #[test]
    fn test_safe_flag() {
        let args = parse(&["clud", "--safe", "-p", "hello"]);
        assert!(args.safe);
        assert_eq!(args.prompt.as_deref(), Some("hello"));
    }

    #[test]
    fn test_dry_run() {
        let args = parse(&["clud", "--dry-run", "-p", "hello"]);
        assert!(args.dry_run);
    }

    #[test]
    fn test_detach_flag() {
        let args = parse(&["clud", "--detach", "-p", "hello"]);
        assert!(args.detach);
        assert!(!args.detachable);
    }

    #[test]
    fn test_detachable_flag() {
        let args = parse(&["clud", "--detachable", "-p", "hello"]);
        assert!(args.detachable);
        assert!(!args.detach);
    }

    #[test]
    fn test_loop_subcommand() {
        let args = parse(&["clud", "loop", "do the task"]);
        match args.command {
            Some(Command::Loop {
                ref task,
                loop_count,
                refresh,
                no_done,
                ref done,
                ref repeat,
            }) => {
                assert_eq!(task.as_deref(), Some("do the task"));
                assert_eq!(loop_count, 50);
                assert!(!refresh);
                assert!(!no_done);
                assert!(done.is_none());
                assert!(repeat.is_none());
            }
            _ => panic!("expected Loop subcommand"),
        }
    }

    #[test]
    fn test_loop_with_count() {
        let args = parse(&["clud", "loop", "--loop-count", "5", "task"]);
        match args.command {
            Some(Command::Loop {
                ref task,
                loop_count,
                ..
            }) => {
                assert_eq!(task.as_deref(), Some("task"));
                assert_eq!(loop_count, 5);
            }
            _ => panic!("expected Loop subcommand"),
        }
    }

    #[test]
    fn test_loop_refresh_flag() {
        let args = parse(&[
            "clud",
            "loop",
            "--refresh",
            "https://github.com/o/r/issues/42",
        ]);
        match args.command {
            Some(Command::Loop {
                ref task,
                refresh,
                no_done,
                ref done,
                ref repeat,
                ..
            }) => {
                assert_eq!(task.as_deref(), Some("https://github.com/o/r/issues/42"));
                assert!(refresh);
                assert!(!no_done);
                assert!(done.is_none());
                assert!(repeat.is_none());
            }
            _ => panic!("expected Loop subcommand"),
        }
    }

    #[test]
    fn test_loop_no_done_flag() {
        let args = parse(&["clud", "loop", "--no-done", "task"]);
        match args.command {
            Some(Command::Loop { no_done, .. }) => {
                assert!(no_done);
            }
            _ => panic!("expected Loop subcommand"),
        }
    }

    #[test]
    fn test_loop_no_done_marker_compat_alias() {
        let args = parse(&["clud", "loop", "--no-done-marker", "task"]);
        match args.command {
            Some(Command::Loop { no_done, .. }) => {
                assert!(no_done);
            }
            _ => panic!("expected Loop subcommand"),
        }
    }

    #[test]
    fn test_loop_done_path() {
        let args = parse(&["clud", "loop", "--done", "DONE.md", "task"]);
        match args.command {
            Some(Command::Loop {
                ref done, no_done, ..
            }) => {
                assert_eq!(done.as_deref(), Some("DONE.md"));
                assert!(!no_done);
            }
            _ => panic!("expected Loop subcommand"),
        }
    }

    #[test]
    fn test_loop_repeat() {
        let args = parse(&["clud", "loop", "--repeat", "1h", "task"]);
        match args.command {
            Some(Command::Loop { ref repeat, .. }) => {
                assert_eq!(repeat.as_deref(), Some("1h"));
            }
            _ => panic!("expected Loop subcommand"),
        }
    }

    #[test]
    fn test_up_subcommand() {
        let args = parse(&["clud", "up"]);
        assert!(matches!(args.command, Some(Command::Up { .. })));
    }

    #[test]
    fn test_up_with_message() {
        let args = parse(&["clud", "up", "-m", "bump version"]);
        match args.command {
            Some(Command::Up {
                ref message,
                publish,
            }) => {
                assert_eq!(message.as_deref(), Some("bump version"));
                assert!(!publish);
            }
            _ => panic!("expected Up subcommand"),
        }
    }

    #[test]
    fn test_up_with_publish() {
        let args = parse(&["clud", "up", "--publish"]);
        match args.command {
            Some(Command::Up {
                ref message,
                publish,
            }) => {
                assert!(message.is_none());
                assert!(publish);
            }
            _ => panic!("expected Up subcommand"),
        }
    }

    #[test]
    fn test_up_with_message_and_publish() {
        let args = parse(&["clud", "up", "-m", "release", "--publish"]);
        match args.command {
            Some(Command::Up {
                ref message,
                publish,
            }) => {
                assert_eq!(message.as_deref(), Some("release"));
                assert!(publish);
            }
            _ => panic!("expected Up subcommand"),
        }
    }

    #[test]
    fn test_rebase_subcommand() {
        let args = parse(&["clud", "rebase"]);
        assert!(matches!(args.command, Some(Command::Rebase)));
    }

    #[test]
    fn test_fix_subcommand() {
        let args = parse(&["clud", "fix"]);
        assert!(matches!(args.command, Some(Command::Fix { .. })));
    }

    #[test]
    fn test_fix_with_url() {
        let args = parse(&[
            "clud",
            "fix",
            "https://github.com/user/repo/actions/runs/123",
        ]);
        match args.command {
            Some(Command::Fix { ref url }) => {
                assert_eq!(
                    url.as_deref(),
                    Some("https://github.com/user/repo/actions/runs/123")
                );
            }
            _ => panic!("expected Fix subcommand"),
        }
    }

    #[test]
    fn test_wasm_subcommand() {
        let args = parse(&["clud", "wasm", "guest.wasm"]);
        match args.command {
            Some(Command::Wasm {
                ref module,
                ref invoke,
            }) => {
                assert_eq!(module, "guest.wasm");
                assert_eq!(invoke, "run");
            }
            _ => panic!("expected Wasm subcommand"),
        }
    }

    #[test]
    fn test_wasm_subcommand_custom_entrypoint() {
        let args = parse(&["clud", "wasm", "guest.wasm", "--invoke", "_start"]);
        match args.command {
            Some(Command::Wasm {
                ref module,
                ref invoke,
            }) => {
                assert_eq!(module, "guest.wasm");
                assert_eq!(invoke, "_start");
            }
            _ => panic!("expected Wasm subcommand"),
        }
    }

    #[test]
    fn test_attach_without_session_id() {
        let args = parse(&["clud", "attach"]);
        match args.command {
            Some(Command::Attach { session_id, last }) => {
                assert!(session_id.is_none());
                assert!(!last);
            }
            _ => panic!("expected Attach subcommand"),
        }
    }

    #[test]
    fn test_attach_with_session_id() {
        let args = parse(&["clud", "attach", "sess-123"]);
        match args.command {
            Some(Command::Attach { session_id, last }) => {
                assert_eq!(session_id.as_deref(), Some("sess-123"));
                assert!(!last);
            }
            _ => panic!("expected Attach subcommand"),
        }
    }

    #[test]
    fn test_attach_with_last() {
        let args = parse(&["clud", "attach", "--last"]);
        match args.command {
            Some(Command::Attach { session_id, last }) => {
                assert!(session_id.is_none());
                assert!(last);
            }
            _ => panic!("expected Attach subcommand"),
        }
    }

    #[test]
    fn test_kill_subcommand() {
        let args = parse(&["clud", "kill", "sess-123"]);
        match args.command {
            Some(Command::Kill { session_id, all }) => {
                assert_eq!(session_id.as_deref(), Some("sess-123"));
                assert!(!all);
            }
            _ => panic!("expected Kill subcommand"),
        }
    }

    #[test]
    fn test_kill_all() {
        let args = parse(&["clud", "kill", "--all"]);
        match args.command {
            Some(Command::Kill { session_id, all }) => {
                assert!(session_id.is_none());
                assert!(all);
            }
            _ => panic!("expected Kill subcommand"),
        }
    }

    #[test]
    fn test_name_flag() {
        let args = parse(&["clud", "--name", "my-session", "--detach", "-p", "hello"]);
        assert_eq!(args.session_name.as_deref(), Some("my-session"));
        assert!(args.detach);
    }

    #[test]
    fn test_list_subcommand() {
        let args = parse(&["clud", "list"]);
        assert!(matches!(args.command, Some(Command::List)));
    }

    #[test]
    fn test_unknown_flags_passthrough() {
        let args = parse(&["clud", "--some-unknown-flag", "-p", "hello"]);
        assert_eq!(args.prompt.as_deref(), Some("hello"));
        assert_eq!(args.passthrough, vec!["--some-unknown-flag"]);
    }

    #[test]
    fn test_passthrough_after_separator() {
        let args = parse(&["clud", "-p", "hello", "--", "--verbose", "--debug"]);
        assert_eq!(args.prompt.as_deref(), Some("hello"));
        assert_eq!(args.passthrough, vec!["--verbose", "--debug"]);
    }

    #[test]
    fn test_verbose_flag() {
        let args = parse(&["clud", "-v"]);
        assert!(args.verbose);
    }

    #[test]
    fn test_default_no_flags() {
        let args = parse(&["clud"]);
        assert!(args.prompt.is_none());
        assert!(args.message.is_none());
        assert!(!args.continue_session);
        assert!(!args.claude);
        assert!(!args.codex);
        assert!(!args.subprocess);
        assert!(!args.pty);
        assert!(!args.safe);
        assert!(!args.dry_run);
        assert!(!args.detach);
        assert!(!args.detachable);
        assert!(args.command.is_none());
        assert!(args.passthrough.is_empty());
    }
}
