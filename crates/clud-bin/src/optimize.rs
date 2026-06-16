//! `clud optimize`: persistent, fast-machine setup recommendations.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::args::{Args, OptimizeTarget};
use crate::clud_settings::{self, RustOptimizeSettings};
use crate::loop_spec;
use running_process::{
    CommandSpec, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode,
};

const REPO_DIRECTIVE_DIR: &str = ".clud";
const REPO_DIRECTIVE_FILE: &str = "settings.json";

pub fn run(
    args: &Args,
    target: OptimizeTarget,
    global: bool,
    repo: bool,
    install_soldr: bool,
    use_soldr_shims: bool,
    soldr_version: &str,
) -> i32 {
    match target {
        OptimizeTarget::Rust => run_rust(
            args,
            global,
            repo,
            install_soldr,
            use_soldr_shims,
            soldr_version,
        ),
    }
}

fn run_rust(
    args: &Args,
    global: bool,
    repo: bool,
    install_soldr: bool,
    use_soldr_shims: bool,
    soldr_version: &str,
) -> i32 {
    let settings = RustOptimizeSettings {
        use_soldr_shims,
        install_soldr,
        soldr_version: soldr_version.to_string(),
    };
    let write_repo = repo || !global;

    if args.dry_run {
        println!("[clud] dry-run: optimize rust");
        if global {
            println!(
                "[clud] dry-run: would write ~/.clud/settings.toml [optimize.rust] use_soldr_shims={} install_soldr={} soldr_version=\"{}\"",
                settings.use_soldr_shims, settings.install_soldr, settings.soldr_version
            );
        }
        if write_repo {
            match repo_directive_path() {
                Ok(path) => println!(
                    "[clud] dry-run: would write repo directive {}",
                    path.display()
                ),
                Err(error) => {
                    eprintln!("[clud] error: {error}");
                    return 1;
                }
            }
        }
        if install_soldr {
            println!("[clud] dry-run: would install soldr if missing from PATH");
        }
        return 0;
    }

    if global {
        if let Err(error) = clud_settings::save_rust_optimize_settings(&settings) {
            eprintln!("[clud] error: failed to save optimize settings: {error}");
            return 1;
        }
        println!("[clud] wrote global Rust optimizer defaults to ~/.clud/settings.toml");
    }

    if write_repo {
        match write_repo_directive(&settings) {
            Ok(path) => {
                println!(
                    "[clud] wrote repo Rust optimizer directive to {}",
                    path.display()
                );
                if let Err(error) = stage_repo_directive(&path) {
                    eprintln!("[clud] error: failed to stage repo directive: {error}");
                    return 1;
                }
                println!("[clud] staged {}", display_repo_path(&path));
            }
            Err(error) => {
                eprintln!("[clud] error: failed to write repo directive: {error}");
                return 1;
            }
        }
    }

    if install_soldr {
        match ensure_soldr_installed(soldr_version) {
            Ok(outcome) => println!("[clud] {outcome}"),
            Err(error) => {
                eprintln!("[clud] error: failed to install soldr: {error}");
                return 1;
            }
        }
    } else {
        println!("[clud] skipped soldr install");
    }

    if use_soldr_shims {
        println!("[clud] enabled soldr shim preference for future clud-managed Rust setup");
    } else {
        println!("[clud] disabled soldr shim preference");
    }
    0
}

fn write_repo_directive(settings: &RustOptimizeSettings) -> io::Result<PathBuf> {
    let path = repo_directive_path()?;
    let repo_root = path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("could not resolve repo root"))?;
    ensure_rust_project(repo_root)?;
    write_repo_directive_at(&path, settings)?;
    let gitignore_updated = ensure_gitignore_tracks_repo_settings(repo_root)?;
    ensure_not_ignored(repo_root, &path)?;
    if gitignore_updated {
        stage_repo_path(repo_root, Path::new(".gitignore"))?;
    }
    Ok(path)
}

fn repo_directive_path() -> io::Result<PathBuf> {
    let cwd = env::current_dir()?;
    let root = loop_spec::git_root_from(&cwd);
    Ok(root.join(REPO_DIRECTIVE_DIR).join(REPO_DIRECTIVE_FILE))
}

fn write_repo_directive_at(path: &Path, settings: &RustOptimizeSettings) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut root = match fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
        Ok(_) => serde_json::json!({}),
        Err(error) if error.kind() == io::ErrorKind::NotFound => serde_json::json!({}),
        Err(error) => return Err(error),
    };
    if !root.is_object() {
        root = serde_json::json!({});
    }
    root["optimize"]["rust"] = serde_json::json!({
        "use_soldr_shims": settings.use_soldr_shims,
        "install_soldr": settings.install_soldr,
        "soldr_version": settings.soldr_version,
    });
    let mut body = serde_json::to_string_pretty(&root).map_err(io::Error::other)?;
    body.push('\n');
    fs::write(path, body)
}

fn ensure_rust_project(repo_root: &Path) -> io::Result<()> {
    if repo_root.join("Cargo.toml").is_file() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "{} does not look like a Rust project: Cargo.toml was not found",
        repo_root.display()
    )))
}

fn ensure_gitignore_tracks_repo_settings(repo_root: &Path) -> io::Result<bool> {
    let gitignore = repo_root.join(".gitignore");
    let original = match fs::read_to_string(&gitignore) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let updated = rewrite_gitignore_for_repo_settings(&original);
    if updated != original {
        fs::write(gitignore, updated)?;
        return Ok(true);
    }
    Ok(false)
}

fn rewrite_gitignore_for_repo_settings(original: &str) -> String {
    let mut lines: Vec<String> = original.lines().map(str::to_string).collect();
    let had_trailing_newline = original.ends_with('\n');
    let has_unignore_dir = lines
        .iter()
        .any(|line| matches!(line.trim(), "!.clud/" | "!/.clud/"));
    let has_unignore_settings = lines.iter().any(|line| {
        matches!(
            line.trim(),
            "!.clud/settings.json" | "!/.clud/settings.json"
        )
    });

    for line in &mut lines {
        if matches!(line.trim(), ".clud/" | "/.clud/" | ".clud" | "/.clud") {
            *line = ".clud/*".to_string();
        }
    }

    if !has_unignore_dir || !has_unignore_settings {
        if !lines.is_empty() && lines.last().is_some_and(|line| !line.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push("# clud project settings".to_string());
        if !has_unignore_dir {
            lines.push("!.clud/".to_string());
        }
        if !has_unignore_settings {
            lines.push("!.clud/settings.json".to_string());
        }
    }

    let mut out = lines.join("\n");
    if had_trailing_newline || !out.is_empty() {
        out.push('\n');
    }
    out
}

fn ensure_not_ignored(repo_root: &Path, path: &Path) -> io::Result<()> {
    let relative = path.strip_prefix(repo_root).unwrap_or(path);
    match run_status(
        vec![
            "git".to_string(),
            "check-ignore".to_string(),
            "--quiet".to_string(),
            relative.to_string_lossy().to_string(),
        ],
        Some(repo_root),
    )? {
        0 => Err(io::Error::other(format!(
            "{} is still ignored by git",
            relative.display()
        ))),
        1 => Ok(()),
        _ => Err(io::Error::other(format!(
            "git check-ignore failed for {}",
            relative.display()
        ))),
    }
}

fn stage_repo_directive(path: &Path) -> io::Result<()> {
    let repo_root = path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("could not resolve repo root"))?;
    let relative = path.strip_prefix(repo_root).unwrap_or(path);
    stage_repo_path(repo_root, relative)
}

fn stage_repo_path(repo_root: &Path, relative: &Path) -> io::Result<()> {
    let code = run_status(
        vec![
            "git".to_string(),
            "add".to_string(),
            relative.to_string_lossy().to_string(),
        ],
        Some(repo_root),
    )?;
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "git add failed with exit code {code}"
        )))
    }
}

fn display_repo_path(path: &Path) -> String {
    path.parent()
        .and_then(|parent| parent.file_name())
        .zip(path.file_name())
        .map(|(dir, file)| format!("{}/{}", dir.to_string_lossy(), file.to_string_lossy()))
        .unwrap_or_else(|| path.display().to_string())
}

fn ensure_soldr_installed(version: &str) -> Result<String, String> {
    if let Ok(path) = which::which("soldr") {
        return Ok(format!("soldr already installed at {}", path.display()));
    }
    let home = home_dir().ok_or_else(|| "could not resolve user home directory".to_string())?;
    let target_dir = global_bin_dir(&home);
    fs::create_dir_all(&target_dir)
        .map_err(|error| format!("create {}: {error}", target_dir.display()))?;

    let asset = soldr_asset_for_current_platform(version)?;
    let url = format!(
        "https://github.com/zackees/soldr/releases/download/v{version}/{}",
        asset.file_name
    );
    let temp_dir = env::temp_dir().join(format!("clud-soldr-{}", std::process::id()));
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir)
            .map_err(|error| format!("clear {}: {error}", temp_dir.display()))?;
    }
    fs::create_dir_all(&temp_dir).map_err(|error| format!("create temp dir: {error}"))?;
    let cleanup_dir = temp_dir.clone();
    let result = install_soldr_from_url(&url, &asset, &temp_dir, &target_dir);
    let _ = fs::remove_dir_all(cleanup_dir);
    result
}

fn install_soldr_from_url(
    url: &str,
    asset: &SoldrAsset,
    temp_dir: &Path,
    target_dir: &Path,
) -> Result<String, String> {
    let archive = temp_dir.join(&asset.file_name);
    eprintln!("[clud] downloading {url}");
    let response = ureq::get(url)
        .call()
        .map_err(|error| format!("download {url}: {error}"))?;
    let mut reader = response.into_reader();
    let mut file = fs::File::create(&archive)
        .map_err(|error| format!("create {}: {error}", archive.display()))?;
    io::copy(&mut reader, &mut file).map_err(|error| format!("write download: {error}"))?;

    extract_archive(&archive, temp_dir, asset)?;
    let src = find_file_named(temp_dir, asset.binary_name)
        .ok_or_else(|| format!("{} not found in {}", asset.binary_name, asset.file_name))?;
    let target = target_dir.join(asset.binary_name);
    fs::copy(&src, &target)
        .map_err(|error| format!("copy {} to {}: {error}", src.display(), target.display()))?;
    make_executable(&target)?;
    Ok(format!(
        "installed soldr {version} to {path}",
        version = asset.version,
        path = target.display()
    ))
}

fn extract_archive(archive: &Path, temp_dir: &Path, asset: &SoldrAsset) -> Result<(), String> {
    let status = if asset.extension == "zip" {
        expand_zip(archive, temp_dir)?
    } else {
        run_status_string(
            vec![
                "tar".to_string(),
                "-xzf".to_string(),
                archive.to_string_lossy().to_string(),
                "-C".to_string(),
                temp_dir.to_string_lossy().to_string(),
            ],
            None,
        )?
    };
    if status == 0 {
        Ok(())
    } else {
        Err(format!(
            "extract {} failed with exit code {status}",
            archive.display()
        ))
    }
}

fn expand_zip(archive: &Path, temp_dir: &Path) -> Result<i32, String> {
    let args = vec![
        "-NoProfile".to_string(),
        "-ExecutionPolicy".to_string(),
        "Bypass".to_string(),
        "-Command".to_string(),
        "Expand-Archive -LiteralPath $args[0] -DestinationPath $args[1] -Force".to_string(),
        archive.to_string_lossy().to_string(),
        temp_dir.to_string_lossy().to_string(),
    ];
    run_status_string(
        std::iter::once("powershell".to_string())
            .chain(args.clone())
            .collect(),
        None,
    )
    .or_else(|first_error| {
        run_status_string(
            std::iter::once("pwsh".to_string()).chain(args).collect(),
            None,
        )
        .map_err(|second_error| {
            format!("failed to start powershell ({first_error}) or pwsh ({second_error})")
        })
    })
}

fn run_status(argv: Vec<String>, cwd: Option<&Path>) -> io::Result<i32> {
    run_status_string(argv, cwd).map_err(io::Error::other)
}

fn run_status_string(argv: Vec<String>, cwd: Option<&Path>) -> Result<i32, String> {
    let process = NativeProcess::new(ProcessConfig {
        command: CommandSpec::Argv(argv),
        cwd: cwd.map(Path::to_path_buf),
        env: None,
        capture: true,
        stderr_mode: StderrMode::Stdout,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    });
    process
        .start()
        .map_err(|error| format!("failed to start command: {error}"))?;

    loop {
        match process.read_combined(Some(Duration::from_millis(100))) {
            ReadStatus::Line(_) => {}
            ReadStatus::Timeout => {
                if process.returncode().is_some() {
                    break;
                }
            }
            ReadStatus::Eof => break,
        }
    }

    process
        .wait(Some(Duration::from_secs(60)))
        .map_err(|error| format!("failed to wait for command: {error}"))
}

fn find_file_named(root: &Path, name: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().and_then(|file| file.to_str()) == Some(name) {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_file_named(&path, name) {
                return Some(found);
            }
        }
    }
    None
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)
        .map_err(|error| format!("metadata {}: {error}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .map_err(|error| format!("chmod {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn global_bin_dir(home: &Path) -> PathBuf {
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cargo"));
    let cargo_bin = cargo_home.join("bin");
    if cargo_bin.exists() {
        cargo_bin
    } else {
        home.join(".local").join("bin")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SoldrAsset {
    version: String,
    file_name: String,
    extension: &'static str,
    binary_name: &'static str,
}

fn soldr_asset_for_current_platform(version: &str) -> Result<SoldrAsset, String> {
    let arch = match env::consts::ARCH {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        other => return Err(format!("unsupported architecture: {other}")),
    };
    let (os, extension, binary_name) = match env::consts::OS {
        "linux" => ("unknown-linux-gnu", "tar.gz", "soldr"),
        "macos" => ("apple-darwin", "tar.gz", "soldr"),
        "windows" => ("pc-windows-msvc", "zip", "soldr.exe"),
        other => return Err(format!("unsupported OS: {other}")),
    };
    Ok(SoldrAsset {
        version: version.to_string(),
        file_name: format!("soldr-v{version}-{arch}-{os}.{extension}"),
        extension,
        binary_name,
    })
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(path) = env::var_os("USERPROFILE") {
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }
    if let Some(path) = env::var_os("HOME") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn writes_repo_directive() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".clud").join("settings.json");
        let settings = RustOptimizeSettings {
            use_soldr_shims: false,
            install_soldr: true,
            soldr_version: "9.9.9".to_string(),
        };

        write_repo_directive_at(&path, &settings).unwrap();

        let text = fs::read_to_string(path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["optimize"]["rust"]["use_soldr_shims"], false);
        assert_eq!(parsed["optimize"]["rust"]["install_soldr"], true);
        assert_eq!(parsed["optimize"]["rust"]["soldr_version"], "9.9.9");
    }

    #[test]
    fn repo_directive_preserves_existing_json() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".clud").join("settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{\n  \"other\": true\n}\n").unwrap();

        write_repo_directive_at(&path, &RustOptimizeSettings::default()).unwrap();

        let parsed: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(parsed["other"], true);
        assert_eq!(parsed["optimize"]["rust"]["use_soldr_shims"], true);
    }

    #[test]
    fn gitignore_rewrite_unignores_settings_only() {
        let original = "target/\n.clud/\n";

        let updated = rewrite_gitignore_for_repo_settings(original);

        assert!(updated.contains(".clud/*"), "{updated}");
        assert!(updated.contains("!.clud/"), "{updated}");
        assert!(updated.contains("!.clud/settings.json"), "{updated}");
    }

    #[test]
    fn soldr_asset_matches_current_platform() {
        let asset = soldr_asset_for_current_platform("1.2.3").unwrap();
        assert!(asset.file_name.starts_with("soldr-v1.2.3-"), "{asset:?}");
        if cfg!(windows) {
            assert!(asset.file_name.ends_with(".zip"), "{asset:?}");
            assert_eq!(asset.binary_name, "soldr.exe");
        } else {
            assert!(asset.file_name.ends_with(".tar.gz"), "{asset:?}");
            assert_eq!(asset.binary_name, "soldr");
        }
    }

    #[test]
    fn finds_nested_binary() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("pkg").join("bin");
        fs::create_dir_all(&nested).unwrap();
        let binary = nested.join("soldr");
        fs::write(&binary, "x").unwrap();

        assert_eq!(find_file_named(dir.path(), "soldr"), Some(binary));
    }
}
