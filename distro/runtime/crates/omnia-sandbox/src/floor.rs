//! The secrets floor: paths a generated capability may never touch.
//!
//! Parts are human-vetted, so in principle nothing reaches here that shouldn't.
//! This is defence in depth for the case where a vetted part is given an
//! argument nobody anticipated -- `snapshot` pointed at `~/.ssh` is a perfectly
//! well-formed use of a perfectly good part, and it must still be refused.
//!
//! This list is not configurable. An operator who wants a capability that reads
//! private keys should write that part deliberately, not obtain it by pointing
//! a backup tool at a key directory.

/// Prefixes that are refused outright, read or write.
const FORBIDDEN_PREFIXES: &[&str] = &[
    "~/.ssh",
    "~/.gnupg",
    "~/.aws",
    "~/.config/gcloud",
    "~/.kube",
    "/etc/shadow",
    "/etc/gshadow",
    "/etc/sudoers",
    "/etc/ssh",
    "/root/.ssh",
    "/proc/kcore",
    "/sys/kernel",
    "/dev/mem",
    "/dev/kmem",
];

/// File names that are refused wherever they appear.
const FORBIDDEN_NAMES: &[&str] = &[
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    "authorized_keys",
    "shadow",
    ".env",
    "credentials",
];

/// Suffixes that are refused wherever they appear.
const FORBIDDEN_SUFFIXES: &[&str] = &[".pem", ".key", ".p12", ".pfx", ".kdbx"];

/// Why a path was refused, phrased for a human reading an error.
pub fn refusal(path: &str) -> Option<String> {
    let normalised = path.trim_end_matches('/');

    for prefix in FORBIDDEN_PREFIXES {
        // Match the prefix itself or anything beneath it, but not a sibling
        // that merely starts with the same characters: /etc/sshd_config is not
        // /etc/ssh.
        if normalised == *prefix || normalised.starts_with(&format!("{prefix}/")) {
            return Some(format!("{prefix} holds secret material"));
        }
    }

    let name = normalised.rsplit('/').next().unwrap_or(normalised);
    if FORBIDDEN_NAMES.contains(&name) {
        return Some(format!("{name} is secret material"));
    }
    for suffix in FORBIDDEN_SUFFIXES {
        if name.ends_with(suffix) {
            return Some(format!("{suffix} files are secret material"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_directories_are_refused() {
        assert!(refusal("~/.ssh").is_some());
        assert!(refusal("~/.ssh/id_rsa").is_some());
        assert!(refusal("/etc/shadow").is_some());
        assert!(refusal("~/.gnupg/private-keys-v1.d").is_some());
    }

    #[test]
    fn a_sibling_with_a_shared_prefix_is_not_refused() {
        // /etc/sshd_config starts with "/etc/ssh" as a substring but is a
        // different path. Refusing on substring would be wrong here.
        assert!(refusal("/etc/sshd_config").is_none());
    }

    #[test]
    fn key_files_are_refused_wherever_they_are() {
        assert!(refusal("/srv/backup/id_rsa").is_some());
        assert!(refusal("/opt/app/server.pem").is_some());
        assert!(refusal("/home/sam/project/.env").is_some());
    }

    #[test]
    fn ordinary_paths_pass() {
        assert!(refusal("~/Pictures").is_none());
        assert!(refusal("/var/backups/pictures").is_none());
        assert!(
            refusal("/srv/data/keynote.pdf").is_none(),
            "'key' in a name is not a key file"
        );
    }

    #[test]
    fn a_trailing_slash_does_not_evade_the_check() {
        assert!(refusal("~/.ssh/").is_some());
    }
}
