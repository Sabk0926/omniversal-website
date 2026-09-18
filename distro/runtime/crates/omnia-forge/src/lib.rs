//! The five-step pipeline: look up, plan, materialise, prove, declare.
//!
//! Every other crate in slice 1 is a stage of this. The ordering is the design:
//! nothing is packaged before it is proven, nothing is proven before its reach
//! is fixed, and nothing is built at all if the machine already has it.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod ladder;
mod package;
mod planner;
mod prove;

pub use ladder::{Climb, Declaration, Fix, Ladder, RealSystem, Rung, Step, System};
pub use package::{PackageError, PackageSpec};
pub use planner::{ModelPlanner, Planner, StubPlanner};
pub use prove::{prove, Proof, Scratch};

use omnia_core::config::ForgeConfig;
use omnia_parts::{total_permissions, Catalog, ResolvedStep};
use omnia_registry::{Capability, Match, Provenance, Registry, TestOutcome};
use omnia_sandbox::{Unit, UnitKind};

/// What a build attempt produced.
#[derive(Debug)]
pub enum Outcome {
    /// The machine already has this. Step 1 doing its job.
    AlreadyHave(Box<Capability>),
    Built {
        capability: Box<Capability>,
        deb: std::path::PathBuf,
        proof: Proof,
    },
    /// Every attempt was rejected. Carries the reasons, in order, because the
    /// interesting question after a failure is what it kept getting wrong.
    Refused { attempts: Vec<String> },
}

#[derive(Debug)]
pub enum ForgeError {
    Registry(String),
    Sandbox(String),
    Package(String),
    Io(String),
}

impl std::fmt::Display for ForgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForgeError::Registry(e) => write!(f, "registry: {e}"),
            ForgeError::Sandbox(e) => write!(f, "sandbox: {e}"),
            ForgeError::Package(e) => write!(f, "packaging: {e}"),
            ForgeError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ForgeError {}

pub struct Forge<'a> {
    pub catalog: &'a Catalog,
    pub registry: &'a mut Registry,
    pub planner: &'a dyn Planner,
    pub config: &'a ForgeConfig,
    pub home: Option<String>,
    pub workdir: std::path::PathBuf,
}

impl Forge<'_> {
    pub fn build(&mut self, intent: &str) -> Result<Outcome, ForgeError> {
        let mut rejections: Vec<String> = Vec::new();
        let mut feedback: Option<String> = None;

        for attempt in 1..=self.config.max_attempts {
            let composition = match self.planner.plan(intent, self.catalog, feedback.as_deref()) {
                Ok(composition) => composition,
                Err(e) => {
                    rejections.push(e.clone());
                    feedback = Some(e);
                    continue;
                }
            };

            // --- step 2b: validate against the catalogue ---
            let steps = match self.catalog.resolve(&composition) {
                Ok(steps) => steps,
                Err(e) => {
                    let message = e.to_string();
                    rejections.push(message.clone());
                    feedback = Some(message);
                    continue;
                }
            };

            // A capability built around a binary this machine lacks would
            // install cleanly and fail on its first scheduled run.
            if let Some(message) = self.missing_binaries(&steps) {
                rejections.push(message.clone());
                feedback = Some(message);
                continue;
            }

            // --- step 1: do we already have it? ---
            // After planning, because the fingerprint is what makes this
            // reliable across differently-worded requests.
            if let Some(Match::Identical(existing)) = self.registry.find(&steps) {
                return Ok(Outcome::AlreadyHave(Box::new(existing)));
            }

            let conflicts = self.registry.conflicts(&steps);
            if !conflicts.is_empty() {
                let message = conflicts
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ");
                rejections.push(message.clone());
                feedback = Some(format!("{message}. Choose a different destination."));
                continue;
            }

            // --- step 3: fix the reach ---
            let derived = total_permissions(&steps);
            if let Err(e) = omnia_sandbox::check(&derived) {
                let message = e.to_string();
                rejections.push(message.clone());
                feedback = Some(message);
                continue;
            }
            let permissions = omnia_sandbox::expand(&derived, self.home.as_deref())
                .map_err(|e| ForgeError::Sandbox(e.to_string()))?;

            // --- step 4: prove it ---
            let scratch = Scratch::new(&format!("attempt{attempt}"))
                .map_err(|e| ForgeError::Io(e.to_string()))?;
            let proof = prove(&steps, scratch.path()).map_err(|e| ForgeError::Io(e.to_string()))?;

            if !proof.passed {
                let message = format!("the capability did not prove out: {}", proof.detail);
                rejections.push(message.clone());
                feedback = Some(message);
                continue;
            }

            // The gate. Configuration cannot turn this off -- omnia-core
            // rejects require_passing_test = false as a config error -- but the
            // check is here too, because the one invariant worth duplicating is
            // the one that makes the rest trustworthy.
            if self.config.require_passing_test && !proof.passed {
                return Ok(Outcome::Refused {
                    attempts: rejections,
                });
            }

            // --- step 5: declare it ---
            let name = capability_name(intent, &steps);
            let unit = Unit {
                name: format!("omnia-cap-{name}"),
                description: summarise(intent),
                kind: if steps.iter().any(|s| s.part == "schedule") {
                    UnitKind::Scheduled
                } else {
                    UnitKind::OneShot
                },
                argv: steps
                    .iter()
                    .find(|s| !s.argv.is_empty())
                    .map(|s| s.argv.clone())
                    .unwrap_or_default(),
                permissions: permissions.clone(),
            };

            let deb = self
                .package(&name, &unit, &steps, &proof)
                .map_err(|e| ForgeError::Package(e.to_string()))?;

            let capability = Capability {
                name: name.clone(),
                version: "1.0.0".into(),
                intent: intent.to_string(),
                fingerprint: String::new(), // filled below from the steps
                permissions,
                steps: steps.clone(),
                package: format!("omnia-cap-{name}"),
                installed_at: omnia_registry::now_epoch_seconds(),
                provenance: Provenance {
                    intent: intent.to_string(),
                    tier: "orchestrator".into(),
                    model: self.planner.describe(),
                    attempts: attempt,
                },
                test: TestOutcome {
                    asserts: proof.asserts.clone(),
                    passed: proof.passed,
                    epoch_seconds: omnia_registry::now_epoch_seconds(),
                },
            };
            let capability = Capability {
                fingerprint: omnia_registry::fingerprint_of(&capability.steps),
                ..capability
            };

            self.registry
                .insert(capability.clone())
                .map_err(|e| ForgeError::Registry(e.to_string()))?;

            return Ok(Outcome::Built {
                capability: Box::new(capability),
                deb,
                proof,
            });
        }

        Ok(Outcome::Refused {
            attempts: rejections,
        })
    }

    fn missing_binaries(&self, steps: &[ResolvedStep]) -> Option<String> {
        for step in steps {
            let Ok(part) = self.catalog.get(&step.part) else {
                continue;
            };
            let missing = self.catalog.missing_binaries(part);
            if !missing.is_empty() {
                return Some(format!(
                    "'{}' needs {} which is not installed on this machine",
                    step.part,
                    missing.join(", ")
                ));
            }
        }
        None
    }

    fn package(
        &self,
        name: &str,
        unit: &Unit,
        steps: &[ResolvedStep],
        proof: &Proof,
    ) -> Result<std::path::PathBuf, PackageError> {
        let mut files = vec![
            (
                format!("/lib/systemd/system/{}.service", unit.name),
                unit.render_service(),
            ),
            (
                format!("/usr/share/omnia/capabilities/{name}/proof.txt"),
                format!("{}\n\n{}\n", proof.asserts, proof.detail),
            ),
            (
                format!("/usr/share/omnia/capabilities/{name}/plan.json"),
                serde_json::to_string_pretty(steps).unwrap_or_default(),
            ),
        ];

        if let Some(at) = steps
            .iter()
            .find(|s| s.part == "schedule")
            .and_then(|s| s.args.get("at"))
        {
            if let Some(timer) = unit.render_timer(&format!("*-*-* {at}:00")) {
                files.push((format!("/lib/systemd/system/{}.timer", unit.name), timer));
            }
        }

        PackageSpec {
            name: unit.name.clone(),
            version: "1.0.0".into(),
            description: unit.description.clone(),
            files,
            executable: Vec::new(),
        }
        .build(&self.workdir)
    }
}

/// A stable, readable package name derived from what the capability does.
///
/// Derived from the plan rather than the wording, so the same capability gets
/// the same name however it was asked for.
fn capability_name(intent: &str, steps: &[ResolvedStep]) -> String {
    let target = steps
        .iter()
        .find(|s| s.part == "snapshot")
        .and_then(|s| s.args.get("source"))
        .map(|source| {
            source
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("data")
                .to_lowercase()
        })
        .unwrap_or_else(|| "capability".into());

    let verb = if intent.to_lowercase().contains("back") {
        "backup"
    } else {
        "manage"
    };
    format!("{verb}-{}", slug(&target))
}

fn slug(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn summarise(intent: &str) -> String {
    let mut chars: Vec<char> = intent.trim().chars().collect();
    if let Some(first) = chars.first_mut() {
        *first = first.to_ascii_uppercase();
    }
    chars.into_iter().take(120).collect()
}

#[cfg(test)]
mod tests;
