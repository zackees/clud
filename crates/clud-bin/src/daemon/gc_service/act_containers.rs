//! `act` job containers whose clud worktree is gone (#1878).
//!
//! A local `act` run binds the worktree it tests. When act exits without
//! cleanup (killed, timed out, `--reuse`) its container keeps running after
//! clud removes the worktree, holding volumes and memory for days. Nothing
//! ties the container to the worktree's lifetime, so the probe thread sweeps
//! them: an `act-*` container is orphaned when one of its bind sources lies
//! under a clud worktree root and no longer exists. Its own per-run volumes
//! (`<name>` and `<name>-env`) go with it; shared ones such as
//! `act-toolcache` are never touched.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::extern_repo::probe_cmd;

/// One `act-*` container: its name and bind-mount sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActContainer {
    pub(crate) name: String,
    pub(crate) binds: Vec<PathBuf>,
}

/// Pure: the containers to remove. A container qualifies only when its name
/// starts with `act-` and some bind source is strictly under one of `roots`
/// and missing.
pub(crate) fn orphaned(
    containers: &[ActContainer],
    roots: &[PathBuf],
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<String> {
    containers
        .iter()
        .filter(|c| c.name.starts_with("act-"))
        .filter(|c| {
            c.binds.iter().any(|source| {
                roots
                    .iter()
                    .any(|root| source.starts_with(root) && source != root)
                    && !exists(source)
            })
        })
        .map(|c| c.name.clone())
        .collect()
}

/// Pure: parse `docker inspect --format '{{.Name}}\t{{json .Mounts}}'`.
/// `None` when any line is malformed: an incomplete answer removes nothing.
pub(crate) fn parse_inspect(output: &str) -> Option<Vec<ActContainer>> {
    let mut out = Vec::new();
    for line in output.lines().filter(|l| !l.trim().is_empty()) {
        let (name, mounts) = line.split_once('\t')?;
        let mounts: serde_json::Value = serde_json::from_str(mounts).ok()?;
        let mut binds = Vec::new();
        for mount in mounts.as_array()? {
            if mount.get("Type")?.as_str()? == "bind" {
                binds.push(PathBuf::from(mount.get("Source")?.as_str()?));
            }
        }
        out.push(ActContainer {
            name: name.trim_start_matches('/').to_string(),
            binds,
        });
    }
    Some(out)
}

/// Remove orphaned `act-*` containers for `roots`. Best effort; logs.
pub(crate) fn sweep(roots: &[PathBuf]) {
    // Production only, like the legacy-root migration: a unit test's probe
    // must never touch the host's containers.
    if cfg!(test) {
        return;
    }
    let Some(docker) = docker_path() else {
        return;
    };
    let Some(cwd) = roots
        .iter()
        .find(|r| r.is_dir())
        .cloned()
        .or_else(|| std::env::current_dir().ok())
    else {
        return;
    };
    let Some((0, ids)) = probe_cmd(
        &docker,
        &cwd,
        &["ps", "--all", "--quiet", "--filter", "name=^act-"],
        Duration::from_secs(5),
    ) else {
        return;
    };
    let ids: Vec<&str> = ids.split_whitespace().take(256).collect();
    if ids.is_empty() {
        return;
    }
    let mut args = vec!["inspect", "--format", "{{.Name}}\t{{json .Mounts}}"];
    args.extend(ids);
    let Some((0, output)) = probe_cmd(&docker, &cwd, &args, Duration::from_secs(10)) else {
        return;
    };
    let Some(containers) = parse_inspect(&output) else {
        return;
    };
    for name in orphaned(&containers, roots, &|p| p.exists()) {
        crate::gc::delete_audit::record("gc.act-container", Path::new(&name), "act-container");
        match probe_cmd(
            &docker,
            &cwd,
            &["rm", "--force", &name],
            Duration::from_secs(30),
        ) {
            Some((0, _)) => {
                let env = format!("{name}-env");
                let _ = probe_cmd(
                    &docker,
                    &cwd,
                    &["volume", "rm", &name, &env],
                    Duration::from_secs(30),
                );
                eprintln!("[clud] gc: removed act container {name} (its worktree is gone)");
            }
            _ => eprintln!("[clud] gc: could not remove act container {name}"),
        }
    }
}

fn docker_path() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    let executable = std::env::current_exe().ok()?;
    let docker = crate::shim_registry::first_on_path("docker", &path, &executable, &[])?;
    docker.to_str().map(str::to_string)
}

#[cfg(test)]
#[path = "act_containers_tests.rs"]
mod tests;
