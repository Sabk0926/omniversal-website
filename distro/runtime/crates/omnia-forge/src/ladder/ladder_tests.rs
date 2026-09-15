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

    let (path, contents) = declaration.modules_load_conf();
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
