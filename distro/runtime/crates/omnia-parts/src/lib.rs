//! The vetted parts library the small model composes capabilities from.
//!
//! Each part is a real, tested building block with a typed manifest. The model
//! wires parts together; it does not write critical daemons from scratch. That
//! holds at any model size, because composing tested parts is verifiable and
//! generating a backup daemon from scratch is not.

#![forbid(unsafe_op_in_unsafe_fn)]
