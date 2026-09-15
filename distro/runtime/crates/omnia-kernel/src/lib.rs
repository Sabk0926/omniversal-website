//! Userspace side of the kernel interface.
//!
//! Three sources merge into one event stream: `/dev/omnia`, BPF ringbufs consumed
//! by mmapping the map fd directly (no libbpf), and sysfs pollers for the
//! level-triggered signals that do not need a BPF program.

#![forbid(unsafe_op_in_unsafe_fn)]
