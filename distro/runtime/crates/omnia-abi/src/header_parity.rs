//! Re-parse `omnia_abi.h` and compare it to this crate, field by field.
//!
//! # Why a parser and not a comment saying "keep these in sync"
//!
//! The two halves of this ABI are built at different times on different
//! machines: the DKMS module compiles on the user's box against their running
//! kernel, the userspace package was built by us months earlier. They meet for
//! the first time in production. A comment asking a future editor to remember
//! both sides is not a mechanism.
//!
//! The header is compiled into the test with `include_str!`, so this parses the
//! real file and fails the build when the C changes without the Rust changing.
//! Nothing here runs at runtime.
//!
//! The parser is deliberately small and strict. It understands the subset of C
//! this header uses and nothing else, and it fails loudly on anything it does
//! not recognise rather than skipping it — a parser that silently ignores a new
//! field would defeat the entire point.

use std::collections::BTreeMap;

use crate::event::{EventType, RawEvent};
use crate::ioctl::{GuardStatus, Stats};

const HEADER: &str = include_str!("../../../../kernel/omnia-kmod/omnia_abi.h");

/// Sizes of the fixed-width types the header uses. `char` is the only one that
/// needs saying out loud: it is one byte here because these are byte arrays for
/// names and payloads, never a signed integer.
fn type_size(name: &str) -> Option<usize> {
    match name {
        "__u8" | "__s8" | "char" => Some(1),
        "__u16" | "__s16" => Some(2),
        "__u32" | "__s32" => Some(4),
        "__u64" | "__s64" => Some(8),
        _ => None,
    }
}

/// Strip `/* ... */` comments. The header uses them on nearly every line, and
/// they contain commas, braces and semicolons that would otherwise confuse
/// every rule below.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start..].find("*/") {
            Some(end) => rest = &rest[start + end + 2..],
            None => return out, // unterminated: nothing after it counts
        }
    }
    out.push_str(rest);
    out
}

/// `#define NAME value` for the numeric defines.
fn defines() -> BTreeMap<String, u64> {
    let text = strip_comments(HEADER);
    let mut found = BTreeMap::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("#define") else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        let (Some(name), Some(value)) = (parts.next(), parts.next()) else {
            continue;
        };
        // `1u`, `1024`, `'O'` -- the ioctl macros are handled separately.
        let value = value.trim_end_matches(['u', 'U', 'l', 'L']);
        if let Ok(parsed) = value.parse::<u64>() {
            found.insert(name.to_string(), parsed);
        }
    }
    found
}

/// The `name = value` pairs of one `enum`.
fn enum_values(name: &str) -> BTreeMap<String, u64> {
    let text = strip_comments(HEADER);
    let start = text
        .find(&format!("enum {name} {{"))
        .unwrap_or_else(|| panic!("the header has no enum {name}"));
    let body_start = start + text[start..].find('{').expect("enum has a brace") + 1;
    let body_end = body_start + text[body_start..].find('}').expect("enum is terminated");

    let mut values = BTreeMap::new();
    let mut next_implicit = 0u64;
    for entry in text[body_start..body_end].split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (key, value) = match entry.split_once('=') {
            Some((key, value)) => {
                let parsed = value
                    .trim()
                    .parse::<u64>()
                    .unwrap_or_else(|_| panic!("cannot read enum value: {entry}"));
                (key.trim().to_string(), parsed)
            }
            // C allows an implicit successor. The header does not use one
            // except for the trailing _MAX, but assuming it away would make
            // this parser wrong the first time someone does.
            None => (entry.to_string(), next_implicit),
        };
        next_implicit = value + 1;
        values.insert(key, value);
    }
    values
}

/// Every field of one `struct`, in order, as `(type, name, array length)`.
fn struct_fields(name: &str) -> Vec<(String, String, usize)> {
    let text = strip_comments(HEADER);
    let start = text
        .find(&format!("struct {name} {{"))
        .unwrap_or_else(|| panic!("the header has no struct {name}"));
    let body_start = start + text[start..].find('{').expect("struct has a brace") + 1;
    let body_end = body_start + text[body_start..].find('}').expect("struct is terminated");

    let mut fields = Vec::new();
    for statement in text[body_start..body_end].split(';') {
        let statement = statement.trim();
        if statement.is_empty() {
            continue;
        }
        // Split once on the first space, then strip whitespace from what is
        // left rather than tokenising it. An array bound is an expression and
        // may contain spaces -- `char last_rule[OMNIA_COMM_LEN * 2]` -- so
        // taking the second whitespace-delimited token silently truncates the
        // bound to its first term and undersizes the struct.
        let (kind, rest) = statement
            .split_once(char::is_whitespace)
            .unwrap_or_else(|| panic!("cannot read field in struct {name}: {statement}"));
        let kind = kind.to_string();
        let declarator: String = rest.chars().filter(|c| !c.is_whitespace()).collect();

        let (field, length) = match declarator.split_once('[') {
            Some((field, bound)) => {
                let bound = bound.trim_end_matches(']').trim();
                let length = defines()
                    .get(bound)
                    .copied()
                    .map(|value| value as usize)
                    .or_else(|| bound.parse::<usize>().ok())
                    .or_else(|| evaluate_product(bound))
                    .unwrap_or_else(|| panic!("cannot size array bound '{bound}'"));
                (field.to_string(), length)
            }
            None => (declarator.to_string(), 1),
        };
        fields.push((kind, field, length));
    }
    fields
}

/// `OMNIA_COMM_LEN * 2` -- the one arithmetic bound the header uses.
fn evaluate_product(expression: &str) -> Option<usize> {
    let (left, right) = expression.split_once('*')?;
    let resolve = |token: &str| -> Option<usize> {
        let token = token.trim();
        defines()
            .get(token)
            .map(|value| *value as usize)
            .or_else(|| token.parse::<usize>().ok())
    };
    Some(resolve(left)? * resolve(right)?)
}

/// Total size of a struct as the header declares it, assuming natural
/// alignment. Every struct here is padding-free by construction; this computes
/// the padding anyway so that if one stops being padding-free the number
/// changes and the comparison fails.
fn struct_size(name: &str) -> usize {
    let mut offset = 0usize;
    let mut widest = 1usize;
    for (kind, field, length) in struct_fields(name) {
        let unit = type_size(&kind)
            .unwrap_or_else(|| panic!("struct {name}: unknown type '{kind}' on field '{field}'"));
        // An array of char aligns to 1, not to its total length.
        let alignment = unit;
        widest = widest.max(alignment);
        if offset % alignment != 0 {
            offset += alignment - (offset % alignment);
        }
        offset += unit * length;
    }
    if offset % widest != 0 {
        offset += widest - (offset % widest);
    }
    offset
}

/// Recompute an ioctl request from the header's own macro text.
fn ioctl_from_header(name: &str) -> u32 {
    let text = strip_comments(HEADER);
    let line = text
        .lines()
        .find(|line| line.trim_start().starts_with("#define") && line.contains(name))
        .unwrap_or_else(|| panic!("the header does not define {name}"));

    // Search after the define's own name, not from the start of the line: the
    // names themselves contain `_IOC_`, so scanning for `_IO` from column zero
    // finds the request name rather than the macro invoking it.
    let rest = line
        .trim_start()
        .strip_prefix("#define")
        .expect("checked above")
        .trim_start();
    let rest = rest.strip_prefix(name).unwrap_or(rest).trim_start();

    let macro_start = rest.find("_IO").expect("an ioctl define uses an _IO macro");
    let (macro_name, args) = rest[macro_start..]
        .split_once('(')
        .expect("an ioctl macro takes arguments");
    let args = args.trim_end().trim_end_matches(')');
    let args: Vec<&str> = args.split(',').map(str::trim).collect();

    let direction = match macro_name.trim() {
        "_IO" => 0u32,
        "_IOW" => 1,
        "_IOR" => 2,
        "_IOWR" => 3,
        other => panic!("unknown ioctl macro {other}"),
    };

    let number = u32::from_str_radix(
        args[1].trim_start_matches("0x"),
        if args[1].starts_with("0x") { 16 } else { 10 },
    )
    .expect("an ioctl ordinal is a number");

    let size = match args.get(2) {
        None => 0,
        Some(kind) => {
            let kind = kind.trim();
            match kind.strip_prefix("struct ") {
                Some(struct_name) => struct_size(struct_name.trim()),
                None => {
                    type_size(kind).unwrap_or_else(|| panic!("unknown ioctl payload type '{kind}'"))
                }
            }
        }
    };

    (direction << 30) | ((size as u32) << 16) | (crate::ioctl::MAGIC << 8) | number
}

#[test]
fn the_constants_match_the_header() {
    let defines = defines();
    assert_eq!(defines["OMNIA_ABI_VERSION"], u64::from(crate::ABI_VERSION));
    assert_eq!(defines["OMNIA_COMM_LEN"] as usize, crate::COMM_LEN);
    assert_eq!(defines["OMNIA_PAYLOAD_LEN"] as usize, crate::PAYLOAD_LEN);
    assert_eq!(defines["OMNIA_RING_EVENTS"] as usize, crate::RING_EVENTS);
    assert!(
        HEADER.contains(&format!("\"{}\"", crate::DEVICE_NAME)),
        "the device name differs from the header"
    );
}

#[test]
fn the_ring_size_is_a_power_of_two_as_kfifo_requires() {
    // Not a parity check but a check on the header itself: kfifo silently
    // rounds a non-power-of-two down, so the module would hold fewer events
    // than the constant claims and drop calculations would be wrong.
    let size = defines()["OMNIA_RING_EVENTS"];
    assert!(size.is_power_of_two(), "OMNIA_RING_EVENTS = {size}");
}

#[test]
fn every_event_type_in_the_header_is_known_to_this_crate() {
    let values = enum_values("omnia_event_type");
    for (name, code) in &values {
        if name == "OMNIA_EV_MAX" {
            continue;
        }
        let kind = EventType::from_code(*code as u32);
        assert!(
            !matches!(kind, EventType::Unknown(_)),
            "the header defines {name} = {code}, which this crate does not know"
        );
        // The label is derived from the C name, so this catches a rename as
        // well as an addition.
        let expected = name.trim_start_matches("OMNIA_EV_").to_lowercase();
        assert_eq!(kind.label(), expected, "{name} is named differently here");
    }
}

#[test]
fn this_crate_invents_no_event_type_the_header_does_not_have() {
    // The other direction. A type here but not there would be one the module
    // can never emit, which means dead handling code that looks live.
    let values = enum_values("omnia_event_type");
    let codes: Vec<u64> = values
        .iter()
        .filter(|(name, _)| *name != "OMNIA_EV_MAX")
        .map(|(_, code)| *code)
        .collect();
    for kind in EventType::known() {
        assert!(
            codes.contains(&u64::from(kind.code())),
            "{kind} is not in the header"
        );
    }
    assert_eq!(codes.len(), EventType::known().len());
}

#[test]
fn every_severity_in_the_header_is_known_to_this_crate() {
    use crate::event::Severity;
    let values = enum_values("omnia_severity");
    assert_eq!(values.len(), 5, "a severity was added or removed");
    for (name, code) in &values {
        let severity = Severity::from_code(*code as u32);
        assert_eq!(
            severity.code(),
            *code as u32,
            "{name} does not round-trip; an out-of-range value became Crit"
        );
        assert_eq!(
            severity.label(),
            name.trim_start_matches("OMNIA_SEV_").to_lowercase()
        );
    }
}

#[test]
fn every_guard_state_in_the_header_is_known_to_this_crate() {
    use crate::ioctl::GuardState;
    for (name, code) in enum_values("omnia_guard_state") {
        let state = GuardState::from_code(code as u32);
        assert!(
            !matches!(state, GuardState::Unknown(_)),
            "the header defines {name} = {code}, which this crate does not know"
        );
    }
}

#[test]
fn the_event_record_has_the_same_fields_in_the_same_order() {
    let fields = struct_fields("omnia_event");
    let expected = [
        ("__u64", "seq", 1),
        ("__u64", "ts_ns", 1),
        ("__u32", "type", 1),
        ("__u32", "pid", 1),
        ("__u32", "uid", 1),
        ("__u32", "severity", 1),
        ("__u64", "arg0", 1),
        ("__u64", "arg1", 1),
        ("char", "comm", crate::COMM_LEN),
        ("char", "payload", crate::PAYLOAD_LEN),
    ];
    assert_eq!(
        fields.len(),
        expected.len(),
        "the header has {} fields, this crate mirrors {}: {fields:?}",
        fields.len(),
        expected.len()
    );
    for (actual, wanted) in fields.iter().zip(expected) {
        assert_eq!(
            (actual.0.as_str(), actual.1.as_str(), actual.2),
            wanted,
            "field order or type changed"
        );
    }
}

#[test]
fn every_struct_is_the_size_this_crate_believes_it_is() {
    assert_eq!(struct_size("omnia_event"), std::mem::size_of::<RawEvent>());
    assert_eq!(struct_size("omnia_event"), crate::EVENT_SIZE);
    assert_eq!(struct_size("omnia_stats"), std::mem::size_of::<Stats>());
    assert_eq!(
        struct_size("omnia_guard_status"),
        std::mem::size_of::<GuardStatus>()
    );
}

#[test]
fn every_struct_is_free_of_padding() {
    // The reason a fixed-size record can be indexed into without a length
    // prefix. Padding would also differ between compilers, which is how the
    // module and userspace would come to disagree while both looking right.
    for name in ["omnia_event", "omnia_stats", "omnia_guard_status"] {
        let declared: usize = struct_fields(name)
            .iter()
            .map(|(kind, _, length)| type_size(kind).unwrap() * length)
            .sum();
        assert_eq!(
            declared,
            struct_size(name),
            "struct {name} needs padding, so its layout is compiler-dependent"
        );
    }
}

#[test]
fn every_ioctl_request_matches_the_header_macro() {
    use crate::ioctl;
    for (name, ours) in [
        ("OMNIA_IOC_ABI", ioctl::IOC_ABI),
        ("OMNIA_IOC_STATS", ioctl::IOC_STATS),
        ("OMNIA_IOC_EMIT", ioctl::IOC_EMIT),
        ("OMNIA_IOC_SUBSCRIBE", ioctl::IOC_SUBSCRIBE),
        ("OMNIA_IOC_GUARD", ioctl::IOC_GUARD),
        ("OMNIA_IOC_CLAIM", ioctl::IOC_CLAIM),
        ("OMNIA_IOC_HEARTBEAT", ioctl::IOC_HEARTBEAT),
    ] {
        assert_eq!(
            ioctl_from_header(name),
            ours,
            "{name} computed from the header differs from this crate"
        );
    }
}

#[test]
fn the_parser_refuses_a_type_it_does_not_understand() {
    // The parser's own guard rail. If it returned a plausible size for an
    // unknown type, every check above would pass while being meaningless.
    assert_eq!(type_size("__u32"), Some(4));
    assert_eq!(type_size("long"), None);
    assert_eq!(type_size("void *"), None);
}

#[test]
fn the_comment_stripper_does_not_eat_code() {
    assert_eq!(strip_comments("a /* b */ c"), "a  c");
    assert_eq!(strip_comments("a /* b"), "a ", "unterminated stops there");
    assert_eq!(strip_comments("no comment"), "no comment");
    assert!(
        !strip_comments(HEADER).contains("kfifo requirement"),
        "comment text survived the strip"
    );
    assert!(
        strip_comments(HEADER).contains("OMNIA_RING_EVENTS"),
        "the define itself did not"
    );
}
