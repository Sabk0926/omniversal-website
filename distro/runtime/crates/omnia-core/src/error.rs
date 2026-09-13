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
