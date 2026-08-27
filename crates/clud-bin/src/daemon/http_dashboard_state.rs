use super::*;

pub(super) fn build_dashboard_state(
    state_dir: &Path,
    gc_tx: Option<&mpsc::Sender<RegistryMsg>>,
    ipc_port: u16,
    started_at_unix: i64,
    live_sessions: Vec<LiveSession>,
) -> Result<DashboardState, String> {
    let now_unix = current_unix();

    let mut sessions = read_session_views(state_dir).unwrap_or_default();
    // Logical API sessions deliberately appear as non-attachable rows rather
    // than worker snapshots. Their IDs may be used by API-aware CLI commands,
    // but never by the attach transport.
    merge_api_sessions(&mut sessions, state_dir);
    merge_launch_records(&mut sessions, launch_log::read_recent(state_dir));
    // Issue #190: surface direct-runner sessions (default `clud` invocation
    // path) by reading the redb session registry. The on-disk snapshot
    // files are only written by the centralized daemon worker, so without
    // this merge the dashboard would render "no sessions recorded" even
    // while a foreground `clud` is clearly running. The caller — typically
    // `handle_state` via `default_live_sessions_provider` — does the
    // actual registry read so tests can inject mock data without env-var
    // entanglement.
    merge_registry_sessions(&mut sessions, live_sessions);
    let live_session_count = sessions.iter().filter(|s| s.live).count();

    let gc_rows = match gc_tx {
        Some(tx) => match send_gc_op(tx, GcOp::List { kind: None }) {
            Ok(GcReply::ListOk { rows }) => rows,
            Ok(GcReply::Error { message }) => return Err(format!("gc.list failed: {message}")),
            Ok(other) => return Err(format!("gc.list unexpected reply: {other:?}")),
            Err(err) => return Err(err),
        },
        None => Vec::new(),
    };

    let repos = match gc_tx {
        Some(tx) => match send_gc_op(tx, GcOp::ListRepoVisits) {
            Ok(GcReply::RepoVisitsOk { rows }) => rows,
            Ok(GcReply::Error { message }) => {
                return Err(format!("gc.list_repo_visits failed: {message}"));
            }
            Ok(other) => {
                return Err(format!("gc.list_repo_visits unexpected reply: {other:?}"));
            }
            Err(err) => return Err(err),
        },
        None => Vec::new(),
    };

    let mut gc_by_kind: HashMap<String, usize> = HashMap::new();
    for row in &gc_rows {
        *gc_by_kind.entry(row.kind.clone()).or_insert(0) += 1;
    }

    let ctrl_c_events =
        ctrl_c_track::read_recent_events(state_dir, ctrl_c_track::DASHBOARD_EVENT_LIMIT);

    let stats = Stats {
        session_count: sessions.len(),
        live_session_count,
        gc_count: gc_rows.len(),
        gc_by_kind,
        repo_count: repos.len(),
    };

    // The daemon RPC returns the sampler's cached snapshot; this HTTP worker
    // never does an expensive process-table scan of its own.
    let mut process_tree = super::super::client::daemon_client_proc_snapshot(state_dir, 0)
        .ok()
        .and_then(|snapshot| serde_json::to_value(snapshot).ok())
        .unwrap_or(serde_json::Value::Null);
    let cwd_by_session: HashMap<String, String> = sessions
        .iter()
        .filter_map(|session| session.cwd.clone().map(|cwd| (session.id.clone(), cwd)))
        .collect();
    if let Some(rows) = process_tree
        .get_mut("rows")
        .and_then(serde_json::Value::as_array_mut)
    {
        for row in rows {
            let cwd = row
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|id| cwd_by_session.get(id))
                .cloned()
                .unwrap_or_else(|| "-".to_string());
            row["cwd"] = serde_json::Value::String(cwd);
        }
    }

    Ok(DashboardState {
        daemon: DaemonStateView {
            pid: std::process::id(),
            ipc_port,
            dashboard_port: read_dashboard_port(state_dir).ok().flatten(),
            started_at_unix,
            now_unix,
            uptime_secs: (now_unix - started_at_unix).max(0) as u64,
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        sessions,
        gc: gc_rows,
        repos,
        ctrl_c_events,
        stats,
        process_tree,
    })
}

fn read_session_views(state_dir: &Path) -> io::Result<Vec<SessionView>> {
    let mut out = Vec::new();
    let dir = sessions_dir(state_dir);
    let entries = match fs::read_dir(&dir) {
        Ok(it) => it,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(err) => return Err(err),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(snap) = read_json_file::<SessionSnapshot>(&path) else {
            continue;
        };
        let live = snap.exit_code.is_none() && identity_is_alive(&snap.worker_identity());
        out.push(SessionView {
            id: snap.id,
            kind: match snap.kind {
                SessionKind::Subprocess => "subprocess".to_string(),
                SessionKind::Pty => "pty".to_string(),
            },
            source: "daemon".to_string(),
            backend: snap.backend,
            launch_mode: snap.launch_mode,
            name: snap.name,
            cwd: snap.cwd,
            repo_root: snap.repo_root,
            command: snap.command,
            clud_argv: Vec::new(),
            clud_pid: None,
            created_at: snap.created_at,
            exited_at: snap.exited_at,
            duration_ms: match (snap.created_at, snap.exited_at) {
                (Some(start), Some(end)) => Some(end.saturating_sub(start)),
                _ => None,
            },
            detachable: snap.detachable,
            background: snap.background,
            attachable: snap.attachable,
            repeat_interval_secs: snap.repeat_interval_secs,
            repeat_next_run_at: snap.repeat_next_run_at,
            repeat_running: snap.repeat_running,
            exit_code: snap.exit_code,
            failure_reason: None,
            worker_port: snap.worker_port,
            live,
            ctrl_c: snap.ctrl_c.map(ctrl_c_profile_view),
        });
    }
    // Newest first.
    out.sort_by(|a, b| b.created_at.unwrap_or(0).cmp(&a.created_at.unwrap_or(0)));
    Ok(out)
}

fn merge_api_sessions(sessions: &mut Vec<SessionView>, state_dir: &Path) {
    for record in super::api_sessions::ApiSessionStore::new(state_dir).list().unwrap_or_default() {
        let live = matches!(record.state, super::api_sessions::ApiSessionState::Running | super::api_sessions::ApiSessionState::Interrupting | super::api_sessions::ApiSessionState::Starting);
        sessions.push(SessionView {
            id: record.id,
            kind: "api".to_string(),
            source: "api".to_string(),
            backend: Some(match record.backend { super::api_sessions::ApiSessionBackend::Claude => "claude", super::api_sessions::ApiSessionBackend::Codex => "codex" }.to_string()),
            launch_mode: Some("subprocess".to_string()),
            name: record.name,
            cwd: Some(record.cwd.display().to_string()),
            repo_root: None,
            command: Vec::new(),
            clud_argv: Vec::new(),
            clud_pid: None,
            created_at: Some(record.created_at_ms),
            exited_at: None,
            duration_ms: None,
            detachable: false,
            background: false,
            attachable: false,
            repeat_interval_secs: None,
            repeat_next_run_at: None,
            repeat_running: false,
            exit_code: None,
            failure_reason: record.last_error,
            worker_port: 0,
            live,
            ctrl_c: None,
        });
    }
    sessions.sort_by(|a, b| b.created_at.unwrap_or(0).cmp(&a.created_at.unwrap_or(0)));
}

/// Merge live rows from the redb session registry into the dashboard's
/// session list (issue #190). Direct-runner `clud` invocations never
/// produce a `SessionSnapshot` JSON file but do register themselves in
/// the redb registry for the fork-bomb cap, so the registry is the only
/// place where they're visible. `live_sessions` is provided by the
/// caller — production wires in the real registry reader; tests pass
/// `Vec::new()` (or seeded data) to avoid env-var racing across the
/// `daemon::http::tests` module.
fn merge_registry_sessions(sessions: &mut Vec<SessionView>, live_sessions: Vec<LiveSession>) {
    for row in live_sessions {
        if sessions
            .iter()
            .any(|session| session.live && session.clud_pid == Some(row.pid))
        {
            continue;
        }
        let id = format!("direct-{}", row.pid);
        sessions.push(SessionView {
            id,
            kind: "direct".to_string(),
            source: "registry".to_string(),
            backend: row.backend.clone(),
            launch_mode: row.launch_mode.clone(),
            // Surface the backend selection (`claude` / `codex`) under the
            // session name column so users can tell which agent each
            // direct-runner row corresponds to.
            name: row.backend.clone(),
            cwd: row.cwd,
            repo_root: None,
            command: Vec::new(),
            clud_argv: Vec::new(),
            clud_pid: Some(row.pid),
            // `started_unix` is seconds; snapshot rows use milliseconds.
            // Convert so the dashboard's age formatter renders both the
            // same way without a per-kind unit-toggle.
            created_at: Some((row.started_unix.max(0) as u64) * 1000),
            exited_at: None,
            duration_ms: None,
            detachable: false,
            background: false,
            attachable: false,
            repeat_interval_secs: None,
            repeat_next_run_at: None,
            repeat_running: false,
            exit_code: None,
            failure_reason: None,
            worker_port: 0,
            // The registry already filtered by OS PID liveness probe.
            live: true,
            ctrl_c: None,
        });
    }

    // Newest first across the merged list.
    sessions.sort_by(|a, b| b.created_at.unwrap_or(0).cmp(&a.created_at.unwrap_or(0)));
}

fn merge_launch_records(sessions: &mut Vec<SessionView>, records: Vec<LaunchRecord>) {
    for record in records {
        // Launch records carry no start time, so this stays a bare-PID probe
        // (issue #558). It is display-only — the dashboard may briefly show a
        // finished launch as live if its PID was reused — and nothing acts on
        // the answer. Recording an identity here would mean widening the
        // launch-log schema, which is out of scope for this change.
        let live = record.exit_code.is_none() && pid_is_alive(record.clud_pid);
        let duration_ms = record.duration_ms();
        sessions.push(SessionView {
            id: format!("launch-{}", record.id),
            kind: record.source.clone(),
            source: record.source,
            backend: Some(record.backend.clone()),
            launch_mode: Some(record.launch_mode.clone()),
            name: Some(record.backend),
            cwd: record.cwd,
            repo_root: record.repo_root,
            command: record.command,
            clud_argv: record.clud_argv,
            clud_pid: Some(record.clud_pid),
            created_at: Some(record.launched_at_ms),
            exited_at: record.exited_at_ms,
            duration_ms,
            detachable: false,
            background: false,
            attachable: false,
            repeat_interval_secs: None,
            repeat_next_run_at: None,
            repeat_running: false,
            exit_code: record.exit_code,
            failure_reason: record.failure_reason,
            worker_port: 0,
            live,
            ctrl_c: None,
        });
    }
    sessions.sort_by(|a, b| b.created_at.unwrap_or(0).cmp(&a.created_at.unwrap_or(0)));
}

fn ctrl_c_profile_view(profile: CtrlCProfile) -> CtrlCProfileView {
    CtrlCProfileView {
        cli_pid: profile.cli_pid,
        cli_observed_at_ms: profile.cli_observed_at_ms,
        cli_handoff_at_ms: profile.cli_handoff_at_ms,
        cli_return_ready_at_ms: profile.cli_return_ready_at_ms,
        cli_handoff_ms: profile.cli_handoff_ms,
        daemon_received_at_ms: profile.daemon_received_at_ms,
        daemon_kill_started_at_ms: profile.daemon_kill_started_at_ms,
        daemon_kill_finished_at_ms: profile.daemon_kill_finished_at_ms,
        daemon_kill_ms: profile.daemon_kill_ms,
        fast_path: profile.fast_path,
    }
}

// ---------- IPC plumbing ----------

pub(super) fn send_gc_op(tx: &mpsc::Sender<RegistryMsg>, op: GcOp) -> Result<GcReply, String> {
    let (reply_tx, reply_rx) = mpsc::sync_channel::<GcReply>(1);
    tx.send(RegistryMsg::Op(GcRequestMsg { op, reply_tx }))
        .map_err(|_| "gc registry worker stopped".to_string())?;
    reply_rx
        .recv_timeout(WORKER_REPLY_TIMEOUT)
        .map_err(|_| "gc registry worker timed out".to_string())
}
