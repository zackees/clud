use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;

use super::*;
use crate::toast::{Severity, Toast, ToastEvent};

fn writer_in(dir: &Path, pid: u32) -> StatusStateWriter {
    StatusStateWriter::new(state_path(dir, pid))
}

#[test]
fn a_published_toast_round_trips_through_the_state_file() {
    let dir = tempfile::tempdir().unwrap();
    let writer = writer_in(dir.path(), 11);
    writer.publish(ToastEvent::Show(Toast::new(
        "cpu",
        "cpu 200 %",
        Severity::Warn,
        Instant::now(),
    )));
    let toast = read_live_toast(writer.path(), now_ms()).unwrap();
    assert_eq!(toast.text, "cpu 200 %");
    assert_eq!(toast.severity, Severity::Warn);
    assert_eq!(toast.expires_ms, None);
    writer.publish(ToastEvent::Close { key: "cpu".into() });
    assert!(read_live_toast(writer.path(), now_ms()).is_none());
}

#[test]
fn an_expiring_toast_disappears_on_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let writer = writer_in(dir.path(), 12);
    writer.publish(ToastEvent::Show(
        Toast::new("cpu", "back to normal", Severity::Info, Instant::now())
            .expiring_after(Duration::from_secs(10)),
    ));
    let now = now_ms();
    assert!(read_live_toast(writer.path(), now).is_some());
    assert!(read_live_toast(writer.path(), now + 11_000).is_none());
}

#[test]
fn an_orphaned_live_toast_goes_stale() {
    let dir = tempfile::tempdir().unwrap();
    let path = state_path(dir.path(), 13);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let state = StatusState {
        updated_ms: 1_000,
        toast: Some(StatusToast {
            text: "cpu 400 %".into(),
            severity: Severity::Alert,
            expires_ms: None,
        }),
        usage: None,
    };
    std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    let stale = u64::try_from(STALE_AFTER.as_millis()).unwrap();
    assert!(read_live_toast(&path, 1_000 + stale).is_some());
    assert!(read_live_toast(&path, 1_000 + stale + 1).is_none());
}

#[test]
fn dropping_the_writer_removes_the_file_and_garbage_reads_as_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = {
        let writer = writer_in(dir.path(), 14);
        writer.publish(ToastEvent::Show(Toast::new(
            "cpu",
            "x",
            Severity::Info,
            Instant::now(),
        )));
        assert!(writer.path().is_file());
        writer.path().to_path_buf()
    };
    assert!(!path.exists());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"{not json").unwrap();
    assert!(read_live_toast(&path, now_ms()).is_none());
    assert!(read_live_toast(&dir.path().join("missing.json"), now_ms()).is_none());
}

#[test]
fn the_rendered_line_is_coloured_and_control_free() {
    let line = render_line(&StatusToast {
        text: "cpu\x1b[2J 300 %".into(),
        severity: Severity::Alert,
        expires_ms: None,
    });
    assert!(line.starts_with("\x1b[1;38;5;203mclud \u{b7} cpu"));
    assert!(line.ends_with("\x1b[0m"));
    assert!(
        !line.contains("\x1b[2J"),
        "control bytes in toast text are stripped"
    );
}

#[test]
fn provider_usage_is_persistent_distinct_and_stale_safe() {
    let dir = tempfile::tempdir().unwrap();
    let writer = writer_in(dir.path(), 15);
    writer.publish_usage(StatusUsage {
        provider: "codex".into(),
        model: "gpt-5.6-terra".into(),
        request_count: 2,
        cached_input_tokens: 12_345,
        uncached_input_tokens: 67_890,
        output_tokens: 12,
        cache_health: "healthy".into(),
    });
    let usage = read_live_usage(writer.path(), now_ms()).expect("live usage");
    assert_eq!(usage.cached_input_tokens, 12_345);
    assert_eq!(usage.uncached_input_tokens, 67_890);
    let line = render_usage(&usage);
    assert!(
        line.contains("read 12.3K cached / 67.8K uncached"),
        "{line}"
    );
    assert!(line.contains("write 12"), "{line}");
    assert!(line.contains("gpt-5.6-terra"), "{line}");
    let stale = now_ms().saturating_add(u64::try_from(STALE_AFTER.as_millis()).unwrap() + 1);
    assert!(read_live_usage(writer.path(), stale).is_none());
}

#[test]
fn a_delayed_older_usage_snapshot_cannot_replace_newer_totals() {
    let dir = tempfile::tempdir().unwrap();
    let writer = writer_in(dir.path(), 16);
    let snapshot = |request_count, uncached_input_tokens| StatusUsage {
        provider: "codex".into(),
        model: "gpt-5.6-terra".into(),
        request_count,
        cached_input_tokens: 0,
        uncached_input_tokens,
        output_tokens: request_count,
        cache_health: "healthy".into(),
    };
    writer.publish_usage(snapshot(2, 200));
    // This simulates an older worker acquiring the status writer after the
    // second terminal response already committed its aggregate snapshot.
    writer.publish_usage(snapshot(1, 100));
    assert_eq!(
        read_live_usage(writer.path(), now_ms()),
        Some(snapshot(2, 200)),
        "the visible ledger must remain monotonic"
    );
}

#[test]
fn the_command_quotes_paths_and_carries_the_chain_losslessly() {
    let chain = "bash ~/.claude/statusline.sh | tr 'a' \"b\"";
    let cmd = statusline_command(
        Path::new("/opt/my tools/clud"),
        4242,
        Path::new("/home/u/.clud/state"),
        Some(chain),
        false,
    )
    .unwrap();
    assert!(cmd.starts_with(
        "\"/opt/my tools/clud\" statusline --session-pid 4242 --state-dir \"/home/u/.clud/state\""
    ));
    let encoded = cmd.rsplit(' ').next().unwrap();
    assert_eq!(decode_chain(encoded).as_deref(), Some(chain));
    assert!(
        !encoded.contains(['+', '/', '=']),
        "URL-safe, unpadded, shell-inert"
    );
}

#[test]
fn windows_commands_use_forward_slashes() {
    let cmd = statusline_command(
        Path::new(r"C:\Users\me\AppData\Roaming\Python\Scripts\clud.exe"),
        7,
        Path::new(r"C:\Users\me\.clud\state"),
        None,
        true,
    )
    .unwrap();
    assert_eq!(
        cmd,
        "\"C:/Users/me/AppData/Roaming/Python/Scripts/clud.exe\" statusline --session-pid 7 --state-dir \"C:/Users/me/.clud/state\""
    );
}

#[test]
fn paths_that_would_need_shell_escaping_are_refused() {
    for bad in ["/tmp/a\"b/clud", "/tmp/$HOME/clud", "/tmp/`x`/clud"] {
        assert!(
            statusline_command(Path::new(bad), 1, Path::new("/s"), None, false).is_none(),
            "{bad}"
        );
    }
}

#[test]
fn only_real_user_commands_are_chained() {
    assert_eq!(chained_command(None), None);
    assert_eq!(
        chained_command(Some(&json!({"type": "command", "command": " echo hi "}))),
        Some("echo hi".into())
    );
    assert_eq!(
        chained_command(Some(&json!({"command": "echo hi"}))),
        Some("echo hi".into())
    );
    assert_eq!(
        chained_command(Some(&json!({"type": "static", "command": "x"}))),
        None
    );
    assert_eq!(
        chained_command(Some(&json!({"type": "command", "command": "  "}))),
        None
    );
    let ours = statusline_command(Path::new("/c"), 1, Path::new("/s"), None, false).unwrap();
    assert_eq!(
        chained_command(Some(&json!({"type": "command", "command": ours}))),
        None,
        "clud's own command is never chained into itself"
    );
}

#[test]
fn the_composed_setting_keeps_user_fields_and_never_slows_refresh() {
    let fresh = compose_setting(None, "cmd".into());
    assert_eq!(
        fresh,
        json!({"type": "command", "command": "cmd", "refreshInterval": REFRESH_INTERVAL_SECS})
    );
    let user = json!({"type": "command", "command": "theirs", "padding": 2, "refreshInterval": 30});
    let merged = compose_setting(Some(&user), "ours".into());
    assert_eq!(merged["padding"], 2);
    assert_eq!(merged["command"], "ours");
    assert_eq!(merged["refreshInterval"], REFRESH_INTERVAL_SECS);
    let faster = compose_setting(Some(&json!({"refreshInterval": 1})), "ours".into());
    assert_eq!(faster["refreshInterval"], 1);
}

#[test]
fn discovery_follows_claude_codes_precedence() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let project = root.path().join("work").join("repo");
    let nested = project.join("src").join("deep");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(project.join(".claude")).unwrap();
    std::fs::create_dir_all(&nested).unwrap();
    let write = |path: std::path::PathBuf, cmd: &str| {
        std::fs::write(
            path,
            json!({"statusLine": {"type": "command", "command": cmd}}).to_string(),
        )
        .unwrap();
    };

    assert_eq!(discover_user_statusline(None, &nested, Some(&home)), None);
    write(home.join(".claude/settings.json"), "home");
    let found = |explicit: Option<&Value>| {
        discover_user_statusline(explicit, &nested, Some(&home))
            .map(|v| v["command"].as_str().unwrap().to_string())
    };
    assert_eq!(found(None).as_deref(), Some("home"));
    write(project.join(".claude/settings.json"), "project");
    assert_eq!(found(None).as_deref(), Some("project"));
    write(project.join(".claude/settings.local.json"), "local");
    assert_eq!(found(None).as_deref(), Some("local"));
    let explicit = json!({"statusLine": {"type": "command", "command": "explicit"}});
    assert_eq!(found(Some(&explicit)).as_deref(), Some("explicit"));
}

#[test]
fn a_project_settings_file_without_a_status_line_falls_back_to_home() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let project = root.path().join("repo");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(project.join(".claude")).unwrap();
    std::fs::write(project.join(".claude/settings.json"), r#"{"model": "x"}"#).unwrap();
    std::fs::write(
        home.join(".claude/settings.json"),
        r#"{"statusLine": {"type": "command", "command": "home"}}"#,
    )
    .unwrap();
    let found = discover_user_statusline(None, &project, Some(&home)).unwrap();
    assert_eq!(found["command"], "home");
}

/// Runs a real shell on every CI lane: `sh -c` on Linux and macOS, Git Bash
/// or `cmd` on Windows.
#[test]
fn the_chain_runs_through_the_platform_shell_with_claudes_stdin() {
    let out = run_chain("echo chained-status", b"{\"model\":{}}").expect("chain ran");
    assert!(
        String::from_utf8_lossy(&out).contains("chained-status"),
        "stdout: {:?}",
        String::from_utf8_lossy(&out)
    );
}

#[cfg(unix)]
#[test]
fn the_chain_receives_the_session_json_on_stdin() {
    let out = run_chain("cat", b"{\"session_id\":\"abc\"}").expect("chain ran");
    assert!(String::from_utf8_lossy(&out).contains("\"session_id\":\"abc\""));
}

#[test]
fn render_prints_the_user_line_then_the_live_toast() {
    let dir = tempfile::tempdir().unwrap();
    let writer = writer_in(dir.path(), 21);
    writer.publish(ToastEvent::Show(Toast::new(
        "cpu",
        "cpu 180 %",
        Severity::Warn,
        Instant::now(),
    )));
    let args = RunArgs {
        session_pid: 21,
        state_dir: dir.path().to_path_buf(),
        chain_b64: Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("echo user-line")),
    };
    let mut out = Vec::new();
    render_into(&mut out, &args, b"{}", now_ms());
    let text = String::from_utf8_lossy(&out);
    let user = text.find("user-line").expect("user line first");
    let toast = text.find("cpu 180 %").expect("toast second");
    assert!(user < toast, "{text:?}");

    let quiet = RunArgs {
        session_pid: 99,
        state_dir: dir.path().to_path_buf(),
        chain_b64: None,
    };
    let mut out = Vec::new();
    render_into(&mut out, &quiet, b"{}", now_ms());
    assert!(out.is_empty(), "no user line and no toast prints nothing");
}

#[test]
fn render_prints_usage_between_the_user_line_and_toast() {
    let dir = tempfile::tempdir().unwrap();
    let writer = writer_in(dir.path(), 22);
    writer.publish_usage(StatusUsage {
        provider: "codex".into(),
        model: "gpt-5.6-terra".into(),
        request_count: 1,
        cached_input_tokens: 5,
        uncached_input_tokens: 6,
        output_tokens: 7,
        cache_health: "cold".into(),
    });
    writer.publish(ToastEvent::Show(Toast::new(
        "cpu",
        "cpu 180 %",
        Severity::Warn,
        Instant::now(),
    )));
    let args = RunArgs {
        session_pid: 22,
        state_dir: dir.path().to_path_buf(),
        chain_b64: Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("echo user-line")),
    };
    let mut out = Vec::new();
    render_into(&mut out, &args, b"{}", now_ms());
    let text = String::from_utf8_lossy(&out);
    assert!(text.find("user-line").unwrap() < text.find("gpt-5.6-terra").unwrap());
    assert!(text.find("gpt-5.6-terra").unwrap() < text.find("cpu 180 %").unwrap());
}
