//! `omni devices` and `omni fix` — the driver ladder, reachable.
//!
//! # Two commands because there are two questions
//!
//! *"What is on this machine and does it work?"* is a survey. It reads sysfs
//! and the module index, touches nothing, and is safe to run anywhere — which
//! is the point, because the alternative way to find out is to plug something
//! in and see.
//!
//! *"Fix this one"* loads kernel code and can write a device ID into a driver.
//! It is a separate word for the same reason `omni ask` is separate from
//! `omni capabilities`: the command that changes the machine should be the one
//! you typed on purpose.
//!
//! # Why a survey is not the ladder's trigger
//!
//! Running enumeration against a real machine reports around ten devices with
//! no driver on a perfectly healthy box — serial ports, a PC speaker, an RTC.
//! None of them is a problem. So `omni devices` reports and `omni fix` acts
//! only on what it is pointed at; the automatic path is a uevent, which carries
//! its own justification, and that arrives with the daemon.

use std::path::PathBuf;

use omnia_core::{Error, Result};
use omnia_forge::{Ladder, RealSystem, Rung};
use omnia_kernel::{device, Device, Expectation, Lookup, ModuleIndex};

use crate::context::Context;

/// `omni devices` — what hardware is here, and what the kernel makes of it.
pub fn survey(_context: &Context, json: bool) -> Result<i32> {
    let sys_root = PathBuf::from(device::SYS_ROOT);
    let index = ModuleIndex::for_running_kernel();
    let devices = omnia_kernel::enumerate(&sys_root);

    if json {
        let entries: Vec<_> = devices
            .iter()
            .map(|device| {
                serde_json::json!({
                    "name": device.name,
                    "subsystem": device.subsystem,
                    "id": device.id_pair(),
                    "driver": device.driver,
                    "state": state_word(device),
                    "description": device.describe(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "devices": entries,
                "module_index": index.source().map(|p| p.display().to_string()),
                "module_index_entries": index.len(),
            })
        );
        return Ok(0);
    }

    if devices.is_empty() {
        println!("No devices with a modalias. Is /sys mounted?");
        return Ok(0);
    }

    let unclaimed: Vec<&Device> = devices.iter().filter(|d| d.needs_a_driver()).collect();
    println!(
        "{} devices · {} working · {} without a driver\n",
        devices.len(),
        devices.len() - unclaimed.len(),
        unclaimed.len()
    );

    if unclaimed.is_empty() {
        println!("Everything that wants a driver has one.");
    } else {
        for device in &unclaimed {
            // What rung 1 would say, without doing anything about it. This is
            // the difference between "unsupported" and "just not loaded", and
            // it is the first thing anyone wants to know.
            // With no index, "no driver claims this" is not a finding, it is a
            // missing file. Saying it anyway is the exact mistake the ladder
            // refuses to make, and it should not leak into the survey either.
            let finding = match (&device.modalias, index.is_empty()) {
                (_, true) => "cannot tell: this machine has no module index".into(),
                (Some(alias), false) => index.lookup(alias).describe(),
                (None, false) => "the kernel publishes no modalias".into(),
            };
            println!("  {:<22} {}", device.name, device.describe());
            println!("  {:<22} {finding}", "");
            println!();
        }
        println!("omni fix <name>   to work down the ladder for one of them");
    }

    // A machine with no module index cannot conclude anything about what
    // drivers exist, and saying so beats reporting every device as unsupported.
    if index.is_empty() {
        println!(
            "\nnote: no module index at {}, so nothing above is a statement about\n\
             what drivers exist on this machine.",
            index
                .source()
                .map_or_else(|| "/lib/modules".into(), |p| p.display().to_string())
        );
    }
    Ok(0)
}

/// `omni fix <name>` — climb the ladder for one device.
pub fn fix(name: &str, context: &Context, json: bool, dry_run: bool) -> Result<i32> {
    let sys_root = PathBuf::from(device::SYS_ROOT);
    let devices = omnia_kernel::enumerate(&sys_root);
    let Some(device) = devices.iter().find(|d| d.name == name) else {
        return Err(not_found(name, &devices));
    };

    // Say nothing has to be done before doing anything, rather than climbing
    // and reporting a no-op at the end.
    match device.expectation() {
        Expectation::Bound(driver) => {
            println!("{} already works, driven by {driver}.", device.name);
            return Ok(0);
        }
        Expectation::NotExpected(reason) => {
            println!("{} needs no driver: {reason}", device.name);
            return Ok(0);
        }
        Expectation::Unclaimed => {}
    }

    let index = ModuleIndex::for_running_kernel();
    if index.is_empty() {
        return Err(Error::config_detail(
            "this machine has no module index",
            format!(
                "nothing can be concluded about what drivers exist, so climbing \
                 would be guesswork. Expected it at {}",
                index.source().map_or_else(
                    || "/lib/modules/<release>".into(),
                    |p| p.display().to_string()
                )
            ),
        ));
    }

    if dry_run {
        return Ok(explain(device, &index, json));
    }

    let system = RealSystem::new();
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: ceiling_for(&context.config.settings.general.profile),
    };
    let climb = ladder.climb(device);

    if json {
        println!(
            "{}",
            serde_json::json!({
                "device": climb.device,
                "resolved": climb.resolved,
                "needs_decision": climb.needs_a_decision(),
                "attempts": climb.attempts.iter().map(|attempt| serde_json::json!({
                    "rung": attempt.rung.number(),
                    "label": attempt.rung.label(),
                    "outcome": attempt.step.describe(),
                })).collect::<Vec<_>>(),
            })
        );
    } else {
        print!("{}", climb.describe());
        if let Some(declaration) = &climb.declaration {
            println!("\nto keep it across reboots, this would install:");
            for (path, _) in declaration.files() {
                println!("  {path}");
            }
        }
    }

    // A climb that needs a human is not a failure, but it is not done either,
    // and a script has to be able to tell the three apart.
    Ok(match (climb.succeeded(), climb.needs_a_decision()) {
        (true, _) => 0,
        (false, true) => 3,
        (false, false) => 1,
    })
}

/// What the ladder would try, without trying it.
///
/// Worth having as its own mode: the commands that load kernel code and write
/// device IDs are exactly the ones somebody wants to read before running.
fn explain(device: &Device, index: &ModuleIndex, json: bool) -> i32 {
    let Some(alias) = &device.modalias else {
        println!(
            "{} publishes no modalias; there is nothing to match on.",
            device.name
        );
        return 1;
    };

    let lookup = index.lookup(alias);
    let candidates = index.id_candidates(alias);

    if json {
        println!(
            "{}",
            serde_json::json!({
                "device": device.describe(),
                "rung_1": lookup.describe(),
                "rung_3": candidates.iter().map(|c| serde_json::json!({
                    "module": c.module,
                    "claims": c.claims,
                    "same_vendor": c.same_vendor,
                    "why": c.describe(),
                })).collect::<Vec<_>>(),
            })
        );
        return 0;
    }

    println!("{}\n", device.describe());
    println!("  rung 1  {}", lookup.describe());
    if matches!(lookup, Lookup::Loadable(_)) {
        println!("          would load it and check the device binds");
    }

    if candidates.is_empty() {
        println!("  rung 3  no driver wants a device like this one");
    } else {
        for candidate in &candidates {
            println!("  rung 3  {}", candidate.describe());
            println!(
                "          {}",
                if candidate.same_vendor {
                    "would be tried"
                } else {
                    "would be put to you rather than tried"
                }
            );
        }
    }
    println!("\nnothing was changed. Drop --dry-run to act.");
    0
}

/// How far a machine may climb unattended.
///
/// A server defaults lower than a workstation for the same reason its autonomy
/// does: the rungs that introduce new code are not something to reach on a
/// machine whose job is to stay exactly as it was on Tuesday.
fn ceiling_for(profile: &str) -> Rung {
    match profile {
        "server" => Rung::Firmware,
        "realtime" => Rung::Config,
        _ => Rung::Config,
    }
}

fn state_word(device: &Device) -> &'static str {
    match device.expectation() {
        Expectation::Bound(_) => "bound",
        Expectation::Unclaimed => "unclaimed",
        Expectation::NotExpected(_) => "not-expected",
    }
}

/// Not found, with the near miss named. A sysfs name is not something anyone
/// types from memory.
fn not_found(name: &str, known: &[Device]) -> Error {
    let suggestion = known
        .iter()
        .find(|device| device.name.contains(name) || name.contains(&device.name));
    match suggestion {
        Some(device) => Error::config_detail(
            format!("no device called '{name}'"),
            format!("did you mean '{}'?", device.name),
        ),
        None => Error::config_detail(
            format!("no device called '{name}'"),
            "omni devices lists what this machine has".to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_does_not_climb_into_the_rungs_that_write_code() {
        // The profile's autonomy level and its ladder ceiling are the same
        // judgement: a machine whose job is to stay as it was does not get to
        // introduce new code on its own.
        assert!(!ceiling_for("server").introduces_new_code());
        assert!(!ceiling_for("workstation").introduces_new_code());
        assert!(!ceiling_for("appliance").introduces_new_code());
        assert_eq!(ceiling_for("server"), Rung::Firmware);
    }

    #[test]
    fn an_unknown_profile_gets_the_cautious_default() {
        // A profile this build has not heard of must not be read as permission.
        assert_eq!(ceiling_for("something-new"), Rung::Config);
    }
}
