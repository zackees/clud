//! Early installer entry and automatic-offer eligibility.

use std::io::{self, IsTerminal, Read};
use std::time::Duration;

use crate::args::Args;

use super::catalog::{Arch, Catalog, GnuEligibility, Host, Os, ResolvedAsset, VersionChoice};
use super::picker::{self, ConfirmChoice, MenuChoice, ReleaseChoice};
use super::transaction;

const CATALOG_URL: &str = "https://zackees.github.io/clud/install/manifest.json";
pub const INSTALL_PAGE: &str = "https://zackees.github.io/clud/install/index.html";
const MAX_CATALOG_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Menu,
    Current,
    Exact(String),
}

pub fn explicit_request(args: &Args) -> Result<Option<Request>, String> {
    if !args.installer {
        return Ok(None);
    }
    // An installer invocation has no backend argv. Keep unrelated launch
    // flags from changing behavior when the menu closes.
    let mut words = args.raw_argv.iter().skip(1);
    while let Some(word) = words.next() {
        match word.as_str() {
            "--installer" | "--install-current" | "--yes" | "-y" => {}
            "--install-version" => {
                words.next().ok_or("--install-version requires a version")?;
            }
            _ if word.starts_with("--install-version=") => {}
            _ => return Err(format!("{word} cannot be combined with --installer")),
        }
    }
    if args.yes && !args.install_current && args.install_version.is_none() {
        return Err("--yes requires --install-current or --install-version".into());
    }
    if args.install_current {
        Ok(Some(Request::Current))
    } else if let Some(version) = &args.install_version {
        crate::self_install::catalog::validate_version(version)?;
        Ok(Some(Request::Exact(version.clone())))
    } else {
        Ok(Some(Request::Menu))
    }
}

pub fn should_offer(args: &Args, stdin_tty: bool, stderr_tty: bool, path_has_clud: bool) -> bool {
    stdin_tty
        && stderr_tty
        && !path_has_clud
        && !args.installer
        && args.command.is_none()
        && !args.dry_run
        && args.prompt.is_none()
        && args.message.is_none()
        && args.passthrough.is_empty()
        && args.raw_argv.len() == 1
}

/// Run an explicit installer request before launch setup has side effects.
pub fn run_explicit(args: &Args) -> Option<i32> {
    match explicit_request(args) {
        Ok(None) => None,
        Ok(Some(request)) => Some(run_request(request, args.yes, false)),
        Err(error) => {
            eprintln!("clud installer: {error}");
            Some(2)
        }
    }
}

/// Offer installation only for a bare interactive first run with no `clud`
/// executable resolvable by name on the effective PATH.
pub fn run_auto_offer(args: &Args) -> Option<i32> {
    let missing = matches!(
        which::which("clud"),
        Err(which::Error::CannotFindBinaryPath)
    );
    if !should_offer(
        args,
        io::stdin().is_terminal(),
        io::stderr().is_terminal(),
        !missing,
    ) {
        return None;
    }
    match picker::prompt_menu(true) {
        Ok(MenuChoice::NotNow) => None,
        Ok(choice) => Some(run_menu_choice(choice)),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Some(130),
        Err(error) => {
            eprintln!("clud installer: {error}");
            Some(1)
        }
    }
}

fn run_request(request: Request, yes: bool, automatic: bool) -> i32 {
    match request {
        Request::Menu => {
            if !terminal_available() {
                eprintln!("clud installer: a terminal is required without an explicit install choice and --yes");
                return 2;
            }
            match picker::prompt_menu(automatic) {
                Ok(choice) => run_menu_choice(choice),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => 130,
                Err(error) => {
                    eprintln!("clud installer: {error}");
                    1
                }
            }
        }
        Request::Current => run_selected(InstallIntent::CurrentExecutable, yes),
        Request::Exact(version) => {
            if !yes && !terminal_available() {
                eprintln!("clud installer: a terminal or --yes is required before installation");
                return 2;
            }
            let catalog = match fetch_catalog() {
                Ok(catalog) => catalog,
                Err(error) => {
                    eprintln!("clud installer: {error}");
                    return 1;
                }
            };
            let host = match host() {
                Ok(host) => host,
                Err(error) => {
                    eprintln!("clud installer: {error}");
                    return 1;
                }
            };
            match catalog.resolve(VersionChoice::Exact(version), host) {
                Ok(asset) => run_selected(InstallIntent::Published(asset), yes),
                Err(error) => {
                    eprintln!("clud installer: {error}");
                    1
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallIntent {
    CurrentExecutable,
    Published(ResolvedAsset),
}

fn run_selected(intent: InstallIntent, yes: bool) -> i32 {
    if !yes && !terminal_available() {
        eprintln!("clud installer: a terminal or --yes is required before installation");
        return 2;
    }
    let plan = match transaction::plan(intent) {
        Ok(plan) => plan,
        Err(error) => {
            eprintln!("clud installer: {error}");
            return 1;
        }
    };
    eprintln!("{}", plan.description());
    if !yes {
        let label = format!("Install clud {} at this destination?", plan.version);
        match picker::prompt_confirm(label) {
            Ok(ConfirmChoice::NotNow) => return 0,
            Ok(ConfirmChoice::Cancelled) => return 130,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return 130,
            Err(error) => {
                eprintln!("clud installer: {error}");
                return 1;
            }
            Ok(ConfirmChoice::Proceed) => {}
        }
    }
    let activation = plan.activation.clone();
    let version = plan.version.clone();
    match transaction::execute(plan) {
        Ok(()) => {
            match activation.apply_and_verify(&version) {
                Ok(()) => {
                    eprintln!("clud installer: installed clud {version} and verified fresh name-based lookup");
                    0
                }
                Err(error) => {
                    eprintln!("clud installer: binary committed, but activation failed: {error}");
                    1
                }
            }
        }
        Err(error) => {
            eprintln!("clud installer: {error}");
            1
        }
    }
}

fn run_menu_choice(choice: MenuChoice) -> i32 {
    match choice {
        MenuChoice::Current => run_selected(InstallIntent::CurrentExecutable, false),
        MenuChoice::Releases => run_releases(),
        MenuChoice::Browser => {
            open_page_with(|url| open::that_detached(url).map_err(|error| error.to_string()))
        }
        MenuChoice::NotNow => 0,
        MenuChoice::Cancelled => 130,
    }
}

fn run_releases() -> i32 {
    let catalog = match fetch_catalog() {
        Ok(catalog) => catalog,
        Err(error) => {
            eprintln!("clud installer: {error}");
            return 1;
        }
    };
    let host = match host() {
        Ok(host) => host,
        Err(error) => {
            eprintln!("clud installer: {error}");
            return 1;
        }
    };
    let releases = catalog.compatible_releases(host);
    if releases.is_empty() {
        eprintln!("clud installer: no complete release is compatible with this host");
        return 1;
    }
    match picker::prompt_releases(releases, catalog.latest_stable_version()) {
        Ok(ReleaseChoice::Selected(selected)) => {
            run_selected(InstallIntent::Published(selected.asset), false)
        }
        Ok(ReleaseChoice::Cancelled) => 130,
        Err(error) if error.kind() == io::ErrorKind::Interrupted => 130,
        Err(error) => {
            eprintln!("clud installer: {error}");
            1
        }
    }
}

fn open_page_with(open: impl FnOnce(&str) -> Result<(), String>) -> i32 {
    println!("{INSTALL_PAGE}");
    match open(INSTALL_PAGE) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("clud installer: could not open browser: {error}; use {INSTALL_PAGE}");
            1
        }
    }
}

fn terminal_available() -> bool {
    io::stdin().is_terminal() && io::stderr().is_terminal()
}

fn fetch_catalog() -> Result<Catalog, String> {
    #[cfg(feature = "installer-ci-fixture")]
    if let Some(directory) = std::env::var_os("CLUD_INSTALLER_CI_FIXTURE_DIR") {
        let path = std::path::PathBuf::from(directory).join("catalog.json");
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("candidate catalog {}: {error}", path.display()))?;
        if bytes.len() as u64 > MAX_CATALOG_BYTES {
            return Err("candidate catalog exceeds size limit".into());
        }
        return Catalog::parse(&bytes);
    }
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(12))
        .redirects(0)
        .build()
        .get(CATALOG_URL)
        .call()
        .map_err(|error| format!("catalog fetch failed: {error}"))?;
    if response.get_url() != CATALOG_URL {
        return Err("catalog response changed origin".into());
    }
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_CATALOG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("catalog read failed: {error}"))?;
    Catalog::parse(&bytes)
}

fn host() -> Result<Host, String> {
    let os = match std::env::consts::OS {
        "windows" => Os::Windows,
        "macos" => Os::Darwin,
        "linux" => Os::Linux,
        value => return Err(format!("unsupported host OS: {value}")),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => Arch::X86_64,
        "aarch64" => Arch::Aarch64,
        value => return Err(format!("unsupported host architecture: {value}")),
    };
    Ok(Host {
        os,
        arch,
        gnu: gnu_eligibility(arch),
    })
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn gnu_eligibility(arch: Arch) -> GnuEligibility {
    use std::ffi::CStr;
    let nixos = std::fs::read_to_string("/etc/os-release")
        .ok()
        .is_some_and(|text| {
            text.lines()
                .any(|line| line.trim_matches('"') == "ID=nixos" || line == "ID=\"nixos\"")
        });
    if nixos {
        return GnuEligibility::Unverified;
    }
    let loader = match arch {
        Arch::X86_64 => "/lib64/ld-linux-x86-64.so.2",
        Arch::Aarch64 => "/lib/ld-linux-aarch64.so.1",
    };
    if !std::path::Path::new(loader).exists() {
        return GnuEligibility::Unverified;
    }
    // SAFETY: glibc returns a process-lifetime NUL-terminated version string.
    let version = unsafe { CStr::from_ptr(libc::gnu_get_libc_version()) }.to_string_lossy();
    let mut parts = version
        .split('.')
        .take(2)
        .filter_map(|part| part.parse::<u64>().ok());
    match (parts.next(), parts.next()) {
        (Some(major), Some(minor)) if (major, minor) >= (2, 17) => GnuEligibility::VerifiedGlibc217,
        _ => GnuEligibility::Unverified,
    }
}

#[cfg(all(target_os = "linux", target_env = "musl"))]
fn gnu_eligibility(arch: Arch) -> GnuEligibility {
    let nixos = std::fs::read_to_string("/etc/os-release")
        .ok()
        .is_some_and(|text| {
            text.lines()
                .any(|line| line.trim_matches('"') == "ID=nixos" || line == "ID=\"nixos\"")
        });
    if nixos {
        return GnuEligibility::Unverified;
    }
    let loader = match arch {
        Arch::X86_64 => "/lib64/ld-linux-x86-64.so.2",
        Arch::Aarch64 => "/lib/ld-linux-aarch64.so.1",
    };
    if !std::path::Path::new(loader).exists() {
        return GnuEligibility::Unverified;
    }
    let Ok(process) = crate::subprocess::ManagedSubprocess::start_inheriting_env(
        vec!["/usr/bin/getconf".into(), "GNU_LIBC_VERSION".into()],
        None,
        true,
        None,
    ) else {
        return GnuEligibility::Unverified;
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut output = Vec::new();
    loop {
        if std::time::Instant::now() >= deadline || output.len() > 64 {
            let _ = process.kill();
            return GnuEligibility::Unverified;
        }
        match process.read_stdout(Some(Duration::from_millis(50))) {
            running_process::ReadStatus::Line(line) => output.extend_from_slice(&line),
            running_process::ReadStatus::Timeout => {
                let _ = process.poll();
            }
            running_process::ReadStatus::Eof => break,
        }
    }
    if process.wait(Some(Duration::from_secs(1))) != Ok(0) {
        return GnuEligibility::Unverified;
    }
    let Ok(text) = std::str::from_utf8(&output) else {
        return GnuEligibility::Unverified;
    };
    let Some(version) = text.trim().strip_prefix("glibc ") else {
        return GnuEligibility::Unverified;
    };
    let Some((major, minor)) = version.split_once('.') else {
        return GnuEligibility::Unverified;
    };
    match (major.parse::<u64>(), minor.parse::<u64>()) {
        (Ok(major), Ok(minor)) if (major, minor) >= (2, 17) => GnuEligibility::VerifiedGlibc217,
        _ => GnuEligibility::Unverified,
    }
}

#[cfg(not(target_os = "linux"))]
fn gnu_eligibility(_arch: Arch) -> GnuEligibility {
    GnuEligibility::Unverified
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(parts: &[&str]) -> Args {
        Args::parse_from_raw(parts.iter().map(|part| (*part).to_owned()).collect())
    }

    #[test]
    fn explicit_flags_are_claimed_and_scoped() {
        let menu = parse(&["clud", "--installer"]);
        assert!(menu.passthrough.is_empty());
        assert_eq!(explicit_request(&menu).unwrap(), Some(Request::Menu));

        let current = parse(&["clud", "--installer", "--install-current", "--yes"]);
        assert!(current.passthrough.is_empty());
        assert_eq!(explicit_request(&current).unwrap(), Some(Request::Current));

        let exact = parse(&[
            "clud",
            "--installer",
            "--install-version",
            "2.8.14",
            "--yes",
        ]);
        assert!(exact.passthrough.is_empty());
        assert_eq!(
            explicit_request(&exact).unwrap(),
            Some(Request::Exact("2.8.14".into()))
        );

        assert!(explicit_request(&parse(&["clud", "--installer", "--yes"])).is_err());
        assert!(explicit_request(&parse(&["clud", "--installer", "--", "--codex"])).is_err());
        assert!(
            explicit_request(&parse(&["clud", "--yes", "--clean-worktrees"]))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn invalid_installer_combinations_fail_in_clap() {
        use clap::Parser;
        assert!(Args::try_parse_from(["clud", "--install-current"]).is_err());
        assert!(Args::try_parse_from(["clud", "--install-version", "2.8.14"]).is_err());
        assert!(Args::try_parse_from([
            "clud",
            "--installer",
            "--install-current",
            "--install-version",
            "2.8.14"
        ])
        .is_err());
    }

    #[test]
    fn automatic_offer_requires_plain_interactive_launch_and_missing_path_name() {
        let bare = parse(&["clud"]);
        assert!(should_offer(&bare, true, true, false));
        assert!(!should_offer(&bare, true, true, true));
        assert!(!should_offer(&bare, false, true, false));
        assert!(!should_offer(&bare, true, false, false));
        assert!(!should_offer(
            &parse(&["clud", "--dry-run"]),
            true,
            true,
            false
        ));
        assert!(!should_offer(
            &parse(&["clud", "--prompt", "hi"]),
            true,
            true,
            false
        ));
        assert!(!should_offer(
            &parse(&["clud", "models", "cheapest"]),
            true,
            true,
            false
        ));
    }

    #[test]
    fn no_tty_without_automation_exits_before_fetch_or_install() {
        assert_eq!(run_explicit(&parse(&["clud", "--installer"])), Some(2));
        assert_eq!(
            run_explicit(&parse(&["clud", "--installer", "--install-current"])),
            Some(2)
        );
        assert_eq!(
            run_explicit(&parse(&[
                "clud",
                "--installer",
                "--install-version",
                "2.8.14"
            ])),
            Some(2)
        );
    }

    #[test]
    fn browser_failure_reports_the_canonical_page() {
        let mut called = false;
        assert_eq!(
            open_page_with(|url| {
                called = true;
                assert_eq!(url, INSTALL_PAGE);
                Err("opener unavailable".into())
            }),
            1
        );
        assert!(called);
    }
}
