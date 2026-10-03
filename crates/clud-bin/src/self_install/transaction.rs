//! Consented, verified installation of one native executable.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;
use running_process::ReadStatus;
use sha2::{Digest, Sha256};

use super::activation::{self, ActivationPlan};
use super::catalog::{Arch, Flavor, MediaType, Os, ResolvedAsset};
use super::entry::InstallIntent;

const MAX_BINARY_BYTES: u64 = 256 * 1024 * 1024;
const MAX_WHEEL_BYTES: u64 = 384 * 1024 * 1024;

#[derive(Debug)]
pub struct InstallPlan {
    pub destination: PathBuf,
    pub version: String,
    pub source_label: String,
    pub source_sha256: String,
    pub replacement: Option<String>,
    pub path_proposal: String,
    pub activation: ActivationPlan,
    source: Source,
}

#[derive(Debug)]
enum Source {
    Current {
        path: PathBuf,
        file: File,
        fingerprint: Fingerprint,
    },
    Published(ResolvedAsset),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

impl Fingerprint {
    fn of(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            dev: metadata.dev(),
            #[cfg(unix)]
            ino: metadata.ino(),
        }
    }
}

pub fn plan(intent: InstallIntent) -> Result<InstallPlan, String> {
    let destination = approved_destination()?;
    inspect_destination(&destination, false)?;
    let backup = destination.with_extension("clud-backup");
    let recovering =
        backup.exists() && (!destination.exists() || run_version(&destination).is_err());
    if recovering {
        inspect_destination(&backup, true)?;
    } else {
        inspect_destination(&destination, true)?;
    }
    let replacement = if recovering {
        Some(hash_path(&backup, MAX_BINARY_BYTES)?.0)
    } else if destination.exists() {
        Some(hash_path(&destination, MAX_BINARY_BYTES)?.0)
    } else {
        None
    };
    let activation = activation::plan(&destination)?;
    let path_proposal = activation.description();
    match intent {
        InstallIntent::CurrentExecutable => {
            let path = std::env::current_exe().map_err(|error| error.to_string())?;
            let mut file =
                File::open(&path).map_err(|error| format!("open running executable: {error}"))?;
            let metadata = file.metadata().map_err(|error| error.to_string())?;
            if !metadata.is_file() || metadata.len() > MAX_BINARY_BYTES {
                return Err("running executable is not a bounded regular file".into());
            }
            let fingerprint = Fingerprint::of(&metadata);
            validate_native_header(&mut file, current_os(), current_arch(), Flavor::Native)?;
            let (sha, _) = hash_reader(&mut file, MAX_BINARY_BYTES)?;
            file.seek(SeekFrom::Start(0))
                .map_err(|error| error.to_string())?;
            let version = env!("CARGO_PKG_VERSION").to_owned();
            verify_version(&path, &version)?;
            Ok(InstallPlan {
                destination,
                version,
                source_label: path.display().to_string(),
                source_sha256: sha,
                replacement,
                path_proposal,
                activation,
                source: Source::Current {
                    path,
                    file,
                    fingerprint,
                },
            })
        }
        InstallIntent::Published(asset) => {
            validate_asset_url(&asset)?;
            if asset.os != current_os() || asset.arch != current_arch() {
                return Err("selected asset does not match this host".into());
            }
            if asset.size_bytes > MAX_WHEEL_BYTES {
                return Err("selected asset is too large".into());
            }
            Ok(InstallPlan {
                destination,
                version: asset.version.clone(),
                source_label: asset.url.clone(),
                source_sha256: asset.sha256.clone(),
                replacement,
                path_proposal,
                activation,
                source: Source::Published(asset),
            })
        }
    }
}

impl InstallPlan {
    pub fn description(&self) -> String {
        format!(
            "Selected version: {}\nSource: {}\nSHA-256: {}\nDestination: {}\nReplacement: {}\nPATH/profile proposal: {}",
            self.version,
            self.source_label,
            self.source_sha256,
            self.destination.display(),
            self.replacement.as_deref().unwrap_or("none; create new executable"),
            self.path_proposal
        )
    }
}

/// A committed binary is still pending fresh name-based lookup by #1495.
#[expect(
    clippy::too_many_lines,
    reason = "complexity ratchet baseline (zackees/ci.yml#229); split this function"
)]
pub fn execute(mut plan: InstallPlan) -> Result<(), String> {
    let parent = plan.destination.parent().ok_or("missing install parent")?;
    inspect_destination(&plan.destination, false)?;
    #[cfg(unix)]
    create_private_dirs(parent)?;
    #[cfg(windows)]
    fs::create_dir_all(parent).map_err(|error| format!("create user bin: {error}"))?;
    inspect_destination(&plan.destination, false)?;
    let lock_path = parent.join(".clud-install.lock");
    #[cfg(windows)]
    if let Ok(meta) = fs::symlink_metadata(&lock_path) {
        if windows_reparse(&meta) {
            return Err("installer lock is a reparse point".into());
        }
    }
    let mut lock_options = OpenOptions::new();
    lock_options
        .read(true)
        .write(true)
        .create(true)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        lock_options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let lock = lock_options
        .open(&lock_path)
        .map_err(|error| format!("open install lock: {error}"))?;
    let lock_meta = lock.metadata().map_err(|error| error.to_string())?;
    if !lock_meta.is_file() {
        return Err("installer lock is not a regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if lock_meta.uid() != unsafe { libc::geteuid() } || lock_meta.nlink() != 1 {
            return Err("installer lock is not exclusively owned by this user".into());
        }
    }
    #[cfg(windows)]
    if windows_reparse(&lock_meta) {
        return Err("installer lock is a reparse point".into());
    }
    lock.lock_exclusive()
        .map_err(|error| format!("lock installer: {error}"))?;
    inspect_destination(&plan.destination, false)?;
    recover(&plan.destination)?;
    inspect_destination(&plan.destination, true)?;
    if plan.destination.exists() {
        let current = hash_path(&plan.destination, MAX_BINARY_BYTES)?.0;
        if plan.replacement.as_deref() != Some(current.as_str()) {
            return Err("installed executable changed after consent".into());
        }
    } else if plan.replacement.is_some() {
        return Err("installed executable disappeared after consent".into());
    }
    let suffix = if cfg!(windows) { ".exe" } else { ".bin" };
    let mut stage = tempfile::Builder::new()
        .prefix(".clud-install-")
        .suffix(suffix)
        .tempfile_in(parent)
        .map_err(|error| format!("create private stage: {error}"))?;
    match &mut plan.source {
        Source::Current {
            path,
            file,
            fingerprint,
        } => {
            file.seek(SeekFrom::Start(0))
                .map_err(|error| error.to_string())?;
            io::copy(&mut file.take(MAX_BINARY_BYTES + 1), stage.as_file_mut())
                .map_err(|error| format!("copy running executable: {error}"))?;
            if stage
                .as_file()
                .metadata()
                .map_err(|error| error.to_string())?
                .len()
                != fingerprint.len
            {
                return Err("running executable changed during copy".into());
            }
            if Fingerprint::of(&fs::metadata(&*path).map_err(|error| error.to_string())?)
                != *fingerprint
            {
                return Err("running executable identity changed after consent".into());
            }
            if hash_path(path, MAX_BINARY_BYTES)?.0 != plan.source_sha256 {
                return Err("running executable digest changed after consent".into());
            }
        }
        Source::Published(asset) => stage_published(asset, stage.as_file_mut())?,
    }
    stage
        .as_file_mut()
        .flush()
        .map_err(|error| error.to_string())?;
    stage
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    let stage_path = stage.path().to_path_buf();
    let (stage_digest, _) = hash_path(&stage_path, MAX_BINARY_BYTES)?;
    match &plan.source {
        Source::Current { .. } if stage_digest != plan.source_sha256 => {
            return Err("staged executable digest differs from source".into());
        }
        Source::Published(asset)
            if asset.media_type == MediaType::Direct && stage_digest != asset.sha256 =>
        {
            return Err("staged executable digest differs from selected release".into());
        }
        _ => {}
    }
    let flavor = match &plan.source {
        Source::Current { .. } => Flavor::Native,
        Source::Published(asset) => asset.flavor,
    };
    validate_native_header(stage.as_file_mut(), current_os(), current_arch(), flavor)?;
    make_executable(&stage_path)?;
    let stage = stage.into_temp_path();
    verify_version(&stage_path, &plan.version)?;
    commit(&plan.destination, &stage, &plan.version, &stage_digest)?;
    Ok(())
}

fn approved_destination() -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("AppData/Local"))
            })
            .ok_or("cannot locate per-user local app data")?;
        if !local.is_absolute() {
            return Err("per-user local app data must be absolute".into());
        }
        Ok(local.join("Programs/clud/bin/clud.exe"))
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is unavailable")?;
        if !home.is_absolute() {
            return Err("HOME must be absolute".into());
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        for directory in [home.join(".local/bin"), home.join("bin")] {
            let on_path = std::env::split_paths(&path).any(|entry| entry == directory);
            if !on_path {
                continue;
            }
            if let Ok(meta) = fs::symlink_metadata(&directory) {
                use std::os::unix::fs::MetadataExt;
                if meta.is_dir()
                    && !meta.file_type().is_symlink()
                    && meta.uid() == unsafe { libc::geteuid() }
                    && meta.mode() & 0o022 == 0
                {
                    return Ok(directory.join("clud"));
                }
            }
        }
        Ok(home.join(".local/bin/clud"))
    }
}

#[cfg(unix)]
pub(super) fn create_private_dirs(parent: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;

    let mut missing = Vec::new();
    let mut current = Some(parent);
    while let Some(directory) = current {
        match fs::symlink_metadata(directory) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(directory.to_path_buf());
                current = directory.parent();
            }
            Err(error) => return Err(format!("inspect {}: {error}", directory.display())),
        }
    }
    for directory in missing.iter().rev() {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(directory)
            .map_err(|error| {
                format!("create private directory {}: {error}", directory.display())
            })?;
    }
    Ok(())
}

pub(super) fn inspect_destination(destination: &Path, probe_version: bool) -> Result<(), String> {
    let parent = destination.parent().ok_or("missing install directory")?;
    #[cfg(unix)]
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is unavailable")?;
    let mut path = PathBuf::new();
    for part in parent.components() {
        path.push(part);
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return Err(format!("unsafe install directory: {}", path.display()));
            }
            #[cfg(unix)]
            if path.starts_with(&home) {
                use std::os::unix::fs::MetadataExt;
                if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 {
                    return Err(format!(
                        "install directory is not exclusively user-controlled: {}",
                        path.display()
                    ));
                }
            }
            #[cfg(windows)]
            if windows_reparse(&meta) {
                return Err(format!(
                    "reparse-point install directory: {}",
                    path.display()
                ));
            }
        }
    }
    if let Ok(meta) = fs::symlink_metadata(destination) {
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err("installed path is not an owned regular file".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.uid() != unsafe { libc::geteuid() } || meta.nlink() != 1 {
                return Err("installed executable is not exclusively owned by this user".into());
            }
        }
        #[cfg(windows)]
        if windows_reparse(&meta) {
            return Err("installed executable is a reparse point".into());
        }
        if probe_version {
            let output = run_version(destination)?;
            if !output.starts_with("clud ") {
                return Err("existing destination is not a clud executable".into());
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn windows_reparse(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}

fn hash_path(path: &Path, limit: u64) -> Result<(String, u64), String> {
    let mut file = File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    hash_reader(&mut file, limit)
}

fn hash_reader(reader: &mut impl Read, limit: u64) -> Result<(String, u64), String> {
    let mut hasher = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read as u64)
            .ok_or("executable too large")?;
        if count > limit {
            return Err("executable exceeds size limit".into());
        }
        hasher.update(&buffer[..read]);
    }
    Ok((format!("{:x}", hasher.finalize()), count))
}

fn current_os() -> Os {
    if cfg!(windows) {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Darwin
    } else {
        Os::Linux
    }
}

fn current_arch() -> Arch {
    if cfg!(target_arch = "aarch64") {
        Arch::Aarch64
    } else {
        Arch::X86_64
    }
}

fn validate_native_header(
    file: &mut File,
    os: Os,
    arch: Arch,
    flavor: Flavor,
) -> Result<(), String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut header = [0_u8; 512];
    let count = file.read(&mut header).map_err(|error| error.to_string())?;
    let data = &header[..count];
    match os {
        Os::Linux => {
            if data.len() < 64 || &data[..6] != b"\x7fELF\x02\x01" {
                return Err("expected ELF64 little-endian executable".into());
            }
            let machine = u16::from_le_bytes([data[18], data[19]]);
            if machine != if arch == Arch::Aarch64 { 183 } else { 62 } {
                return Err("ELF architecture mismatch".into());
            }
            if flavor == Flavor::StaticMusl {
                // Full program-header and dynamic-table checks are in catalog.rs.
                super::catalog::verify_static_elf_file(file, arch)?;
            }
        }
        Os::Darwin => {
            if data.len() < 8 || &data[..4] != b"\xcf\xfa\xed\xfe" {
                return Err("expected Mach-O 64-bit executable".into());
            }
            let cpu = u32::from_le_bytes(data[4..8].try_into().map_err(|_| "short Mach-O header")?);
            if cpu
                != if arch == Arch::Aarch64 {
                    0x0100_000c
                } else {
                    0x0100_0007
                }
            {
                return Err("Mach-O architecture mismatch".into());
            }
        }
        Os::Windows => {
            if data.len() < 64 || &data[..2] != b"MZ" {
                return Err("expected PE executable".into());
            }
            let offset = u32::from_le_bytes(data[60..64].try_into().map_err(|_| "short PE header")?)
                as usize;
            if offset.checked_add(6).is_none_or(|end| end > data.len())
                || &data[offset..offset + 4] != b"PE\0\0"
            {
                return Err("invalid PE header".into());
            }
            let machine = u16::from_le_bytes([data[offset + 4], data[offset + 5]]);
            if machine
                != if arch == Arch::Aarch64 {
                    0xaa64
                } else {
                    0x8664
                }
            {
                return Err("PE architecture mismatch".into());
            }
        }
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn run_version(path: &Path) -> Result<String, String> {
    let command = vec![path.to_string_lossy().to_string(), "--version".to_owned()];
    let process =
        crate::subprocess::ManagedSubprocess::start_inheriting_env(command, None, true, None)
            .map_err(|error| format!("execute {}: {error}", path.display()))?;
    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if Instant::now() >= deadline {
            let _ = process.kill();
            return Err("executable version probe timed out".into());
        }
        match process.read_stdout(Some(Duration::from_millis(100))) {
            ReadStatus::Line(line) => output.extend_from_slice(&line),
            ReadStatus::Timeout => {
                let _ = process.poll();
            }
            ReadStatus::Eof => break,
        }
        if output.len() > 4096 {
            return Err("version output too long".into());
        }
    }
    let code = process
        .wait(Some(Duration::from_secs(10)))
        .map_err(|error| error.to_string())?;
    if code != 0 {
        return Err(format!("{} --version failed with {code}", path.display()));
    }
    Ok(String::from_utf8_lossy(&output).trim().to_owned())
}

fn verify_version(path: &Path, expected: &str) -> Result<(), String> {
    let version = run_version(path)?;
    if version != format!("clud {expected}") {
        return Err(format!(
            "executable version mismatch: expected clud {expected}, got {version}"
        ));
    }
    Ok(())
}

fn validate_asset_url(asset: &ResolvedAsset) -> Result<(), String> {
    let first = format!(
        "https://github.com/zackees/clud/releases/download/{}/{}",
        asset.version, asset.filename
    );
    let second = format!(
        "https://github.com/zackees/clud/releases/download/v{}/{}",
        asset.version, asset.filename
    );
    if asset.url != first && asset.url != second {
        return Err("release URL changed after catalog resolution".into());
    }
    Ok(())
}

fn stage_published(asset: &ResolvedAsset, target: &mut File) -> Result<(), String> {
    validate_asset_url(asset)?;
    if asset.media_type == MediaType::Direct {
        let current = std::env::current_exe().map_err(|error| error.to_string())?;
        #[cfg(feature = "installer-ci-fixture")]
        let force_download =
            std::env::var_os("CLUD_INSTALLER_CI_FORCE_DOWNLOAD").is_some_and(|value| value == "1");
        #[cfg(not(feature = "installer-ci-fixture"))]
        let force_download = false;
        if !force_download && current_matches_asset(&current, asset)? {
            let mut source = File::open(current).map_err(|error| error.to_string())?;
            io::copy(&mut source, target).map_err(|error| error.to_string())?;
            return Ok(());
        }
    }
    #[cfg(feature = "installer-ci-fixture")]
    if let Some(directory) = std::env::var_os("CLUD_INSTALLER_CI_FIXTURE_DIR") {
        let path = PathBuf::from(directory)
            .join("assets")
            .join(&asset.filename);
        let mut source = File::open(&path)
            .map_err(|error| format!("candidate asset {}: {error}", path.display()))?;
        return consume_published_reader(&mut source, target, asset);
    }
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(30))
        .build();
    let mut url = asset.url.clone();
    let mut response = None;
    for hop in 0..=4 {
        let value = match agent.get(&url).call() {
            Ok(value) | Err(ureq::Error::Status(_, value)) => value,
            Err(error) => return Err(format!("release download failed: {error}")),
        };
        if (300..400).contains(&value.status()) {
            if hop == 4 {
                return Err("too many release redirects".into());
            }
            let next = value
                .header("Location")
                .ok_or("release redirect has no Location")?;
            validate_redirect(next)?;
            url = next.to_owned();
            continue;
        }
        if value.status() != 200 {
            return Err(format!("release download returned HTTP {}", value.status()));
        }
        response = Some(value);
        break;
    }
    let response = response.ok_or("too many release redirects")?;
    let mut reader = response.into_reader();
    consume_published_reader(&mut reader, target, asset)
}

fn consume_published_reader(
    reader: &mut impl Read,
    target: &mut File,
    asset: &ResolvedAsset,
) -> Result<(), String> {
    if asset.media_type == MediaType::Direct {
        verify_direct_reader(reader, target, asset)?;
    } else {
        let mut wheel = tempfile::tempfile().map_err(|error| error.to_string())?;
        io::copy(&mut reader.take(asset.size_bytes + 1), &mut wheel)
            .map_err(|error| error.to_string())?;
        wheel
            .seek(SeekFrom::Start(0))
            .map_err(|error| error.to_string())?;
        let (digest, size) = hash_reader(&mut wheel, MAX_WHEEL_BYTES)?;
        if size != asset.size_bytes || digest != asset.sha256 {
            return Err("wheel size or digest mismatch".into());
        }
        wheel
            .seek(SeekFrom::Start(0))
            .map_err(|error| error.to_string())?;
        extract_wheel(wheel, target, asset)?;
    }
    Ok(())
}

fn current_matches_asset(path: &Path, asset: &ResolvedAsset) -> Result<bool, String> {
    if asset.media_type != MediaType::Direct {
        return Ok(false);
    }
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() != asset.size_bytes {
        return Ok(false);
    }
    Ok(hash_path(path, MAX_BINARY_BYTES)?.0 == asset.sha256)
}

fn verify_direct_reader(
    reader: &mut impl Read,
    target: &mut File,
    asset: &ResolvedAsset,
) -> Result<(), String> {
    io::copy(&mut reader.take(asset.size_bytes + 1), target).map_err(|error| error.to_string())?;
    target.flush().map_err(|error| error.to_string())?;
    target
        .seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let (digest, size) = hash_reader(target, MAX_BINARY_BYTES)?;
    if size != asset.size_bytes || digest != asset.sha256 {
        return Err("release size or digest mismatch".into());
    }
    Ok(())
}

fn validate_redirect(value: &str) -> Result<(), String> {
    let parsed = url::Url::parse(value).map_err(|error| error.to_string())?;
    if parsed.scheme() != "https"
        || parsed.port().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err("unsafe release redirect".into());
    }
    if !matches!(
        parsed.host_str(),
        Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com")
    ) {
        return Err("release redirect left the approved CDN".into());
    }
    Ok(())
}

fn extract_wheel(wheel: File, target: &mut File, asset: &ResolvedAsset) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(wheel).map_err(|error| error.to_string())?;
    if archive.len() > 10_000 {
        return Err("wheel has too many entries".into());
    }
    let expected = if asset.os == Os::Windows {
        "clud.exe"
    } else {
        "clud"
    };
    let mut found = false;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        let path = Path::new(entry.name());
        if entry.name().contains(['\\', ':', '\0'])
            || path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err("unsafe wheel entry path".into());
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("wheel contains a symlink".into());
        }
        if path.file_name().is_some_and(|name| name == expected) {
            if found {
                return Err("wheel has duplicate clud executables".into());
            }
            found = true;
            if entry.size() > MAX_BINARY_BYTES {
                return Err("wheel executable is too large".into());
            }
            io::copy(&mut entry.by_ref().take(MAX_BINARY_BYTES + 1), target)
                .map_err(|error| error.to_string())?;
        }
    }
    if !found {
        return Err("wheel lacks a clud executable".into());
    }
    Ok(())
}

fn make_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    #[cfg(windows)]
    let _ = path;
    Ok(())
}

fn recover(destination: &Path) -> Result<(), String> {
    let backup = destination.with_extension("clud-backup");
    let metadata = match fs::symlink_metadata(&backup) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("inspect prior backup: {error}")),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("installer backup is not a regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.nlink() != 1 {
            return Err("installer backup is not exclusively owned by this user".into());
        }
    }
    #[cfg(windows)]
    if windows_reparse(&metadata) {
        return Err("installer backup is a reparse point".into());
    }
    let mut file = File::open(&backup).map_err(|error| error.to_string())?;
    validate_native_header(&mut file, current_os(), current_arch(), Flavor::Native)?;
    drop(file);
    if destination.exists() && run_version(destination).is_ok() {
        fs::remove_file(&backup).map_err(|error| format!("clear old backup: {error}"))?;
    } else {
        #[cfg(windows)]
        if destination.exists() {
            fs::remove_file(destination).map_err(|error| error.to_string())?;
        }
        fs::rename(&backup, destination).map_err(|error| format!("restore prior clud: {error}"))?;
    }
    Ok(())
}

fn commit(destination: &Path, stage: &Path, version: &str, digest: &str) -> Result<(), String> {
    let backup = destination.with_extension("clud-backup");
    if destination.exists() {
        let mut source = File::open(destination).map_err(|error| error.to_string())?;
        let mut saved = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)
            .map_err(|error| format!("create prior backup: {error}"))?;
        io::copy(&mut source, &mut saved).map_err(|error| format!("backup prior clud: {error}"))?;
        drop(source);
        make_executable(&backup)?;
        saved.sync_all().map_err(|error| error.to_string())?;
        drop(saved);
        #[cfg(windows)]
        if let Err(error) = fs::remove_file(destination) {
            let _ = fs::remove_file(&backup);
            return Err(format!("prior clud is in use; it was preserved: {error}"));
        }
    }
    if let Err(error) = fs::rename(stage, destination) {
        if !destination.exists() && backup.exists() {
            let _ = fs::rename(&backup, destination);
        } else if backup.exists() {
            let _ = fs::remove_file(&backup);
        }
        return Err(format!("activate staged clud: {error}"));
    }
    let verified = hash_path(destination, MAX_BINARY_BYTES)
        .and_then(|(actual, _)| {
            if actual == digest {
                Ok(())
            } else {
                Err("committed executable digest changed".into())
            }
        })
        .and_then(|()| verify_version(destination, version));
    if let Err(error) = verified {
        if backup.exists() {
            #[cfg(windows)]
            fs::remove_file(destination).map_err(|failure| failure.to_string())?;
            fs::rename(&backup, destination).map_err(|failure| failure.to_string())?;
        } else {
            fs::remove_file(destination).map_err(|failure| failure.to_string())?;
        }
        return Err(error);
    }
    if backup.exists() {
        fs::remove_file(backup).map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct_asset(bytes: &[u8]) -> ResolvedAsset {
        ResolvedAsset {
            version: "2.8.14".into(),
            filename: "clud-2.8.14-x86_64-unknown-linux-musl".into(),
            media_type: MediaType::Direct,
            size_bytes: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            url: "https://github.com/zackees/clud/releases/download/2.8.14/clud-2.8.14-x86_64-unknown-linux-musl".into(),
            flavor: Flavor::StaticMusl,
            os: Os::Linux,
            arch: Arch::X86_64,
        }
    }

    #[test]
    fn exact_release_body_rejects_truncation_and_changed_digest() {
        let bytes = b"published native bytes";
        let asset = direct_asset(bytes);
        let mut complete = tempfile::tempfile().unwrap();
        verify_direct_reader(&mut &bytes[..], &mut complete, &asset).unwrap();
        let mut truncated = tempfile::tempfile().unwrap();
        assert!(verify_direct_reader(&mut &bytes[..5], &mut truncated, &asset).is_err());
        let mut altered = tempfile::tempfile().unwrap();
        assert!(
            verify_direct_reader(&mut &b"published native bytez"[..], &mut altered, &asset)
                .is_err()
        );
    }

    #[test]
    fn exact_digest_decides_release_reuse() {
        let bytes = b"published native bytes";
        let asset = direct_asset(bytes);
        let mut local = tempfile::NamedTempFile::new().unwrap();
        local.write_all(bytes).unwrap();
        assert!(current_matches_asset(local.path(), &asset).unwrap());
        local.as_file_mut().set_len(0).unwrap();
        local.write_all(b"different bytes").unwrap();
        assert!(!current_matches_asset(local.path(), &asset).unwrap());
    }

    #[test]
    fn unsafe_redirects_are_rejected() {
        for url in [
            "http://release-assets.githubusercontent.com/a",
            "https://evil.example/a",
            "https://user@release-assets.githubusercontent.com/a",
            "https://release-assets.githubusercontent.com:444/a",
        ] {
            assert!(validate_redirect(url).is_err(), "{url}");
        }
        assert!(validate_redirect("https://release-assets.githubusercontent.com/a").is_ok());
    }

    #[test]
    fn wrong_native_format_is_rejected() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"not a native executable").unwrap();
        assert!(
            validate_native_header(&mut file, Os::Linux, Arch::X86_64, Flavor::Native).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_destination_and_backup_are_rejected_without_following_them() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("other");
        fs::write(&other, b"owned bytes").unwrap();
        let destination = dir.path().join("clud");
        symlink(&other, &destination).unwrap();
        assert!(inspect_destination(&destination, false).is_err());
        fs::remove_file(&destination).unwrap();
        symlink(&other, destination.with_extension("clud-backup")).unwrap();
        assert!(recover(&destination).is_err());
        assert_eq!(fs::read(&other).unwrap(), b"owned bytes");
    }

    #[test]
    fn failed_post_commit_verification_restores_prior_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("clud");
        fs::write(&destination, b"prior bytes").unwrap();
        let stage = dir.path().join("stage");
        fs::write(&stage, b"new bytes").unwrap();
        let digest = format!("{:x}", Sha256::digest(b"new bytes"));
        assert!(commit(&destination, &stage, "0.0.0", &digest).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"prior bytes");
        assert!(!destination.with_extension("clud-backup").exists());
    }
}
