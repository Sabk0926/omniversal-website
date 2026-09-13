//! Prompt assembly, split so the KV cache can actually be reused.
//!
//! This is the single highest-leverage thing in the runtime. Our workload is
//! long-context and short-output: a few thousand tokens of system state in, a
//! short plan out. That makes it prefill-dominated, and prefill is compute
//! bound. On a CPU-only board a 3k-token prefill is on the order of a minute
//! before the first token appears.
//!
//! llama.cpp's server reuses the KV cache for the longest common *prefix* of
//! consecutive requests when `cache_prompt` is set. The catch is that "common
//! prefix" means byte-identical: one changed character anywhere in the prefix
//! and everything after it is recomputed.
//!
//! So a prompt is built in two halves that are never mixed:
//!
//! * **stable** -- facts that do not change between requests on this machine:
//!   OS release, hardware, the parts catalogue, the system instructions.
//! * **volatile** -- the request itself and anything time-varying.
//!
//! Anything with a timestamp, a PID, a random id or a duration belongs in the
//! volatile half. Putting a clock in the stable half silently disables the
//! cache and nothing visibly breaks -- it just gets slow, which is exactly the
//! failure this module exists to prevent. `Prompt::check_stability` catches
//! the common cases.

use std::fmt::Write as _;

/// A prompt split into a cacheable prefix and a per-request remainder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    stable: String,
    volatile: String,
}

/// Patterns that must never appear in the stable half, with the reason.
/// Matching is deliberately crude: false positives cost a developer one
/// rename, a false negative costs every user a minute per request.
const UNSTABLE_MARKERS: &[(&str, &str)] = &[
    ("T00:", "a timestamp"),
    ("T01:", "a timestamp"),
    ("T02:", "a timestamp"),
    ("uptime", "uptime"),
    ("pid=", "a process id"),
    ("elapsed", "an elapsed time"),
    ("seconds ago", "a relative time"),
    ("ms)", "a duration"),
];

impl Prompt {
    pub fn new() -> Prompt {
        Prompt {
            stable: String::new(),
            volatile: String::new(),
        }
    }

    /// Append to the cacheable prefix. Only facts that are identical on the
    /// next request belong here.
    pub fn stable(mut self, section: &str, body: &str) -> Prompt {
        let _ = writeln!(self.stable, "## {section}\n{}\n", body.trim_end());
        self
    }

    /// Append to the per-request remainder.
    pub fn volatile(mut self, section: &str, body: &str) -> Prompt {
        let _ = writeln!(self.volatile, "## {section}\n{}\n", body.trim_end());
        self
    }

    pub fn stable_text(&self) -> &str {
        &self.stable
    }

    pub fn volatile_text(&self) -> &str {
        &self.volatile
    }

    /// The full prompt. The stable half always comes first -- that ordering is
    /// what makes it a prefix, and reversing it would silently kill the cache.
    pub fn render(&self) -> String {
        format!("{}{}", self.stable, self.volatile)
    }

    /// A cheap fingerprint of the stable half.
    ///
    /// Not cryptographic and does not need to be: it only answers "is this the
    /// same prefix as last time", so a collision costs one wasted prefill.
    pub fn stable_fingerprint(&self) -> u64 {
        fnv1a(self.stable.as_bytes())
    }

    /// Report anything in the stable half that looks time-varying.
    ///
    /// Returns the reasons found. Empty means it looks cacheable. Callers log
    /// these loudly rather than failing: a degraded cache is slow, not wrong,
    /// and refusing to answer would be the worse outcome.
    pub fn check_stability(&self) -> Vec<String> {
        let lower = self.stable.to_lowercase();
        UNSTABLE_MARKERS
            .iter()
            .filter(|(marker, _)| lower.contains(&marker.to_lowercase()))
            .map(|(marker, reason)| {
                format!("stable prefix contains {reason} ({marker:?}); this disables the KV cache")
            })
            .collect()
    }
}

impl Default for Prompt {
    fn default() -> Self {
        Prompt::new()
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_half_comes_first_in_the_render() {
        let prompt = Prompt::new()
            .volatile("Request", "do the thing")
            .stable("System", "ubuntu 24.04");
        let rendered = prompt.render();
        let system = rendered.find("System").unwrap();
        let request = rendered.find("Request").unwrap();
        assert!(
            system < request,
            "stable must be a prefix or the cache never hits"
        );
    }

    #[test]
    fn identical_stable_halves_fingerprint_the_same() {
        let a = Prompt::new()
            .stable("System", "x")
            .volatile("Request", "one");
        let b = Prompt::new()
            .stable("System", "x")
            .volatile("Request", "two");
        assert_eq!(
            a.stable_fingerprint(),
            b.stable_fingerprint(),
            "volatile changes must not disturb the cached prefix"
        );
    }

    #[test]
    fn changing_the_stable_half_changes_the_fingerprint() {
        let a = Prompt::new().stable("System", "ubuntu 24.04");
        let b = Prompt::new().stable("System", "ubuntu 24.10");
        assert_ne!(a.stable_fingerprint(), b.stable_fingerprint());
    }

    #[test]
    fn a_clock_in_the_stable_half_is_flagged() {
        let prompt = Prompt::new().stable("Boot", "uptime 3 days");
        let problems = prompt.check_stability();
        assert_eq!(problems.len(), 1);
        assert!(
            problems[0].contains("disables the KV cache"),
            "{}",
            problems[0]
        );
    }

    #[test]
    fn timestamps_in_the_stable_half_are_flagged() {
        let prompt = Prompt::new().stable("Now", "2026-09-13T01:22:00Z");
        assert!(!prompt.check_stability().is_empty());
    }

    #[test]
    fn a_clean_stable_half_passes() {
        let prompt = Prompt::new()
            .stable("System", "Ubuntu 24.04, aarch64, 8 GB")
            .volatile("Request", "back up my photos at 02:00 nightly");
        assert!(
            prompt.check_stability().is_empty(),
            "volatile timestamps must not trip the check"
        );
    }
}
