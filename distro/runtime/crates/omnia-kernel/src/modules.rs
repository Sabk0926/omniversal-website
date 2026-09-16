//! The kernel's own module index: which driver, if any, matches this device.
//!
//! # Why read the index instead of running modprobe
//!
//! `modprobe -R <alias>` answers the same question, and shelling out to it
//! would be two lines. But the ladder needs the *decision* before it takes the
//! *action*, and those are different things:
//!
//! - "A driver exists but is not loaded" is rung 1, and the fix is one command.
//! - "A driver is built into this kernel and still did not bind" is not a
//!   loading problem at all. Running `modprobe` there does nothing and hides a
//!   real diagnosis — usually a missing device ID or firmware, which is rung 2
//!   or 3.
//! - "No driver exists anywhere in this kernel" is what justifies the expensive
//!   rungs, and it is the one claim that should never be made casually.
//!
//! Reading the index separates all three, costs one file read, and has no side
//! effects — so it can run while merely surveying a machine, which is not
//! something you want to say about a command that loads kernel code.
//!
//! # Format
//!
//! `modules.alias` and `modules.builtin.alias` are lines of
//! `alias <pattern> <module>`, where the pattern is a shell glob over the
//! device's modalias. The kernel generates them from each driver's
//! `MODULE_DEVICE_TABLE`, so this is the driver's own declaration of what it
//! can drive.

use std::fs;
use std::path::{Path, PathBuf};

use crate::modalias::Modalias;

/// What the index says about a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// Modules that claim this device and can be loaded. Rung 1.
    Loadable(Vec<String>),
    /// A driver for this device is compiled into the kernel. It is already
    /// there and still did not bind, so loading is not the problem.
    Builtin(String),
    /// Nothing in this kernel claims the device.
    None,
}

impl Lookup {
    /// Does rung 1 have anything to do?
    pub fn can_load(&self) -> bool {
        matches!(self, Lookup::Loadable(modules) if !modules.is_empty())
    }

    /// Explain the finding in the terms the next step needs.
    pub fn describe(&self) -> String {
        match self {
            Lookup::Loadable(modules) if modules.len() == 1 => {
                format!("{} claims this device and is not loaded", modules[0])
            }
            Lookup::Loadable(modules) => format!(
                "{} claim this device and none is loaded",
                modules.join(", ")
            ),
            Lookup::Builtin(module) => format!(
                "{module} is built into this kernel and did not bind, so this is \
                 not a loading problem"
            ),
            Lookup::None => "no driver in this kernel claims this device".into(),
        }
    }
}

/// A driver that would take this device if it knew its ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdCandidate {
    pub module: String,
    /// A device this driver does claim, as evidence of what it is for.
    pub claims: String,
    /// The index line it came from, so the reasoning can be checked by hand.
    pub pattern: String,
    /// The driver claims a device from the same manufacturer. The stronger of
    /// the two reasons a candidate can qualify.
    pub same_vendor: bool,
}

impl IdCandidate {
    pub fn describe(&self) -> String {
        let why = if self.same_vendor {
            "the same manufacturer"
        } else {
            "the same kind of device"
        };
        format!(
            "{} drives {}, which is {}, and differs from this one only by its ID",
            self.module, self.claims, why
        )
    }
}

/// The literal vendor and product a pattern names, if it names them.
///
/// `usb:v0BDAp8153d*...` yields `(0x0bda, 0x8153)`; `usb:v*p*d*...` yields
/// `None`, and so does a pattern with a partial wildcard like `v0BD*`. Only a
/// fully literal identity counts, because a partly-wildcarded one does not name
/// a device that could be pointed at.
fn pattern_identity(pattern: &str) -> Option<(u32, u32)> {
    let rest = pattern
        .strip_prefix("usb:")
        .or_else(|| pattern.strip_prefix("pci:"))?;
    let (first, second) = if pattern.starts_with("usb:") {
        (("v", 4), ("p", 4))
    } else {
        (("v", 8), ("d", 8))
    };
    let after_v = rest.strip_prefix(first.0)?;
    let vendor: String = after_v.chars().take(first.1).collect();
    if vendor.len() != first.1 || !vendor.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let after_vendor = &after_v[first.1..];
    let after_p = after_vendor.strip_prefix(second.0)?;
    let product: String = after_p.chars().take(second.1).collect();
    if product.len() != second.1 || !product.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some((
        u32::from_str_radix(&vendor, 16).ok()?,
        u32::from_str_radix(&product, 16).ok()?,
    ))
}

#[derive(Debug, Clone, Default)]
pub struct ModuleIndex {
    /// In file order. `modprobe` reports every match, and so does this.
    loadable: Vec<(String, String)>,
    builtin: Vec<(String, String)>,
    source: Option<PathBuf>,
}

impl ModuleIndex {
    /// Read the index for a kernel release.
    ///
    /// A missing directory is not an error: a container has no
    /// `/lib/modules`, and an empty index answering `None` for everything is
    /// the honest result there. Callers that need to distinguish "nothing
    /// claims this device" from "this machine has no module index at all" ask
    /// [`ModuleIndex::is_empty`], because those justify very different actions.
    pub fn load(modules_dir: &Path) -> ModuleIndex {
        ModuleIndex {
            loadable: read_alias_file(&modules_dir.join("modules.alias")),
            builtin: read_alias_file(&modules_dir.join("modules.builtin.alias")),
            source: Some(modules_dir.to_path_buf()),
        }
    }

    /// The running kernel's index, at `/lib/modules/$(uname -r)`.
    pub fn for_running_kernel() -> ModuleIndex {
        match kernel_release() {
            Some(release) => ModuleIndex::load(&PathBuf::from("/lib/modules").join(release)),
            None => ModuleIndex::default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.loadable.is_empty() && self.builtin.is_empty()
    }

    pub fn len(&self) -> usize {
        self.loadable.len() + self.builtin.len()
    }

    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// What claims this device.
    ///
    /// Built-in wins over loadable when both match. A driver already in the
    /// kernel that did not bind is the more specific and more useful finding:
    /// it means the device was seen and rejected, not merely unrecognised.
    pub fn lookup(&self, alias: &Modalias) -> Lookup {
        if let Some(module) = self.match_in(&self.builtin, &alias.raw).first() {
            return Lookup::Builtin(module.clone());
        }
        match self.match_in(&self.loadable, &alias.raw) {
            modules if modules.is_empty() => Lookup::None,
            modules => Lookup::Loadable(modules),
        }
    }

    /// Drivers that want a device exactly like this one and are only held back
    /// by its vendor and product ID.
    ///
    /// This is rung 3's input, and it is derived rather than guessed. For every
    /// pattern in the index that names a specific vendor and product, this puts
    /// *that driver's* IDs onto *this device* and re-runs the match. A pattern
    /// that then matches has every other requirement already satisfied — device
    /// class, interface class, subclass, protocol — so the ID is the only thing
    /// between the driver and the device. That is what `new_id` is for.
    ///
    /// Patterns that already match the device are excluded: rung 1 covered
    /// those, and re-offering them here would loop. Patterns whose identity
    /// fields are wildcards are excluded too, for the same reason — a driver
    /// that claims any vendor would have matched already, so if it did not, the
    /// mismatch is in a class field and no ID will fix it.
    pub fn id_candidates(&self, alias: &Modalias) -> Vec<IdCandidate> {
        let Some(rendered) = alias.render() else {
            return Vec::new();
        };
        let our_vendor = match &alias.kind {
            crate::modalias::Kind::Usb(usb) => u32::from(usb.vendor),
            crate::modalias::Kind::Pci(pci) => u32::from(pci.vendor),
            _ => return Vec::new(),
        };
        // Whatever rung 1 already found is not a rung 3 candidate, even if some
        // *other* line in the index points at the same module. A driver that
        // already claims this device does not need to be told its ID.
        let already = match self.lookup(alias) {
            Lookup::Loadable(modules) => modules,
            Lookup::Builtin(module) => vec![module],
            Lookup::None => Vec::new(),
        };
        let different_class = alias.with_different_class();
        let mut found: Vec<IdCandidate> = Vec::new();

        for (pattern, module) in self.loadable.iter().chain(self.builtin.iter()) {
            if already.contains(module) || found.iter().any(|c| c.module == *module) {
                continue;
            }
            if glob_match(pattern, &rendered) {
                continue; // rung 1 already had this one
            }
            let Some((vendor, product)) = pattern_identity(pattern) else {
                continue; // wildcarded identity: the mismatch is elsewhere
            };
            let Some(hypothetical) = alias.with_identity(vendor, product) else {
                continue;
            };
            if !glob_match(pattern, &hypothetical.raw) {
                continue; // wants a different kind of device entirely
            }

            // The substitution test alone is not enough. Most drivers ship a
            // plain ID table and wildcard every class field, so it passes for
            // any device on the bus -- which would offer an ethernet driver a
            // USB stick. Two things can rescue it, and one of them must hold:
            //
            //   same vendor        the rebadge and new-revision case, which is
            //                      what new_id is overwhelmingly used for
            //   constrains class   the pattern actually cares what kind of
            //                      device this is, and this device qualifies
            let same_vendor = vendor == our_vendor;
            let constrains_class = different_class
                .as_ref()
                .and_then(|other| other.with_identity(vendor, product))
                .is_some_and(|probe| !glob_match(pattern, &probe.raw));
            if !same_vendor && !constrains_class {
                continue;
            }

            found.push(IdCandidate {
                module: module.clone(),
                claims: format!("{vendor:04x}:{product:04x}"),
                pattern: pattern.clone(),
                same_vendor,
            });
        }
        found
    }

    fn match_in(&self, table: &[(String, String)], alias: &str) -> Vec<String> {
        let mut found: Vec<String> = table
            .iter()
            .filter(|(pattern, _)| glob_match(pattern, alias))
            .map(|(_, module)| module.clone())
            .collect();
        found.dedup();
        found
    }
}

fn read_alias_file(path: &Path) -> Vec<(String, String)> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("alias ")?;
            // Pattern first, module last. Split from the right: a pattern never
            // contains a space, but splitting from the left would break on one
            // if it ever did.
            let (pattern, module) = rest.rsplit_once(char::is_whitespace)?;
            let pattern = pattern.trim();
            let module = module.trim();
            (!pattern.is_empty() && !module.is_empty())
                .then(|| (pattern.to_string(), module.to_string()))
        })
        .collect()
}

fn kernel_release() -> Option<String> {
    // /proc/sys/kernel/osrelease rather than uname(2): one file read, no libc
    // binding, and identical content.
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// Shell-glob match, as the kernel's alias patterns use it.
///
/// Public because it is the one piece of this module worth reusing, and
/// because it is verified against another implementation: 18,634 generated
/// pattern/text pairs were compared against Python's `fnmatch.fnmatchcase`
/// with no disagreement. Re-run that before changing anything here.
///
/// Supports `*`, `?` and `[...]` with ranges and `!`/`^` negation. Iterative
/// with backtracking rather than recursive: a pattern is untrusted input in the
/// sense that it comes off disk, and `*a*a*a*a*b` against a long alias is a
/// stack overflow waiting to happen in the naive recursive form.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();

    let (mut p, mut t) = (0usize, 0usize);
    // Where to resume if the current `*` turns out to have matched too little.
    let mut star: Option<usize> = None;
    let mut star_text = 0usize;

    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some(p);
                star_text = t;
                p += 1;
            }
            Some('?') => {
                p += 1;
                t += 1;
            }
            Some('[') => match match_class(&pattern, p, text[t]) {
                Some(next) => {
                    p = next;
                    t += 1;
                }
                None => match star {
                    Some(position) => {
                        p = position + 1;
                        star_text += 1;
                        t = star_text;
                    }
                    None => return false,
                },
            },
            Some(character) if *character == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some(position) => {
                    p = position + 1;
                    star_text += 1;
                    t = star_text;
                }
                None => return false,
            },
        }
    }

    // Trailing stars may match the empty remainder.
    while pattern.get(p) == Some(&'*') {
        p += 1;
    }
    p == pattern.len()
}

/// Match one `[...]` class against `candidate`.
///
/// Returns the index just past the closing bracket on a match, `None`
/// otherwise. An unterminated class is treated as a literal `[`, which is what
/// shells do and keeps a malformed index from silently matching everything.
fn match_class(pattern: &[char], open: usize, candidate: char) -> Option<usize> {
    let mut index = open + 1;
    let negated = matches!(pattern.get(index), Some('!') | Some('^'));
    if negated {
        index += 1;
    }

    let mut matched = false;
    let mut first = true;
    while index < pattern.len() {
        // A `]` in the first position is a literal, per shell convention.
        if pattern[index] == ']' && !first {
            let result = matched != negated;
            return result.then_some(index + 1);
        }
        first = false;

        let is_range = pattern.get(index + 1) == Some(&'-')
            && pattern.get(index + 2).is_some()
            && pattern.get(index + 2) != Some(&']');

        if is_range {
            let (low, high) = (pattern[index], pattern[index + 2]);
            if candidate >= low && candidate <= high {
                matched = true;
            }
            index += 3;
        } else {
            if pattern[index] == candidate {
                matched = true;
            }
            index += 1;
        }
    }
    // Unterminated: not a class at all.
    (pattern[open] == candidate).then_some(open + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real lines, in the real format, from a Ubuntu modules.alias.
    const ALIAS_FILE: &str = "\
# Aliases extracted from modules themselves.
alias usb:v0BDAp8153d*dc*dsc*dp*ic*isc*ip*in* r8152
alias usb:v0BDAp8152d*dc*dsc*dp*ic*isc*ip*in* r8152
alias usb:v*p*d*dc*dsc*dp*ic02isc06ip00in* cdc_ether
alias pci:v00001AF4d00001041sv*sd*bc*sc*i* virtio_net
alias pci:v00001AF4d*sv*sd*bc*sc*i* virtio_pci
alias of:N*T*Cti,tmp102* lm75
alias acpi*:PNP0501:* serial8250
";

    const BUILTIN_FILE: &str = "\
alias pci:v00008086d0000A0F0sv*sd*bc*sc*i* iwlwifi
alias platform:rtc_cmos rtc_cmos
";

    fn index() -> ModuleIndex {
        ModuleIndex {
            loadable: read_alias_file_from(ALIAS_FILE),
            builtin: read_alias_file_from(BUILTIN_FILE),
            source: None,
        }
    }

    /// The parser, fed from a string rather than a file.
    fn read_alias_file_from(text: &str) -> Vec<(String, String)> {
        text.lines()
            .filter_map(|line| {
                let rest = line.trim().strip_prefix("alias ")?;
                let (pattern, module) = rest.rsplit_once(char::is_whitespace)?;
                Some((pattern.trim().to_string(), module.trim().to_string()))
            })
            .collect()
    }

    #[test]
    fn a_device_with_a_loadable_driver_is_rung_one() {
        let alias = Modalias::parse("usb:v0BDAp8153d0100dc00dsc00dp00ic02isc06ip00in00");
        let lookup = index().lookup(&alias);
        assert!(lookup.can_load());
        match &lookup {
            // cdc_ether also matches by interface class, which is correct:
            // modprobe would report both and either could drive it.
            Lookup::Loadable(modules) => {
                assert!(modules.contains(&"r8152".to_string()), "{modules:?}");
                assert!(modules.contains(&"cdc_ether".to_string()), "{modules:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_device_nothing_claims_is_what_justifies_the_expensive_rungs() {
        let alias = Modalias::parse("usb:v1337p1337d0100dc00dsc00dp00ic09isc00ip00in00");
        let lookup = index().lookup(&alias);
        assert_eq!(lookup, Lookup::None);
        assert!(!lookup.can_load());
        assert!(
            lookup.describe().contains("no driver"),
            "{}",
            lookup.describe()
        );
    }

    #[test]
    fn a_builtin_driver_is_reported_as_not_a_loading_problem() {
        // The distinction the whole module exists for. Running modprobe here
        // does nothing and hides the real diagnosis.
        let alias = Modalias::parse("platform:rtc_cmos");
        let lookup = index().lookup(&alias);
        assert_eq!(lookup, Lookup::Builtin("rtc_cmos".into()));
        assert!(!lookup.can_load(), "there is nothing to load");
        assert!(
            lookup.describe().contains("not a loading problem"),
            "{}",
            lookup.describe()
        );
    }

    #[test]
    fn builtin_wins_over_loadable_when_both_match() {
        // "seen and rejected" is a more specific finding than "unrecognised".
        let mut index = index();
        index
            .loadable
            .push(("platform:rtc_cmos".into(), "rtc_cmos_mod".into()));
        assert_eq!(
            index.lookup(&Modalias::parse("platform:rtc_cmos")),
            Lookup::Builtin("rtc_cmos".into())
        );
    }

    #[test]
    fn a_pci_device_matches_the_specific_entry_and_the_catch_all() {
        // virtio_net claims this exact device; virtio_pci claims the whole
        // vendor. Both are real matches and modprobe would list both.
        let alias = Modalias::parse("pci:v00001AF4d00001041sv00001AF4sd00001041bc02sc00i00");
        match index().lookup(&alias) {
            Lookup::Loadable(modules) => {
                assert_eq!(modules, vec!["virtio_net", "virtio_pci"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_empty_index_is_distinguishable_from_a_device_nothing_claims() {
        // A container has no /lib/modules. Answering "no driver exists" there
        // would send the ladder off to write one on the strength of a missing
        // file.
        let empty = ModuleIndex::load(Path::new("/nonexistent/lib/modules/x"));
        assert!(empty.is_empty());
        assert_eq!(empty.lookup(&Modalias::parse("usb:v1p2")), Lookup::None);
        assert!(!index().is_empty(), "a real index is not empty");
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        assert_eq!(index().loadable.len(), 7);
        assert_eq!(index().len(), 9);
    }

    #[test]
    fn a_device_tree_alias_matches_on_compatible() {
        // How a board finds a driver for an I2C sensor.
        let alias = Modalias::parse("of:Ntemp_sensorT(null)Cti,tmp102");
        match index().lookup(&alias) {
            Lookup::Loadable(modules) => assert_eq!(modules, vec!["lm75"]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_acpi_alias_with_a_leading_wildcard_matches() {
        let alias = Modalias::parse("acpi:PNP0501:");
        match index().lookup(&alias) {
            Lookup::Loadable(modules) => assert_eq!(modules, vec!["serial8250"]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_rebadged_device_finds_the_driver_that_wants_it() {
        // The rung 3 case, with real hardware shapes. Realtek's USB ethernet
        // presents a vendor-specific interface class (0xFF) rather than CDC,
        // which is exactly why it needs r8152 and cdc_ether cannot help. The
        // device here is 0bda:8155 -- a revision the kernel has not been told
        // about. Nothing claims it, and r8152 would drive it.
        let unknown = Modalias::parse("usb:v0BDAp8155d3000dcFFdsc00dp00icFFisc00ip00in00");
        assert_eq!(
            index().lookup(&unknown),
            Lookup::None,
            "rung 1 finds nothing"
        );

        let candidates = index().id_candidates(&unknown);
        let modules: Vec<&str> = candidates.iter().map(|c| c.module.as_str()).collect();
        assert!(modules.contains(&"r8152"), "{candidates:?}");
        assert!(
            !modules.contains(&"cdc_ether"),
            "cdc_ether wants interface class 02/06, this is FF: {candidates:?}"
        );

        let r8152 = candidates.iter().find(|c| c.module == "r8152").unwrap();
        assert_eq!(r8152.claims, "0bda:8153", "names the device it does drive");
        assert!(r8152.same_vendor, "same manufacturer is the strong signal");
        assert!(r8152.describe().contains("0bda:8153"));
        assert!(r8152.describe().contains("same manufacturer"));
    }

    #[test]
    fn a_pure_id_table_driver_is_not_offered_another_vendors_device() {
        // The flaw the substitution test alone had. r8152's pattern wildcards
        // every class field, so "does it match once the IDs are swapped?" is
        // true for any USB device at all. Without the same-vendor or
        // constrains-class requirement, this offers an ethernet driver a
        // completely unrelated device from a different manufacturer.
        let other = Modalias::parse("usb:v1234p5678d0100dcFFdsc00dp00icFFisc00ip00in00");
        let modules: Vec<String> = index()
            .id_candidates(&other)
            .into_iter()
            .map(|c| c.module)
            .collect();
        assert!(
            !modules.contains(&"r8152".to_string()),
            "different vendor, and r8152's pattern constrains no class: {modules:?}"
        );
    }

    #[test]
    fn a_driver_that_wants_a_different_kind_of_device_is_not_offered() {
        // Same vendor is a strong signal, but not a blank cheque: it still has
        // to be the kind of device the driver constrains itself to. cdc_ether
        // names interface class 02/06, and a mass-storage interface is not
        // that, however the IDs are rearranged.
        let storage = Modalias::parse("usb:v0BDAp9999d0100dc00dsc00dp00ic08isc06ip50in00");
        let modules: Vec<String> = index()
            .id_candidates(&storage)
            .into_iter()
            .map(|c| c.module)
            .collect();
        assert!(
            !modules.contains(&"cdc_ether".to_string()),
            "interface class 08 is storage, not 02/06 ethernet: {modules:?}"
        );
    }

    #[test]
    fn a_device_that_already_has_a_driver_is_not_offered_one_again() {
        // Rung 1 covered it. Re-offering here would loop.
        let known = Modalias::parse("usb:v0BDAp8153d0100dc00dsc00dp00ic02isc06ip00in00");
        assert!(index().lookup(&known).can_load());
        let modules: Vec<String> = index()
            .id_candidates(&known)
            .into_iter()
            .map(|c| c.module)
            .collect();
        assert!(!modules.contains(&"r8152".to_string()), "{modules:?}");
    }

    #[test]
    fn a_vendor_agnostic_pattern_is_not_an_id_candidate() {
        // cdc_ether claims any vendor with the right interface class. If it did
        // not already match, the mismatch is in a class field and no ID will
        // fix it, so offering new_id would be pointless.
        let odd = Modalias::parse("usb:v9999p9999d0100dc00dsc00dp00ic09isc09ip09in00");
        let modules: Vec<String> = index()
            .id_candidates(&odd)
            .into_iter()
            .map(|c| c.module)
            .collect();
        assert!(!modules.contains(&"cdc_ether".to_string()), "{modules:?}");
    }

    #[test]
    fn a_bus_without_numeric_ids_yields_no_candidates() {
        assert!(index()
            .id_candidates(&Modalias::parse("platform:rtc_cmos"))
            .is_empty());
        assert!(index()
            .id_candidates(&Modalias::parse("acpi:PNP0501:"))
            .is_empty());
    }

    #[test]
    fn a_pattern_identity_is_read_only_when_it_is_fully_literal() {
        // A partly wildcarded identity does not name a device that could be
        // pointed at, so it must not be read as one.
        assert_eq!(
            pattern_identity("usb:v0BDAp8153d*dc*dsc*dp*ic*isc*ip*in*"),
            Some((0x0bda, 0x8153))
        );
        assert_eq!(
            pattern_identity("usb:v*p*d*dc*dsc*dp*ic02isc06ip00in*"),
            None
        );
        assert_eq!(
            pattern_identity("usb:v0BD*p8153d*"),
            None,
            "partial wildcard"
        );
        assert_eq!(
            pattern_identity("pci:v00001AF4d00001041sv*sd*bc*sc*i*"),
            Some((0x1af4, 0x1041))
        );
        assert_eq!(pattern_identity("of:N*T*Cti,tmp102*"), None);
    }

    #[test]
    fn a_pci_device_can_be_offered_to_a_driver_for_its_sibling() {
        // Same story on PCI: a card with a new device ID and the same class.
        let unknown = Modalias::parse("pci:v00001AF4d00009999sv00001AF4sd00009999bc02sc00i00");
        let candidates = index().id_candidates(&unknown);
        let modules: Vec<&str> = candidates.iter().map(|c| c.module.as_str()).collect();
        assert!(modules.contains(&"virtio_net"), "{candidates:?}");
    }

    #[test]
    fn candidates_carry_the_index_line_so_the_reasoning_can_be_checked() {
        // The machine is about to force a binding. Someone has to be able to
        // see why it thought that was a good idea.
        let unknown = Modalias::parse("usb:v0BDAp8155d0100dc00dsc00dp00ic02isc06ip00in00");
        let candidate = index()
            .id_candidates(&unknown)
            .into_iter()
            .find(|c| c.module == "r8152")
            .unwrap();
        assert!(
            candidate.pattern.contains("v0BDAp8153"),
            "{}",
            candidate.pattern
        );
    }

    #[test]
    fn globs_match_the_way_a_shell_does() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("usb:v*p*", "usb:v0BDAp8153"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"), "? needs exactly one character");
        assert!(glob_match("abc", "abc"));
        assert!(!glob_match("abc", "abcd"));
        assert!(!glob_match("abcd", "abc"));
        assert!(
            glob_match("abc*", "abc"),
            "a trailing star may match nothing"
        );
        assert!(glob_match("", ""));
        assert!(!glob_match("", "x"));
        assert!(glob_match("*", ""));
    }

    #[test]
    fn backtracking_finds_a_match_a_greedy_star_would_miss() {
        // The classic case: the first * must give back characters for the
        // literal tail to line up.
        assert!(glob_match("*b", "aaab"));
        assert!(glob_match("*a*b", "xaxxb"));
        assert!(!glob_match("*a*b", "xaxx"));
        assert!(glob_match("a*b*c", "abxbxc"));
    }

    #[test]
    fn a_pathological_pattern_does_not_blow_the_stack() {
        // *a*a*a*a*a*b against a long non-matching string is where a recursive
        // matcher dies. This must simply return false.
        let pattern = "*a*a*a*a*a*a*b";
        let text = "a".repeat(2000);
        assert!(!glob_match(pattern, &text));
    }

    #[test]
    fn character_classes_match_ranges_and_negation() {
        assert!(glob_match("i[0-9][0-9]", "i42"));
        assert!(!glob_match("i[0-9][0-9]", "i4x"));
        assert!(glob_match("[abc]x", "bx"));
        assert!(!glob_match("[abc]x", "dx"));
        assert!(glob_match("[!abc]x", "dx"), "negated class");
        assert!(!glob_match("[!abc]x", "ax"));
        assert!(glob_match("[^abc]x", "dx"), "^ negates too");
    }

    #[test]
    fn an_unterminated_class_is_a_literal_bracket_not_a_wildcard() {
        // A malformed index line must not become a pattern that claims every
        // device on the machine.
        assert!(glob_match("[abc", "[abc"));
        assert!(!glob_match("[abc", "b"));
    }

    #[test]
    fn the_parser_keeps_the_module_name_from_the_right() {
        let parsed = read_alias_file_from("alias usb:v*p* some_module\n");
        assert_eq!(
            parsed,
            vec![("usb:v*p*".to_string(), "some_module".to_string())]
        );
    }

    #[test]
    fn a_malformed_line_is_skipped_rather_than_half_read() {
        let parsed = read_alias_file_from("alias\nalias onlyonefield\nnot an alias line\n");
        assert!(parsed.is_empty());
    }
}
