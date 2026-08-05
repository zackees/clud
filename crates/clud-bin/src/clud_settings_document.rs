use super::*;

pub(super) fn set_default_model_provider(document: &mut Value, provider: ModelProvider) {
    object_entry(document, "backend").insert(
        "default".to_string(),
        Value::String(provider.as_str().to_string()),
    );
}

pub(super) fn set_default_harness(document: &mut Value, harness: HarnessSelection) {
    object_entry(document, "harness").insert(
        "default".to_string(),
        Value::String(harness.as_str().to_string()),
    );
}

pub(super) fn set_launch_setup_scope(
    document: &mut Value,
    backend: Backend,
    scope: LaunchSetupScope,
) {
    let launch_setup = object_entry(document, "launch_setup");
    let entry = launch_setup
        .entry(backend_settings_key(backend).to_string())
        .or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    entry.as_object_mut().unwrap().insert(
        "scope".to_string(),
        Value::String(scope.as_str().to_string()),
    );
}

pub(super) fn infer_default_backend_from_launch_setup(document: &Value) -> Option<Backend> {
    let mut inferred = None;
    for backend in [Backend::Claude, Backend::Codex] {
        let is_global = document
            .get("launch_setup")
            .and_then(|item| item.get(backend_settings_key(backend)))
            .and_then(|item| item.get("scope"))
            .and_then(Value::as_str)
            .and_then(LaunchSetupScope::from_settings_str)
            == Some(LaunchSetupScope::Global);
        if is_global {
            if inferred.is_some() {
                return None;
            }
            inferred = Some(backend);
        }
    }
    inferred
}

pub(super) fn object_entry<'a>(document: &'a mut Value, key: &str) -> &'a mut Map<String, Value> {
    if !document.is_object() {
        *document = json!({});
    }
    let root = document.as_object_mut().unwrap();
    let entry = root.entry(key.to_string()).or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    entry.as_object_mut().unwrap()
}

pub(super) fn seed_object_entry<'a>(
    document: &'a mut Value,
    key: &str,
) -> Option<&'a mut Map<String, Value>> {
    if !document.is_object() {
        *document = json!({});
    }
    let root = document.as_object_mut().unwrap();
    if !root.contains_key(key) {
        root.insert(key.to_string(), json!({}));
    }
    root.get_mut(key).and_then(Value::as_object_mut)
}

pub(super) fn backend_settings_key(backend: Backend) -> &'static str {
    backend.executable_name()
}
