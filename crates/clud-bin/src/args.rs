use clap::{Parser, Subcommand};

/// Fast CLI for running Claude Code and Codex in YOLO mode.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "clud",
    version,
    about = "Fast CLI for running Claude Code and Codex in YOLO mode",
    after_help = "Unknown flags are forwarded directly to the backend agent."
)]
pub struct Args {
    /// Run with prompt, exit when done.
    #[arg(short = 'p', long = "prompt")]
    pub prompt: Option<String>,

    /// Send a one-off message.
    #[arg(short = 'm', long = "message")]
    pub message: Option<String>,

    /// Continue the most recent session.
    #[arg(short = 'c', long = "continue")]
    pub continue_session: bool,

    /// Resume a session by ID or search term.
    #[arg(short = 'r', long = "resume")]
    pub resume: Option<Option<String>>,

    /// Use Claude as the backend.
    #[arg(long = "claude", conflicts_with = "codex")]
    pub claude: bool,

    /// Use Codex as the backend.
    #[arg(long = "codex", conflicts_with = "claude")]
    pub codex: bool,

    /// Model preference (e.g., haiku, sonnet, opus).
    #[arg(long = "model")]
    pub model: Option<String>,

    /// Disable YOLO mode (don't inject --dangerously-skip-permissions).
    #[arg(long = "safe")]
    pub safe: bool,

    /// Print what would be executed, then exit.
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Enable verbose/debug output.
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,

    /// Subcommands: loop, up, rebase, fix.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Unknown flags forwarded to the backend.
    #[arg(last = true, id = "BACKEND_ARGS")]
    pub passthrough: Vec<String>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Run autonomous loop iterations.
    Loop {
        /// Prompt text or path to a file (e.g., LOOP.md).
        prompt: Option<String>,

        /// Number of iterations (default: 50).
        #[arg(long = "loop-count", default_value = "50")]
        loop_count: u32,
    },

    /// Codeup workflow: lint, test, commit.
    Up,

    /// Rebase workflow.
    Rebase,

    /// Auto-fix linting/test errors.
    Fix,
}

impl Args {
    /// Parse args, allowing unknown flags to be forwarded to the backend.
    ///
    /// Clap's `last = true` requires a `--` separator before passthrough args.
    /// For better UX, we manually split known vs unknown args before parsing.
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

/// Split raw CLI args into (known to clap, unknown to forward).
fn split_known_unknown(raw: &[String]) -> (Vec<String>, Vec<String>) {
    let mut known = vec![raw[0].clone()]; // program name
    let mut unknown = Vec::new();
    let mut i = 1;

    // Known long flags that take a value
    let value_flags: &[&str] = &[
        "--prompt",
        "--message",
        "--resume",
        "--model",
        "--loop-count",
    ];
    // Known short flags that take a value
    let short_value_flags: &[&str] = &["-p", "-m", "-r"];
    // Known boolean flags (no value)
    let bool_flags: &[&str] = &[
        "--continue",
        "--claude",
        "--codex",
        "--safe",
        "--dry-run",
        "--verbose",
        "--help",
        "--version",
    ];
    let short_bool_flags: &[&str] = &["-c", "-v", "-h", "-V"];
    // Known subcommands
    let subcommands: &[&str] = &["loop", "up", "rebase", "fix"];

    // Once we hit a subcommand, everything after is known (clap handles it)
    let mut in_subcommand = false;

    while i < raw.len() {
        let arg = &raw[i];

        if arg == "--" {
            // Everything after -- is passthrough
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

        // Check for --flag=value style
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

        // Unknown flag — forward to backend
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
    fn test_loop_subcommand() {
        let args = parse(&["clud", "loop", "do the task"]);
        match args.command {
            Some(Command::Loop {
                ref prompt,
                loop_count,
            }) => {
                assert_eq!(prompt.as_deref(), Some("do the task"));
                assert_eq!(loop_count, 50);
            }
            _ => panic!("expected Loop subcommand"),
        }
    }

    #[test]
    fn test_loop_with_count() {
        let args = parse(&["clud", "loop", "--loop-count", "5", "task"]);
        match args.command {
            Some(Command::Loop {
                ref prompt,
                loop_count,
            }) => {
                assert_eq!(prompt.as_deref(), Some("task"));
                assert_eq!(loop_count, 5);
            }
            _ => panic!("expected Loop subcommand"),
        }
    }

    #[test]
    fn test_up_subcommand() {
        let args = parse(&["clud", "up"]);
        assert!(matches!(args.command, Some(Command::Up)));
    }

    #[test]
    fn test_rebase_subcommand() {
        let args = parse(&["clud", "rebase"]);
        assert!(matches!(args.command, Some(Command::Rebase)));
    }

    #[test]
    fn test_fix_subcommand() {
        let args = parse(&["clud", "fix"]);
        assert!(matches!(args.command, Some(Command::Fix)));
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
        assert!(!args.safe);
        assert!(!args.dry_run);
        assert!(args.command.is_none());
        assert!(args.passthrough.is_empty());
    }
}
