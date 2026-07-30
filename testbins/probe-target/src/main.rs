#[cfg(target_os = "windows")]
fn main() {
    if let Err(err) = run() {
        eprintln!("probe-target: {err}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("probe-target is Windows-only");
}

#[cfg(target_os = "windows")]
fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("sleep-then-exit") => {
            let ms = parse_u64(args.next(), "sleep ms")?;
            let code = parse_i32(args.next(), "exit code")?;
            println!("PROBE_READY pid={}", std::process::id());
            std::thread::sleep(std::time::Duration::from_millis(ms));
            std::process::exit(code);
        }
        Some("open-and-hold") => {
            let path = args.next().ok_or("missing file path")?;
            let ms = parse_u64(args.next(), "hold ms")?;
            let _file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|err| format!("open {path}: {err}"))?;
            println!("PROBE_READY pid={}", std::process::id());
            std::thread::sleep(std::time::Duration::from_millis(ms));
            Ok(())
        }
        Some("open-cycle") => {
            let path = args.next().ok_or("missing file path")?;
            let count = parse_u32(args.next(), "count")?;
            println!("PROBE_READY pid={}", std::process::id());
            for _ in 0..count {
                let _file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .map_err(|err| format!("open {path}: {err}"))?;
            }
            Ok(())
        }
        Some("spawn-chain") => {
            let depth = parse_u32(args.next(), "depth")?;
            let sleep_ms = parse_u64(args.next(), "sleep ms")?;
            let code = parse_i32(args.next(), "exit code")?;
            println!("PROBE_READY pid={} depth={depth}", std::process::id());
            std::thread::sleep(std::time::Duration::from_millis(350));
            if depth > 1 {
                let exe = std::env::current_exe().map_err(|err| err.to_string())?;
                let status = std::process::Command::new(exe)
                    .arg("spawn-chain")
                    .arg((depth - 1).to_string())
                    .arg(sleep_ms.to_string())
                    .arg(code.to_string())
                    .status()
                    .map_err(|err| format!("spawn child: {err}"))?;
                if !status.success() {
                    return Err(format!("child exited with {status}"));
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
            std::process::exit(code);
        }
        Some("try-breakaway") => {
            use std::os::windows::process::CommandExt;

            const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

            println!("PROBE_READY pid={}", std::process::id());
            std::thread::sleep(std::time::Duration::from_millis(500));
            let exe = std::env::current_exe().map_err(|err| err.to_string())?;
            match std::process::Command::new(exe)
                .arg("sleep-then-exit")
                .arg("50")
                .arg("0")
                .creation_flags(CREATE_BREAKAWAY_FROM_JOB)
                .status()
            {
                Ok(status) => {
                    println!("BREAKAWAY_SPAWNED status={status}");
                    Ok(())
                }
                Err(err) => {
                    println!(
                        "BREAKAWAY_DENIED os_error={}",
                        err.raw_os_error().unwrap_or_default()
                    );
                    Ok(())
                }
            }
        }
        Some(other) => Err(format!("unknown command {other:?}")),
        None => Err("missing command".into()),
    }
}

#[cfg(target_os = "windows")]
fn parse_u64(value: Option<String>, name: &str) -> Result<u64, String> {
    value
        .ok_or_else(|| format!("missing {name}"))?
        .parse()
        .map_err(|err| format!("invalid {name}: {err}"))
}

#[cfg(target_os = "windows")]
fn parse_u32(value: Option<String>, name: &str) -> Result<u32, String> {
    value
        .ok_or_else(|| format!("missing {name}"))?
        .parse()
        .map_err(|err| format!("invalid {name}: {err}"))
}

#[cfg(target_os = "windows")]
fn parse_i32(value: Option<String>, name: &str) -> Result<i32, String> {
    value
        .ok_or_else(|| format!("missing {name}"))?
        .parse()
        .map_err(|err| format!("invalid {name}: {err}"))
}
