//! Minimal HTTP/1.1 client and SSE parser.
//!
//! Exists so the runtime can talk to a loopback llama.cpp server without pulling
//! in an async runtime and a TLS stack. Cloud escalation lives behind a feature
//! flag precisely so the base image stays small.

#![forbid(unsafe_op_in_unsafe_fn)]
