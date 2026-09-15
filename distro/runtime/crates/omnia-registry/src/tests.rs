use super::*;
use omnia_parts::{Catalog, Composition, Permissions};

fn steps_for(json: &str) -> Vec<ResolvedStep> {
    let catalog = Catalog::builtin().unwrap();
    let composition = Composition::from_json(json).expect("test plan parses");
    catalog.resolve(&composition).expect("test plan resolves")
}

fn backup_steps(source: &str, destination: &str) -> Vec<ResolvedStep> {
    steps_for(&format!(
        r#"{{"steps":[
            {{"part":"snapshot","args":{{"source":"{source}","destination":"{destination}"}}}},
            {{"part":"schedule","args":{{"at":"02:00"}}}}
        ]}}"#
    ))
}

fn capability(name: &str, intent: &str, steps: Vec<ResolvedStep>) -> Capability {
    Capability {
        name: name.to_string(),
        version: "1.0.0".into(),
        intent: intent.to_string(),
        fingerprint: record::fingerprint(&steps),
        permissions: omnia_parts::total_permissions(&steps),
        steps,
        package: format!("omnia-cap-{name}"),
        installed_at: 1_789_339_011,
        provenance: Provenance {
            intent: intent.to_string(),
            tier: "orchestrator".into(),
            model: "qwen2.5-3b-instruct-q4".into(),
            attempts: 1,
        },
        test: TestOutcome {
            asserts: "a file restores byte for byte".into(),
            passed: true,
            epoch_seconds: 1_789_339_011,
        },
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("omnia-registry-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

#[test]
fn a_missing_directory_is_an_empty_registry() {
    // A machine that has learned nothing yet is normal, not broken.
    let registry = Registry::open(&scratch("missing").join("never-created")).unwrap();
    assert!(registry.is_empty());
}

#[test]
fn a_capability_round_trips_through_disk() {
    let dir = scratch("roundtrip");
    let mut registry = Registry::open(&dir).unwrap();
    let steps = backup_steps("~/Pictures", "/var/backups/pictures");
    registry
        .insert(capability(
            "backup-pictures",
            "back up my photos nightly",
            steps,
        ))
        .unwrap();

    let reopened = Registry::open(&dir).unwrap();
    assert_eq!(reopened.len(), 1);
    let found = reopened.get("backup-pictures").unwrap();
    assert_eq!(found.intent, "back up my photos nightly");
    assert!(
        found.describe().contains("2026-09-13"),
        "{}",
        found.describe()
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn differently_worded_requests_that_resolve_the_same_are_one_capability() {
    // This is the assertion that matters: dedup keys on what the plan resolves
    // to, not on the words the user happened to use.
    let dir = scratch("dedup");
    let mut registry = Registry::open(&dir).unwrap();
    registry
        .insert(capability(
            "backup-pictures",
            "back up my photos nightly",
            backup_steps("~/Pictures", "/var/backups/pictures"),
        ))
        .unwrap();

    let asked_again = backup_steps("~/Pictures", "/var/backups/pictures");
    match registry.find(&asked_again) {
        Some(Match::Identical(existing)) => assert_eq!(existing.name, "backup-pictures"),
        other => panic!("expected Identical, got {other:?}"),
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_different_folder_is_the_same_shape_not_the_same_capability() {
    let dir = scratch("shape");
    let mut registry = Registry::open(&dir).unwrap();
    registry
        .insert(capability(
            "backup-pictures",
            "back up my photos",
            backup_steps("~/Pictures", "/var/backups/pictures"),
        ))
        .unwrap();

    let documents = backup_steps("~/Documents", "/var/backups/documents");
    match registry.find(&documents) {
        Some(Match::SameShape(existing)) => assert_eq!(existing.name, "backup-pictures"),
        other => panic!("expected SameShape, got {other:?}"),
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn step_order_changes_the_fingerprint() {
    let forward = steps_for(
        r#"{"steps":[{"part":"snapshot","args":{"source":"~/a","destination":"/b"}},
                     {"part":"schedule","args":{"at":"02:00"}}]}"#,
    );
    let reversed = steps_for(
        r#"{"steps":[{"part":"schedule","args":{"at":"02:00"}},
                     {"part":"snapshot","args":{"source":"~/a","destination":"/b"}}]}"#,
    );
    assert_ne!(
        record::fingerprint(&forward),
        record::fingerprint(&reversed)
    );
}

#[test]
fn two_capabilities_writing_the_same_path_conflict() {
    // The junk drawer, caught before it exists: two backup jobs writing the
    // same destination would silently corrupt each other.
    let dir = scratch("conflict");
    let mut registry = Registry::open(&dir).unwrap();
    registry
        .insert(capability(
            "backup-pictures",
            "back up photos",
            backup_steps("~/Pictures", "/var/backups/shared"),
        ))
        .unwrap();

    let rival = backup_steps("~/Documents", "/var/backups/shared");
    let conflicts = registry.conflicts(&rival);
    assert_eq!(conflicts.len(), 1);
    assert!(
        conflicts[0].to_string().contains("/var/backups/shared"),
        "{}",
        conflicts[0]
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_capability_does_not_conflict_with_itself() {
    let dir = scratch("selfconflict");
    let mut registry = Registry::open(&dir).unwrap();
    let steps = backup_steps("~/Pictures", "/var/backups/pictures");
    registry
        .insert(capability("backup-pictures", "photos", steps.clone()))
        .unwrap();
    assert!(
        registry.conflicts(&steps).is_empty(),
        "replacing itself is not a conflict"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn similar_intents_is_a_hint_that_ignores_filler() {
    let dir = scratch("similar");
    let mut registry = Registry::open(&dir).unwrap();
    registry
        .insert(capability(
            "backup-pictures",
            "back up my photos every night",
            backup_steps("~/Pictures", "/var/backups/pictures"),
        ))
        .unwrap();

    let hits = registry.similar_intents("please back up the photos for me");
    assert_eq!(
        hits.len(),
        1,
        "shared content words match through different filler"
    );

    assert!(
        registry
            .similar_intents("restart nginx when it fails")
            .is_empty(),
        "unrelated requests must not match"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn removing_is_idempotent() {
    let dir = scratch("remove");
    let mut registry = Registry::open(&dir).unwrap();
    registry
        .insert(capability("x", "x", backup_steps("~/a", "/b")))
        .unwrap();
    assert!(registry.remove("x").unwrap());
    assert!(
        !registry.remove("x").unwrap(),
        "removing what is gone is not an error"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_corrupt_record_names_the_file() {
    let dir = scratch("corrupt");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("broken.json"), "{not json").unwrap();
    let err = Registry::open(&dir).unwrap_err();
    assert!(err.to_string().contains("broken.json"), "{err}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn permissions_are_carried_on_the_record() {
    let steps = backup_steps("~/Pictures", "/var/backups/pictures");
    let capability = capability("backup-pictures", "photos", steps);
    assert_eq!(
        capability.permissions.write_paths,
        vec!["/var/backups/pictures"]
    );
    assert_eq!(
        capability.permissions,
        Permissions {
            read_paths: vec!["~/Pictures".into()],
            write_paths: vec!["/var/backups/pictures".into()],
            network: false,
        }
    );
}
