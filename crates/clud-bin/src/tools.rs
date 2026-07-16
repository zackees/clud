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
        rel_path: "git/clud-git-diff.py",
        body: include_str!("../assets/tools/git/clud-git-diff.py"),
        // Native OS webview diff viewer (pywebview). The world (git
        // history) owns the state; killing the process just closes the
        // window — `Resumable`. The viewer is silent while the user
        // reads, so `quiet_ok` suppresses the progress watchdog.
        // 60-minute ceiling because a deep code review can take a while.
        kill_semantics: KillSemantics::Resumable,
        command_timeout: DEFAULT_KILLABLE_TIMEOUT,
        progress_timeout: None,
        quiet_ok: true,
    },
    BundledTool {
        rel_path: "hooks/block-bad-cmd.py",
        body: include_str!("../assets/tools/hooks/block-bad-cmd.py"),
        // Compatibility shim for hand-written hook configs that still call
        // `clud tool run hooks/block-bad-cmd.py`. The normal hot path is
        // the PyPI-shipped native `clud-block-bad-cmd` executable. The
        // shim execs that binary, so the decision IS still the work;
        // killing mid-run loses the verdict — `Killable`.
        kill_semantics: KillSemantics::Killable,
        command_timeout: Duration::from_secs(30),
        progress_timeout: None,
        quiet_ok: true,
    },
    BundledTool {
        rel_path: "hooks/uv_run_hook_guard.py",
        body: include_str!("../assets/tools/hooks/uv_run_hook_guard.py"),
        // Startup-time scanner: walks .claude/.codex hook configs in
        // a Python+Rust polyglot repo and warns on bare `uv run` in
        // Pre/PostToolUse hooks. Killable because the process IS the
        // work; the 3-second sleep at the end of `main()` accounts
        // for the only deliberate wall-clock spent (the warning's
        // visibility delay). 30s backstop is comfortable headroom.
        kill_semantics: KillSemantics::Killable,
        command_timeout: Duration::from_secs(30),
        progress_timeout: None,
        quiet_ok: true,
    },
    BundledTool {
        rel_path: "hooks/telemetry.py",
        body: include_str!("../assets/tools/hooks/telemetry.py"),
        // PostToolUse hook (#473): ships the hook payload to the clud
        // daemon's /telemetry/log endpoint. ALWAYS exits 0 by contract —
        // a stuck or broken telemetry path must never block a tool call.
        // Killable because the work IS the POST; killing mid-flight loses
        // that one record but the tool call is unaffected (recommended
        // wiring uses `async: true`). 30s backstop matches the other
        // hooks; the script itself enforces a 2s HTTP timeout internally.
        kill_semantics: KillSemantics::Killable,
        command_timeout: Duration::from_secs(30),
        progress_timeout: None,
        quiet_ok: true,
    },
    // docker-build tool family — implementation of zackees/clud#421.
    // Trampoline filename uses a hyphen to match the public CLI shape
    // (`clud tool run docker/docker-build.py soldr <path>`); sibling
    // per-stack files use underscores so Python `importlib` can load
    // them as modules without a hyphen-in-identifier syntax error.
    //
    // All four are `Killable`: the underlying work IS the running docker
    // subprocess (init writes files; up/run/shell/verify/clean drive a
    // container). Killing the python process kills the docker exec —
    // and re-invocation re-runs from scratch (idempotent for `up`,
    // re-issues the command for `run`).
    //
    // `progress_timeout: Some(10 min)` — a healthy `docker build` /
    // `docker run` emits layer-step output continuously; ten minutes of
    // silence is the daemon hung, not legitimate work. The 60-min
    // `command_timeout` ceiling still applies when the build is making
    // visible progress.
    BundledTool {
        rel_path: "docker/docker-build.py",
        body: include_str!("../assets/tools/docker/docker-build.py"),
        kill_semantics: KillSemantics::Killable,
        command_timeout: DEFAULT_KILLABLE_TIMEOUT,
        progress_timeout: Some(Duration::from_secs(60 * 10)),
        quiet_ok: false,
    },
    BundledTool {
        rel_path: "docker/docker_build_soldr.py",
        body: include_str!("../assets/tools/docker/docker_build_soldr.py"),
        kill_semantics: KillSemantics::Killable,
        command_timeout: DEFAULT_KILLABLE_TIMEOUT,
        progress_timeout: Some(Duration::from_secs(60 * 10)),
        quiet_ok: false,
    },
    BundledTool {
        rel_path: "docker/docker_build_python.py",
        body: include_str!("../assets/tools/docker/docker_build_python.py"),
        kill_semantics: KillSemantics::Killable,
        command_timeout: DEFAULT_KILLABLE_TIMEOUT,
        progress_timeout: Some(Duration::from_secs(60 * 10)),
        quiet_ok: false,
    },
    BundledTool {
        rel_path: "docker/docker_build_cpp.py",
        body: include_str!("../assets/tools/docker/docker_build_cpp.py"),
        kill_semantics: KillSemantics::Killable,
        command_timeout: DEFAULT_KILLABLE_TIMEOUT,
        progress_timeout: Some(Duration::from_secs(60 * 10)),
        quiet_ok: false,
    },
    BundledTool {
        rel_path: "docker/docker_recover.py",
        body: include_str!("../assets/tools/docker/docker_recover.py"),
        kill_semantics: KillSemantics::Killable,
        command_timeout: Duration::from_secs(60 * 3),
        progress_timeout: Some(Duration::from_secs(30)),
        quiet_ok: false,
    },
    BundledTool {
        rel_path: "python/lint_deadcode.py",
        body: include_str!("../assets/tools/python/lint_deadcode.py"),
        // Vulture scans the source tree and exits with the report.
        // The process IS the work — `Killable`. 2-minute progress
        // watchdog because vulture should be emitting status during
        // its walk.
        kill_semantics: KillSemantics::Killable,
        command_timeout: DEFAULT_KILLABLE_TIMEOUT,
        progress_timeout: Some(std::time::Duration::from_secs(120)),
        quiet_ok: false,
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

    /// The `uv_run_hook_guard` tool ships and is invoked from clud
    /// startup (`main.rs` → `uv_run_hook_guard::run`). If the entry is
    /// removed or renamed the wrapper silently does nothing and the
    /// startup warning never fires — invariant test catches the drift.
    #[test]
    fn bundled_includes_uv_run_hook_guard() {
        let names: Vec<&str> = BUNDLED_TOOLS.iter().map(|t| t.rel_path).collect();
        assert!(
            names.contains(&"hooks/uv_run_hook_guard.py"),
            "BUNDLED_TOOLS must include hooks/uv_run_hook_guard.py; \
             got {names:?}",
        );
    }

    /// Issue #489: the old Python command guard stays in BUNDLED_TOOLS only
    /// as a compatibility shim. The native binary owns the policy logic.
    #[test]
    fn bundled_includes_block_bad_cmd_compat_shim() {
        let tool = BUNDLED_TOOLS
            .iter()
            .find(|t| t.rel_path == "hooks/block-bad-cmd.py")
            .expect("compatibility shim must remain for existing hook configs");
        assert!(
            tool.body.contains("clud-block-bad-cmd"),
            "block-bad-cmd.py must delegate to the native binary",
        );
        assert!(
            tool.body.contains("compatibility"),
            "block-bad-cmd.py must document that it is compatibility-only",
        );
    }

    /// Issue #473: the `hooks/telemetry.py` PostToolUse bridge ships in
    /// the bundle and is the documented producer for the daemon's
    /// `/telemetry/log` endpoint (#469). If the entry is renamed or
    /// removed, the recommended `~/.claude/settings.json` wiring
    /// (`clud tool run hooks/telemetry.py`) silently breaks.
    #[test]
    fn bundled_includes_telemetry_hook() {
        let names: Vec<&str> = BUNDLED_TOOLS.iter().map(|t| t.rel_path).collect();
        assert!(
            names.contains(&"hooks/telemetry.py"),
            "BUNDLED_TOOLS must include hooks/telemetry.py; got {names:?}",
        );
        let tool = BUNDLED_TOOLS
            .iter()
            .find(|t| t.rel_path == "hooks/telemetry.py")
            .expect("just-asserted entry");
        // Contract assertions: never blocks, env-var-driven discovery.
        assert!(
            tool.body.contains("ALWAYS exits 0"),
            "telemetry.py must document the always-exit-0 contract",
        );
        assert!(
            tool.body.contains("CLUD_DAEMON_HTTP_SERVER"),
            "telemetry.py must reference the env-var discovery contract",
        );
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

    /// Issue #421: the docker-build tool family ships in the bundle.
    /// All four entries (trampoline + three per-stack tools) must be
    /// present or the SKILL.md / consumer skills break.
    #[test]
    fn bundled_includes_docker_build_family() {
        let names: Vec<&str> = BUNDLED_TOOLS.iter().map(|t| t.rel_path).collect();
        for required in [
            "docker/docker-build.py",
            "docker/docker_build_soldr.py",
            "docker/docker_build_python.py",
            "docker/docker_build_cpp.py",
        ] {
            assert!(
                names.contains(&required),
                "BUNDLED_TOOLS must include {required}; got {names:?}",
            );
        }
    }

    /// Issue #531: recovery must be available before a Linux build attempts
    /// to use an unavailable Desktop engine. Keep its storage safety contract
    /// visible at the embedded-asset boundary.
    #[test]
    fn bundled_includes_docker_recover() {
        let tool = BUNDLED_TOOLS
            .iter()
            .find(|t| t.rel_path == "docker/docker_recover.py")
            .expect("docker recovery tool must be bundled");
        for required_marker in [
            "CustomWslDistroDir",
            "docker_recover.py doctor",
            "--yes",
            "NEVER compacts",
            "mutate Docker storage",
        ] {
            assert!(
                tool.body.contains(required_marker),
                "docker_recover.py must contain `{required_marker}`"
            );
        }
    }

    /// The docker-build trampoline must document its dispatch contract
    /// in the docstring so callers (the SKILL.md, other skills) can
    /// rely on its argv shape. Locks the public CLI shape in via the
    /// embedded body so a docstring rewrite that breaks the contract
    /// trips this test before the bad tool ships.
    #[test]
    fn docker_build_trampoline_documents_dispatch_shape() {
        let trampoline = BUNDLED_TOOLS
            .iter()
            .find(|t| t.rel_path == "docker/docker-build.py")
            .expect("docker-build trampoline must be in BUNDLED_TOOLS");
        for required_line in [
            "clud tool run docker/docker-build.py <stack> <path> [subcommand]",
            "clud tool run docker/docker-build.py doctor",
            "Stacks: soldr",
            "doctor",
        ] {
            assert!(
                trampoline.body.contains(required_line),
                "docker-build.py must document `{required_line}` in its body",
            );
        }
    }

    /// Each per-stack tool must declare its v0 scope honestly: the
    /// soldr stack ships init+up+run+shell+clean+doctor; python+cpp
    /// ship init only (other subcommands exit 64). Lock that in.
    #[test]
    fn docker_build_stack_v0_scopes_match_issue_421() {
        let soldr = BUNDLED_TOOLS
            .iter()
            .find(|t| t.rel_path == "docker/docker_build_soldr.py")
            .expect("soldr stack tool must exist");
        for required_marker in [
            "def cmd_init",
            "def cmd_up",
            "def cmd_run",
            "def cmd_doctor",
        ] {
            assert!(
                soldr.body.contains(required_marker),
                "soldr stack tool must implement `{required_marker}` in v0",
            );
        }
        for stub_stack in ["python", "cpp"] {
            let path = format!("docker/docker_build_{stub_stack}.py");
            let tool = BUNDLED_TOOLS
                .iter()
                .find(|t| t.rel_path == path)
                .unwrap_or_else(|| panic!("{stub_stack} stack tool must exist"));
            assert!(
                tool.body.contains("def cmd_init"),
                "{stub_stack} stack must at least implement init in v0",
            );
            assert!(
                tool.body.contains("not implemented in v0"),
                "{stub_stack} stack must clearly mark not-yet-implemented subcommands",
            );
        }
    }

    /// Issue #502: the soldr docker-build stack must not spend the first
    /// command installing soldr or rebuilding soldr's daemon state from
    /// scratch. The generated image bakes the release tarball in, and the
    /// run wrapper keeps /root/.soldr in a named Docker volume.
    #[test]
    fn docker_build_soldr_image_bakes_soldr_and_persists_home() {
        let soldr = BUNDLED_TOOLS
            .iter()
            .find(|t| t.rel_path == "docker/docker_build_soldr.py")
            .expect("soldr stack tool must exist");
        for required_marker in [
            "ARG SOLDR_VERSION=0.8.0",
            "soldr-v${SOLDR_VERSION}-x86_64-unknown-linux-gnu.tar.zst",
            "soldr-clang-shim cargo-chef crgx",
            "soldr --version",
            "soldr_home = \"/root/.soldr\"",
            "(\"soldr-home\", \"/root/.soldr\")",
            "\"soldr-home\"",
        ] {
            assert!(
                soldr.body.contains(required_marker),
                "docker_build_soldr.py must contain `{required_marker}` so \
                 the helper image has a baked soldr install and a persistent \
                 soldr home volume",
            );
        }
    }

    /// Every bundled tool body must start with the canonical PEP 723
    /// uv-run shebang. Two reasons:
    ///   1. `clud tool run` execs `uv run <path>` explicitly, but users
    ///      who copy the installed file out of `~/.clud/tools/` and try
    ///      `./tool.py` expect the shebang to take it from there.
    ///   2. It's a one-line drift check — a tool author who skips the
    ///      shebang has likely also skipped the PEP 723 block (lint_deadcode
    ///      did exactly this; see the fix in this commit's sibling edit).
    #[test]
    fn bundled_tools_have_uv_run_shebang() {
        const SHEBANG: &str = "#!/usr/bin/env -S uv run --script";
        for tool in BUNDLED_TOOLS {
            assert!(
                tool.body.starts_with(SHEBANG),
                "tool {} must start with `{SHEBANG}` — the convention every \
                 other bundled tool follows, so a hand-run via `./tool.py` \
                 dispatches to uv the same way `clud tool run` does",
                tool.rel_path,
            );
        }
    }

    /// Every bundled tool body must declare `requires-python = ">=3.11"`
    /// in its PEP 723 inline metadata. The fleet baseline; older targets
    /// would let a tool slip in that uses 3.11-only syntax without anyone
    /// noticing on the dev box (which is typically 3.13+). 3.11 is the
    /// oldest CPython that still receives security fixes at the time of
    /// writing.
    #[test]
    fn bundled_tools_declare_requires_python_3_11() {
        const REQUIRED: &str = "requires-python = \">=3.11\"";
        for tool in BUNDLED_TOOLS {
            assert!(
                tool.body.contains(REQUIRED),
                "tool {} must declare `{REQUIRED}` in its PEP 723 metadata block",
                tool.rel_path,
            );
        }
    }

    /// Every `Killable` tool with a `command_timeout` over 5 minutes must
    /// either set a `progress_timeout` or carry an explicit
    /// `quiet_ok: true` (indicating that long stretches of silence are
    /// expected). The default `progress_timeout: None` + multi-hour cap
    /// is the silent-hang failure mode docker tools were hitting (full
    /// 60-min wait on a hung daemon) before the sweep commit.
    #[test]
    fn long_running_killables_declare_progress_intent() {
        const FIVE_MIN: Duration = Duration::from_secs(60 * 5);
        for tool in BUNDLED_TOOLS {
            if !matches!(tool.kill_semantics, KillSemantics::Killable) {
                continue;
            }
            if tool.command_timeout <= FIVE_MIN {
                continue;
            }
            assert!(
                tool.progress_timeout.is_some() || tool.quiet_ok,
                "tool {} is Killable with command_timeout={:?} > 5 min but \
                 sets progress_timeout=None and quiet_ok=false. Pick one: \
                 set progress_timeout to catch silent hangs, or set \
                 quiet_ok=true if long silences are expected for this tool.",
                tool.rel_path,
                tool.command_timeout,
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
