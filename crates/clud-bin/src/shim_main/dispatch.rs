//! The one entry path for every `clud-shim` alias (#1546).
//!
//! This module alone owns the three decisions every shim shares:
//!
//! 1. **Session detection.** A session is valid when [`registry::ABI_KEY`]
//!    equals [`registry::SHIM_ABI`] and the shim's own keys are present and
//!    sane. A session built by a different clud version fails the stamp.
//! 2. **Target resolution.** A session target must be absolute, executable,
//!    outside every shim directory, and not a copy of this binary.
//! 3. **Fail-open passthrough.** Without a valid session a `Passthrough` shim
//!    execs the next same-named binary on PATH, printing nothing. The only
//!    failure is the shell's own `127 command not found`, when no real binary
//!    exists.
//!
//! Handlers in `shim_main.rs` receive a [`Session`] and never read a session
//! key, so none of them can fail closed for a session reason. The guard test
//! in `shim_main.rs` enforces that split.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use crate::shim_registry::{self as registry, Fallback, ShimKind};

/// The personality's own name; `clud-shim --registry` prints the registry.
const SELF_NAME: &str = "clud-shim";

pub struct GhSession {
    pub target: PathBuf,
    /// `CLUD_EXE`, for the `pr checks --watch` upgrade. Without it the
    /// upgrade is skipped and the real `gh` runs the command unchanged.
    pub watcher: Option<PathBuf>,
    pub fail_fast: bool,
}

pub enum Session {
    Python { target: PathBuf },
    Gh(GhSession),
    Rm,
}

/// Everything dispatch reads from the process, injectable for tests.
pub struct Facts<'a> {
    pub self_exe: PathBuf,
    pub path: OsString,
    pub home: Option<PathBuf>,
    pub var: &'a dyn Fn(&str) -> Option<OsString>,
}

pub fn run(argv: &[OsString]) -> i32 {
    let argv0 = argv.first().cloned().unwrap_or_default();
    let args = argv.get(1..).unwrap_or(&[]);
    let name = registry::invoked_name(&argv0);
    let is_self_name = name
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case(SELF_NAME));
    if is_self_name && args == [OsString::from("--registry")] {
        println!("{}", registry_json());
        return 0;
    }
    let var = |key: &str| std::env::var_os(key);
    let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let facts = Facts {
        self_exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from(&argv0)),
        path: std::env::var_os("PATH").unwrap_or_default(),
        home: std::env::var_os(home_key).map(PathBuf::from),
        var: &var,
    };
    match decide(&argv0, &facts) {
        Decision::Native(kind) => native(kind, args),
        Decision::Session(session) => handle(session, args),
        Decision::Passthrough(name) => match resolve_passthrough(&name, &facts) {
            Some(real) => super::exec(&name, &real, args),
            None => {
                eprintln!("{name}: command not found");
                127
            }
        },
    }
}

pub enum Decision {
    Native(ShimKind),
    Session(Session),
    Passthrough(String),
}

pub fn decide(argv0: &OsStr, facts: &Facts) -> Decision {
    let Some(spec) = registry::lookup(argv0) else {
        let name = registry::invoked_name(argv0).unwrap_or_default();
        return Decision::Passthrough(name);
    };
    if spec.fallback == Fallback::Native {
        return Decision::Native(spec.kind);
    }
    match session(spec.kind, facts) {
        Some(session) => Decision::Session(session),
        None => Decision::Passthrough(spec.name.to_string()),
    }
}

fn session(kind: ShimKind, facts: &Facts) -> Option<Session> {
    let var = facts.var;
    if var(registry::ABI_KEY).as_deref() != Some(OsStr::new(registry::SHIM_ABI)) {
        return None;
    }
    let session_dir = var(registry::SESSION_DIR_KEY)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute() && dir.is_dir());
    let dirs = registry::shim_dirs(
        &facts.self_exe,
        session_dir.as_deref(),
        facts.home.as_deref(),
    );
    let target = |key: &str| {
        var(key)
            .map(PathBuf::from)
            .filter(|path| registry::valid_target(path, &facts.self_exe, &dirs))
    };
    match kind {
        ShimKind::Python => {
            target(registry::PYTHON_TARGET_KEY).map(|target| Session::Python { target })
        }
        ShimKind::Gh => target(registry::GH_TARGET_KEY).map(|target| {
            Session::Gh(GhSession {
                target,
                watcher: var(registry::CLUD_EXE_KEY)
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute() && path.is_file()),
                fail_fast: var(registry::GH_FAIL_FAST_KEY).as_deref() != Some(OsStr::new("0")),
            })
        }),
        ShimKind::Rm => session_dir.map(|_| Session::Rm),
        // Native: never validated here.
        ShimKind::SafeRm => None,
    }
}

/// The next real `name` on PATH after the shim directories.
pub fn resolve_passthrough(name: &str, facts: &Facts) -> Option<PathBuf> {
    let session_dir = (facts.var)(registry::SESSION_DIR_KEY).map(PathBuf::from);
    let dirs = registry::shim_dirs(
        &facts.self_exe,
        session_dir.as_deref(),
        facts.home.as_deref(),
    );
    registry::next_on_path(
        &registry::file_name(name),
        &facts.path,
        &facts.self_exe,
        &dirs,
        false,
    )
    .ok()
}

fn handle(session: Session, args: &[OsString]) -> i32 {
    match session {
        Session::Python { target } => super::python_shim::run(&target, args),
        Session::Gh(gh) => super::gh_shim::run(&gh, args),
        Session::Rm => super::rm_shim::run(args),
    }
}

fn native(kind: ShimKind, args: &[OsString]) -> i32 {
    match kind {
        ShimKind::SafeRm => super::safe_rm::run(args),
        // A registry test keeps every other kind `Passthrough`.
        ShimKind::Python | ShimKind::Gh | ShimKind::Rm => {
            unreachable!("{kind:?} is not a native shim")
        }
    }
}

fn registry_json() -> String {
    let shims: Vec<_> = registry::SHIMS
        .iter()
        .map(|spec| {
            serde_json::json!({
                "name": spec.name,
                "fallback": format!("{:?}", spec.fallback),
                "session_keys": spec.session_keys,
            })
        })
        .collect();
    serde_json::json!({
        "abi_key": registry::ABI_KEY,
        "abi": registry::SHIM_ABI,
        "session_dir_key": registry::SESSION_DIR_KEY,
        "shims": shims,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;
    use tempfile::TempDir;

    fn write_exe(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    struct World {
        _temp: TempDir,
        shim_dir: PathBuf,
        real_dir: PathBuf,
    }

    impl World {
        fn new() -> Self {
            let temp = TempDir::new().unwrap();
            let shim_dir = temp.path().join("shim");
            let real_dir = temp.path().join("real");
            for spec in registry::SHIMS {
                write_exe(&shim_dir.join(registry::file_name(spec.name)), b"shim");
                write_exe(&real_dir.join(registry::file_name(spec.name)), b"real");
            }
            World {
                _temp: temp,
                shim_dir,
                real_dir,
            }
        }

        fn decide(&self, name: &str, vars: &HashMap<&str, OsString>) -> Decision {
            let var = |key: &str| vars.get(key).cloned();
            let facts = Facts {
                self_exe: self.shim_dir.join(registry::file_name(name)),
                path: std::env::join_paths([&self.shim_dir, &self.real_dir]).unwrap(),
                home: None,
                var: &var,
            };
            decide(OsStr::new(name), &facts)
        }

        fn real(&self, name: &str) -> PathBuf {
            self.real_dir.join(registry::file_name(name))
        }

        fn valid_session(&self) -> HashMap<&'static str, OsString> {
            HashMap::from([
                (registry::ABI_KEY, OsString::from(registry::SHIM_ABI)),
                (
                    registry::SESSION_DIR_KEY,
                    self.shim_dir.clone().into_os_string(),
                ),
                (
                    registry::PYTHON_TARGET_KEY,
                    self.real("python").into_os_string(),
                ),
                (registry::GH_TARGET_KEY, self.real("gh").into_os_string()),
            ])
        }
    }

    fn passthrough_name(decision: Decision) -> Option<String> {
        match decision {
            Decision::Passthrough(name) => Some(name),
            _ => None,
        }
    }

    #[test]
    fn every_passthrough_shim_passes_through_with_an_empty_session() {
        let world = World::new();
        for spec in registry::SHIMS {
            let decision = world.decide(spec.name, &HashMap::new());
            match spec.fallback {
                Fallback::Passthrough => {
                    assert_eq!(passthrough_name(decision).as_deref(), Some(spec.name));
                }
                Fallback::Native => {
                    assert!(matches!(decision, Decision::Native(_)), "{}", spec.name)
                }
            }
        }
    }

    #[test]
    fn every_passthrough_shim_runs_its_handler_in_a_valid_session() {
        let world = World::new();
        let vars = world.valid_session();
        for spec in registry::SHIMS
            .iter()
            .filter(|s| s.fallback == Fallback::Passthrough)
        {
            assert!(
                matches!(world.decide(spec.name, &vars), Decision::Session(_)),
                "{}",
                spec.name
            );
        }
    }

    #[test]
    fn a_stale_or_foreign_session_passes_through() {
        let world = World::new();
        let valid = world.valid_session();
        let mut cases: Vec<(&str, HashMap<&str, OsString>)> = Vec::new();
        let mut no_stamp = valid.clone();
        no_stamp.remove(registry::ABI_KEY);
        cases.push(("missing ABI stamp (older clud's session)", no_stamp));
        let mut other_abi = valid.clone();
        other_abi.insert(registry::ABI_KEY, OsString::from("0"));
        cases.push(("different ABI", other_abi));
        for (key, name) in [
            (registry::GH_TARGET_KEY, "gh"),
            (registry::PYTHON_TARGET_KEY, "python"),
        ] {
            for (label, value) in [
                ("deleted", world.real_dir.join("deleted").into_os_string()),
                ("relative", OsString::from(registry::file_name(name))),
                (
                    "into the shim dir",
                    world
                        .shim_dir
                        .join(registry::file_name(name))
                        .into_os_string(),
                ),
            ] {
                let mut vars = valid.clone();
                vars.insert(key, value);
                let decision = world.decide(name, &vars);
                assert_eq!(passthrough_name(decision).as_deref(), Some(name), "{label}");
            }
        }
        let mut no_dir = valid.clone();
        no_dir.insert(
            registry::SESSION_DIR_KEY,
            world.real_dir.join("gone").into_os_string(),
        );
        assert_eq!(
            passthrough_name(world.decide("rm", &no_dir)).as_deref(),
            Some("rm")
        );
        for (label, vars) in cases {
            for spec in registry::SHIMS
                .iter()
                .filter(|s| s.fallback == Fallback::Passthrough)
            {
                let decision = world.decide(spec.name, &vars);
                assert_eq!(
                    passthrough_name(decision).as_deref(),
                    Some(spec.name),
                    "{label}"
                );
            }
        }
    }

    #[test]
    fn passthrough_resolves_the_real_binary_and_never_the_shim() {
        let world = World::new();
        let var = |_: &str| None;
        for spec in registry::SHIMS {
            let facts = Facts {
                self_exe: world.shim_dir.join(registry::file_name(spec.name)),
                path: std::env::join_paths([&world.shim_dir, &world.real_dir]).unwrap(),
                home: None,
                var: &var,
            };
            assert_eq!(
                resolve_passthrough(spec.name, &facts),
                Some(world.real(spec.name))
            );
            let alone = Facts {
                path: std::env::join_paths([&world.shim_dir]).unwrap(),
                ..facts
            };
            assert_eq!(
                resolve_passthrough(spec.name, &alone),
                None,
                "{}",
                spec.name
            );
        }
    }

    #[test]
    fn gh_watch_upgrade_needs_a_watcher_and_honors_the_fail_fast_opt_out() {
        let world = World::new();
        let mut vars = world.valid_session();
        let Decision::Session(Session::Gh(gh)) = world.decide("gh", &vars) else {
            panic!("valid gh session");
        };
        assert!(gh.watcher.is_none() && gh.fail_fast);
        vars.insert(registry::CLUD_EXE_KEY, world.real("gh").into_os_string());
        vars.insert(registry::GH_FAIL_FAST_KEY, OsString::from("0"));
        let Decision::Session(Session::Gh(gh)) = world.decide("gh", &vars) else {
            panic!("valid gh session");
        };
        assert_eq!(gh.watcher, Some(world.real("gh")));
        assert!(!gh.fail_fast);
    }

    #[test]
    fn unregistered_names_pass_through_under_their_own_name() {
        let world = World::new();
        assert_eq!(
            passthrough_name(world.decide("node", &world.valid_session())).as_deref(),
            Some("node")
        );
    }
}
