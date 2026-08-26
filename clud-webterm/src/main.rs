use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use base64::Engine as _;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::{Emitter, State};

const ENV_WEBTERM: &str = "CLUD_WEBTERM";

struct PtyTab {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

#[derive(Default)]
struct PtyState {
    tabs: Mutex<HashMap<u64, PtyTab>>,
    next_id: AtomicU64,
}

#[derive(Clone, Serialize)]
struct PtyData {
    tab_id: u64,
    data: String,
}

#[derive(Clone, Serialize)]
struct PtyExit {
    tab_id: u64,
}

fn initial_command_argv() -> Vec<String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--") {
        args.remove(0);
    }
    args
}

fn default_shell_argv() -> Vec<String> {
    if cfg!(windows) {
        vec!["cmd.exe".to_string()]
    } else {
        vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())]
    }
}

fn command_from_argv(argv: Vec<String>) -> Result<CommandBuilder, String> {
    let mut argv = argv;
    if argv.is_empty() {
        argv = default_shell_argv();
    }
    let program = argv.remove(0);
    let mut command = CommandBuilder::new(program);
    for arg in argv {
        command.arg(arg);
    }
    command.env(ENV_WEBTERM, "1");
    if let Ok(cwd) = std::env::current_dir() {
        command.cwd(cwd);
    }
    Ok(command)
}

#[tauri::command]
fn initial_command() -> Vec<String> {
    initial_command_argv()
}

#[tauri::command]
fn start_tab(
    app: tauri::AppHandle,
    state: State<PtyState>,
    cols: u16,
    rows: u16,
    argv: Vec<String>,
) -> Result<u64, String> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| format!("openpty: {error}"))?;
    let child = pair
        .slave
        .spawn_command(command_from_argv(argv)?)
        .map_err(|error| format!("spawn terminal: {error}"))?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| format!("clone PTY reader: {error}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|error| format!("take PTY writer: {error}"))?;
    let tab_id = state.next_id.fetch_add(1, Ordering::Relaxed) + 1;
    state.tabs.lock().map_err(|_| "PTY state poisoned")?.insert(
        tab_id,
        PtyTab {
            writer,
            master: pair.master,
            child,
        },
    );

    std::thread::spawn(move || {
        let mut buf = [0_u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(size) => {
                    let data = base64::engine::general_purpose::STANDARD.encode(&buf[..size]);
                    let _ = app.emit("pty://data", PtyData { tab_id, data });
                }
            }
        }
        let _ = app.emit("pty://exit", PtyExit { tab_id });
    });
    Ok(tab_id)
}

#[tauri::command]
fn write_pty(state: State<PtyState>, tab_id: u64, data: String) -> Result<(), String> {
    let mut tabs = state.tabs.lock().map_err(|_| "PTY state poisoned")?;
    let tab = tabs
        .get_mut(&tab_id)
        .ok_or_else(|| format!("unknown terminal tab {tab_id}"))?;
    tab.writer
        .write_all(data.as_bytes())
        .map_err(|error| format!("write terminal: {error}"))?;
    tab.writer
        .flush()
        .map_err(|error| format!("flush terminal: {error}"))
}

#[tauri::command]
fn resize_pty(state: State<PtyState>, tab_id: u64, cols: u16, rows: u16) -> Result<(), String> {
    let tabs = state.tabs.lock().map_err(|_| "PTY state poisoned")?;
    let tab = tabs
        .get(&tab_id)
        .ok_or_else(|| format!("unknown terminal tab {tab_id}"))?;
    tab.master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| format!("resize terminal: {error}"))
}

#[tauri::command]
fn close_tab(state: State<PtyState>, tab_id: u64) -> Result<(), String> {
    let mut tab = state
        .tabs
        .lock()
        .map_err(|_| "PTY state poisoned")?
        .remove(&tab_id)
        .ok_or_else(|| format!("unknown terminal tab {tab_id}"))?;
    tab.child
        .kill()
        .map_err(|error| format!("stop terminal: {error}"))
}

fn main() {
    // This must run before Tauri initializes so installed-wheel CI can prove
    // that Windows loaded the companion (including its manifest) without
    // opening a desktop window. See #1033.
    if std::env::args().skip(1).eq(["--startup-check".to_string()]) {
        return;
    }
    tauri::Builder::default()
        .manage(PtyState::default())
        .invoke_handler(tauri::generate_handler![
            initial_command,
            start_tab,
            write_pty,
            resize_pty,
            close_tab
        ])
        .run(tauri::generate_context!())
        .expect("error while running clud web terminal");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_command_selects_a_platform_shell() {
        assert!(command_from_argv(Vec::new()).is_ok());
    }

    #[test]
    fn command_argv_preserves_multiple_arguments() {
        assert!(command_from_argv(vec!["clud".to_string(), "--codex".to_string()]).is_ok());
    }
}
