use super::*;

fn record(name: &str, created_at: u64) -> GenerationRecord {
    GenerationRecord {
        repo_id: "repo".to_string(),
        workspace_id: "ws".to_string(),
        stack: "rust".to_string(),
        role: "target".to_string(),
        scope_digest: name.to_string(),
        name: name.to_string(),
        created_at,
        last_used: created_at,
        size_bytes: 100,
    }
}

fn lease(id: &str, resources: &[&str], now: u64) -> Lease {
    Lease {
        id: id.to_string(),
        workspace_id: "ws".to_string(),
        stack: "rust".to_string(),
        resources: resources.iter().map(|name| (*name).to_string()).collect(),
        client_pid: 1,
        client_start: 1,
        acquired_at: now,
        last_heartbeat: now,
        ttl_secs: super::super::lease::LEASE_TTL_SECS,
    }
}

fn empty() -> Registry {
    Registry {
        schema: SCHEMA,
        registry_id: "reg".to_string(),
        generations: Vec::new(),
        leases: Vec::new(),
    }
}

#[test]
fn a_missing_file_starts_a_fresh_registry_with_a_random_id() {
    let dir = tempfile::tempdir().unwrap();
    let a = load(&path_in(dir.path())).unwrap();
    let b = load(&path_in(dir.path().join("other").as_path())).unwrap();
    assert_eq!(a.schema, SCHEMA);
    assert_eq!(a.registry_id.len(), 32);
    assert_ne!(a.registry_id, b.registry_id);
}

#[test]
fn a_saved_registry_round_trips_and_keeps_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = path_in(dir.path());
    let mut registry = load(&path).unwrap();
    registry.record_use(record("g1", 10));
    registry.acquire(lease("l1", &["g1"], 10));
    save(&path, &registry).unwrap();
    assert_eq!(load(&path).unwrap(), registry);
}

#[test]
fn a_corrupt_or_newer_registry_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = path_in(dir.path());
    fs::write(&path, "{not json").unwrap();
    assert_eq!(load(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);

    let mut newer = empty();
    newer.schema = SCHEMA + 1;
    fs::write(&path, serde_json::to_string(&newer).unwrap()).unwrap();
    let error = load(&path).unwrap_err();
    assert!(error.to_string().contains("upgrade clud"), "{error}");
}

#[test]
fn releasing_a_lease_restarts_the_retention_clock() {
    let mut registry = empty();
    registry.record_use(record("g1", 10));
    registry.acquire(lease("l1", &["g1"], 10));
    assert!(registry.heartbeat("l1", 500));
    assert!(!registry.heartbeat("missing", 500));
    assert!(registry.release("l1", 900));
    assert!(!registry.release("l1", 901));
    assert!(registry.leases.is_empty());
    assert_eq!(registry.generations[0].last_used, 900);
}

#[test]
fn record_use_refreshes_rather_than_duplicates() {
    let mut registry = empty();
    registry.record_use(record("g1", 10));
    let mut again = record("g1", 5);
    again.last_used = 50;
    again.size_bytes = 7;
    registry.record_use(again);
    assert_eq!(registry.generations.len(), 1);
    assert_eq!(registry.generations[0].last_used, 50);
    assert_eq!(registry.generations[0].size_bytes, 7);
}

#[test]
fn the_view_marks_leases_and_the_newest_generation_per_role() {
    let mut registry = empty();
    registry.record_use(record("old", 10));
    registry.record_use(record("new", 20));
    let mut other_role = record("cargo", 5);
    other_role.role = "cargo-home".to_string();
    registry.record_use(other_role);
    registry.acquire(lease("l1", &["old"], 30));

    let view = registry.view(&|ws| ws == "ws", &|_| Some(42));
    let by_name = |name: &str| {
        view.iter()
            .find(|generation| generation.scope_digest == name)
            .unwrap()
    };
    assert!(by_name("old").leased);
    assert!(!by_name("old").newest);
    assert!(by_name("new").newest && !by_name("new").leased);
    assert!(by_name("cargo").newest, "newest is per role");
    assert!(view.iter().all(|generation| generation.workspace_done));
    assert!(view
        .iter()
        .all(|generation| generation.live_cwd_activity == Some(42)));
}

#[test]
fn forget_drops_only_the_named_generations() {
    let mut registry = empty();
    registry.record_use(record("a", 1));
    registry.record_use(record("b", 2));
    registry.forget(&["a".to_string()]);
    let names: Vec<&str> = registry
        .generations
        .iter()
        .map(|generation| generation.name.as_str())
        .collect();
    assert_eq!(names, ["b"]);
}
