//! Configuration, paths, errors and logging shared by every Omnia binary.
//!
//! Deliberately small and dependency-light: every other crate pulls this in,
//! including the ones that end up in the initramfs.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod config;
pub mod error;
pub mod log;
pub mod paths;

pub use config::Config;
pub use error::{Error, Result};
