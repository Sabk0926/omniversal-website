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
    pub fix: Fix,
}

/// The shape of the fix, which decides what has to be installed to keep it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fix {
    /// The module exists and just was not loaded. One drop-in.
    LoadModule,
    /// An existing driver had to be told this device's ID. Needs both a rule to
    /// notice the device and a unit to do the telling, because `new_id` does
    /// not survive a reboot or a re-plug.
    BindById {
        bus: String,
        vendor: u32,
        product: u32,
    },
}

impl Declaration {
    /// The files the package installs.
    ///
    /// Drop-ins under `/usr/lib`, never edits to files someone else owns: the
    /// whole point of shipping generated things as packages is that removing
    /// the package removes the change, and an edit to `/etc/modules` could not
    /// be undone that cleanly.
    ///
    /// Nothing here runs a shell. The unit's `ExecStart` is an argv vector and
    /// the value it writes arrives on standard input, so a device ID is data
    /// and never a fragment of a command line.
    pub fn files(&self) -> Vec<(String, String)> {
        match &self.fix {
            Fix::LoadModule => vec![(
                format!("/usr/lib/modules-load.d/{}.conf", self.name),
                format!(
                    "# Written by Omnia: {} needs this module and does not load it \
                     automatically.\n{}\n",
                    self.device_id
                        .as_deref()
                        .unwrap_or("a device on this machine"),
                    self.module
                ),
            )],
            Fix::BindById {
                bus,
                vendor,
                product,
            } => {
                let unit = format!("omnia-bind-{}.service", self.slug());
                vec![
                    (
                        format!("/usr/lib/udev/rules.d/70-omnia-{}.rules", self.slug()),
                        self.udev_rule(bus, *vendor, *product, &unit),
                    ),
                    (
                        format!("/usr/lib/systemd/system/{unit}"),
                        self.bind_unit(bus, *vendor, *product),
                    ),
                ]
            }
        }
    }

    /// The device part of the name, without the `driver-` prefix the package
    /// carries. `driver-0bda-8155` becomes `0bda-8155`, so the unit reads
    /// `omnia-bind-0bda-8155.service` rather than stuttering.
    fn slug(&self) -> &str {
        self.name.strip_prefix("driver-").unwrap_or(&self.name)
    }

    /// Notice the device and ask systemd to run the unit.
    ///
    /// `ENV{SYSTEMD_WANTS}` rather than `RUN+=`: udev's RUN runs a short-lived
    /// process inside the udev event, where a blocking write to sysfs can stall
    /// the whole event queue. Handing it to systemd also means the work is a
    /// unit with a name, a log and a status, rather than something invisible.
    fn udev_rule(&self, bus: &str, vendor: u32, product: u32, unit: &str) -> String {
        let attributes = match bus {
            "pci" => {
                format!("ATTR{{vendor}}==\"0x{vendor:04x}\", ATTR{{device}}==\"0x{product:04x}\"")
            }
            // usb and everything else that names its ids this way
            _ => {
                format!("ATTR{{idVendor}}==\"{vendor:04x}\", ATTR{{idProduct}}==\"{product:04x}\"")
            }
        };
        format!(
            "# Written by Omnia. {} drives devices like this one but was not told\n\
             # about {:04x}:{:04x}, so this hands it the ID when the device appears.\n\
             ACTION==\"add\", SUBSYSTEM==\"{bus}\", {attributes}, \
             TAG+=\"systemd\", ENV{{SYSTEMD_WANTS}}+=\"{unit}\"\n",
            self.module, vendor, product
        )
    }

    fn bind_unit(&self, bus: &str, vendor: u32, product: u32) -> String {
        let new_id = format!("/sys/bus/{bus}/drivers/{}/new_id", self.module);
        format!(
            "[Unit]\n\
             Description=Tell {module} about {vendor:04x}:{product:04x}\n\
             Documentation=man:omni(1)\n\
             \n\
             [Service]\n\
             Type=oneshot\n\
             RemainAfterExit=yes\n\
             # new_id only exists once the driver is loaded, and on a hot plug\n\
             # nothing else will have loaded it. Loading it here rather than\n\
             # testing for the path: a ConditionPathExists would skip this unit\n\
             # silently in exactly the case it is needed.\n\
             ExecStartPre=/usr/sbin/modprobe {module}\n\
             # argv only, no shell: the ID is data on stdin, never a command line.\n\
             StandardInputText={vendor:04x} {product:04x}\n\
             ExecStart=/usr/bin/tee {new_id}\n\
             StandardOutput=null\n\
             \n\
             # Re-running this is normal: the unit fires again on every re-plug,\n\
             # and a duplicate new_id write returns EEXIST. That is the fix\n\
             # already being in place, not a failure.\n\
             SuccessExitStatus=0 1\n",
            module = self.module,
            vendor = vendor,
            product = product,
            new_id = new_id,
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

    /// Tell a loaded driver about a device ID it does not know, by writing to
    /// its `new_id`. The driver then tries to bind; whether it succeeds is a
    /// separate question, which is why the caller checks.
    fn bind_by_id(&self, bus: &str, driver: &str, vendor: u32, product: u32) -> Result<(), String>;
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
                fix: Fix::LoadModule,
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

        // ---- rung 3: the driver is there and does not know this device ----
        if Rung::Config <= self.ceiling {
            let step = self.rung_three(device, alias);
            let bound = matches!(step, Step::Bound { .. });
            if let Step::Bound { driver } = &step {
                climb.resolved = Some(driver.clone());
                climb.declaration = self.id_declaration(device, alias, driver);
            }
            climb.attempts.push(Attempt {
                rung: Rung::Config,
                step,
            });
            if bound {
                return climb;
            }
        }

        // ---- rungs 4 and up ----
        for rung in [
            Rung::Config,
            Rung::Source,
            Rung::Userspace,
            Rung::KernelModule,
        ] {
            if climb.attempts.iter().any(|a| a.rung == rung) {
                continue;
            }
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

    /// An existing driver handles devices like this one and has not been told
    /// this one's ID.
    ///
    /// # Where the autonomy line sits
    ///
    /// Forcing a binding is a bigger step than loading a module: the wrong
    /// driver writing to the wrong registers can wedge the hardware, and unlike
    /// `modprobe` there is no "it declined to claim it" outcome to fall back on.
    /// So the two kinds of candidate are treated differently:
    ///
    /// - **Same manufacturer.** A newer revision or a rebadge of a chip the
    ///   driver already handles. This is what `new_id` is overwhelmingly used
    ///   for, and it is acted on.
    /// - **A different manufacturer's driver for this class.** Plausible, and
    ///   still a guess about someone else's hardware. It is written up and
    ///   handed over rather than tried.
    fn rung_three(&self, device: &Device, alias: &omnia_kernel::Modalias) -> Step {
        let Some(bus) = device.subsystem.clone() else {
            return Step::NotApplicable("the device is on no bus this can write to".into());
        };
        let Some((vendor, product)) = split_id_pair(alias) else {
            return Step::NotApplicable("this bus does not identify devices by number".into());
        };

        let mut candidates = self.index.id_candidates(alias);
        if candidates.is_empty() {
            return Step::NotApplicable(
                "no driver in this kernel wants a device like this one".into(),
            );
        }
        // Strongest evidence first.
        candidates.sort_by_key(|candidate| !candidate.same_vendor);

        let mut failures = Vec::new();
        for candidate in &candidates {
            if !candidate.same_vendor {
                return Step::NeedsApproval(format!(
                    "{}. Binding it would be a guess about another manufacturer's hardware",
                    candidate.describe()
                ));
            }
            // new_id lives under the driver's directory, which only exists once
            // the driver is loaded.
            if let Err(e) = self.system.load_module(&candidate.module) {
                failures.push(format!("{} would not load: {e}", candidate.module));
                continue;
            }
            match self
                .system
                .bind_by_id(&bus, &candidate.module, vendor, product)
            {
                Ok(()) => match self.system.bound_driver(&device.syspath) {
                    Some(driver) => return Step::Bound { driver },
                    None => failures.push(format!(
                        "{} took the ID and still did not claim the device",
                        candidate.module
                    )),
                },
                Err(e) => failures.push(format!("{} refused the ID: {e}", candidate.module)),
            }
        }
        Step::Failed(failures.join("; "))
    }

    fn id_declaration(
        &self,
        device: &Device,
        alias: &omnia_kernel::Modalias,
        driver: &str,
    ) -> Option<Declaration> {
        let bus = device.subsystem.clone()?;
        let (vendor, product) = split_id_pair(alias)?;
        Some(Declaration {
            name: declaration_name(device),
            module: driver.to_string(),
            device_id: device.id_pair(),
            proves: format!("{driver} is told about {vendor:04x}:{product:04x} and binds to it"),
            fix: Fix::BindById {
                bus,
                vendor,
                product,
            },
        })
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

/// The numeric identity a `new_id` write needs.
fn split_id_pair(alias: &omnia_kernel::Modalias) -> Option<(u32, u32)> {
    let pair = alias.id_pair()?;
    let (vendor, product) = pair.split_once(':')?;
    Some((
        u32::from_str_radix(vendor, 16).ok()?,
        u32::from_str_radix(product, 16).ok()?,
    ))
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

#[path = "ladder/system.rs"]
pub mod system;

pub use self::system::{firmware_requests, RealSystem};

#[cfg(test)]
#[path = "ladder/ladder_tests.rs"]
mod ladder_tests;
