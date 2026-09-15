//! Rust bindings to `kernel/omnia-kmod/omnia_abi.h`.
//!
//! # Why this crate exists separately
//!
//! A DKMS module is built on the user's machine, at module-build time, against
//! whatever kernel is running. The userspace package is built by us, months
//! earlier. Those two artifacts meet for the first time on someone else's
//! computer, and if their idea of `struct omnia_event` differs by one field the
//! symptom is not a crash — it is plausible-looking garbage: a thermal event
//! read as a disk-pressure event, a pid read out of a timestamp.
//!
//! So this crate does two things and nothing else:
//!
//! 1. Mirrors the header exactly, with the sizes asserted at compile time.
//! 2. Re-parses the real `.h` in a test and compares it field by field, so a
//!    change to the C that is not made here fails the build here.
//!
//! # Decoding without `unsafe`
//!
//! Records arrive as bytes from a `read(2)`. The obvious move is to transmute
//! the buffer into the struct, which is wrong twice: the buffer is not
//! guaranteed to be aligned, and it is attacker-adjacent data being reinterpreted
//! as a type with invariants. Fields are decoded one at a time with
//! `from_ne_bytes` instead. Native-endian is correct and not a shortcut: the
//! writer is the kernel on this same machine.

#![forbid(unsafe_code)]

pub mod event;
pub mod ioctl;

pub use event::{Event, EventMask, EventType, RawEvent, Severity};
pub use ioctl::{GuardState, GuardStatus, Stats};

/// Bumped whenever a field is reordered or resized. Userspace refuses to attach
/// to a device whose major differs.
pub const ABI_VERSION: u32 = 1;

pub const DEVICE_NAME: &str = "omnia";

/// `/dev/omnia`, the char device the module creates.
pub const DEVICE_PATH: &str = "/dev/omnia";

/// Matches `TASK_COMM_LEN`.
pub const COMM_LEN: usize = 16;

pub const PAYLOAD_LEN: usize = 192;

/// Records the kernel ring holds. A power of two, per kfifo.
pub const RING_EVENTS: usize = 1024;

/// One record, fixed size so a short read is a bug rather than a partial event.
pub const EVENT_SIZE: usize = 256;

/// Does this build understand a device reporting `version`?
///
/// Only the major matters, and version 1 has no minor, so this is currently an
/// equality check with a name that will still be correct when it is not.
pub fn abi_compatible(version: u32) -> bool {
    version == ABI_VERSION
}

#[cfg(test)]
mod header_parity;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_event_record_is_exactly_the_size_the_header_promises() {
        assert_eq!(EVENT_SIZE, 256);
        assert_eq!(
            8 + 8 + 4 + 4 + 4 + 4 + 8 + 8 + COMM_LEN + PAYLOAD_LEN,
            EVENT_SIZE
        );
    }

    #[test]
    fn an_unknown_abi_version_is_refused_in_both_directions() {
        assert!(abi_compatible(ABI_VERSION));
        assert!(
            !abi_compatible(ABI_VERSION + 1),
            "a newer module is not assumed compatible"
        );
        assert!(!abi_compatible(0), "an unset version is not a match");
    }
}
