use super::*;

#[test]
fn latest_orphan_sweep_picks_the_newest_finished_event() {
    let lines = [
        r#"{"op":"orphan_sweep_started","ts_ms":100}"#,
        r#"{"op":"orphan_sweep_finished","ts_ms":200,"found":5,"reaped":3}"#,
        r#"{"op":"some_other_event","ts_ms":300}"#,
        r#"{"op":"orphan_sweep_finished","ts_ms":250,"found":1,"reaped":0}"#,
        r#"not json"#,
    ];
    let latest = latest_orphan_sweep(lines.iter().map(|s| s.to_string())).unwrap();
    assert_eq!(latest.ts_ms, 250);
    assert_eq!((latest.found, latest.reaped), (1, 0));
}

#[test]
fn latest_orphan_sweep_is_none_when_never_swept() {
    let lines = [r#"{"op":"orphan_sweep_started","ts_ms":100}"#];
    assert!(latest_orphan_sweep(lines.iter().map(|s| s.to_string())).is_none());
}

#[test]
fn sentinel_is_preferred_over_the_event_log() {
    let tmp = tempfile::TempDir::new().unwrap();
    // No sentinel yet → None, so the caller falls back to the log scan.
    assert!(sentinel_orphan_sweep(tmp.path()).is_none());

    std::fs::write(
        super::super::server::orphan_sweep_sentinel_path(tmp.path()),
        r#"{"ts_ms":1700000000000,"found":7,"reaped":3}"#,
    )
    .unwrap();
    let status = sentinel_orphan_sweep(tmp.path()).expect("sentinel parses");
    assert_eq!(status.ts_ms, 1_700_000_000_000);
    assert_eq!((status.found, status.reaped), (7, 3));

    // A corrupt sentinel falls back rather than panicking.
    std::fs::write(
        super::super::server::orphan_sweep_sentinel_path(tmp.path()),
        "{ not json",
    )
    .unwrap();
    assert!(sentinel_orphan_sweep(tmp.path()).is_none());
}

#[test]
fn orphan_status_also_searches_the_rotated_log() {
    // Regression: the shared event log rotates at 1 MB, so a burst of
    // unrelated events can push every sweep record into `<log>.1`. Reading
    // only the active file reported a false "no sweep recorded — STALE".
    let paths = orphan_status_log_paths(Path::new("state"));
    assert_eq!(paths.len(), 2, "active + one rotated backup");
    assert!(paths[0].ends_with("daemon-events.jsonl"));
    assert!(
        paths[1].ends_with("daemon-events.jsonl.1"),
        "second path must be the rotated backup, got {:?}",
        paths[1]
    );
}

#[test]
fn sweep_is_stale_past_two_intervals_or_never() {
    let interval = 60_000;
    assert!(
        sweep_is_stale(None, 1_000_000, interval),
        "never swept → stale"
    );
    let fresh = OrphanSweepStatus {
        ts_ms: 1_000_000,
        found: 0,
        reaped: 0,
    };
    // 1× interval later: fresh.
    assert!(!sweep_is_stale(Some(&fresh), 1_060_000, interval));
    // Exactly 2× is the boundary (not yet stale); just past it is stale.
    assert!(!sweep_is_stale(Some(&fresh), 1_120_000, interval));
    assert!(sweep_is_stale(Some(&fresh), 1_120_001, interval));
}

// Regression for the symptom reported after PR #151:
//   clud  (no args, interactive terminal)
//   → [clud] daemon session sess-...
//   → Error: Input must be provided either through stdin or as a
//     prompt argument when using --print
//
// Cause: the centralized daemon mapped `LaunchMode::Subprocess`
// straight through to `SessionKind::Subprocess`, and the worker's
// subprocess path uses `StdinMode::Null`. Claude saw no TTY,
// dropped into `--print` mode, and bailed for lack of a prompt.
// Interactive launches must force PTY so the worker hands the
// backend a pseudo-terminal it can drive.
#[test]
fn interactive_launch_forces_pty_even_when_plan_says_subprocess() {
    assert!(matches!(
        select_session_kind(LaunchMode::Subprocess, false, false),
        SessionKind::Pty
    ));
}

#[test]
fn interactive_pty_plan_stays_pty() {
    assert!(matches!(
        select_session_kind(LaunchMode::Pty, false, false),
        SessionKind::Pty
    ));
}

#[test]
fn prompted_subprocess_plan_stays_subprocess() {
    // `clud -p "hi"` — claude consumes the prompt arg, no TTY needed.
    assert!(matches!(
        select_session_kind(LaunchMode::Subprocess, false, true),
        SessionKind::Subprocess
    ));
}

#[test]
fn prompted_pty_plan_stays_pty() {
    assert!(matches!(
        select_session_kind(LaunchMode::Pty, false, true),
        SessionKind::Pty
    ));
}

#[test]
fn repeat_jobs_always_subprocess() {
    // Repeat jobs are background, have their own embedded prompt,
    // and never need an attached TTY — even for the interactive
    // case the override must win.
    assert!(matches!(
        select_session_kind(LaunchMode::Subprocess, true, false),
        SessionKind::Subprocess
    ));
    assert!(matches!(
        select_session_kind(LaunchMode::Pty, true, false),
        SessionKind::Subprocess
    ));
    assert!(matches!(
        select_session_kind(LaunchMode::Subprocess, true, true),
        SessionKind::Subprocess
    ));
}

#[test]
fn transcript_forces_centralized_daemon() {
    let args = Args {
        prompt: Some("hi".into()),
        message: None,
        continue_session: false,
        resume: None,
        claude: false,
        codex: false,
        harness: None,
        subprocess: false,
        pty: false,
        graphics: crate::graphics::GraphicsMode::Auto,
        graphics_image: None,
        demo_gfx_sixel: false,
        model: None,
        safe: false,
        unattended: false,
        allow_plan_mode: false,
        dry_run: false,
        detach: false,
        detachable: false,
        session_name: None,
        transcript: Some(PathBuf::from("session.log")),
        backlog_size: None,
        verbose: false,
        no_dnd: false,
        clean_worktrees: false,
        fix_hooks: false,
        no_fix_hooks: false,
        stale_after: "1d".into(),
        yes: false,
        force: false,
        experimental_daemon_centralized: false,
        daemon_state_dir: None,
        daemon_mode: None,
        no_daemon: false,
        keep_orphans: false,
        quiet_orphans: false,
        explain_orphans: false,
        no_cpu_banner: false,
        command: None,
        passthrough: Vec::new(),
        codex_config_overrides: Vec::new(),
    };
    assert!(experimental_enabled(&args));
}

#[test]
fn repeat_command_pins_resolved_provider_and_effective_harness() {
    let args = Args::parse_from_raw(
        [
            "clud",
            "--codex",
            "--harness",
            "claude",
            "loop",
            "--repeat",
            "1h",
            "task",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
    );
    let target =
        crate::backend::resolve_launch_target(args.claude, args.codex, args.harness, None, None)
            .unwrap();
    let plan = crate::command::build_launch_plan_for_target(&args, target, "claude");
    let command = build_repeat_once_command(&args, &plan).unwrap();
    assert!(command.windows(1).any(|part| part == ["--codex"]));
    assert!(command
        .windows(2)
        .any(|part| part == ["--harness", "claude"]));
}
