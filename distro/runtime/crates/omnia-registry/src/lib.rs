//! What this machine has taught itself.
//!
//! # The junk drawer problem
//!
//! A system that builds capabilities on demand will, without discipline, end up
//! with twelve half-working backup scripts, three of them running at once, and
//! no way to explain any of them. This crate is the discipline.
//!
//! # Deduplicating on what a plan *resolves to*
//!
//! The naive approach is to match the request text: "back up my photos every
//! night" against what was asked before. That fails immediately, because the
//! same intent has unlimited phrasings and a near-match is not a match.
//!
//! Instead, dedup happens on the **resolved composition** -- the ordered parts
//! and their arguments after validation. "back up my photos nightly" and
//! "copy ~/Pictures to /var/backups every day at 2am" converge on the same
//! steps, so they produce the same fingerprint and the second one reuses the
//! first instead of building a rival.
//!
//! That costs one planning call before the check can run, which is why there is
//! also a cheap [`Registry::similar_intents`] pre-check for the interactive
//! case. The fingerprint is the reliable mechanism; the text match is a hint.
//!
//! # Conflicts
//!
//! Two capabilities writing to the same path is the junk drawer actually
//! happening. [`Registry::conflicts`] finds that before anything is installed.

#![forbid(unsafe_op_in_unsafe_fn)]

mod record;
mod time;

pub use record::{Capability, Provenance, TestOutcome};
pub use time::{format_utc, now_epoch_seconds};

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use omnia_parts::ResolvedStep;

#[derive(Debug)]
pub enum RegistryError {
    Io {
        context: String,
        source: std::io::Error,
    },
    Corrupt {
        path: String,
        reason: String,
    },
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::Io { context, source } => write!(f, "{context}: {source}"),
            RegistryError::Corrupt { path, reason } => {
                write!(f, "capability record {path} is unreadable: {reason}")
            }
        }
    }
}

impl std::error::Error for RegistryError {}

pub type Result<T> = std::result::Result<T, RegistryError>;

/// Why a lookup matched, so the caller can decide how much to trust it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    /// Same resolved composition. Reuse it; do not build a second one.
    Identical(Capability),
    /// Same parts and shape, different arguments. Probably an update to an
    /// existing capability rather than a new one.
    SameShape(Capability),
}

/// Two capabilities that would fight each other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub existing: String,
    pub path: String,
}

impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "'{}' already writes to {}", self.existing, self.path)
    }
}

#[derive(Debug, Clone)]
pub struct Registry {
    dir: PathBuf,
    entries: BTreeMap<String, Capability>,
}

impl Registry {
    /// Open the registry at `dir`, creating nothing. A missing directory is an
    /// empty registry: a machine that has learned nothing yet is normal.
    pub fn open(dir: &Path) -> Result<Registry> {
        let mut registry = Registry {
            dir: dir.to_path_buf(),
            entries: BTreeMap::new(),
        };
        let read = match fs::read_dir(dir) {
            Ok(read) => read,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(registry),
            Err(e) => {
                return Err(RegistryError::Io {
                    context: format!("cannot read {}", dir.display()),
                    source: e,
                })
            }
        };

        for entry in read.filter_map(|e| e.ok()) {
            let path = entry.path();
            // map_or rather than is_none_or: the latter is Rust 1.82 and the
            // workspace MSRV is 1.75, which older Ubuntu toolchains still ship.
            if path.extension().map_or(true, |ext| ext != "json") {
                continue;
            }
            let text = fs::read_to_string(&path).map_err(|e| RegistryError::Io {
                context: format!("cannot read {}", path.display()),
                source: e,
            })?;
            let capability: Capability =
                serde_json::from_str(&text).map_err(|e| RegistryError::Corrupt {
                    path: path.display().to_string(),
                    reason: e.to_string(),
                })?;
            registry.entries.insert(capability.name.clone(), capability);
        }
        Ok(registry)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&Capability> {
        self.entries.get(name)
    }

    /// Everything this machine can do, oldest first -- which is the order a
    /// human wants when asking how the machine got this way.
    pub fn all(&self) -> Vec<&Capability> {
        let mut all: Vec<&Capability> = self.entries.values().collect();
        all.sort_by_key(|c| c.installed_at);
        all
    }

    /// The reliable dedup check. Run after planning, before building.
    pub fn find(&self, steps: &[ResolvedStep]) -> Option<Match> {
        let fingerprint = record::fingerprint(steps);
        if let Some(existing) = self.entries.values().find(|c| c.fingerprint == fingerprint) {
            return Some(Match::Identical(existing.clone()));
        }
        let shape = record::shape(steps);
        self.entries
            .values()
            .find(|c| record::shape_of(&c.steps) == shape)
            .cloned()
            .map(Match::SameShape)
    }

    /// Cheap pre-check before spending a planning call. A hint, not a decision:
    /// it exists so an interactive caller can say "you already have X" without
    /// waiting for the model.
    pub fn similar_intents(&self, intent: &str) -> Vec<&Capability> {
        let wanted = keywords(intent);
        if wanted.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(usize, &Capability)> = self
            .entries
            .values()
            .filter_map(|capability| {
                let have = keywords(&capability.intent);
                let overlap = wanted.iter().filter(|word| have.contains(*word)).count();
                // Two shared content words is weak but it is only a hint; the
                // fingerprint check is what actually prevents duplicates.
                (overlap >= 2).then_some((overlap, capability))
            })
            .collect();
        scored.sort_by_key(|(overlap, _)| std::cmp::Reverse(*overlap));
        scored
            .into_iter()
            .map(|(_, capability)| capability)
            .collect()
    }

    /// Capabilities that already write where this plan wants to write.
    ///
    /// This is the junk drawer caught in the act: two backup jobs writing the
    /// same destination will silently corrupt each other's output.
    pub fn conflicts(&self, steps: &[ResolvedStep]) -> Vec<Conflict> {
        let fingerprint = record::fingerprint(steps);
        let wanted = omnia_parts::total_permissions(steps);
        let mut conflicts = Vec::new();
        for capability in self.entries.values() {
            // Replacing a capability with itself is not a conflict.
            if capability.fingerprint == fingerprint {
                continue;
            }
            let theirs = omnia_parts::total_permissions(&capability.steps);
            for path in &wanted.write_paths {
                if theirs.write_paths.contains(path) {
                    conflicts.push(Conflict {
                        existing: capability.name.clone(),
                        path: path.clone(),
                    });
                }
            }
        }
        conflicts.sort_by(|a, b| (&a.existing, &a.path).cmp(&(&b.existing, &b.path)));
        conflicts
    }

    /// Write a record. Atomic: a crash mid-write leaves the old record, not a
    /// truncated one that would fail to parse on next open.
    pub fn insert(&mut self, capability: Capability) -> Result<()> {
        fs::create_dir_all(&self.dir).map_err(|e| RegistryError::Io {
            context: format!("cannot create {}", self.dir.display()),
            source: e,
        })?;

        let final_path = self.dir.join(format!("{}.json", capability.name));
        let temp_path = self.dir.join(format!(".{}.json.tmp", capability.name));
        let text =
            serde_json::to_string_pretty(&capability).map_err(|e| RegistryError::Corrupt {
                path: final_path.display().to_string(),
                reason: e.to_string(),
            })?;

        fs::write(&temp_path, text.as_bytes()).map_err(|e| RegistryError::Io {
            context: format!("cannot write {}", temp_path.display()),
            source: e,
        })?;
        fs::rename(&temp_path, &final_path).map_err(|e| RegistryError::Io {
            context: format!("cannot install {}", final_path.display()),
            source: e,
        })?;

        self.entries.insert(capability.name.clone(), capability);
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Result<bool> {
        if self.entries.remove(name).is_none() {
            return Ok(false);
        }
        let path = self.dir.join(format!("{name}.json"));
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            // Already gone on disk: the caller's intent is satisfied.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(e) => Err(RegistryError::Io {
                context: format!("cannot remove {}", path.display()),
                source: e,
            }),
        }
    }
}

/// Content words from a request, lowercased, with the filler removed.
fn keywords(text: &str) -> Vec<String> {
    const NOISE: &[&str] = &[
        "the", "a", "an", "my", "me", "i", "to", "of", "and", "or", "for", "in", "on", "at",
        "every", "each", "please", "can", "you", "want", "need", "would", "like", "it", "is", "be",
        "do", "make", "set", "up", "with", "that", "this",
    ];
    text.split(|c: char| !c.is_alphanumeric())
        .map(|word| word.to_lowercase())
        .filter(|word| word.len() > 2 && !NOISE.contains(&word.as_str()))
        .collect()
}

#[cfg(test)]
mod tests;
