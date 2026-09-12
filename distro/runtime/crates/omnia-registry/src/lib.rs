//! Capability registry: dedup, provenance, and retained tests.
//!
//! Answers "do I already have this?" before anything is built, which is what
//! prevents twelve half-working backup scripts.

#![forbid(unsafe_op_in_unsafe_fn)]
