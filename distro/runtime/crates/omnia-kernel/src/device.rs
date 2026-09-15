//! Reading devices out of sysfs.
//!
//! # What counts as a device
//!
//! `/sys/devices` holds a lot of things that are not devices: bus roots, ACPI
//! containers, power-management objects, per-CPU nodes. The one that matters
//! here is narrower — anything with a `modalias`, because that is the kernel
//! publishing "this is what would match me", which is exactly the set the
//! driver ladder can act on.
//!
//! # Everything takes a root
//!
//! Every function here is given the sysfs root rather than hard-coding `/sys`.
//! That is not just for tests: a device tree can be captured from a failing
//! board and replayed here, which is the difference between debugging a driver
//! decision and guessing about one.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::modalias::{Expectation, Modalias};

/// The default sysfs mount. Callers that do not need to override it use this.
pub const SYS_ROOT: &str = "/sys";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub syspath: PathBuf,
    /// The directory name, e.g. `0000:00:08.0` or `1-1.4`.
    pub name: String,
    /// From the `subsystem` symlink: `pci`, `usb`, `platform`, `i2c`.
    pub subsystem: Option<String>,
    pub modalias: Option<Modalias>,
    /// The bound driver, from the `driver` symlink. `None` means unclaimed.
    pub driver: Option<String>,
    /// The `uevent` file, parsed. Carries things the symlinks do not, such as
    /// `PCI_SLOT_NAME` and `DEVTYPE`.
    pub properties: BTreeMap<String, String>,
}

impl Device {
    /// Read one device directory.
    pub fn read(syspath: &Path) -> io::Result<Device> {
        let name = syspath
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();

        Ok(Device {
            name,
            subsystem: link_target_name(&syspath.join("subsystem")),
            modalias: read_trimmed(&syspath.join("modalias")).map(|raw| Modalias::parse(&raw)),
            driver: link_target_name(&syspath.join("driver")),
            properties: read_trimmed(&syspath.join("uevent"))
                .map(|text| parse_uevent_file(&text))
                .unwrap_or_default(),
            syspath: syspath.to_path_buf(),
        })
    }

    /// What the ladder should do about this device.
    ///
    /// A device with no modalias is `NotExpected`: the kernel has not published
    /// anything to match on, so there is nothing for `modprobe` to look up and
    /// nothing to identify it to a driver index either.
    pub fn expectation(&self) -> Expectation {
        match &self.modalias {
            Some(alias) => alias.expectation(self.driver.as_deref()),
            None => match &self.driver {
                Some(driver) => Expectation::Bound(driver.clone()),
                None => Expectation::NotExpected(
                    "no modalias, so the kernel publishes nothing to match on".into(),
                ),
            },
        }
    }

    pub fn needs_a_driver(&self) -> bool {
        self.expectation().needs_the_ladder()
    }

    /// A human-readable name, best available. Falls back through the vendor and
    /// product strings USB provides, then the uevent, then the alias.
    pub fn describe(&self) -> String {
        let identity = self
            .modalias
            .as_ref()
            .map(Modalias::describe)
            .unwrap_or_else(|| {
                format!("{} device", self.subsystem.as_deref().unwrap_or("unknown"))
            });

        match self.product_name() {
            Some(product) => format!("{product} ({identity})"),
            None => identity,
        }
    }

    /// The vendor and product strings, when the bus provides them. USB does;
    /// PCI does not, which is why `lspci` ships a database.
    fn product_name(&self) -> Option<String> {
        let product = read_trimmed(&self.syspath.join("product"))?;
        match read_trimmed(&self.syspath.join("manufacturer")) {
            Some(manufacturer) => Some(format!("{manufacturer} {product}")),
            None => Some(product),
        }
    }

    /// `vendor:product`, for a search or a driver index lookup.
    pub fn id_pair(&self) -> Option<String> {
        self.modalias.as_ref().and_then(Modalias::id_pair)
    }
}

/// Every device under `root` that publishes a modalias.
///
/// Depth-limited because `/sys/devices` contains symlink loops back up the
/// tree through `subsystem` and `driver`, and an unbounded walk following
/// directories will find the same device at a dozen paths. Symlinks are not
/// followed at all here for the same reason — a device's canonical location is
/// the real directory, and every other path to it is an alias.
pub fn enumerate(root: &Path) -> Vec<Device> {
    let mut devices = Vec::new();
    walk(&root.join("devices"), 0, &mut devices);
    devices.sort_by(|a, b| a.syspath.cmp(&b.syspath));
    devices
}

/// Devices that have no driver and should have one. The ladder's input.
pub fn unclaimed(root: &Path) -> Vec<Device> {
    enumerate(root)
        .into_iter()
        .filter(Device::needs_a_driver)
        .collect()
}

const MAX_DEPTH: usize = 12;

fn walk(dir: &Path, depth: usize, found: &mut Vec<Device>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        // `file_type` does not follow the link, which is what stops the walk
        // from leaving /sys/devices through a subsystem symlink.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() {
            continue;
        }
        if path.join("modalias").is_file() {
            if let Ok(device) = Device::read(&path) {
                found.push(device);
            }
        }
        walk(&path, depth + 1, found);
    }
}

/// `KEY=VALUE` lines, as `/sys/.../uevent` and a netlink message both use.
pub fn parse_uevent_file(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect()
}

/// The basename a symlink points at: `../../../bus/pci` becomes `pci`.
///
/// Read rather than resolved. Resolving would touch the target, which for a
/// device that is being removed underneath us turns a missing driver into an
/// I/O error at exactly the wrong moment.
fn link_target_name(link: &Path) -> Option<String> {
    let target = fs::read_link(link).ok()?;
    target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

fn read_trimmed(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// Build a sysfs-shaped tree in a scratch directory.
    struct FakeSys {
        root: PathBuf,
    }

    impl FakeSys {
        fn new(name: &str) -> FakeSys {
            let root = std::env::temp_dir().join(format!(
                "omnia-sys-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0)
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("devices")).unwrap();
            fs::create_dir_all(root.join("bus/pci")).unwrap();
            fs::create_dir_all(root.join("bus/usb")).unwrap();
            FakeSys { root }
        }

        /// Add a device. `driver` of `None` leaves it unclaimed.
        fn device(
            &self,
            path: &str,
            modalias: &str,
            subsystem: &str,
            driver: Option<&str>,
        ) -> PathBuf {
            let dir = self.root.join("devices").join(path);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("modalias"), format!("{modalias}\n")).unwrap();
            fs::write(
                dir.join("uevent"),
                format!("MODALIAS={modalias}\nDEVTYPE=test\n"),
            )
            .unwrap();

            let bus = self.root.join("bus").join(subsystem);
            fs::create_dir_all(&bus).unwrap();
            let _ = symlink(&bus, dir.join("subsystem"));

            if let Some(driver) = driver {
                let driver_dir = bus.join("drivers").join(driver);
                fs::create_dir_all(&driver_dir).unwrap();
                let _ = symlink(&driver_dir, dir.join("driver"));
            }
            dir
        }
    }

    impl Drop for FakeSys {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn a_device_reports_its_alias_subsystem_and_driver() {
        let sys = FakeSys::new("read");
        let path = sys.device(
            "pci0000:00/0000:00:08.0",
            "pci:v00001AF4d00001041sv00001AF4sd00001041bc02sc00i00",
            "pci",
            Some("virtio-pci"),
        );

        let device = Device::read(&path).unwrap();
        assert_eq!(device.name, "0000:00:08.0");
        assert_eq!(device.subsystem.as_deref(), Some("pci"));
        assert_eq!(device.driver.as_deref(), Some("virtio-pci"));
        assert_eq!(device.id_pair().unwrap(), "1af4:1041");
        assert_eq!(device.properties.get("DEVTYPE").unwrap(), "test");
        assert!(!device.needs_a_driver(), "it already has one");
    }

    #[test]
    fn an_unclaimed_device_is_found_and_a_bridge_is_not() {
        // The distinction the whole module exists for, end to end through a
        // real directory walk.
        let sys = FakeSys::new("unclaimed");
        sys.device(
            "pci0000:00/0000:00:00.0",
            "pci:v00008086d00000D57sv00000000sd00000000bc06sc00i00",
            "pci",
            None,
        );
        sys.device(
            "pci0000:00/0000:00:04.0/usb1/1-1",
            "usb:v0BDAp8153d0100dc00dsc00dp00ic02isc06ip00in00",
            "usb",
            None,
        );
        sys.device(
            "pci0000:00/0000:00:08.0",
            "pci:v00001AF4d00001041sv00001AF4sd00001041bc02sc00i00",
            "pci",
            Some("virtio-pci"),
        );

        let unclaimed = unclaimed(&sys.root);
        assert_eq!(
            unclaimed.len(),
            1,
            "only the USB adapter: {:?}",
            unclaimed.iter().map(|d| &d.name).collect::<Vec<_>>()
        );
        assert_eq!(unclaimed[0].id_pair().unwrap(), "0bda:8153");
    }

    #[test]
    fn enumerate_finds_devices_at_every_depth() {
        let sys = FakeSys::new("depth");
        sys.device("platform/rtc_cmos", "platform:rtc_cmos", "platform", None);
        sys.device(
            "pci0000:00/0000:00:04.0/usb1/1-1/1-1.4/1-1.4:1.0",
            "usb:v0BDAp8153d0100dc00dsc00dp00ic02isc06ip00in00",
            "usb",
            None,
        );

        let devices = enumerate(&sys.root);
        assert_eq!(devices.len(), 2);
        assert!(devices.iter().any(|d| d.name == "rtc_cmos"));
        assert!(devices.iter().any(|d| d.name == "1-1.4:1.0"));
    }

    #[test]
    fn the_walk_does_not_leave_sysfs_through_a_symlink() {
        // Every device has a `subsystem` symlink pointing back up the tree.
        // Following it would find the same devices again under /sys/bus, and
        // on a real machine that is an effectively unbounded walk.
        let sys = FakeSys::new("loops");
        sys.device(
            "pci0000:00/0000:00:08.0",
            "pci:v00001AF4d00001041sv00001AF4sd00001041bc02sc00i00",
            "pci",
            Some("virtio-pci"),
        );
        // A bus-level alias to the same device, as /sys/bus/pci/devices has.
        let alias_dir = sys.root.join("bus/pci/devices");
        fs::create_dir_all(&alias_dir).unwrap();
        symlink(
            sys.root.join("devices/pci0000:00/0000:00:08.0"),
            alias_dir.join("0000:00:08.0"),
        )
        .unwrap();

        let devices = enumerate(&sys.root);
        assert_eq!(devices.len(), 1, "the device is found once, not twice");
    }

    #[test]
    fn a_device_with_no_modalias_is_not_a_ladder_candidate() {
        // /sys/devices is full of these: power objects, bus roots, containers.
        let sys = FakeSys::new("nomodalias");
        let dir = sys.root.join("devices/system/cpu/cpu0");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("uevent"), "DRIVER=processor\n").unwrap();

        assert!(
            enumerate(&sys.root).is_empty(),
            "no modalias, not enumerated"
        );

        let device = Device::read(&dir).unwrap();
        assert!(!device.needs_a_driver());
        match device.expectation() {
            Expectation::NotExpected(reason) => assert!(reason.contains("modalias"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_missing_directory_yields_nothing_rather_than_an_error() {
        // A board with no /sys/devices is a broken machine, but reading it is
        // not where that should be reported.
        assert!(enumerate(Path::new("/nonexistent-omnia-sysfs")).is_empty());
    }

    #[test]
    fn uevent_files_parse_into_properties() {
        let text =
            "DRIVER=virtio-pci\nPCI_CLASS=20000\nPCI_ID=1AF4:1041\nPCI_SLOT_NAME=0000:00:08.0\n";
        let parsed = parse_uevent_file(text);
        assert_eq!(parsed["DRIVER"], "virtio-pci");
        assert_eq!(parsed["PCI_ID"], "1AF4:1041");
        assert_eq!(parsed.len(), 4);
    }

    #[test]
    fn a_uevent_value_containing_an_equals_sign_survives() {
        // MODALIAS values do not, but OF_COMPATIBLE and firmware paths can.
        let parsed = parse_uevent_file("KEY=a=b\nOTHER=x\n");
        assert_eq!(parsed["KEY"], "a=b");
    }

    #[test]
    fn a_malformed_uevent_line_is_skipped_not_fatal() {
        let parsed = parse_uevent_file("GOOD=1\ngarbage without an equals\nALSO=2\n");
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn a_usb_device_is_described_by_its_product_strings_when_it_has_them() {
        let sys = FakeSys::new("product");
        let path = sys.device(
            "pci0000:00/usb1/1-1",
            "usb:v0BDAp8153d0100dc00dsc00dp00ic02isc06ip00in00",
            "usb",
            None,
        );
        fs::write(path.join("manufacturer"), "Realtek\n").unwrap();
        fs::write(path.join("product"), "USB 10/100/1000 LAN\n").unwrap();

        let description = Device::read(&path).unwrap().describe();
        assert!(
            description.contains("Realtek USB 10/100/1000 LAN"),
            "{description}"
        );
        assert!(description.contains("0bda:8153"), "{description}");
    }

    #[test]
    fn a_device_without_product_strings_is_still_described() {
        let sys = FakeSys::new("noproduct");
        let path = sys.device(
            "pci0000:00/0000:00:08.0",
            "pci:v00001AF4d00001041sv00001AF4sd00001041bc02sc00i00",
            "pci",
            None,
        );
        let description = Device::read(&path).unwrap().describe();
        assert!(description.contains("1af4:1041"), "{description}");
    }
}
