//! Long-lived daemon stub for the reaper survival suite (#674).
//!
//! A clud session must never kill a daemon started inside it. The daemons that
//! matter — zccache, soldr, sccache, `FBuildWorker`, dockerd, language servers
//! — detach in three *different* ways, and clud's spare-list keys on the
//! resulting OS signals, not on names. This binary reproduces each of those
//! **signal shapes** so the survival tests are hermetic:
//!
//! | Mode | Signal shape | Real-world analogue |
//! |---|---|---|
//! | `spawn-breakaway` | outside the caller's Job Object, daemon marker set | anything using `spawn_daemon_breaking_away_from_job` |
//! | `spawn-marked` | **inside** the Job Object, daemon marker set | zccache, soldr — the load-bearing marker case |
//! | `spawn-detached` | inside the Job Object, **no** marker, own detach, owns a listening socket | **sccache**, `FBuildWorker`, language servers |
//!
//! `spawn-detached` is the hard case and the reason this crate exists: those
//! daemons never call `running-process`, so the cooperative marker gives them
//! nothing. Depending on a real sccache being installed on a CI runner would
//! make the test non-hermetic; reproducing the signal shape is what the code
//! actually keys on.
//!
//! Every mode writes the served child's PID to the given path and exits, so a
//! test can assert on that PID's survival without owning the child.
//!
//! Raw `std::process::Command` is deliberate here, and permitted: `testbins/`
//! is outside `ci/banned_imports.py`'s scan. `spawn-detached` must detach
//! *without* `running-process`, because a `NativeProcess` would attach its own
//! containment and set the very marker whose absence is under test.

use std::io::Write;
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;

/// How long a served stub stays alive. Long enough for a session lifecycle,
/// short enough that a crashed test does not leave it around for an hour.
const SERVE_SECONDS: u64 = 120;

fn usage() -> ! {
    eprintln!(
        "usage: daemon-stub <serve|spawn-breakaway|spawn-marked|spawn-detached> <pid-file-path>"
    );
    std::process::exit(2);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(mode), Some(pid_path)) = (args.next(), args.next()) else {
        usage();
    };
    let pid_path = Path::new(&pid_path).to_path_buf();

    match mode.as_str() {
        "serve" => serve(&pid_path),
        "spawn-breakaway" => spawn_via_running_process(&pid_path, true),
        "spawn-marked" => spawn_via_running_process(&pid_path, false),
        "spawn-detached" => spawn_detached(&pid_path),
        _ => usage(),
    }
}

/// Be the daemon: own a listening endpoint and stay up.
///
/// The listener is the signal that makes a process *discoverable and reusable
/// by later, unrelated invocations* — which is what a build cache or language
/// server is, whatever it declared.
fn serve(pid_path: &Path) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub listener");
    write_pid(pid_path, std::process::id());
    // Hold the listener open for the whole life of the process.
    std::thread::sleep(std::time::Duration::from_secs(SERVE_SECONDS));
    drop(listener);
}

/// Detach through `running-process`, which sets the cooperative daemon marker.
///
/// `breakaway` additionally leaves the caller's Job Object, which is how a
/// build-cache server that expects to outlive every session detaches.
fn spawn_via_running_process(pid_path: &Path, breakaway: bool) {
    let exe = std::env::current_exe().expect("current exe");
    let mut command = Command::new(exe);
    command.arg("serve").arg(pid_path);

    let child = if breakaway {
        running_process::spawn::spawn_daemon_breaking_away_from_job(&mut command)
    } else {
        running_process::spawn::spawn_daemon(&mut command)
    }
    .expect("spawn daemon stub");
    // The served child writes its own PID; this is the launcher's view of it,
    // written only if the child has not got there first.
    let _ = child.id();
}

/// Detach the way sccache does: no marker, no job breakaway, just the OS
/// primitives for "do not die with my shell".
///
/// Windows: `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` — no console, its own
/// group, but still inside whatever Job Object the launcher belongs to.
/// Unix: `setsid()` in the child, making it a session leader.
fn spawn_detached(pid_path: &Path) {
    let exe = std::env::current_exe().expect("current exe");
    let mut command = Command::new(exe);
    command.arg("serve").arg(pid_path);
    // Explicitly strip the marker so this can never accidentally inherit a
    // daemon declaration from whatever launched it.
    command.env_remove("RUNNING_PROCESS_IS_DAEMON");

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    // Deliberately never waited on: outliving its launcher is the entire
    // behaviour under test, and the launcher exits immediately below.
    #[allow(clippy::zombie_processes)]
    let child = command.spawn().expect("spawn detached daemon stub");
    let _ = child.id();
}

fn write_pid(path: &Path, pid: u32) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file = std::fs::File::create(path).expect("create pid file");
    write!(file, "{pid}").expect("write pid");
}
