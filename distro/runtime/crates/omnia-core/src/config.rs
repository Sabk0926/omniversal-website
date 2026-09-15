//! Layered configuration.
//!
//! Lowest precedence first:
//!
//! ```text
//! /usr/share/omnia/config.toml    shipped defaults
//! /etc/omnia/config.toml          machine policy
//! /etc/omnia/config.d/*.toml      machine drop-ins, lexical order
//! ~/.config/omnia/config.toml     user
//! OMNIA_* environment             per-invocation
//! ```
//!
//! Merging happens on the `toml::Value` tree rather than on typed structs,
//! because merging typed structs means every field needs an Option and every
//! consumer needs to unwrap. One merge, one deserialize, one typed value.
//!
//! An admin pins keys against user override by listing dotted paths in
//! `policy.locked`. Dropped user values are recorded in `Config::suppressed`
//! so `omni doctor` can explain why a setting had no effect -- silent
//! suppression is how you get bug reports that take a week to diagnose.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Value;

use crate::error::{Error, Result};
use crate::paths;

/// The shipped defaults, compiled in so a missing config.toml is not fatal.
/// This is also the documentation of every knob that exists.
pub const BUILTIN_DEFAULTS: &str = r#"
[general]
profile = "workstation"
log_level = "info"

[model]
host = "127.0.0.1"
port = 9111
# Tier the forge plans with. The floor target must work at "orchestrator".
tier = "orchestrator"
request_timeout_seconds = 300
# Reuse the KV cache for the static system-context prefix. This is the single
# highest-leverage setting in the system: on a CPU-only board a 3k-token
# prefill is a minute of latency, and caching the stable prefix removes almost
# all of it. Disable only when debugging prompt construction.
cache_static_prefix = true
max_tokens = 2048
temperature = 0.2

[forge]
# A capability is never installed until its generated test passes.
require_passing_test = true
# How many plan-build-test rounds before giving up and reporting.
max_attempts = 3
# Refuse to build if the plan wants a part that is not in the vetted library.
parts_only = false

[sandbox]
# Rendered into systemd unit hardening. Undeclared access is denied.
default_syscall_filter = "@system-service"
protect_system = "strict"
private_network = true

[policy]
locked = []
"#;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct General {
    pub profile: String,
    pub log_level: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    pub host: String,
    pub port: u16,
    pub tier: String,
    pub request_timeout_seconds: u64,
    pub cache_static_prefix: bool,
    pub max_tokens: u32,
    pub temperature: f32,
}

impl ModelConfig {
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ForgeConfig {
    pub require_passing_test: bool,
    pub max_attempts: u32,
    pub parts_only: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SandboxConfig {
    pub default_syscall_filter: String,
    pub protect_system: String,
    pub private_network: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    pub locked: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub general: General,
    pub model: ModelConfig,
    pub forge: ForgeConfig,
    pub sandbox: SandboxConfig,
    pub policy: PolicyConfig,
}

/// A resolved configuration, plus where each layer came from.
#[derive(Debug, Clone)]
pub struct Config {
    pub settings: Settings,
    /// Ordered list of layers that contributed, for `omni doctor`.
    pub sources: Vec<String>,
    /// Dotted keys dropped from the user layer because an admin locked them.
    pub suppressed: Vec<String>,
}

impl Config {
    /// Load with the standard layering. `user` is false for services running
    /// as the `omnia` system user, which must never read a human's config.
    pub fn load(user: bool) -> Result<Config> {
        let mut merged: Value = toml::from_str(BUILTIN_DEFAULTS)
            .map_err(|e| Error::config(format!("builtin defaults are invalid: {e}")))?;
        let mut sources = vec!["<builtin>".to_string()];

        for path in machine_layers() {
            if let Some(layer) = read_layer(&path)? {
                merge(&mut merged, layer);
                sources.push(path.display().to_string());
            }
        }

        // Locked keys are read from the machine layers only. A user cannot
        // unlock themselves by setting policy.locked = [].
        let locked = locked_keys(&merged);
        let mut suppressed = Vec::new();

        if user {
            let path = paths::user_config();
            if let Some(layer) = read_layer(&path)? {
                let (kept, dropped) = strip_locked(layer, &locked);
                suppressed.extend(dropped);
                merge(&mut merged, kept);
                sources.push(path.display().to_string());
            }
        }

        let env_layer = environment_layer();
        if !env_layer.is_empty() {
            let value = Value::Table(env_layer);
            let (kept, dropped) = if user {
                strip_locked(value, &locked)
            } else {
                (value, Vec::new())
            };
            suppressed.extend(dropped);
            merge(&mut merged, kept);
            sources.push("<environment>".to_string());
        }

        let settings: Settings = merged
            .try_into()
            .map_err(|e| Error::config(format!("configuration is invalid: {e}")))?;

        let config = Config {
            settings,
            sources,
            suppressed,
        };
        config.validate()?;
        Ok(config)
    }

    /// Defaults only. Used by tests and by `omni doctor --offline`.
    pub fn builtin() -> Config {
        let settings: Settings =
            toml::from_str(BUILTIN_DEFAULTS).expect("builtin defaults must parse");
        Config {
            settings,
            sources: vec!["<builtin>".into()],
            suppressed: Vec::new(),
        }
    }

    fn validate(&self) -> Result<()> {
        const PROFILES: [&str; 5] = ["workstation", "appliance", "realtime", "server", "builder"];
        if !PROFILES.contains(&self.settings.general.profile.as_str()) {
            return Err(Error::config_detail(
                format!("unknown profile '{}'", self.settings.general.profile),
                format!("valid profiles: {}", PROFILES.join(", ")),
            ));
        }

        if self.settings.model.port == 0 {
            return Err(Error::config("model.port must not be 0"));
        }

        if self.settings.forge.max_attempts == 0 {
            return Err(Error::config_detail(
                "forge.max_attempts must be at least 1",
                "0 would mean never building anything",
            ));
        }

        // The whole design rests on this. Turning it off is not a supported
        // configuration, it is a different product.
        if !self.settings.forge.require_passing_test {
            return Err(Error::config_detail(
                "forge.require_passing_test cannot be false",
                "a capability that is kept without proving it works is the one \
                 thing this system must never do",
            ));
        }

        Ok(())
    }
}

fn machine_layers() -> Vec<PathBuf> {
    if let Some(explicit) = std::env::var_os("OMNIA_CONFIG") {
        return vec![PathBuf::from(explicit)];
    }
    let mut layers = vec![paths::default_config(), paths::machine_config()];
    if let Ok(entries) = fs::read_dir(paths::config_drop_in_dir()) {
        let mut drop_ins: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "toml"))
            .collect();
        drop_ins.sort();
        layers.extend(drop_ins);
    }
    layers
}

fn read_layer(path: &Path) -> Result<Option<Value>> {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .map(Some)
            .map_err(|e| Error::config(format!("invalid TOML in {}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(format!("cannot read {}", path.display()), e)),
    }
}

/// Recursive table merge. Scalars and arrays are replaced wholesale; only
/// tables merge key-by-key. Replacing arrays is deliberate -- an admin
/// narrowing a list must not have the default values merged back in.
fn merge(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Table(base_table), Value::Table(overlay_table)) => {
            for (key, value) in overlay_table {
                match base_table.get_mut(&key) {
                    Some(existing) => merge(existing, value),
                    None => {
                        base_table.insert(key, value);
                    }
                }
            }
        }
        (slot, overlay) => *slot = overlay,
    }
}

fn locked_keys(merged: &Value) -> Vec<String> {
    merged
        .get("policy")
        .and_then(|p| p.get("locked"))
        .and_then(|l| l.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Remove locked dotted keys from a layer, returning what is left and what
/// was dropped.
fn strip_locked(layer: Value, locked: &[String]) -> (Value, Vec<String>) {
    if locked.is_empty() {
        return (layer, Vec::new());
    }
    let mut dropped = Vec::new();
    let kept = strip_recursive(layer, "", locked, &mut dropped);
    (kept, dropped)
}

fn strip_recursive(
    value: Value,
    prefix: &str,
    locked: &[String],
    dropped: &mut Vec<String>,
) -> Value {
    match value {
        Value::Table(table) => {
            let mut kept = toml::map::Map::new();
            for (key, child) in table {
                let dotted = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                if locked.contains(&dotted) {
                    dropped.push(dotted);
                    continue;
                }
                // A lock on a parent covers everything beneath it.
                if locked
                    .iter()
                    .any(|lock| dotted.starts_with(&format!("{lock}.")))
                {
                    dropped.push(dotted);
                    continue;
                }
                kept.insert(key, strip_recursive(child, &dotted, locked, dropped));
            }
            Value::Table(kept)
        }
        other => other,
    }
}

/// `OMNIA_<SECTION>_<KEY>=value`, e.g. `OMNIA_MODEL_PORT=9200`.
///
/// Reserved variables that select paths rather than settings are skipped, or
/// `OMNIA_STATE=/srv/x` would be parsed as section "state" with no key.
fn environment_layer() -> toml::map::Map<String, Value> {
    const RESERVED: [&str; 5] = [
        "OMNIA_PREFIX",
        "OMNIA_ETC",
        "OMNIA_STATE",
        "OMNIA_CONFIG",
        "OMNIA_LOG",
    ];

    let mut sections: BTreeMap<String, toml::map::Map<String, Value>> = BTreeMap::new();
    for (name, raw) in std::env::vars() {
        if !name.starts_with("OMNIA_") || RESERVED.contains(&name.as_str()) {
            continue;
        }
        let rest = name["OMNIA_".len()..].to_lowercase();
        let Some((section, key)) = rest.split_once('_') else {
            continue;
        };
        sections
            .entry(section.to_string())
            .or_default()
            .insert(key.to_string(), coerce(&raw));
    }

    sections
        .into_iter()
        .map(|(name, table)| (name, Value::Table(table)))
        .collect()
}

/// Environment values are strings; config wants typed values. Parse the
/// obvious cases and leave the rest as a string for serde to reject loudly.
fn coerce(raw: &str) -> Value {
    match raw.to_lowercase().as_str() {
        "true" | "yes" | "on" => return Value::Boolean(true),
        "false" | "no" | "off" => return Value::Boolean(false),
        _ => {}
    }
    if let Ok(int) = raw.parse::<i64>() {
        return Value::Integer(int);
    }
    if let Ok(float) = raw.parse::<f64>() {
        return Value::Float(float);
    }
    if raw.contains(',') {
        return Value::Array(
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.to_string()))
                .collect(),
        );
    }
    Value::String(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_defaults_parse_and_validate() {
        let config = Config::builtin();
        assert_eq!(config.settings.general.profile, "workstation");
        assert_eq!(config.settings.model.tier, "orchestrator");
        assert!(config.settings.model.cache_static_prefix);
        config.validate().expect("defaults must validate");
    }

    #[test]
    fn tables_merge_but_scalars_replace() {
        let mut base: Value = toml::from_str("[model]\nport = 9111\nhost = \"a\"").unwrap();
        let overlay: Value = toml::from_str("[model]\nport = 9200").unwrap();
        merge(&mut base, overlay);
        assert_eq!(base["model"]["port"].as_integer(), Some(9200));
        assert_eq!(
            base["model"]["host"].as_str(),
            Some("a"),
            "untouched key survives"
        );
    }

    #[test]
    fn arrays_are_replaced_not_appended() {
        // An admin narrowing a list must not get the defaults merged back in.
        let mut base: Value = toml::from_str("[policy]\nlocked = [\"a\", \"b\"]").unwrap();
        let overlay: Value = toml::from_str("[policy]\nlocked = [\"c\"]").unwrap();
        merge(&mut base, overlay);
        let locked = base["policy"]["locked"].as_array().unwrap();
        assert_eq!(locked.len(), 1);
    }

    #[test]
    fn locked_keys_are_stripped_and_recorded() {
        let layer: Value = toml::from_str("[model]\nport = 9999\ntier = \"desktop\"").unwrap();
        let (kept, dropped) = strip_locked(layer, &["model.port".to_string()]);
        assert_eq!(dropped, vec!["model.port"]);
        assert!(kept["model"].get("port").is_none());
        assert_eq!(
            kept["model"]["tier"].as_str(),
            Some("desktop"),
            "sibling survives"
        );
    }

    #[test]
    fn locking_a_section_covers_its_children() {
        let layer: Value = toml::from_str("[model]\nport = 1\ntier = \"x\"").unwrap();
        let (kept, dropped) = strip_locked(layer, &["model".to_string()]);
        assert_eq!(dropped.len(), 1, "the section itself is dropped whole");
        assert!(kept.get("model").is_none());
    }

    #[test]
    fn environment_values_are_typed() {
        assert_eq!(coerce("9200"), Value::Integer(9200));
        assert_eq!(coerce("true"), Value::Boolean(true));
        assert_eq!(coerce("0.7"), Value::Float(0.7));
        assert_eq!(
            coerce("a,b"),
            Value::Array(vec![Value::String("a".into()), Value::String("b".into())])
        );
        assert_eq!(coerce("orchestrator"), Value::String("orchestrator".into()));
    }

    #[test]
    fn unknown_profile_is_rejected_with_the_valid_list() {
        let mut config = Config::builtin();
        config.settings.general.profile = "controller".into();
        let err = config.validate().unwrap_err();
        let text = err.to_string();
        assert!(text.contains("controller"), "names the bad value: {text}");
        assert!(text.contains("realtime"), "lists valid options: {text}");
    }

    #[test]
    fn disabling_the_test_gate_is_refused() {
        let mut config = Config::builtin();
        config.settings.forge.require_passing_test = false;
        assert!(config.validate().is_err(), "the proof gate is not optional");
    }
}
