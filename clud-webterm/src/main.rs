// clud-webterm — Phase-1 fidelity spike for zackees/clud#929.
//
// Minimal Tauri v2 backend: open a single PTY, spawn a shell (or an agent),
// stream its output to the xterm.js frontend, and forward keystrokes and
// resizes back. The whole point of this spike is to answer ONE question:
// does a real `claude` / `codex` TUI render and drive correctly inside
// xterm.js hosted in a Tauri webview? Everything else is intentionally crude.
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

use std::io::{Read, Write};
use std::sync::Mutex;

use base64::Engine as _;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tauri::{Emitter, State};

/// Shared handles to the live PTY. `master` is kept for resize; `writer`
/// for forwarding keystrokes. Both are `None` until `start_pty` runs.
#[derive(Default)]
struct PtyState {
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
}

/// Build the command to run in the PTY.
///
/// Override with `WEBTERM_CMD` (whitespace-split, naive — fine for a spike),
/// e.g. `WEBTERM_CMD=claude` or `WEBTERM_CMD="clud --help"`. Defaults to the
/// platform shell so the spike is a usable terminal you can launch an agent
/// inside by hand.
fn build_command() -> CommandBuilder {
    let mut cmd = if let Ok(spec) = std::env::var("WEBTERM_CMD") {
        let mut parts = spec.split_whitespace();
        let prog = parts.next().unwrap_or("cmd.exe").to_string();
        let mut b = CommandBuilder::new(prog);
        for a in parts {
            b.arg(a);
        }
        b
    } else if cfg!(windows) {
        CommandBuilder::new("cmd.exe")
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        CommandBuilder::new(shell)
    };

    // Recursion guard: a clud launched inside this webterm must NOT re-spawn
    // another window (see clud#929 gotchas). Real clud will check this env.
    cmd.env("CLUD_WEBTERM", "1");
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }
    cmd
}

/// Open the PTY, spawn the child, and start pumping output to the frontend.
/// Idempotent-ish: a second call while a PTY is live is a no-op.
#[tauri::command]
fn start_pty(app: tauri::AppHandle, state: State<PtyState>, cols: u16, rows: u16) -> Result<(), String> {
    if state.master.lock().unwrap().is_some() {
        return Ok(()); // already running
    }

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
        .map_err(|e| format!("openpty: {e}"))?;

    let child = pair
        .slave
        .spawn_command(build_command())
        .map_err(|e| format!("spawn: {e}"))?;
    // Drop the slave so the master sees EOF when the child exits.
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("clone_reader: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("take_writer: {e}"))?;

    *state.writer.lock().unwrap() = Some(writer);
    *state.master.lock().unwrap() = Some(pair.master);

    // Output pump: read raw bytes, base64-encode (preserves partial UTF-8
    // sequences across chunk boundaries — xterm reassembles them), emit.
    std::thread::spawn(move || {
        let mut child = child;
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let payload = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
                    let _ = app.emit("pty://data", payload);
                }
                Err(_) => break,
            }
        }
        let _ = child.wait();
        let _ = app.emit("pty://exit", ());
    });

    Ok(())
}

/// Forward keystrokes (xterm `onData`) to the PTY.
#[tauri::command]
fn write_pty(state: State<PtyState>, data: String) -> Result<(), String> {
    if let Some(w) = state.writer.lock().unwrap().as_mut() {
        w.write_all(data.as_bytes()).map_err(|e| format!("write: {e}"))?;
        w.flush().map_err(|e| format!("flush: {e}"))?;
    }
    Ok(())
}

/// Resize the PTY when the xterm viewport changes.
#[tauri::command]
fn resize_pty(state: State<PtyState>, cols: u16, rows: u16) -> Result<(), String> {
    if let Some(m) = state.master.lock().unwrap().as_ref() {
        m.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| format!("resize: {e}"))?;
    }
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .manage(PtyState::default())
        .invoke_handler(tauri::generate_handler![start_pty, write_pty, resize_pty])
        .run(tauri::generate_context!())
        .expect("error while running clud-webterm spike");
}
