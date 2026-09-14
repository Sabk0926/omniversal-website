//! Turning records into something a person can read at a glance.
//!
//! One rule throughout: say what happened and what it means, never both in a
//! paragraph. A machine that changes your computer on its own owes you output
//! you can skim at 3am.

use std::fmt::Write as _;

use omnia_parts::Permissions;
use omnia_registry::Capability;

/// How long ago, in the units a human would use.
///
/// A clock skew or a record written on a machine whose clock later moved back
/// yields a future timestamp; that reads as "just now" rather than as a
/// negative age, because a wrong-looking age is a distraction, not a finding.
pub fn relative(epoch_seconds: u64, now: u64) -> String {
    if epoch_seconds >= now {
        return "just now".into();
    }
    let seconds = now - epoch_seconds;
    match seconds {
        0..=89 => "just now".into(),
        90..=5399 => format!("{}m ago", seconds / 60),
        5400..=172_799 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86400),
    }
}

/// What a capability can touch, on one line.
///
/// This is the sentence that makes an autonomous build reviewable, so it lists
/// every path rather than summarising: "and 3 more" is where a bad permission
/// hides.
pub fn reach(permissions: &Permissions) -> String {
    let mut parts = Vec::new();
    if !permissions.read_paths.is_empty() {
        parts.push(format!("reads {}", permissions.read_paths.join(", ")));
    }
    if !permissions.write_paths.is_empty() {
        parts.push(format!("writes {}", permissions.write_paths.join(", ")));
    }
    parts.push(if permissions.network {
        "network allowed".into()
    } else {
        "no network".into()
    });
    parts.join(" · ")
}

/// A label-and-value block, aligned so the values line up.
pub fn field(out: &mut String, label: &str, value: &str) {
    let _ = writeln!(out, "    {label:<11} {value}");
}

/// One capability in full: what it does, where it came from, what it proved.
pub fn capability_detail(capability: &Capability) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "  {} {}", capability.name, capability.version);
    field(&mut out, "asked for", &capability.provenance.intent);
    field(&mut out, "planned by", &capability.provenance.model);
    field(
        &mut out,
        "built",
        &format!(
            "{} ({})",
            omnia_registry::format_utc(capability.installed_at),
            attempts(capability.provenance.attempts)
        ),
    );
    field(
        &mut out,
        "proves",
        &format!(
            "{} — {}",
            capability.test.asserts,
            if capability.test.passed {
                "passed"
            } else {
                "FAILING"
            }
        ),
    );
    field(&mut out, "reaches", &reach(&capability.permissions));
    field(&mut out, "package", &capability.package);

    let plan = capability
        .steps
        .iter()
        .map(|step| step.part.as_str())
        .collect::<Vec<_>>()
        .join(" → ");
    field(&mut out, "plan", &plan);
    out
}

/// The full plan, one argv per line. Only `omni show` prints this: it is the
/// answer to "what does it actually run", which nobody wants in a list but
/// everybody wants once.
pub fn capability_steps(capability: &Capability) -> String {
    let mut out = String::new();
    for step in &capability.steps {
        let _ = writeln!(out, "    {}", step.part);
        if !step.argv.is_empty() {
            let _ = writeln!(out, "      {}", step.argv.join(" "));
        }
        for (key, value) in &step.args {
            let _ = writeln!(out, "      {key} = {value}");
        }
    }
    out
}

fn attempts(count: u32) -> String {
    match count {
        1 => "first attempt".into(),
        n => format!("{n} attempts"),
    }
}

/// English pluralisation for the handful of counted nouns in the output.
pub fn plural(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("{count} {singular}")
    } else {
        format!("{count} {plural}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_read_the_way_a_person_would_say_them() {
        let now = 1_000_000;
        assert_eq!(relative(now, now), "just now");
        assert_eq!(relative(now - 30, now), "just now");
        assert_eq!(relative(now - 600, now), "10m ago");
        assert_eq!(relative(now - 7200, now), "2h ago");
        assert_eq!(relative(now - 172_800, now), "2d ago");
    }

    #[test]
    fn a_future_timestamp_does_not_underflow() {
        // Clocks move backwards on boards without an RTC. A panic here would
        // take out the inbox on exactly the machines that need it most.
        assert_eq!(relative(2_000_000, 1_000_000), "just now");
    }

    #[test]
    fn reach_names_every_path_rather_than_summarising() {
        let permissions = Permissions {
            read_paths: vec!["/home/a/Pictures".into(), "/var/backups/pictures".into()],
            write_paths: vec!["/var/backups/pictures".into()],
            network: false,
        };
        let text = reach(&permissions);
        assert!(text.contains("/home/a/Pictures"));
        assert!(text.contains("/var/backups/pictures"));
        assert!(text.contains("no network"));
        assert!(!text.contains("more"), "nothing is elided: {text}");
    }

    #[test]
    fn empty_permissions_still_state_the_network_position() {
        // "nothing listed" must not be mistaken for "unknown".
        let text = reach(&Permissions::default());
        assert_eq!(text, "no network");
    }

    #[test]
    fn plurals_are_not_reported_as_one_s() {
        assert_eq!(plural(1, "capability", "capabilities"), "1 capability");
        assert_eq!(plural(0, "capability", "capabilities"), "0 capabilities");
        assert_eq!(plural(3, "capability", "capabilities"), "3 capabilities");
    }

    #[test]
    fn a_single_attempt_is_not_reported_as_one_attempts() {
        assert_eq!(attempts(1), "first attempt");
        assert_eq!(attempts(3), "3 attempts");
    }
}
