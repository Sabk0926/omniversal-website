//! The acceptance test for slice 1.
//!
//! These run the real pipeline against the real filesystem and build real
//! `.deb` files. Nothing here is mocked: the point of slice 1 is to find out
//! whether the idea works, and a test that stubs the interesting parts would
//! not answer that.

use super::*;
use omnia_core::Config;
use omnia_registry::Registry;
use std::fs;
use std::path::PathBuf;

struct Harness {
    root: PathBuf,
}

impl Harness {
    fn new(name: &str) -> Harness {
        let root = std::env::temp_dir().join(format!(
            "omnia-forge-test-{name}-{}-{}",
            std::process::id(),
            omnia_registry::now_epoch_seconds()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("registry")).unwrap();
        fs::create_dir_all(root.join("work")).unwrap();
        Harness { root }
    }

    fn registry(&self) -> Registry {
        Registry::open(&self.root.join("registry")).unwrap()
    }

    fn workdir(&self) -> PathBuf {
        self.root.join("work")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn dpkg_available() -> bool {
    std::process::Command::new("dpkg-deb")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn asking_for_a_backup_builds_a_tested_installable_package() {
    if !dpkg_available() {
        return;
    }
    let harness = Harness::new("build");
    let catalog = Catalog::builtin().unwrap();
    let config = Config::builtin();
    let mut registry = harness.registry();
    let planner = StubPlanner;

    let mut forge = Forge {
        catalog: &catalog,
        registry: &mut registry,
        planner: &planner,
        config: &config.settings.forge,
        home: Some("/home/tester".into()),
        workdir: harness.workdir(),
    };

    let outcome = forge
        .build("back up my photos every night")
        .expect("build must not error");

    match outcome {
        Outcome::Built {
            capability,
            deb,
            proof,
        } => {
            // 1. a .deb exists and dpkg can read it
            assert!(deb.exists(), "no .deb at {}", deb.display());
            let contents = std::process::Command::new("dpkg-deb")
                .arg("--contents")
                .arg(&deb)
                .output()
                .unwrap();
            let listing = String::from_utf8_lossy(&contents.stdout);
            assert!(
                listing.contains(".service"),
                "service unit missing:\n{listing}"
            );
            assert!(
                listing.contains(".timer"),
                "timer missing for a nightly job:\n{listing}"
            );

            // 2. its reach is only what the parts declared
            assert_eq!(
                capability.permissions.write_paths,
                vec!["/var/backups/pictures"],
                "writes exactly one place"
            );
            // Two reads, not one: snapshot reads the source, and verify-restore
            // reads the archive back to prove the restore. The union across
            // parts is the capability's real reach.
            assert_eq!(
                capability.permissions.read_paths,
                vec!["/var/backups/pictures", "/home/tester/Pictures"]
            );
            assert!(
                !capability.permissions.network,
                "a local backup needs no network"
            );

            // 3. the test that gated it actually asserted a restore
            assert!(proof.passed);
            assert!(proof.asserts.contains("byte for byte"), "{}", proof.asserts);

            // 4. provenance answers "why is this here"
            assert_eq!(capability.provenance.attempts, 1);
            assert!(capability.provenance.model.contains("stub"));
            assert_eq!(capability.intent, "back up my photos every night");
        }
        other => panic!("expected a build, got {other:?}"),
    }
}

#[test]
fn asking_twice_does_not_build_a_second_one() {
    if !dpkg_available() {
        return;
    }
    let harness = Harness::new("dedup");
    let catalog = Catalog::builtin().unwrap();
    let config = Config::builtin();
    let mut registry = harness.registry();
    let planner = StubPlanner;

    {
        let mut forge = Forge {
            catalog: &catalog,
            registry: &mut registry,
            planner: &planner,
            config: &config.settings.forge,
            home: Some("/home/tester".into()),
            workdir: harness.workdir(),
        };
        forge.build("back up my photos every night").unwrap();
    }

    // Different wording, same resolved plan.
    let mut forge = Forge {
        catalog: &catalog,
        registry: &mut registry,
        planner: &planner,
        config: &config.settings.forge,
        home: Some("/home/tester".into()),
        workdir: harness.workdir(),
    };
    let outcome = forge.build("backup the photos nightly please").unwrap();

    match outcome {
        Outcome::AlreadyHave(existing) => {
            assert_eq!(existing.name, "backup-pictures");
        }
        other => panic!("expected AlreadyHave, got {other:?}"),
    }
    assert_eq!(registry.len(), 1, "exactly one capability, not two");
}

#[test]
fn a_capability_that_cannot_be_proven_is_never_installed() {
    // The gate, tested directly: a plan whose command succeeds but produces
    // nothing must not reach the registry.
    let harness = Harness::new("unproven");
    let catalog = Catalog::builtin().unwrap();
    let config = Config::builtin();
    let mut registry = harness.registry();

    struct BrokenPlanner;
    impl Planner for BrokenPlanner {
        fn plan(
            &self,
            _intent: &str,
            _catalog: &Catalog,
            _feedback: Option<&str>,
        ) -> Result<omnia_parts::Composition, String> {
            // `schedule` alone copies nothing, so no file can be restored.
            omnia_parts::Composition::from_json(
                r#"{"steps":[{"part":"schedule","args":{"at":"02:00"}}]}"#,
            )
        }
        fn describe(&self) -> String {
            "broken planner".into()
        }
    }

    let planner = BrokenPlanner;
    let mut forge = Forge {
        catalog: &catalog,
        registry: &mut registry,
        planner: &planner,
        config: &config.settings.forge,
        home: Some("/home/tester".into()),
        workdir: harness.workdir(),
    };

    match forge.build("back up my photos").unwrap() {
        Outcome::Refused { attempts } => {
            assert!(!attempts.is_empty());
            assert!(
                attempts.iter().any(|a| a.contains("never created")),
                "the reason must be the failed proof: {attempts:?}"
            );
        }
        other => panic!("expected Refused, got {other:?}"),
    }
    assert_eq!(registry.len(), 0, "nothing unproven reaches the registry");
}

#[test]
fn a_plan_aimed_at_secrets_is_refused_before_anything_is_built() {
    let harness = Harness::new("secrets");
    let catalog = Catalog::builtin().unwrap();
    let config = Config::builtin();
    let mut registry = harness.registry();

    struct SecretsPlanner;
    impl Planner for SecretsPlanner {
        fn plan(
            &self,
            _intent: &str,
            _catalog: &Catalog,
            _feedback: Option<&str>,
        ) -> Result<omnia_parts::Composition, String> {
            omnia_parts::Composition::from_json(
                r#"{"steps":[{"part":"snapshot","args":{"source":"~/.ssh","destination":"/var/backups/keys"}}]}"#,
            )
        }
        fn describe(&self) -> String {
            "secrets planner".into()
        }
    }

    let planner = SecretsPlanner;
    let mut forge = Forge {
        catalog: &catalog,
        registry: &mut registry,
        planner: &planner,
        config: &config.settings.forge,
        home: Some("/home/tester".into()),
        workdir: harness.workdir(),
    };

    match forge.build("back up my ssh keys").unwrap() {
        Outcome::Refused { attempts } => {
            assert!(
                attempts.iter().any(|a| a.contains("secret material")),
                "{attempts:?}"
            );
        }
        other => panic!("expected Refused, got {other:?}"),
    }
    assert_eq!(registry.len(), 0);
}

#[test]
fn rejections_are_fed_back_so_a_retry_can_correct() {
    let harness = Harness::new("retry");
    let catalog = Catalog::builtin().unwrap();
    let config = Config::builtin();
    let mut registry = harness.registry();

    use std::cell::RefCell;
    struct LearningPlanner {
        seen: RefCell<Vec<String>>,
    }
    impl Planner for LearningPlanner {
        fn plan(
            &self,
            _intent: &str,
            _catalog: &Catalog,
            feedback: Option<&str>,
        ) -> Result<omnia_parts::Composition, String> {
            self.seen
                .borrow_mut()
                .push(feedback.unwrap_or("none").to_string());
            if feedback.is_none() {
                // First attempt invents a part.
                omnia_parts::Composition::from_json(
                    r#"{"steps":[{"part":"archive-everything","args":{}}]}"#,
                )
            } else {
                omnia_parts::Composition::from_json(
                    r#"{"steps":[{"part":"snapshot","args":{"source":"~/Pictures","destination":"/var/backups/pictures"}}]}"#,
                )
            }
        }
        fn describe(&self) -> String {
            "learning planner".into()
        }
    }

    let planner = LearningPlanner {
        seen: RefCell::new(Vec::new()),
    };
    let mut forge = Forge {
        catalog: &catalog,
        registry: &mut registry,
        planner: &planner,
        config: &config.settings.forge,
        home: Some("/home/tester".into()),
        workdir: harness.workdir(),
    };

    let _ = forge.build("back up my photos");

    let seen = planner.seen.borrow();
    assert_eq!(seen[0], "none", "first attempt gets no feedback");
    assert!(
        seen[1].contains("archive-everything") && seen[1].contains("snapshot"),
        "the retry must carry both the mistake and the real options: {}",
        seen[1]
    );
}

#[test]
fn the_capability_name_comes_from_the_plan_not_the_wording() {
    let steps = Catalog::builtin()
        .unwrap()
        .resolve(
            &omnia_parts::Composition::from_json(
                r#"{"steps":[{"part":"snapshot","args":{"source":"~/Pictures","destination":"/var/backups/pictures"}}]}"#,
            )
            .unwrap(),
        )
        .unwrap();

    assert_eq!(
        capability_name("back up my photos", &steps),
        "backup-pictures"
    );
    assert_eq!(
        capability_name("please backup the photos nightly", &steps),
        "backup-pictures",
        "different wording, same name"
    );
}
