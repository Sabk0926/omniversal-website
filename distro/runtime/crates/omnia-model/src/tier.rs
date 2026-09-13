//! Model tiers.
//!
//! A tier is a role, not a file name. Callers ask for the capability they need
//! and the server resolves it to whatever is resident, so a request written on
//! a workstation runs unchanged on a board with a smaller model.

use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// ~1.5B, initramfs-resident. Boot-failure triage before the rootfs is up.
    Micro,
    /// 3-4B. The floor. Everything the OS depends on must work here.
    Orchestrator,
    /// 7B+, loaded on demand. Harder planning, driver generation.
    Desktop,
    /// Off-box. Opt-in, off by default.
    Cloud,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Micro => "micro",
            Tier::Orchestrator => "orchestrator",
            Tier::Desktop => "desktop",
            Tier::Cloud => "cloud",
        }
    }

    /// The next tier up, for escalation. `None` at the top -- escalating past
    /// `Cloud` is not a thing, and returning `Cloud` again would loop.
    pub fn escalate(self) -> Option<Tier> {
        match self {
            Tier::Micro => Some(Tier::Orchestrator),
            Tier::Orchestrator => Some(Tier::Desktop),
            Tier::Desktop => Some(Tier::Cloud),
            Tier::Cloud => None,
        }
    }

    /// Does reaching this tier send bytes off the machine? Callers gate on
    /// this rather than string-matching the name.
    pub fn leaves_the_machine(self) -> bool {
        matches!(self, Tier::Cloud)
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Tier {
    type Err = String;

    fn from_str(raw: &str) -> Result<Tier, String> {
        match raw.to_lowercase().as_str() {
            "micro" => Ok(Tier::Micro),
            "orchestrator" => Ok(Tier::Orchestrator),
            "desktop" => Ok(Tier::Desktop),
            "cloud" => Ok(Tier::Cloud),
            other => Err(format!(
                "unknown tier '{other}' (expected micro, orchestrator, desktop or cloud)"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escalation_walks_up_and_stops() {
        assert_eq!(Tier::Micro.escalate(), Some(Tier::Orchestrator));
        assert_eq!(Tier::Orchestrator.escalate(), Some(Tier::Desktop));
        assert_eq!(Tier::Desktop.escalate(), Some(Tier::Cloud));
        assert_eq!(
            Tier::Cloud.escalate(),
            None,
            "escalating past cloud would loop"
        );
    }

    #[test]
    fn only_cloud_leaves_the_machine() {
        assert!(!Tier::Micro.leaves_the_machine());
        assert!(!Tier::Orchestrator.leaves_the_machine());
        assert!(!Tier::Desktop.leaves_the_machine());
        assert!(Tier::Cloud.leaves_the_machine());
    }

    #[test]
    fn tiers_order_by_capability() {
        assert!(Tier::Desktop > Tier::Orchestrator);
        assert!(Tier::Orchestrator > Tier::Micro);
    }

    #[test]
    fn parsing_is_case_insensitive_and_errors_helpfully() {
        assert_eq!("ORCHESTRATOR".parse::<Tier>().unwrap(), Tier::Orchestrator);
        let err = "huge".parse::<Tier>().unwrap_err();
        assert!(
            err.contains("huge") && err.contains("orchestrator"),
            "{err}"
        );
    }
}
