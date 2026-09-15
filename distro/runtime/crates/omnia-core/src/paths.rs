//! Every path Omnia touches, resolved in one place.
//!
//! Nothing else builds a path by concatenation, so running from a git checkout
//! with `OMNIA_PREFIX=/opt/omnia` behaves identically to running the installed
//! packages. The tests depend on that.

use std::env;
use std::path::{Path, PathBuf};

fn env_path(key: &str, fallback: &str) -> PathBuf {
    match env::var_os(key) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => PathBuf::from(fallback),
    }
}

/// `/usr` by default. Shipped, read-only data lives under here.
pub fn prefix() -> PathBuf {
    env_path("OMNIA_PREFIX", "/usr")
}

pub fn share() -> PathBuf {
    prefix().join("share/omnia")
}

/// Shipped defaults. Never edited by an admin.
pub fn default_config() -> PathBuf {
    share().join("config.toml")
}

/// The vetted parts library.
pub fn parts_dir() -> PathBuf {
    share().join("parts")
}

/// `/etc/omnia` by default. Admin-owned.
pub fn etc() -> PathBuf {
    env_path("OMNIA_ETC", "/etc/omnia")
}

pub fn machine_config() -> PathBuf {
    etc().join("config.toml")
}

pub fn config_drop_in_dir() -> PathBuf {
    etc().join("config.d")
}

/// `/var/lib/omnia` by default. Machine state, owned by the service user.
pub fn state() -> PathBuf {
    env_path("OMNIA_STATE", "/var/lib/omnia")
}

pub fn models_dir() -> PathBuf {
    state().join("models")
}

/// Where the registry records what this machine has taught itself.
pub fn registry_dir() -> PathBuf {
    state().join("capabilities")
}

/// Scratch space for the forge: staging a capability before it is proven.
pub fn build_dir() -> PathBuf {
    state().join("build")
}

/// Reverse actions, written before anything is applied.
pub fn undo_dir() -> PathBuf {
    state().join("undo")
}

fn home() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/root"))
}

fn xdg(key: &str, fallback: &str) -> PathBuf {
    match env::var_os(key) {
        Some(value) if Path::new(&value).is_absolute() => PathBuf::from(value),
        _ => home().join(fallback),
    }
}

pub fn user_config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("omnia")
}

pub fn user_config() -> PathBuf {
    user_config_dir().join("config.toml")
}

pub fn user_state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("omnia")
}

/// The inbox: events, actions taken, and proposals awaiting a decision.
pub fn inbox() -> PathBuf {
    user_state_dir().join("inbox.jsonl")
}

/// Hash-chained audit records.
pub fn audit_log() -> PathBuf {
    user_state_dir().join("audit.jsonl")
}

/// Runtime directory for sockets. Falls back to a uid-keyed /tmp path when
/// XDG_RUNTIME_DIR is absent (cron, containers, ssh without lingering) --
/// same trust boundary, minus the automatic cleanup at logout.
pub fn runtime_dir() -> PathBuf {
    match env::var_os("XDG_RUNTIME_DIR") {
        Some(value) if Path::new(&value).is_dir() => PathBuf::from(value).join("omnia"),
        _ => PathBuf::from(format!("/tmp/omnia-{}", current_uid())),
    }
}

/// Avoids a libc dependency in this crate for one call. `getuid` is infallible
/// and takes no arguments, so there is nothing to get wrong at the boundary.
fn current_uid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_is_overridable() {
        // Serialised implicitly: these are the only tests touching this var.
        std::env::set_var("OMNIA_PREFIX", "/opt/omnia");
        assert_eq!(share(), PathBuf::from("/opt/omnia/share/omnia"));
        assert_eq!(parts_dir(), PathBuf::from("/opt/omnia/share/omnia/parts"));
        std::env::remove_var("OMNIA_PREFIX");
        assert_eq!(share(), PathBuf::from("/usr/share/omnia"));
    }

    #[test]
    fn empty_env_falls_back_to_default() {
        std::env::set_var("OMNIA_ETC", "");
        assert_eq!(etc(), PathBuf::from("/etc/omnia"));
        std::env::remove_var("OMNIA_ETC");
    }
}
