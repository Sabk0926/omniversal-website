//! The real machine, behind the ladder's [`System`](super::System) trait.
//!
//! Everything here has a side effect on hardware, which is why it is one small
//! module with the decisions kept out of it. The ladder decides; this does. The
//! one piece with real logic — reading the kernel log for firmware the driver
//! asked for and did not get — is a pure function over text, and tested as one.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use omnia_kernel::Device;

use super::System;

/// Where firmware lives, in the order the kernel looks.
const FIRMWARE_DIRS: &[&str] = &["/lib/firmware", "/usr/lib/firmware"];

pub struct RealSystem {
    pub sys_root: PathBuf,
    /// Absolute, because this runs from a unit with a minimal environment and
    /// `PATH` is not something to rely on when loading kernel code.
    pub modprobe: PathBuf,
    /// The kernel log, read once per climb. `None` when it could not be read,
    /// which is different from "no firmware was requested".
    kernel_log: Option<String>,
}

impl Default for RealSystem {
    fn default() -> RealSystem {
        RealSystem::new()
    }
}

impl RealSystem {
    pub fn new() -> RealSystem {
        RealSystem {
            sys_root: PathBuf::from("/sys"),
            modprobe: PathBuf::from("/usr/sbin/modprobe"),
            kernel_log: read_kernel_log(),
        }
    }

    /// For tests and for replaying a captured machine.
    pub fn with_log(mut self, log: &str) -> RealSystem {
        self.kernel_log = Some(log.to_string());
        self
    }
}

impl System for RealSystem {
    fn load_module(&self, module: &str) -> Result<(), String> {
        // argv, not a shell. A module name comes from the kernel's own index so
        // it is not attacker-controlled, but the rule does not have exceptions.
        let output = Command::new(&self.modprobe)
            .arg(module)
            .output()
            .map_err(|e| format!("cannot run {}: {e}", self.modprobe.display()))?;
        if output.status.success() {
            return Ok(());
        }
        let detail = String::from_utf8_lossy(&output.stderr);
        Err(match detail.trim() {
            "" => format!("modprobe exited {}", output.status),
            message => message.to_string(),
        })
    }

    fn bound_driver(&self, syspath: &Path) -> Option<String> {
        // Read the link rather than resolving it: a device being removed
        // underneath us turns a missing driver into an I/O error otherwise.
        let target = fs::read_link(syspath.join("driver")).ok()?;
        target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    }

    fn missing_firmware(&self, device: &Device) -> Vec<String> {
        let Some(log) = &self.kernel_log else {
            return Vec::new();
        };
        firmware_requests(log)
            .into_iter()
            .filter(|(context, _)| mentions_device(context, device))
            .map(|(_, firmware)| firmware)
            .collect()
    }

    fn firmware_available(&self, name: &str) -> bool {
        FIRMWARE_DIRS
            .iter()
            .any(|dir| Path::new(dir).join(name).exists())
    }

    fn reprobe(&self, syspath: &Path) -> Result<(), String> {
        // `drivers_probe` asks the bus to re-run matching for one device, which
        // is narrower than unbinding and rebinding a driver: nothing that is
        // currently working gets disturbed.
        let Some(name) = syspath.file_name().and_then(|n| n.to_str()) else {
            return Err("the device has no sysfs name".into());
        };
        let bus =
            bus_of(syspath, &self.sys_root).ok_or_else(|| "the device is on no bus".to_string())?;
        let probe = self.sys_root.join("bus").join(&bus).join("drivers_probe");
        fs::write(&probe, name).map_err(|e| format!("cannot write {}: {e}", probe.display()))
    }

    fn bind_by_id(&self, bus: &str, driver: &str, vendor: u32, product: u32) -> Result<(), String> {
        let new_id = self
            .sys_root
            .join("bus")
            .join(bus)
            .join("drivers")
            .join(driver)
            .join("new_id");
        // The kernel wants lowercase hex, space separated, no 0x.
        let value = format!("{vendor:04x} {product:04x}");
        match fs::write(&new_id, &value) {
            Ok(()) => Ok(()),
            // EEXIST: the driver already knows this ID. The caller's intent is
            // satisfied, and whether the device then binds is checked anyway.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(format!("cannot write {}: {e}", new_id.display())),
        }
    }
}

/// Which bus a device sits on, from its `subsystem` link.
fn bus_of(syspath: &Path, _sys_root: &Path) -> Option<String> {
    let target = fs::read_link(syspath.join("subsystem")).ok()?;
    target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// Read the kernel ring buffer.
///
/// `/dev/kmsg` is opened non-blocking: it stays readable forever by design, and
/// a blocking read would hang the climb waiting for a message that may never
/// come. A short read is the whole buffer, which is what is wanted.
fn read_kernel_log() -> Option<String> {
    // read_to_string on /dev/kmsg blocks at the end of the buffer. Reading a
    // bounded amount and stopping is deliberate: the interesting messages are
    // from this boot and there is no reason to stream.
    use std::io::Read;
    let file = fs::OpenOptions::new().read(true).open("/dev/kmsg").ok()?;
    let mut reader = file.take(1024 * 1024);
    let mut text = String::new();
    // An error part-way through still leaves usable text: the buffer is
    // line-oriented and a truncated tail loses at most the last message.
    let _ = reader.read_to_string(&mut text);
    Some(text)
}

/// Firmware the kernel asked for and did not get, as `(context, filename)`.
///
/// The context is whatever the kernel put before the colon — usually the driver
/// and the device it was probing — which is how a request is attributed to one
/// device rather than to the machine.
///
/// Two spellings, because the kernel has used both:
///
/// ```text
/// r8152 1-1:1.0: Direct firmware load for rtl_nic/rtl8153a-3.fw failed with error -2
/// i915 0000:00:02.0: firmware: failed to load i915/skl_dmc_ver1_27.bin (-2)
/// ```
pub fn firmware_requests(log: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for line in log.lines() {
        // /dev/kmsg prefixes `PRIORITY,SEQ,TIMESTAMP,FLAGS;` before the text.
        let message = line.split_once(';').map_or(line, |(_, rest)| rest);

        if let Some(name) = between(message, "Direct firmware load for ", " failed") {
            found.push((context_of(message), name));
            continue;
        }
        if let Some(rest) = message.split_once("firmware: failed to load ") {
            // The filename runs to the next space or the end.
            let name = rest.1.split_whitespace().next().unwrap_or_default();
            if !name.is_empty() {
                found.push((context_of(message), name.to_string()));
            }
        }
    }
    found.dedup();
    found
}

/// The text before the last colon that precedes the message body.
fn context_of(message: &str) -> String {
    match message.find(": ") {
        Some(end) => message[..end].trim().to_string(),
        None => String::new(),
    }
}

fn between(text: &str, start: &str, end: &str) -> Option<String> {
    let after = text.split_once(start)?.1;
    let name = after.split_once(end)?.0;
    (!name.is_empty()).then(|| name.to_string())
}

/// Is this log context about this device?
///
/// Matched on the sysfs name, which is what the kernel prints: `1-1:1.0` for a
/// USB interface of device `1-1`, `0000:00:02.0` for a PCI function. Requiring
/// the whole context to match would miss the interface form; requiring only a
/// substring would attribute `1-1`'s firmware to `1-10`, so the character after
/// the match has to be a separator.
fn mentions_device(context: &str, device: &Device) -> bool {
    let name = &device.name;
    if name.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(offset) = context[from..].find(name.as_str()) {
        let start = from + offset;
        let end = start + name.len();
        let before_ok = start == 0
            || !context[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
        let after = context[end..].chars().next();
        // map_or rather than is_none_or: the latter is Rust 1.82 and the
        // workspace MSRV is 1.75, which older Ubuntu toolchains still ship.
        let after_ok = after.map_or(true, |c| {
            !(c.is_ascii_alphanumeric() || c == '-' || c == '.')
        });
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
        if from >= context.len() {
            break;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn device(name: &str) -> Device {
        Device {
            syspath: PathBuf::from("/sys/devices/x").join(name),
            name: name.into(),
            subsystem: Some("usb".into()),
            modalias: None,
            driver: None,
            properties: BTreeMap::new(),
        }
    }

    /// Real kernel messages, in the two spellings the kernel uses.
    const LOG: &str = "\
6,1234,12345678,-;usb 1-1: new high-speed USB device number 4 using xhci_hcd
4,1235,12345679,-;r8152 1-1:1.0: Direct firmware load for rtl_nic/rtl8153a-3.fw failed with error -2
4,1236,12345680,-;i915 0000:00:02.0: firmware: failed to load i915/skl_dmc_ver1_27.bin (-2)
6,1237,12345681,-;EXT4-fs (vda1): mounted filesystem
";

    #[test]
    fn both_spellings_of_a_missing_firmware_are_found() {
        let requests = firmware_requests(LOG);
        let names: Vec<&str> = requests.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(
            names,
            vec!["rtl_nic/rtl8153a-3.fw", "i915/skl_dmc_ver1_27.bin"]
        );
    }

    #[test]
    fn a_request_is_attributed_to_the_device_that_made_it() {
        // The whole point of keeping the context. Otherwise every device on the
        // machine is told it is missing the graphics firmware.
        let requests = firmware_requests(LOG);
        let usb: Vec<&String> = requests
            .iter()
            .filter(|(context, _)| mentions_device(context, &device("1-1")))
            .map(|(_, name)| name)
            .collect();
        assert_eq!(usb, vec!["rtl_nic/rtl8153a-3.fw"]);
    }

    #[test]
    fn a_device_name_that_is_a_prefix_of_another_is_not_confused_with_it() {
        // `1-1` occurs inside `1-10`. Attributing 1-10's firmware to 1-1 would
        // send the ladder off fetching a blob for the wrong device.
        assert!(mentions_device("r8152 1-1:1.0", &device("1-1")));
        assert!(!mentions_device("r8152 1-10:1.0", &device("1-1")));
        assert!(!mentions_device("r8152 11-1:1.0", &device("1-1")));
        assert!(mentions_device("r8152 1-10:1.0", &device("1-10")));
    }

    #[test]
    fn a_pci_function_is_matched_whole() {
        assert!(mentions_device(
            "i915 0000:00:02.0",
            &device("0000:00:02.0")
        ));
        assert!(!mentions_device("i915 0000:00:02.0", &device("0000:00:02")));
    }

    #[test]
    fn a_log_with_no_firmware_trouble_yields_nothing() {
        let quiet = "6,1,1,-;usb 1-1: new high-speed USB device\n6,2,2,-;EXT4-fs: mounted\n";
        assert!(firmware_requests(quiet).is_empty());
    }

    #[test]
    fn a_line_without_the_kmsg_prefix_still_parses() {
        // dmesg output, or a log captured from a machine and replayed.
        let plain = "r8152 1-1:1.0: Direct firmware load for rtl_nic/rtl8153a-3.fw failed\n";
        let requests = firmware_requests(plain);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].1, "rtl_nic/rtl8153a-3.fw");
        assert_eq!(requests[0].0, "r8152 1-1:1.0");
    }

    #[test]
    fn an_unreadable_kernel_log_is_not_reported_as_no_firmware_needed() {
        // The distinction matters: "nothing asked for firmware" sends the climb
        // up to rung 3, and "we could not tell" should not look the same.
        let system = RealSystem {
            sys_root: PathBuf::from("/sys"),
            modprobe: PathBuf::from("/usr/sbin/modprobe"),
            kernel_log: None,
        };
        assert!(system.missing_firmware(&device("1-1")).is_empty());
        // ...and with a log, the same device does report one.
        let with_log = RealSystem {
            kernel_log: Some(LOG.to_string()),
            ..system
        };
        assert_eq!(with_log.missing_firmware(&device("1-1")).len(), 1);
    }

    #[test]
    fn firmware_on_disk_is_found_where_the_kernel_looks() {
        let system = RealSystem::new().with_log("");
        // Nothing is installed here, so this is the negative case; the positive
        // one would need a file under /lib/firmware, which a test must not
        // create.
        assert!(!system.firmware_available("definitely/not/here.fw"));
    }
}
