//! The driver ladder: making a device work, cheapest and safest first.
//!
//! # A working device is a capability
//!
//! This lives in the forge rather than next to the sysfs reading because the
//! ladder ends the same way every other capability does: **building ends in
//! declaring.** Loading a module makes the device work until the next reboot.
//! Writing down that this machine needs that module for that device, as a
//! versioned package with a test that re-checks it, is what makes the machine
//! actually gain something.
//!
//! So the rungs are the "plan" step of the same five-step pipeline, and the
//! result goes through the same proving and packaging as a backup tool.
//!
//! # The rungs
//!
//! | # | Situation | What happens |
//! |---|---|---|
//! | 1 | Driver is in the kernel, just not loaded | `modprobe`, check it binds |
//! | 2 | Needs a firmware blob | Fetch from `linux-firmware`, reload |
//! | 3 | Needs a quirk, ID, udev rule or overlay | Generate config. No code |
//! | 4 | Driver source exists somewhere | Fetch, build, DKMS, verify |
//! | 5 | USB/I2C/SPI/serial with no driver anywhere | Sandboxed userspace driver |
//! | 6 | Novel device needing kernel code | Scaffold, load with rollback armed |
//!
//! Rungs 1 and 2 are two filesystem lookups and do not wake the model at all.
//! Most devices end there, which is the point of the ordering: the expensive,
//! risky rungs are reached only after the cheap ones have been ruled out by
//! evidence rather than by assumption.
//!
//! # What "it worked" means
//!
//! A driver binding is the only success condition. Not "modprobe exited 0" —
//! a module can load cleanly and claim nothing, which is exactly what happens
//! when the driver exists but does not know this device's ID. That is a rung 3
//! problem wearing a rung 1 disguise, and checking the bind is what tells them
//! apart.

use std::path::Path;

use omnia_kernel::{Device, Lookup, ModuleIndex};

/// Where on the ladder an attempt sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rung {
    Modprobe = 1,
    Firmware = 2,
    Config = 3,
    Source = 4,
    Userspace = 5,
    KernelModule = 6,
}

impl Rung {
    pub fn number(self) -> u8 {
        self as u8
    }

    pub fn label(self) -> &'static str {
        match self {
            Rung::Modprobe => "load an existing module",
            Rung::Firmware => "supply missing firmware",
            Rung::Config => "generate a quirk, ID or rule",
            Rung::Source => "build a driver from source",
            Rung::Userspace => "write a sandboxed userspace driver",
            Rung::KernelModule => "write a kernel module",
        }
    }

    /// Does this rung run code that was not already on the machine?
    ///
    /// The line the autonomy policy cares about. Everything at or below
    /// [`Rung::Config`] rearranges what is already installed; everything above
    /// introduces something new, and needs a correspondingly better reason.
    pub fn introduces_new_code(self) -> bool {
        self >= Rung::Source
    }
}

impl std::fmt::Display for Rung {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rung {} ({})", self.number(), self.label())
    }
}

/// What one rung did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// The device is driven now. The climb stops here.
    Bound { driver: String },
    /// This rung has nothing to offer for this device. Carries why, because
    /// "we skipped it" is the most common thing to have to explain later.
    NotApplicable(String),
    /// The rung tried something and the device still is not driven.
    Failed(String),
    /// The rung knows what to do and is not allowed to do it unattended.
    NeedsApproval(String),
    /// Not built yet. Named rather than silently skipped, so a climb that runs
    /// out of implemented rungs says so instead of reporting a dead end.
    NotImplemented(String),
}

impl Step {
    pub fn describe(&self) -> String {
        match self {
            Step::Bound { driver } => format!("bound to {driver}"),
            Step::NotApplicable(reason) => format!("skipped — {reason}"),
            Step::Failed(reason) => format!("failed — {reason}"),
            Step::NeedsApproval(what) => format!("needs a decision — {what}"),
            Step::NotImplemented(what) => format!("not built yet — {what}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub rung: Rung,
    pub step: Step,
}

/// The whole climb, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Climb {
    /// How the device was described when the climb started.
    pub device: String,
    pub attempts: Vec<Attempt>,
    /// The driver that ended up bound, if any.
    pub resolved: Option<String>,
    /// What to declare so the fix survives a reboot. `None` when nothing was
    /// fixed, or when the fix needs no declaration.
    pub declaration: Option<Declaration>,
}

impl Climb {
    pub fn succeeded(&self) -> bool {
        self.resolved.is_some()
    }

    /// The highest rung that was actually reached.
    pub fn reached(&self) -> Option<Rung> {
        self.attempts.last().map(|attempt| attempt.rung)
    }

    /// Is a human waiting on this?
    pub fn needs_a_decision(&self) -> bool {
        self.attempts
            .iter()
            .any(|attempt| matches!(attempt.step, Step::NeedsApproval(_)))
    }

    pub fn describe(&self) -> String {
        let mut out = format!("{}\n", self.device);
        for attempt in &self.attempts {
            out.push_str(&format!(
                "  rung {}  {:<38} {}\n",
                attempt.rung.number(),
                attempt.rung.label(),
                attempt.step.describe()
            ));
        }
        match &self.resolved {
            Some(driver) => out.push_str(&format!("\nworking now, driven by {driver}\n")),
            None => out.push_str("\nstill not working\n"),
        }
        out
    }
}

/// What has to be written down so the fix outlives this boot.
///
/// A module loaded by hand is gone after a reboot, and a machine that silently
/// re-solves the same problem every morning has not learned anything. This is
/// the ladder's half of "building ends in declaring".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// A stable name derived from the device, so re-solving the same device
    /// produces the same package rather than a rival to it.
    pub name: String,
    pub module: String,
    /// The device this is for, as `vendor:product` where the bus has one.
    pub device_id: Option<String>,
    /// What the retained test will check.
    pub proves: String,
}

impl Declaration {
    /// `/usr/lib/modules-load.d/<name>.conf`, which systemd-modules-load reads
    /// at boot. A drop-in rather than an edit to an existing file: it can be
    /// removed by removing the package, which is the whole reason generated
    /// things are packages.
    pub fn modules_load_conf(&self) -> (String, String) {
        (
            format!("/usr/lib/modules-load.d/{}.conf", self.name),
            format!(
                "# Written by Omnia: {} needs this module and does not load it \
                 automatically.\n{}\n",
                self.device_id
                    .as_deref()
                    .unwrap_or("a device on this machine"),
                self.module
            ),
        )
    }
}

/// Everything the ladder does that touches the machine.
///
/// Behind a trait because the rungs are decisions and the decisions are what
/// need testing. A test can hand the ladder a machine where a module loads but
/// binds nothing, which is a real and common case and one that is close to
/// impossible to arrange on demand with real hardware.
pub trait System {
    /// Load a module. `Ok` means it loaded, which is emphatically not the same
    /// as the device working.
    fn load_module(&self, module: &str) -> Result<(), String>;

    /// Which driver is bound to this device right now.
    fn bound_driver(&self, syspath: &Path) -> Option<String>;

    /// Firmware files the kernel asked for and did not get, for this device.
    fn missing_firmware(&self, device: &Device) -> Vec<String>;

    /// Is this firmware file present on the machine now?
    fn firmware_available(&self, name: &str) -> bool;

    /// Ask the kernel to re-probe, after supplying firmware or a new ID.
    fn reprobe(&self, syspath: &Path) -> Result<(), String>;
}

pub struct Ladder<'a> {
    pub index: &'a ModuleIndex,
    pub system: &'a dyn System,
    /// Highest rung this machine is allowed to reach unattended. Per-profile:
    /// a workstation may climb further than a server.
    pub ceiling: Rung,
}

impl Ladder<'_> {
    pub fn climb(&self, device: &Device) -> Climb {
        let mut climb = Climb {
            device: device.describe(),
            attempts: Vec::new(),
            resolved: None,
            declaration: None,
        };

        // Rung 0, which is not a rung: is anything actually wrong?
        if let Some(driver) = &device.driver {
            climb.resolved = Some(driver.clone());
            return climb;
        }
        if let Some(reason) = device
            .modalias
            .as_ref()
            .and_then(omnia_kernel::Modalias::driver_not_expected)
        {
            climb.attempts.push(Attempt {
                rung: Rung::Modprobe,
                step: Step::NotApplicable(reason),
            });
            return climb;
        }

        let Some(alias) = &device.modalias else {
            climb.attempts.push(Attempt {
                rung: Rung::Modprobe,
                step: Step::NotApplicable(
                    "the kernel publishes no modalias, so there is nothing to match on".into(),
                ),
            });
            return climb;
        };

        // ---- rung 1: a module exists and is not loaded ----
        let lookup = self.index.lookup(alias);
        let step = self.rung_one(device, &lookup);
        let bound = matches!(step, Step::Bound { .. });
        if let Step::Bound { driver } = &step {
            climb.resolved = Some(driver.clone());
            climb.declaration = Some(Declaration {
                name: declaration_name(device),
                module: driver.clone(),
                device_id: device.id_pair(),
                proves: format!(
                    "the module loads and {} binds to it",
                    device.id_pair().unwrap_or_else(|| device.name.clone())
                ),
            });
        }
        climb.attempts.push(Attempt {
            rung: Rung::Modprobe,
            step,
        });
        if bound {
            return climb;
        }

        // ---- rung 2: firmware ----
        let step = self.rung_two(device);
        let bound = matches!(step, Step::Bound { .. });
        if let Step::Bound { driver } = &step {
            climb.resolved = Some(driver.clone());
        }
        climb.attempts.push(Attempt {
            rung: Rung::Firmware,
            step,
        });
        if bound {
            return climb;
        }

        // ---- rungs 3 and up ----
        for rung in [
            Rung::Config,
            Rung::Source,
            Rung::Userspace,
            Rung::KernelModule,
        ] {
            if rung > self.ceiling {
                climb.attempts.push(Attempt {
                    rung,
                    step: Step::NeedsApproval(format!(
                        "{} is above this machine's ceiling of rung {}",
                        rung,
                        self.ceiling.number()
                    )),
                });
                break;
            }
            climb.attempts.push(Attempt {
                rung,
                step: Step::NotImplemented(rung.label().to_string()),
            });
        }

        climb
    }

    /// A module claims this device and is not loaded. Load it, then check the
    /// only thing that matters.
    fn rung_one(&self, device: &Device, lookup: &Lookup) -> Step {
        match lookup {
            Lookup::Builtin(module) => Step::NotApplicable(format!(
                "{module} is built into this kernel and still did not bind, so \
                 loading is not the problem"
            )),
            Lookup::None if self.index.is_empty() => Step::NotApplicable(
                "this machine has no module index, so nothing can be concluded \
                 about what drivers exist"
                    .into(),
            ),
            Lookup::None => {
                Step::NotApplicable("no module in this kernel claims this device".into())
            }
            Lookup::Loadable(modules) => {
                let mut failures = Vec::new();
                for module in modules {
                    match self.system.load_module(module) {
                        Ok(()) => match self.system.bound_driver(&device.syspath) {
                            Some(driver) => return Step::Bound { driver },
                            // The trap this rung exists to avoid: the module
                            // loaded and claimed nothing. That is a missing
                            // device ID, which is rung 3, not a rung 1 success.
                            None => failures
                                .push(format!("{module} loaded but did not claim the device")),
                        },
                        Err(e) => failures.push(format!("{module} would not load: {e}")),
                    }
                }
                Step::Failed(failures.join("; "))
            }
        }
    }

    /// The kernel asked for firmware and did not get it.
    fn rung_two(&self, device: &Device) -> Step {
        let missing = self.system.missing_firmware(device);
        if missing.is_empty() {
            return Step::NotApplicable("the kernel has not asked for any firmware".into());
        }

        let (present, absent): (Vec<String>, Vec<String>) = missing
            .into_iter()
            .partition(|name| self.system.firmware_available(name));

        if !absent.is_empty() {
            // Fetching from linux-firmware is a network action with a signed
            // source, so it is a real step rather than something to invent here.
            return Step::NeedsApproval(format!(
                "{} is missing from this machine and would have to be fetched",
                absent.join(", ")
            ));
        }

        // Present but the kernel asked and did not get it: it appeared after
        // the driver looked, so a re-probe is exactly the fix.
        if let Err(e) = self.system.reprobe(&device.syspath) {
            return Step::Failed(format!(
                "{} is present but the device would not re-probe: {e}",
                present.join(", ")
            ));
        }
        match self.system.bound_driver(&device.syspath) {
            Some(driver) => Step::Bound { driver },
            None => Step::Failed(format!(
                "{} is present and the device still did not bind",
                present.join(", ")
            )),
        }
    }
}

/// A stable package name for the declaration.
///
/// Derived from the device identity rather than from the path: the same USB
/// adapter in a different port is the same problem with the same answer, and
/// naming it after the port would build a second package for it.
fn declaration_name(device: &Device) -> String {
    let identity = device
        .id_pair()
        .unwrap_or_else(|| device.name.clone())
        .replace(':', "-");
    let slug: String = identity
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("driver-{}", slug.trim_matches('-'))
}

#[cfg(test)]
#[path = "ladder/ladder_tests.rs"]
mod ladder_tests;
