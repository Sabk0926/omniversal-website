//! Permission manifest to systemd unit hardening.
//!
//! `omnia-parts` derives what a capability may touch. This crate turns that
//! into something the kernel enforces, by rendering a systemd service unit with
//! the hardening directives that match.
//!
//! # Why systemd rather than our own sandbox
//!
//! `ProtectSystem`, `ReadWritePaths`, `SystemCallFilter` and the rest are a
//! mature, audited sandbox that ships on every Ubuntu machine and that
//! administrators already know how to read. Writing a second one would mean
//! reimplementing namespaces, seccomp and cgroups less well, and producing
//! units nobody can review.
//!
//! An operator can read the generated unit and see exactly what a capability
//! may do. That reviewability is part of the point: an OS that installs things
//! on its own owes the reader a file they can check.

#![forbid(unsafe_op_in_unsafe_fn)]

mod floor;
mod unit;

pub use floor::refusal;
pub use unit::{Unit, UnitKind};

use std::fmt;

use omnia_parts::Permissions;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxError {
    /// A path hit the secrets floor. Carries both the path and the reason so
    /// the message explains itself without a lookup.
    Forbidden { path: String, reason: String },
    /// A `~/` path was used but no home directory is known.
    NoHome { path: String },
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SandboxError::Forbidden { path, reason } => {
                write!(f, "refusing access to {path}: {reason}")
            }
            SandboxError::NoHome { path } => {
                write!(f, "cannot expand {path}: no home directory is known")
            }
        }
    }
}

impl std::error::Error for SandboxError {}

pub type Result<T> = std::result::Result<T, SandboxError>;

/// Check a permission set against the secrets floor.
///
/// Called before anything is built, so a capability that would need forbidden
/// access is rejected at planning time rather than at install time.
pub fn check(permissions: &Permissions) -> Result<()> {
    for path in permissions
        .read_paths
        .iter()
        .chain(&permissions.write_paths)
    {
        if let Some(reason) = floor::refusal(path) {
            return Err(SandboxError::Forbidden {
                path: path.clone(),
                reason,
            });
        }
    }
    Ok(())
}

/// Expand a leading `~/` against a home directory.
///
/// systemd system units cannot interpret `~`, so paths are resolved when the
/// unit is written rather than when it runs. A capability built for one user is
/// therefore pinned to that user's paths, which is the intended behaviour: the
/// alternative is a unit whose meaning changes depending on who starts it.
pub fn expand_home(path: &str, home: Option<&str>) -> Result<String> {
    let Some(rest) = path.strip_prefix("~/") else {
        return Ok(path.to_string());
    };
    let home = home.ok_or_else(|| SandboxError::NoHome {
        path: path.to_string(),
    })?;
    Ok(format!("{}/{rest}", home.trim_end_matches('/')))
}

/// Expand every path in a permission set.
pub fn expand(permissions: &Permissions, home: Option<&str>) -> Result<Permissions> {
    let map = |paths: &[String]| -> Result<Vec<String>> {
        paths.iter().map(|p| expand_home(p, home)).collect()
    };
    Ok(Permissions {
        read_paths: map(&permissions.read_paths)?,
        write_paths: map(&permissions.write_paths)?,
        network: permissions.network,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permissions(read: &[&str], write: &[&str], network: bool) -> Permissions {
        Permissions {
            read_paths: read.iter().map(|s| s.to_string()).collect(),
            write_paths: write.iter().map(|s| s.to_string()).collect(),
            network,
        }
    }

    #[test]
    fn an_ordinary_permission_set_passes() {
        assert!(check(&permissions(
            &["~/Pictures"],
            &["/var/backups/pictures"],
            false
        ))
        .is_ok());
    }

    #[test]
    fn a_vetted_part_pointed_at_secrets_is_still_refused() {
        // `snapshot` with source=~/.ssh is a well-formed use of a good part.
        // The floor is what stops it.
        let err = check(&permissions(&["~/.ssh"], &["/var/backups/keys"], false)).unwrap_err();
        assert!(err.to_string().contains("secret material"), "{err}");
        assert!(err.to_string().contains("~/.ssh"), "names the path: {err}");
    }

    #[test]
    fn a_forbidden_write_target_is_refused_too() {
        assert!(check(&permissions(&[], &["/etc/shadow"], false)).is_err());
    }

    #[test]
    fn home_expansion_pins_paths_to_a_user() {
        assert_eq!(
            expand_home("~/Pictures", Some("/home/sam")).unwrap(),
            "/home/sam/Pictures"
        );
        assert_eq!(
            expand_home("/var/backups", Some("/home/sam")).unwrap(),
            "/var/backups"
        );
    }

    #[test]
    fn a_trailing_slash_on_home_does_not_double_up() {
        assert_eq!(
            expand_home("~/Pictures", Some("/home/sam/")).unwrap(),
            "/home/sam/Pictures"
        );
    }

    #[test]
    fn expanding_without_a_home_is_an_error_not_a_guess() {
        let err = expand_home("~/Pictures", None).unwrap_err();
        assert!(matches!(err, SandboxError::NoHome { .. }));
    }
}
