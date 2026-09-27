//! User-approved shell activation and fresh name-based install verification.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use fs4::fs_std::FileExt;

#[derive(Debug, Clone)]
pub struct ActivationPlan {
    pub destination: PathBuf,
    #[cfg(unix)]
    shell: PathBuf,
    #[cfg(unix)]
    snippet: String,
    #[cfg(unix)]
    edits: Vec<ProfileEdit>,
    #[cfg(unix)]
    manual_instruction: Option<String>,
    #[cfg(windows)]
    pub(super) prior_user_path: Option<WindowsPathValue>,
    #[cfg(windows)]
    pub(super) next_user_path: WindowsPathValue,
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq)]
pub(super) struct WindowsPathValue {
    pub(super) bytes: Vec<u8>,
    pub(super) kind: winreg::enums::RegType,
}

#[cfg(unix)]
#[derive(Debug, Clone)]
struct ProfileEdit {
    path: PathBuf,
    prior: Option<Vec<u8>>,
    next: Vec<u8>,
    missing_parents: Vec<PathBuf>,
}

pub fn plan(destination: &Path) -> Result<ActivationPlan, String> {
    #[cfg(unix)]
    {
        posix::plan(destination)
    }
    #[cfg(windows)]
    {
        super::activation_windows::plan(destination)
    }
}

impl ActivationPlan {
    pub fn description(&self) -> String {
        #[cfg(unix)]
        {
            posix::description(self)
        }
        #[cfg(windows)]
        {
            super::activation_windows::description(self)
        }
    }

    pub fn apply_and_verify(&self, version: &str) -> Result<(), String> {
        super::transaction::inspect_destination(&self.destination, false)?;
        let parent = self
            .destination
            .parent()
            .ok_or("missing install directory")?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let lock_path = parent.join(".clud-install.lock");
        #[cfg(windows)]
        if let Ok(meta) = std::fs::symlink_metadata(&lock_path) {
            if super::transaction::windows_reparse(&meta) {
                return Err("installer lock is a reparse point".into());
            }
        }
        let lock = options
            .open(&lock_path)
            .map_err(|error| format!("open installer lock: {error}"))?;
        let lock_meta = lock.metadata().map_err(|error| error.to_string())?;
        if !lock_meta.is_file() {
            return Err("installer lock is not a regular file".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if lock_meta.uid() != unsafe { libc::geteuid() } || lock_meta.nlink() != 1 {
                return Err("installer lock is not exclusively user-owned".into());
            }
        }
        #[cfg(windows)]
        if super::transaction::windows_reparse(&lock_meta) {
            return Err("installer lock is a reparse point".into());
        }
        lock.lock_exclusive().map_err(|error| error.to_string())?;
        super::transaction::inspect_destination(&self.destination, false)?;
        #[cfg(unix)]
        {
            posix::apply_and_verify(self, version)
        }
        #[cfg(windows)]
        {
            super::activation_windows::apply_and_verify(self, version)
        }
    }
}

#[cfg(unix)]
mod posix {
    use super::{ActivationPlan, ProfileEdit};
    use std::ffi::{CStr, OsString};
    use std::fs::{self, OpenOptions};
    use std::io::{Read, Write};
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use running_process::ReadStatus;

    const START: &str = "# >>> clud installer PATH >>>";
    const END: &str = "# <<< clud installer PATH <<<";
    const MAX_PROFILE_BYTES: u64 = 1024 * 1024;

    pub(super) fn plan(destination: &Path) -> Result<ActivationPlan, String> {
        let bin = destination.parent().ok_or("missing install directory")?;
        if bin
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .any(|byte| matches!(*byte, b':' | b'\n' | b'\r' | 0))
        {
            return Err("install directory contains a PATH separator or control character".into());
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is unavailable")?;
        if !home.is_absolute() {
            return Err("HOME must be absolute".into());
        }
        let shell = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .or_else(account_shell)
            .ok_or("cannot determine the account login shell")?;
        if !shell.is_absolute() {
            return Err("login shell must be an absolute path".into());
        }
        let name = shell
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("login shell name is unavailable")?;
        let paths = match name {
            "bash" => {
                let login = [".bash_profile", ".bash_login", ".profile"]
                    .iter()
                    .map(|name| home.join(name))
                    .find(|path| fs::symlink_metadata(path).is_ok())
                    .unwrap_or_else(|| home.join(".bash_profile"));
                vec![login, home.join(".bashrc")]
            }
            "zsh" => {
                let zdotdir = std::env::var_os("ZDOTDIR")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.clone());
                if !zdotdir.is_absolute() {
                    return Err("ZDOTDIR must be absolute".into());
                }
                vec![zdotdir.join(".zprofile"), zdotdir.join(".zshrc")]
            }
            "fish" => {
                let config = std::env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".config"));
                if !config.is_absolute() {
                    return Err("XDG_CONFIG_HOME must be absolute".into());
                }
                vec![config.join("fish/conf.d/clud-path.fish")]
            }
            _ => {
                let bin_text = bin.to_string_lossy();
                let quoted = shell_words::quote(&bin_text);
                return Ok(ActivationPlan {
                    destination: destination.to_path_buf(),
                    shell,
                    snippet: String::new(),
                    edits: Vec::new(),
                    manual_instruction: Some(format!(
                        "add `export PATH={quoted}:\"$PATH\"` to this shell's startup file, then run a new shell"
                    )),
                });
            }
        };
        let snippet = if name == "fish" {
            format!(
                "{START}\nfish_add_path --path --move -- {}\n{END}\n",
                fish_quote(bin)?
            )
        } else {
            let bin_text = bin.to_string_lossy();
            let quoted = shell_words::quote(&bin_text);
            format!(
                "{START}\ncase \"$PATH\" in\n  {quoted}:*) ;;\n  *) PATH={quoted}:\"$PATH\"; export PATH ;;\nesac\n{END}\n"
            )
        };
        let mut edits = Vec::new();
        for path in paths {
            let prior = read_profile(&path)?;
            let next = match &prior {
                Some(bytes) => {
                    let text = std::str::from_utf8(bytes)
                        .map_err(|_| format!("profile is not UTF-8: {}", path.display()))?;
                    if text.contains(START) || text.contains(END) {
                        if text.matches(START).count() != 1 || text.matches(END).count() != 1 {
                            return Err(format!(
                                "startup file has ambiguous installer markers: {}",
                                path.display()
                            ));
                        }
                        let start = text.find(START).ok_or("missing installer start marker")?;
                        let end_start = text.find(END).ok_or("missing installer end marker")?;
                        if end_start <= start {
                            return Err(format!("invalid installer markers: {}", path.display()));
                        }
                        let end = end_start + END.len();
                        let end = if text.as_bytes().get(end) == Some(&b'\n') {
                            end + 1
                        } else {
                            end
                        };
                        let mut result = bytes[..start].to_vec();
                        result.extend_from_slice(snippet.as_bytes());
                        result.extend_from_slice(&bytes[end..]);
                        result
                    } else {
                        let mut result = bytes.clone();
                        if !result.is_empty() && !result.ends_with(b"\n") {
                            result.push(b'\n');
                        }
                        result.extend_from_slice(snippet.as_bytes());
                        result
                    }
                }
                None => snippet.as_bytes().to_vec(),
            };
            if next.len() as u64 > MAX_PROFILE_BYTES {
                return Err(format!(
                    "startup file would be too large: {}",
                    path.display()
                ));
            }
            let mut missing_parents = Vec::new();
            let mut ancestor = path.parent();
            while let Some(directory) = ancestor {
                if fs::symlink_metadata(directory).is_ok() {
                    break;
                }
                missing_parents.push(directory.to_path_buf());
                ancestor = directory.parent();
            }
            edits.push(ProfileEdit {
                path,
                prior,
                next,
                missing_parents,
            });
        }
        Ok(ActivationPlan {
            destination: destination.to_path_buf(),
            shell,
            snippet,
            edits,
            manual_instruction: None,
        })
    }

    fn account_shell() -> Option<PathBuf> {
        let mut buffer = vec![0_u8; 4096];
        loop {
            let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
            let mut found = std::ptr::null_mut();
            // SAFETY: getpwuid_r receives a writable entry and buffer that live through this call.
            let status = unsafe {
                libc::getpwuid_r(
                    libc::geteuid(),
                    entry.as_mut_ptr(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut found,
                )
            };
            if status == libc::ERANGE && buffer.len() < 1024 * 1024 {
                buffer.resize(buffer.len() * 2, 0);
                continue;
            }
            if status != 0 || found.is_null() {
                return None;
            }
            // SAFETY: found points to the entry written by successful getpwuid_r.
            let shell_ptr = unsafe { (*found).pw_shell };
            if shell_ptr.is_null() {
                return None;
            }
            // SAFETY: a successful getpwuid_r points pw_shell into the live buffer.
            let shell = unsafe { CStr::from_ptr(shell_ptr) };
            return Some(PathBuf::from(OsString::from_vec(shell.to_bytes().to_vec())));
        }
    }

    fn fish_quote(path: &Path) -> Result<String, String> {
        let value = path.to_str().ok_or("fish path is not UTF-8")?;
        let mut quoted = String::from("\"");
        for ch in value.chars() {
            if matches!(ch, '\\' | '"' | '$') {
                quoted.push('\\');
            }
            quoted.push(ch);
        }
        quoted.push('"');
        Ok(quoted)
    }

    fn read_profile(path: &Path) -> Result<Option<Vec<u8>>, String> {
        inspect_parent(path)?;
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("inspect {}: {error}", path.display())),
        };
        if !meta.is_file()
            || meta.file_type().is_symlink()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.nlink() != 1
            || meta.mode() & 0o022 != 0
            || meta.len() > MAX_PROFILE_BYTES
        {
            return Err(format!("unsafe startup file: {}", path.display()));
        }
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| format!("open {}: {error}", path.display()))?;
        let opened = file.metadata().map_err(|error| error.to_string())?;
        if opened.dev() != meta.dev() || opened.ino() != meta.ino() {
            return Err(format!(
                "startup file changed while opening: {}",
                path.display()
            ));
        }
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(MAX_PROFILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_PROFILE_BYTES {
            return Err(format!("startup file is too large: {}", path.display()));
        }
        Ok(Some(bytes))
    }

    fn inspect_parent(path: &Path) -> Result<(), String> {
        let parent = path.parent().ok_or("startup file has no parent")?;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is unavailable")?;
        let mut current = PathBuf::new();
        for part in parent.components() {
            current.push(part);
            let meta = match fs::symlink_metadata(&current) {
                Ok(meta) => meta,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("inspect {}: {error}", current.display())),
            };
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err(format!("unsafe startup directory: {}", current.display()));
            }
            if (current.starts_with(&home) || current == parent)
                && meta.uid() != unsafe { libc::geteuid() }
            {
                return Err(format!(
                    "startup directory is not user-owned: {}",
                    current.display()
                ));
            }
            if meta.mode() & 0o002 != 0 && meta.mode() & 0o1000 == 0 {
                return Err(format!(
                    "world-writable startup directory: {}",
                    current.display()
                ));
            }
        }
        Ok(())
    }

    pub(super) fn description(plan: &ActivationPlan) -> String {
        if let Some(instruction) = &plan.manual_instruction {
            return format!(
                "shell: {}; manual activation: {instruction}",
                plan.shell.display()
            );
        }
        let mut lines = vec![format!("shell: {}", plan.shell.display())];
        for edit in &plan.edits {
            lines.push(format!(
                "{} {}",
                if edit.prior.as_ref() == Some(&edit.next) {
                    "keep"
                } else if edit.prior.is_some() {
                    "update"
                } else {
                    "create"
                },
                edit.path.display()
            ));
        }
        format!(
            "{}\nmanaged startup text:\n{}",
            lines.join("; "),
            plan.snippet
        )
    }

    pub(super) fn apply_and_verify(plan: &ActivationPlan, version: &str) -> Result<(), String> {
        if let Some(instruction) = &plan.manual_instruction {
            return Err(format!("manual activation required: {instruction}"));
        }
        let mut changed = Vec::new();
        for edit in &plan.edits {
            let observed = match read_profile(&edit.path) {
                Ok(observed) => observed,
                Err(error) => {
                    rollback(&changed)?;
                    cleanup_created_dirs(&plan.edits);
                    return Err(error);
                }
            };
            if observed != edit.prior {
                rollback(&changed)?;
                cleanup_created_dirs(&plan.edits);
                return Err(format!(
                    "startup file changed after consent: {}",
                    edit.path.display()
                ));
            }
            if edit.prior.as_ref() == Some(&edit.next) {
                continue;
            }
            if let Err(error) = write_profile(&edit.path, &edit.next, edit.prior.as_deref()) {
                rollback(&changed)?;
                cleanup_created_dirs(&plan.edits);
                return Err(error);
            }
            changed.push(edit);
        }
        if let Err(error) = verify(plan, version) {
            rollback(&changed)?;
            cleanup_created_dirs(&plan.edits);
            return Err(error);
        }
        Ok(())
    }

    fn write_profile(path: &Path, bytes: &[u8], expected: Option<&[u8]>) -> Result<(), String> {
        inspect_parent(path)?;
        let parent = path.parent().ok_or("startup file has no parent")?;
        if !parent.exists() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        inspect_parent(path)?;
        let mode = if expected.is_some() {
            fs::symlink_metadata(path)
                .map_err(|error| error.to_string())?
                .permissions()
                .mode()
        } else {
            0o600
        };
        let mut stage = tempfile::Builder::new()
            .prefix(".clud-profile-")
            .tempfile_in(parent)
            .map_err(|error| error.to_string())?;
        stage.write_all(bytes).map_err(|error| error.to_string())?;
        stage
            .as_file()
            .set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|error| error.to_string())?;
        stage
            .as_file()
            .sync_all()
            .map_err(|error| error.to_string())?;
        if read_profile(path)?.as_deref() != expected {
            return Err(format!(
                "startup file changed during update: {}",
                path.display()
            ));
        }
        stage
            .persist(path)
            .map_err(|error| error.error.to_string())?;
        Ok(())
    }

    fn rollback(changed: &[&ProfileEdit]) -> Result<(), String> {
        for edit in changed.iter().rev() {
            if read_profile(&edit.path)?.as_deref() != Some(&edit.next) {
                return Err(format!(
                    "cannot safely restore changed startup file: {}",
                    edit.path.display()
                ));
            }
            match &edit.prior {
                Some(bytes) => write_profile(&edit.path, bytes, Some(edit.next.as_slice()))?,
                None => fs::remove_file(&edit.path).map_err(|error| error.to_string())?,
            }
        }
        Ok(())
    }

    fn cleanup_created_dirs(edits: &[ProfileEdit]) {
        for edit in edits {
            for directory in &edit.missing_parents {
                let _ = fs::remove_dir(directory);
            }
        }
    }

    fn verify(plan: &ActivationPlan, version: &str) -> Result<(), String> {
        let expected = format!("clud {version}");
        for mode in ["-lc", "-ic", "-lic"] {
            let mut env: Vec<(String, String)> = std::env::vars_os()
                .filter_map(|(key, value)| {
                    Some((key.into_string().ok()?, value.into_string().ok()?))
                })
                .filter(|(key, _)| !matches!(key.as_str(), "PATH" | "BASH_ENV" | "ENV"))
                .collect();
            env.push(("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()));
            let command = vec![
                plan.shell.to_string_lossy().to_string(),
                mode.into(),
                "command -v clud; clud --version".into(),
            ];
            let process =
                crate::subprocess::ManagedSubprocess::start(command, None, env, true, None)?;
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut output = Vec::new();
            loop {
                if Instant::now() >= deadline {
                    let _ = process.kill();
                    return Err(format!("fresh shell {mode} lookup timed out"));
                }
                match process.read_stdout(Some(Duration::from_millis(100))) {
                    ReadStatus::Line(line) => {
                        output.extend_from_slice(&line);
                        output.push(b'\n');
                    }
                    ReadStatus::Timeout => {
                        let _ = process.poll();
                    }
                    ReadStatus::Eof => break,
                }
                if output.len() > 32 * 1024 {
                    let _ = process.kill();
                    return Err("fresh shell output is too large".into());
                }
            }
            let code = process
                .wait(Some(Duration::from_secs(1)))
                .map_err(|error| error.to_string())?;
            let lines: Vec<_> = String::from_utf8_lossy(&output)
                .lines()
                .map(str::to_owned)
                .collect();
            let actual = lines
                .iter()
                .rev()
                .nth(1)
                .map(String::as_str)
                .unwrap_or_default();
            let found_version = lines.last().map(String::as_str).unwrap_or_default();
            if code != 0
                || actual != plan.destination.to_string_lossy()
                || found_version != expected
            {
                return Err(format!("fresh shell {mode} resolved {actual:?} with {found_version:?}; expected {} and {expected}", plan.destination.display()));
            }
        }
        Ok(())
    }
}
