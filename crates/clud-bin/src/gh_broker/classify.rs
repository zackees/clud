//! Which in-session `gh` invocations the read broker may answer (#1743).
//!
//! Phase 1 brokers exactly one shape: `gh api <endpoint>` as a plain GET
//! whose only other flags are output formatting (`--jq`, `--template`,
//! `--silent`), an explicit `GET` method, or `--hostname`. Every other
//! invocation, including an unknown flag, goes to the real `gh` unchanged.
//! A false "not a read" costs one ordinary `gh` call; a false "read" would
//! change behavior, so the parser rejects anything it does not recognize.

use std::ffi::OsString;

/// A brokerable `gh api` read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiRead {
    /// Index into the shim's argv (after argv\[0\]) of the endpoint word,
    /// which the shim swaps for the local replay URL.
    pub endpoint_index: usize,
    /// The endpoint exactly as the caller wrote it; the upstream fetch
    /// passes it to the real `gh` unchanged.
    pub endpoint: String,
    /// `--hostname`, when given.
    pub hostname: Option<String>,
}

impl ApiRead {
    /// The endpoint without the optional leading `/`, which `gh` ignores.
    pub fn normalized_endpoint(&self) -> &str {
        self.endpoint.trim_start_matches('/')
    }
}

/// Classify the shim's argv (after argv\[0\]). `Some` only for a read the
/// broker can answer with output identical to the real `gh`.
pub fn api_read(args: &[OsString]) -> Option<ApiRead> {
    let words: Vec<&str> = args.iter().map(|arg| arg.to_str()).collect::<Option<_>>()?;
    if words.first() != Some(&"api") {
        return None;
    }
    let mut endpoint: Option<(usize, &str)> = None;
    let mut hostname = None;
    let mut i = 1;
    while i < words.len() {
        let word = words[i];
        match word {
            "--silent" => {}
            "-q" | "--jq" | "-t" | "--template" => {
                words.get(i + 1)?;
                i += 1;
            }
            "-X" | "--method" => {
                if *words.get(i + 1)? != "GET" {
                    return None;
                }
                i += 1;
            }
            "--method=GET" | "-XGET" => {}
            "--hostname" => {
                hostname = Some(valid_hostname(words.get(i + 1)?)?);
                i += 1;
            }
            _ if word.starts_with("--hostname=") => {
                hostname = Some(valid_hostname(&word["--hostname=".len()..])?);
            }
            _ if word.starts_with("--jq=") || word.starts_with("--template=") => {}
            // pflag shorthand with an attached value: `-q.name`, `-t{{.x}}`.
            _ if (word.starts_with("-q") || word.starts_with("-t"))
                && word.len() > 2
                && !word.starts_with("--") => {}
            _ if word.starts_with('-') => return None,
            _ => {
                if endpoint.is_some() {
                    return None;
                }
                endpoint = Some((i, word));
            }
        }
        i += 1;
    }
    let (endpoint_index, endpoint) = endpoint?;
    if !brokerable_endpoint(endpoint) {
        return None;
    }
    Some(ApiRead {
        endpoint_index,
        endpoint: endpoint.to_string(),
        hostname: hostname.map(str::to_string),
    })
}

/// A REST path the broker can key without `gh`'s help: not GraphQL (a
/// POST), not an absolute URL, and free of `{owner}`-style placeholders,
/// which `gh` fills from the caller's checkout.
pub(crate) fn brokerable_endpoint(endpoint: &str) -> bool {
    let path = endpoint.trim_start_matches('/');
    !path.is_empty()
        && !endpoint.starts_with('-')
        && path != "graphql"
        && !path.starts_with("graphql?")
        && !endpoint.contains("://")
        && !endpoint.contains(['{', '}'])
        && !endpoint
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
}

pub(crate) fn valid_hostname(value: &str) -> Option<&str> {
    let ok = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'));
    ok.then_some(value)
}

/// Whether a non-read invocation may have changed GitHub state, so the
/// broker must revalidate before serving cached reads again. Deliberately
/// broad: a false positive costs one ETag revalidation (a `304`, which
/// GitHub does not charge against the rate limit), a false negative serves a
/// stale read for up to one TTL.
pub fn may_write(args: &[OsString]) -> bool {
    let words: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let positionals: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .filter(|word| !word.starts_with('-'))
        .collect();
    let Some(&top) = positionals.first() else {
        return false;
    };
    if top == "api" {
        return api_may_write(&words);
    }
    if matches!(
        top,
        "search"
            | "status"
            | "browse"
            | "help"
            | "version"
            | "completion"
            | "config"
            | "extension"
            | "alias"
    ) {
        return false;
    }
    let sub = positionals.get(1).copied().unwrap_or("");
    !matches!(
        sub,
        "view" | "list" | "checks" | "diff" | "status" | "watch" | "download"
    )
}

fn api_may_write(words: &[String]) -> bool {
    let mut i = 0;
    while i < words.len() {
        let word = words[i].as_str();
        match word {
            "-X" | "--method" => {
                if words.get(i + 1).is_some_and(|m| m != "GET") {
                    return true;
                }
                i += 1;
            }
            "-f" | "-F" | "--field" | "--raw-field" | "--input" => return true,
            _ if word.starts_with("--method=") && word != "--method=GET" => return true,
            _ if word.starts_with("-X") && word.len() > 2 && word != "-XGET" => return true,
            _ if word.starts_with("--field=")
                || word.starts_with("--raw-field=")
                || word.starts_with("--input=") =>
            {
                return true
            }
            _ if (word.starts_with("-f") || word.starts_with("-F")) && word.len() > 2 => {
                return true
            }
            _ => {}
        }
        i += 1;
    }
    words.iter().any(|w| w == "graphql")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    fn read(words: &[&str]) -> Option<ApiRead> {
        api_read(&args(words))
    }

    #[test]
    fn plain_get_is_a_read() {
        let r = read(&["api", "repos/o/r/actions/runs/1"]).unwrap();
        assert_eq!(r.endpoint_index, 1);
        assert_eq!(r.endpoint, "repos/o/r/actions/runs/1");
        assert_eq!(r.hostname, None);
        let r = read(&["api", "/repos/o/r?per_page=5"]).unwrap();
        assert_eq!(r.normalized_endpoint(), "repos/o/r?per_page=5");
    }

    #[test]
    fn formatting_flags_and_explicit_get_stay_reads() {
        for words in [
            &["api", "repos/o/r", "--jq", ".name"][..],
            &["api", "--jq", ".name", "repos/o/r"],
            &["api", "repos/o/r", "-q", ".name"],
            &["api", "repos/o/r", "-q.name"],
            &["api", "repos/o/r", "--jq=.name"],
            &["api", "repos/o/r", "-t", "{{.name}}"],
            &["api", "repos/o/r", "--template={{.name}}"],
            &["api", "repos/o/r", "--silent"],
            &["api", "-X", "GET", "repos/o/r"],
            &["api", "--method", "GET", "repos/o/r"],
            &["api", "--method=GET", "repos/o/r"],
            &["api", "-XGET", "repos/o/r"],
        ] {
            let r = read(words).unwrap_or_else(|| panic!("{words:?} is a read"));
            assert_eq!(r.endpoint, "repos/o/r", "{words:?}");
            assert_eq!(args(words)[r.endpoint_index], "repos/o/r");
        }
        let r = read(&["api", "--hostname", "ghe.example.com", "repos/o/r"]).unwrap();
        assert_eq!(r.hostname.as_deref(), Some("ghe.example.com"));
        assert_eq!(r.endpoint_index, 3);
    }

    #[test]
    fn non_get_methods_and_body_flags_pass_through() {
        for words in [
            &["api", "-X", "POST", "repos/o/r/issues"][..],
            &["api", "--method", "PATCH", "repos/o/r"],
            &["api", "--method=DELETE", "repos/o/r"],
            &["api", "-XPOST", "repos/o/r"],
            &["api", "-X", "get", "repos/o/r"],
            &["api", "repos/o/r", "-f", "title=x"],
            &["api", "repos/o/r", "-F", "n=1"],
            &["api", "repos/o/r", "--field", "n=1"],
            &["api", "repos/o/r", "--raw-field", "n=1"],
            &["api", "repos/o/r", "--input", "body.json"],
            &["api", "-X", "GET", "search/issues", "-f", "q=x"],
        ] {
            assert_eq!(read(words), None, "{words:?}");
        }
    }

    #[test]
    fn unsupported_flags_and_shapes_pass_through() {
        for words in [
            &["api", "repos/o/r", "--paginate"][..],
            &["api", "repos/o/r", "--slurp"],
            &["api", "repos/o/r", "-i"],
            &["api", "repos/o/r", "--include"],
            &["api", "repos/o/r", "--verbose"],
            &["api", "repos/o/r", "--cache", "1h"],
            &[
                "api",
                "repos/o/r",
                "-H",
                "Accept: application/vnd.github.raw",
            ],
            &["api", "repos/o/r", "-p", "corsair"],
            &["api", "repos/o/r", "--unknown-future-flag"],
            &["api", "repos/o/r", "--jq"],
            &["api", "repos/o/r", "extra"],
            &["api", "graphql", "-f", "query=x"],
            &["api", "graphql"],
            &["api", "repos/{owner}/{repo}/pulls"],
            &["api", "https://api.github.com/repos/o/r"],
            &["api", "--", "repos/o/r"],
            &["api"],
            &["pr", "view", "1"],
            &["run", "view", "1"],
            &[],
        ] {
            assert_eq!(read(words), None, "{words:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_args_pass_through() {
        use std::os::unix::ffi::OsStringExt;
        let args = vec![
            OsString::from("api"),
            OsString::from_vec(b"repos/\xff".to_vec()),
        ];
        assert_eq!(api_read(&args), None);
    }

    #[test]
    fn writes_are_detected_and_reads_are_not() {
        for words in [
            &["api", "-X", "POST", "repos/o/r/issues"][..],
            &["api", "repos/o/r/issues", "-f", "title=x"],
            &["api", "--method=PATCH", "repos/o/r"],
            &["api", "graphql", "-f", "query=mutation{}"],
            &["pr", "merge", "1"],
            &["pr", "comment", "1", "--body", "x"],
            &["run", "rerun", "5"],
            &["issue", "close", "3"],
            &["workflow", "run", "ci.yml"],
            &["auth", "switch"],
            &["auth", "login"],
            &["auth", "logout"],
            &["--repo", "o/r", "pr", "edit", "2"],
        ] {
            assert!(may_write(&args(words)), "{words:?}");
        }
        for words in [
            &["api", "repos/o/r"][..],
            &["api", "repos/o/r", "--paginate"],
            &["pr", "view", "1", "--json", "state"],
            &["pr", "checks", "1"],
            &["run", "view", "5", "--log"],
            &["run", "list"],
            &["auth", "status"],
            &["search", "issues", "x"],
            &[],
        ] {
            assert!(!may_write(&args(words)), "{words:?}");
        }
    }
}
