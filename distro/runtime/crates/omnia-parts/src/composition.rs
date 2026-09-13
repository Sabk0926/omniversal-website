//! What the model produces, and how it is checked before anything is built.
//!
//! The model emits a [`Composition`]: an ordered list of parts with arguments.
//! [`Catalog::resolve`] turns it into [`ResolvedStep`]s or rejects it. Every
//! rejection carries enough detail to be fed straight back as a retry prompt,
//! because "invalid" on its own gives a 3-4B model nothing to correct.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::catalog::Catalog;
use crate::manifest::{substitute, ExecKind, Permissions};
use crate::{PartsError, Result};

/// One step as the model wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Step {
    pub part: String,
    #[serde(default)]
    pub args: BTreeMap<String, String>,
}

/// A whole plan as the model wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Composition {
    pub steps: Vec<Step>,
}

impl Composition {
    /// Parse the model's JSON. Kept here so the caller does not need serde_json
    /// and so the error text is shaped for a retry prompt.
    pub fn from_json(text: &str) -> std::result::Result<Composition, String> {
        serde_json::from_str(text).map_err(|e| format!("plan is not valid JSON: {e}"))
    }
}

/// One step after checking, with its reach computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedStep {
    pub part: String,
    pub kind: ExecKind,
    /// Fully substituted argv. Empty for non-command parts such as `schedule`.
    pub argv: Vec<String>,
    pub args: BTreeMap<String, String>,
    /// What this step may touch, derived from the manifest and these args.
    pub permissions: Permissions,
}

impl Catalog {
    /// Check a composition and compute what it may touch.
    ///
    /// The resulting permissions are the union of the parts' declared reach
    /// with arguments substituted in. Nothing the model wrote contributes a
    /// permission directly, which is what bounds a generated capability to the
    /// catalogue rather than to the model's judgement.
    pub fn resolve(&self, composition: &Composition) -> Result<Vec<ResolvedStep>> {
        if composition.steps.is_empty() {
            return Err(PartsError::EmptyComposition);
        }

        let mut resolved = Vec::with_capacity(composition.steps.len());
        for step in &composition.steps {
            let part = self.get(&step.part)?;

            for name in step.args.keys() {
                if part.param(name).is_none() {
                    return Err(PartsError::UnknownArgument {
                        part: part.name.clone(),
                        named: name.clone(),
                        expected: part.param_names(),
                    });
                }
            }

            // Build the effective argument set: what was given, plus defaults.
            let mut effective: BTreeMap<String, String> = BTreeMap::new();
            for param in &part.params {
                match step.args.get(&param.name) {
                    Some(value) => {
                        part.validate_value(param, value)?;
                        effective.insert(param.name.clone(), value.clone());
                    }
                    None => {
                        if param.required {
                            return Err(PartsError::MissingArgument {
                                part: part.name.clone(),
                                param: param.name.clone(),
                                doc: param.doc.clone(),
                            });
                        }
                        if let Some(default) = &param.default {
                            effective.insert(param.name.clone(), default.clone());
                        }
                    }
                }
            }

            let lookup = |name: &str| effective.get(name).cloned();
            let expand = |templates: &[String]| -> Vec<String> {
                templates.iter().map(|t| substitute(t, &lookup)).collect()
            };

            resolved.push(ResolvedStep {
                part: part.name.clone(),
                kind: part.exec.kind,
                argv: expand(&part.exec.argv),
                permissions: Permissions {
                    read_paths: expand(&part.permissions.read_paths),
                    write_paths: expand(&part.permissions.write_paths),
                    network: part.permissions.network,
                },
                args: effective,
            });
        }
        Ok(resolved)
    }
}

/// The union of what a whole plan may touch.
pub fn total_permissions(steps: &[ResolvedStep]) -> Permissions {
    let mut total = Permissions::default();
    for step in steps {
        for path in &step.permissions.read_paths {
            if !total.read_paths.contains(path) {
                total.read_paths.push(path.clone());
            }
        }
        for path in &step.permissions.write_paths {
            if !total.write_paths.contains(path) {
                total.write_paths.push(path.clone());
            }
        }
        total.network |= step.permissions.network;
    }
    total.read_paths.sort();
    total.write_paths.sort();
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(json: &str) -> Composition {
        Composition::from_json(json).expect("test plan must parse")
    }

    fn backup_plan() -> Composition {
        plan(
            r#"{"steps":[
                {"part":"snapshot","args":{"source":"~/Pictures","destination":"/var/backups/pictures"}},
                {"part":"schedule","args":{"at":"02:00"}},
                {"part":"verify-restore","args":{"archive":"/var/backups/pictures","original":"~/Pictures"}}
            ]}"#,
        )
    }

    #[test]
    fn the_backup_plan_resolves() {
        let catalog = Catalog::builtin().unwrap();
        let steps = catalog.resolve(&backup_plan()).expect("plan must resolve");
        assert_eq!(steps.len(), 3);
        assert_eq!(
            steps[0].argv,
            vec![
                "rsync",
                "--archive",
                "--delete",
                "~/Pictures/",
                "/var/backups/pictures/"
            ]
        );
    }

    #[test]
    fn permissions_are_derived_from_the_parts_not_the_model() {
        let catalog = Catalog::builtin().unwrap();
        let steps = catalog.resolve(&backup_plan()).unwrap();
        let total = total_permissions(&steps);

        assert_eq!(total.write_paths, vec!["/var/backups/pictures"]);
        assert_eq!(
            total.read_paths,
            vec!["/var/backups/pictures", "~/Pictures"]
        );
        assert!(!total.network, "nothing in this plan needs the network");
    }

    #[test]
    fn defaults_are_applied() {
        let catalog = Catalog::builtin().unwrap();
        let steps = catalog
            .resolve(&plan(
                r#"{"steps":[{"part":"schedule","args":{"at":"02:00"}}]}"#,
            ))
            .unwrap();
        assert_eq!(
            steps[0].args.get("frequency").map(String::as_str),
            Some("daily")
        );
    }

    #[test]
    fn an_invented_part_is_rejected_with_the_real_list() {
        let catalog = Catalog::builtin().unwrap();
        let err = catalog
            .resolve(&plan(
                r#"{"steps":[{"part":"encrypt-and-upload","args":{}}]}"#,
            ))
            .unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("encrypt-and-upload") && text.contains("snapshot"),
            "{text}"
        );
    }

    #[test]
    fn a_missing_required_argument_names_it_and_explains_it() {
        let catalog = Catalog::builtin().unwrap();
        let err = catalog
            .resolve(&plan(
                r#"{"steps":[{"part":"snapshot","args":{"source":"~/Pictures"}}]}"#,
            ))
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("destination"), "{text}");
        assert!(
            text.contains("copy to"),
            "carries the doc for the retry: {text}"
        );
    }

    #[test]
    fn an_invented_argument_is_rejected_with_the_real_ones() {
        let catalog = Catalog::builtin().unwrap();
        let err = catalog
            .resolve(&plan(
                r#"{"steps":[{"part":"schedule","args":{"at":"02:00","timezone":"UTC"}}]}"#,
            ))
            .unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("timezone") && text.contains("frequency"),
            "{text}"
        );
    }

    #[test]
    fn path_traversal_in_an_argument_is_refused() {
        // The model asking to back up ~/../../etc must not widen the sandbox.
        let catalog = Catalog::builtin().unwrap();
        let err = catalog
            .resolve(&plan(
                r#"{"steps":[{"part":"snapshot","args":{"source":"~/../../etc","destination":"/tmp/x"}}]}"#,
            ))
            .unwrap_err();
        assert!(err.to_string().contains(".."), "{err}");
    }

    #[test]
    fn shell_metacharacters_are_inert_because_there_is_no_shell() {
        // A path containing '; rm -rf /' is just an odd directory name: it
        // becomes one argv element and is never re-parsed.
        let catalog = Catalog::builtin().unwrap();
        let steps = catalog
            .resolve(&plan(
                r#"{"steps":[{"part":"snapshot","args":{"source":"~/Pictures","destination":"/tmp/a; rm -rf /"}}]}"#,
            ))
            .unwrap();
        assert_eq!(steps[0].argv[4], "/tmp/a; rm -rf //");
        assert_eq!(
            steps[0].argv.len(),
            5,
            "still five argv elements, not a shell line"
        );
    }

    #[test]
    fn a_bad_time_is_rejected() {
        let catalog = Catalog::builtin().unwrap();
        let err = catalog
            .resolve(&plan(
                r#"{"steps":[{"part":"schedule","args":{"at":"2am"}}]}"#,
            ))
            .unwrap_err();
        assert!(err.to_string().contains("HH:MM"), "{err}");
    }

    #[test]
    fn an_out_of_range_enum_is_rejected_with_the_options() {
        let catalog = Catalog::builtin().unwrap();
        let err = catalog
            .resolve(&plan(
                r#"{"steps":[{"part":"schedule","args":{"at":"02:00","frequency":"fortnightly"}}]}"#,
            ))
            .unwrap_err();
        assert!(err.to_string().contains("hourly, daily, weekly"), "{err}");
    }

    #[test]
    fn an_empty_plan_is_rejected() {
        let catalog = Catalog::builtin().unwrap();
        assert_eq!(
            catalog.resolve(&plan(r#"{"steps":[]}"#)).unwrap_err(),
            PartsError::EmptyComposition
        );
    }

    #[test]
    fn malformed_json_explains_itself() {
        let err = Composition::from_json("{not json").unwrap_err();
        assert!(err.contains("not valid JSON"), "{err}");
    }
}
