//! Compatibility checks for clients and daemons from different releases.

use std::cmp::Ordering;
use std::io;

pub(super) fn is_old_daemon_signature(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedVersion {
    core: [u64; 3],
    prerelease: Option<Vec<String>>,
}

fn parse_version(version: &str) -> Option<ParsedVersion> {
    let without_build = version.split_once('+').map_or(version, |(head, _)| head);
    let (core, prerelease) = match without_build.split_once('-') {
        Some((core, prerelease)) if !prerelease.is_empty() => (
            core,
            Some(prerelease.split('.').map(str::to_owned).collect()),
        ),
        Some(_) => return None,
        None => (without_build, None),
    };
    let mut components = core.split('.');
    let parsed = [
        components.next()?.parse().ok()?,
        components.next()?.parse().ok()?,
        components.next()?.parse().ok()?,
    ];
    if components.next().is_some() {
        return None;
    }
    Some(ParsedVersion {
        core: parsed,
        prerelease,
    })
}

fn compare_prerelease(left: &Option<Vec<String>>, right: &Option<Vec<String>>) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => {
            for (left, right) in left.iter().zip(right) {
                let order = match (left.parse::<u64>(), right.parse::<u64>()) {
                    (Ok(left), Ok(right)) => left.cmp(&right),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => left.cmp(right),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
            left.len().cmp(&right.len())
        }
    }
}

/// Compare two SemVer release strings without adding a dependency to the
/// daemon's startup path. Build metadata is deliberately ignored.
pub(super) fn compare_versions(left: &str, right: &str) -> Option<Ordering> {
    let left = parse_version(left)?;
    let right = parse_version(right)?;
    Some(
        left.core
            .cmp(&right.core)
            .then_with(|| compare_prerelease(&left.prerelease, &right.prerelease)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_release_and_prerelease_versions() {
        assert_eq!(compare_versions("2.7.0", "2.7.1"), Some(Ordering::Less));
        assert_eq!(compare_versions("2.7.1", "2.7.1"), Some(Ordering::Equal));
        assert_eq!(compare_versions("2.8.0", "2.7.1"), Some(Ordering::Greater));
        assert_eq!(
            compare_versions("2.8.0-rc.2", "2.8.0-rc.10"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("2.8.0", "2.8.0-rc.10"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_versions("2.8.0+local", "2.8.0+release"),
            Some(Ordering::Equal)
        );
        assert_eq!(compare_versions("not-semver", "2.8.0"), None);
    }
}
