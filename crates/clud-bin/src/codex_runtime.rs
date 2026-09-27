//! Published Codex model choices, checked against the local Codex account.

pub const FALLBACK_SOL: &str = "gpt-6-sol";
pub const FALLBACK_LUNA: &str = "gpt-6-luna";
pub const MANIFEST_URL: &str = "https://zackees.github.io/clud/models/manifest.json";
const CACHE_PATH: &str = ".clud/cache/models/manifest.json";
const CACHE_INTERVAL: Duration = Duration::from_secs(15 * 60);

use running_process::{
    CommandSpec, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode, StreamKind,
};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

fn cache_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(CACHE_PATH))
}

fn read_cached(path: &Path) -> Option<Manifest> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    Manifest::parse(&bytes).ok()
}

fn cache_fresh(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age < CACHE_INTERVAL)
}

fn attempt_path(path: &Path) -> PathBuf {
    path.with_extension("json.last-attempt")
}

fn record_attempt(path: &Path) {
    let stamp = attempt_path(path);
    if let Some(parent) = stamp.parent() {
        if std::fs::create_dir_all(parent).is_ok() {
            let _ = std::fs::write(stamp, b"");
        }
    }
}

fn fetch_manifest(url: &str) -> Result<Manifest, String> {
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(900))
        .build()
        .get(url)
        .call()
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    Manifest::parse(&bytes)
}

fn write_cached(path: &Path, manifest: &Manifest) -> Result<(), String> {
    let parent = path.parent().ok_or("model cache has no parent directory")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.partial");
    let bytes = serde_json::to_vec(manifest).map_err(|error| error.to_string())?;
    std::fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    std::fs::rename(temporary, path).map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Families {
    pub sol: String,
    pub luna: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CodexDefault {
    model: String,
    effort: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Defaults {
    codex: CodexDefault,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
    schema_version: u64,
    source: String,
    checked_at: String,
    trigger: String,
    families: Families,
    defaults: Defaults,
}

fn stable_id(id: &str, family: &str) -> bool {
    let Some(version) = id
        .strip_prefix("gpt-")
        .and_then(|rest| rest.strip_suffix(&format!("-{family}")))
    else {
        return false;
    };
    let mut parts = version.split('.');
    let major = parts.next().unwrap_or_default();
    let minor = parts.next();
    !major.is_empty()
        && !major.starts_with('0')
        && major.bytes().all(|byte| byte.is_ascii_digit())
        && minor
            .is_none_or(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && parts.next().is_none()
}

fn valid_checked_at(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return false;
    }
    let number = |start: usize, end: usize| -> Option<u32> {
        let part = bytes.get(start..end)?;
        part.iter().all(u8::is_ascii_digit).then(|| {
            part.iter()
                .fold(0_u32, |value, digit| value * 10 + u32::from(digit - b'0'))
        })
    };
    let (Some(year), Some(month), Some(day)) = (number(0, 4), number(5, 7), number(8, 10)) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return false,
    };
    year > 0
        && (1..=days).contains(&day)
        && matches!(number(11, 13), Some(0..=23))
        && matches!(number(14, 16), Some(0..=59))
        && matches!(number(17, 19), Some(0..=59))
}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value = crate::server_settings::parse_strict_json(bytes, 64 * 1024)?;
        let document: Self = serde_json::from_value(value).map_err(|error| error.to_string())?;
        if document.schema_version != 1 {
            return Err("unsupported model manifest schema".to_string());
        }
        if document.source != "openrouter+models.dev-merge"
            && document.source != "openai-models+openrouter-cross-check"
        {
            return Err("untrusted model manifest source".to_string());
        }
        if !matches!(document.trigger.as_str(), "manual" | "nightly") {
            return Err("invalid model manifest trigger".to_string());
        }
        if !valid_checked_at(&document.checked_at) {
            return Err("invalid model manifest timestamp".to_string());
        }
        if !stable_id(&document.families.sol, "sol") || !stable_id(&document.families.luna, "luna")
        {
            return Err("invalid stable Sol or Luna model ID".to_string());
        }
        if document.defaults.codex.model != document.families.sol
            || document.defaults.codex.effort != "low"
        {
            return Err("manifest Codex default must be Sol/low".to_string());
        }
        Ok(document)
    }

    pub fn families(&self) -> &Families {
        &self.families
    }
}

#[derive(Debug, Clone, Deserialize)]
struct EffortOption {
    #[serde(rename = "reasoningEffort")]
    reasoning_effort: String,
}

#[derive(Debug, Clone, Deserialize)]
struct AvailableModel {
    id: String,
    #[serde(default)]
    hidden: bool,
    #[serde(rename = "supportedReasoningEfforts")]
    supported_reasoning_efforts: Vec<EffortOption>,
}

#[derive(Debug, Clone, Deserialize)]
struct ModelList {
    data: Vec<AvailableModel>,
}

pub fn usable_low_effort_ids(result: &serde_json::Value) -> Result<Vec<String>, String> {
    let list: ModelList = serde_json::from_value(result.clone()).map_err(|e| e.to_string())?;
    Ok(list
        .data
        .into_iter()
        .filter(|model| {
            !model.hidden
                && model
                    .supported_reasoning_efforts
                    .iter()
                    .any(|effort| effort.reasoning_effort == "low")
        })
        .map(|model| model.id)
        .collect())
}

/// Ask the installed Codex App Server which models this account can use at low effort.
pub fn local_low_effort_models() -> Result<Vec<String>, String> {
    let process = NativeProcess::new(ProcessConfig {
        command: CommandSpec::Argv(vec!["codex".into(), "app-server".into()]),
        cwd: None,
        env: None,
        capture: true,
        stderr_mode: StderrMode::Pipe,
        creationflags: crate::win_creation_flags::invisible_helper_creationflags(),
        create_process_group: false,
        stdin_mode: StdinMode::Piped,
        nice: None,
        address_space_limit_bytes: None,
    });
    process.start().map_err(|error| error.to_string())?;
    let messages = concat!(
        "{\"method\":\"initialize\",\"id\":0,\"params\":{\"clientInfo\":{\"name\":\"clud\",\"title\":\"clud\",\"version\":\"1\"}}}\n",
        "{\"method\":\"initialized\",\"params\":{}}\n",
        "{\"method\":\"model/list\",\"id\":1,\"params\":{\"limit\":100,\"includeHidden\":true}}\n",
    );
    let result = (|| {
        process
            .write_stdin_streaming(messages.as_bytes())
            .map_err(|error| error.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if Instant::now() >= deadline {
                return Err("Codex model/list timed out".to_string());
            }
            match process.read_stream(StreamKind::Stdout, Some(Duration::from_millis(50))) {
                ReadStatus::Line(bytes) => {
                    if bytes.len() > 128 * 1024 {
                        return Err("Codex model/list response is too large".to_string());
                    }
                    let Ok(frame) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                        continue;
                    };
                    if frame.get("id").and_then(serde_json::Value::as_i64) == Some(1) {
                        return match frame.get("result") {
                            Some(value) => usable_low_effort_ids(value),
                            None => Err(format!("Codex model/list failed: {}", frame["error"])),
                        };
                    }
                }
                ReadStatus::Eof => {
                    return Err("Codex app-server closed before model/list".to_string())
                }
                ReadStatus::Timeout => {}
            }
        }
    })();
    let _ = process.kill();
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Published,
    LastGood,
    BuiltIn,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Published => "published",
            Self::LastGood => "last_good",
            Self::BuiltIn => "built_in",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub sol: String,
    pub luna: String,
    pub source: Source,
}

pub fn choose(
    published: Option<&Manifest>,
    last_good: Option<&Manifest>,
    available: &[String],
) -> Choice {
    for (candidate, source) in [
        (published, Source::Published),
        (last_good, Source::LastGood),
    ] {
        if let Some(manifest) = candidate {
            let families = manifest.families();
            if available.contains(&families.sol) && available.contains(&families.luna) {
                return Choice {
                    sol: families.sol.clone(),
                    luna: families.luna.clone(),
                    source,
                };
            }
        }
    }
    Choice {
        sol: FALLBACK_SOL.to_string(),
        luna: FALLBACK_LUNA.to_string(),
        source: Source::BuiltIn,
    }
}

/// The per-process effective dynamic Codex choice. Explicit pins never use it.
pub fn active_choice() -> &'static Choice {
    static CHOICE: OnceLock<Choice> = OnceLock::new();
    CHOICE.get_or_init(load_choice)
}

/// Resolve a stable catalog row to its current account-checked wire ID.
pub fn wire_id(cli_id: &str, built_in: &str) -> String {
    if matches!(cli_id, "codex-sol" | "codex-luna") {
        wire_id_for_choice(cli_id, built_in, active_choice())
    } else {
        built_in.to_string()
    }
}

fn wire_id_for_choice(cli_id: &str, built_in: &str, choice: &Choice) -> String {
    match cli_id {
        "codex-sol" => choice.sol.clone(),
        "codex-luna" => choice.luna.clone(),
        _ => built_in.to_string(),
    }
}

fn load_choice() -> Choice {
    if cfg!(test) {
        return choose(None, None, &[]);
    }
    let url_override = std::env::var("CLUD_MODEL_MANIFEST_URL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let path = url_override.is_none().then(cache_path).flatten();
    let cached = path.as_deref().and_then(read_cached);
    let fresh = cached.is_some() && path.as_deref().is_some_and(cache_fresh);
    let recently_attempted = match path.as_deref() {
        Some(candidate) => cache_fresh(&attempt_path(candidate)),
        None => false,
    };
    let served = if fresh || recently_attempted {
        cached.clone()
    } else {
        if let Some(path) = path.as_deref() {
            record_attempt(path);
        }
        let url = url_override.unwrap_or_else(|| MANIFEST_URL.to_string());
        match fetch_manifest(&url) {
            Ok(manifest) => Some(manifest),
            Err(error) => {
                eprintln!("[clud] model manifest unavailable ({error}); checking cached models");
                None
            }
        }
    };
    let available = match local_low_effort_models() {
        Ok(models) => models,
        Err(error) => {
            eprintln!("[clud] Codex model/list unavailable ({error}); using built-in Sol/low");
            Vec::new()
        }
    };
    let choice = choose(served.as_ref(), cached.as_ref(), &available);
    if choice.source == Source::Published && !fresh && !recently_attempted {
        if let (Some(path), Some(manifest)) = (path.as_deref(), served.as_ref()) {
            if let Err(error) = write_cached(path, manifest) {
                eprintln!("[clud] could not cache model manifest: {error}");
            }
        }
    } else {
        eprintln!(
            "[clud] Codex model fallback: {:?} (Sol {}, low)",
            choice.source, choice.sol
        );
    }
    choice
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest(sol: &str, luna: &str) -> Manifest {
        let document = json!({
            "schema_version": 1,
            "source": "openrouter+models.dev-merge",
            "checked_at": "2026-09-26T21:00:00Z",
            "trigger": "manual",
            "families": {"sol": sol, "luna": luna},
            "defaults": {"codex": {"model": sol, "effort": "low"}}
        });
        Manifest::parse(document.to_string().as_bytes()).unwrap()
    }

    #[test]
    fn malformed_preview_id_is_rejected() {
        let mut value = serde_json::to_value(manifest("gpt-6-sol", "gpt-6-luna")).unwrap();
        value["families"]["sol"] = json!("gpt-7-sol-preview");
        assert!(Manifest::parse(value.to_string().as_bytes()).is_err());
    }

    #[test]
    fn malformed_timestamp_is_rejected() {
        let mut value = serde_json::to_value(manifest("gpt-6-sol", "gpt-6-luna")).unwrap();
        value["checked_at"] = json!("not-a-real-timestampZ");
        assert!(Manifest::parse(value.to_string().as_bytes()).is_err());
        value["checked_at"] = json!("2026-19-26T21:00:00Z");
        assert!(Manifest::parse(value.to_string().as_bytes()).is_err());
    }

    #[test]
    fn legacy_published_source_remains_usable_during_migration() {
        let mut value = serde_json::to_value(manifest("gpt-6-sol", "gpt-6-luna")).unwrap();
        value["source"] = json!("openai-models+openrouter-cross-check");
        assert!(Manifest::parse(value.to_string().as_bytes()).is_ok());
    }

    #[test]
    fn future_additive_fields_do_not_break_a_valid_document() {
        let mut value = serde_json::to_value(manifest("gpt-7-sol", "gpt-7-luna")).unwrap();
        value["future"] = json!({"publisher": "example"});
        value["families"]["future"] = json!("gpt-7-terra");
        value["defaults"]["codex"]["future"] = json!(true);
        assert!(Manifest::parse(value.to_string().as_bytes()).is_ok());
    }

    #[test]
    fn fake_model_list_filters_hidden_and_unsupported_efforts() {
        let result = json!({"data": [
            {"id": "gpt-6-sol", "supportedReasoningEfforts": [{"reasoningEffort": "low"}]},
            {"id": "gpt-7-sol", "supportedReasoningEfforts": [{"reasoningEffort": "high"}]},
            {"id": "gpt-6-luna", "hidden": true, "supportedReasoningEfforts": [{"reasoningEffort": "low"}]}
        ]});
        assert_eq!(usable_low_effort_ids(&result).unwrap(), ["gpt-6-sol"]);
    }

    #[test]
    fn published_then_last_good_then_builtin() {
        let latest = manifest("gpt-7-sol", "gpt-7-luna");
        let previous = manifest("gpt-6-sol", "gpt-6-luna");
        let all = ["gpt-7-sol", "gpt-7-luna", "gpt-6-sol", "gpt-6-luna"].map(str::to_string);
        assert_eq!(
            choose(Some(&latest), Some(&previous), &all).source,
            Source::Published
        );
        assert_eq!(
            choose(Some(&latest), Some(&previous), &all[2..]).source,
            Source::LastGood
        );
        let fallback = choose(Some(&latest), Some(&previous), &[]);
        assert_eq!(fallback.source, Source::BuiltIn);
        assert_eq!(fallback.sol, "gpt-6-sol");
    }

    #[test]
    fn stable_discovery_rows_map_to_future_wire_ids() {
        let future = manifest("gpt-7-sol", "gpt-7-luna");
        let available = ["gpt-7-sol".to_string(), "gpt-7-luna".to_string()];
        let choice = choose(Some(&future), None, &available);
        assert_eq!(
            wire_id_for_choice("codex-sol", FALLBACK_SOL, &choice),
            "gpt-7-sol"
        );
        assert_eq!(
            wire_id_for_choice("codex-luna", FALLBACK_LUNA, &choice),
            "gpt-7-luna"
        );
        assert_eq!(
            wire_id_for_choice("codex-terra", "gpt-5.6-terra", &choice),
            "gpt-5.6-terra"
        );
    }
}
