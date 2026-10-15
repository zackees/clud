//! Unix half of `crate::runner`'s PTY execution path. The console drag-drop
//! target and the native console-input reader are Windows-only, so every
//! function here is an inert stub.

use std::sync::mpsc::Receiver;

/// No console IDropTarget off Windows.
pub fn register_dnd_pty(dnd_enabled: bool) -> (Option<impl Sized>, Option<Receiver<Vec<u8>>>) {
    let _ = dnd_enabled;
    (None::<()>, None)
}

/// No native console-input reader off Windows; POSIX uses the byte stream.
pub fn spawn_console_input_for_iteration() -> (Option<Receiver<Vec<u8>>>, Option<impl Sized>) {
    (None, None::<()>)
}

/// Nothing to retire off Windows.
pub fn end_console_input_iteration(_guard: Option<impl Sized>) {}

#[cfg(test)]
mod tests {
    use crate::runner::child_env;

    fn env_lookup(env: &[(String, String)], key: &str) -> Option<String> {
        env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn child_env_leaves_wine_loader_alone_off_windows() {
        use crate::shell::completion_guard::SUPPRESS_KEY;
        // Only meaningful when the ambient env doesn't already carry it.
        if std::env::var(SUPPRESS_KEY).is_err() {
            assert_eq!(env_lookup(&child_env(), SUPPRESS_KEY), None);
        }
    }
}
