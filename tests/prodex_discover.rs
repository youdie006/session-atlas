//! prodex discovery goes through the machine-wide bridges registry that
//! prodex >=0.11.0 maintains. Missing roots are normal (a registered repo may
//! be deleted); no registry means no prodex on this machine.

use sessionwiki::adapters;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

#[test]
fn discovers_tasks_across_registered_bridge_roots() {
    let _g = LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Two registered roots: one real (fixture copy), one deleted.
    let repo = dir.path().join("repo-a");
    let tasks = repo.join(".bridge").join("tasks");
    std::fs::create_dir_all(&tasks).unwrap();
    std::fs::write(
        tasks.join("task_20260707_090000_x.json"),
        r#"{"id":"task_20260707_090000_x","title":"t","prompt":"p"}"#,
    )
    .unwrap();
    let registry = dir.path().join("bridges.json");
    std::fs::write(
        &registry,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "roots": [repo.to_str().unwrap(), "/no/such/repo-anywhere"]
        }))
        .unwrap(),
    )
    .unwrap();
    std::env::set_var("SESSIONWIKI_PRODEX_REGISTRY", &registry);

    let adapter = adapters::by_name("prodex").unwrap();
    let d = adapter.discover();
    assert_eq!(d.files.len(), 1, "one task found, missing root skipped");
    assert!(!d.had_error, "a deleted registered repo is not an error");

    std::env::remove_var("SESSIONWIKI_PRODEX_REGISTRY");
}

#[test]
fn no_registry_means_no_prodex() {
    let _g = LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var(
        "SESSIONWIKI_PRODEX_REGISTRY",
        dir.path().join("absent.json"),
    );
    let adapter = adapters::by_name("prodex").unwrap();
    let d = adapter.discover();
    assert!(d.files.is_empty());
    assert!(!d.had_error);
    std::env::remove_var("SESSIONWIKI_PRODEX_REGISTRY");
}
