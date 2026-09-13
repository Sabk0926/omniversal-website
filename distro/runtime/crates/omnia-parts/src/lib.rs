//! The vetted parts library and the composition the model is allowed to build.
//!
//! # Why parts at all
//!
//! A 3-4B model cannot write a correct backup daemon. It can reliably pick
//! `snapshot + schedule + verify-restore` and fill in three arguments. The
//! library is what turns the second, tractable problem into the one the model
//! is asked to solve.
//!
//! # The property that matters
//!
//! **Permissions are derived, never declared.** A part's manifest states what
//! that part may touch, as templates over its own parameters, and a human vets
//! that manifest. The model chooses parts and arguments; the permission set
//! falls out mechanically.
//!
//! So a model cannot over-request access, because it is never asked what access
//! it wants. The blast radius of a generated capability is bounded by the
//! catalogue, not by the model's honesty or by our prompt.
//!
//! # No shell
//!
//! Parts render to `argv` vectors and arguments are substituted as whole
//! elements. There is no shell, so there is nothing to inject into, and a
//! destination path containing `; rm -rf /` is just an unusual directory name.

#![forbid(unsafe_op_in_unsafe_fn)]

mod catalog;
mod composition;
mod manifest;

pub use catalog::Catalog;
pub use composition::{total_permissions, Composition, ResolvedStep, Step};
pub use manifest::{ExecKind, Param, ParamType, Part, Permissions};

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartsError {
    /// The model named a part that does not exist. Listing the real ones makes
    /// the retry prompt useful instead of just negative.
    UnknownPart {
        named: String,
        available: Vec<String>,
    },
    MissingArgument {
        part: String,
        param: String,
        doc: String,
    },
    UnknownArgument {
        part: String,
        named: String,
        expected: Vec<String>,
    },
    BadValue {
        part: String,
        param: String,
        value: String,
        reason: String,
    },
    EmptyComposition,
    Manifest {
        path: String,
        reason: String,
    },
}

impl fmt::Display for PartsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PartsError::UnknownPart { named, available } => write!(
                f,
                "no part called '{named}'. Available: {}",
                available.join(", ")
            ),
            PartsError::MissingArgument { part, param, doc } => {
                write!(f, "'{part}' needs '{param}': {doc}")
            }
            PartsError::UnknownArgument {
                part,
                named,
                expected,
            } => write!(
                f,
                "'{part}' has no parameter '{named}'. It takes: {}",
                expected.join(", ")
            ),
            PartsError::BadValue {
                part,
                param,
                value,
                reason,
            } => {
                write!(f, "'{part}' rejected {param}={value:?}: {reason}")
            }
            PartsError::EmptyComposition => write!(f, "a composition needs at least one step"),
            PartsError::Manifest { path, reason } => {
                write!(f, "bad part manifest {path}: {reason}")
            }
        }
    }
}

impl std::error::Error for PartsError {}

pub type Result<T> = std::result::Result<T, PartsError>;
