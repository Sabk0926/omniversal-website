//! Userspace side of the kernel interface.
//!
//! Two jobs, and they are separate on purpose.
//!
//! **Knowing what hardware is here** — [`device`] and [`modalias`]. This is the
//! driver ladder's input: what is plugged in, what is bound to it, and whether
//! anything should be. Pure filesystem reads, so it works on a captured device
//! tree from a failing board as readily as on the running machine.
//!
//! **Knowing what the kernel is telling us** — `/dev/omnia`, BPF ringbufs
//! consumed by mmapping the map fd directly (no libbpf), and sysfs pollers for
//! the level-triggered signals that do not need a BPF program. That half needs
//! the module loaded and lands with the daemon.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod device;
pub mod modalias;
pub mod modules;
pub mod uevent;

pub use device::{enumerate, unclaimed, Device};
pub use modalias::{Expectation, Modalias};
pub use modules::{IdCandidate, Lookup, ModuleIndex};
pub use uevent::{Action, Uevent};
