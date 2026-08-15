use super::*;
use tempfile::tempdir;

#[test]
fn missing_settings_file_has_no_launch_setup_scope() {
    let home = tempdir().unwrap();
    assert_eq!(
        load_launch_setup_scope_at(home.path(), Backend::Codex).unwrap(),
        None
    );
}

#[test]
fn missing_settings_file_defaults_auto_fix_hooks_enabled() {
    let home = tempdir().unwrap();
    assert!(load_auto_fix_hooks_enabled_at(home.path()).unwrap());
}

#[test]
fn missing_settings_file_defaults_pr_wait_fail_fast_disabled() {
    let home = tempdir().unwrap();
    assert!(!load_pr_wait_fail_fast_enabled_at(home.path()).unwrap());
}

#[test]
fn saves_pr_wait_fail_fast_sticky_opt_in_and_reset() {
    let home = tempdir().unwrap();

    save_pr_wait_fail_fast_enabled_at(home.path(), true).unwrap();
    assert!(load_pr_wait_fail_fast_enabled_at(home.path()).unwrap());

    save_pr_wait_fail_fast_enabled_at(home.path(), false).unwrap();
    assert!(!load_pr_wait_fail_fast_enabled_at(home.path()).unwrap());

    let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["git"]["pr_wait_fail_fast"], false);
    assert!(json["git"]["pr_wait_fail_fast_note"]
        .as_str()
        .unwrap()
        .contains("gh pr checks --watch"));
}

#[test]
fn pr_wait_fail_fast_setting_preserves_existing_settings() {
    let home = tempdir().unwrap();
    let path = settings_path_at(home.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{"unrelated":{"value":"kept"},"launch_setup":{"codex":{"scope":"global"}}}"#,
    )
    .unwrap();

    save_pr_wait_fail_fast_enabled_at(home.path(), true).unwrap();

    let text = fs::read_to_string(path).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["unrelated"]["value"], "kept");
    assert_eq!(json["launch_setup"]["codex"]["scope"], "global");
    assert_eq!(json["git"]["pr_wait_fail_fast"], true);
}

#[test]
fn missing_codex_overrides_default_without_writing_on_dry_run() {
    let home = tempdir().unwrap();

    assert_eq!(
        load_or_init_codex_config_overrides_at(home.path(), false).unwrap(),
        default_codex_config_overrides()
    );
    assert!(!settings_path_at(home.path()).exists());
}

#[test]
fn missing_settings_file_has_no_default_backend() {
    let home = tempdir().unwrap();
    assert_eq!(load_default_backend_at(home.path()).unwrap(), None);
    assert_eq!(
        load_global_launch_preferences_at(home.path()).unwrap(),
        GlobalLaunchPreferences::default()
    );
}

#[test]
fn saves_default_backend() {
    let home = tempdir().unwrap();

    save_default_backend_at(home.path(), Backend::Codex).unwrap();

    assert_eq!(
        load_default_backend_at(home.path()).unwrap(),
        Some(Backend::Codex)
    );
    let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["backend"]["default"], "codex");
}

#[test]
fn infers_default_backend_from_sole_global_launch_setup_scope() {
    let home = tempdir().unwrap();

    save_launch_setup_scope_at(home.path(), Backend::Codex, LaunchSetupScope::Global).unwrap();

    assert_eq!(
        load_default_backend_at(home.path()).unwrap(),
        Some(Backend::Codex)
    );
}

#[test]
fn does_not_infer_default_backend_when_multiple_scopes_are_global() {
    let home = tempdir().unwrap();

    save_launch_setup_scope_at(home.path(), Backend::Claude, LaunchSetupScope::Global).unwrap();
    save_launch_setup_scope_at(home.path(), Backend::Codex, LaunchSetupScope::Global).unwrap();

    assert_eq!(load_default_backend_at(home.path()).unwrap(), None);
}

#[test]
fn default_backend_preserves_existing_settings() {
    let home = tempdir().unwrap();
    let path = settings_path_at(home.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{"unrelated":{"value":"kept"},"launch_setup":{"codex":{"scope":"global"}}}"#,
    )
    .unwrap();

    save_default_backend_at(home.path(), Backend::Claude).unwrap();

    let text = fs::read_to_string(path).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["unrelated"]["value"], "kept");
    assert_eq!(json["launch_setup"]["codex"]["scope"], "global");
    assert_eq!(json["backend"]["default"], "claude");
}

#[test]
fn saves_global_launch_setup_selection_in_one_document() {
    let home = tempdir().unwrap();

    save_global_launch_setup_selection_at(home.path(), Backend::Codex, LaunchSetupScope::Global)
        .unwrap();

    assert_eq!(
        load_default_backend_at(home.path()).unwrap(),
        Some(Backend::Codex)
    );
    assert_eq!(
        load_launch_setup_scope_at(home.path(), Backend::Codex).unwrap(),
        Some(LaunchSetupScope::Global)
    );
    let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["backend"]["default"], "codex");
    assert_eq!(json["launch_setup"]["codex"]["scope"], "global");
}

#[test]
fn round_trips_harness_preference_and_reads_old_settings() {
    let home = tempdir().unwrap();
    let path = settings_path_at(home.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, r#"{"backend":{"default":"codex"}}"#).unwrap();

    assert_eq!(
        load_global_launch_preferences_at(home.path()).unwrap(),
        GlobalLaunchPreferences {
            model_provider: Some(ModelProvider::Codex),
            harness: None,
        }
    );

    save_settings_patch_at(
        home.path(),
        GlobalSettingsPatch {
            harness: Some(HarnessSelection::Claude),
            ..GlobalSettingsPatch::default()
        },
    )
    .unwrap();
    assert_eq!(
        load_global_launch_preferences_at(home.path()).unwrap(),
        GlobalLaunchPreferences {
            model_provider: Some(ModelProvider::Codex),
            harness: Some(HarnessSelection::Claude),
        }
    );
}

#[test]
fn deepseek_model_provider_round_trips_through_settings() {
    let home = tempdir().unwrap();

    save_settings_patch_at(
        home.path(),
        GlobalSettingsPatch {
            model_provider: Some(ModelProvider::DeepSeek),
            ..GlobalSettingsPatch::default()
        },
    )
    .unwrap();

    assert_eq!(
        load_global_launch_preferences_at(home.path())
            .unwrap()
            .model_provider,
        Some(ModelProvider::DeepSeek)
    );
    let json: Value =
        serde_json::from_str(&fs::read_to_string(settings_path_at(home.path())).unwrap()).unwrap();
    assert_eq!(json["backend"]["default"], "deepseek");
}

/// Kimi twin of `deepseek_model_provider_round_trips_through_settings`
/// (#937 Phase 3).
#[test]
fn kimi_model_provider_round_trips_through_settings() {
    let home = tempdir().unwrap();

    save_settings_patch_at(
        home.path(),
        GlobalSettingsPatch {
            model_provider: Some(ModelProvider::Kimi),
            ..GlobalSettingsPatch::default()
        },
    )
    .unwrap();

    assert_eq!(
        load_global_launch_preferences_at(home.path())
            .unwrap()
            .model_provider,
        Some(ModelProvider::Kimi)
    );
    let json: Value =
        serde_json::from_str(&fs::read_to_string(settings_path_at(home.path())).unwrap()).unwrap();
    assert_eq!(json["backend"]["default"], "kimi");
}

#[test]
fn one_atomic_patch_updates_all_typed_settings_and_preserves_unknown_fields() {
    let home = tempdir().unwrap();
    let path = settings_path_at(home.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, r#"{"unrelated":{"kept":true}}"#).unwrap();

    save_settings_patch_at(
        home.path(),
        GlobalSettingsPatch {
            model_provider: Some(ModelProvider::Codex),
            harness: Some(HarnessSelection::Claude),
            pr_wait_fail_fast: Some(true),
            provider_profiles: Vec::new(),
        },
    )
    .unwrap();

    let json: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(json["backend"]["default"], "codex");
    assert_eq!(json["harness"]["default"], "claude");
    assert_eq!(json["git"]["pr_wait_fail_fast"], true);
    assert_eq!(json["unrelated"]["kept"], true);
}

#[test]
fn partial_settings_patch_preserves_omission_and_latest_launch_preferences() {
    let home = tempdir().unwrap();
    let path = settings_path_at(home.path());

    save_settings_patch_at(
        home.path(),
        GlobalSettingsPatch {
            pr_wait_fail_fast: Some(true),
            ..GlobalSettingsPatch::default()
        },
    )
    .unwrap();
    let json: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!(json.get("backend").is_none());
    assert!(json.get("harness").is_none());

    save_settings_patch_at(
        home.path(),
        GlobalSettingsPatch {
            model_provider: Some(ModelProvider::Codex),
            harness: Some(HarnessSelection::Default),
            ..GlobalSettingsPatch::default()
        },
    )
    .unwrap();
    save_settings_patch_at(
        home.path(),
        GlobalSettingsPatch {
            harness: Some(HarnessSelection::Claude),
            ..GlobalSettingsPatch::default()
        },
    )
    .unwrap();

    // This represents a TUI save whose snapshot predates the harness
    // change. Because unchanged rows are omitted, the latest launch
    // preferences survive the atomic patch.
    save_settings_patch_at(
        home.path(),
        GlobalSettingsPatch {
            pr_wait_fail_fast: Some(false),
            ..GlobalSettingsPatch::default()
        },
    )
    .unwrap();
    assert_eq!(
        load_global_launch_preferences_at(home.path()).unwrap(),
        GlobalLaunchPreferences {
            model_provider: Some(ModelProvider::Codex),
            harness: Some(HarnessSelection::Claude),
        }
    );
}

#[test]
fn atomic_settings_patch_validates_the_merged_launch_preferences() {
    let home = tempdir().unwrap();
    let error = save_settings_patch_at(
        home.path(),
        GlobalSettingsPatch {
            harness: Some(HarnessSelection::Codex),
            ..GlobalSettingsPatch::default()
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        SettingsError::InvalidLaunchTarget(LaunchTargetError::ClaudeViaCodexUnsupported)
    ));

    let path = settings_path_at(home.path());
    assert!(
        !path.exists(),
        "an invalid merged preference must not be written"
    );
}

#[test]
fn global_prompt_save_rejects_a_concurrently_invalidated_partial_choice() {
    let home = tempdir().unwrap();
    save_settings_patch_at(
        home.path(),
        GlobalSettingsPatch {
            model_provider: Some(ModelProvider::Codex),
            harness: Some(HarnessSelection::Default),
            ..GlobalSettingsPatch::default()
        },
    )
    .unwrap();

    // The interactive launch resolved while Codex was the provider, but
    // another process changes the provider before the user confirms the
    // global harness choice.
    save_settings_patch_at(
        home.path(),
        GlobalSettingsPatch {
            model_provider: Some(ModelProvider::Claude),
            ..GlobalSettingsPatch::default()
        },
    )
    .unwrap();
    let path = settings_path_at(home.path());
    let before = fs::read_to_string(&path).unwrap();

    let error = save_global_launch_preferences_at(
        home.path(),
        None,
        Some(HarnessSelection::Codex),
        LaunchSetupScope::Global,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        SettingsError::InvalidLaunchTarget(LaunchTargetError::ClaudeViaCodexUnsupported)
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
    assert_eq!(
        load_global_launch_preferences_at(home.path()).unwrap(),
        GlobalLaunchPreferences {
            model_provider: Some(ModelProvider::Claude),
            harness: Some(HarnessSelection::Default),
        }
    );
    assert_eq!(
        load_launch_setup_scope_at(home.path(), Backend::Codex).unwrap(),
        None
    );
}

#[test]
fn first_run_codex_overrides_are_documented_in_settings_json() {
    let home = tempdir().unwrap();

    assert_eq!(
        load_or_init_codex_config_overrides_at(home.path(), true).unwrap(),
        default_codex_config_overrides()
    );

    let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        json["codex"]["config_overrides"][0],
        DEFAULT_CODEX_GITHUB_PLUGIN_CONFIG_OVERRIDE
    );
    assert!(
        json["codex"]["config_overrides_note"]
            .as_str()
            .unwrap()
            .contains("codex -c"),
        "{text}"
    );
}

#[test]
fn first_run_global_settings_seed_discoverable_defaults_only() {
    let home = tempdir().unwrap();

    let document = load_or_init_global_settings_at(home.path()).unwrap();

    assert_eq!(document["shell"]["disable_powershell"], false);
    assert_eq!(document["hook_health"]["auto_fix_hooks"], true);
    assert_eq!(document["git"]["pr_wait_fail_fast"], false);
    assert_eq!(document["daemon"]["idle_timeout_secs"], 900);
    assert_eq!(document["daemon"]["proc_sampler"]["interval_ms"], 2_000);
    // #465 AC 1: both orphan-sweep knobs are seeded so an operator can
    // discover them by reading the file, rather than by reading the source.
    assert_eq!(document["daemon"]["orphan_sweep"]["interval_ms"], 60_000);
    assert_eq!(document["daemon"]["orphan_sweep"]["grace_ms"], 10_000);
    assert_eq!(
        document["codex"]["config_overrides"][0],
        DEFAULT_CODEX_GITHUB_PLUGIN_CONFIG_OVERRIDE
    );
    assert!(document.get("launch_setup").is_none());
    assert!(document.get("optimize").is_none());

    let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
    assert!(text.contains("\"shell\""));
    assert!(text.contains("\"hook_health\""));
    assert!(text.contains("\"git\""));
    assert!(text.contains("\"daemon\""));
    assert!(text.contains("\"codex\""));
}

#[test]
fn global_settings_seed_preserves_null_opinions() {
    let home = tempdir().unwrap();
    let path = settings_path_at(home.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
            &path,
            r#"{"shell":{"disable_powershell":null},"hook_health":null,"codex":{"config_overrides":null}}"#,
        )
        .unwrap();

    let document = load_or_init_global_settings_at(home.path()).unwrap();

    assert!(document["shell"]["disable_powershell"].is_null());
    assert!(document["hook_health"].is_null());
    assert!(document["codex"]["config_overrides"].is_null());
}

#[test]
fn merged_settings_deep_merges_and_treats_null_as_defer() {
    let global = json!({
        "shell": {
            "disable_powershell": true,
            "claude": { "disable_powershell": false }
        },
        "codex": {
            "config_overrides": ["global"],
            "extra": { "a": 1, "b": 2 }
        },
        "unknown": { "global": true }
    });
    let local = json!({
        "shell": {
            "disable_powershell": null,
            "claude": { "disable_powershell": true }
        },
        "codex": {
            "config_overrides": ["local"],
            "extra": { "b": null, "c": 3 }
        },
        "unknown": { "local": true }
    });

    let merged = merged_settings_document(&global, Some(&local));

    assert_eq!(merged["shell"]["disable_powershell"], true);
    assert_eq!(merged["shell"]["claude"]["disable_powershell"], true);
    assert_eq!(merged["codex"]["config_overrides"], json!(["local"]));
    assert_eq!(merged["codex"]["extra"]["a"], 1);
    assert_eq!(merged["codex"]["extra"]["b"], 2);
    assert_eq!(merged["codex"]["extra"]["c"], 3);
    assert_eq!(merged["unknown"]["global"], true);
    assert_eq!(merged["unknown"]["local"], true);
}

#[test]
fn existing_codex_overrides_are_user_owned() {
    let home = tempdir().unwrap();
    let path = settings_path_at(home.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, r#"{"codex":{"config_overrides":[]}}"#).unwrap();

    assert_eq!(
        load_or_init_codex_config_overrides_at(home.path(), true).unwrap(),
        Vec::<String>::new()
    );
    let text = fs::read_to_string(path).unwrap();
    assert!(!text.contains(DEFAULT_CODEX_GITHUB_PLUGIN_CONFIG_OVERRIDE));
}

#[test]
fn saves_auto_fix_hooks_sticky_opt_out_and_reset() {
    let home = tempdir().unwrap();

    save_auto_fix_hooks_enabled_at(home.path(), false).unwrap();
    assert!(!load_auto_fix_hooks_enabled_at(home.path()).unwrap());

    save_auto_fix_hooks_enabled_at(home.path(), true).unwrap();
    assert!(load_auto_fix_hooks_enabled_at(home.path()).unwrap());

    let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["hook_health"]["auto_fix_hooks"], true);
}

#[test]
fn saves_launch_setup_scope_per_backend() {
    let home = tempdir().unwrap();

    save_launch_setup_scope_at(home.path(), Backend::Codex, LaunchSetupScope::Global).unwrap();

    assert_eq!(
        load_launch_setup_scope_at(home.path(), Backend::Codex).unwrap(),
        Some(LaunchSetupScope::Global)
    );
    assert_eq!(
        load_launch_setup_scope_at(home.path(), Backend::Claude).unwrap(),
        None
    );
    let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["launch_setup"]["codex"]["scope"], "global");
}

#[test]
fn preserves_existing_settings_when_saving_scope() {
    let home = tempdir().unwrap();
    let path = settings_path_at(home.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{"unrelated":{"value":"kept"},"launch_setup":{"claude":{"scope":"session-only"}}}"#,
    )
    .unwrap();

    save_launch_setup_scope_at(home.path(), Backend::Codex, LaunchSetupScope::Global).unwrap();

    let text = fs::read_to_string(path).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["unrelated"]["value"], "kept");
    assert_eq!(json["launch_setup"]["claude"]["scope"], "session-only");
    assert_eq!(json["launch_setup"]["codex"]["scope"], "global");
}

#[test]
fn auto_fix_hooks_setting_preserves_existing_settings() {
    let home = tempdir().unwrap();
    let path = settings_path_at(home.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{"unrelated":{"value":"kept"},"launch_setup":{"codex":{"scope":"global"}}}"#,
    )
    .unwrap();

    save_auto_fix_hooks_enabled_at(home.path(), false).unwrap();

    let text = fs::read_to_string(path).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["unrelated"]["value"], "kept");
    assert_eq!(json["launch_setup"]["codex"]["scope"], "global");
    assert_eq!(json["hook_health"]["auto_fix_hooks"], false);
}

#[test]
fn saves_rust_optimize_settings() {
    let home = tempdir().unwrap();
    let settings = RustOptimizeSettings {
        use_soldr_shims: true,
        install_soldr: false,
        soldr_version: "1.2.3".to_string(),
    };

    save_rust_optimize_settings_at(home.path(), &settings).unwrap();

    assert_eq!(
        load_rust_optimize_settings_at(home.path()).unwrap(),
        Some(settings)
    );
    let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["optimize"]["rust"]["use_soldr_shims"], true);
    assert_eq!(json["optimize"]["rust"]["install_soldr"], false);
    assert_eq!(json["optimize"]["rust"]["soldr_version"], "1.2.3");
}

#[test]
fn rust_optimize_settings_preserve_existing_settings() {
    let home = tempdir().unwrap();
    let path = settings_path_at(home.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, r#"{"unrelated":{"value":"kept"}}"#).unwrap();

    save_rust_optimize_settings_at(home.path(), &RustOptimizeSettings::default()).unwrap();

    let text = fs::read_to_string(path).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["unrelated"]["value"], "kept");
    assert!(json["optimize"]["rust"].is_object());
}

#[test]
fn legacy_toml_is_read_and_migrated_on_next_save() {
    let home = tempdir().unwrap();
    let legacy = legacy_settings_path_at(home.path());
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::write(
            &legacy,
            "[backend]\ndefault = \"codex\"\n\n[hook_health]\nauto_fix_hooks = false\n\n[launch_setup.codex]\nscope = \"global\"\n\n[optimize.rust]\nuse_soldr_shims = true\ninstall_soldr = false\nsoldr_version = \"9.9.9\"\n",
        )
        .unwrap();

    assert!(!load_auto_fix_hooks_enabled_at(home.path()).unwrap());
    assert_eq!(
        load_default_backend_at(home.path()).unwrap(),
        Some(Backend::Codex)
    );
    assert_eq!(
        load_launch_setup_scope_at(home.path(), Backend::Codex).unwrap(),
        Some(LaunchSetupScope::Global)
    );
    assert_eq!(
        load_rust_optimize_settings_at(home.path()).unwrap(),
        Some(RustOptimizeSettings {
            use_soldr_shims: true,
            install_soldr: false,
            soldr_version: "9.9.9".to_string(),
        })
    );

    save_auto_fix_hooks_enabled_at(home.path(), true).unwrap();
    let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["backend"]["default"], "codex");
    assert_eq!(json["hook_health"]["auto_fix_hooks"], true);
    assert_eq!(json["launch_setup"]["codex"]["scope"], "global");
    assert_eq!(json["optimize"]["rust"]["soldr_version"], "9.9.9");
}

#[test]
fn missing_shell_section_defaults_to_false_for_both_backends() {
    let home = tempdir().unwrap();
    assert!(!load_shell_disable_powershell_for_backend_at(home.path(), Backend::Claude).unwrap());
    assert!(!load_shell_disable_powershell_for_backend_at(home.path(), Backend::Codex).unwrap());
    assert!(!settings_path_at(home.path()).exists());
}

#[test]
fn top_level_disable_propagates_to_both_backends_when_overrides_null() {
    let home = tempdir().unwrap();
    save_shell_disable_powershell_at(home.path(), true).unwrap();

    assert!(load_shell_disable_powershell_for_backend_at(home.path(), Backend::Claude).unwrap());
    assert!(load_shell_disable_powershell_for_backend_at(home.path(), Backend::Codex).unwrap());
}

#[test]
fn backend_override_wins_over_top_level() {
    let home = tempdir().unwrap();
    save_shell_disable_powershell_at(home.path(), true).unwrap();
    save_shell_disable_powershell_for_backend_at(home.path(), Backend::Claude, Some(false))
        .unwrap();

    assert!(
        !load_shell_disable_powershell_for_backend_at(home.path(), Backend::Claude).unwrap(),
        "claude override should resolve false even when top-level is true"
    );
    assert!(
        load_shell_disable_powershell_for_backend_at(home.path(), Backend::Codex).unwrap(),
        "codex with null override should inherit top-level true"
    );
}

#[test]
fn saves_shell_disable_powershell_preserves_existing_settings() {
    let home = tempdir().unwrap();
    let path = settings_path_at(home.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{"unrelated":{"value":"kept"},"launch_setup":{"codex":{"scope":"global"}}}"#,
    )
    .unwrap();

    save_shell_disable_powershell_at(home.path(), true).unwrap();

    let text = fs::read_to_string(&path).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["unrelated"]["value"], "kept");
    assert_eq!(json["launch_setup"]["codex"]["scope"], "global");
    assert_eq!(json["shell"]["disable_powershell"], true);
    assert!(
        json["shell"]["disable_powershell_note"]
            .as_str()
            .unwrap()
            .contains("PreToolUse hook"),
        "{text}"
    );
}

#[test]
fn clearing_backend_override_falls_back_to_top_level() {
    let home = tempdir().unwrap();
    save_shell_disable_powershell_at(home.path(), true).unwrap();
    save_shell_disable_powershell_for_backend_at(home.path(), Backend::Claude, Some(false))
        .unwrap();
    assert!(!load_shell_disable_powershell_for_backend_at(home.path(), Backend::Claude).unwrap());

    save_shell_disable_powershell_for_backend_at(home.path(), Backend::Claude, None).unwrap();
    assert!(
        load_shell_disable_powershell_for_backend_at(home.path(), Backend::Claude).unwrap(),
        "after clearing override claude should inherit top-level true"
    );

    let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert!(
        json["shell"]["claude"]["disable_powershell"].is_null(),
        "cleared override should serialize as JSON null, got: {text}"
    );
}

#[test]
fn legacy_toml_shell_section_is_migrated() {
    let home = tempdir().unwrap();
    let legacy = legacy_settings_path_at(home.path());
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::write(
        &legacy,
        "[shell]\ndisable_powershell = true\n\n[shell.codex]\ndisable_powershell = false\n",
    )
    .unwrap();

    assert!(
        load_shell_disable_powershell_for_backend_at(home.path(), Backend::Claude).unwrap(),
        "claude should inherit top-level true from legacy TOML"
    );
    assert!(
        !load_shell_disable_powershell_for_backend_at(home.path(), Backend::Codex).unwrap(),
        "codex override should override top-level"
    );
}

#[test]
fn cpu_banner_defaults_when_settings_missing() {
    let home = tempdir().unwrap();
    let got = load_cpu_banner_settings_at(home.path()).unwrap();
    assert_eq!(got, CpuBannerSettings::default());
}

#[test]
fn cpu_banner_reads_enabled_override() {
    let home = tempdir().unwrap();
    let path = settings_path_at(home.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, r#"{"foreground":{"cpu_banner":{"enabled":false}}}"#).unwrap();
    let got = load_cpu_banner_settings_at(home.path()).unwrap();
    assert!(!got.enabled);
    assert_eq!(got.heartbeat_secs, 30, "default heartbeat preserved");
}

#[test]
fn cpu_banner_reads_heartbeat_override() {
    let home = tempdir().unwrap();
    let path = settings_path_at(home.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{"foreground":{"cpu_banner":{"heartbeat_secs":120}}}"#,
    )
    .unwrap();
    let got = load_cpu_banner_settings_at(home.path()).unwrap();
    assert!(got.enabled, "default enabled preserved");
    assert_eq!(got.heartbeat_secs, 120);
}

#[test]
fn provider_profile_round_trips_through_save_and_load() {
    let home = tempdir().unwrap();
    save_settings_patch_at(
        home.path(),
        GlobalSettingsPatch {
            provider_profiles: vec![ProviderProfilePatch {
                provider: Some(ModelProvider::Codex),
                model: Some("codex-luna".to_string()),
                harness: Some(HarnessSelection::Codex),
                effort: Some(Some(EffortLevel::High)),
                context_window: Some(Some("auto".to_string())),
            }],
            ..GlobalSettingsPatch::default()
        },
    )
    .unwrap();

    let snapshot = load_launch_preferences_read_only_at(home.path()).unwrap();
    let profile = snapshot.profile(ModelProvider::Codex).unwrap();
    assert_eq!(profile.model.as_deref(), Some("codex-luna"));
    assert_eq!(profile.harness, Some(HarnessSelection::Codex));
    assert_eq!(profile.effort, Some(EffortLevel::High));
    assert_eq!(profile.context_window.as_deref(), Some("auto"));

    let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["providers"]["codex"]["model"], "codex-luna");
    assert_eq!(json["providers"]["codex"]["effort"], "high");
}

/// Kimi twin of `provider_profile_round_trips_through_save_and_load`
/// (#937 Phase 3): `providers.kimi.*` validates and round-trips like every
/// other provider's profile.
#[test]
fn provider_profile_round_trips_for_kimi() {
    let home = tempdir().unwrap();
    save_settings_patch_at(
        home.path(),
        GlobalSettingsPatch {
            provider_profiles: vec![ProviderProfilePatch {
                provider: Some(ModelProvider::Kimi),
                model: Some("kimi-k3".to_string()),
                harness: Some(HarnessSelection::Claude),
                effort: Some(Some(EffortLevel::High)),
                context_window: Some(Some("auto".to_string())),
            }],
            ..GlobalSettingsPatch::default()
        },
    )
    .unwrap();

    let snapshot = load_launch_preferences_read_only_at(home.path()).unwrap();
    let profile = snapshot.profile(ModelProvider::Kimi).unwrap();
    assert_eq!(profile.model.as_deref(), Some("kimi-k3"));
    assert_eq!(profile.harness, Some(HarnessSelection::Claude));
    assert_eq!(profile.effort, Some(EffortLevel::High));
    assert_eq!(profile.context_window.as_deref(), Some("auto"));

    let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["providers"]["kimi"]["model"], "kimi-k3");
    assert_eq!(json["providers"]["kimi"]["effort"], "high");
}

#[test]
fn provider_profile_effort_clears_when_patched_to_none() {
    let home = tempdir().unwrap();
    save_settings_patch_at(
        home.path(),
        GlobalSettingsPatch {
            provider_profiles: vec![ProviderProfilePatch {
                provider: Some(ModelProvider::Codex),
                effort: Some(Some(EffortLevel::High)),
                ..ProviderProfilePatch::default()
            }],
            ..GlobalSettingsPatch::default()
        },
    )
    .unwrap();
    save_settings_patch_at(
        home.path(),
        GlobalSettingsPatch {
            provider_profiles: vec![ProviderProfilePatch {
                provider: Some(ModelProvider::Codex),
                effort: Some(None),
                ..ProviderProfilePatch::default()
            }],
            ..GlobalSettingsPatch::default()
        },
    )
    .unwrap();

    let snapshot = load_launch_preferences_read_only_at(home.path()).unwrap();
    assert_eq!(
        snapshot
            .profile(ModelProvider::Codex)
            .and_then(|p| p.effort),
        None
    );
}

fn write_provider_settings(home: &Path, body: &str) {
    let path = settings_path_at(home);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, body).unwrap();
}

#[test]
fn provider_profile_rejects_non_canonical_model() {
    let home = tempdir().unwrap();
    write_provider_settings(
        home.path(),
        r#"{"providers":{"codex":{"model":"gpt-5.6-terra"}}}"#,
    );
    assert!(matches!(
        load_launch_preferences_read_only_at(home.path()),
        Err(SettingsError::InvalidProviderProfile {
            provider: ModelProvider::Codex,
            ..
        })
    ));
}

#[test]
fn provider_profile_rejects_cross_provider_model() {
    let home = tempdir().unwrap();
    write_provider_settings(
        home.path(),
        r#"{"providers":{"codex":{"model":"deepseek-v4-pro"}}}"#,
    );
    assert!(matches!(
        load_launch_preferences_read_only_at(home.path()),
        Err(SettingsError::InvalidProviderProfile {
            provider: ModelProvider::Codex,
            ..
        })
    ));
}

/// Cross-provider model rejection, mirrored onto Kimi (#937 Phase 3): a
/// `deepseek-*` model in `providers.kimi.*` must be rejected the same way a
/// `deepseek-v4-pro` model was rejected on the Codex profile above.
#[test]
fn provider_profile_rejects_deepseek_model_on_kimi_profile() {
    let home = tempdir().unwrap();
    write_provider_settings(
        home.path(),
        r#"{"providers":{"kimi":{"model":"deepseek-v4-pro"}}}"#,
    );
    assert!(matches!(
        load_launch_preferences_read_only_at(home.path()),
        Err(SettingsError::InvalidProviderProfile {
            provider: ModelProvider::Kimi,
            ..
        })
    ));
}

#[test]
fn provider_profile_rejects_unknown_effort() {
    let home = tempdir().unwrap();
    write_provider_settings(home.path(), r#"{"providers":{"codex":{"effort":"turbo"}}}"#);
    assert!(matches!(
        load_launch_preferences_read_only_at(home.path()),
        Err(SettingsError::InvalidProviderProfile { .. })
    ));
}

#[test]
fn provider_profile_rejects_unsupported_context_window() {
    let home = tempdir().unwrap();
    write_provider_settings(
        home.path(),
        r#"{"providers":{"codex":{"context_window":"2m"}}}"#,
    );
    assert!(matches!(
        load_launch_preferences_read_only_at(home.path()),
        Err(SettingsError::InvalidProviderProfile { .. })
    ));
}

#[test]
fn provider_profile_rejects_codex_harness_on_non_codex_provider() {
    let home = tempdir().unwrap();
    write_provider_settings(
        home.path(),
        r#"{"providers":{"deepseek":{"harness":"codex"}}}"#,
    );
    assert!(matches!(
        load_launch_preferences_read_only_at(home.path()),
        Err(SettingsError::InvalidProviderProfile {
            provider: ModelProvider::DeepSeek,
            ..
        })
    ));
}
