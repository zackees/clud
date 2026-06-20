//! Bundled-tools registry — declares every Python tool that `clud` ships
//! and auto-installs to `~/.clud/tools/`.
//!
//! Companion to [`crate::skills::BUNDLED_SKILLS`] for `SKILL.md` files. The
//! key shape differences:
//!
//! - Tools live under `~/.clud/tools/<rel_path>`, not under any agent's
//!   skills directory — they're invoked by `clud tool run <rel_path>` and
//!   the agent backend never resolves them directly.
//! - Tools are `uv run` scripts (PEP 723 inline script metadata). No
//!   `executable: bool` field — the file mode is irrelevant because callers
//!   invoke uv explicitly.
//! - `rel_path` may contain `/` so tools naturally group by domain
//!   (`github/…`, `git/…`, `docker/…`).
//!
//! Install lifecycle lives in [`crate::tool_install`]. The default cache
//! root for `uv` invocations on bundled tools is
//! [`clud_uv_cache_dir`]; the `clud tool run` subcommand sets `UV_CACHE_DIR`
//! to that path so per-script venvs land under clud's own state root and
//! never leak into the user's global `~/.cache/uv/`.

use std::path::PathBuf;
use std::time::Duration;

/// Literal marker that distinguishes clud-managed tool files from
/// user-authored ones. Mirrors the same string used by
/// [`crate::skill_install`]; future cross-cutting tooling can grep for one
/// literal across both registries.
pub const MANAGED_BY_CLUD_MARKER: &str = "managed-by: clud";

/// Default `command_timeout` for a tool declared `KillSemantics::Resumable`.
/// Resumable tools observe external state (PR merge polls, CI watchers);
/// a 20-minute cap matches PM2's polling-friendly cadence and stays within
/// 4× Anthropic's 5-minute prompt-cache window. Entries may override.
pub const DEFAULT_RESUMABLE_TIMEOUT: Duration = Duration::from_secs(60 * 20);

/// Default `command_timeout` for a tool declared `KillSemantics::Killable`
/// (or `ResumableWithKillableSubsteps`). One hour matches typical
/// long-build wall-clock (cargo build with cold cache, docker multi-stage
/// build). Entries may override.
pub const DEFAULT_KILLABLE_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Kill-vs-resume semantics for a tool invocation. Drives how the
/// `tool_run` wrapper handles `command_timeout` and what abort terminal
/// it returns. See #427 for the full taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillSemantics {
    /// Killing the tool process loses no underlying state — the world
    /// holds the state, the tool just observes. On `command_timeout` the
    /// wrapper returns `status: in-progress` and exit 0; re-invocation
    /// with the same args resumes. Examples: `gh-pr-merge-wait`,
    /// `gh-watch-issue`.
    Resumable,
    /// The tool process IS the state — killing it aborts the work.
    /// On `command_timeout` or `progress_timeout` the wrapper kills the
    /// process tree and returns a structured abort terminal with last 50
    /// lines + diagnostic block. Examples: `lint`, `docker-build`,
    /// `cargo build`.
    Killable,
    /// Outer loop is resumable, but individual substeps are killable
    /// (e.g. `docker-run-watch` polling logs while a per-step docker
    /// exec can be aborted). Slice 5 may need a richer wrapper for
    /// this variant; at the outer-cap layer it behaves like `Resumable`.
    ResumableWithKillableSubsteps,
}

impl KillSemantics {
    /// Default `command_timeout` for this kill-semantics class.
    pub const fn default_command_timeout(self) -> Duration {
        match self {
            KillSemantics::Resumable => DEFAULT_RESUMABLE_TIMEOUT,
            KillSemantics::Killable => DEFAULT_KILLABLE_TIMEOUT,
            KillSemantics::ResumableWithKillableSubsteps => DEFAULT_RESUMABLE_TIMEOUT,
        }
    }
}

/// One bundled tool: where it lands under `~/.clud/tools/`, the embedded
/// body baked into the binary at compile time, plus the execution
/// semantics that drive the `tool_run` wrapper's watchdog and abort
/// terminals.
///
/// Slice 1 of #427 added `kill_semantics`, `command_timeout`,
/// `progress_timeout`, and `quiet_ok`. The wrapper enforcement that
/// honors these fields lands in slice 5 (#432); slices 2–4 use them only
/// for the session index's metadata.
pub struct BundledTool {
    /// Relative path under `~/.clud/tools/`, forward-slash-separated. May
    /// include subdirectories (e.g. `"github/pr_merge_watch.py"`).
    pub rel_path: &'static str,
    /// Verbatim file content. Must contain the literal
    /// [`MANAGED_BY_CLUD_MARKER`] string so the installer can distinguish
    /// the managed copy from a user-edited override.
    pub body: &'static str,
    /// Whether this tool's process is `Resumable` (observation) or
    /// `Killable` (state-bearing). Drives the wrapper's abort terminal.
    pub kill_semantics: KillSemantics,
    /// Wall-clock cap on a single invocation. Defaults are 20 min for
    /// resumable and 60 min for killable; entries may override.
    pub command_timeout: Duration,
    /// If `Some`, the wrapper aborts when no output (stdout or stderr)
    /// has been observed for this duration — even before `command_timeout`
    /// fires. `None` = no progress watchdog. Use for long-running builds
    /// or downloads where silence indicates a hang.
    pub progress_timeout: Option<Duration>,
    /// If `true`, the wrapper does NOT arm the progress watchdog even
    /// when `progress_timeout` is `Some`. Use for tools whose normal
    /// operation is silent (e.g. `git diff` with no changes is not hung).
    pub quiet_ok: bool,
}

/// Every tool `clud` ships and auto-installs. Adding a tool is a one-line
/// entry here plus a new file under `crates/clud-bin/assets/tools/`.
pub const BUNDLED_TOOLS: &[BundledTool] = &[
    BundledTool {
        rel_path: "github/pr_merge_watch.py",
        body: include_str!("../assets/tools/github/pr_merge_watch.py"),
        // PR-merge watcher polls GitHub state; the world owns the merge
        // status, so killing this process loses no work — `Resumable`.
        kill_semantics: KillSemantics::Resumable,
        command_timeout: DEFAULT_RESUMABLE_TIMEOUT,
        progress_timeout: None,
        quiet_ok: false,
    },
    BundledTool {
        rel_path: "hooks/block-bad-cmd.py",
        body: include_str!("../assets/tools/hooks/block-bad-cmd.py"),
        // PreToolUse hook: reads a small JSON blob from stdin, decides
        // allow/deny, exits. The decision IS the work; killing mid-run
        // loses the verdict — `Killable`. Hook runners cap themselves
        // at a few seconds, so the 30s ceiling here is a backstop, not
        // an expected wall-clock.
        kill_semantics: KillSemantics::Killable,
        command_timeout: Duration::from_secs(30),
        progress_timeout: None,
        quiet_ok: true,
    },
];

/// The single source of truth for the `UV_CACHE_DIR` value used by every
/// bundled-tool invocation in clud's process tree.
///
/// Three callers must agree on this path:
/// 1. `main.rs` — sets the env var at startup so every descendant
///    subprocess inherits it (Layer 3 of the three-layer enforcement).
/// 2. `tool_run.rs` — re-affirms the env var per-invocation so a forgotten
///    Layer-3 set still lands in the right cache (Layer 1 chokepoint).
/// 3. Any future `clud gc --kind uv-cache` implementation operates on this
///    same directory.
///
/// Returns the path under the resolved home directory; if the home dir is
/// unresolvable, returns a deliberately broken sentinel rather than
/// `None` — callers that hit the sentinel will fail loudly the first time
/// `uv` tries to use it, which is the right failure mode (a misconfigured
/// host should not silently scribble into a relative path).
pub fn clud_uv_cache_dir() -> PathBuf {
    home_dir()
        .map(|h| h.join(".clud/cache/uv"))
        .unwrap_or_else(|| PathBuf::from("/nonexistent/clud-uv-cache-no-home"))
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::BUNDLED_SKILLS;

    /// Bundled tool bodies must always carry the managed-by marker.
    /// Foundation PR: vacuously passes (empty registry) but locks the
    /// invariant in before the first tool body lands.
    #[test]
    fn bundled_tools_carry_managed_by_marker() {
        for tool in BUNDLED_TOOLS {
            assert!(
                tool.body.contains(MANAGED_BY_CLUD_MARKER),
                "tool {} is missing the `{MANAGED_BY_CLUD_MARKER}` marker — \
                 install lifecycle treats it as user-edited and never updates it",
                tool.rel_path
            );
        }
    }

    /// Bundled tool relative paths must be unique. Foundation PR: vacuous,
    /// but prevents a future entry from silently shadowing another.
    #[test]
    fn bundled_tool_paths_are_unique() {
        let mut seen: Vec<&'static str> = Vec::new();
        for tool in BUNDLED_TOOLS {
            assert!(
                !seen.contains(&tool.rel_path),
                "duplicate BundledTool rel_path: {}",
                tool.rel_path
            );
            seen.push(tool.rel_path);
        }
    }

    /// Bundled assets (skills and tools both) must never invoke a bundled
    /// tool via raw `uv run ~/.clud/tools/…`. The `UV_CACHE_DIR` pin only
    /// kicks in when invocation goes through `clud tool run`; raw calls
    /// would bypass it and leak the env into the user's global cache.
    ///
    /// Layer 2 of the three-layer enforcement from issue #408. Today both
    /// registries pass trivially — no bundled asset references any tool
    /// yet — but the test locks the invariant in before any do.
    #[test]
    fn bundled_assets_invoke_tools_via_clud_subcommand_only() {
        const FORBIDDEN: &[&str] = &["uv run ~/.clud/tools/", "uv run $HOME/.clud/tools/"];
        for skill in BUNDLED_SKILLS {
            for pat in FORBIDDEN {
                assert!(
                    !skill.skill_md.contains(pat),
                    "skill {} invokes a bundled tool via raw `uv run`; \
                     use `clud tool run <rel_path>` so UV_CACHE_DIR is pinned. \
                     Forbidden pattern matched: {pat}",
                    skill.name,
                );
            }
        }
        for tool in BUNDLED_TOOLS {
            for pat in FORBIDDEN {
                assert!(
                    !tool.body.contains(pat),
                    "tool {} invokes a sibling tool via raw `uv run`; \
                     use `clud tool run <rel_path>` instead. \
                     Forbidden pattern matched: {pat}",
                    tool.rel_path,
                );
            }
        }
    }

    /// Issue #418 sub-task 1: the pr_merge_watch.py tool ships in the
    /// bundle. If this test fires, the BUNDLED_TOOLS entry was renamed,
    /// removed, or the asset file went missing — any of which breaks the
    /// downstream `clud-pr` / `clud-pr-merge` skill calls.
    #[test]
    fn bundled_includes_pr_merge_watch() {
        let names: Vec<&str> = BUNDLED_TOOLS.iter().map(|t| t.rel_path).collect();
        assert!(
            names.contains(&"github/pr_merge_watch.py"),
            "BUNDLED_TOOLS must include github/pr_merge_watch.py; \
             got {names:?}",
        );
    }

    /// The bundled pr_merge_watch.py must declare its expected exit
    /// codes in its docstring — those codes are the public contract that
    /// any caller (`clud-pr-merge` skill, future tooling) depends on.
    #[test]
    fn pr_merge_watch_documents_exit_codes() {
        let tool = BUNDLED_TOOLS
            .iter()
            .find(|t| t.rel_path == "github/pr_merge_watch.py")
            .expect("pr_merge_watch.py must be in BUNDLED_TOOLS");
        for code_line in [
            "Exit codes:",
            "0  all required checks green",
            "1  at least one required check failed",
            "2  new review activity",
            "3  PR closed or merged",
            "4  timeout",
        ] {
            assert!(
                tool.body.contains(code_line),
                "pr_merge_watch.py docstring must document `{code_line}`",
            );
        }
    }

    #[test]
    fn clud_uv_cache_dir_resolves_under_home() {
        // We can't control the host's $HOME / %USERPROFILE% from a unit
        // test, so just confirm the path ends with the expected suffix
        // when a home is available.
        let cache = clud_uv_cache_dir();
        let s = cache.to_string_lossy();
        assert!(
            s.ends_with(".clud/cache/uv")
                || s.ends_with(".clud\\cache\\uv")
                || s.contains("clud-uv-cache-no-home"),
            "clud_uv_cache_dir returned an unexpected path: {s}",
        );
    }
}
