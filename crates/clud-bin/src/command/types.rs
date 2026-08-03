use serde::{Deserialize, Serialize};

use crate::backend::{Backend, HarnessSelection, LaunchMode, ModelProvider, PreferenceSource};
use crate::graphics::GraphicsConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchPlan {
    pub command: Vec<String>,
    pub iterations: u32,
    pub backend: Backend,
    /// Additive provider/harness metadata. `None` means a pre-#625 payload;
    /// consumers must fall back to the legacy `backend` field.
    #[serde(default)]
    pub model_provider: Option<ModelProvider>,
    #[serde(default)]
    pub requested_harness: Option<HarnessSelection>,
    #[serde(default)]
    pub effective_harness: Option<Backend>,
    #[serde(default)]
    pub provider_source: Option<PreferenceSource>,
    #[serde(default)]
    pub harness_source: Option<PreferenceSource>,
    pub launch_mode: LaunchMode,
    pub cwd: Option<String>,
    #[serde(default)]
    pub graphics: GraphicsConfig,
    #[serde(default)]
    pub repeat_schedule: Option<RepeatSchedule>,
    #[serde(default)]
    pub task_summary: Option<String>,
    /// When set, the outer loop should poll for DONE/BLOCKED marker files
    /// after each iteration and terminate accordingly.
    #[serde(default)]
    pub loop_markers: Option<LoopMarkers>,
    /// When set, claude is being invoked with `--output-format stream-json
    /// --verbose` and the subprocess runner should route its captured stdout
    /// through `stream_json::render_line` so the user sees live progress.
    #[serde(default)]
    pub stream_json_progress: bool,
    /// The Codex model+effort selection for a Codex-provider / Claude-harness
    /// launch, canonicalized (`gpt-5.6-terra@high`) from whatever short form
    /// the user gave. Carried on the plan rather than re-derived from argv so
    /// every launch path — subprocess, PTY, daemon, detach, repeat — hands the
    /// bridge the same value, and so `--dry-run` can show it.
    #[serde(default)]
    pub codex_model: Option<String>,
}

impl LaunchPlan {
    pub fn model_provider(&self) -> ModelProvider {
        self.model_provider
            .unwrap_or_else(|| self.backend.as_model_provider())
    }

    pub fn effective_harness(&self) -> Backend {
        self.effective_harness.unwrap_or(self.backend)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_with_metadata(
        provider: ModelProvider,
        requested: HarnessSelection,
        effective: Backend,
        provider_source: PreferenceSource,
        harness_source: PreferenceSource,
    ) -> LaunchPlan {
        LaunchPlan {
            command: vec![effective.executable_name().to_string()],
            iterations: 1,
            backend: effective,
            model_provider: Some(provider),
            requested_harness: Some(requested),
            effective_harness: Some(effective),
            provider_source: Some(provider_source),
            harness_source: Some(harness_source),
            launch_mode: LaunchMode::Subprocess,
            cwd: None,
            graphics: GraphicsConfig::default(),
            repeat_schedule: None,
            task_summary: None,
            loop_markers: None,
            stream_json_progress: false,
            codex_model: None,
        }
    }

    #[test]
    fn launch_plan_metadata_round_trips_for_default_cli_and_saved_routes() {
        let cases = [
            (
                ModelProvider::Claude,
                HarnessSelection::Default,
                Backend::Claude,
                PreferenceSource::BuiltInDefault,
                PreferenceSource::BuiltInDefault,
            ),
            (
                ModelProvider::Codex,
                HarnessSelection::Claude,
                Backend::Claude,
                PreferenceSource::Cli,
                PreferenceSource::Cli,
            ),
            (
                ModelProvider::Codex,
                HarnessSelection::Claude,
                Backend::Claude,
                PreferenceSource::GlobalSetting,
                PreferenceSource::GlobalSetting,
            ),
        ];

        for (provider, requested, effective, provider_source, harness_source) in cases {
            let plan = plan_with_metadata(
                provider,
                requested,
                effective,
                provider_source,
                harness_source,
            );
            let json = serde_json::to_value(&plan).unwrap();
            assert_eq!(json["model_provider"], provider.as_str());
            assert_eq!(json["requested_harness"], requested.as_str());
            assert_eq!(
                json["effective_harness"],
                serde_json::to_value(effective).unwrap()
            );
            assert_eq!(json["provider_source"], provider_source.as_str());
            assert_eq!(json["harness_source"], harness_source.as_str());

            let decoded: LaunchPlan = serde_json::from_value(json).unwrap();
            assert_eq!(decoded.model_provider, Some(provider));
            assert_eq!(decoded.requested_harness, Some(requested));
            assert_eq!(decoded.effective_harness, Some(effective));
            assert_eq!(decoded.provider_source, Some(provider_source));
            assert_eq!(decoded.harness_source, Some(harness_source));
        }
    }

    #[test]
    fn old_launch_plan_payload_defaults_new_metadata_to_legacy_backend() {
        let plan: LaunchPlan = serde_json::from_str(
            r#"{
                "command":["codex"],
                "iterations":1,
                "backend":"Codex",
                "launch_mode":"subprocess",
                "cwd":null,
                "stream_json_progress":false
            }"#,
        )
        .unwrap();
        assert_eq!(plan.model_provider, None);
        assert_eq!(plan.requested_harness, None);
        assert_eq!(plan.effective_harness, None);
        assert_eq!(plan.provider_source, None);
        assert_eq!(plan.harness_source, None);
        assert_eq!(plan.model_provider(), ModelProvider::Codex);
        assert_eq!(plan.effective_harness(), Backend::Codex);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopMarkers {
    pub done_path: String,
    pub blocked_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepeatSchedule {
    pub interval_secs: u64,
}
