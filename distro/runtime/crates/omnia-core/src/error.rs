//! Errors and exit codes.
//!
//! Exit codes are part of the interface. The shell integration branches on
//! them, so they are fixed here and documented in omni(1). In particular
//! `NoModel` and `BrokerUnavailable` are the two the `command_not_found`
//! handler must distinguish: the first is worth telling the user about, the
//! second should silently fall through to the normal "command not found".

use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// Configuration is malformed or contradictory.
    Config {
        message: String,
        detail: Option<String>,
    },
    /// No model is resident that satisfies the requested tier.
    NoModel { tier: String, hint: String },
    /// omnia-modeld is not answering.
    BackendUnavailable { message: String },
    /// A capability's generated test did not pass, so nothing was installed.
    ProofFailed { capability: String, reason: String },
    /// The permission manifest asked for something the sandbox refuses.
    PermissionRefused {
        capability: String,
        requested: String,
    },
    /// No plan survived validation, so nothing was built. Distinct from
    /// `ProofFailed`: there, something was built and failed its own test; here
    /// nothing ever got far enough to be tested.
    Refused {
        intent: String,
        attempts: Vec<String>,
    },
    /// Underlying I/O.
    Io {
        context: String,
        source: std::io::Error,
    },
}

impl Error {
    /// Fixed, documented exit codes. Do not renumber.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Config { .. } => 2,
            Error::BackendUnavailable { .. } => 5,
            Error::NoModel { .. } => 4,
            Error::ProofFailed { .. } => 6,
            Error::PermissionRefused { .. } => 7,
            Error::Refused { .. } => 8,
            Error::Io { .. } => 1,
        }
    }

    pub fn config(message: impl Into<String>) -> Self {
        Error::Config {
            message: message.into(),
            detail: None,
        }
    }

    pub fn config_detail(message: impl Into<String>, detail: impl Into<String>) -> Self {
        Error::Config {
            message: message.into(),
            detail: Some(detail.into()),
        }
    }

    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io {
            context: context.into(),
            source,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config { message, detail } => {
                write!(f, "{message}")?;
                if let Some(detail) = detail {
                    write!(f, "\n  {detail}")?;
                }
                Ok(())
            }
            Error::NoModel { tier, hint } => {
                write!(f, "no model resident for tier '{tier}'\n  {hint}")
            }
            Error::BackendUnavailable { message } => {
                write!(f, "omnia-modeld is not answering: {message}")
            }
            Error::ProofFailed { capability, reason } => write!(
                f,
                "'{capability}' was not installed: its test did not pass\n  {reason}"
            ),
            Error::PermissionRefused {
                capability,
                requested,
            } => write!(
                f,
                "'{capability}' asked for access the sandbox refuses: {requested}"
            ),
            Error::Refused { intent, attempts } => {
                write!(f, "nothing was built for \"{intent}\"")?;
                if attempts.is_empty() {
                    return write!(f, "\n  no plan was produced at all");
                }
                // Every rejection, in order. After a refusal the useful
                // question is what it kept getting wrong, and only the
                // sequence answers that.
                for (index, attempt) in attempts.iter().enumerate() {
                    write!(f, "\n  attempt {}: {attempt}", index + 1)?;
                }
                Ok(())
            }
            Error::Io { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_distinct() {
        // The shell integration branches on these. Two errors sharing a code
        // would make a branch silently wrong rather than loudly broken.
        let codes = [
            Error::config("x").exit_code(),
            Error::NoModel {
                tier: "x".into(),
                hint: String::new(),
            }
            .exit_code(),
            Error::BackendUnavailable {
                message: String::new(),
            }
            .exit_code(),
            Error::ProofFailed {
                capability: "x".into(),
                reason: String::new(),
            }
            .exit_code(),
            Error::PermissionRefused {
                capability: "x".into(),
                requested: String::new(),
            }
            .exit_code(),
            Error::Refused {
                intent: "x".into(),
                attempts: Vec::new(),
            }
            .exit_code(),
        ];
        let mut sorted = codes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "codes collide: {codes:?}");
        assert!(!sorted.contains(&0), "no error may exit successfully");
    }

    #[test]
    fn a_refusal_lists_every_attempt_in_order() {
        let text = Error::Refused {
            intent: "back up my photos".into(),
            attempts: vec!["no part called 'archive'".into(), "missing 'source'".into()],
        }
        .to_string();
        assert!(text.contains("back up my photos"));
        assert!(
            text.contains("attempt 1: no part called 'archive'"),
            "{text}"
        );
        assert!(text.contains("attempt 2: missing 'source'"), "{text}");
    }

    #[test]
    fn a_refusal_with_no_attempts_still_says_something_useful() {
        let text = Error::Refused {
            intent: "x".into(),
            attempts: Vec::new(),
        }
        .to_string();
        assert!(text.contains("no plan was produced"), "{text}");
    }
}
