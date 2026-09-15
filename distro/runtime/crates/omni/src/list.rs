//! The read-only commands: the inbox, what this machine can do, and the parts
//! a plan may be built from.
//!
//! A system that installs software on its own has to be able to answer "what
//! is on my machine and why" without the user going digging. These commands
//! are that answer.

use omnia_core::{Error, Result};
use omnia_registry::Capability;

use crate::context::Context;
use crate::render;

/// `omni` with no arguments.
///
/// The inbox is one queue of events, actions taken and pending proposals. In
/// slice 1 the only producer is the forge, so what it renders is every
/// capability this machine built. The event feed and the approval queue arrive
/// with the autonomous daemon; until then the inbox says so rather than
/// implying an empty queue means an idle machine.
pub fn inbox(context: &Context, json: bool) -> Result<i32> {
    let now = omnia_registry::now_epoch_seconds();
    let mut capabilities = context.registry.all();
    capabilities.sort_by_key(|capability| std::cmp::Reverse(capability.installed_at));

    if json {
        let entries: Vec<_> = capabilities
            .iter()
            .map(|capability| {
                serde_json::json!({
                    "kind": "built",
                    "name": capability.name,
                    "version": capability.version,
                    "intent": capability.intent,
                    "at": capability.installed_at,
                    "test_passed": capability.test.passed,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({ "entries": entries, "needs_you": [] })
        );
        return Ok(0);
    }

    if capabilities.is_empty() {
        println!("Nothing yet. Ask for something: omni ask \"back up my photos nightly\"");
        return Ok(0);
    }

    println!(
        "{} · nothing needs you\n",
        render::plural(capabilities.len(), "thing happened", "things happened")
    );
    for capability in &capabilities {
        // A capability whose retained test is recorded as failing is the one
        // line in this list that matters, so it is marked here rather than
        // waiting for someone to run doctor.
        let marker = if capability.test.passed {
            "built"
        } else {
            "FAILING"
        };
        println!(
            "  {marker:<8}{} {} — {}   {}",
            capability.name,
            capability.version,
            capability.intent,
            render::relative(capability.installed_at, now)
        );
    }
    println!("\nomni capabilities  ·  omni doctor");
    Ok(0)
}

/// `omni capabilities` — what this machine has taught itself.
pub fn capabilities(context: &Context, json: bool) -> Result<i32> {
    let capabilities = context.registry.all();

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&capabilities)
                .map_err(|e| Error::config(format!("cannot render capabilities: {e}")))?
        );
        return Ok(0);
    }

    if capabilities.is_empty() {
        println!("This machine has not built anything yet.");
        return Ok(0);
    }

    println!(
        "{}\n",
        render::plural(capabilities.len(), "capability", "capabilities")
    );
    for capability in capabilities {
        print!("{}", render::capability_detail(capability));
        println!();
    }
    Ok(0)
}

/// `omni show <name>` — one capability, including what it actually runs.
pub fn show(name: &str, context: &Context, json: bool) -> Result<i32> {
    let Some(capability) = context.registry.get(name) else {
        return Err(not_found(name, &context.registry.all()));
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(capability)
                .map_err(|e| Error::config(format!("cannot render {name}: {e}")))?
        );
        return Ok(0);
    }

    print!("{}", render::capability_detail(capability));
    println!("\n  what it runs");
    print!("{}", render::capability_steps(capability));
    println!("\n  to remove it:  sudo apt remove {}", capability.package);
    Ok(0)
}

/// `omni parts` — the vetted library a plan may draw from.
///
/// Worth showing because of what the parts library *is*: permissions are
/// derived from these manifests, never declared by the model, so this list is
/// the actual bound on what any generated capability can touch.
pub fn parts(context: &Context, json: bool) -> Result<i32> {
    let names = context.catalog.names();

    if json {
        let entries: Vec<_> = names
            .iter()
            .filter_map(|name| context.catalog.get(name).ok())
            .map(|part| {
                serde_json::json!({
                    "name": part.name,
                    "version": part.version,
                    "summary": part.summary,
                    "requires": part.requires,
                    "missing": context.catalog.missing_binaries(part),
                    "params": part.params.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "parts": entries }));
        return Ok(0);
    }

    println!(
        "{} ({} from disk, the rest built in)\n",
        render::plural(names.len(), "part", "parts"),
        context.installed_parts
    );

    for name in &names {
        let Ok(part) = context.catalog.get(name) else {
            continue;
        };
        println!("  {} {} — {}", part.name, part.version, part.summary);
        for param in &part.params {
            let requirement = if param.required {
                "required"
            } else {
                "optional"
            };
            println!("    {} ({requirement}) — {}", param.name, param.doc);
        }
        let missing = context.catalog.missing_binaries(part);
        if !missing.is_empty() {
            println!(
                "    unusable here: needs {} which is not installed",
                missing.join(", ")
            );
        }
        println!("    reach: {}", render::reach(&part.permissions));
        println!();
    }
    Ok(0)
}

/// Not found, with the near miss named if there is one. A capability name is
/// something the machine chose, so getting it slightly wrong is normal.
fn not_found(name: &str, known: &[&Capability]) -> Error {
    let suggestion = known
        .iter()
        .find(|capability| capability.name.contains(name) || name.contains(&capability.name));
    match suggestion {
        Some(capability) => Error::config_detail(
            format!("no capability called '{name}'"),
            format!("did you mean '{}'?", capability.name),
        ),
        None => Error::config_detail(
            format!("no capability called '{name}'"),
            "omni capabilities lists what this machine has".to_string(),
        ),
    }
}
