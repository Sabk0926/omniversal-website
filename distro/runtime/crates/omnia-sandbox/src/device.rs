//! Which device nodes a generated driver may be handed.
//!
//! # An allow-list, not a deny-list
//!
//! The obvious shape is a list of dangerous nodes to refuse: `/dev/mem`,
//! `/dev/sda`, and so on. That is the wrong way round here, for the same reason
//! the parts library is: a deny-list is only as good as what somebody
//! remembered, and the thing it is constraining is generated code aimed at
//! hardware. Everything not on the list would be permitted by default, and
//! `/dev` is full of nodes that can ruin a machine — a raw disk, a watchdog, a
//! GPIO line wired to something that moves.
//!
//! So this is an allow-list of the buses rung 5 actually covers: USB, I2C, SPI,
//! serial and hidraw. That is not a limitation smuggled in as a safety measure;
//! it is the scope of the rung. A device on any other bus is not something a
//! sandboxed userspace driver can drive anyway, so being refused here costs
//! nothing and rules out the whole category of accidents at once.
//!
//! # The console is still refused
//!
//! A serial port is on the list, and the machine's own console is a serial port.
//! Handing a generated driver exclusive access to the console is how a board
//! becomes unreachable, so the console nodes are carved back out.

/// Bus prefixes a generated userspace driver may be given.
///
/// Each entry is a prefix that must be followed by more path, except where the
/// entry is a complete node. The table is the rung's scope written down.
const ALLOWED: &[(&str, &str)] = &[
    ("/dev/bus/usb/", "a USB device, through usbfs"),
    ("/dev/i2c-", "an I2C bus"),
    ("/dev/spidev", "an SPI device"),
    ("/dev/ttyUSB", "a USB serial adapter"),
    ("/dev/ttyACM", "a USB CDC serial device"),
    ("/dev/hidraw", "a raw HID device"),
    ("/dev/gpiochip", "a GPIO chip"),
];

/// Is this node one of the machine's own consoles?
///
/// Matched precisely rather than by prefix. `/dev/tty` is a prefix of
/// `/dev/ttyUSB0` and `/dev/ttyACM0`, which are ordinary USB adapters and
/// exactly what rung 5 exists to drive, so a prefix test plus a list of
/// exemptions gets this wrong the first time somebody plugs in a bus the
/// exemption list has not heard of.
///
/// The consoles are: the controlling terminal, the virtual consoles, and the
/// hardware UARTs — which on a board is usually where the console lives.
/// Losing one to a generated driver turns a recoverable mistake into a trip to
/// wherever the machine is.
fn is_console(path: &str) -> bool {
    if path == "/dev/console" || path == "/dev/tty" {
        return true;
    }
    for prefix in ["/dev/tty", "/dev/ttyS"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            // Digits and nothing else: /dev/tty1 and /dev/ttyS0 are consoles,
            // /dev/ttyUSB0 and /dev/ttyACM0 are not.
            if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
    }
    false
}

/// Why a device node was refused, or `None` if it may be used.
pub fn refusal(path: &str) -> Option<String> {
    let path = path.trim_end_matches('/');

    if !path.starts_with("/dev/") {
        return Some(format!(
            "{path} is not a device node; a driver is given devices, not files"
        ));
    }

    // Traversal is refused outright rather than resolved. Resolving would mean
    // deciding what a path means before the sandbox does, and the two could
    // disagree.
    if path.contains("/../") || path.ends_with("/..") || path.contains("//") {
        return Some(format!("{path} is not a plain path"));
    }

    if is_console(path) {
        return Some(format!(
            "{path} may be this machine's console, and a driver that takes it \
             can make the machine unreachable"
        ));
    }

    for (prefix, _) in ALLOWED {
        if path.starts_with(prefix) && path.len() > prefix.len() {
            return None;
        }
    }

    Some(format!(
        "{path} is not on a bus a sandboxed driver can drive. Allowed: {}",
        ALLOWED
            .iter()
            .map(|(prefix, what)| format!("{prefix}* ({what})"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buses_rung_five_covers_are_allowed() {
        for path in [
            "/dev/bus/usb/001/004",
            "/dev/i2c-1",
            "/dev/spidev0.0",
            "/dev/ttyUSB0",
            "/dev/ttyACM0",
            "/dev/hidraw3",
            "/dev/gpiochip0",
        ] {
            assert_eq!(refusal(path), None, "{path} should be allowed");
        }
    }

    #[test]
    fn a_raw_disk_is_refused() {
        // The category the allow-list exists to rule out wholesale. A generated
        // driver with /dev/sda is a formatted machine.
        for path in [
            "/dev/sda",
            "/dev/sda1",
            "/dev/nvme0n1",
            "/dev/mmcblk0",
            "/dev/dm-0",
        ] {
            assert!(refusal(path).is_some(), "{path} must be refused");
        }
    }

    #[test]
    fn physical_memory_and_ports_are_refused() {
        for path in ["/dev/mem", "/dev/kmem", "/dev/port", "/dev/watchdog"] {
            assert!(refusal(path).is_some(), "{path} must be refused");
        }
    }

    #[test]
    fn the_console_is_refused_even_though_serial_is_allowed() {
        // The carve-out. A board that loses its console to a generated driver
        // is a board someone has to go and find.
        for path in ["/dev/console", "/dev/tty", "/dev/tty1", "/dev/ttyS0"] {
            let reason = refusal(path).unwrap_or_else(|| panic!("{path} must be refused"));
            assert!(reason.contains("console"), "{path}: {reason}");
        }
        // ...but a USB serial adapter is not the console, however much its
        // name looks like one. This is the case a prefix test gets wrong.
        for path in ["/dev/ttyUSB0", "/dev/ttyACM0", "/dev/ttyUSB11"] {
            assert_eq!(refusal(path), None, "{path} is an adapter, not a console");
        }
    }

    #[test]
    fn a_bare_prefix_with_no_device_after_it_is_refused() {
        // /dev/bus/usb/ is the whole USB bus, not one device. Allowing it would
        // hand a driver every device on the machine.
        assert!(refusal("/dev/bus/usb/").is_some());
        assert!(refusal("/dev/i2c-").is_some());
        assert!(refusal("/dev/hidraw").is_some());
    }

    #[test]
    fn traversal_is_refused_rather_than_resolved() {
        // Resolving would mean deciding what the path means before the sandbox
        // does, and the two could disagree.
        assert!(refusal("/dev/bus/usb/../../etc/shadow").is_some());
        assert!(refusal("/dev/bus/usb/001/..").is_some());
        assert!(refusal("/dev//bus/usb/001/004").is_some());
    }

    #[test]
    fn something_that_is_not_a_device_at_all_is_refused() {
        assert!(refusal("/etc/passwd").is_some());
        assert!(refusal("relative/path").is_some());
        assert!(refusal("").is_some());
    }

    #[test]
    fn the_refusal_says_what_would_have_been_allowed() {
        // A refusal that does not tell the caller what to do instead produces
        // another identical attempt.
        let reason = refusal("/dev/sda").unwrap();
        assert!(reason.contains("/dev/bus/usb/"), "{reason}");
        assert!(reason.contains("/dev/i2c-"), "{reason}");
    }
}
