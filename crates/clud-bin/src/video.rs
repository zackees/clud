//! `clud video [DIR]` (#1851): browser-use/video-use as a session-scoped
//! Claude Code plugin, never auto-loaded.
//!
//! video-use is a third-party Claude Code skill that drives ffmpeg and the
//! ElevenLabs Scribe API. Its documented install symlinks it into
//! `~/.claude/skills/`, which would make it eligible to trigger in every
//! session. clud instead keeps a pinned checkout under
//! `~/.clud/extern/video-use/<sha>/` (Python deps in a private `uv` venv
//! inside it) and a clud-owned plugin wrapper under
//! `~/.clud/extern/video-use-plugin/`, and passes that wrapper to the Claude
//! harness with `--plugin-dir` **only** for a `clud video` launch. Nothing is
//! written to `~/.claude/skills` or `~/.codex/skills`, and there is no
//! `BUNDLED_SKILLS` entry. See `docs/architecture/launch-targets.md` and
//! DD-163.

use std::path::{Path, PathBuf};
use std::time::Duration;

use running_process::ReadStatus;

use crate::provider_auth::{NativeSecretStore, SecretStore};
use crate::subprocess::ManagedSubprocess;
use crate::win_creation_flags::invisible_helper_creationflags;

/// Upstream repository. Reviewed at the pinned commit below.
pub const VIDEO_USE_REPO: &str = "https://github.com/browser-use/video-use";
/// Reviewed upstream commit. video-use tells the agent to run shell commands,
/// so bump this deliberately after reading the diff.
pub const VIDEO_USE_SHA: &str = "b877063835e6ea6e457124da7e28a0ae26691dc3";
/// The skill name upstream's `SKILL.md` frontmatter declares.
pub const SKILL_NAME: &str = "video-use";
pub const PLUGIN_NAME: &str = "clud-video-use";
pub const ELEVENLABS_ENV: &str = "ELEVENLABS_API_KEY";
/// Vault identifiers for the ElevenLabs key. Frozen: renaming them strands
/// every stored key.
pub const ELEVENLABS_VAULT_SERVICE: &str = "clud.elevenlabs";
pub const ELEVENLABS_VAULT_ACCOUNT: &str = "api-key-v1";
/// Written into a checkout only after it is verified; its absence means the
/// directory is not a usable install.
const VERIFIED_MARKER: &str = ".clud-verified";

/// Root of every managed video-use checkout.
pub fn managed_root(home: &Path) -> PathBuf {
    home.join(".clud").join("extern").join("video-use")
}

/// The pinned checkout for `home`, whether or not it exists.
pub fn checkout_dir(home: &Path) -> PathBuf {
    managed_root(home).join(VIDEO_USE_SHA)
}

/// The clud-owned plugin wrapper passed to `--plugin-dir`.
pub fn plugin_dir(home: &Path) -> PathBuf {
    home.join(".clud").join("extern").join("video-use-plugin")
}

pub fn is_installed(home: &Path) -> bool {
    checkout_dir(home).join(VERIFIED_MARKER).is_file()
}

/// `.claude-plugin/plugin.json` for the wrapper. Claude Code requires only
/// `name`; skills are auto-discovered from `<plugin>/skills/<name>/SKILL.md`.
pub fn plugin_manifest() -> String {
    format!(
        concat!(
            "{{\n",
            "  \"name\": \"{name}\",\n",
            "  \"version\": \"0.0.0-{short}\",\n",
            "  \"description\": \"browser-use/video-use pinned at {sha}, loaded only by `clud video`\",\n",
            "  \"homepage\": \"{repo}\",\n",
            "  \"author\": {{ \"name\": \"browser-use (packaged by clud)\" }},\n",
            "  \"license\": \"MIT\"\n",
            "}}\n"
        ),
        name = PLUGIN_NAME,
        short = &VIDEO_USE_SHA[..7],
        sha = VIDEO_USE_SHA,
        repo = VIDEO_USE_REPO,
    )
}

/// The seed prompt for a `clud video` session: start the skill's own
/// inventory -> propose -> wait-for-approval flow, and never spend ElevenLabs
/// credits before the user agrees.
pub fn seed_prompt(checkout: &Path) -> String {
    format!(
        "/{SKILL_NAME} Use the {SKILL_NAME} skill to edit the videos in the current directory. \
         Follow its flow: inventory the source files, propose an edit plan, and wait for my OK \
         before transcribing (each transcription is a paid ElevenLabs call) or rendering. \
         The skill's helpers live in {checkout}; run them with \
         `uv run --project {checkout} python {checkout}/helpers/<script>.py`.",
        checkout = checkout.display()
    )
}

/// Write (or refresh) the plugin wrapper so `skills/video-use` resolves to
/// `checkout`. A symlink is preferred; a copy (minus `.git`) is the fallback
/// where links cannot be made.
pub fn write_plugin_wrapper(home: &Path, checkout: &Path) -> Result<PathBuf, String> {
    let dir = plugin_dir(home);
    let manifest_dir = dir.join(".claude-plugin");
    std::fs::create_dir_all(&manifest_dir)
        .map_err(|e| format!("cannot create {}: {e}", manifest_dir.display()))?;
    std::fs::write(manifest_dir.join("plugin.json"), plugin_manifest())
        .map_err(|e| format!("cannot write the plugin manifest: {e}"))?;
    let skills = dir.join("skills"); // skill-source-lint: allow (plugin wrapper, not a backend skills dir; DD-163)
    std::fs::create_dir_all(&skills)
        .map_err(|e| format!("cannot create {}: {e}", skills.display()))?;
    let link = skills.join(SKILL_NAME);
    remove_any(&link);
    if link_dir(checkout, &link).is_err() {
        copy_tree(checkout, &link)
            .map_err(|e| format!("cannot copy the skill into {}: {e}", link.display()))?;
    }
    Ok(dir)
}

fn remove_any(path: &Path) {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            let _ = std::fs::remove_dir_all(path);
        }
        Ok(_) => {
            let _ = std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path));
        }
        Err(_) => {}
    }
}

#[cfg(unix)]
fn link_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn link_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let target = to.join(&name);
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Media tools the skill shells out to that are missing, given a PATH probe.
pub fn missing_media_tools(on_path: &dyn Fn(&str) -> bool) -> Vec<&'static str> {
    ["ffmpeg", "ffprobe"]
        .into_iter()
        .filter(|tool| !on_path(tool))
        .collect()
}

/// One actionable line for missing ffmpeg/ffprobe. clud never installs
/// system packages itself (and never runs sudo).
pub fn missing_media_tools_message(missing: &[&str], os: &str) -> String {
    let hint = match os {
        "macos" => "brew install ffmpeg",
        "windows" => "winget install --id Gyan.FFmpeg",
        _ => "install your distro's ffmpeg package (e.g. `sudo apt install ffmpeg`)",
    };
    format!(
        "`clud video` needs {} on PATH; {hint}, then retry",
        missing.join(" and ")
    )
}

/// Where the ElevenLabs key came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Ambient,
    Vault,
    Prompt,
}

pub fn key_is_well_formed(key: &str) -> bool {
    !key.is_empty() && !key.chars().any(char::is_whitespace)
}

/// Resolve the ElevenLabs key: an ambient `ELEVENLABS_API_KEY` wins, then the
/// clud vault, then (only when `interactive`) one prompt whose answer is
/// stored. Otherwise an actionable error.
pub fn resolve_key(
    ambient: Option<String>,
    store: &dyn SecretStore,
    interactive: bool,
    prompt: &mut dyn FnMut() -> Result<String, ()>,
) -> Result<(String, KeySource), String> {
    if let Some(key) = ambient.filter(|key| !key.trim().is_empty()) {
        return Ok((key.trim().to_string(), KeySource::Ambient));
    }
    if let Ok(Some(key)) = store.get() {
        if key_is_well_formed(&key) {
            return Ok((key, KeySource::Vault));
        }
    }
    if !interactive {
        return Err(format!(
            "`clud video` needs an ElevenLabs API key for transcription; set {ELEVENLABS_ENV} \
             or run `clud video` once in a terminal to store one in clud's credential vault"
        ));
    }
    let key = prompt()
        .map_err(|()| "no ElevenLabs API key entered".to_string())?
        .trim()
        .to_string();
    if !key_is_well_formed(&key) {
        return Err("the ElevenLabs API key is empty or contains whitespace".to_string());
    }
    if let Err(error) = store.set(&key) {
        eprintln!("[clud] warning: could not store the ElevenLabs key: {error}");
    }
    Ok((key, KeySource::Prompt))
}

pub fn elevenlabs_store() -> Result<NativeSecretStore, String> {
    NativeSecretStore::new_for(ELEVENLABS_VAULT_SERVICE, ELEVENLABS_VAULT_ACCOUNT)
        .map_err(|e| e.to_string())
}

/// Install the pinned checkout and its venv, then refresh the wrapper. A
/// verified checkout is reused unless `force`. The clone runs in a sibling
/// staging directory and is renamed into place only after `HEAD` equals the
/// pinned SHA, `SKILL.md` exists, and `uv sync` succeeded.
pub fn install(home: &Path, force: bool) -> Result<PathBuf, String> {
    let checkout = checkout_dir(home);
    if force || !is_installed(home) {
        let uv = which::which("uv").map_err(|_| {
            "uv was not found on PATH; install it (https://docs.astral.sh/uv/) and retry"
                .to_string()
        })?;
        let git = which::which("git").map_err(|_| "git was not found on PATH".to_string())?;
        let root = managed_root(home);
        std::fs::create_dir_all(&root)
            .map_err(|e| format!("cannot create {}: {e}", root.display()))?;
        let staging = root.join(format!("{VIDEO_USE_SHA}.staging-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staging);
        let result = stage(&git, &uv, &staging).and_then(|()| {
            remove_any(&checkout);
            std::fs::rename(&staging, &checkout)
                .map_err(|e| format!("cannot move the checkout into {}: {e}", checkout.display()))
        });
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        result?;
    }
    write_plugin_wrapper(home, &checkout)
}

fn stage(git: &Path, uv: &Path, staging: &Path) -> Result<(), String> {
    std::fs::create_dir_all(staging)
        .map_err(|e| format!("cannot create {}: {e}", staging.display()))?;
    let git = git.to_string_lossy().into_owned();
    let dir = staging.to_string_lossy().into_owned();
    for step in [
        vec!["init", "-q"],
        vec!["fetch", "-q", "--depth", "1", VIDEO_USE_REPO, VIDEO_USE_SHA],
        vec!["checkout", "-q", "--detach", "FETCH_HEAD"],
    ] {
        let mut argv = vec![git.clone(), "-C".to_string(), dir.clone()];
        argv.extend(step.iter().map(|s| s.to_string()));
        run_checked(argv, None)?;
    }
    let head = run_checked(
        vec![
            git,
            "-C".into(),
            dir.clone(),
            "rev-parse".into(),
            "HEAD".into(),
        ],
        None,
    )?;
    if head.trim() != VIDEO_USE_SHA {
        return Err(format!(
            "checkout HEAD is {}, expected {VIDEO_USE_SHA}",
            head.trim()
        ));
    }
    if !staging.join("SKILL.md").is_file() {
        return Err("the pinned video-use checkout has no SKILL.md".to_string());
    }
    run_checked(
        vec![
            uv.to_string_lossy().into_owned(),
            "sync".into(),
            "--quiet".into(),
        ],
        Some(staging),
    )?;
    std::fs::write(staging.join(VERIFIED_MARKER), VIDEO_USE_SHA)
        .map_err(|e| format!("cannot mark the checkout verified: {e}"))
}

fn run_checked(argv: Vec<String>, cwd: Option<&Path>) -> Result<String, String> {
    let label = argv
        .iter()
        .skip(1)
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let process = ManagedSubprocess::start_inheriting_env(
        argv,
        cwd.map(Path::to_path_buf),
        true,
        invisible_helper_creationflags(),
    )?;
    let mut buf = Vec::<u8>::new();
    loop {
        match process.read_stdout(Some(Duration::from_millis(100))) {
            ReadStatus::Line(line) => {
                buf.extend_from_slice(&line);
                buf.push(b'\n');
            }
            ReadStatus::Timeout => {
                let _ = process.poll();
            }
            ReadStatus::Eof => break,
        }
    }
    let code = process.wait(Some(Duration::from_secs(900)))?;
    let output = String::from_utf8_lossy(&buf).into_owned();
    if code != 0 {
        let tail: Vec<&str> = output.lines().rev().take(8).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        return Err(format!(
            "`{label}` exited with {code}: {}",
            tail.join(" | ")
        ));
    }
    Ok(output)
}

/// `clud video --update`: (re)install the pinned checkout and its venv.
pub fn run_update() -> i32 {
    let Some(home) = crate::home::user_home() else {
        eprintln!("[clud] error: home directory unavailable");
        return 1;
    };
    match install(&home, true) {
        Ok(wrapper) => {
            println!(
                "Installed video-use {} at {} (plugin wrapper {})",
                &VIDEO_USE_SHA[..7],
                checkout_dir(&home).display(),
                wrapper.display()
            );
            0
        }
        Err(error) => {
            eprintln!("[clud] video-use install failed: {error}");
            1
        }
    }
}

/// The checkout `clud video --path` reports: media tools must be present, and
/// the managed install is created first if missing. `installer` is
/// [`install`] in production and injected in tests.
pub fn checkout_for_path(
    home: &Path,
    missing_tools: &[&str],
    installer: &mut dyn FnMut(&Path) -> Result<PathBuf, String>,
) -> Result<PathBuf, String> {
    if !missing_tools.is_empty() {
        return Err(missing_media_tools_message(
            missing_tools,
            std::env::consts::OS,
        ));
    }
    installer(home)?;
    Ok(checkout_dir(home))
}

/// `clud video --path` (#1866): print the pinned checkout, installing it on
/// first use. The explicit-only `/video-use` skill calls this so the pinned
/// SHA lives only in this module.
pub fn run_path() -> i32 {
    let Some(home) = crate::home::user_home() else {
        eprintln!("[clud] error: home directory unavailable");
        return 1;
    };
    let missing = missing_media_tools(&|tool| which::which(tool).is_ok());
    let mut installer = |home: &Path| {
        if !is_installed(home) {
            eprintln!(
                "[clud] installing video-use {} into {}",
                &VIDEO_USE_SHA[..7],
                checkout_dir(home).display()
            );
        }
        install(home, false)
    };
    match checkout_for_path(&home, &missing, &mut installer) {
        Ok(checkout) => {
            println!("{}", checkout.display());
            0
        }
        Err(error) => {
            eprintln!("[clud] video-use unavailable: {error}");
            1
        }
    }
}

/// Everything a real (non-dry-run) `clud video` launch needs before the
/// harness starts: media tools, the managed install, and the key. Returns the
/// key, which the caller places only in the child environment.
pub fn prepare_launch(interactive: bool) -> Result<String, String> {
    let missing = missing_media_tools(&|tool| which::which(tool).is_ok());
    if !missing.is_empty() {
        return Err(missing_media_tools_message(&missing, std::env::consts::OS));
    }
    let home = crate::home::user_home().ok_or("home directory unavailable")?;
    if !is_installed(&home) {
        eprintln!(
            "[clud] installing video-use {} into {}",
            &VIDEO_USE_SHA[..7],
            checkout_dir(&home).display()
        );
    }
    install(&home, false)?;
    let store = elevenlabs_store()?;
    let (key, source) = resolve_key(
        std::env::var(ELEVENLABS_ENV).ok(),
        &store,
        interactive,
        &mut || crate::provider_auth::prompt_secret("ElevenLabs"),
    )?;
    if source == KeySource::Prompt {
        eprintln!("[clud] saved the ElevenLabs key to clud's credential vault");
    }
    Ok(key)
}

#[cfg(test)]
#[path = "video_tests.rs"]
mod tests;
