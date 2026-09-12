//! Permission manifest -> systemd unit hardening + seccomp.
//!
//! Undeclared access is denied by construction rather than by policy.

#![forbid(unsafe_op_in_unsafe_fn)]
