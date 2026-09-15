//! What every command needs before it can do anything: config, the parts
//! catalogue, and the record of what this machine already knows.
//!
//! Loaded once per invocation and passed down, so a command cannot
//! accidentally read a different configuration from the one `omni doctor`
//! reported.

use std::path::PathBuf;

use omnia_core::{paths, Config, Error, Result};
use omnia_parts::Catalog;
use omnia_registry::Registry;

pub struct Context {
    pub config: Config,
    pub catalog: Catalog,
    pub registry: Registry,
    /// Parts loaded from disk on top of the compiled-in ones.
    pub installed_parts: usize,
    pub home: Option<String>,
}

impl Context {
    pub fn load() -> Result<Context> {
        let config = Config::load(true)?;
        omnia_core::log::init(&config.settings.general.log_level);

        let mut catalog = Catalog::builtin()
            .map_err(|e| Error::config(format!("the built-in parts library is broken: {e}")))?;
        let installed_parts = catalog.load_dir(&paths::parts_dir()).map_err(|e| {
            Error::config(format!(
                "cannot read the parts library at {}: {e}",
                paths::parts_dir().display()
            ))
        })?;

        let registry_dir = paths::registry_dir();
        let registry = Registry::open(&registry_dir).map_err(|e| {
            Error::config(format!(
                "cannot read what this machine knows, at {}: {e}",
                registry_dir.display()
            ))
        })?;

        Ok(Context {
            config,
            catalog,
            registry,
            installed_parts,
            home: std::env::var("HOME").ok(),
        })
    }

    pub fn build_dir(&self) -> PathBuf {
        paths::build_dir()
    }

    /// The machine facts that go in the cacheable half of the model prompt.
    ///
    /// Everything here must be stable for the life of the machine. A clock, a
    /// pid or an uptime would invalidate the KV cache on every single request,
    /// which on a CPU-only board is the difference between two seconds and a
    /// minute. `Prompt::check_stability` catches the obvious cases; keeping
    /// this function boring is what stops the subtle ones.
    pub fn system_facts(&self) -> String {
        let mut facts = vec![
            format!("Architecture: {}", std::env::consts::ARCH),
            format!("Profile: {}", self.config.settings.general.profile),
        ];
        if let Some(release) = os_release() {
            facts.insert(0, format!("Distribution: {release}"));
        }
        if let Some(home) = &self.home {
            facts.push(format!("The user's home directory is {home}"));
        }
        facts.join("\n")
    }
}

/// `PRETTY_NAME` from /etc/os-release, unquoted.
fn os_release() -> Option<String> {
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    parse_pretty_name(&text)
}

fn parse_pretty_name(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|value| value.trim().trim_matches('"').to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_name_is_unquoted() {
        let text = "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nID=ubuntu\n";
        assert_eq!(parse_pretty_name(text).unwrap(), "Ubuntu 24.04.1 LTS");
    }

    #[test]
    fn an_unquoted_pretty_name_also_works() {
        assert_eq!(parse_pretty_name("PRETTY_NAME=Debian\n").unwrap(), "Debian");
    }

    #[test]
    fn a_file_without_pretty_name_yields_nothing_rather_than_empty_text() {
        assert_eq!(parse_pretty_name("ID=ubuntu\n"), None);
        assert_eq!(parse_pretty_name("PRETTY_NAME=\"\"\n"), None);
    }
}
