//! Turning an intent into a composition.
//!
//! Two implementations, behind one trait, for a specific reason: the plumbing
//! and the prompt are separate risks and should fail separately. With a stub
//! planner the whole pipeline runs deterministically, so a failure is a code
//! bug. With the model planner, a failure that the stub does not reproduce is a
//! prompt bug. Tangling the two makes every failure ambiguous.

use omnia_model::{ModelClient, Prompt, Tier};
use omnia_parts::{Catalog, Composition};

/// How a plan was produced, recorded in provenance.
pub trait Planner {
    /// Produce a composition. `feedback` carries the previous attempt's
    /// rejection, so the planner can correct rather than guess again.
    fn plan(
        &self,
        intent: &str,
        catalog: &Catalog,
        feedback: Option<&str>,
    ) -> Result<Composition, String>;

    /// Identifies the planner in the capability's provenance record.
    fn describe(&self) -> String;
}

/// A deterministic planner that recognises the shapes slice 1 ships parts for.
///
/// Not a mock: it is a real, selectable planner (`--planner=stub`) so the
/// pipeline can be exercised, demonstrated and regression-tested on a machine
/// with no model installed at all. Every capability it produces goes through
/// exactly the same validation, sandboxing, proving and packaging as one the
/// model planned.
#[derive(Debug, Clone, Default)]
pub struct StubPlanner;

impl Planner for StubPlanner {
    fn plan(
        &self,
        intent: &str,
        _catalog: &Catalog,
        _feedback: Option<&str>,
    ) -> Result<Composition, String> {
        let lower = intent.to_lowercase();
        let backup = lower.contains("back up") || lower.contains("backup");
        if !backup {
            return Err(format!(
                "the stub planner only understands backup requests; '{intent}' is not one. \
                 Use --planner=model for anything else."
            ));
        }

        let (source, name) = if lower.contains("document") {
            ("~/Documents", "documents")
        } else {
            ("~/Pictures", "pictures")
        };
        let at = if lower.contains("night") || lower.contains("nightly") {
            "02:00"
        } else {
            "03:00"
        };
        let destination = format!("/var/backups/{name}");

        let json = format!(
            r#"{{"steps":[
                {{"part":"snapshot","args":{{"source":"{source}","destination":"{destination}"}}}},
                {{"part":"schedule","args":{{"at":"{at}"}}}},
                {{"part":"verify-restore","args":{{"archive":"{destination}","original":"{source}"}}}}
            ]}}"#
        );
        Composition::from_json(&json)
    }

    fn describe(&self) -> String {
        "stub planner (deterministic, no model)".into()
    }
}

/// Asks the local model for a composition.
pub struct ModelPlanner {
    client: ModelClient,
    tier: Tier,
    system_facts: String,
}

impl ModelPlanner {
    pub fn new(client: ModelClient, tier: Tier, system_facts: String) -> ModelPlanner {
        ModelPlanner {
            client,
            tier,
            system_facts,
        }
    }

    /// Build the prompt, keeping everything reusable in the stable half.
    ///
    /// The catalogue and the instructions are identical on every request, which
    /// is exactly the large block the KV cache should absorb. Only the intent
    /// and any retry feedback go in the volatile half.
    fn build_prompt(&self, intent: &str, catalog: &Catalog, feedback: Option<&str>) -> Prompt {
        let prompt = Prompt::new()
            .stable("System", &self.system_facts)
            .stable("Available parts", &catalog.render_for_prompt())
            .stable(
                "Instructions",
                "Answer with JSON only, no prose and no code fences.\n\
                 Shape: {\"steps\":[{\"part\":\"NAME\",\"args\":{\"KEY\":\"VALUE\"}}]}\n\
                 Use only the parts listed above. Use only their documented parameters.\n\
                 Paths must be absolute or begin with ~/.\n\
                 A capability that writes data must include verify-restore so the \
                 result can be proven.",
            )
            .volatile("Request", intent);

        match feedback {
            Some(feedback) => prompt.volatile(
                "Your previous attempt was rejected",
                &format!("{feedback}\n\nCorrect it and answer with JSON only."),
            ),
            None => prompt,
        }
    }
}

impl Planner for ModelPlanner {
    fn plan(
        &self,
        intent: &str,
        catalog: &Catalog,
        feedback: Option<&str>,
    ) -> Result<Composition, String> {
        let prompt = self.build_prompt(intent, catalog, feedback);
        let completion = self.client.complete(&prompt).map_err(|e| e.to_string())?;

        // A plan cut off at the token limit is not a plan. Saying so beats
        // handing a truncated brace to a JSON parser and reporting a syntax
        // error the model cannot act on.
        if completion.truncated {
            return Err("the plan was cut off at the token limit; it is incomplete".into());
        }

        omnia_core::debug!("forge", "plan: {}", completion.stats.describe());
        Composition::from_json(extract_json(&completion.text))
    }

    fn describe(&self) -> String {
        format!("model planner ({} tier)", self.tier)
    }
}

/// Pull the JSON object out of a completion.
///
/// Small models wrap JSON in code fences or prose despite instructions. Fishing
/// out the outermost braces costs nothing and removes a whole class of retry.
fn extract_json(text: &str) -> &str {
    let trimmed = text.trim();
    match (trimmed.find('{'), trimmed.rfind('}')) {
        (Some(start), Some(end)) if end > start => &trimmed[start..=end],
        _ => trimmed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stub_plans_a_backup() {
        let catalog = Catalog::builtin().unwrap();
        let plan = StubPlanner
            .plan("back up my photos every night", &catalog, None)
            .unwrap();
        assert_eq!(plan.steps.len(), 3);
        assert_eq!(plan.steps[0].part, "snapshot");
        assert_eq!(plan.steps[0].args.get("source").unwrap(), "~/Pictures");
        assert_eq!(plan.steps[1].args.get("at").unwrap(), "02:00");
    }

    #[test]
    fn the_stub_refuses_what_it_does_not_understand_and_says_so() {
        let catalog = Catalog::builtin().unwrap();
        let err = StubPlanner
            .plan("set up a web server", &catalog, None)
            .unwrap_err();
        assert!(
            err.contains("--planner=model"),
            "points at the real planner: {err}"
        );
    }

    #[test]
    fn json_is_extracted_from_code_fences_and_prose() {
        assert_eq!(extract_json("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(
            extract_json("Sure! Here you go: {\"a\":1} Hope that helps."),
            "{\"a\":1}"
        );
        assert_eq!(extract_json("{\"a\":1}"), "{\"a\":1}");
    }

    #[test]
    fn the_prompt_keeps_the_catalogue_in_the_cacheable_half() {
        // The catalogue and instructions are identical every request; putting
        // them in the volatile half would re-prefill thousands of tokens.
        let catalog = Catalog::builtin().unwrap();
        let planner = ModelPlanner::new(
            ModelClient::from_config(&omnia_core::Config::builtin().settings.model),
            Tier::Orchestrator,
            "Ubuntu 24.04, aarch64".into(),
        );
        let prompt = planner.build_prompt("back up my photos", &catalog, None);
        assert!(
            prompt.stable_text().contains("snapshot"),
            "catalogue must be cacheable"
        );
        assert!(prompt.volatile_text().contains("back up my photos"));
        assert!(
            !prompt.stable_text().contains("back up my photos"),
            "intent must not be cached"
        );
        assert!(
            prompt.check_stability().is_empty(),
            "no clocks in the cacheable half"
        );
    }

    #[test]
    fn retry_feedback_goes_in_the_volatile_half_only() {
        let catalog = Catalog::builtin().unwrap();
        let planner = ModelPlanner::new(
            ModelClient::from_config(&omnia_core::Config::builtin().settings.model),
            Tier::Orchestrator,
            "Ubuntu 24.04".into(),
        );
        let first = planner.build_prompt("back up photos", &catalog, None);
        let retry = planner.build_prompt(
            "back up photos",
            &catalog,
            Some("no part called 'archive-all'"),
        );
        assert_eq!(
            first.stable_fingerprint(),
            retry.stable_fingerprint(),
            "a retry must not invalidate the cached prefix"
        );
        assert!(retry.volatile_text().contains("archive-all"));
    }
}
