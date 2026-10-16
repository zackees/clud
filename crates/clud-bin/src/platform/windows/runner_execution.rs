//! Windows half of `crate::runner`'s PTY execution path: the console
//! drag-drop target and the native console-input reader.

use std::sync::mpsc::Receiver;

/// Register the console IDropTarget for a PTY launch when `dnd_enabled`.
/// The guard must be held for the whole launch.
pub fn register_dnd_pty(dnd_enabled: bool) -> (Option<impl Sized>, Option<Receiver<Vec<u8>>>) {
    if dnd_enabled {
        crate::startup::try_register_console_drop_target_pty()
    } else {
        (None, None)
    }
}

/// Start the native console-input reader for one PTY iteration (#141,
/// #575). The returned guard's Drop retires the reader's channel.
pub fn spawn_console_input_for_iteration() -> (Option<Receiver<Vec<u8>>>, Option<impl Sized>) {
    if crate::session::terminals_are_interactive() {
        match crate::console_input::spawn_console_input_reader() {
            Ok(mut handle) => {
                let rx = handle.take_receiver();
                (rx, Some(handle))
            }
            Err(e) => {
                eprintln!("[clud] note: console-input reader unavailable: {e}");
                (None, None)
            }
        }
    } else {
        (None, None)
    }
}

/// End one iteration's console-input capture. Here the guard is a real
/// `ConsoleInputHandle` whose Drop retires the reader's channel, and dropping
/// it now is what scopes capture to one PTY iteration (#141, #575).
pub fn end_console_input_iteration(guard: Option<impl Sized>) {
    drop(guard);
}

#[cfg(test)]
mod tests {
    use crate::runner::child_env;

    fn env_lookup(env: &[(String, String)], key: &str) -> Option<String> {
        env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    /// Issue #168: Windows children get UTF-8 forced via Python env vars
    /// so any Python helper the agent spawns emits and reads UTF-8.
    /// IN_CLUD and ORIGINATOR vars must still be present, and PYTHONUTF8
    /// must be exactly "1" (Python accepts 0/1 only).
    #[test]
    fn child_env_sets_python_utf8_vars_on_windows() {
        let env = child_env();
        let lookup = |key: &str| -> Option<String> {
            env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
        };
        assert_eq!(lookup("PYTHONIOENCODING").as_deref(), Some("utf-8"));
        assert_eq!(lookup("PYTHONUTF8").as_deref(), Some("1"));
        assert_eq!(lookup("IN_CLUD").as_deref(), Some("1"));
        assert!(
            lookup(running_process::ORIGINATOR_ENV_VAR).is_some(),
            "ORIGINATOR var must still be set"
        );
        let pyio_count = env.iter().filter(|(k, _)| k == "PYTHONIOENCODING").count();
        assert_eq!(pyio_count, 1, "PYTHONIOENCODING must appear exactly once");
    }

    /// Issue #753: the backend's shell snapshot must not capture Git-Bash
    /// completion functions. Windows-only — see shell::completion_guard for
    /// why this variable is deliberately not set elsewhere.
    #[test]
    fn child_env_suppresses_git_bash_completions_on_windows() {
        use crate::shell::completion_guard::{OPT_OUT_KEY, SUPPRESS_KEY};

        // Guard the process-global opt-out so this test is order-independent.
        let prior = std::env::var(OPT_OUT_KEY).ok();
        std::env::remove_var(OPT_OUT_KEY);
        let env = child_env();
        match prior {
            Some(v) => std::env::set_var(OPT_OUT_KEY, v),
            None => std::env::remove_var(OPT_OUT_KEY),
        }

        assert_eq!(
            env_lookup(&env, SUPPRESS_KEY).as_deref(),
            Some("1"),
            "{SUPPRESS_KEY} must be set so the login shell skips git-completion.bash"
        );
        let count = env.iter().filter(|(k, _)| k == SUPPRESS_KEY).count();
        assert_eq!(count, 1, "{SUPPRESS_KEY} must appear exactly once");
    }
}
