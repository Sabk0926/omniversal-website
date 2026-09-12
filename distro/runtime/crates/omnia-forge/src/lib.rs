//! The capability lifecycle: plan -> materialise -> prove -> package -> declare.
//!
//! Building ends in declaring, never in merely running. Nothing is installed until
//! its generated test proves the capability actually works.

#![forbid(unsafe_op_in_unsafe_fn)]
