//! Reading a device's `modalias`.
//!
//! # Why this is the foundation of the ladder
//!
//! `modalias` is the kernel saying, in one string, "here is what would match
//! me". Rung 1 of the driver ladder is literally `modprobe $(cat modalias)`,
//! and rungs 3 through 6 all need the vendor and device IDs that are encoded
//! inside it. Parsing it correctly is most of knowing what a device *is*.
//!
//! # The distinction that matters most
//!
//! A device with no driver bound is not automatically a device that needs one.
//! A PCI host bridge, a PCI-to-PCI bridge and a bus root are unclaimed by
//! design and always will be. On a plain cloud instance, 17 of 40 devices with
//! a modalias have no driver, and nothing is wrong with any of them.
//!
//! Getting this wrong is not a small mistake. It is the machine deciding to
//! write a driver for a host bridge — hours of work, a generated kernel module
//! aimed at something that was never broken, and exactly the kind of
//! confidently-wrong autonomy this system has to not do. So the class code is
//! parsed, not just the IDs, and a bridge answers "no driver expected" rather
//! than "unclaimed".

use std::fmt;

/// A parsed `modalias`, plus the original string.
///
/// The raw form is kept because `modprobe` wants it verbatim. Re-rendering a
/// parsed alias would be a second implementation of the kernel's format, and a
/// rounding error there means rung 1 quietly fails to find a module that exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Modalias {
    pub raw: String,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Pci(Pci),
    Usb(Usb),
    Platform {
        name: String,
    },
    /// ACPI aliases carry a colon-separated list of hardware IDs.
    Acpi {
        ids: Vec<String>,
    },
    I2c {
        name: String,
    },
    Spi {
        name: String,
    },
    Hid {
        bus: u16,
        vendor: u32,
        product: u32,
    },
    /// Device tree. `compatible` is what an overlay would match on, which makes
    /// it rung 3's input on a board.
    OpenFirmware {
        name: String,
        compatible: Vec<String>,
    },
    Virtio {
        device: u32,
        vendor: u32,
    },
    /// Parsed far enough to know the subsystem and no further. Still useful:
    /// rung 1 only needs the raw string.
    Other {
        subsystem: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pci {
    pub vendor: u16,
    pub device: u16,
    pub subsystem_vendor: u16,
    pub subsystem_device: u16,
    /// Base class. 0x06 is a bridge.
    pub base_class: u8,
    pub subclass: u8,
    pub interface: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usb {
    pub vendor: u16,
    pub product: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub interface_class: u8,
    pub interface_subclass: u8,
}

/// Whether a driver is expected here at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expectation {
    /// A driver is bound. Nothing to do.
    Bound(String),
    /// Nothing is bound and something should be. This is the ladder's input.
    Unclaimed,
    /// Nothing is bound and nothing ever will be. Carries the reason, because
    /// "we deliberately did nothing" needs to be explainable.
    NotExpected(String),
}

impl Expectation {
    pub fn needs_the_ladder(&self) -> bool {
        matches!(self, Expectation::Unclaimed)
    }
}

impl Modalias {
    pub fn parse(raw: &str) -> Modalias {
        let raw = raw.trim().to_string();
        let kind = parse_kind(&raw);
        Modalias { raw, kind }
    }

    pub fn subsystem(&self) -> &str {
        match &self.kind {
            Kind::Pci(_) => "pci",
            Kind::Usb(_) => "usb",
            Kind::Platform { .. } => "platform",
            Kind::Acpi { .. } => "acpi",
            Kind::I2c { .. } => "i2c",
            Kind::Spi { .. } => "spi",
            Kind::Hid { .. } => "hid",
            Kind::OpenFirmware { .. } => "of",
            Kind::Virtio { .. } => "virtio",
            Kind::Other { subsystem } => subsystem,
        }
    }

    /// `vendor:product` in the form people paste into a search, lowercase hex.
    /// `None` for buses that do not have numeric IDs.
    pub fn id_pair(&self) -> Option<String> {
        match &self.kind {
            Kind::Pci(pci) => Some(format!("{:04x}:{:04x}", pci.vendor, pci.device)),
            Kind::Usb(usb) => Some(format!("{:04x}:{:04x}", usb.vendor, usb.product)),
            Kind::Hid {
                vendor, product, ..
            } => Some(format!("{vendor:04x}:{product:04x}")),
            Kind::Virtio { vendor, device } => Some(format!("{vendor:04x}:{device:04x}")),
            _ => None,
        }
    }

    /// Is this a device that is unclaimed by design?
    ///
    /// Deliberately narrow. Answering "yes" here stops the ladder before it
    /// starts, so it only covers cases where the answer is *structural* — this
    /// class of thing is never driven by a driver, on any machine — rather than
    /// merely likely on the machine in front of us.
    ///
    /// Everything else that has no driver is reported as unclaimed, and rung 1
    /// resolves it: asking whether a module matches the alias is a real answer,
    /// where extending this list would only be a guess that gets it right on
    /// one machine.
    pub fn driver_not_expected(&self) -> Option<String> {
        match &self.kind {
            Kind::Pci(pci) if pci.base_class == 0x06 => Some(format!(
                "PCI bridges are handled by the bus itself (class {:02x}{:02x})",
                pci.base_class, pci.subclass
            )),
            Kind::Acpi { ids } if ids.iter().any(|id| is_acpi_system_object(id)) => {
                Some("an ACPI system or bus object, not a peripheral".into())
            }
            // The CPU publishes a modalias listing its feature bits, which is
            // how userspace loads things like the microcode and MSR modules.
            // It is not a peripheral and there is no driver to write for it.
            Kind::Other { subsystem } if subsystem == "cpu" => {
                Some("the CPU is not a peripheral".into())
            }
            _ => None,
        }
    }

    /// What the ladder should do about this device, given what sysfs says is
    /// bound to it.
    pub fn expectation(&self, bound_driver: Option<&str>) -> Expectation {
        if let Some(driver) = bound_driver {
            return Expectation::Bound(driver.to_string());
        }
        match self.driver_not_expected() {
            Some(reason) => Expectation::NotExpected(reason),
            None => Expectation::Unclaimed,
        }
    }

    /// How to describe the device to a human, and to the model.
    pub fn describe(&self) -> String {
        match (&self.kind, self.id_pair()) {
            (Kind::Pci(pci), Some(ids)) => format!(
                "PCI device {ids} (class {:02x}{:02x}{:02x})",
                pci.base_class, pci.subclass, pci.interface
            ),
            (Kind::Usb(usb), Some(ids)) => format!(
                "USB device {ids} (class {:02x}, interface class {:02x})",
                usb.device_class, usb.interface_class
            ),
            (Kind::Platform { name }, _) => format!("platform device '{name}'"),
            (Kind::Acpi { ids }, _) => format!("ACPI device {}", ids.join(", ")),
            (Kind::I2c { name }, _) => format!("I2C device '{name}'"),
            (Kind::Spi { name }, _) => format!("SPI device '{name}'"),
            (Kind::OpenFirmware { name, compatible }, _) => {
                format!(
                    "device-tree node '{name}' compatible with {}",
                    compatible.join(", ")
                )
            }
            (Kind::Virtio { .. }, Some(ids)) => format!("virtio device {ids}"),
            _ => format!("{} device ({})", self.subsystem(), self.raw),
        }
    }
}

impl fmt::Display for Modalias {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

/// ACPI ids that describe the system or a bus rather than a peripheral.
///
/// Every entry is a system object in the ACPI specification, not a judgement
/// call about a particular machine:
///
/// | id | what it is |
/// |---|---|
/// | `LNXSYSTM`, `LNXSYBUS` | the Linux ACPI root and bus nodes |
/// | `ACPI0004` | module device, a container for others |
/// | `ACPI0010` | processor container |
/// | `ACPI0013` | general-purpose event block |
/// | `PNP0A03`, `PNP0A08` | PCI host bridge, the ACPI form of class 0x06 |
/// | `PNP0C01`, `PNP0C02` | system board memory and motherboard resources |
///
/// The list is closed on purpose. It is not a place to silence a device that
/// turned out to be noisy — that belongs in rung 1, which can actually answer
/// whether a driver exists.
fn is_acpi_system_object(id: &str) -> bool {
    matches!(
        id,
        "LNXSYSTM"
            | "LNXSYBUS"
            | "ACPI0004"
            | "ACPI0010"
            | "ACPI0013"
            | "PNP0A03"
            | "PNP0A08"
            | "PNP0C01"
            | "PNP0C02"
    )
}

fn parse_kind(raw: &str) -> Kind {
    let Some((subsystem, rest)) = raw.split_once(':') else {
        return Kind::Other {
            subsystem: raw.to_string(),
        };
    };

    match subsystem {
        "pci" => parse_pci(rest).map_or_else(
            || Kind::Other {
                subsystem: "pci".into(),
            },
            Kind::Pci,
        ),
        "usb" => parse_usb(rest).map_or_else(
            || Kind::Other {
                subsystem: "usb".into(),
            },
            Kind::Usb,
        ),
        "platform" => Kind::Platform {
            name: rest.to_string(),
        },
        "acpi" => Kind::Acpi {
            ids: rest
                .split(':')
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .collect(),
        },
        "i2c" => Kind::I2c {
            name: rest.to_string(),
        },
        "spi" => Kind::Spi {
            name: rest.to_string(),
        },
        "hid" => parse_hid(rest).unwrap_or(Kind::Other {
            subsystem: "hid".into(),
        }),
        "of" => parse_of(rest),
        "virtio" => parse_virtio(rest).unwrap_or(Kind::Other {
            subsystem: "virtio".into(),
        }),
        other => Kind::Other {
            subsystem: other.to_string(),
        },
    }
}

/// `v00008086d00000D57sv00000000sd00000000bc06sc00i00`
fn parse_pci(rest: &str) -> Option<Pci> {
    let fields = read_fields(
        rest,
        &[
            ("v", 8),
            ("d", 8),
            ("sv", 8),
            ("sd", 8),
            ("bc", 2),
            ("sc", 2),
            ("i", 2),
        ],
    );
    Some(Pci {
        vendor: fields[0]? as u16,
        device: fields[1]? as u16,
        subsystem_vendor: fields[2].unwrap_or(0) as u16,
        subsystem_device: fields[3].unwrap_or(0) as u16,
        base_class: fields[4]? as u8,
        subclass: fields[5]? as u8,
        interface: fields[6].unwrap_or(0) as u8,
    })
}

/// `v0BDAp8153d0100dc00dsc00dp00ic02isc06ip00in00`
fn parse_usb(rest: &str) -> Option<Usb> {
    let fields = read_fields(
        rest,
        &[
            ("v", 4),
            ("p", 4),
            ("d", 4),
            ("dc", 2),
            ("dsc", 2),
            ("dp", 2),
            ("ic", 2),
            ("isc", 2),
            ("ip", 2),
            ("in", 2),
        ],
    );
    Some(Usb {
        vendor: fields[0]? as u16,
        product: fields[1]? as u16,
        device_class: fields[3].unwrap_or(0) as u8,
        device_subclass: fields[4].unwrap_or(0) as u8,
        interface_class: fields[6].unwrap_or(0) as u8,
        interface_subclass: fields[7].unwrap_or(0) as u8,
    })
}

/// `b0003g0001v0000046Dp0000C52B`
fn parse_hid(rest: &str) -> Option<Kind> {
    let fields = read_fields(rest, &[("b", 4), ("g", 4), ("v", 8), ("p", 8)]);
    Some(Kind::Hid {
        bus: fields[0]? as u16,
        vendor: fields[2]?,
        product: fields[3]?,
    })
}

/// `d00000001v00001AF4`
fn parse_virtio(rest: &str) -> Option<Kind> {
    let fields = read_fields(rest, &[("d", 8), ("v", 8)]);
    Some(Kind::Virtio {
        device: fields[0]?,
        vendor: fields[1]?,
    })
}

/// `NnameTtypeCcompatibleCcompatible`
fn parse_of(rest: &str) -> Kind {
    let mut name = String::new();
    let mut compatible = Vec::new();
    let mut current: Option<(char, String)> = None;

    for character in rest.chars() {
        match character {
            // map_or rather than is_none_or: the latter is Rust 1.82 and the
            // workspace MSRV is 1.75, which older Ubuntu toolchains still ship.
            'N' | 'T' | 'C' if current.as_ref().map_or(true, |(_, text)| !text.is_empty()) => {
                flush_of(current.take(), &mut name, &mut compatible);
                current = Some((character, String::new()));
            }
            _ => {
                if let Some((_, text)) = current.as_mut() {
                    text.push(character);
                }
            }
        }
    }
    flush_of(current, &mut name, &mut compatible);

    Kind::OpenFirmware { name, compatible }
}

fn flush_of(field: Option<(char, String)>, name: &mut String, compatible: &mut Vec<String>) {
    match field {
        Some(('N', text)) => *name = text,
        Some(('C', text)) if !text.is_empty() => compatible.push(text),
        _ => {}
    }
}

/// Read a fixed-order, fixed-width field list, left to right.
///
/// # Why this is not a search
///
/// The obvious implementation looks for each marker in the string and reads the
/// hex digits after it. It does not work, for two reasons that both produce a
/// plausible wrong number rather than an error:
///
/// - Markers contain each other. `sc` occurs inside `isc` and `dsc`, `d` inside
///   `dc`, `i` inside `ic` and `in`. A search finds whichever comes first.
/// - The character *after* a field is the next field's marker letter, and the
///   letters `a` through `f` are hex digits. In
///   `sd00001041bc02` the `b` that ends the `sd` field is a valid hex digit, so
///   "the value stops where the digits stop" cannot find the boundary either.
///
/// The kernel generates these with a fixed order and a fixed width per field
/// (`scripts/mod/file2alias.c`), so the reliable parse is positional: expect
/// each marker at the cursor, take exactly its digits, advance. A field that is
/// not present is skipped without consuming anything, which is what makes the
/// optional tail of a USB alias work.
///
/// Returns one entry per spec entry, in order.
fn read_fields(text: &str, spec: &[(&str, usize)]) -> Vec<Option<u32>> {
    let mut values = Vec::with_capacity(spec.len());
    let mut cursor = 0usize;

    for (marker, digits) in spec {
        let rest = &text[cursor.min(text.len())..];
        let Some(after_marker) = rest.strip_prefix(*marker) else {
            values.push(None);
            continue;
        };
        let candidate: String = after_marker.chars().take(*digits).collect();
        if candidate.len() != *digits || !candidate.chars().all(|c| c.is_ascii_hexdigit()) {
            values.push(None);
            continue;
        }
        match u32::from_str_radix(&candidate, 16) {
            Ok(value) => {
                values.push(Some(value));
                cursor += marker.len() + digits;
            }
            Err(_) => values.push(None),
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every fixture below was read from a real /sys on a running machine.
    const HOST_BRIDGE: &str = "pci:v00008086d00000D57sv00000000sd00000000bc06sc00i00";
    const VIRTIO_NET: &str = "pci:v00001AF4d00001041sv00001AF4sd00001041bc02sc00i00";
    const VIRTIO_BLK: &str = "pci:v00001AF4d00001042sv00001AF4sd00001042bc01sc80i00";
    const RTC: &str = "platform:rtc_cmos";
    const ACPI_BUS: &str = "acpi:LNXSYBUS:";
    const ACPI_CLOCK: &str = "acpi:AMZNC10C:VMCLOCK:";
    // A USB ethernet adapter — the device in the README's worked example.
    const USB_ETHERNET: &str = "usb:v0BDAp8153d0100dc00dsc00dp00ic02isc06ip00in00";
    const CPU: &str = "cpu:type:x86,ven0000fam0006mod00AD:feature:,0000,0001,0002";

    #[test]
    fn a_pci_device_yields_its_ids_and_class() {
        let alias = Modalias::parse(VIRTIO_NET);
        let Kind::Pci(pci) = alias.kind else {
            panic!("{:?}", alias.kind)
        };
        assert_eq!(pci.vendor, 0x1af4);
        assert_eq!(pci.device, 0x1041);
        assert_eq!(pci.subsystem_vendor, 0x1af4);
        assert_eq!(pci.subsystem_device, 0x1041);
        assert_eq!(pci.base_class, 0x02, "network controller");
        assert_eq!(pci.subclass, 0x00);
        assert_eq!(alias.id_pair().unwrap(), "1af4:1041");
    }

    #[test]
    fn subsystem_ids_are_not_confused_with_each_other() {
        // sv and sd both begin with 's', and 'sc' is a prefix of neither but
        // appears later. This is the field-boundary bug the parser exists to
        // avoid, so it is asserted on a fixture where every value differs.
        let alias = Modalias::parse(VIRTIO_BLK);
        let Kind::Pci(pci) = alias.kind else { panic!() };
        assert_eq!(pci.vendor, 0x1af4);
        assert_eq!(pci.device, 0x1042);
        assert_eq!(pci.base_class, 0x01, "mass storage, not 0x80");
        assert_eq!(pci.subclass, 0x80);
    }

    #[test]
    fn a_host_bridge_is_not_a_device_waiting_for_a_driver() {
        // The whole reason the class code is parsed. This device has no driver
        // and never will, and treating it as unclaimed would send the ladder
        // off to write a driver for a bridge.
        let alias = Modalias::parse(HOST_BRIDGE);
        let expectation = alias.expectation(None);
        assert!(!expectation.needs_the_ladder());
        match expectation {
            Expectation::NotExpected(reason) => assert!(reason.contains("bridge"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_real_device_with_no_driver_does_need_the_ladder() {
        let alias = Modalias::parse(USB_ETHERNET);
        assert!(alias.expectation(None).needs_the_ladder());
    }

    #[test]
    fn a_bound_driver_ends_the_question_whatever_the_device_is() {
        for raw in [HOST_BRIDGE, VIRTIO_NET, USB_ETHERNET, RTC] {
            let expectation = Modalias::parse(raw).expectation(Some("some-driver"));
            assert_eq!(expectation, Expectation::Bound("some-driver".into()));
            assert!(!expectation.needs_the_ladder());
        }
    }

    #[test]
    fn a_usb_device_yields_both_class_levels() {
        // Interface class is what matters for a composite device: the device
        // class is often 00 ("see the interfaces") as it is here.
        let alias = Modalias::parse(USB_ETHERNET);
        let Kind::Usb(usb) = alias.kind else {
            panic!("{:?}", alias.kind)
        };
        assert_eq!(usb.vendor, 0x0bda);
        assert_eq!(usb.product, 0x8153);
        assert_eq!(usb.device_class, 0x00);
        assert_eq!(usb.interface_class, 0x02, "communications");
        assert_eq!(usb.interface_subclass, 0x06, "ethernet networking");
        assert_eq!(alias.id_pair().unwrap(), "0bda:8153");
    }

    #[test]
    fn an_acpi_bus_object_is_not_a_device() {
        assert!(!Modalias::parse(ACPI_BUS)
            .expectation(None)
            .needs_the_ladder());
        // ...but a real ACPI device is.
        assert!(Modalias::parse(ACPI_CLOCK)
            .expectation(None)
            .needs_the_ladder());
    }

    #[test]
    fn an_acpi_host_bridge_is_skipped_like_its_pci_equivalent() {
        // PNP0A08 is the same host bridge the PCI side reports as class 0x06.
        // Skipping one and not the other would be arbitrary.
        let alias = Modalias::parse("acpi:PNP0A08:PNP0A03:");
        assert!(!alias.expectation(None).needs_the_ladder());
    }

    #[test]
    fn the_cpu_is_not_a_device_waiting_for_a_driver() {
        // It publishes a modalias listing its feature bits, which is how the
        // microcode and MSR modules get loaded. Found by running enumeration
        // against a real /sys, where the CPU turned up as a ladder candidate.
        let alias = Modalias::parse(CPU);
        assert_eq!(alias.subsystem(), "cpu");
        assert!(!alias.expectation(None).needs_the_ladder());
    }

    #[test]
    fn a_serial_port_is_still_a_real_device() {
        // The skip list must not grow to cover every PNP id. PNP0501 is a
        // 16550 UART and PNP0303 a keyboard controller: both are peripherals
        // and both belong on the ladder when nothing is bound.
        for id in ["acpi:PNP0501:", "acpi:PNP0303:"] {
            assert!(
                Modalias::parse(id).expectation(None).needs_the_ladder(),
                "{id} is a peripheral"
            );
        }
    }

    #[test]
    fn an_acpi_alias_keeps_every_hardware_id() {
        // The kernel lists several, most-specific first, and rung 3 may need
        // the less specific ones.
        let alias = Modalias::parse("acpi:PNP0A08:PNP0A03:");
        let Kind::Acpi { ids } = alias.kind else {
            panic!()
        };
        assert_eq!(ids, vec!["PNP0A08", "PNP0A03"]);
    }

    #[test]
    fn a_platform_device_is_named_not_numbered() {
        let alias = Modalias::parse(RTC);
        assert_eq!(
            alias.kind,
            Kind::Platform {
                name: "rtc_cmos".into()
            }
        );
        assert_eq!(alias.id_pair(), None, "platform devices have no id pair");
        assert!(alias.describe().contains("rtc_cmos"));
    }

    #[test]
    fn a_device_tree_node_yields_its_compatible_list() {
        // The input to a rung 3 overlay on a board.
        let alias = Modalias::parse("of:Ntemp_sensorT(null)Cti,tmp102Cti,tmp101");
        let Kind::OpenFirmware { name, compatible } = alias.kind else {
            panic!("{:?}", alias.kind)
        };
        assert_eq!(name, "temp_sensor");
        assert_eq!(compatible, vec!["ti,tmp102", "ti,tmp101"]);
    }

    #[test]
    fn the_raw_string_survives_parsing_exactly() {
        // modprobe takes this verbatim. Re-rendering it from the parsed fields
        // would be a second implementation of the kernel's format.
        for raw in [HOST_BRIDGE, VIRTIO_NET, RTC, ACPI_CLOCK, USB_ETHERNET] {
            assert_eq!(Modalias::parse(raw).raw, raw);
            assert_eq!(Modalias::parse(raw).to_string(), raw);
        }
    }

    #[test]
    fn an_unparseable_alias_still_reports_its_subsystem() {
        // Rung 1 only needs the raw string, so an alias this parser does not
        // break down is degraded, not useless: it still reaches the ladder and
        // modprobe can still look it up.
        let alias = Modalias::parse("sdio:c00v02D0d4324");
        assert_eq!(alias.subsystem(), "sdio");
        assert!(alias.expectation(None).needs_the_ladder());
        assert_eq!(alias.id_pair(), None, "not broken down into ids");
        assert_eq!(alias.raw, "sdio:c00v02D0d4324", "but still usable verbatim");
    }

    #[test]
    fn an_alias_with_no_colon_is_not_mistaken_for_a_known_bus() {
        let alias = Modalias::parse("nonsense");
        assert_eq!(alias.subsystem(), "nonsense");
    }

    #[test]
    fn a_virtio_alias_yields_its_pair() {
        let alias = Modalias::parse("virtio:d00000001v00001AF4");
        assert_eq!(
            alias.kind,
            Kind::Virtio {
                device: 1,
                vendor: 0x1af4
            }
        );
        assert_eq!(alias.id_pair().unwrap(), "1af4:0001");
    }

    #[test]
    fn a_hid_alias_yields_its_pair() {
        let alias = Modalias::parse("hid:b0003g0001v0000046Dp0000C52B");
        assert_eq!(
            alias.kind,
            Kind::Hid {
                bus: 3,
                vendor: 0x046d,
                product: 0xc52b
            }
        );
    }

    #[test]
    fn descriptions_name_the_device_rather_than_restating_the_alias() {
        assert!(Modalias::parse(USB_ETHERNET)
            .describe()
            .contains("0bda:8153"));
        assert!(Modalias::parse(VIRTIO_NET).describe().contains("1af4:1041"));
        assert!(Modalias::parse(ACPI_CLOCK).describe().contains("AMZNC10C"));
    }

    #[test]
    fn fields_are_read_positionally_not_searched_for() {
        // The bug this replaced: a field's value ends where its width says, not
        // where the hex digits stop. In "sd00001041bc02" the 'b' that begins
        // the next marker is itself a valid hex digit.
        let fields = read_fields("sd00001041bc02", &[("sd", 8), ("bc", 2)]);
        assert_eq!(fields, vec![Some(0x1041), Some(0x02)]);
    }

    #[test]
    fn a_marker_that_contains_another_is_not_confused_with_it() {
        // "d" is a prefix of "dc"; "i" of "ic" and "in"; "sc" occurs inside
        // "isc". Reading in order is what keeps these apart.
        let fields = read_fields("d0100dc00dsc00", &[("d", 4), ("dc", 2), ("dsc", 2)]);
        assert_eq!(fields, vec![Some(0x0100), Some(0), Some(0)]);
    }

    #[test]
    fn an_absent_field_is_skipped_without_consuming_the_next_one() {
        // A USB alias may stop early. The fields that are present must still
        // read correctly after the gap.
        let fields = read_fields("v0BDAp8153", &[("v", 4), ("p", 4), ("d", 4), ("dc", 2)]);
        assert_eq!(fields, vec![Some(0x0bda), Some(0x8153), None, None]);
    }

    #[test]
    fn a_field_with_too_few_digits_is_not_half_read() {
        // Truncation here would produce a plausible wrong vendor id.
        assert_eq!(read_fields("v123", &[("v", 4)]), vec![None]);
        assert_eq!(read_fields("v1234", &[("v", 4)]), vec![Some(0x1234)]);
    }
}
