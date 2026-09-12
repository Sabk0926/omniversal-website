//! Rust bindings to `kernel/omnia-kmod/omnia_abi.h`.
//!
//! Struct sizes are asserted at compile time and a test re-parses the C header,
//! so a DKMS module built from one version can never silently talk to a package
//! built from another.

#![forbid(unsafe_op_in_unsafe_fn)]
