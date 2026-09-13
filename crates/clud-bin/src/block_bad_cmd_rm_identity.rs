//! Owner-sanctioned effective-PATH contract (#1183). Never execute source.
use super::*;

pub(super) fn identity_reason(path_env: &str, trusted: &Path) -> Result<(), String> {
    // Relative/empty components depend on shell cwd, including subsequent cd.
    // Refuse them instead of resolving against the hook process's cwd.
    if path_env.is_empty() || std::env::split_paths(path_env).any(|p| !p.is_absolute()) {
        return Err(
            "rm identity: effective PATH is missing or contains relative/empty entries".into(),
        );
    }
    let found = crate::shim_resolve::which("rm", path_env)
        .ok_or("rm identity: no executable rm on the effective PATH")?;
    let expected = std::fs::read(trusted)
        .map_err(|e| format!("rm identity: packaged clud-shim is unreadable: {e}"))?;
    let actual = std::fs::read(&found)
        .map_err(|e| format!("rm identity: selected rm is unreadable: {e}"))?;
    if expected.is_empty() || actual != expected {
        return Err(format!(
            "rm identity: {} is not byte-identical to packaged clud-shim",
            found.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn source_reason(command: &str) -> Result<(), String> {
    source_reason_with_tap(command, false)
}

fn source_reason_with_tap(command: &str, trusted_tap: bool) -> Result<(), String> {
    let refuse = || "rm identity: command changes or bypasses provable shim resolution".to_string();
    // Substitutions and process substitutions execute in contexts whose
    // environment/startup state this PATH contract cannot establish.
    if command.contains("$(")
        || command.contains('`')
        || command.contains("<(")
        || command.contains(">(")
    {
        return Err(refuse());
    }
    for segment in block_bad_cmd_rm_vars::identity_statements(command).map_err(|_| refuse())? {
        let words = shell_words::split(segment.trim()).map_err(|_| refuse())?;
        let mut index = 0;
        while words.get(index).is_some_and(|w| is_env_assignment(w)) {
            let name = words[index].split('=').next().unwrap_or_default();
            if resolution_variable(name) {
                return Err(refuse());
            }
            index += 1;
        }
        // Packaged tap's CommandSpec::Argv forwards this exact environment.
        // Its identity must also be established before treating it as transparent.
        if trusted_tap
            && words
                .get(index)
                .is_some_and(|w| w == "tap" || w == "tap.exe")
        {
            index += 1;
        }
        let Some(program) = words.get(index) else {
            continue;
        };
        let base = program_name(program);
        if program == "."
            || program.contains([
                '$', '`', '\\', '(', ')', '{', '}', '*', '?', '[', ']', '~', '!',
            ])
        {
            return Err(refuse());
        }
        if base == "rm" && !matches!(program.as_str(), "rm" | "rm.exe") {
            return Err(refuse());
        }
        if matches!(
            base.as_str(),
            "hash"
                | "alias"
                | "unalias"
                | "source"
                | "."
                | "enable"
                | "function"
                | "eval"
                | "builtin"
                | "command"
                | "trap"
                | "bind"
                | "complete"
                | "compgen"
                | "fc"
        ) {
            return Err(refuse());
        }
        if matches!(
            base.as_str(),
            "export"
                | "unset"
                | "readonly"
                | "declare"
                | "typeset"
                | "local"
                | "read"
                | "mapfile"
                | "readarray"
                | "getopts"
        ) {
            if words[index + 1..].iter().any(|w| {
                w.contains('$')
                    || resolution_variable(
                        w.split('=')
                            .next()
                            .unwrap_or_default()
                            .trim_end_matches('+'),
                    )
                    || (w.starts_with('-') && w.contains('n'))
            }) {
                return Err(refuse());
            }
        }
        // printf -v assigns to a computed variable; namerefs and array syntax
        // make even a seemingly unrelated destination unprovable here.
        if base == "printf" && words[index + 1..].iter().any(|w| w == "-v") {
            return Err(refuse());
        }
        if !matches!(base.as_str(), "echo" | "printf")
            && words[index + 1..].iter().any(|w| {
                w.split_once('=')
                    .is_some_and(|(name, _)| resolution_variable(name.trim_end_matches('+')))
            })
        {
            return Err(refuse());
        }
        if matches!(base.as_str(), "env" | "exec" | "sudo" | "su")
            && words[index + 1..].iter().any(|w| w.contains('$'))
        {
            return Err(refuse());
        }
        // Wrappers can select a different PATH, shell startup state, or an
        // internal applet. Only a direct rm command has the checked contract.
        if base != "rm"
            && !matches!(base.as_str(), "echo" | "printf")
            && !(base == "git" && words.get(index + 1).is_some_and(|w| w == "rm"))
            && words[index + 1..]
                .iter()
                .any(|w| program_name(w) == "rm" || w.contains("rm "))
        {
            return Err(refuse());
        }
        if matches!(base.as_str(), "env" | "sudo" | "su" | "exec" | "command")
            && words[index + 1..].iter().any(|w| {
                w.starts_with("PATH=") || w == "-p" || w == "-i" || w == "--ignore-environment"
            })
        {
            return Err(refuse());
        }
    }
    Ok(())
}

fn resolution_variable(name: &str) -> bool {
    matches!(
        name,
        "PATH"
            | "PATHEXT"
            | "ENV"
            | "BASH_ENV"
            | "SHELLOPTS"
            | "BASHOPTS"
            | "IFS"
            | "LD_PRELOAD"
            | "LD_LIBRARY_PATH"
            | "PROMPT_COMMAND"
    )
}

pub(super) fn check(command: &str, path_env: &str) -> Result<(), String> {
    let trusted = crate::shim_install::packaged_shim().map_err(|e| format!("rm identity: {e}"))?;
    identity_reason(path_env, &trusted)?;
    let trusted_tap = if command.contains("tap") {
        let name = if cfg!(windows) { "tap.exe" } else { "tap" };
        let packaged = trusted.with_file_name(name);
        crate::shim_resolve::which("tap", path_env)
            .and_then(|p| std::fs::read(p).ok())
            .zip(std::fs::read(packaged).ok())
            .is_some_and(|(actual, expected)| !expected.is_empty() && actual == expected)
    } else {
        false
    };
    source_reason_with_tap(command, trusted_tap)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allows_exact_bytes_and_denies_missing_replaced_system() {
        let tmp = tempfile::tempdir().unwrap();
        let trusted = tmp.path().join("clud-shim");
        let rm = tmp.path().join(if cfg!(windows) { "rm.exe" } else { "rm" });
        std::fs::write(&trusted, b"packaged bytes").unwrap();
        let path = tmp.path().to_str().unwrap();
        assert!(identity_reason(path, &trusted).is_err());
        std::fs::copy(&trusted, &rm).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&rm, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(identity_reason(path, &trusted), Ok(()));
        std::fs::write(&rm, b"replacement").unwrap();
        assert!(identity_reason(path, &trusted).is_err());
        assert!(identity_reason("", &trusted).is_err());
        assert!(identity_reason("/bin:/usr/bin", &trusted).is_err());
        assert!(identity_reason(path, &tmp.path().join("missing")).is_err());
    }
    #[test]
    fn source_contract_both_directions() {
        for source in [
            "rm -rf ./build",
            "V=/tmp/safe; rm -rf \"$V\"/",
            "echo 'rm -rf /'",
            "git rm -r --cached foo",
            "docker run --rm ubuntu",
        ] {
            assert!(source_reason(source).is_ok(), "{source}");
        }
        for source in [
            "/bin/rm -rf ./build",
            "env PATH=/bin rm x",
            "command -p rm x",
            "busybox rm x",
            "sudo rm x",
            "hash -p /bin/rm rm",
            "alias rm=/bin/rm",
            "PATH=/bin; rm x",
            "export PATH=/bin",
            "bash -c 'rm x'",
            "$REMOVE x",
            "source setup.sh",
            ". /tmp/setup",
            "builtin printf -v PATH /bin; rm ./build",
            "command printf -v PATH /bin; rm ./build",
            "trap ' PATH=/bin' DEBUG; rm ./build",
            "r? ./build",
            "NAME=PATH; export \"$NAME=/bin\"; rm ./build",
            "env BASH_ENV=/tmp/setup bash -c true",
            "printf -v PATH /bin; rm ./build",
            "declare -n P=PATH; P=/bin; rm ./build",
            "echo ok & /bin/rm ./build",
            "echo \"$(/bin/rm ./build)\"",
        ] {
            assert!(source_reason(source).is_err(), "{source}");
        }
    }
}
