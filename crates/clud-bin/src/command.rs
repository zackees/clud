use crate::args::{Args, Command};
use crate::backend::Backend;

/// Predefined prompts for special commands.
const UP_PROMPT: &str = "\
Run the project's lint and test commands. Fix any errors found. \
When everything passes, create a git commit with a descriptive message.";

const REBASE_PROMPT: &str = "\
Rebase the current branch onto main. Resolve any merge conflicts. \
Run lint and tests to verify everything still works after the rebase.";

const FIX_PROMPT: &str = "\
Run lint and test commands. Fix all errors and warnings. \
Keep running until everything passes cleanly.";

/// A fully resolved command ready to execute (or print in dry-run mode).
#[derive(Debug, Clone)]
pub struct LaunchPlan {
    pub command: Vec<String>,
    pub iterations: u32,
}

/// Build the command to execute for the given args and backend.
pub fn build_launch_plan(args: &Args, backend: Backend, backend_path: &str) -> LaunchPlan {
    let mut cmd = vec![backend_path.to_string()];
    let mut iterations = 1u32;

    // YOLO mode: always inject unless --safe
    if !args.safe {
        cmd.push("--dangerously-skip-permissions".to_string());
    }

    // Model preference
    if let Some(ref model) = args.model {
        cmd.push("--model".to_string());
        cmd.push(model.clone());
    }

    // Handle subcommands
    match &args.command {
        Some(Command::Loop { prompt, loop_count }) => {
            iterations = *loop_count;
            if let Some(ref p) = prompt {
                let prompt_text = read_prompt_or_literal(p);
                cmd.push("-p".to_string());
                cmd.push(prompt_text);
            }
        }
        Some(Command::Up) => {
            cmd.push("-p".to_string());
            cmd.push(UP_PROMPT.to_string());
        }
        Some(Command::Rebase) => {
            cmd.push("-p".to_string());
            cmd.push(REBASE_PROMPT.to_string());
        }
        Some(Command::Fix) => {
            cmd.push("-p".to_string());
            cmd.push(FIX_PROMPT.to_string());
        }
        None => {
            // Direct flags
            if let Some(ref prompt) = args.prompt {
                cmd.push("-p".to_string());
                cmd.push(prompt.clone());
            }
            if let Some(ref message) = args.message {
                cmd.push("-m".to_string());
                cmd.push(message.clone());
            }
            if args.continue_session {
                cmd.push("--continue".to_string());
            }
            if let Some(ref resume) = args.resume {
                cmd.push("--resume".to_string());
                if let Some(ref term) = resume {
                    cmd.push(term.clone());
                }
            }
        }
    }

    // Forward unknown flags
    cmd.extend(args.passthrough.iter().cloned());

    // Suppress backend name for display purposes
    let _ = backend;

    LaunchPlan {
        command: cmd,
        iterations,
    }
}

/// If the string is a path to an existing file, read its contents.
/// Otherwise treat it as a literal prompt string.
fn read_prompt_or_literal(input: &str) -> String {
    let path = std::path::Path::new(input);
    if path.is_file() {
        std::fs::read_to_string(path).unwrap_or_else(|_| input.to_string())
    } else {
        input.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::Args;

    fn parse(raw: &[&str]) -> Args {
        let raw: Vec<String> = raw.iter().map(|s| s.to_string()).collect();
        Args::parse_from_raw(raw)
    }

    fn plan(raw: &[&str]) -> LaunchPlan {
        let args = parse(raw);
        let backend = crate::backend::resolve_backend(args.claude, args.codex);
        build_launch_plan(&args, backend, backend.executable_name())
    }

    #[test]
    fn test_prompt_with_yolo() {
        let p = plan(&["clud", "-p", "hello"]);
        assert_eq!(
            p.command,
            vec!["claude", "--dangerously-skip-permissions", "-p", "hello"]
        );
        assert_eq!(p.iterations, 1);
    }

    #[test]
    fn test_safe_mode_no_yolo() {
        let p = plan(&["clud", "--safe", "-p", "hello"]);
        assert_eq!(p.command, vec!["claude", "-p", "hello"]);
    }

    #[test]
    fn test_codex_backend() {
        let p = plan(&["clud", "--codex", "-p", "hello"]);
        assert_eq!(
            p.command,
            vec!["codex", "--dangerously-skip-permissions", "-p", "hello"]
        );
    }

    #[test]
    fn test_model_flag() {
        let p = plan(&["clud", "--model", "opus", "-p", "hello"]);
        assert_eq!(
            p.command,
            vec![
                "claude",
                "--dangerously-skip-permissions",
                "--model",
                "opus",
                "-p",
                "hello"
            ]
        );
    }

    #[test]
    fn test_continue_session() {
        let p = plan(&["clud", "-c"]);
        assert_eq!(
            p.command,
            vec!["claude", "--dangerously-skip-permissions", "--continue"]
        );
    }

    #[test]
    fn test_message_flag() {
        let p = plan(&["clud", "-m", "fix bug"]);
        assert_eq!(
            p.command,
            vec!["claude", "--dangerously-skip-permissions", "-m", "fix bug"]
        );
    }

    #[test]
    fn test_up_command() {
        let p = plan(&["clud", "up"]);
        assert_eq!(p.command[0], "claude");
        assert_eq!(p.command[1], "--dangerously-skip-permissions");
        assert_eq!(p.command[2], "-p");
        assert!(p.command[3].contains("lint"));
        assert!(p.command[3].contains("commit"));
    }

    #[test]
    fn test_rebase_command() {
        let p = plan(&["clud", "rebase"]);
        assert!(p.command[3].contains("Rebase"));
    }

    #[test]
    fn test_fix_command() {
        let p = plan(&["clud", "fix"]);
        assert!(p.command[3].contains("Fix"));
    }

    #[test]
    fn test_loop_command() {
        let p = plan(&["clud", "loop", "--loop-count", "5", "do stuff"]);
        assert_eq!(p.iterations, 5);
        assert!(p.command.contains(&"-p".to_string()));
        assert!(p.command.contains(&"do stuff".to_string()));
    }

    #[test]
    fn test_loop_default_count() {
        let p = plan(&["clud", "loop", "task"]);
        assert_eq!(p.iterations, 50);
    }

    #[test]
    fn test_passthrough_flags() {
        let p = plan(&["clud", "--some-flag", "-p", "hello"]);
        assert!(p.command.contains(&"--some-flag".to_string()));
    }

    #[test]
    fn test_passthrough_after_separator() {
        let p = plan(&["clud", "-p", "hello", "--", "--verbose"]);
        assert!(p.command.contains(&"--verbose".to_string()));
    }
}
