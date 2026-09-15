//! The record of one capability: what it is, what it does, and why it exists.

use serde::{Deserialize, Serialize};

use omnia_parts::{Permissions, ResolvedStep};

/// What the generated test actually proved.
///
/// Stored rather than recomputed, because "this passed when installed" and
/// "this passes now" are different claims and both matter: the first is
/// provenance, the second is what `omni doctor` re-establishes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestOutcome {
    /// What the test asserts, in a sentence a human can check.
    pub asserts: String,
    pub passed: bool,
    pub epoch_seconds: u64,
}

/// Where a capability came from. This is the answer to "why is this on my
/// machine", which a system that installs things on its own owes the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// The request, in the words it was asked in.
    pub intent: String,
    /// Which tier planned it.
    pub tier: String,
    /// The model that produced the plan, as it identified itself.
    pub model: String,
    /// How many plan-and-test rounds it took. More than one is worth knowing.
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    pub version: String,
    /// The original request. Duplicated from provenance for cheap text search.
    pub intent: String,
    /// Hash of the resolved composition. The dedup key.
    pub fingerprint: String,
    pub steps: Vec<ResolvedStep>,
    pub permissions: Permissions,
    /// The `.deb` that owns the installed files. Removal goes through apt, not
    /// through this registry -- the package manager is the source of truth for
    /// what is on disk.
    pub package: String,
    pub installed_at: u64,
    pub provenance: Provenance,
    pub test: TestOutcome,
}

impl Capability {
    /// A one-line summary for `omni capabilities`.
    pub fn describe(&self) -> String {
        format!(
            "{} {} — {} (built {}, {})",
            self.name,
            self.version,
            self.intent,
            crate::time::format_utc(self.installed_at),
            if self.test.passed {
                "test passed"
            } else {
                "TEST FAILING"
            },
        )
    }
}

/// Fingerprint the resolved composition.
///
/// Order-sensitive on purpose: `snapshot` then `schedule` is a different
/// capability from `schedule` then `snapshot`, even with identical arguments.
/// Arguments are already a BTreeMap, so their order is stable without sorting.
pub fn fingerprint(steps: &[ResolvedStep]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut absorb = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
    };
    for step in steps {
        absorb(step.part.as_bytes());
        absorb(b"\x1f");
        for (key, value) in &step.args {
            absorb(key.as_bytes());
            absorb(b"=");
            absorb(value.as_bytes());
            absorb(b"\x1f");
        }
        absorb(b"\x1e");
    }
    format!("{hash:016x}")
}

/// The shape of a plan: its parts in order, arguments ignored.
///
/// Two plans with the same shape are the same *kind* of capability, which is
/// usually an update rather than a new thing -- backing up a different folder
/// is still "a backup", and the caller may want to say so.
pub fn shape(steps: &[ResolvedStep]) -> Vec<String> {
    steps.iter().map(|s| s.part.clone()).collect()
}

pub fn shape_of(steps: &[ResolvedStep]) -> Vec<String> {
    shape(steps)
}
