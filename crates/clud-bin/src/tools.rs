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

/// Literal marker that distinguishes clud-managed tool files from
/// user-authored ones. Mirrors the same string used by
/// [`crate::skill_install`]; future cross-cutting tooling can grep for one
/// literal across both registries.
pub const MANAGED_BY_CLUD_MARKER: &str = "managed-by: clud";

/// One bundled tool: where it lands under `~/.clud/tools/` and the embedded
/// body baked into the binary at compile time.
pub struct BundledTool {
    /// Relative path under `~/.clud/tools/`, forward-slash-separated. May
    /// include subdirectories (e.g. `"github/pr_merge_watch.py"`).
    pub rel_path: &'static str,
    /// Verbatim file content. Must contain the literal
    /// [`MANAGED_BY_CLUD_MARKER`] string so the installer can distinguish
    /// the managed copy from a user-edited override.
    pub body: &'static str,
}

/// Every tool `clud` ships and auto-installs. Adding a tool is a one-line
/// entry here plus a new file under `crates/clud-bin/assets/tools/`.
///
/// Foundation PR — initially empty. First entry (the
/// `github/pr_merge_watch.py` script for issue #408) lands in a follow-up
/// PR once its body is designed; the empty array still exercises the
/// install loop, the registry-presence test, and the guardrail scan.
pub const BUNDLED_TOOLS: &[BundledTool] = &[];

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
