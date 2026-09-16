//! Ladder tests.
//!
//! The machine is faked, deliberately and completely. What is being tested is
//! the *decision*: which rung runs, in what order, what stops the climb, and
//! what gets written down. Those are the parts that can be wrong in a way that
//! wastes hours or loads the wrong code, and they are also the parts that real
//! hardware cannot exercise on demand — "a module that loads and binds nothing"
//! is a common failure and not something you can go out and buy.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use omnia_kernel::{Device, Modalias, ModuleIndex};

use super::*;

/// A machine whose responses are scripted.
#[derive(Default)]
struct FakeSystem {
    /// Modules that load successfully. Anything else refuses.
    loadable: Vec<String>,
    /// Which driver is bound, keyed by how many times it has been asked. This
    /// is what lets a test say "nothing is bound, then after the load, r8152
    /// is" without the fake having to model the kernel.
    binds_after: RefCell<BTreeMap<String, Option<String>>>,
    bound: RefCell<Option<String>>,
    missing_firmware: Vec<String>,
    firmware_present: Vec<String>,
    reprobe_fails: Option<String>,
    /// Drivers whose new_id write succeeds, and what the device binds to after.
    accepts_id: BTreeMap<String, Option<String>>,
    /// What the device binds to once it is re-probed.
    binds_on_reprobe: Option<String>,
    /// Every call, in order, so a test can assert what was *not* done.
    calls: RefCell<Vec<String>>,
}

impl FakeSystem {
    fn loads(mut self, module: &str) -> Self {
        self.loadable.push(module.to_string());
        self
    }

    /// Loading `module` succeeds and the device then binds to `driver`.
    fn binds(mut self, module: &str, driver: &str) -> Self {
        self.loadable.push(module.to_string());
        self.binds_after
            .borrow_mut()
            .insert(module.to_string(), Some(driver.to_string()));
        self
    }

    fn wants_firmware(mut self, names: &[&str]) -> Self {
        self.missing_firmware = names.iter().map(|n| (*n).to_string()).collect();
        self
    }

    fn has_firmware(mut self, names: &[&str]) -> Self {
        self.firmware_present = names.iter().map(|n| (*n).to_string()).collect();
        self
    }

    /// The device is dead until a re-probe, then it binds. This is what
    /// "firmware arrived after the driver looked" actually looks like.
    fn binds_on_reprobe(mut self, driver: &str) -> Self {
        self.binds_on_reprobe = Some(driver.to_string());
        self
    }

    /// Writing this device's ID to `driver` succeeds and it then binds.
    fn accepts_id(mut self, driver: &str, binds_to: Option<&str>) -> Self {
        self.loadable.push(driver.to_string());
        self.accepts_id
            .insert(driver.to_string(), binds_to.map(str::to_string));
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl System for FakeSystem {
    fn load_module(&self, module: &str) -> Result<(), String> {
        self.calls.borrow_mut().push(format!("load {module}"));
        if !self.loadable.contains(&module.to_string()) {
            return Err("module not found".into());
        }
        if let Some(driver) = self.binds_after.borrow().get(module) {
            *self.bound.borrow_mut() = driver.clone();
        }
        Ok(())
    }

    fn bound_driver(&self, _syspath: &Path) -> Option<String> {
        self.bound.borrow().clone()
    }

    fn missing_firmware(&self, _device: &Device) -> Vec<String> {
        self.missing_firmware.clone()
    }

    fn firmware_available(&self, name: &str) -> bool {
        self.firmware_present.iter().any(|n| n == name)
    }

    fn bind_by_id(&self, bus: &str, driver: &str, vendor: u32, product: u32) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(format!("new_id {bus}/{driver} {vendor:04x}:{product:04x}"));
        match self.accepts_id.get(driver) {
            Some(binds_to) => {
                *self.bound.borrow_mut() = binds_to.clone();
                Ok(())
            }
            None => Err("no such driver".into()),
        }
    }

    fn reprobe(&self, _syspath: &Path) -> Result<(), String> {
        self.calls.borrow_mut().push("reprobe".into());
        if let Some(e) = &self.reprobe_fails {
            return Err(e.clone());
        }
        if let Some(driver) = &self.binds_on_reprobe {
            *self.bound.borrow_mut() = Some(driver.clone());
        }
        Ok(())
    }
}

/// An index built from alias lines, as `modules.alias` holds them.
fn index_with(lines: &[&str]) -> ModuleIndex {
    let dir = scratch("index");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("modules.alias"),
        lines
            .iter()
            .map(|line| format!("alias {line}\n"))
            .collect::<String>(),
    )
    .unwrap();
    ModuleIndex::load(&dir)
}

fn builtin_index(lines: &[&str]) -> ModuleIndex {
    let dir = scratch("builtin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("modules.builtin.alias"),
        lines
            .iter()
            .map(|line| format!("alias {line}\n"))
            .collect::<String>(),
    )
    .unwrap();
    ModuleIndex::load(&dir)
}

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "omnia-ladder-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&path);
    path
}

/// The USB ethernet adapter from the README's worked example.
fn usb_adapter() -> Device {
    Device {
        syspath: PathBuf::from("/sys/devices/pci0000:00/usb1/1-1"),
        name: "1-1".into(),
        subsystem: Some("usb".into()),
        modalias: Some(Modalias::parse(
            "usb:v0BDAp8153d0100dc00dsc00dp00ic02isc06ip00in00",
        )),
        driver: None,
        properties: BTreeMap::new(),
    }
}

const R8152_ALIAS: &str = "usb:v0BDAp8153d*dc*dsc*dp*ic*isc*ip*in* r8152";

#[test]
fn a_device_with_a_driver_already_bound_is_not_climbed() {
    // Rung 0, which is not a rung. The climb must not load anything.
    let system = FakeSystem::default();
    let index = index_with(&[R8152_ALIAS]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Userspace,
    };

    let device = Device {
        driver: Some("r8152".into()),
        ..usb_adapter()
    };
    let climb = ladder.climb(&device);

    assert!(climb.succeeded());
    assert_eq!(climb.resolved.as_deref(), Some("r8152"));
    assert!(climb.attempts.is_empty(), "nothing was attempted");
    assert!(system.calls().is_empty(), "nothing was done to the machine");
}

#[test]
fn rung_one_loads_the_module_and_the_device_works() {
    // The common case, end to end.
    let system = FakeSystem::default().binds("r8152", "r8152");
    let index = index_with(&[R8152_ALIAS]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Userspace,
    };

    let climb = ladder.climb(&usb_adapter());

    assert!(climb.succeeded());
    assert_eq!(climb.resolved.as_deref(), Some("r8152"));
    assert_eq!(climb.reached(), Some(Rung::Modprobe), "stopped at rung 1");
    assert_eq!(system.calls(), vec!["load r8152"]);
}

#[test]
fn a_module_that_loads_and_claims_nothing_is_not_a_success() {
    // The trap. modprobe exits 0, the device is still dead, and reporting
    // success here would close the case on a machine that does not work.
    let system = FakeSystem::default().loads("r8152");
    let index = index_with(&[R8152_ALIAS]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };

    let climb = ladder.climb(&usb_adapter());

    assert!(!climb.succeeded());
    let rung_one = &climb.attempts[0];
    assert_eq!(rung_one.rung, Rung::Modprobe);
    match &rung_one.step {
        Step::Failed(reason) => assert!(
            reason.contains("did not claim the device"),
            "names the real failure: {reason}"
        ),
        other => panic!("{other:?}"),
    }
    // ...and it kept going, because a loaded-but-unclaiming driver is exactly
    // the missing-ID case that rung 3 fixes.
    assert!(climb.attempts.iter().any(|a| a.rung == Rung::Config));
}

#[test]
fn a_builtin_driver_stops_rung_one_without_loading_anything() {
    let system = FakeSystem::default();
    let index = builtin_index(&["usb:v0BDAp8153d*dc*dsc*dp*ic*isc*ip*in* cdc_ether"]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };

    let climb = ladder.climb(&usb_adapter());

    match &climb.attempts[0].step {
        Step::NotApplicable(reason) => {
            assert!(reason.contains("built into this kernel"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
    assert!(
        system.calls().is_empty(),
        "modprobe would have been a no-op and a wrong diagnosis"
    );
}

#[test]
fn an_empty_module_index_does_not_justify_climbing() {
    // A container has no /lib/modules. Concluding "no driver exists" from a
    // missing file and going off to write one is the expensive mistake.
    let system = FakeSystem::default();
    let index = ModuleIndex::load(Path::new("/nonexistent/modules"));
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::KernelModule,
    };

    let climb = ladder.climb(&usb_adapter());

    match &climb.attempts[0].step {
        Step::NotApplicable(reason) => assert!(
            reason.contains("no module index"),
            "says the index is missing rather than that no driver exists: {reason}"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn several_candidate_modules_are_tried_in_order_until_one_binds() {
    let system = FakeSystem::default()
        .loads("cdc_ether")
        .binds("r8152", "r8152");
    let index = index_with(&[
        "usb:v0BDAp8153d*dc*dsc*dp*ic*isc*ip*in* cdc_ether",
        R8152_ALIAS,
    ]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };

    let climb = ladder.climb(&usb_adapter());

    assert!(climb.succeeded());
    assert_eq!(climb.resolved.as_deref(), Some("r8152"));
    assert_eq!(system.calls(), vec!["load cdc_ether", "load r8152"]);
}

#[test]
fn rung_two_asks_before_fetching_firmware_that_is_not_here() {
    // Fetching is a network action against a signed source. It is a real step
    // with a real decision, not something to do quietly.
    let system = FakeSystem::default().wants_firmware(&["rtl_nic/rtl8153a-3.fw"]);
    let index = index_with(&[]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };

    let climb = ladder.climb(&usb_adapter());
    let rung_two = climb
        .attempts
        .iter()
        .find(|a| a.rung == Rung::Firmware)
        .expect("rung 2 ran");

    match &rung_two.step {
        Step::NeedsApproval(what) => assert!(what.contains("rtl8153a-3.fw"), "{what}"),
        other => panic!("{other:?}"),
    }
    assert!(climb.needs_a_decision());
}

#[test]
fn rung_two_reprobes_when_the_firmware_is_already_here() {
    // The firmware arrived after the driver looked for it. A re-probe is the
    // whole fix, and no new code is involved. The device is dead until the
    // re-probe happens, so this fails if the ladder skips that call.
    let system = FakeSystem::default()
        .wants_firmware(&["rtl_nic/rtl8153a-3.fw"])
        .has_firmware(&["rtl_nic/rtl8153a-3.fw"])
        .binds_on_reprobe("r8152");
    let index = index_with(&[]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };

    assert!(
        system.bound_driver(Path::new("/")).is_none(),
        "dead to start"
    );

    let climb = ladder.climb(&usb_adapter());

    assert!(climb.succeeded());
    assert_eq!(climb.resolved.as_deref(), Some("r8152"));
    assert_eq!(system.calls(), vec!["reprobe"], "nothing was loaded");
    assert_eq!(climb.reached(), Some(Rung::Firmware), "stopped at rung 2");
}

#[test]
fn rung_two_reports_a_reprobe_that_does_not_help() {
    // Firmware present, re-probed, still dead. That is a real outcome and the
    // climb must keep going rather than claiming success.
    let system = FakeSystem::default()
        .wants_firmware(&["rtl_nic/rtl8153a-3.fw"])
        .has_firmware(&["rtl_nic/rtl8153a-3.fw"]);
    let index = index_with(&[]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };

    let climb = ladder.climb(&usb_adapter());

    assert!(!climb.succeeded());
    let rung_two = climb
        .attempts
        .iter()
        .find(|a| a.rung == Rung::Firmware)
        .expect("rung 2 ran");
    match &rung_two.step {
        Step::Failed(reason) => assert!(reason.contains("still did not bind"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_ceiling_stops_the_climb_and_says_so() {
    // A server defaults to a lower ceiling than a workstation. Reaching it is
    // a decision for a human, not a dead end.
    let system = FakeSystem::default();
    let index = index_with(&[]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };

    let climb = ladder.climb(&usb_adapter());

    assert!(!climb.succeeded());
    assert!(climb.needs_a_decision());
    let last = climb.attempts.last().unwrap();
    assert_eq!(last.rung, Rung::Source, "stopped at the first rung above");
    match &last.step {
        Step::NeedsApproval(what) => assert!(what.contains("ceiling"), "{what}"),
        other => panic!("{other:?}"),
    }
    assert!(
        !climb.attempts.iter().any(|a| a.rung == Rung::Userspace),
        "nothing above the ceiling was even considered"
    );
}

#[test]
fn a_bridge_is_never_climbed_at_all() {
    // The finding from running against a real /sys, enforced here.
    let system = FakeSystem::default();
    let index = index_with(&[]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::KernelModule,
    };

    let bridge = Device {
        modalias: Some(Modalias::parse(
            "pci:v00008086d00000D57sv00000000sd00000000bc06sc00i00",
        )),
        name: "0000:00:00.0".into(),
        ..usb_adapter()
    };
    let climb = ladder.climb(&bridge);

    assert_eq!(climb.attempts.len(), 1);
    match &climb.attempts[0].step {
        Step::NotApplicable(reason) => assert!(reason.contains("bridge"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert!(system.calls().is_empty());
}

#[test]
fn success_produces_something_that_survives_a_reboot() {
    // The ladder's half of "building ends in declaring". A module loaded by
    // hand is gone in the morning, and a machine that re-solves the same
    // problem every boot has not learned anything.
    let system = FakeSystem::default().binds("r8152", "r8152");
    let index = index_with(&[R8152_ALIAS]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };

    let climb = ladder.climb(&usb_adapter());
    let declaration = climb
        .declaration
        .expect("a fix that is not written down is not a fix");

    assert_eq!(declaration.name, "driver-0bda-8153");
    assert_eq!(declaration.module, "r8152");
    assert_eq!(declaration.device_id.as_deref(), Some("0bda:8153"));

    assert_eq!(declaration.fix, Fix::LoadModule);
    let files = declaration.files();
    assert_eq!(files.len(), 1, "one drop-in, nothing else");
    let (path, contents) = &files[0];
    assert_eq!(path, "/usr/lib/modules-load.d/driver-0bda-8153.conf");
    assert!(contents.contains("r8152"));
    assert!(
        contents.contains("0bda:8153"),
        "says which device: {contents}"
    );
    assert!(contents.starts_with('#'), "explains itself: {contents}");
}

#[test]
fn the_declaration_is_named_after_the_device_not_the_port() {
    // Move the adapter to another USB port and it is the same problem with the
    // same answer. Naming it after the path would build a second package.
    let system = FakeSystem::default().binds("r8152", "r8152");
    let index = index_with(&[R8152_ALIAS]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };

    let first = ladder.climb(&usb_adapter()).declaration.unwrap();

    let system = FakeSystem::default().binds("r8152", "r8152");
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };
    let moved = Device {
        syspath: PathBuf::from("/sys/devices/pci0000:00/usb2/2-4"),
        name: "2-4".into(),
        ..usb_adapter()
    };
    let second = ladder.climb(&moved).declaration.unwrap();

    assert_eq!(first.name, second.name);
}

#[test]
fn a_failed_climb_declares_nothing() {
    let system = FakeSystem::default();
    let index = index_with(&[]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };
    let climb = ladder.climb(&usb_adapter());

    assert!(!climb.succeeded());
    assert!(
        climb.declaration.is_none(),
        "nothing worked, nothing to write down"
    );
}

#[test]
fn rungs_above_config_are_marked_as_introducing_new_code() {
    // The line the autonomy policy cares about.
    assert!(!Rung::Modprobe.introduces_new_code());
    assert!(!Rung::Firmware.introduces_new_code());
    assert!(!Rung::Config.introduces_new_code());
    assert!(Rung::Source.introduces_new_code());
    assert!(Rung::Userspace.introduces_new_code());
    assert!(Rung::KernelModule.introduces_new_code());
}

#[test]
fn a_climb_reads_as_a_record_of_what_was_tried() {
    let system = FakeSystem::default().binds("r8152", "r8152");
    let index = index_with(&[R8152_ALIAS]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };

    let text = ladder.climb(&usb_adapter()).describe();
    assert!(text.contains("0bda:8153"), "{text}");
    assert!(text.contains("rung 1"), "{text}");
    assert!(text.contains("bound to r8152"), "{text}");
    assert!(text.contains("working now"), "{text}");
}

/// A Realtek USB ethernet revision the kernel has not been told about. Vendor
/// specific interface class, which is why the generic CDC driver cannot help.
fn unknown_realtek() -> Device {
    Device {
        syspath: PathBuf::from("/sys/devices/pci0000:00/usb1/1-1"),
        name: "1-1".into(),
        subsystem: Some("usb".into()),
        modalias: Some(Modalias::parse(
            "usb:v0BDAp8155d3000dcFFdsc00dp00icFFisc00ip00in00",
        )),
        driver: None,
        properties: BTreeMap::new(),
    }
}

/// r8152 claims 0bda:8153 and wildcards every class field, as a plain ID table.
const R8152_TABLE: &str = "usb:v0BDAp8153d*dc*dsc*dp*ic*isc*ip*in* r8152";

#[test]
fn rung_three_tells_a_driver_about_a_device_it_would_handle() {
    // The whole rung, end to end: nothing claims 0bda:8155, r8152 claims
    // 0bda:8153 from the same manufacturer, so it is handed the ID and binds.
    let system = FakeSystem::default().accepts_id("r8152", Some("r8152"));
    let index = index_with(&[R8152_TABLE]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };

    let climb = ladder.climb(&unknown_realtek());

    assert!(climb.succeeded());
    assert_eq!(climb.resolved.as_deref(), Some("r8152"));
    assert_eq!(climb.reached(), Some(Rung::Config));
    assert!(
        system
            .calls()
            .contains(&"new_id usb/r8152 0bda:8155".to_string()),
        "{:?}",
        system.calls()
    );
}

#[test]
fn a_driver_that_takes_the_id_and_still_does_not_bind_is_not_a_success() {
    // The same trap as rung 1. Writing new_id always "succeeds"; whether the
    // driver then claims the device is the only thing that matters.
    let system = FakeSystem::default().accepts_id("r8152", None);
    let index = index_with(&[R8152_TABLE]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };

    let climb = ladder.climb(&unknown_realtek());

    assert!(!climb.succeeded());
    let rung_three = climb
        .attempts
        .iter()
        .find(|a| a.rung == Rung::Config)
        .expect("rung 3 ran");
    match &rung_three.step {
        Step::Failed(reason) => assert!(
            reason.contains("still did not claim"),
            "names the real failure: {reason}"
        ),
        other => panic!("{other:?}"),
    }
    assert!(
        climb.declaration.is_none(),
        "nothing worked, nothing declared"
    );
}

#[test]
fn another_manufacturers_driver_is_written_up_rather_than_tried() {
    // Where the autonomy line sits. Forcing a binding can wedge hardware, and
    // a different vendor's driver for this class is a guess about someone
    // else's silicon. It gets handed over, not attempted.
    let system = FakeSystem::default().accepts_id("some_ether", Some("some_ether"));
    let index = index_with(&["usb:v1234p5678d*dc*dsc*dp*icFFisc00ip00in* some_ether"]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };

    let climb = ladder.climb(&unknown_realtek());

    assert!(!climb.succeeded());
    assert!(climb.needs_a_decision());
    let rung_three = climb
        .attempts
        .iter()
        .find(|a| a.rung == Rung::Config)
        .unwrap();
    match &rung_three.step {
        Step::NeedsApproval(what) => {
            assert!(what.contains("some_ether"), "{what}");
            assert!(what.contains("guess"), "says why it stopped: {what}");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        !system.calls().iter().any(|c| c.starts_with("new_id")),
        "nothing was written to the hardware: {:?}",
        system.calls()
    );
}

#[test]
fn rung_three_says_so_when_no_driver_wants_this_kind_of_device() {
    let system = FakeSystem::default();
    let index = index_with(&["usb:v9999p9999d*dc*dsc*dp*ic*isc*ip*in* unrelated"]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };

    let climb = ladder.climb(&unknown_realtek());
    let rung_three = climb
        .attempts
        .iter()
        .find(|a| a.rung == Rung::Config)
        .unwrap();
    match &rung_three.step {
        Step::NotApplicable(reason) => assert!(reason.contains("no driver"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_rung_three_fix_installs_a_rule_and_a_unit_that_survive_a_replug() {
    // new_id does not survive a reboot, and it does not survive unplugging the
    // device either. A fix that only lasts until Tuesday is not a fix.
    let system = FakeSystem::default().accepts_id("r8152", Some("r8152"));
    let index = index_with(&[R8152_TABLE]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };

    let declaration = ladder.climb(&unknown_realtek()).declaration.unwrap();
    assert_eq!(
        declaration.fix,
        Fix::BindById {
            bus: "usb".into(),
            vendor: 0x0bda,
            product: 0x8155
        }
    );

    let files = declaration.files();
    assert_eq!(files.len(), 2, "a rule to notice it and a unit to act");

    let (rule_path, rule) = &files[0];
    assert!(
        rule_path.starts_with("/usr/lib/udev/rules.d/"),
        "{rule_path}"
    );
    assert!(rule.contains("ATTR{idVendor}==\"0bda\""), "{rule}");
    assert!(rule.contains("ATTR{idProduct}==\"8155\""), "{rule}");
    assert!(rule.contains("SYSTEMD_WANTS"), "{rule}");
    assert!(
        !rule.contains("RUN+="),
        "RUN blocks the udev event queue on a sysfs write: {rule}"
    );

    let (unit_path, unit) = &files[1];
    assert_eq!(
        unit_path, "/usr/lib/systemd/system/omnia-bind-0bda-8155.service",
        "the unit is named after the device, without stuttering"
    );
    assert!(
        unit.contains("ExecStart=/usr/bin/tee /sys/bus/usb/drivers/r8152/new_id"),
        "{unit}"
    );
    assert!(unit.contains("StandardInputText=0bda 8155"), "{unit}");
    // The hot-plug case: on a re-plug nothing else will have loaded the
    // driver, so a ConditionPathExists on new_id would skip this unit silently
    // in exactly the situation it exists for.
    assert!(
        unit.contains("ExecStartPre=/usr/sbin/modprobe r8152"),
        "{unit}"
    );
    // The directive, not the word: the unit explains in a comment why it does
    // not use one, and matching prose would fail on the explanation.
    assert!(
        !unit
            .lines()
            .any(|line| line.starts_with("ConditionPathExists=")),
        "that would skip the unit on a cold hot-plug: {unit}"
    );
}

#[test]
fn nothing_the_rung_three_fix_writes_goes_through_a_shell() {
    // A device ID is data. If it ever reached a command line, a crafted
    // modalias would be a command-injection vector into a root unit.
    let system = FakeSystem::default().accepts_id("r8152", Some("r8152"));
    let index = index_with(&[R8152_TABLE]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };

    for (path, contents) in ladder
        .climb(&unknown_realtek())
        .declaration
        .unwrap()
        .files()
    {
        for shell in ["/bin/sh", "/bin/bash", "sh -c", "bash -c", "$(", "`"] {
            assert!(
                !contents.contains(shell),
                "{path} invokes a shell via {shell}:\n{contents}"
            );
        }
    }
}

#[test]
fn a_pci_rung_three_rule_matches_on_the_attributes_pci_actually_uses() {
    // PCI names them vendor/device and writes them with an 0x prefix; USB uses
    // idVendor/idProduct and no prefix. A rule with the wrong attribute names
    // silently never fires.
    let system = FakeSystem::default().accepts_id("virtio_net", Some("virtio_net"));
    let index = index_with(&["pci:v00001AF4d00001041sv*sd*bc*sc*i* virtio_net"]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };

    let card = Device {
        syspath: PathBuf::from("/sys/devices/pci0000:00/0000:00:09.0"),
        name: "0000:00:09.0".into(),
        subsystem: Some("pci".into()),
        modalias: Some(Modalias::parse(
            "pci:v00001AF4d00009999sv00001AF4sd00009999bc02sc00i00",
        )),
        driver: None,
        properties: BTreeMap::new(),
    };

    let files = ladder.climb(&card).declaration.unwrap().files();
    let rule = &files[0].1;
    assert!(rule.contains("ATTR{vendor}==\"0x1af4\""), "{rule}");
    assert!(rule.contains("ATTR{device}==\"0x9999\""), "{rule}");
    assert!(rule.contains("SUBSYSTEM==\"pci\""), "{rule}");
}

#[test]
fn rung_three_is_skipped_entirely_when_the_ceiling_is_below_it() {
    let system = FakeSystem::default().accepts_id("r8152", Some("r8152"));
    let index = index_with(&[R8152_TABLE]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Firmware,
    };

    let climb = ladder.climb(&unknown_realtek());

    assert!(!climb.succeeded());
    assert!(climb.needs_a_decision());
    assert!(
        !system.calls().iter().any(|c| c.starts_with("new_id")),
        "{:?}",
        system.calls()
    );
}

#[test]
fn the_generated_unit_is_valid_to_systemd_itself() {
    // Asserting on substrings proves the strings are there, not that systemd
    // will accept the file. A misspelled directive passes every assertion above
    // and is then silently ignored on the target machine, which is the worst
    // possible outcome for a unit whose whole job is to run unattended.
    //
    // Skipped where systemd-analyze is absent, which is most containers. It is
    // present on every machine this actually ships to.
    let Ok(probe) = std::process::Command::new("systemd-analyze")
        .arg("--version")
        .output()
    else {
        return;
    };
    if !probe.status.success() {
        return;
    }

    let system = FakeSystem::default().accepts_id("r8152", Some("r8152"));
    let index = index_with(&[R8152_TABLE]);
    let ladder = Ladder {
        index: &index,
        system: &system,
        ceiling: Rung::Config,
    };
    let files = ladder
        .climb(&unknown_realtek())
        .declaration
        .unwrap()
        .files();

    let dir = scratch("unit-verify");
    std::fs::create_dir_all(&dir).unwrap();
    let mut unit_path = None;
    for (path, contents) in &files {
        let name = Path::new(path).file_name().unwrap();
        let written = dir.join(name);
        std::fs::write(&written, contents).unwrap();
        if path.ends_with(".service") {
            unit_path = Some(written);
        }
    }

    let output = std::process::Command::new("systemd-analyze")
        .arg("verify")
        .arg(unit_path.expect("a unit was generated"))
        .output()
        .expect("systemd-analyze runs");
    let complaints = String::from_utf8_lossy(&output.stderr);
    let _ = std::fs::remove_dir_all(&dir);

    // A missing executable is the container's problem, not the unit's: neither
    // modprobe nor tee is installed here, and both are on any machine this
    // targets. Everything else systemd says is a real defect in the file.
    let real: Vec<&str> = complaints
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| !line.contains("is not executable"))
        .collect();
    assert!(
        real.is_empty(),
        "systemd rejects the generated unit:\n{}",
        real.join("\n")
    );
}
