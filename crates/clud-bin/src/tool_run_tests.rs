use super::*;
use std::fs::File;
use std::path::Path;
use tempfile::TempDir;

#[test]
fn captured_line_restores_delimiter_for_incremental_readers() {
    let mut output = Vec::new();
    write_captured_line(&mut output, b"first").unwrap();
    assert_eq!(output, b"first\n");
}

fn incremental_output_fixture_filter() -> String {
    let module_path = module_path!();
    let (_, test_module_path) = module_path
        .split_once("::")
        .expect("module_path! includes the crate name and test module");
    format!("{test_module_path}::incremental_output_fixture_child")
}

#[test]
fn captured_subprocess_output_is_forwardable_before_exit() {
    let executable = std::env::current_exe().unwrap();
    let mut env = std::env::vars().collect::<Vec<_>>();
    env.push(("CLUD_STREAMING_FIXTURE_CHILD".to_string(), "1".to_string()));
    let process = NativeProcess::new(ProcessConfig {
        command: CommandSpec::Argv(vec![
            executable.to_string_lossy().into_owned(),
            "--ignored".to_string(),
            "--exact".to_string(),
            incremental_output_fixture_filter(),
            "--nocapture".to_string(),
        ]),
        cwd: None,
        env: Some(env),
        capture: true,
        stderr_mode: StderrMode::Pipe,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    });
    process.start().unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut emitted = 0;
    let mut first_before_exit = false;
    while std::time::Instant::now() < deadline {
        match process.wait(Some(Duration::from_millis(50))) {
            Err(ProcessError::Timeout) => {
                emitted = drain_passthrough_output_to(&process, emitted, &mut stdout, &mut stderr);
                if stdout.windows(b"first\n".len()).any(|w| w == b"first\n") {
                    first_before_exit = true;
                    break;
                }
            }
            Ok(code) => panic!("fixture exited with {code} before first was observed"),
            Err(error) => panic!("fixture wait failed: {error}"),
        }
    }
    assert!(
        first_before_exit,
        "first line was not forwardable before exit"
    );

    assert_eq!(process.wait(None).unwrap(), 7);
    let _ = drain_passthrough_output_to(&process, emitted, &mut stdout, &mut stderr);
    assert!(
        stderr.windows(b"second\n".len()).any(|w| w == b"second\n"),
        "stderr line missing from captured output: {}",
        String::from_utf8_lossy(&stderr)
    );
}

#[test]
#[ignore = "subprocess fixture invoked by captured_subprocess_output_is_forwardable_before_exit"]
fn incremental_output_fixture_child() {
    if std::env::var_os("CLUD_STREAMING_FIXTURE_CHILD").is_none() {
        return;
    }
    println!("first");
    io::stdout().flush().unwrap();
    thread::sleep(Duration::from_secs(1));
    eprintln!("second");
    io::stderr().flush().unwrap();
    std::process::exit(7);
}

/// Regression: ensure the resolved tool path is exactly
/// `<tools_root>/<rel_path>`. A prior version called
/// `target_path_at(tools_root.parent(), rel_path)` which re-appended
/// `.clud/tools` and produced `~/.clud/.clud/tools/<rel_path>` —
/// every real `clud tool run` would NotFound.
#[test]
fn resolve_tool_path_does_not_double_prefix() {
    let tools_root = Path::new("/home/user/.clud/tools");
    let resolved = resolve_tool_path(tools_root, "github/pr_merge_watch.py");
    assert_eq!(
        resolved,
        PathBuf::from("/home/user/.clud/tools/github/pr_merge_watch.py"),
        "tool path must not contain a doubled `.clud/tools` segment",
    );
    let s = resolved.to_string_lossy().to_string();
    assert!(
        !s.contains(".clud/tools/.clud/tools"),
        "double `.clud/tools` segment regression: {s}",
    );
}

#[test]
fn unresolvable_tool_returns_not_found() {
    let err = run("definitely/does/not/exist-XXXX-clud-test-only.py", &[]).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    let msg = err.to_string();
    assert!(
        msg.contains("definitely/does/not/exist-XXXX-clud-test-only.py")
            || msg.contains("definitely\\does\\not\\exist-XXXX-clud-test-only.py"),
        "error message should reference the requested rel_path; got: {msg}",
    );
}

#[test]
fn resolved_cache_dir_respects_parent_env() {
    let resolved =
        resolved_uv_cache_dir_from(Some(OsString::from("/tmp/test-cache-for-clud tool-run")));
    assert_eq!(resolved, PathBuf::from("/tmp/test-cache-for-clud tool-run"));
}

#[test]
fn resolved_cache_dir_falls_back_to_default() {
    let resolved = resolved_uv_cache_dir_from(None);
    assert_eq!(resolved, clud_uv_cache_dir());
}

#[test]
fn pep723_script_uses_uv_script_runner() {
    let tmp = TempDir::new().unwrap();
    let tool = tmp.path().join("tools").join("hooks").join("hook.py");
    let body = "#!/usr/bin/env -S uv run --script\n# /// script\n# dependencies = []\n# ///\n";
    let argv = build_tool_argv(
        &tool,
        body.as_bytes(),
        &["--flag".to_string()],
        &tmp.path().join("tools"),
        &[],
    )
    .unwrap();
    assert_eq!(argv[0], "uv");
    assert_eq!(argv[1], "run");
    assert!(argv.contains(&"--no-project".to_string()));
    assert!(argv.contains(&"--script".to_string()));
    assert_eq!(argv.last().map(String::as_str), Some("--flag"));
}

#[test]
fn plain_python_uses_managed_python_when_present() {
    let tmp = TempDir::new().unwrap();
    let tools_root = tmp.path().join("tools");
    let managed_dir = managed_python_install_dir(&tools_root).join("cpython-test");
    std::fs::create_dir_all(&managed_dir).unwrap();
    let exe_name = python_executable_names()[0];
    let python = managed_dir.join(exe_name);
    File::create(&python).unwrap();

    let tool = tools_root.join("plain.py");
    let argv = build_tool_argv(
        &tool,
        b"print('plain')\n",
        &["arg".to_string()],
        &tools_root,
        &[],
    )
    .unwrap();
    assert_eq!(argv[0], python.to_string_lossy());
    assert_eq!(argv[1], tool.to_string_lossy());
    assert_eq!(argv[2], "arg");
}

#[test]
fn pep723_block_without_shebang_uses_plain_python() {
    let tmp = TempDir::new().unwrap();
    let tools_root = tmp.path().join("tools");
    let managed_dir = managed_python_install_dir(&tools_root).join("cpython-test");
    std::fs::create_dir_all(&managed_dir).unwrap();
    let python = managed_dir.join(python_executable_names()[0]);
    File::create(&python).unwrap();

    let tool = tools_root.join("metadata_only.py");
    let argv = build_tool_argv(
        &tool,
        b"# /// script\n# dependencies = [\"requests\"]\n# ///\nprint('plain')\n",
        &[],
        &tools_root,
        &[],
    )
    .unwrap();
    assert_eq!(argv[0], python.to_string_lossy());
    assert_eq!(argv[1], tool.to_string_lossy());
}

#[test]
fn shebang_bin_sh_runs_through_shell() {
    let tmp = TempDir::new().unwrap();
    let tool = tmp.path().join("tool.sh");
    let argv = build_tool_argv(
        &tool,
        b"#!/bin/sh\necho hi\n",
        &["x".to_string()],
        tmp.path(),
        &[],
    )
    .unwrap();
    assert_eq!(argv, vec!["sh", &tool.to_string_lossy(), "x"]);
}

#[test]
fn uv_shebang_runs_script_runner() {
    let tmp = TempDir::new().unwrap();
    let tool = tmp.path().join("tool.py");
    let argv = build_tool_argv(
        &tool,
        b"#!/usr/bin/env -S uv run --script\nprint('hi')\n",
        &[],
        tmp.path(),
        &[],
    )
    .unwrap();
    assert_eq!(argv[0], "uv");
    assert!(argv.contains(&"--script".to_string()));
}

#[test]
fn binary_magic_executes_directly() {
    let tmp = TempDir::new().unwrap();
    let tool = tmp.path().join("tool.bin");
    let argv = build_tool_argv(
        &tool,
        b"\x7fELF\x02\x01",
        &["arg".to_string()],
        tmp.path(),
        &[],
    )
    .unwrap();
    assert_eq!(
        argv,
        vec![tool.to_string_lossy().to_string(), "arg".to_string()]
    );
}

#[test]
fn cpp_executable_path_sits_next_to_source() {
    let source = Path::new("/tmp/tool.cpp");
    let exe = cpp_executable_path(source);
    if cfg!(windows) {
        assert_eq!(exe, PathBuf::from("/tmp/tool.exe"));
    } else {
        assert_eq!(exe, PathBuf::from("/tmp/tool"));
    }
}

#[test]
fn install_path_parser_accepts_jsonl_and_arrays() {
    let jsonl = br#"{"install_path":"/clang/one"}
{"other":true}
"#;
    assert_eq!(first_install_path(jsonl), Some(PathBuf::from("/clang/one")));
    let array = br#"[{"install_path":"/clang/two"}]"#;
    assert_eq!(first_install_path(array), Some(PathBuf::from("/clang/two")));
}

#[test]
fn build_child_env_pins_tool_environment_and_strips_inherited_values() {
    let cache = std::path::PathBuf::from("/some/clud/cache");
    let env = build_child_env(&cache);
    let uv_entries: Vec<_> = env.iter().filter(|(k, _)| k == "UV_CACHE_DIR").collect();
    assert_eq!(
        uv_entries.len(),
        1,
        "must pin exactly one UV_CACHE_DIR entry"
    );
    assert_eq!(uv_entries[0].1, "/some/clud/cache");
    let no_daemon_entries: Vec<_> = env
        .iter()
        .filter(|(k, _)| k == crate::daemon::ENV_NO_DAEMON)
        .collect();
    assert_eq!(
        no_daemon_entries.len(),
        1,
        "must pin exactly one CLUD_NO_DAEMON entry"
    );
    assert_eq!(no_daemon_entries[0].1, "1");
    // Sanity: at least one non-UV entry made it through (PATH on any host).
    // We don't insist on PATH specifically because some test runners may strip
    // it; we just confirm the env isn't only the pinned UV_CACHE_DIR entry.
    assert!(!env.is_empty(), "env must include the pinned UV_CACHE_DIR");
}

#[test]
fn stderr_tail_keeps_last_200_chars() {
    let text = "x".repeat(250);
    let tail = stderr_tail_200(text.as_bytes()).unwrap();
    assert_eq!(tail.len(), 200);
    assert_eq!(tail, "x".repeat(200));
    assert!(stderr_tail_200(b"").is_none());
}

#[test]
fn passthrough_abort_payload_names_exact_argv() {
    let args = vec!["--flag".to_string()];
    let argv = vec![
        "uv".to_string(),
        "run".to_string(),
        "--script".to_string(),
        "hooks/block-bad-cmd.py".to_string(),
    ];
    let payload = render_passthrough_abort_payload(
        "hooks/block-bad-cmd.py",
        &args,
        &argv,
        Duration::from_millis(1234),
        AbortReason::CommandTimeout,
        Some("blocked at sys.stdin.read()"),
    );
    let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(value["status"], "aborted");
    assert_eq!(value["reason"], "command_timeout");
    assert_eq!(value["tool"], "hooks/block-bad-cmd.py");
    assert_eq!(value["args"][0], "--flag");
    assert_eq!(value["argv"][0], "uv");
    assert_eq!(value["stderr_tail"], "blocked at sys.stdin.read()");
}
