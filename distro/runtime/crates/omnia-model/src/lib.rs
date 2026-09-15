//! Talking to the local model: prompt assembly, tiers, and the llama.cpp client.
//!
//! The crate's reason for existing beyond "an HTTP call" is [`prompt::Prompt`],
//! which splits a prompt into a cacheable prefix and a per-request remainder so
//! the KV cache can be reused. See that module for why that matters more than
//! anything else here.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod client;
pub mod prompt;
pub mod tier;

pub use client::{CacheStats, Completion, ModelClient, ModelError};
pub use prompt::Prompt;
pub use tier::Tier;
