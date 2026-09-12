//! Hardware probe, model catalog, and the llama.cpp supervisor.
//!
//! Owns tier selection (micro / orchestrator / desktop / cloud) and the decision
//! to escalate from parts composition to a large model.

#![forbid(unsafe_op_in_unsafe_fn)]
