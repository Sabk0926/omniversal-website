//! The event record, and the two forms it takes.
//!
//! [`RawEvent`] is the wire format: fixed 256 bytes, exactly the C layout, used
//! for `OMNIA_IOC_EMIT` and for decoding what comes off the device.
//!
//! [`Event`] is the same thing once it is safe to look at — strings terminated,
//! type and severity resolved. Everything above this crate uses that one.

use crate::{COMM_LEN, EVENT_SIZE, PAYLOAD_LEN};

/// What happened.
///
/// `Unknown` is not a defensive afterthought, it is the compatibility rule: a
/// module newer than this binary will emit types this binary has never heard of,
/// and during an incident an event you cannot name is still worth surfacing.
/// Dropping it silently would make an upgrade look like a quiet machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EventType {
    None,
    /// A process exec'd. `arg0` is the parent pid.
    Exec,
    /// OOM kill. `arg0` is RSS in kilobytes.
    Oom,
    /// A systemd unit entered the failed state.
    UnitFail,
    /// `arg0` millidegrees, `arg1` trip point.
    Thermal,
    /// `arg0` free bytes, `arg1` total.
    DiskPressure,
    /// `arg0` runqueue latency in nanoseconds.
    SchedStall,
    /// Payload is the interface name.
    NetDown,
    /// `arg0` is PSI some-avg10 times 100.
    MemPressure,
    /// `arg0` errno, payload is the device.
    BlockError,
    /// The never-list refused something. Cannot be emitted from userspace.
    GuardDeny,
    /// The executor missed its heartbeat.
    Watchdog,
    /// Emitted by userspace through `OMNIA_IOC_EMIT`.
    User,
    Unknown(u32),
}

impl EventType {
    pub fn from_code(code: u32) -> EventType {
        match code {
            0 => EventType::None,
            1 => EventType::Exec,
            2 => EventType::Oom,
            3 => EventType::UnitFail,
            4 => EventType::Thermal,
            5 => EventType::DiskPressure,
            6 => EventType::SchedStall,
            7 => EventType::NetDown,
            8 => EventType::MemPressure,
            9 => EventType::BlockError,
            10 => EventType::GuardDeny,
            11 => EventType::Watchdog,
            12 => EventType::User,
            other => EventType::Unknown(other),
        }
    }

    pub fn code(self) -> u32 {
        match self {
            EventType::None => 0,
            EventType::Exec => 1,
            EventType::Oom => 2,
            EventType::UnitFail => 3,
            EventType::Thermal => 4,
            EventType::DiskPressure => 5,
            EventType::SchedStall => 6,
            EventType::NetDown => 7,
            EventType::MemPressure => 8,
            EventType::BlockError => 9,
            EventType::GuardDeny => 10,
            EventType::Watchdog => 11,
            EventType::User => 12,
            EventType::Unknown(code) => code,
        }
    }

    /// Every type this build knows, for masks and for tests.
    pub fn known() -> [EventType; 13] {
        [
            EventType::None,
            EventType::Exec,
            EventType::Oom,
            EventType::UnitFail,
            EventType::Thermal,
            EventType::DiskPressure,
            EventType::SchedStall,
            EventType::NetDown,
            EventType::MemPressure,
            EventType::BlockError,
            EventType::GuardDeny,
            EventType::Watchdog,
            EventType::User,
        ]
    }

    /// The name as it appears in the header, lowercased. Used in logs and in
    /// the prompt, so it has to be stable.
    pub fn label(self) -> String {
        match self {
            EventType::None => "none".into(),
            EventType::Exec => "exec".into(),
            EventType::Oom => "oom".into(),
            EventType::UnitFail => "unit_fail".into(),
            EventType::Thermal => "thermal".into(),
            EventType::DiskPressure => "disk_pressure".into(),
            EventType::SchedStall => "sched_stall".into(),
            EventType::NetDown => "net_down".into(),
            EventType::MemPressure => "mem_pressure".into(),
            EventType::BlockError => "block_error".into(),
            EventType::GuardDeny => "guard_deny".into(),
            EventType::Watchdog => "watchdog".into(),
            EventType::User => "user".into(),
            EventType::Unknown(code) => format!("unknown({code})"),
        }
    }

    /// Can userspace emit this through `OMNIA_IOC_EMIT`?
    ///
    /// The module refuses `GuardDeny` specifically: the event that attests the
    /// floor is working must not be forgeable by the thing the floor
    /// constrains. Checked here too so a caller finds out before the ioctl.
    pub fn emittable(self) -> bool {
        self != EventType::GuardDeny
    }
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Debug,
    Info,
    Warn,
    Error,
    Crit,
}

impl Severity {
    /// An out-of-range severity reads as `Crit` rather than as `Debug`.
    ///
    /// Something has to happen to a value this build does not recognise, and
    /// the two options are not symmetric: treating an unknown severity as
    /// noise is how an incident gets filtered out of the log that would have
    /// explained it.
    pub fn from_code(code: u32) -> Severity {
        match code {
            0 => Severity::Debug,
            1 => Severity::Info,
            2 => Severity::Warn,
            3 => Severity::Error,
            _ => Severity::Crit,
        }
    }

    pub fn code(self) -> u32 {
        match self {
            Severity::Debug => 0,
            Severity::Info => 1,
            Severity::Warn => 2,
            Severity::Error => 3,
            Severity::Crit => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Severity::Debug => "debug",
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Error => "error",
            Severity::Crit => "crit",
        }
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A subscription mask: which event types the reader wants.
///
/// Filtering in the kernel rather than in the reader is the point. A subscriber
/// that only cares about thermal events should not be woken for every exec on a
/// busy machine, and on a board that difference is most of the CPU cost of
/// running at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EventMask(u64);

impl EventMask {
    pub fn none() -> EventMask {
        EventMask(0)
    }

    pub fn all() -> EventMask {
        EventMask(u64::MAX)
    }

    pub fn of(types: &[EventType]) -> EventMask {
        types
            .iter()
            .fold(EventMask::none(), |mask, kind| mask.with(*kind))
    }

    /// A type with a code past 63 cannot be represented, so it is dropped
    /// rather than wrapping into an unrelated bit — a shift overflow here would
    /// silently subscribe to the wrong thing.
    pub fn with(self, kind: EventType) -> EventMask {
        match kind.code() {
            code if code < 64 => EventMask(self.0 | (1u64 << code)),
            _ => self,
        }
    }

    pub fn contains(self, kind: EventType) -> bool {
        match kind.code() {
            code if code < 64 => self.0 & (1u64 << code) != 0,
            _ => false,
        }
    }

    pub fn bits(self) -> u64 {
        self.0
    }

    pub fn from_bits(bits: u64) -> EventMask {
        EventMask(bits)
    }
}

/// The wire record, byte for byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct RawEvent {
    pub seq: u64,
    pub ts_ns: u64,
    pub kind: u32,
    pub pid: u32,
    pub uid: u32,
    pub severity: u32,
    pub arg0: u64,
    pub arg1: u64,
    pub comm: [u8; COMM_LEN],
    pub payload: [u8; PAYLOAD_LEN],
}

// The header's layout has no padding by construction, which is what makes a
// fixed-size record safe to index into. If someone adds a field that changes
// that, this stops the build rather than shipping a silent skew.
const _: () = assert!(std::mem::size_of::<RawEvent>() == EVENT_SIZE);
const _: () = assert!(std::mem::align_of::<RawEvent>() == 8);

impl Default for RawEvent {
    fn default() -> RawEvent {
        RawEvent {
            seq: 0,
            ts_ns: 0,
            kind: 0,
            pid: 0,
            uid: 0,
            severity: 0,
            arg0: 0,
            arg1: 0,
            comm: [0; COMM_LEN],
            payload: [0; PAYLOAD_LEN],
        }
    }
}

impl RawEvent {
    /// Decode one record. `None` if the slice is short, which for a fixed-size
    /// record means a truncated read and never a partial event.
    pub fn from_bytes(bytes: &[u8]) -> Option<RawEvent> {
        if bytes.len() < EVENT_SIZE {
            return None;
        }
        let u64_at = |offset: usize| {
            let mut buffer = [0u8; 8];
            buffer.copy_from_slice(&bytes[offset..offset + 8]);
            u64::from_ne_bytes(buffer)
        };
        let u32_at = |offset: usize| {
            let mut buffer = [0u8; 4];
            buffer.copy_from_slice(&bytes[offset..offset + 4]);
            u32::from_ne_bytes(buffer)
        };

        let mut comm = [0u8; COMM_LEN];
        comm.copy_from_slice(&bytes[48..48 + COMM_LEN]);
        let mut payload = [0u8; PAYLOAD_LEN];
        payload.copy_from_slice(&bytes[64..64 + PAYLOAD_LEN]);

        Some(RawEvent {
            seq: u64_at(0),
            ts_ns: u64_at(8),
            kind: u32_at(16),
            pid: u32_at(20),
            uid: u32_at(24),
            severity: u32_at(28),
            arg0: u64_at(32),
            arg1: u64_at(40),
            comm,
            payload,
        })
    }

    pub fn to_bytes(self) -> [u8; EVENT_SIZE] {
        let mut out = [0u8; EVENT_SIZE];
        out[0..8].copy_from_slice(&self.seq.to_ne_bytes());
        out[8..16].copy_from_slice(&self.ts_ns.to_ne_bytes());
        out[16..20].copy_from_slice(&self.kind.to_ne_bytes());
        out[20..24].copy_from_slice(&self.pid.to_ne_bytes());
        out[24..28].copy_from_slice(&self.uid.to_ne_bytes());
        out[28..32].copy_from_slice(&self.severity.to_ne_bytes());
        out[32..40].copy_from_slice(&self.arg0.to_ne_bytes());
        out[40..48].copy_from_slice(&self.arg1.to_ne_bytes());
        out[48..48 + COMM_LEN].copy_from_slice(&self.comm);
        out[64..64 + PAYLOAD_LEN].copy_from_slice(&self.payload);
        out
    }

    pub fn decode(self) -> Event {
        Event {
            seq: self.seq,
            ts_ns: self.ts_ns,
            kind: EventType::from_code(self.kind),
            pid: self.pid,
            uid: self.uid,
            severity: Severity::from_code(self.severity),
            arg0: self.arg0,
            arg1: self.arg1,
            comm: nul_terminated(&self.comm),
            payload: nul_terminated(&self.payload),
        }
    }
}

/// A record in the form the rest of the system reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Monotonic. A gap means the ring dropped records.
    pub seq: u64,
    /// `CLOCK_MONOTONIC` at emit. Not wall clock: this is for ordering and for
    /// measuring intervals, both of which a wall clock gets wrong across an NTP
    /// step or a board with no RTC.
    pub ts_ns: u64,
    pub kind: EventType,
    pub pid: u32,
    pub uid: u32,
    pub severity: Severity,
    pub arg0: u64,
    pub arg1: u64,
    pub comm: String,
    pub payload: String,
}

impl Event {
    /// Build one to emit. Truncates rather than refusing: a payload too long
    /// for the record is a caller bug, but losing the whole event over it
    /// during an incident is worse than losing its tail.
    pub fn emit(kind: EventType, severity: Severity, payload: &str) -> Event {
        Event {
            seq: 0,
            ts_ns: 0,
            kind,
            pid: 0,
            uid: 0,
            severity,
            arg0: 0,
            arg1: 0,
            comm: String::new(),
            payload: truncate_to(payload, PAYLOAD_LEN - 1),
        }
    }

    pub fn with_args(mut self, arg0: u64, arg1: u64) -> Event {
        self.arg0 = arg0;
        self.arg1 = arg1;
        self
    }

    pub fn encode(&self) -> RawEvent {
        let mut raw = RawEvent {
            seq: self.seq,
            ts_ns: self.ts_ns,
            kind: self.kind.code(),
            pid: self.pid,
            uid: self.uid,
            severity: self.severity.code(),
            arg0: self.arg0,
            arg1: self.arg1,
            ..RawEvent::default()
        };
        copy_nul_terminated(&mut raw.comm, &self.comm);
        copy_nul_terminated(&mut raw.payload, &self.payload);
        raw
    }

    /// One line for a log or the inbox.
    pub fn describe(&self) -> String {
        let mut line = format!("{} [{}]", self.kind, self.severity);
        if !self.comm.is_empty() {
            line.push_str(&format!(" {}({})", self.comm, self.pid));
        }
        if !self.payload.is_empty() {
            line.push_str(&format!(" {}", self.payload));
        }
        line
    }
}

/// Bytes up to the first NUL, as UTF-8 with invalid sequences replaced.
///
/// `comm` comes from the kernel and is whatever the process called itself,
/// which is not required to be UTF-8. Replacing beats refusing: the name is for
/// a human to read, and a lossy name still identifies the process.
fn nul_terminated(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Copy into a fixed field, always leaving room for the NUL.
fn copy_nul_terminated(field: &mut [u8], text: &str) {
    let room = field.len() - 1;
    let bytes = text.as_bytes();
    let take = bytes.len().min(room);
    field[..take].copy_from_slice(&bytes[..take]);
    for byte in &mut field[take..] {
        *byte = 0;
    }
}

/// Truncate on a character boundary, so a multi-byte character at the limit
/// does not become a replacement character.
fn truncate_to(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_survives_a_round_trip() {
        let original = Event {
            seq: 42,
            ts_ns: 1_234_567_890,
            kind: EventType::Thermal,
            pid: 900,
            uid: 1000,
            severity: Severity::Warn,
            arg0: 85_000,
            arg1: 2,
            comm: "kworker".into(),
            payload: "thermal_zone0".into(),
        };
        let bytes = original.encode().to_bytes();
        assert_eq!(bytes.len(), EVENT_SIZE);
        let decoded = RawEvent::from_bytes(&bytes).unwrap().decode();
        assert_eq!(decoded, original);
    }

    #[test]
    fn a_short_read_is_refused_rather_than_zero_filled() {
        // Fixed-size records mean a short read is always a bug. Padding it out
        // would turn that bug into an event with plausible-looking fields.
        assert!(RawEvent::from_bytes(&[0u8; EVENT_SIZE - 1]).is_none());
        assert!(RawEvent::from_bytes(&[]).is_none());
        assert!(RawEvent::from_bytes(&[0u8; EVENT_SIZE]).is_some());
    }

    #[test]
    fn a_longer_buffer_decodes_the_first_record_only() {
        // How a multi-record read is consumed: decode, advance 256, repeat.
        let mut buffer = vec![0u8; EVENT_SIZE * 2];
        let first = Event::emit(EventType::Oom, Severity::Crit, "first").encode();
        let second = Event::emit(EventType::Exec, Severity::Info, "second").encode();
        buffer[..EVENT_SIZE].copy_from_slice(&first.to_bytes());
        buffer[EVENT_SIZE..].copy_from_slice(&second.to_bytes());

        assert_eq!(
            RawEvent::from_bytes(&buffer).unwrap().decode().payload,
            "first"
        );
        assert_eq!(
            RawEvent::from_bytes(&buffer[EVENT_SIZE..])
                .unwrap()
                .decode()
                .payload,
            "second"
        );
    }

    #[test]
    fn an_unknown_event_type_is_kept_rather_than_dropped() {
        // A module newer than this binary. The event still matters.
        let kind = EventType::from_code(200);
        assert_eq!(kind, EventType::Unknown(200));
        assert_eq!(kind.code(), 200, "round-trips, so it can be re-emitted");
        assert_eq!(kind.label(), "unknown(200)");
    }

    #[test]
    fn an_unknown_severity_reads_as_critical_not_as_debug() {
        // The asymmetry is deliberate: filtering an incident out of the log is
        // worse than over-reporting one.
        assert_eq!(Severity::from_code(99), Severity::Crit);
        assert_eq!(Severity::from_code(0), Severity::Debug);
    }

    #[test]
    fn the_guard_denial_event_is_not_emittable_from_userspace() {
        // The event attesting the floor works must not be forgeable by the
        // thing the floor constrains.
        assert!(!EventType::GuardDeny.emittable());
        for kind in EventType::known() {
            if kind != EventType::GuardDeny {
                assert!(kind.emittable(), "{kind} should be emittable");
            }
        }
    }

    #[test]
    fn strings_longer_than_their_field_are_truncated_with_room_for_the_nul() {
        let long = "x".repeat(500);
        let event = Event::emit(EventType::User, Severity::Info, &long);
        let decoded = RawEvent::from_bytes(&event.encode().to_bytes())
            .unwrap()
            .decode();
        assert_eq!(decoded.payload.len(), PAYLOAD_LEN - 1);
        assert_eq!(
            event.encode().payload[PAYLOAD_LEN - 1],
            0,
            "the field stays NUL-terminated"
        );
    }

    #[test]
    fn truncation_does_not_split_a_character() {
        // A multi-byte character at the boundary must not become a replacement
        // character on the way back.
        let text = "é".repeat(200);
        let event = Event::emit(EventType::User, Severity::Info, &text);
        assert!(
            event.payload.chars().all(|c| c == 'é'),
            "no split character"
        );
        assert!(event.payload.len() < PAYLOAD_LEN, "room stays for the NUL");
    }

    #[test]
    fn a_comm_that_is_not_utf8_still_identifies_the_process() {
        let mut raw = RawEvent {
            kind: EventType::Exec.code(),
            ..RawEvent::default()
        };
        raw.comm[..4].copy_from_slice(&[b'k', 0xff, b'd', 0]);
        let decoded = raw.decode();
        assert!(decoded.comm.starts_with('k'), "{}", decoded.comm);
        assert_eq!(decoded.comm.chars().count(), 3);
    }

    #[test]
    fn a_full_comm_field_with_no_nul_is_read_whole() {
        // TASK_COMM_LEN names exactly 16 bytes long are not NUL-terminated.
        let raw = RawEvent {
            comm: *b"0123456789abcdef",
            ..RawEvent::default()
        };
        assert_eq!(raw.decode().comm, "0123456789abcdef");
    }

    #[test]
    fn a_mask_subscribes_to_what_was_asked_for_and_nothing_else() {
        let mask = EventMask::of(&[EventType::Thermal, EventType::Oom]);
        assert!(mask.contains(EventType::Thermal));
        assert!(mask.contains(EventType::Oom));
        assert!(!mask.contains(EventType::Exec), "exec is the noisy one");
        assert_eq!(mask.bits(), (1 << 4) | (1 << 2));
    }

    #[test]
    fn a_mask_ignores_a_type_it_cannot_represent_rather_than_wrapping() {
        // 1u64 << 70 is a shift overflow; wrapping would subscribe to type 6.
        let mask = EventMask::none().with(EventType::Unknown(70));
        assert_eq!(mask.bits(), 0);
        assert!(!mask.contains(EventType::SchedStall));
    }

    #[test]
    fn all_and_none_are_what_they_say() {
        assert!(EventType::known()
            .iter()
            .all(|kind| EventMask::all().contains(*kind)));
        assert!(!EventType::known()
            .iter()
            .any(|kind| EventMask::none().contains(*kind)));
    }

    #[test]
    fn describe_reads_as_a_log_line() {
        let event = Event {
            comm: "nginx".into(),
            pid: 1234,
            ..Event::emit(EventType::UnitFail, Severity::Error, "nginx.service")
        };
        assert_eq!(
            event.describe(),
            "unit_fail [error] nginx(1234) nginx.service"
        );
    }

    #[test]
    fn every_known_type_round_trips_through_its_code() {
        for kind in EventType::known() {
            assert_eq!(EventType::from_code(kind.code()), kind);
        }
        for code in 0..=4 {
            assert_eq!(Severity::from_code(code).code(), code);
        }
    }
}
