//! Preserve explicitly requested Command Prompt windows across session cleanup.

/// Only bare interactive Command Prompt requests leave session containment.
/// All other `start` arguments retain Git Bash's normal implementation.
pub fn bash_function() -> &'static str {
    r#"if [ -n "${CLUD_EXE:-}" ] && [ -x "${CLUD_EXE}" ]; then
    start() {
        if [ "$#" -eq 1 ]; then
            case "$1" in
                [cC][mM][dD]|[cC][mM][dD].[eE][xX][eE])
                    "$CLUD_EXE" __start-cmd
                    return $?
                    ;;
            esac
        fi
        command start "$@"
    }
fi
"#
}

/// Launch a separate console using the existing explicit-persistence contract.
pub fn run() -> i32 {
    match launch() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("clud: could not open Command Prompt: {error}");
            1
        }
    }
}

fn launch() -> std::io::Result<i32> {
    let comspec = std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into());
    // running-process: command-builder
    let mut command = std::process::Command::new(comspec);
    command.args(["/D", "/C", "start", "", "cmd.exe"]);
    // Leave the inner PTY kill-on-close job. The inherited daemon marker
    // also spares the new console in clud's outer tracker and orphan sweep.
    // Preserve the tool shell's cwd and environment in the user's console.
    let mut child = running_process::spawn_daemon_breaking_away_with_env_policy(
        &mut command,
        running_process::EnvironmentPolicy::Inherit,
    )?;
    child.wait()
}
