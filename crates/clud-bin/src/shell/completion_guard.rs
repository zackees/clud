//! Issue #753: keep Git-Bash completion functions out of the backend agent's
//! shell snapshot.
//!
//! Claude Code builds a "shell snapshot" once per session by running the shell
//! as a **login** shell (`execFile(shell, ["-c", "-l", script])`) and
//! serialising every captured function into a file that is then `source`d by
//! *every* subsequent Bash tool call. Its capture filter is
//! `declare -F | cut -d' ' -f3 | grep -vE '^_[^_]'` — which drops
//! single-underscore completion functions but deliberately keeps
//! double-underscore helpers (mise's `__zsh_like_cd`, `__pyenv_init`, …).
//!
//! On Windows that filter backfires. `-l` makes bash read `/etc/profile`,
//! which sources `/etc/profile.d/git-prompt.sh`, which sources
//! `git-completion.bash` — and Git's completion internals are *all*
//! double-underscore (`__git_*`), so all ~84 of them survive the filter. Each
//! one is emitted as
//!
//! ```text
//! eval "$(echo '<base64>' | base64 -d)" > /dev/null 2>&1
//! ```
//!
//! i.e. a subshell **plus a real `base64.exe`** — two process spawns per
//! function, ~170 per snapshot, replayed on every single Bash tool call.
//! Under MSYS2's emulated `fork()` that measured 4.4 s of CPU per tool call on
//! an idle machine, degrading to ~20 s once the resulting process storm
//! saturated the box.
//!
//! Git for Windows guards both completion-sourcing blocks in
//! `/etc/profile.d/git-prompt.sh` with `test -z "$WINELOADERNOEXEC"`, so
//! exporting that variable makes the login shell skip `git-completion.bash`
//! entirely. Measured effect: 85 captured functions → 1, and 4,413 ms → 49 ms
//! per Bash tool call.
//!
//! This is a **mitigation, not the fix**. The real fix is upstream: emit
//! `declare -f "$func"` straight into the snapshot instead of round-tripping
//! through base64 (which is what Claude Code's own *zsh* branch already does
//! via `typeset -f`). That would cost zero spawns regardless of how many
//! functions are captured, on every platform. Until then, this keeps the
//! Windows worst case survivable.
//!
//! Because the lever is a variable Git for Windows consults rather than one it
//! documents as an API, a change on their side would silently stop suppressing
//! completions. The guardrail against that is
//! `tests/shell_completion_guard.rs`, which asserts the observed **function
//! count** of a real login shell — not merely that this variable is set.

/// The variable Git for Windows' `/etc/profile.d/git-prompt.sh` consults.
/// Both of its completion-sourcing blocks are wrapped in
/// `if test -z "$WINELOADERNOEXEC"`, so any non-empty value suppresses them.
///
/// Nothing on native Windows reads this apart from those guards: diffing the
/// full exported environment of a login shell with and without it set shows a
/// single difference — `PS1` loses its `` `__git_ps1` `` segment, which is
/// meaningless in the non-interactive shell that runs tool calls.
pub const SUPPRESS_KEY: &str = "WINELOADERNOEXEC";

/// Escape hatch. Set to `1`/`true`/`yes`/`on` to keep Git-Bash completions in
/// the snapshot and pay the spawn cost — for anyone who has tooling that
/// genuinely depends on `__git_*` being defined inside a Bash tool call.
pub const OPT_OUT_KEY: &str = "CLUD_GIT_BASH_COMPLETIONS";

/// Whether the caller asked to keep completions via [`OPT_OUT_KEY`].
pub fn is_opted_out() -> bool {
    matches!(std::env::var(OPT_OUT_KEY), Ok(v) if is_truthy(&v))
}

fn is_truthy(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// The `(key, value)` overrides to layer onto a backend child environment.
///
/// Empty on non-Windows (the guard lives in Git for Windows' profile, and
/// `WINELOADERNOEXEC` is a real Wine variable we should not set on a platform
/// where Wine may actually be running) and empty when opted out.
pub fn env_overrides() -> Vec<(String, String)> {
    env_overrides_for(cfg!(windows), is_opted_out())
}

/// Test seam — the platform and opt-out decisions are passed in so the policy
/// can be exercised on any host without mutating the process environment.
pub fn env_overrides_for(is_windows: bool, opted_out: bool) -> Vec<(String, String)> {
    if !is_windows || opted_out {
        return Vec::new();
    }
    vec![(SUPPRESS_KEY.to_string(), "1".to_string())]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suppresses_on_windows_by_default() {
        let overrides = env_overrides_for(true, false);
        assert_eq!(
            overrides,
            vec![(SUPPRESS_KEY.to_string(), "1".to_string())],
            "Windows should suppress Git-Bash completion capture by default"
        );
    }

    #[test]
    fn opt_out_keeps_completions() {
        assert!(
            env_overrides_for(true, true).is_empty(),
            "{OPT_OUT_KEY} must restore the stock login-shell behaviour"
        );
    }

    #[test]
    fn no_op_off_windows() {
        assert!(
            env_overrides_for(false, false).is_empty(),
            "WINELOADERNOEXEC is a real Wine variable — never set it off Windows"
        );
        assert!(env_overrides_for(false, true).is_empty());
    }

    #[test]
    fn truthy_parsing_matches_documented_values() {
        for yes in ["1", "true", "TRUE", "yes", "on", "  On  "] {
            assert!(is_truthy(yes), "{yes:?} should opt out");
        }
        for no in ["0", "false", "no", "off", "", "banana"] {
            assert!(!is_truthy(no), "{no:?} should not opt out");
        }
    }
}
