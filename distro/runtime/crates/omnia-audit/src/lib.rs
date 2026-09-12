//! Hash-chained audit records and the kernel cross-check.
//!
//! Every record commits to the previous one, so deletion and editing are
//! detectable. The kernel independently counts what the executor actually did,
//! in maps the daemon cannot write, so a doctored log shows up as a divergence
//! rather than as silence.
//!
//! Detection, not prevention. A single fully-compromised host with no external
//! anchor cannot audit itself -- see docs/DESIGN.md, "the circularity problem".
//!
//! Records carry the *reasoning*, not only the action: inputs, the plan chosen,
//! alternatives rejected, and the test result.

#![forbid(unsafe_op_in_unsafe_fn)]
