use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::PathBuf;

use super::utils::display_path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HookFrontend {
    Claude,
    Codex,
}

impl HookFrontend {
    pub(in crate::hook_health) fn display_name(self) -> &'static str {
        match self {
            HookFrontend::Claude => "Claude",
            HookFrontend::Codex => "Codex",
        }
    }

    pub(in crate::hook_health) fn config_name(self) -> &'static str {
        match self {
            HookFrontend::Claude => "Claude Code",
            HookFrontend::Codex => "Codex",
        }
    }

    pub(in crate::hook_health) fn lower(self) -> &'static str {
        match self {
            HookFrontend::Claude => "claude",
            HookFrontend::Codex => "codex",
        }
    }
}

impl fmt::Display for HookFrontend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.lower())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedHook {
    pub frontend: HookFrontend,
    pub matcher: String,
    pub source_path: PathBuf,
    pub group_index: usize,
    pub handler_index: usize,
    pub command: Option<String>,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct FrontendHookSummary {
    pub frontend: HookFrontend,
    pub hooks: Vec<NormalizedHook>,
    pub warnings: Vec<String>,
    pub parse_errors: Vec<HookConfigError>,
}

impl FrontendHookSummary {
    pub(in crate::hook_health) fn new(frontend: HookFrontend) -> Self {
        Self {
            frontend,
            hooks: Vec::new(),
            warnings: Vec::new(),
            parse_errors: Vec::new(),
        }
    }

    pub(in crate::hook_health) fn active_hooks(&self) -> Vec<&NormalizedHook> {
        self.hooks.iter().filter(|hook| hook.active).collect()
    }

    #[cfg(test)]
    pub(in crate::hook_health) fn all_matchers(&self) -> BTreeSet<String> {
        self.hooks
            .iter()
            .map(|hook| hook.matcher.clone())
            .collect::<BTreeSet<_>>()
    }

    pub(in crate::hook_health) fn active_matchers(&self) -> BTreeSet<String> {
        self.hooks
            .iter()
            .filter(|hook| hook.active)
            .map(|hook| hook.matcher.clone())
            .collect::<BTreeSet<_>>()
    }

    pub(in crate::hook_health) fn hook_by_matcher(&self) -> BTreeMap<String, NormalizedHook> {
        let mut hooks = BTreeMap::new();
        for hook in &self.hooks {
            hooks
                .entry(hook.matcher.clone())
                .or_insert_with(|| hook.clone());
        }
        hooks
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookConfigError {
    pub frontend: HookFrontend,
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Clone, Default)]
pub struct CodexProjectTrust {
    pub config_path: PathBuf,
    pub canonical_key: String,
    pub canonical_present: bool,
    pub canonical_trusted: bool,
    pub extended_trusted: bool,
    pub parse_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HookHealthReport {
    pub repo_root: PathBuf,
    pub home: Option<PathBuf>,
    pub claude: FrontendHookSummary,
    pub codex: FrontendHookSummary,
    pub codex_project_trust: Option<CodexProjectTrust>,
    pub codex_legacy_hook_feature_configs: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairAction {
    AddCodexProjectTrust {
        config_path: PathBuf,
        project_key: String,
    },
    MigrateCodexHooksFeatureFlag {
        config_path: PathBuf,
    },
    BackendPrompt {
        source: HookFrontend,
        target: HookFrontend,
        matcher: String,
        source_path: PathBuf,
        prompt: String,
    },
    ValidationPrompt {
        frontend: HookFrontend,
        config_path: PathBuf,
        prompt: String,
    },
}

#[derive(Debug)]
pub struct DeterministicRepairError {
    pub(in crate::hook_health) path: PathBuf,
    pub(in crate::hook_health) error: io::Error,
}

impl fmt::Display for DeterministicRepairError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", display_path(&self.path), self.error)
    }
}

impl std::error::Error for DeterministicRepairError {}
