//! ioctl request numbers, and the structs they carry.
//!
//! # Computing the numbers rather than hard-coding them
//!
//! An ioctl request encodes the direction, the size of the payload struct, a
//! magic byte and an ordinal. Encoding the size is the useful part: pass the
//! wrong struct and the kernel returns `ENOTTY` instead of reading past the end
//! of your buffer.
//!
//! So the numbers are computed here from `size_of` the same way the C macros
//! compute them from `sizeof`. Hard-coding `0x80284f02` would work right up
//! until a struct grew a field, at which point the constant and the struct
//! would disagree and the check that is supposed to catch that would be the
//! thing that is wrong.
//!
//! The encoding below is `asm-generic`, which covers x86_64, aarch64, riscv and
//! every other architecture Ubuntu ships. Alpha, MIPS, PowerPC and SPARC use a
//! different bit layout; none of them is a target, and a build for one would
//! need this module revisited rather than silently misbehaving.

use crate::EVENT_SIZE;

const NRBITS: u32 = 8;
const TYPEBITS: u32 = 8;
const SIZEBITS: u32 = 14;

const NRSHIFT: u32 = 0;
const TYPESHIFT: u32 = NRSHIFT + NRBITS;
const SIZESHIFT: u32 = TYPESHIFT + TYPEBITS;
const DIRSHIFT: u32 = SIZESHIFT + SIZEBITS;

const DIR_NONE: u32 = 0;
const DIR_WRITE: u32 = 1;
const DIR_READ: u32 = 2;

/// The magic byte, `'O'`, shared with the C header.
pub const MAGIC: u32 = b'O' as u32;

const fn encode(direction: u32, number: u32, size: usize) -> u32 {
    // A payload larger than 14 bits cannot be encoded. Catching it here turns a
    // silently truncated size into a build failure.
    assert!(size < (1 << SIZEBITS));
    (direction << DIRSHIFT)
        | (MAGIC << TYPESHIFT)
        | (number << NRSHIFT)
        | ((size as u32) << SIZESHIFT)
}

/// `_IOR` — the kernel writes into the caller's buffer.
const fn ior(number: u32, size: usize) -> u32 {
    encode(DIR_READ, number, size)
}

/// `_IOW` — the caller's buffer goes to the kernel.
const fn iow(number: u32, size: usize) -> u32 {
    encode(DIR_WRITE, number, size)
}

/// `_IO` — no payload.
const fn io(number: u32) -> u32 {
    encode(DIR_NONE, number, 0)
}

/// Read the module's ABI version. The first call any client makes.
pub const IOC_ABI: u32 = ior(0x01, std::mem::size_of::<u32>());
pub const IOC_STATS: u32 = ior(0x02, std::mem::size_of::<Stats>());
pub const IOC_EMIT: u32 = iow(0x03, EVENT_SIZE);
pub const IOC_SUBSCRIBE: u32 = iow(0x04, std::mem::size_of::<u64>());
pub const IOC_GUARD: u32 = ior(0x05, std::mem::size_of::<GuardStatus>());
/// Claim the executor role. Exactly one process may hold it.
pub const IOC_CLAIM: u32 = io(0x06);
/// Prove the executor is still alive. Missing it trips the watchdog.
pub const IOC_HEARTBEAT: u32 = io(0x07);

/// Whether the never-list is actually enforcing.
///
/// `omniad` refuses to act autonomously unless this reports `Active`. The guard
/// runs below the daemon, in a cgroup it cannot edit, because a rule the daemon
/// enforces on itself is one bad generation away from not existing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardState {
    /// No BPF-LSM programs pinned.
    Absent,
    Active,
    /// Pinned, but a hook failed to attach. Partial enforcement, which is not
    /// enforcement.
    Degraded,
    Unknown(u32),
}

impl GuardState {
    pub fn from_code(code: u32) -> GuardState {
        match code {
            0 => GuardState::Absent,
            1 => GuardState::Active,
            2 => GuardState::Degraded,
            other => GuardState::Unknown(other),
        }
    }

    /// May the daemon act without asking?
    ///
    /// Only `Active`. `Degraded` is deliberately not enough: some hooks
    /// attached means some rules are unenforced, and which ones is exactly the
    /// thing that cannot be assumed.
    pub fn permits_autonomy(self) -> bool {
        self == GuardState::Active
    }

    pub fn label(self) -> String {
        match self {
            GuardState::Absent => "absent".into(),
            GuardState::Active => "active".into(),
            GuardState::Degraded => "degraded".into(),
            GuardState::Unknown(code) => format!("unknown({code})"),
        }
    }
}

impl std::fmt::Display for GuardState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct GuardStatus {
    pub state: u32,
    pub hooks_attached: u32,
    pub denials: u64,
    pub last_denial_ns: u64,
    pub last_rule: [u8; crate::COMM_LEN * 2],
}

const _: () = assert!(std::mem::size_of::<GuardStatus>() == 56);

impl Default for GuardStatus {
    fn default() -> GuardStatus {
        GuardStatus {
            state: 0,
            hooks_attached: 0,
            denials: 0,
            last_denial_ns: 0,
            last_rule: [0; crate::COMM_LEN * 2],
        }
    }
}

impl GuardStatus {
    pub fn state(&self) -> GuardState {
        GuardState::from_code(self.state)
    }

    pub fn last_rule(&self) -> String {
        let end = self
            .last_rule
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(self.last_rule.len());
        String::from_utf8_lossy(&self.last_rule[..end]).into_owned()
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<GuardStatus> {
        if bytes.len() < std::mem::size_of::<GuardStatus>() {
            return None;
        }
        let mut last_rule = [0u8; crate::COMM_LEN * 2];
        last_rule.copy_from_slice(&bytes[24..24 + crate::COMM_LEN * 2]);
        Some(GuardStatus {
            state: u32_at(bytes, 0),
            hooks_attached: u32_at(bytes, 4),
            denials: u64_at(bytes, 8),
            last_denial_ns: u64_at(bytes, 16),
            last_rule,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct Stats {
    pub abi_version: u32,
    pub readers: u32,
    pub events_emitted: u64,
    /// The ring was full. A non-zero value here means a reader is too slow and
    /// events that were meant to explain something are gone.
    pub events_dropped: u64,
    /// 0 when no executor has claimed.
    pub executor_pid: u64,
    pub last_heartbeat_ns: u64,
}

const _: () = assert!(std::mem::size_of::<Stats>() == 40);

impl Stats {
    pub fn from_bytes(bytes: &[u8]) -> Option<Stats> {
        if bytes.len() < std::mem::size_of::<Stats>() {
            return None;
        }
        Some(Stats {
            abi_version: u32_at(bytes, 0),
            readers: u32_at(bytes, 4),
            events_emitted: u64_at(bytes, 8),
            events_dropped: u64_at(bytes, 16),
            executor_pid: u64_at(bytes, 24),
            last_heartbeat_ns: u64_at(bytes, 32),
        })
    }

    pub fn has_executor(&self) -> bool {
        self.executor_pid != 0
    }

    /// Share of emitted events that never reached a reader.
    pub fn drop_ratio(&self) -> f32 {
        let total = self.events_emitted + self.events_dropped;
        if total == 0 {
            return 0.0;
        }
        self.events_dropped as f32 / total as f32
    }
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    let mut buffer = [0u8; 4];
    buffer.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_ne_bytes(buffer)
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    let mut buffer = [0u8; 8];
    buffer.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_ne_bytes(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The values the C macros produce on asm-generic, computed by hand from
    /// the header so this is an independent check and not a restatement of the
    /// code above.
    ///
    ///   _IOR('O', 0x01, __u32)  = (2<<30)|(4<<16)|(0x4f<<8)|0x01
    #[test]
    fn the_request_numbers_match_the_c_macros() {
        assert_eq!(IOC_ABI, (2 << 30) | (4 << 16) | (0x4f << 8) | 0x01);
        assert_eq!(IOC_STATS, (2 << 30) | (40 << 16) | (0x4f << 8) | 0x02);
        assert_eq!(IOC_EMIT, (1 << 30) | (256 << 16) | (0x4f << 8) | 0x03);
        assert_eq!(IOC_SUBSCRIBE, (1 << 30) | (8 << 16) | (0x4f << 8) | 0x04);
        assert_eq!(IOC_GUARD, (2 << 30) | (56 << 16) | (0x4f << 8) | 0x05);
        assert_eq!(IOC_CLAIM, (0x4f << 8) | 0x06);
        assert_eq!(IOC_HEARTBEAT, (0x4f << 8) | 0x07);
    }

    #[test]
    fn the_size_field_is_what_makes_a_wrong_struct_fail_loudly() {
        // Pull the size back out of the encoded request. This is the field the
        // kernel compares against, and the reason these are computed rather
        // than written down.
        let size_of = |request: u32| (request >> SIZESHIFT) & ((1 << SIZEBITS) - 1);
        assert_eq!(size_of(IOC_STATS) as usize, std::mem::size_of::<Stats>());
        assert_eq!(
            size_of(IOC_GUARD) as usize,
            std::mem::size_of::<GuardStatus>()
        );
        assert_eq!(size_of(IOC_EMIT) as usize, EVENT_SIZE);
    }

    #[test]
    fn every_request_has_a_distinct_ordinal() {
        let requests = [
            IOC_ABI,
            IOC_STATS,
            IOC_EMIT,
            IOC_SUBSCRIBE,
            IOC_GUARD,
            IOC_CLAIM,
            IOC_HEARTBEAT,
        ];
        let mut ordinals: Vec<u32> = requests.iter().map(|r| r & 0xff).collect();
        ordinals.sort_unstable();
        ordinals.dedup();
        assert_eq!(
            ordinals.len(),
            requests.len(),
            "two ioctls share an ordinal"
        );
    }

    #[test]
    fn a_degraded_guard_does_not_permit_autonomy() {
        // Some hooks attached means some rules unenforced, and which ones is
        // exactly what cannot be assumed.
        assert!(GuardState::Active.permits_autonomy());
        assert!(!GuardState::Degraded.permits_autonomy());
        assert!(!GuardState::Absent.permits_autonomy());
        assert!(!GuardState::Unknown(7).permits_autonomy());
    }

    #[test]
    fn guard_status_decodes_its_rule_text() {
        let mut status = GuardStatus {
            state: 1,
            hooks_attached: 4,
            denials: 3,
            last_denial_ns: 99,
            ..GuardStatus::default()
        };
        let rule = b"write:/etc/shadow";
        status.last_rule[..rule.len()].copy_from_slice(rule);

        let bytes = {
            let mut out = vec![0u8; std::mem::size_of::<GuardStatus>()];
            out[0..4].copy_from_slice(&status.state.to_ne_bytes());
            out[4..8].copy_from_slice(&status.hooks_attached.to_ne_bytes());
            out[8..16].copy_from_slice(&status.denials.to_ne_bytes());
            out[16..24].copy_from_slice(&status.last_denial_ns.to_ne_bytes());
            out[24..].copy_from_slice(&status.last_rule);
            out
        };

        let decoded = GuardStatus::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.state(), GuardState::Active);
        assert_eq!(decoded.last_rule(), "write:/etc/shadow");
        assert_eq!(decoded.hooks_attached, 4);
    }

    #[test]
    fn stats_decode_and_report_drops() {
        let mut bytes = vec![0u8; std::mem::size_of::<Stats>()];
        bytes[0..4].copy_from_slice(&1u32.to_ne_bytes());
        bytes[4..8].copy_from_slice(&2u32.to_ne_bytes());
        bytes[8..16].copy_from_slice(&75u64.to_ne_bytes());
        bytes[16..24].copy_from_slice(&25u64.to_ne_bytes());
        bytes[24..32].copy_from_slice(&4242u64.to_ne_bytes());

        let stats = Stats::from_bytes(&bytes).unwrap();
        assert_eq!(stats.abi_version, 1);
        assert_eq!(stats.readers, 2);
        assert!(stats.has_executor());
        assert_eq!(stats.executor_pid, 4242);
        assert!((stats.drop_ratio() - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn an_idle_module_does_not_report_a_drop_ratio_of_nan() {
        // 0/0. A NaN here would propagate into a health check comparison that
        // silently evaluates false.
        assert_eq!(Stats::default().drop_ratio(), 0.0);
        assert!(!Stats::default().has_executor());
    }

    #[test]
    fn a_truncated_ioctl_response_is_refused() {
        assert!(Stats::from_bytes(&[0u8; 39]).is_none());
        assert!(GuardStatus::from_bytes(&[0u8; 55]).is_none());
    }
}
